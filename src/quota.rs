//! 5h / 7d quota, and this 7d window's API-equivalent spend on this machine.

use crate::ansi::{bold, by_load, cyan, dim, yellow};
use crate::fmt::{fmt_k, fmt_usd};
use crate::js::{self, HOUR_MS, MINUTE_MS};
use crate::paths;
use crate::store::{read_json, write_json_atomic};
use chrono::{Local, TimeZone};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub fn parse_reset_ms(v: Option<&Value>) -> f64 {
    match v {
        None | Some(Value::Null) => f64::NAN,
        Some(Value::Number(n)) => {
            let x = n.as_f64().unwrap_or(f64::NAN);
            if x < 1e12 {
                x * 1000.0
            } else {
                x
            }
        }
        Some(Value::String(s)) if s.is_empty() => f64::NAN,
        Some(other) => {
            let s = js::to_string(other);
            let s = s.trim();
            let mut parts = s.splitn(2, '.');
            let int = parts.next().unwrap_or("");
            let frac = parts.next();
            let numeric = !int.is_empty()
                && int.bytes().all(|b| b.is_ascii_digit())
                && frac.is_none_or(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()));
            if numeric {
                let x = js::parse_number(s);
                if x < 1e12 {
                    x * 1000.0
                } else {
                    x
                }
            } else {
                js::parse_date_str(s)
            }
        }
    }
}

/// Countdown to a reset: "1d20h", "3h29m", "12m".
pub fn until_reset(v: Option<&Value>) -> String {
    let t = parse_reset_ms(v);
    if !t.is_finite() {
        return String::new();
    }
    let ms = t - js::now_ms();
    if ms <= 0.0 {
        return String::new();
    }
    let total_min = (ms / 60_000.0).floor().max(1.0) as u64;
    let d = total_min / (60 * 24);
    let h = (total_min % (60 * 24)) / 60;
    let m = total_min % 60;
    if d >= 1 {
        format!("{d}d{h}h")
    } else if h > 0 {
        format!("{h}h{m:02}m")
    } else {
        format!("{m}m")
    }
}

pub fn has_window(w: Option<&Value>) -> bool {
    js::truthy(w) && js::as_number(js::get(w, &["used_percentage"])).is_some()
}

fn write_quota_cache(rl: &Value) {
    if !has_window(rl.get("five_hour")) && !has_window(rl.get("seven_day")) {
        return;
    }
    let mut m = rl.as_object().cloned().unwrap_or_default();
    m.insert("cached_at".into(), js::json_num(js::now_ms()));
    write_json_atomic(&paths::quota_cache(), &Value::Object(m));
}

/// The usage cache CCometixLine's own usage segment writes.
fn from_ccline_api_cache(raw: Option<Value>) -> Option<Value> {
    let raw = raw?;
    let raw = raw.as_object()?;
    let mut rl = Map::new();
    let window = |pct: &str, reset: &str| {
        let mut w = Map::new();
        w.insert("used_percentage".into(), raw.get(pct).cloned().unwrap_or(Value::Null));
        if let Some(r) = raw.get(reset) {
            w.insert("resets_at".into(), r.clone());
        }
        Value::Object(w)
    };
    if js::as_number(raw.get("five_hour_utilization")).is_some() {
        rl.insert("five_hour".into(), window("five_hour_utilization", "five_hour_resets_at"));
    }
    if js::as_number(raw.get("seven_day_utilization")).is_some() {
        rl.insert("seven_day".into(), window("seven_day_utilization", "resets_at"));
    }
    if rl.is_empty() {
        None
    } else {
        Some(Value::Object(rl))
    }
}

/// Account-wide 5h/7d: stdin first, then the last stdin snapshot, then the ccline usage cache.
pub fn resolve_rate_limits(stdin_rl: Option<&Value>) -> Option<Value> {
    if let Some(rl) = stdin_rl {
        if has_window(rl.get("five_hour")) || has_window(rl.get("seven_day")) {
            write_quota_cache(rl);
            return Some(rl.clone());
        }
    }
    let cached = read_json(&paths::quota_cache());
    if let Some(c) = &cached {
        if has_window(c.get("five_hour")) || has_window(c.get("seven_day")) {
            return cached;
        }
    }
    from_ccline_api_cache(read_json(&paths::ccline_api_cache()))
}

// ---- this window's spend ----

const SEVEN_DAYS_MS: f64 = 7.0 * 24.0 * HOUR_MS;
const WEEK_SCAN_EVERY_MS: f64 = MINUTE_MS;
// Bumped when week-spend.json changes shape; an older file is rebuilt.
const WEEK_SPEND_VERSION: u32 = 2;
// Bytes read in one scan while catching up, over all files.
const WEEK_READ_CAP: u64 = 32 * 1024 * 1024;
// A message's records are written one after another, so a few recent ids catch its repeats.
const RECENT_IDS: usize = 16;

