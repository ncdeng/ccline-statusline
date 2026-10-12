//! The transcript tail: prompt-cache countdown, output speed and time to first token.

use crate::ansi::{dim, green, red, yellow};
use crate::js::{self, HOUR_MS, MINUTE_MS};
use serde_json::Value;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// The last bytes of a file. The first line is cut mid-way unless the whole file was read.
pub struct Tail {
    text: String,
    pub is_whole: bool,
}

impl Tail {
    pub fn read(path: &Path, bytes: u64) -> std::io::Result<Tail> {
        let mut f = File::open(path)?;
        let size = f.metadata()?.len();
        let len = size.min(bytes);
        f.seek(SeekFrom::Start(size - len))?;
        let mut buf = Vec::with_capacity(len as usize);
        f.take(len).read_to_end(&mut buf)?;
        Ok(Tail { text: String::from_utf8_lossy(&buf).into_owned(), is_whole: len == size })
    }

    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.text.split('\n').skip(usize::from(!self.is_whole))
    }
}

/// One main-thread response: its records' times, output tokens, and first-token time.
pub struct Msg {
    /// Time of the record written just before the request (prompt or tool result).
    start: Option<f64>,
    first: f64,
    last: f64,
    out: f64,
    first_token: Option<f64>,
    done: bool,
}

pub struct Scan {
    /// In the order the responses first appear.
    msgs: Vec<Msg>,
    last_user_t: Option<f64>,
    /// TTL of the latest response that wrote cache.
    ttl_ms: Option<f64>,
}

/// A slash command that runs inside CC (/model, /cost …) and sends no request.
fn is_local_command(m: Option<&Value>) -> bool {
    match js::get(m, &["content"]) {
        Some(Value::String(c)) => c.starts_with("<local-command") || c.starts_with("<command-name>"),
        _ => false,
    }
}

/// One pass over the transcript tail, shared by the cache countdown and the
/// speed figures. Main-thread records only: subagents write their own files.
///
/// First token = thinking block's write time − thinkingDurationMs. CC can
/// write two thinking records for one response: the real duration first,
/// then a summary block with thinkingDurationMs ≈ 1, so the earliest
/// first-token estimate wins. Records with stop_reason null carry the
/// message_start usage (output_tokens ≈ 8), not the final count.
pub fn scan_transcript<'a>(lines: impl Iterator<Item = &'a str>) -> Scan {
    let mut msgs: Vec<Msg> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut prev_t: Option<f64> = None;
    let mut last_user_t = None;
    let mut ttl_ms = None;
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(r) = serde_json::from_str::<Value>(line) else { continue };
        if js::truthy(r.get("isSidechain")) {
            continue;
        }
        let ty = r.get("type").and_then(Value::as_str);
        if ty != Some("assistant") && ty != Some("user") {
            continue;
        }
        let t = js::date_parse(r.get("timestamp"));
        if !t.is_finite() {
            continue;
        }
        let m = r.get("message");
        let id = js::get(m, &["id"]);
        if ty == Some("assistant") && js::truthy(id) {
            let key = id.map(js::to_string).unwrap_or_default();
            let i = *index.entry(key).or_insert_with(|| {
                msgs.push(Msg { start: prev_t, first: t, last: t, out: 0.0, first_token: None, done: false });
                msgs.len() - 1
            });
            let s = &mut msgs[i];
            s.last = s.last.max(t);
            let usage = js::get(m, &["usage"]);
            s.out = s.out.max(js::number_or0(js::get(usage, &["output_tokens"])));
            if js::truthy(js::get(m, &["stop_reason"])) {
                s.done = true;
            }
            if let Some(d) = js::as_number(r.get("thinkingDurationMs")) {
                let ft = t - d;
                s.first_token = Some(s.first_token.map_or(ft, |x| x.min(ft)));
            }
            let cc = js::get(usage, &["cache_creation"]);
            if js::truthy(cc) {
                if js::number(js::get(cc, &["ephemeral_1h_input_tokens"])) > 0.0 {
                    ttl_ms = Some(HOUR_MS);
                } else if js::number(js::get(cc, &["ephemeral_5m_input_tokens"])) > 0.0 {
                    ttl_ms = Some(5.0 * MINUTE_MS);
                }
            }
        } else if ty == Some("user") && !js::truthy(r.get("isMeta")) && !is_local_command(m) {
            last_user_t = Some(t);
        }
        prev_t = Some(t);
    }
    Scan { msgs, last_user_t, ttl_ms }
}

/// When the prompt cache was last refreshed. A cache entry lives from the
/// start of the request that wrote or read it, so the clock starts at the
/// record written just before the latest main-thread request, not at the end
/// of the reply. A prompt or tool result after the latest reply means a
/// request is on its way, and it starts the clock again.
fn cache_anchor(scan: &Scan) -> Option<f64> {
    let latest = scan.msgs.last();
    let mut at = latest.map(|s| s.start.unwrap_or(s.first));
    if let Some(u) = scan.last_user_t {
        if latest.is_none_or(|s| u > s.last) {
            at = Some(at.unwrap_or(0.0).max(u));
        }
    }
    at
}

