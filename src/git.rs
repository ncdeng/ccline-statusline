//! Git branch and state for the work dir, from one `git status` and a cache.

use crate::js;
use crate::paths;
use crate::store::{read_json, run, write_json_atomic, RunResult};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::Duration;

// Commits, checkouts, adds, fetches and pushes are seen at once through git_stamp().
// Only edits to work-tree files wait for these TTLs.
const GIT_ACTIVE_TTL_MS: f64 = 10_000.0; // the transcript changed since the last git run
const GIT_IDLE_TTL_MS: f64 = 60_000.0;
const GIT_LIMIT: Duration = Duration::from_millis(1500);

#[derive(Serialize, Deserialize, Clone)]
pub struct GitStatus {
    pub head: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u64,
    pub behind: u64,
    /// "clean", "dirty" or "conflicts".
    pub state: String,
}

#[derive(Serialize, Deserialize)]
struct GitCache {
    at: f64,
    stamp: String,
    #[serde(default)]
    info: Option<GitStatus>,
}

/// `path.resolve(base, p)`: an absolute path with "." and ".." removed.
pub fn resolve(base: &Path, p: &Path) -> PathBuf {
    let joined = if p.is_absolute() { p.to_path_buf() } else { base.join(p) };
    let abs = std::path::absolute(&joined).unwrap_or(joined);
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

fn find_git_root(dir: &Path) -> Option<PathBuf> {
    let d = resolve(Path::new("."), dir);
    d.ancestors().find(|a| a.join(".git").exists()).map(Path::to_path_buf)
}

/// The repo's git dir, and the common dir that holds refs (they differ in a worktree).
struct GitDirs {
    git_dir: PathBuf,
    common: PathBuf,
}

fn git_dirs(root: &Path) -> GitDirs {
    let dot_git = root.join(".git");
    let mut git_dir = dot_git.clone();
    if dot_git.is_file() {
        if let Ok(text) = fs::read_to_string(&dot_git) {
            let target = text.lines().filter_map(|l| l.strip_prefix("gitdir:")).map(str::trim).find(|t| !t.is_empty());
            if let Some(t) = target {
                git_dir = resolve(root, Path::new(t));
            }
        }
    }
    let common = fs::read_to_string(git_dir.join("commondir"))
        .map(|c| resolve(&git_dir, Path::new(c.trim())))
        .unwrap_or_else(|_| git_dir.clone());
    GitDirs { git_dir, common }
}

/// Files that change on commit, checkout, add, fetch, pull and push.
fn git_stamp(dirs: &GitDirs, info: Option<&GitStatus>) -> String {
    let mut files = vec![
        dirs.git_dir.join("HEAD"),
        dirs.git_dir.join("index"),
        dirs.common.join("FETCH_HEAD"),
        dirs.common.join("packed-refs"),
    ];
    if let Some(h) = info.and_then(|i| i.head.as_deref()) {
        files.push(dirs.common.join("refs").join("heads").join(h));
    }
    if let Some(u) = info.and_then(|i| i.upstream.as_deref()) {
        files.push(dirs.common.join("refs").join("remotes").join(u));
    }
    files
        .iter()
        .map(|f| match fs::metadata(f) {
            Ok(m) => format!("{}:{}", js::num_str(m.modified().map(js::time_ms).unwrap_or(0.0)), m.len()),
            Err(_) => "-".into(),
        })
        .collect::<Vec<_>>()
        .join("|")
}

/// Branch, upstream, ahead/behind and work-tree state from `git status --porcelain=v2 --branch`.
fn parse_git_status(out: &str) -> GitStatus {
    let mut info = GitStatus { head: None, upstream: None, ahead: 0, behind: 0, state: "clean".into() };
    for raw in out.split('\n') {
        let line = raw.trim_end();
        if let Some(b) = line.strip_prefix("# branch.head ") {
            if b != "(detached)" {
                info.head = Some(b.into());
            }
        } else if let Some(u) = line.strip_prefix("# branch.upstream ") {
            info.upstream = Some(u.into());
        } else if let Some(ab) = line.strip_prefix("# branch.ab ") {
            let mut it = ab.split(' ');
            let a = it.next().and_then(|s| s.strip_prefix('+')).and_then(|s| s.parse().ok());
            let b = it.next().and_then(|s| s.strip_prefix('-')).and_then(|s| s.parse().ok());
            if let (Some(a), Some(b)) = (a, b) {
                info.ahead = a;
                info.behind = b;
            }
        } else if line.starts_with("u ") {
            info.state = "conflicts".into();
        } else if !line.is_empty() && !line.starts_with('#') && info.state == "clean" {
            info.state = "dirty".into();
        }
    }
    info
}

/// The real git.exe, not the cmd\git.exe launcher: one process instead of two.
fn git_bin() -> PathBuf {
    if cfg!(windows) {
        let pf = std::env::var_os("ProgramFiles").unwrap_or_else(|| "C:\\Program Files".into());
        let real = Path::new(&pf).join("Git").join("mingw64").join("bin").join("git.exe");
        if real.exists() {
            return real;
        }
    }
    PathBuf::from("git")
}

enum GitRun {
    Done(Option<GitStatus>),
    TimedOut,
}

/// One git process gives everything ccline got from five.
fn run_git_status(cwd: &Path) -> GitRun {
    let mut cmd = Command::new(git_bin());
    cmd.args(["--no-optional-locks", "status", "--porcelain=v2", "--branch"]).current_dir(cwd);
    match run(cmd, None, GIT_LIMIT) {
        RunResult::Exited { success: true, stdout } => {
            GitRun::Done(Some(parse_git_status(&String::from_utf8_lossy(&stdout))))
        }
        RunResult::Exited { .. } | RunResult::Failed => GitRun::Done(None),
        RunResult::TimedOut { .. } => GitRun::TimedOut,
    }
}

/// Git info for the work dir, from the cache when nothing in the repo has changed.
pub fn git_info(cwd: &str, transcript: Option<&str>) -> Option<GitStatus> {
    if cwd.is_empty() || !Path::new(cwd).exists() {
        return None;
    }
    let root = find_git_root(Path::new(cwd))?;
    let dirs = git_dirs(&root);
    let abs = resolve(Path::new("."), Path::new(cwd));
    let key = sha1_smol::Sha1::from(abs.to_string_lossy().as_bytes()).digest().to_string();
    let file = paths::git_cache_dir().join(format!("{}.json", &key[..16]));
    let cached: Option<GitCache> = read_json(&file).and_then(|v| serde_json::from_value(v).ok());
    let now = js::now_ms();
    if let Some(c) = &cached {
        if c.stamp == git_stamp(&dirs, c.info.as_ref()) {
            let active = transcript
                .and_then(|t| fs::metadata(t).ok())
                .and_then(|m| m.modified().ok())
                .is_some_and(|t| js::time_ms(t) > c.at);
            if now - c.at < if active { GIT_ACTIVE_TTL_MS } else { GIT_IDLE_TTL_MS } {
                return c.info.clone();
            }
        }
    }
    let info = match run_git_status(&abs) {
        GitRun::Done(info) => info,
        GitRun::TimedOut => return cached.and_then(|c| c.info),
    };
    let entry = GitCache { at: now, stamp: git_stamp(&dirs, info.as_ref()), info: info.clone() };
    if let Ok(v) = serde_json::to_value(&entry) {
        write_json_atomic(&file, &v);
    }
    info
}

pub fn git_marks(g: &GitStatus) -> String {
    let mut parts = vec![match g.state.as_str() {
        "conflicts" => "⚠".to_string(),
        "dirty" => "●".to_string(),
        _ => "✓".to_string(),
    }];
    if g.ahead > 0 {
        parts.push(format!("↑{}", g.ahead));
    }
    if g.behind > 0 {
        parts.push(format!("↓{}", g.behind));
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_v2() {
        let out = "# branch.oid abc\n# branch.head main\n# branch.upstream origin/main\n# branch.ab +2 -1\n1 .M N... 100644 100644 100644 a b f.rs\n";
        let g = parse_git_status(out);
        assert_eq!(g.head.as_deref(), Some("main"));
        assert_eq!(g.upstream.as_deref(), Some("origin/main"));
        assert_eq!((g.ahead, g.behind), (2, 1));
        assert_eq!(git_marks(&g), "● ↑2 ↓1");
        let g = parse_git_status("# branch.head (detached)\nu UU N... x\n");
        assert_eq!(g.head, None);
        assert_eq!(git_marks(&g), "⚠");
    }
}
