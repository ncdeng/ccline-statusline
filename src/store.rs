//! JSON state files, and child processes with a time limit.

use serde_json::Value;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

pub fn read_json(file: &Path) -> Option<Value> {
    let text = fs::read_to_string(file).ok()?;
    serde_json::from_str(&text).ok()
}

/// Writes to a temporary file, then renames it over the old one, so a reader
/// never sees half a file.
pub fn write_json_atomic(file: &Path, v: &Value) {
    let Ok(text) = serde_json::to_string(v) else { return };
    let mut tmp = file.as_os_str().to_owned();
    tmp.push(format!(".{}.tmp", std::process::id()));
    let tmp = Path::new(&tmp);
    let ok = file.parent().is_none_or(|d| fs::create_dir_all(d).is_ok())
        && fs::write(tmp, text).is_ok()
        && fs::rename(tmp, file).is_ok();
    if !ok {
        let _ = fs::remove_file(tmp);
    }
}

pub enum RunResult {
    /// The process exited: whether it succeeded, and its stdout.
    Exited { success: bool, stdout: Vec<u8> },
    /// The time limit passed and the process was killed: its stdout so far.
    TimedOut { stdout: Vec<u8> },
    /// The process could not start.
    Failed,
}

/// Runs a command with stdin `input`, and kills it after `limit`.
pub fn run(mut cmd: Command, input: Option<&[u8]>, limit: Duration) -> RunResult {
    let deadline = Instant::now() + limit;
    cmd.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let Ok(mut child) = cmd.spawn() else { return RunResult::Failed };
    if let (Some(data), Some(mut stdin)) = (input, child.stdin.take()) {
        let data = data.to_vec();
        thread::spawn(move || {
            let _ = stdin.write_all(&data);
        });
    }
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    if let Some(mut out) = child.stdout.take() {
        thread::spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            loop {
                match out.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
    } else {
        drop(tx);
    }
    // Collect stdout until it closes, then wait for the exit.
    let mut stdout = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(chunk) => stdout.extend_from_slice(&chunk),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = child.kill();
                let _ = child.wait();
                return RunResult::TimedOut { stdout };
            }
        }
    }
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return RunResult::Exited { success: status.success(), stdout },
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(1)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return RunResult::TimedOut { stdout };
            }
            Err(_) => return RunResult::Failed,
        }
    }
}
