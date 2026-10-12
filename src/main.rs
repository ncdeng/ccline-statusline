//! Claude Code status line, based on Haleclipse's CCometixLine (MIT).
//!
//! The model / directory / git / cost segments are drawn like ccline 1.1.2,
//! from ~/.claude/ccline/config.toml and models.toml; ccline itself runs only
//! when config.toml asks for something not drawn here. Added: a session cost
//! that survives restarts, the prompt-cache countdown, output speed and time
//! to first token, and the 5h/7d quota with this week's spend and the weekly
//! limit it implies. When the line is wider than the terminal (COLUMNS),
//! whole segments move to the next line, so Claude Code does not cut the end
//! off with "…".

mod ansi;
mod ccline;
mod fmt;
mod git;
mod js;
mod ledger;
mod paths;
mod quota;
mod store;
mod transcript;

use ansi::{dim, red, yellow, Unit};
use serde_json::Value;
use std::io::{IsTerminal, Read, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use transcript::{scan_transcript, Tail};

/// Runs `f`; a panic gives None instead of ending the program.
fn guard<T>(f: impl FnOnce() -> T) -> Option<T> {
    catch_unwind(AssertUnwindSafe(f)).ok()
}

/// stdin, for at most 2 s: Claude Code may never close it.
fn read_stdin() -> String {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return String::new();
    }
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    thread::spawn(move || {
        let mut lock = stdin.lock();
        let mut buf = [0u8; 16 * 1024];
        loop {
            match lock.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut bytes = Vec::new();
    while let Ok(chunk) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

// ---- prompt-cache hit ratio and misses ----

// CC's hit ratio covers the running process, whose first requests after a
// restart may have to write the whole cache again.
const HIT_MIN_REQUESTS: f64 = 10.0;
const HIT_WARN: f64 = 0.95;
const RECENT_MISS_MS: f64 = 10.0 * js::MINUTE_MS;

fn miss_cause_label(c: &str) -> &str {
    match c {
        "system_prompt_changed" => "system",
        "tools_changed" => "tools",
        "model_changed" => "model",
        "fast_mode_changed" => "fast mode",
        "cache_scope_or_ttl_changed" => "ttl",
        "betas_changed" => "betas",
        "effort_changed" => "effort",
        "thinking_mode_changed" => "thinking",
        "thinking_display_changed" => "thinking display",
        "auto_mode_changed" => "auto mode",
        "overage_changed" => "overage",
        "extra_body_changed" => "extra body",
        "defer_loading_changed" => "deferred tools",
        "messages_rewritten" => "history edited",
        "ttl_expired_5m" => "idle >5m",
        "ttl_expired_1h" => "idle >1h",
        "likely_server_side" => "server side",
        other => other,
    }
}

/// After the cache countdown, only when something is off: the hit ratio when
/// it is under 95% after 10 requests, and in the 10 minutes after a cache miss
/// its cause and the tokens it wrote again (" · miss: tools +466k").
fn fmt_cache_extras(pc: Option<&Value>, ledger: Option<&ledger::Ledger>) -> String {
    let mut s = String::new();
    if let Some(hit) = js::as_number(js::get(pc, &["hit_ratio"])) {
        if js::number_or0(js::get(pc, &["requests"])) >= HIT_MIN_REQUESTS && hit < HIT_WARN {
            let pct = format!("{}%", js::to_fixed(hit * 100.0, 1));
            s += &dim(" · hit ");
            s += &if hit >= 0.85 { yellow(&pct) } else { red(&pct) };
        }
    }
    let at = js::number(js::get(pc, &["last_miss_at"])) * 1000.0;
    if at > 0.0 && js::now_ms() - at < RECENT_MISS_MS {
        let causes = js::get(pc, &["last_miss_cause", "causes"]).and_then(Value::as_array);
        let why = causes
            .map(|cs| cs.iter().map(|c| miss_cause_label(&js::to_string(c)).to_string()).collect::<Vec<_>>().join("+"))
            .filter(|w| !w.is_empty())
            .unwrap_or_else(|| "unknown".into());
        let n = ledger.and_then(|l| l.last_miss_tokens).filter(|n| *n > 0.0);
        let size = n.map(|n| format!(" +{}", fmt::fmt_k(n))).unwrap_or_default();
        s += &dim(" · ");
        s += &red(&format!("miss: {why}{size}"));
    }
    s
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("ccline-statusline {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    std::panic::set_hook(Box::new(|_| {}));
    // Hard cap: never linger, even if a child process hangs.
    thread::spawn(|| {
        thread::sleep(Duration::from_secs(5));
        std::process::exit(0);
    });

    let raw = read_stdin();
    let data: Value = serde_json::from_str(if raw.is_empty() { "{}" } else { &raw })
        .ok()
        .filter(Value::is_object)
        .unwrap_or_else(|| Value::Object(Default::default()));

    let ledger = guard(|| ledger::session_ledger(&data));
    let lines = thread::scope(|sc| {
        // git status runs while this thread reads the transcripts.
        let cc = sc.spawn(|| guard(|| ccline::render_ccline(&raw, &data, ledger.as_ref())));

        // The transcript tail is read and parsed once, for the cache countdown and the speed.
        let tp = data.get("transcript_path").and_then(Value::as_str).filter(|s| !s.is_empty());
        let tail = tp.and_then(|p| Tail::read(Path::new(p), 256 * 1024).ok());
        let scan = tail.as_ref().and_then(|t| guard(|| scan_transcript(t.lines())));

        let rl = guard(|| quota::resolve_rate_limits(data.get("rate_limits"))).flatten();
        let week = guard(|| quota::week_spend(js::get(rl.as_ref(), &["seven_day"]))).flatten();
        let quota = guard(|| quota::fmt_quota(rl.as_ref(), week.as_ref())).unwrap_or_default();
        let plain = guard(ccline::plain_icons).unwrap_or(false);
        let ctx = guard(|| fmt::fmt_context(data.get("context_window"), plain)).unwrap_or_default();
        let pc = data.get("prompt_cache");
        let cache_ttl = guard(|| transcript::fmt_cache_ttl(scan.as_ref(), pc)).unwrap_or_default();
        let tps = guard(|| transcript::fmt_tps(tp, tail.as_ref(), scan.as_ref())).unwrap_or_default();
        let cache_seg = if cache_ttl.is_empty() {
            String::new()
        } else {
            cache_ttl + &guard(|| fmt_cache_extras(pc, ledger.as_ref())).unwrap_or_default()
        };

        let (cc_text, cc_sep) = cc.join().ok().flatten().unwrap_or_default();

        // The effort level goes right after the first ccline segment (the model).
        let sep = dim(" | ");
        let cc_text = cc_text.trim_end();
        let mut cc_parts: Vec<String> =
            if cc_text.is_empty() { Vec::new() } else { cc_text.split(cc_sep.as_str()).map(String::from).collect() };
        let effort = fmt::fmt_effort(&data);
        if let Some(first) = cc_parts.first_mut() {
            if !effort.is_empty() {
                *first += &format!(" {effort}");
            }
        }

        let mut units: Vec<Unit> = cc_parts
            .into_iter()
            .enumerate()
            .map(|(i, p)| Unit { sep: if i > 0 { cc_sep.clone() } else { sep.clone() }, parts: vec![p] })
            .collect();
        for s in [ctx, cache_seg, tps] {
            if !s.is_empty() {
                units.push(Unit { sep: sep.clone(), parts: vec![s] });
            }
        }
        units.push(Unit { sep: sep.clone(), parts: quota });
        ansi::wrap_units(&units, ansi::line_width(), &sep)
    });

    if !lines.is_empty() {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(format!("{}\n", lines.join("\n")).as_bytes());
        let _ = out.flush();
    }
    std::process::exit(0);
}