/// USD per million tokens: input, output, cache read. Cache writes: 5 min 1.25x input, 1 h 2x.
/// Longer keys first, so "claude-opus-5-5" is not read as "claude-opus-5".
const PRICES: &[(&str, [f64; 3])] = &[
    ("claude-sonnet-5-5", [2.0, 10.0, 0.1]), // CC 2.1.296 prices its cache reads at $0.10
    ("claude-sonnet-4-6", [3.0, 15.0, 0.3]),
    ("claude-fable-5-1", [10.0, 50.0, 0.25]),
    ("claude-haiku-5-5", [0.1, 0.5, 0.01]),
    ("claude-haiku-4-5", [1.0, 5.0, 0.1]),
    ("claude-opus-5-5", [4.0, 20.0, 0.2]),
    ("claude-opus-4-8", [5.0, 25.0, 0.5]),
    ("claude-opus-4-7", [5.0, 25.0, 0.5]),
    ("claude-opus-4-6", [5.0, 25.0, 0.5]),
    ("claude-sonnet-5", [2.0, 10.0, 0.2]),
    ("claude-fable-5", [10.0, 50.0, 1.0]),
    ("claude-opus-5", [5.0, 25.0, 0.5]),
];

/// API list price of one response's usage, or None for a model not in PRICES.
pub fn usage_cost(model: Option<&Value>, u: &Value) -> Option<f64> {
    let m = model.filter(|v| !v.is_null()).map(js::to_string).unwrap_or_default().to_lowercase();
    let m = m.split('[').next().unwrap_or("");
    let &(key, prices) = PRICES.iter().find(|(k, _)| m.starts_with(k))?;
    let [mut p_in, mut p_out, mut p_read] = prices;
    let n = |k: &str| js::number_or0(u.get(k));
    let inp = n("input_tokens");
    let out = n("output_tokens");
    let read = n("cache_read_input_tokens");
    let cc = u.get("cache_creation");
    let (w5, w1) = if js::truthy(cc) {
        (
            js::number_or0(js::get(cc, &["ephemeral_5m_input_tokens"])),
            js::number_or0(js::get(cc, &["ephemeral_1h_input_tokens"])),
        )
    } else {
        (n("cache_creation_input_tokens"), 0.0)
    };
    // Claude Haiku 5.5 costs more for prompts over 100K tokens.
    if key == "claude-haiku-5-5" && inp + read + w5 + w1 > 100_000.0 {
        (p_in, p_out, p_read) = (0.5, 2.5, 0.05);
    }
    let searches = js::number_or0(js::get(Some(u), &["server_tool_use", "web_search_requests"]));
    Some((inp * p_in + out * p_out + read * p_read + w5 * 1.25 * p_in + w1 * 2.0 * p_in) / 1e6 + searches * 0.01)
}

/// All tokens of one response: input, output, cache reads and cache writes.
fn usage_tokens(u: &Value) -> f64 {
    ["input_tokens", "output_tokens", "cache_read_input_tokens", "cache_creation_input_tokens"]
        .iter()
        .map(|k| js::number_or0(u.get(*k)))
        .sum()
}

/// Every transcript and subagent file under ~/.claude/projects.
fn list_transcripts(projects: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(dirs) = fs::read_dir(projects) else { return out };
    for d in dirs.flatten() {
        if !d.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Ok(ents) = fs::read_dir(d.path()) else { continue };
        for e in ents.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            let name = e.file_name();
            if ft.is_file() && name.to_string_lossy().ends_with(".jsonl") {
                out.push(e.path());
            } else if ft.is_dir() {
                let Ok(subs) = fs::read_dir(e.path().join("subagents")) else { continue };
                for s in subs.flatten() {
                    if s.file_name().to_string_lossy().ends_with(".jsonl") {
                        out.push(s.path());
                    }
                }
            }
        }
    }
    out
}

#[derive(Serialize, Deserialize)]
struct FileState {
    off: u64,
    /// The file's last message ids, with the cost and tokens counted for each.
    recent: Vec<(String, (f64, f64))>,
}

#[derive(Serialize, Deserialize)]
struct WeekState {
    v: u32,
    window: i64,
    total: f64,
    tokens: f64,
    #[serde(rename = "scannedAt")]
    scanned_at: f64,
    behind: bool,
    files: HashMap<String, FileState>,
    seen: Vec<String>,
}

pub struct Week {
    pub usd: f64,
    pub tokens: f64,
    pub behind: bool,
}

/// Message ids counted in this window, in the order they were seen.
struct Seen {
    order: Vec<String>,
    set: HashSet<String>,
}

