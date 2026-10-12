//! The model / directory / git / cost segments, drawn like CCometixLine
//! (ccline) 1.1.2 from its ~/.claude/ccline/config.toml and models.toml.

use crate::git::{git_info, git_marks};
use crate::js;
use crate::ledger::Ledger;
use crate::paths;
use crate::store::{run, RunResult};
use regex_lite::Regex;
use serde_json::{Map, Value};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// Used when ~/.claude/ccline has no config.toml or models.toml.
const DEFAULT_CONFIG: &str = include_str!("../config.toml");
const DEFAULT_MODELS: &str = include_str!("../models.toml");

/// The separator ccline.exe puts between its segments.
pub const CCLINE_SEP: &str = "\x1b[37m | \x1b[0m";

// ---- a small TOML reader: enough for ccline's config.toml and models.toml ----

enum Seg {
    Key(String),
    Index(usize),
}

fn resolve<'a>(root: &'a mut Value, path: &[Seg]) -> Option<&'a mut Value> {
    let mut v = root;
    for s in path {
        v = match s {
            Seg::Key(k) => v.get_mut(k.as_str())?,
            Seg::Index(i) => v.get_mut(*i)?,
        };
    }
    Some(v)
}

fn is_key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// The table name in "[name]" or "[[name]]".
fn header<'a>(line: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let inner = line.strip_prefix(open)?.strip_suffix(close)?.trim();
    (!inner.is_empty() && inner.chars().all(|c| is_key_char(c) || c == '.')).then_some(inner)
}

fn key_value(line: &str) -> Option<(&str, &str)> {
    let end = line.find(|c: char| !is_key_char(c)).unwrap_or(line.len());
    if end == 0 {
        return None;
    }
    let rest = line[end..].trim_start().strip_prefix('=')?;
    Some((&line[..end], rest.trim_start()))
}

/// The table a header opens; None when it has nowhere to go (writes are dropped).
fn toml_table(root: &mut Value, dotted: &str, is_array_item: bool) -> Option<Vec<Seg>> {
    let keys: Vec<&str> = dotted.split('.').collect();
    let mut path = Vec::new();
    for (i, k) in keys.iter().enumerate() {
        let Some(Value::Object(t)) = resolve(root, &path) else { return None };
        if is_array_item && i == keys.len() - 1 {
            let e = t.entry(*k).or_insert_with(|| Value::Array(Vec::new()));
            if !e.is_array() {
                *e = Value::Array(Vec::new());
            }
            if let Value::Array(a) = e {
                a.push(Value::Object(Map::new()));
            }
        } else if t.get(*k).is_none_or(Value::is_null) {
            t.insert((*k).to_string(), Value::Object(Map::new()));
        }
        path.push(Seg::Key((*k).to_string()));
        if let Some(Value::Array(a)) = t.get(*k) {
            if a.is_empty() {
                return None;
            }
            path.push(Seg::Index(a.len() - 1));
        }
    }
    Some(path)
}

/// Replaces "\UXXXXXXXX" with its character, before the JSON string rules read the rest.
fn replace_big_u(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("\\U") {
        let hex = rest.get(i + 2..i + 10).filter(|h| h.len() == 8 && h.bytes().all(|b| b.is_ascii_hexdigit()));
        let ch = hex.and_then(|h| u32::from_str_radix(h, 16).ok()).and_then(char::from_u32);
        out.push_str(&rest[..i]);
        if let Some(c) = ch {
            out.push(c);
            rest = &rest[i + 10..];
        } else {
            out.push_str("\\U");
            rest = &rest[i + 2..];
        }
    }
    out.push_str(rest);
    out
}

/// `.replace(/\s+#.*$/, '')`: cuts the line at the first whitespace run followed by "#".
fn strip_comment(s: &str) -> &str {
    let mut run_start = None;
    for (i, c) in s.char_indices() {
        if c.is_whitespace() {
            run_start.get_or_insert(i);
        } else {
            if c == '#' {
                if let Some(r) = run_start {
                    return &s[..r];
                }
            }
            run_start = None;
        }
    }
    s
}

