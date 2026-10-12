//! A session's total cost that survives Claude Code restarts, and the size
//! of its latest prompt-cache miss.

use crate::js;
use crate::paths;
use crate::store::{read_json, write_json_atomic};
use serde_json::{Map, Value};

// Within one CC process, now − cost.total_duration_ms stays within a few seconds.
const SAME_PROCESS_MS: f64 = 60_000.0;

pub struct Ledger {
    pub usd: Option<f64>,
    pub last_miss_tokens: Option<f64>,
}

/// `path.basename(p)`, with "\" also a separator on Windows.
fn basename(p: &str) -> &str {
    let is_sep = |c: char| c == '/' || (cfg!(windows) && c == '\\');
    let t = p.trim_end_matches(is_sep);
    t.rsplit(is_sep).next().unwrap_or(t)
}

/// The transcript file name: CC 2.1.296 gives a resumed conversation a new
/// session_id while it keeps writing the same transcript.
fn session_key(data: &Value) -> Option<String> {
    let tp = data.get("transcript_path").and_then(Value::as_str).unwrap_or("");
    let id = if tp.is_empty() {
        data.get("session_id").filter(|v| !v.is_null()).map(js::to_string).unwrap_or_default()
    } else {
        let b = basename(tp);
        let cut = b.len().checked_sub(6).filter(|i| b.is_char_boundary(*i));
        match cut {
            Some(i) if b[i..].eq_ignore_ascii_case(".jsonl") => b[..i].to_string(),
            _ => b.to_string(),
        }
    };
    let ok = id.len() >= 8 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    ok.then_some(id)
}

/// cost.total_cost_usd counts only the running process. On resume CC brings
/// the old figure back only when the folder's lastSessionId in ~/.claude.json
/// is this session, and every session in the same folder overwrites it.
/// A ledger per session keeps base (spent by earlier processes) and last (the
/// current process's highest reading). When a new process shows up, the
/// total carries on from where it was: base = old total − the new process's
/// first reading. That is right whether CC restored the old figure, an older
/// one, or nothing. A stale reading from the same process never lowers it.
///
/// CC also counts misses and the tokens they wrote again per process; a rise
/// in its counts since the last reading gives the size of the miss, a drop a
/// new process.
pub fn session_ledger(data: &Value) -> Ledger {
    let c = js::as_number(js::get(Some(data), &["cost", "total_cost_usd"])).filter(|c| c.is_finite());
    let Some(sid) = session_key(data) else {
        return Ledger { usd: c, last_miss_tokens: None };
    };
    let file = paths::session_cost_dir().join(format!("{sid}.json"));
    let old: Map<String, Value> = read_json(&file).and_then(|v| v.as_object().cloned()).unwrap_or_default();
    let mut next = old.clone();
    if let Some(c) = c {
        let d = js::number(js::get(Some(data), &["cost", "total_duration_ms"]));
        let start = (d.is_finite() && d >= 0.0).then(|| js::now_ms() - d);
        let start_v = start.map_or(Value::Null, js::json_num);
        let base = js::as_number(old.get("base"));
        let last = js::as_number(old.get("last"));
        if let (Some(base), Some(last)) = (base, last) {
            let same_process = match (start, js::as_number(old.get("start"))) {
                (Some(s), Some(os)) => (s - os).abs() < SAME_PROCESS_MS,
                _ => c >= last,
            };
            if same_process {
                let keep_start = old.get("start").filter(|v| !v.is_null()).cloned().unwrap_or(start_v);
                next.insert("base".into(), old["base"].clone());
                next.insert("last".into(), js::json_num(last.max(c)));
                next.insert("start".into(), keep_start);
            } else {
                next.insert("base".into(), js::json_num((base + last - c).max(0.0)));
                next.insert("last".into(), js::json_num(c));
                next.insert("start".into(), start_v);
            }
        } else {
            next.insert("base".into(), js::json_num(0.0));
            next.insert("last".into(), js::json_num(c));
            next.insert("start".into(), start_v);
        }
    }
    let pc = data.get("prompt_cache");
    if let Some(misses) = js::as_number(js::get(pc, &["misses"])) {
        let recache = js::number_or0(js::get(pc, &["miss_recache_tokens"]));
        let seen = old.get("missSeen").filter(|v| !v.is_null());
        let (mut seen_m, mut seen_r) = match seen {
            Some(s) => (js::number(s.get("misses")), js::number(s.get("recache"))),
            None => (0.0, 0.0),
        };
        if misses < seen_m || recache < seen_r {
            (seen_m, seen_r) = (0.0, 0.0);
        }
        if misses > seen_m {
            next.insert("lastMissTokens".into(), js::json_num(recache - seen_r));
        }
        let mut s = Map::new();
        s.insert("misses".into(), js::json_num(misses));
        s.insert("recache".into(), js::json_num(recache));
        next.insert("missSeen".into(), Value::Object(s));
    }
    if next != old {
        write_json_atomic(&file, &Value::Object(next.clone()));
    }
    let total = || Some(js::number(next.get("base")) + js::number(next.get("last")));
    Ledger { usd: c.and_then(|_| total()), last_miss_tokens: js::as_number(next.get("lastMissTokens")) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_by_transcript_name() {
        let d = json!({ "transcript_path": "C:/x/0f1e2d3c-4b5a-4697-8877-665544332211.jsonl", "session_id": "other-id-1234" });
        assert_eq!(session_key(&d).as_deref(), Some("0f1e2d3c-4b5a-4697-8877-665544332211"));
        assert_eq!(session_key(&json!({ "session_id": "abcd1234" })).as_deref(), Some("abcd1234"));
        assert_eq!(session_key(&json!({ "session_id": "short" })), None);
        assert_eq!(session_key(&json!({ "session_id": "../../evil" })), None);
    }
}