/// Adds the cost and tokens of responses in complete lines from the window's
/// start, once per message id: `seen` holds every id counted this window (a
/// forked conversation repeats the messages it was forked from), f.recent the
/// file's last ids, whose later records add only their growth.
fn tally_spend(text: &str, f: &mut FileState, seen: &mut Seen, start: f64, st: &mut WeekState) {
    for line in text.split('\n') {
        if !line.contains("\"usage\"") {
            continue;
        }
        let Ok(r) = serde_json::from_str::<Value>(line) else { continue };
        let m = r.get("message");
        let id = js::get(m, &["id"]);
        let usage = js::get(m, &["usage"]);
        if r.get("type").and_then(Value::as_str) != Some("assistant") || !js::truthy(id) || !js::truthy(usage) {
            continue;
        }
        let model = js::get(m, &["model"]);
        if model.and_then(Value::as_str) == Some("<synthetic>") {
            continue;
        }
        let ts = js::date_parse(r.get("timestamp"));
        if ts.is_nan() || ts < start {
            continue;
        }
        let (Some(id), Some(usage)) = (id.and_then(Value::as_str), usage) else { continue };
        let Some(c) = usage_cost(model, usage) else { continue };
        let n = usage_tokens(usage);
        let chars: Vec<char> = id.chars().collect();
        let tag: String = chars[chars.len().saturating_sub(12)..].iter().collect();
        if let Some(e) = f.recent.iter_mut().find(|(t, _)| *t == tag) {
            let (pc, pn) = e.1;
            if c > pc || n > pn {
                st.total += (c - pc).max(0.0);
                st.tokens += (n - pn).max(0.0);
                e.1 = (c.max(pc), n.max(pn));
            }
            continue;
        }
        if seen.set.contains(&tag) {
            continue;
        }
        seen.set.insert(tag.clone());
        seen.order.push(tag.clone());
        f.recent.push((tag, (c, n)));
        st.total += c;
        st.tokens += n;
        if f.recent.len() > RECENT_IDS {
            f.recent.remove(0);
        }
    }
}

fn read_bytes(file: &Path, start: u64, len: u64) -> std::io::Result<Vec<u8>> {
    let mut f = fs::File::open(file)?;
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity(len as usize);
    f.take(len).read_to_end(&mut buf)?;
    Ok(buf)
}

/// API-equivalent spend of all Claude Code use on this machine in the current
/// 7d window, priced from the transcripts' token usage. A scan runs at most
/// once a minute and reads only what was appended since the last one;
/// week-spend.json keeps the offsets, the ids and the total, and starts over
/// when the window resets. Transcripts leave out some requests CC pays for
/// (background compaction, for one), so the figure runs a few percent low.
pub fn week_spend(w: Option<&Value>) -> Option<Week> {
    if !has_window(w) {
        return None;
    }
    let reset = parse_reset_ms(js::get(w, &["resets_at"]));
    let now = js::now_ms();
    if reset.is_nan() || reset <= now {
        return None;
    }
    let win = js::round(reset / MINUTE_MS) as i64;
    let start = reset - SEVEN_DAYS_MS;
    let file = paths::week_spend();
    let st: Option<WeekState> = read_json(&file).and_then(|v| serde_json::from_value(v).ok());
    let mut st = match st {
        Some(s) if s.window == win && s.v == WEEK_SPEND_VERSION => s,
        _ => WeekState {
            v: WEEK_SPEND_VERSION,
            window: win,
            total: 0.0,
            tokens: 0.0,
            scanned_at: 0.0,
            behind: true,
            files: HashMap::new(),
            seen: Vec::new(),
        },
    };
    if !st.behind && now - st.scanned_at < WEEK_SCAN_EVERY_MS {
        return Some(Week { usd: st.total, tokens: st.tokens, behind: false });
    }
    let order = std::mem::take(&mut st.seen);
    let mut seen = Seen { set: order.iter().cloned().collect(), order };
    let mut budget = WEEK_READ_CAP;
    let mut behind = false;
    let projects = paths::projects();
    for file in list_transcripts(&projects) {
        let Ok(stat) = fs::metadata(&file) else { continue };
        let mtime = stat.modified().map(js::time_ms).unwrap_or(0.0);
        if mtime < start {
            continue;
        }
        let rel = file.strip_prefix(&projects).unwrap_or(&file).to_string_lossy().into_owned();
        let size = stat.len();
        let mut f = match st.files.remove(&rel) {
            Some(f) if size >= f.off => f,
            _ => FileState { off: 0, recent: Vec::new() },
        };
        if size == f.off {
            if !f.recent.is_empty() && now - mtime > HOUR_MS {
                f.recent.clear();
            }
            st.files.insert(rel, f);
            continue;
        }
        if budget == 0 {
            behind = true;
            st.files.insert(rel, f);
            continue;
        }
        let len = (size - f.off).min(budget);
        if len < size - f.off {
            behind = true;
        }
        let Ok(buf) = read_bytes(&file, f.off, len) else {
            st.files.insert(rel, f);
            continue;
        };
        // The last line may still be being written.
        if let Some(end) = buf.iter().rposition(|&b| b == b'\n') {
            budget -= end as u64 + 1;
            tally_spend(&String::from_utf8_lossy(&buf[..=end]), &mut f, &mut seen, start, &mut st);
            f.off += end as u64 + 1;
        }
        st.files.insert(rel, f);
    }
    st.seen = seen.order;
    st.scanned_at = now;
    st.behind = behind;
    if let Ok(v) = serde_json::to_value(&st) {
        write_json_atomic(&file, &v);
    }
    Some(Week { usd: st.total, tokens: st.tokens, behind })
}