fn toml_value(s: &str) -> Value {
    if let Some(rest) = s.strip_prefix('"') {
        let b = rest.as_bytes();
        let mut i = 0;
        let mut end = None;
        while i < b.len() {
            match b[i] {
                b'\\' if i + 1 < b.len() => i += 2,
                b'\\' => break,
                b'"' => {
                    end = Some(i);
                    break;
                }
                _ => i += 1,
            }
        }
        if let Some(e) = end.filter(|e| rest.is_char_boundary(*e)) {
            let body = &rest[..e];
            let json = format!("\"{}\"", replace_big_u(body));
            return serde_json::from_str::<String>(&json)
                .map_or_else(|_| Value::String(body.to_string()), Value::String);
        }
    }
    if let Some(rest) = s.strip_prefix('\'') {
        if let Some(e) = rest.find('\'') {
            return Value::String(rest[..e].to_string());
        }
    }
    let v = strip_comment(s).trim();
    match v {
        "true" => return Value::Bool(true),
        "false" => return Value::Bool(false),
        _ => {}
    }
    let n = js::parse_number(v);
    if !v.is_empty() && n.is_finite() {
        return js::json_num(n);
    }
    Value::String(v.to_string())
}

pub fn parse_toml(text: &str) -> Value {
    let mut root = Value::Object(Map::new());
    let mut table: Option<Vec<Seg>> = Some(Vec::new());
    for raw in text.split('\n') {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = header(line, "[[", "]]") {
            table = toml_table(&mut root, name, true);
        } else if let Some(name) = header(line, "[", "]") {
            table = toml_table(&mut root, name, false);
        } else if let Some((k, v)) = key_value(line) {
            if let Some(Value::Object(t)) = table.as_deref().and_then(|p| resolve(&mut root, p)) {
                t.insert(k.to_string(), toml_value(v));
            }
        }
    }
    root
}

