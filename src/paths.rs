//! Files under ~/.claude.

use std::path::PathBuf;
use std::sync::OnceLock;

/// The home folder, as Node's os.homedir() finds it.
pub fn home() -> &'static PathBuf {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
        let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        std::env::var_os(var).filter(|v| !v.is_empty()).map_or_else(|| PathBuf::from("."), PathBuf::from)
    })
}

pub fn claude_dir() -> PathBuf {
    home().join(".claude")
}

/// ~/.claude/ccline: CCometixLine's folder, shared with it.
pub fn ccline_dir() -> PathBuf {
    claude_dir().join("ccline")
}

/// CCometixLine itself, run only for config.toml settings this program does not draw.
pub fn ccline_exe() -> PathBuf {
    ccline_dir().join(if cfg!(windows) { "ccline.exe" } else { "ccline" })
}

pub fn quota_cache() -> PathBuf {
    ccline_dir().join("rate-limits-cache.json")
}

pub fn ccline_api_cache() -> PathBuf {
    ccline_dir().join(".api_usage_cache.json")
}

pub fn week_spend() -> PathBuf {
    ccline_dir().join("week-spend.json")
}

pub fn session_cost_dir() -> PathBuf {
    ccline_dir().join("session-cost")
}

pub fn git_cache_dir() -> PathBuf {
    ccline_dir().join("git-cache")
}

pub fn projects() -> PathBuf {
    claude_dir().join("projects")
}