/// Local reset time as "Wed 10:00", rounded to the minute.
fn reset_clock(t: f64) -> String {
    let ms = (js::round(t / MINUTE_MS) * MINUTE_MS) as i64;
    match Local.timestamp_millis_opt(ms).single() {
        Some(d) => d.format("%a %H:%M").to_string(),
        None => String::new(),
    }
}

/// The 5h and 7d parts, to be joined with dim(' | ').
/// A window whose reset time has passed is back at 0%: CC keeps the old figure
/// until the next request. The 7d reset shows the countdown and the local
/// weekday and time it resets, then this window's API-equivalent spend and
/// tokens, and the whole weekly limit they imply (from 5% used on, while the
/// earlier figure would be too rough):
/// "1d21h · Wed 10:00 · $651 2.3B tok/~$1.3k 4.5B tok".
pub fn fmt_quota(rl: Option<&Value>, week: Option<&Week>) -> Vec<String> {
    let mut parts = Vec::new();
    for (key, label) in [("five_hour", "5h"), ("seven_day", "7d")] {
        let w = js::get(rl, &[key]);
        if !has_window(w) {
            parts.push(format!("{label} {}", dim("--")));
            continue;
        }
        let used = js::as_number(js::get(w, &["used_percentage"])).unwrap_or(0.0);
        let resets_at = js::get(w, &["resets_at"]);
        let t = parse_reset_ms(resets_at);
        let passed = t.is_finite() && t - js::now_ms() <= 0.0;
        let pct = if passed { 0.0 } else { js::round(used) };
        let mut left = if passed { String::new() } else { until_reset(resets_at) };
        let mut spend = String::new();
        if !left.is_empty() && key == "seven_day" {
            left += &format!(" · {}", reset_clock(t));
            if let Some(week) = week {
                // Used this week in bold yellow, the estimated limit in cyan.
                spend = format!(
                    "{}{}",
                    dim(" · "),
                    bold(&yellow(&format!("{} {} tok", fmt_usd(week.usd), fmt_k(week.tokens))))
                );
                if week.behind {
                    spend += &dim("…");
                } else if used >= 5.0 {
                    let f = used / 100.0;
                    spend += &format!(
                        "{}{}",
                        dim("/"),
                        cyan(&format!("~{} {} tok", fmt_usd(week.usd / f), fmt_k(week.tokens / f)))
                    );
                }
            }
        }
        let arrow = if left.is_empty() { String::new() } else { dim(&format!("→{left}")) };
        parts.push(format!("{label} {}{arrow}{spend}", by_load(pct, &format!("{}%", js::num_str(pct)))));
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prices_pick_the_longest_key() {
        let u = json!({ "input_tokens": 1_000_000, "output_tokens": 0 });
        assert_eq!(usage_cost(Some(&json!("claude-opus-5-5[1m]")), &u), Some(4.0));
        assert_eq!(usage_cost(Some(&json!("claude-opus-5-20260101")), &u), Some(5.0));
        assert_eq!(usage_cost(Some(&json!("gpt-5")), &u), None);
    }

    #[test]
    fn cache_writes_cost_more() {
        let u = json!({ "cache_creation": { "ephemeral_5m_input_tokens": 1_000_000, "ephemeral_1h_input_tokens": 1_000_000 } });
        assert_eq!(usage_cost(Some(&json!("claude-opus-5-5")), &u), Some(4.0 * 1.25 + 4.0 * 2.0));
    }

    #[test]
    fn reset_times() {
        assert_eq!(parse_reset_ms(Some(&json!(1_800_000_000))), 1_800_000_000_000.0);
        assert_eq!(parse_reset_ms(Some(&json!("1800000000"))), 1_800_000_000_000.0);
        assert!(parse_reset_ms(Some(&json!(""))).is_nan());
        assert_eq!(until_reset(Some(&json!(1.0))), "");
    }
}