/// Time left on the main-thread prompt cache.
/// CC's own prompt_cache.expires_at (epoch s) comes first: the moment it sent
/// the latest request, plus that request's TTL. CC leaves prompt_cache out
/// until the process has sent a request, so right after a restart the time
/// comes from the transcript, with the TTL of the latest response that wrote
/// cache: 1h normally, 5 min in usage overage.
pub fn fmt_cache_ttl(scan: Option<&Scan>, pc: Option<&Value>) -> String {
    let mut expires_at = None;
    let mut ttl_ms = HOUR_MS;
    let raw = js::get(pc, &["expires_at"]);
    let e = js::number(raw);
    if raw.is_some_and(|v| !v.is_null()) && e.is_finite() && e > 0.0 {
        expires_at = Some(e * 1000.0);
        if js::get(pc, &["ttl"]).and_then(Value::as_str) == Some("5m") {
            ttl_ms = 5.0 * MINUTE_MS;
        }
    } else if let Some(scan) = scan {
        if let Some(at) = cache_anchor(scan).filter(|a| *a != 0.0) {
            ttl_ms = scan.ttl_ms.unwrap_or(HOUR_MS);
            expires_at = Some(at + ttl_ms);
        }
    }
    let Some(expires_at) = expires_at else { return String::new() };
    let left_ms = expires_at - js::now_ms();
    if left_ms <= 0.0 {
        return dim(&red("❄️ Expired"));
    }
    let left_sec = (left_ms / 1000.0).floor();
    let m = (left_sec / 60.0).floor();
    let s = left_sec % 60.0;
    let text = format!("⚡ {}m{:02}s", js::num_str(m), s as u64);
    // 1h TTL: green from 15m, yellow from 5m (same fractions for the 5-min TTL).
    let ttl_sec = ttl_ms / 1000.0;
    if left_sec >= ttl_sec / 4.0 {
        green(&text)
    } else if left_sec >= ttl_sec / 12.0 {
        yellow(&text)
    } else {
        red(&text)
    }
}

/// Responses in one speed figure. One short tool call, whose 1–3 s window
/// reads 20–40% high, does not set the figure on its own.
const TPS_WINDOW: usize = 8;

struct Speed {
    tps: f64,
    ttft: Option<f64>,
    n: usize,
}

/// tok/s = Σ output tokens / Σ (last write − first token) over the last
/// TPS_WINDOW responses with a thinking block: the wait before the first
/// token is left out (generation speed, not end-to-end).
/// ttft = first token − the record written just before the request, for the
/// latest response; it includes CC's own time to build the request.
fn recent_speed(scan: &Scan) -> Option<Speed> {
    let done: Vec<&Msg> = scan
        .msgs
        .iter()
        .filter(|s| s.done && s.out > 0.0 && s.first_token.is_some_and(|ft| s.last - ft >= 300.0))
        .collect();
    let done = &done[done.len().saturating_sub(TPS_WINDOW)..];
    let latest = done.last()?;
    let out: f64 = done.iter().map(|s| s.out).sum();
    let ms: f64 = done.iter().map(|s| s.last - s.first_token.unwrap_or(s.last)).sum();
    let ft = latest.first_token.unwrap_or(0.0);
    let ttft = latest.start.filter(|st| ft > *st).map(|st| ft - st);
    Some(Speed { tps: out / (ms / 1000.0), ttft, n: done.len() })
}

/// Speed from the shared 256 KB scan; a 2 MB read only when that holds too few responses.
pub fn fmt_tps(path: Option<&str>, tail: Option<&Tail>, scan: Option<&Scan>) -> String {
    let (Some(scan), Some(tail)) = (scan, tail) else { return String::new() };
    let mut speed = recent_speed(scan);
    if speed.as_ref().is_none_or(|s| s.n < TPS_WINDOW) && !tail.is_whole {
        let Some(path) = path else { return String::new() };
        let Ok(more) = Tail::read(Path::new(path), 2 * 1024 * 1024) else { return String::new() };
        speed = recent_speed(&scan_transcript(more.lines())).or(speed);
    }
    let Some(speed) = speed else { return String::new() };
    let n = js::round(speed.tps);
    let color = if n >= 60.0 {
        green
    } else if n >= 30.0 {
        yellow
    } else {
        red
    };
    let mut s = format!("{}{}", color(&js::num_str(n)), dim(" tok/s"));
    if let Some(ttft) = speed.ttft {
        let sec = ttft / 1000.0;
        let t_color = if sec < 4.0 {
            green
        } else if sec < 8.0 {
            yellow
        } else {
            red
        };
        let v = if sec < 10.0 { js::to_fixed(sec, 1) } else { js::num_str(js::round(sec)) };
        s += &format!("{}{}{}", dim(" · ttft "), t_color(&v), dim("s"));
    }
    s
}