/// A TOML file, or the built-in copy when the file does not exist.
fn read_toml(file: &Path, default: &str) -> Option<Value> {
    match fs::read_to_string(file) {
        Ok(text) => Some(parse_toml(&text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(parse_toml(default)),
        Err(_) => None,
    }
}

/// Whether config.toml asks for plain icons (emoji) instead of Nerd Font ones.
pub fn plain_icons() -> bool {
    let cfg = read_toml(&paths::ccline_dir().join("config.toml"), DEFAULT_CONFIG);
    js::get(cfg.as_ref(), &["style", "mode"]).and_then(Value::as_str) == Some("plain")
}

// ---- segments ----

const NATIVE_SEGMENTS: [&str; 4] = ["model", "directory", "git", "cost"];

/// Enabled segments, or None when config.toml needs something only ccline.exe draws.
fn native_segments(cfg: &Value) -> Option<Vec<Value>> {
    let style = cfg.get("style").filter(|s| js::truthy(Some(s)))?;
    let segments = cfg.get("segments")?.as_array()?;
    let mode = style.get("mode").and_then(Value::as_str);
    if mode != Some("plain") && mode != Some("nerd_font") {
        return None;
    }
    let sep = style.get("separator").and_then(Value::as_str)?;
    if sep == "\u{e0b0}" {
        return None;
    }
    let on: Vec<Value> = segments.iter().filter(|s| js::truthy(s.get("enabled"))).cloned().collect();
    let ok = on.iter().all(|s| {
        s.get("id").and_then(Value::as_str).is_some_and(|id| NATIVE_SEGMENTS.contains(&id))
            && !js::truthy(js::get(Some(s), &["colors", "background"]))
            && !js::truthy(js::get(Some(s), &["options", "show_sha"]))
    });
    ok.then_some(on)
}

/// ccline's AnsiColor: { c16 } | { c256 } | { r, g, b }.
fn fg_code(c: Option<&Value>) -> Option<String> {
    if let Some(n) = js::as_number(js::get(c, &["c16"])) {
        return Some(js::num_str(if n < 8.0 { 30.0 + n } else { 90.0 + (n - 8.0) }));
    }
    if let Some(n) = js::as_number(js::get(c, &["c256"])) {
        return Some(format!("38;5;{}", js::num_str(n)));
    }
    if js::as_number(js::get(c, &["r"])).is_some() {
        let ch = |k: &str| js::template(js::get(c, &[k]));
        return Some(format!("38;2;{};{};{}", ch("r"), ch("g"), ch("b")));
    }
    None
}

fn paint(text: &str, color: Option<&Value>) -> String {
    match fg_code(color) {
        Some(code) => format!("\x1b[{code}m{text}\x1b[0m"),
        None => text.to_string(),
    }
}

fn styled(text: &str, color: Option<&Value>, is_bold: bool) -> String {
    let mut codes = Vec::new();
    if is_bold {
        codes.push("1".to_string());
    }
    codes.extend(fg_code(color));
    if codes.is_empty() {
        text.to_string()
    } else {
        format!("\x1b[{}m{text}\x1b[0m", codes.join(";"))
    }
}

fn render_segment(seg: &Value, mode: &str, primary: &str, secondary: &str) -> String {
    let icon_key = if mode == "plain" { "plain" } else { "nerd_font" };
    let icon = js::get(Some(seg), &["icon", icon_key]).filter(|v| !v.is_null()).map(js::to_string).unwrap_or_default();
    let is_bold = js::truthy(js::get(Some(seg), &["styles", "text_bold"]));
    let text_color = js::get(Some(seg), &["colors", "text"]);
    let mut out =
        format!("{} {}", paint(&icon, js::get(Some(seg), &["colors", "icon"])), styled(primary, text_color, is_bold));
    if !secondary.is_empty() {
        out += &format!(" {}", styled(secondary, text_color, is_bold));
    }
    out
}

// ccline's built-in model entries and context modifiers (after models.toml in priority).
const BUILTIN_MODELS: [(&str, &str); 4] =
    [("glm-4.5", "GLM-4.5"), ("kimi-k2-turbo", "Kimi K2 Turbo"), ("kimi-k2", "Kimi K2"), ("qwen3-coder", "Qwen Coder")];
const BUILTIN_MODIFIERS: [(&str, &str); 1] = [("[1m]", " 1M")];
const MODEL_FAMILIES: [(&str, &str); 3] = [("sonnet", "Sonnet"), ("opus", "Opus"), ("haiku", "Haiku")];

/// Same order as ccline: models.toml, built-in entries, Claude families, then the stdin name.
pub fn model_name(model: Option<&Value>, models: Option<&Value>) -> String {
    let id = js::get(model, &["id"]).filter(|v| !v.is_null()).map(js::to_string).unwrap_or_default();
    let lower = id.to_lowercase();
    let hit =
        |pattern: Option<&Value>| pattern.and_then(Value::as_str).is_some_and(|p| lower.contains(&p.to_lowercase()));
    let list = |key: &str| js::get(models, &[key]).and_then(Value::as_array).cloned().unwrap_or_default();
    let mut base: Option<String> = None;
    let file_hit = list("models").into_iter().find(|e| hit(e.get("pattern")));
    if let Some(e) = file_hit {
        base = e.get("display_name").filter(|v| !v.is_null()).map(js::to_string);
    } else if let Some((_, name)) = BUILTIN_MODELS.iter().find(|(p, _)| lower.contains(p)) {
        base = Some((*name).to_string());
    }
    if base.is_none() {
        for (kw, name) in MODEL_FAMILIES {
            let re = Regex::new(&format!(
                r"(?:(\d{{1,2}})(?:-(\d{{1,2}}))?-{kw}|{kw}-(\d{{1,2}})(?:-(\d{{1,2}}))?)(?:-\d{{3,}}|-[a-z]|\[|$)"
            ));
            let Some(caps) = re.ok().and_then(|r| {
                r.captures(&lower).map(|c| {
                    let g = |i| c.get(i).map(|m| m.as_str().to_string());
                    (g(3).or(g(1)), g(4).or(g(2)))
                })
            }) else {
                continue;
            };
            let major = caps.0.unwrap_or_default();
            base = Some(match caps.1 {
                Some(minor) => format!("{name} {major}.{minor}"),
                None => format!("{name} {major}"),
            });
            break;
        }
    }
    let file_mod = list("context_modifiers").into_iter().find(|e| hit(e.get("pattern")));
    let suffix = match file_mod {
        Some(e) => e.get("display_suffix").filter(|v| !v.is_null()).map(js::to_string).unwrap_or_default(),
        None => {
            BUILTIN_MODIFIERS.iter().find(|(p, _)| lower.contains(p)).map(|(_, s)| (*s).to_string()).unwrap_or_default()
        }
    };
    let name = base.unwrap_or_else(|| {
        let d = js::get(model, &["display_name"]);
        if js::truthy(d) {
            d.map(js::to_string).unwrap_or_default()
        } else {
            id.clone()
        }
    });
    name + &suffix
}

/// The last part of the work dir, split on "\" first, then on "/".
pub fn dir_name(p: &str) -> String {
    let unix = p.rsplit('/').next().unwrap_or(p);
    let win = p.rsplit('\\').next().unwrap_or(p);
    let name = if win.len() < p.len() {
        win
    } else if unix.len() < p.len() {
        unix
    } else {
        p
    };
    if name.is_empty() {
        "root".into()
    } else {
        name.into()
    }
}

/// "$12.34", rounded as Rust (and ccline) round: an exact tie goes to even.
pub fn cost_text(ledger: Option<&Ledger>) -> Option<String> {
    let c = ledger?.usd?;
    Some(if c < 0.01 { "$0".into() } else { format!("${c:.2}") })
}

/// Fallback: the real ccline.exe, for config.toml settings not drawn in this process.
fn run_ccline(raw: &str) -> String {
    match run(Command::new(paths::ccline_exe()), Some(raw.as_bytes()), Duration::from_millis(1500)) {
        RunResult::Exited { stdout, .. } | RunResult::TimedOut { stdout } => {
            String::from_utf8_lossy(&stdout).into_owned()
        }
        RunResult::Failed => String::new(),
    }
}

/// The ccline segments as one string, and the separator between them.
pub fn render_ccline(raw: &str, data: &Value, ledger: Option<&Ledger>) -> (String, String) {
    let dir = paths::ccline_dir();
    let cfg = read_toml(&dir.join("config.toml"), DEFAULT_CONFIG);
    let segs = cfg.as_ref().and_then(native_segments);
    let (Some(cfg), Some(segs)) = (cfg, segs) else {
        return (run_ccline(raw), CCLINE_SEP.to_string());
    };
    let mode = js::get(Some(&cfg), &["style", "mode"]).and_then(Value::as_str).unwrap_or("plain");
    let cwd = js::get(Some(data), &["workspace", "current_dir"]).and_then(Value::as_str).unwrap_or("");
    let transcript = data.get("transcript_path").and_then(Value::as_str);
    let has_git = segs.iter().any(|s| s.get("id").and_then(Value::as_str) == Some("git"));
    let git = if has_git { git_info(cwd, transcript) } else { None };
    let models = read_toml(&dir.join("models.toml"), DEFAULT_MODELS);
    let mut out = Vec::new();
    for s in &segs {
        match s.get("id").and_then(Value::as_str) {
            Some("model") => out.push(render_segment(s, mode, &model_name(data.get("model"), models.as_ref()), "")),
            Some("directory") if !cwd.is_empty() => out.push(render_segment(s, mode, &dir_name(cwd), "")),
            Some("git") => {
                if let Some(g) = &git {
                    out.push(render_segment(s, mode, g.head.as_deref().unwrap_or("detached"), &git_marks(g)));
                }
            }
            Some("cost") => {
                if let Some(c) = cost_text(ledger) {
                    out.push(render_segment(s, mode, &c, ""));
                }
            }
            _ => {}
        }
    }
    let sep_text = js::get(Some(&cfg), &["style", "separator"]).and_then(Value::as_str).unwrap_or(" | ");
    let sep = format!("\x1b[37m{sep_text}\x1b[0m");
    (out.join(&sep), sep)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_ccline_config() {
        let cfg = parse_toml(DEFAULT_CONFIG);
        assert_eq!(cfg["style"]["mode"], "nerd_font");
        assert_eq!(cfg["segments"][0]["id"], "model");
        assert_eq!(cfg["segments"][0]["colors"]["icon"]["c16"], 14);
        assert_eq!(cfg["segments"][0]["styles"]["text_bold"], true);
        assert_eq!(native_segments(&cfg).map(|s| s.len()), Some(4));
    }

    #[test]
    fn toml_values() {
        assert_eq!(toml_value("\"a\\tb\" # c"), json!("a\tb"));
        assert_eq!(toml_value("\"\\U0001F600\""), json!("😀"));
        assert_eq!(toml_value("'lit'"), json!("lit"));
        assert_eq!(toml_value("3 # three"), json!(3));
        assert_eq!(toml_value("x#y"), json!("x#y"));
    }

    #[test]
    fn model_names() {
        let m = |id: &str| model_name(Some(&json!({ "id": id, "display_name": "D" })), None);
        assert_eq!(m("claude-opus-5-5"), "Opus 5.5");
        assert_eq!(m("claude-opus-5-5[1m]"), "Opus 5.5 1M");
        assert_eq!(m("claude-sonnet-4-5-20250929"), "Sonnet 4.5");
        assert_eq!(m("claude-3-5-haiku-20241022"), "Haiku 3.5");
        assert_eq!(m("claude-fable-5-1"), "D");
        let models = parse_toml(DEFAULT_MODELS);
        assert_eq!(model_name(Some(&json!({ "id": "claude-fable-5-1" })), Some(&models)), "Fable 5.1");
    }

    #[test]
    fn dir_names() {
        assert_eq!(dir_name("D:/my-project"), "my-project");
        assert_eq!(dir_name("D:\\a\\b"), "b");
        assert_eq!(dir_name("/"), "root");
    }
}
