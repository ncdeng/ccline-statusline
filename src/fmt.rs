//! Number formats, and the context and effort segments.

use crate::ansi::{bold, by_load, cyan, dim, mag};
use crate::js;
use serde_json::Value;

/// Token counts: "950", "12.5k", "1.2M", "2.31B".
pub fn fmt_k(n: f64) -> String {
    let x = if n.is_nan() { 0.0 } else { n };
    let unit = |v: f64, suffix: &str| {
        if v == v.trunc() {
            format!("{}{suffix}", js::num_str(v))
        } else {
            format!("{}{suffix}", strip_zeros(&js::to_fixed(v, 2)))
        }
    };
    // A value that would round up to 1000 of a unit shows in the next one ("1B", not "1000M").
    if x >= 999_995_000.0 {
        return unit(x / 1e9, "B");
    }
    if x >= 999_950.0 {
        return unit(x / 1e6, "M");
    }
    if x >= 1000.0 {
        let k = x / 1000.0;
        return if k == k.trunc() { format!("{}k", js::num_str(k)) } else { format!("{}k", js::to_fixed(k, 1)) };
    }
    js::num_str(js::round(x))
}

/// `.replace(/\.?0+$/, '')` on a number with decimals.
fn strip_zeros(s: &str) -> String {
    if !s.contains('.') {
        return s.to_string();
    }
    let t = s.trim_end_matches('0');
    t.strip_suffix('.').unwrap_or(t).to_string()
}

/// Dollars: "$4.56", "$12", "$1.3k".
pub fn fmt_usd(x: f64) -> String {
    if x >= 1000.0 {
        return format!("${}k", js::to_fixed(x / 1000.0, 1));
    }
    if x >= 10.0 {
        format!("${}", js::num_str(js::round(x)))
    } else {
        format!("${}", js::to_fixed(x, 2))
    }
}

// Material Design range: also renders on the Mac.
const CTX_ICON: &str = "\u{F01BC}";
// For `mode = "plain"`, without a Nerd Font.
const CTX_ICON_PLAIN: &str = "🧠";

/// The current session's context, from Claude Code's stdin.
pub fn fmt_context(cw: Option<&Value>, plain: bool) -> String {
    let size = js::number_or0(js::get(cw, &["context_window_size"]));
    let used = js::number_or0(js::get(cw, &["total_input_tokens"]));
    let raw = js::get(cw, &["used_percentage"]);
    let pct = match raw {
        None | Some(Value::Null) => {
            if size != 0.0 {
                used / size * 100.0
            } else {
                0.0
            }
        }
        Some(Value::String(s)) if s.is_empty() => {
            if size != 0.0 {
                used / size * 100.0
            } else {
                0.0
            }
        }
        v => js::number_or0(v),
    };
    let pct_str = if (pct - js::round(pct)).abs() < 0.05 {
        format!("{}%", js::num_str(js::round(pct)))
    } else {
        format!("{}%", js::to_fixed(pct, 1))
    };
    let token_str =
        if size != 0.0 { format!("{}/{}", fmt_k(used), fmt_k(size)) } else { format!("{} tokens", fmt_k(used)) };
    let icon = if plain { CTX_ICON_PLAIN } else { CTX_ICON };
    format!("{} {} {}", mag(icon), by_load(js::round(pct), &pct_str), dim(&format!("· {token_str}")))
}

/// The effort level, shown after the model name.
pub fn fmt_effort(data: &Value) -> String {
    let level = js::get(Some(data), &["effort", "level"]);
    let summary = js::get(Some(data), &["model", "param_summary"]);
    let raw = if js::truthy(level) {
        level.map(js::to_string).unwrap_or_default()
    } else if js::truthy(summary) {
        summary.map(js::to_string).unwrap_or_default()
    } else {
        String::new()
    };
    let level = raw.replace(['(', ')'], "").trim().to_lowercase();
    if level.is_empty() || level == "unset" || level == "null" {
        return String::new();
    }
    bold(&cyan(&level))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_units() {
        assert_eq!(fmt_k(950.0), "950");
        assert_eq!(fmt_k(1000.0), "1k");
        assert_eq!(fmt_k(466_600.0), "466.6k");
        assert_eq!(fmt_k(999_950.0), "1M");
        assert_eq!(fmt_k(1_000_000.0), "1M");
        assert_eq!(fmt_k(2_310_000_000.0), "2.31B");
        assert_eq!(fmt_k(4_500_000_000.0), "4.5B");
        assert_eq!(fmt_k(999_994_000.0), "999.99M");
        assert_eq!(fmt_k(999_996_000.0), "1B");
        assert_eq!(fmt_k(f64::NAN), "0");
    }

    #[test]
    fn dollars() {
        assert_eq!(fmt_usd(4.561), "$4.56");
        assert_eq!(fmt_usd(12.5), "$13");
        assert_eq!(fmt_usd(1290.0), "$1.3k");
        assert_eq!(fmt_usd(0.0), "$0.00");
    }
}
