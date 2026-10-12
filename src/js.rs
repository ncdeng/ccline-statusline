//! JavaScript's rules for numbers and values. The status line was first a
//! Node.js script; these keep the output and the state files the same.

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime};
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

pub const MINUTE_MS: f64 = 60_000.0;
pub const HOUR_MS: f64 = 60.0 * MINUTE_MS;

/// `Date.now()`.
pub fn now_ms() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as f64).unwrap_or(0.0)
}

/// Milliseconds since the epoch, as Node's `stat.mtimeMs`.
pub fn time_ms(t: SystemTime) -> f64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as f64 * 1000.0 + f64::from(d.subsec_nanos()) / 1e6,
        Err(_) => 0.0,
    }
}

/// `a?.b?.c`.
pub fn get<'a>(v: Option<&'a Value>, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(v?, |cur, k| cur.get(*k))
}

/// `Number(v)`: NaN for a missing value and for one that is not a number.
pub fn number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => parse_number(s),
        Some(_) => f64::NAN,
    }
}

/// A string to a number as `Number(s)` reads it: "" is 0; decimal notation and "Infinity" only.
pub fn parse_number(s: &str) -> f64 {
    let t = s.trim();
    if t.is_empty() {
        return 0.0;
    }
    let body = t.strip_prefix(['+', '-']).unwrap_or(t);
    if body == "Infinity" {
        return if t.starts_with('-') { f64::NEG_INFINITY } else { f64::INFINITY };
    }
    let ok = body.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-'));
    if !ok {
        return f64::NAN;
    }
    t.parse::<f64>().unwrap_or(f64::NAN)
}

/// `Number(v) || 0`.
pub fn number_or0(v: Option<&Value>) -> f64 {
    let x = number(v);
    if x.is_nan() {
        0.0
    } else {
        x
    }
}

/// The value when `typeof v === 'number'`.
pub fn as_number(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => n.as_f64(),
        _ => None,
    }
}

/// `Boolean(v)`.
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|x| x != 0.0 && !x.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// `String(v)`.
pub fn to_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => num_str(n.as_f64().unwrap_or(f64::NAN)),
        Value::String(s) => s.clone(),
        Value::Array(a) => {
            a.iter().map(|x| if x.is_null() { String::new() } else { to_string(x) }).collect::<Vec<_>>().join(",")
        }
        Value::Object(_) => "[object Object]".into(),
    }
}

/// A value placed in a template string: a missing one prints "undefined".
pub fn template(v: Option<&Value>) -> String {
    v.map_or_else(|| "undefined".into(), to_string)
}

/// A number as JavaScript prints it, for the sizes this program shows.
pub fn num_str(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    if x == 0.0 {
        return "0".into();
    }
    if x == x.trunc() && x.abs() < 1e21 {
        return format!("{x:.0}");
    }
    format!("{x}")
}

/// `Math.round`: a half rounds up, toward +∞.
pub fn round(x: f64) -> f64 {
    let f = x.floor();
    if x - f >= 0.5 {
        f + 1.0
    } else {
        f
    }
}

/// `x.toFixed(d)`. Rust's `{:.d}` rounds an exact tie to even; JavaScript rounds it up.
pub fn to_fixed(x: f64, d: usize) -> String {
    if !x.is_finite() || x.abs() >= 1e21 {
        return num_str(x);
    }
    let neg = x < 0.0;
    let a = x.abs();
    let long = format!("{a:.prec$}", prec = d + 30);
    let (head, tail) = long.split_at(long.len() - 30);
    let s = if tail.starts_with('5') && tail[1..].bytes().all(|b| b == b'0') {
        round_up_digits(head.trim_end_matches('.'))
    } else {
        format!("{a:.d$}")
    };
    if neg {
        format!("-{s}")
    } else {
        s
    }
}

/// Adds one to the last digit of a decimal string, with carry.
fn round_up_digits(s: &str) -> String {
    let mut b = s.as_bytes().to_vec();
    let mut i = b.len();
    loop {
        if i == 0 {
            b.insert(0, b'1');
            break;
        }
        i -= 1;
        match b[i] {
            b'.' => continue,
            b'9' => b[i] = b'0',
            _ => {
                b[i] += 1;
                break;
            }
        }
    }
    String::from_utf8(b).unwrap_or_default()
}

/// A number for a JSON file, written as `JSON.stringify` writes it: no ".0" on whole numbers.
pub fn json_num(x: f64) -> Value {
    if x.is_finite() && x == x.trunc() && x.abs() < 9.0e15 {
        Value::from(x as i64)
    } else {
        serde_json::Number::from_f64(x).map_or(Value::Null, Value::Number)
    }
}

/// `Date.parse(v)` for the ISO 8601 strings Claude Code writes; NaN otherwise.
pub fn date_parse(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::String(s)) => parse_date_str(s),
        _ => f64::NAN,
    }
}

pub fn parse_date_str(s: &str) -> f64 {
    let s = s.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return t.timestamp_millis() as f64;
    }
    // A date alone is UTC; a date and time without an offset is local time.
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        if let Some(t) = d.and_hms_opt(0, 0, 0) {
            return t.and_utc().timestamp_millis() as f64;
        }
    }
    for f in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M"] {
        if let Ok(n) = NaiveDateTime::parse_from_str(s, f) {
            if let Some(t) = n.and_local_timezone(Local).earliest() {
                return t.timestamp_millis() as f64;
            }
        }
    }
    f64::NAN
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_fixed_rounds_ties_up() {
        assert_eq!(to_fixed(0.125, 2), "0.13");
        assert_eq!(to_fixed(2.5, 0), "3");
        assert_eq!(to_fixed(9.995, 2), "9.99"); // 9.99499999…
        assert_eq!(to_fixed(0.5, 0), "1");
        assert_eq!(to_fixed(99.95, 1), "100.0"); // 99.9500000000000028…
        assert_eq!(to_fixed(-0.0, 2), "0.00");
        assert_eq!(to_fixed(1234.5678, 1), "1234.6");
    }

    #[test]
    fn numbers_print_like_javascript() {
        assert_eq!(num_str(14.0), "14");
        assert_eq!(num_str(33.5), "33.5");
        assert_eq!(num_str(-0.0), "0");
        assert_eq!(round(2.5), 3.0);
        assert_eq!(round(-2.5), -2.0);
        assert_eq!(round(0.499_999_999_999_999_94), 0.0);
    }

    #[test]
    fn number_reads_like_javascript() {
        assert_eq!(parse_number(" 80 "), 80.0);
        assert_eq!(parse_number(""), 0.0);
        assert!(parse_number("inf").is_nan());
        assert!(parse_number("abc").is_nan());
        assert_eq!(number(Some(&Value::Null)), 0.0);
        assert!(number(None).is_nan());
    }

    #[test]
    fn json_numbers_drop_point_zero() {
        assert_eq!(serde_json::to_string(&json_num(2.0)).unwrap(), "2");
        assert_eq!(serde_json::to_string(&json_num(1.25)).unwrap(), "1.25");
    }

    #[test]
    fn iso_dates() {
        assert_eq!(parse_date_str("2026-10-12T05:43:38.533Z"), 1_791_783_818_533.0);
        assert!(parse_date_str("nope").is_nan());
    }
}
