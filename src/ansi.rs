//! ANSI colors, the terminal width of text, and line wrapping.

use unicode_width::UnicodeWidthChar;

pub fn sgr(code: &str, s: &str) -> String {
    format!("\x1b[{code}m{s}\x1b[0m")
}

pub fn dim(s: &str) -> String {
    sgr("2", s)
}

pub fn green(s: &str) -> String {
    sgr("32", s)
}

pub fn yellow(s: &str) -> String {
    sgr("33", s)
}

pub fn red(s: &str) -> String {
    sgr("31", s)
}

pub fn mag(s: &str) -> String {
    sgr("95", s)
}

pub fn cyan(s: &str) -> String {
    sgr("96", s)
}

pub fn bold(s: &str) -> String {
    sgr("1", s)
}

pub fn by_load(pct: f64, text: &str) -> String {
    if pct >= 85.0 {
        red(text)
    } else if pct >= 60.0 {
        yellow(text)
    } else {
        green(text)
    }
}

/// Removes SGR codes (`ESC [ … m`) and OSC 8 hyperlinks.
pub fn strip_ansi(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == 0x1b {
            if b.get(i + 1) == Some(&b'[') {
                let mut j = i + 2;
                while j < b.len() && (b[j].is_ascii_digit() || b[j] == b';') {
                    j += 1;
                }
                if b.get(j) == Some(&b'm') {
                    i = j + 1;
                    continue;
                }
            } else if b[i + 1..].starts_with(b"]8;") {
                let mut j = i + 4;
                while j < b.len() && b[j] != 0x07 && b[j] != 0x1b {
                    j += 1;
                }
                if b.get(j) == Some(&0x07) {
                    i = j + 1;
                    continue;
                }
                if b.get(j) == Some(&0x1b) && b.get(j + 1) == Some(&b'\\') {
                    i = j + 2;
                    continue;
                }
            }
        }
        let Some(c) = s[i..].chars().next() else { break };
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// Terminal columns of a string: ANSI codes are 0, CJK and emoji are 2,
/// and a character followed by VS16 (U+FE0F) is 2.
pub fn text_width(s: &str) -> usize {
    let mut w = 0;
    let mut prev = 0;
    for c in strip_ansi(s).chars() {
        if c == '\u{FE0F}' {
            if prev == 1 {
                w += 1;
                prev = 2;
            }
            continue;
        }
        let cw = if c.is_control() { 0 } else { c.width().unwrap_or(0) };
        if cw > 0 {
            w += cw;
            prev = cw;
        }
    }
    w
}

/// Columns for the status line. Claude Code puts its own width into COLUMNS
/// for each run and pads the footer by 2 columns on each side; a wider line
/// is cut with "…".
pub fn line_width() -> f64 {
    const FOOTER_PADDING: f64 = 4.0;
    let cols = std::env::var("COLUMNS").map_or(f64::NAN, |s| crate::js::parse_number(&s));
    if cols.is_finite() && cols > FOOTER_PADDING {
        cols - FOOTER_PADDING
    } else {
        f64::INFINITY
    }
}

/// `sep` goes before the unit; `parts` are joined with the part separator.
pub struct Unit {
    pub sep: String,
    pub parts: Vec<String>,
}

/// A unit moves to the next line as a whole; only a unit wider than a line
/// is split. When everything fits, the result is one line.
pub fn wrap_units(units: &[Unit], width: f64, part_sep: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0usize;
    let mut place = |sep: &str, text: &str| {
        let w = text_width(text);
        let sep_w = text_width(sep);
        if !cur.is_empty() && (cur_w + sep_w + w) as f64 > width {
            lines.push(std::mem::take(&mut cur));
            cur_w = 0;
        }
        if cur.is_empty() {
            cur = text.to_string();
            cur_w = w;
        } else {
            cur.push_str(sep);
            cur.push_str(text);
            cur_w += sep_w + w;
        }
    };
    for u in units {
        let whole = u.parts.join(part_sep);
        if text_width(&whole) as f64 <= width {
            place(&u.sep, &whole);
        } else {
            for (i, p) in u.parts.iter().enumerate() {
                place(if i > 0 { part_sep } else { &u.sep }, p);
            }
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_ignores_ansi_and_counts_wide_chars() {
        assert_eq!(text_width(&red("abc")), 3);
        assert_eq!(text_width("⚡ 39m59s"), 9);
        assert_eq!(text_width("❄️ Expired"), 10);
        assert_eq!(text_width("目录"), 4);
        assert_eq!(text_width("\u{F04D2} Opus"), 6);
        assert_eq!(text_width("\x1b]8;;https://x\x07link\x1b]8;;\x07"), 4);
    }

    #[test]
    fn wraps_whole_units() {
        let u = |s: &str| Unit { sep: " | ".into(), parts: vec![s.into()] };
        let units = [u("aaaa"), u("bbbb"), u("cccc")];
        assert_eq!(wrap_units(&units, 11.0, " | "), ["aaaa | bbbb", "cccc"]);
        assert_eq!(wrap_units(&units, f64::INFINITY, " | "), ["aaaa | bbbb | cccc"]);
    }
}
