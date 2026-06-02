//! Recording sessions capture the raw output of real commands into
//! `.tokenix/recordings/<cmd>/NNN.out` so `tokenix filter generate` can build a
//! filter from diverse, realistic samples instead of a single re-run.
//!
//! A session is a marker file (`recordings/session.json`) written by
//! `tokenix filter record start` and removed by `tokenix filter record stop`.
//! While it exists, the PreToolUse hook routes commands through `tokenix run`
//! (the only execution path that sees their raw output under Claude Code), and
//! `tokenix run` / the MCP runner call [`capture`].

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Bound disk use: a recording session left running should not grow without limit.
const MAX_CAPTURES_PER_CMD: usize = 30;
const MAX_BYTES_PER_CAPTURE: usize = 256 * 1024;

#[derive(Serialize, Deserialize, Clone)]
pub struct Session {
    pub started_at: f64,
    /// When set, only this base command is captured; otherwise everything is.
    #[serde(default)]
    pub command: Option<String>,
}

pub fn recordings_dir(repo_root: &Path) -> PathBuf {
    repo_root.join(".tokenix").join("recordings")
}

fn session_path(repo_root: &Path) -> PathBuf {
    recordings_dir(repo_root).join("session.json")
}

pub fn is_active(repo_root: &Path) -> bool {
    session_path(repo_root).exists()
}

pub fn active_session(repo_root: &Path) -> Option<Session> {
    let raw = std::fs::read_to_string(session_path(repo_root)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Begin a session. A named `command` is validated up front because it later
/// becomes a directory name; `None` records every command.
pub fn start(repo_root: &Path, command: Option<String>) -> Result<Session> {
    let command = match command {
        Some(c) => Some(sanitize_base(&c).ok_or_else(|| {
            anyhow::anyhow!("refusing unsafe command name {c:?}: only [A-Za-z0-9._-] allowed")
        })?),
        None => None,
    };
    std::fs::create_dir_all(recordings_dir(repo_root))?;
    let session = Session {
        started_at: crate::compress::now_ts(),
        command,
    };
    std::fs::write(
        session_path(repo_root),
        serde_json::to_string_pretty(&session)?,
    )?;
    Ok(session)
}

pub fn stop(repo_root: &Path) -> Result<()> {
    let p = session_path(repo_root);
    if p.exists() {
        std::fs::remove_file(p)?;
    }
    Ok(())
}

/// True if a session is active and `command` is in scope for it. Used by the
/// hook to decide whether to wrap a command for capture.
pub fn is_in_scope(repo_root: &Path, command: &str) -> bool {
    let Some(session) = active_session(repo_root) else {
        return false;
    };
    match base_of(command) {
        Some(base) => in_scope(&session, &base),
        None => false,
    }
}

/// Best-effort: append the raw output of `command` to its recordings folder.
/// Called from command-execution paths; silently no-ops when not recording.
pub fn capture(repo_root: &Path, command: &str, stdout: &str, stderr: &str) {
    let Some(session) = active_session(repo_root) else {
        return;
    };
    let Some(base) = base_of(command) else {
        return;
    };
    if !in_scope(&session, &base) {
        return;
    }

    let out = stdout.trim();
    let err = stderr.trim();
    if out.is_empty() && err.is_empty() {
        return; // nothing worth learning a filter from
    }

    let mut body = format!("$ {}\n", command.trim());
    if !out.is_empty() {
        body.push_str(stdout);
        if !body.ends_with('\n') {
            body.push('\n');
        }
    }
    if !err.is_empty() {
        body.push_str("--- stderr ---\n");
        body.push_str(stderr);
    }
    let body = truncate_bytes(&body, MAX_BYTES_PER_CAPTURE);

    let dir = recordings_dir(repo_root).join(&base);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let n = count_captures(&dir);
    if n >= MAX_CAPTURES_PER_CMD {
        return;
    }
    let _ = std::fs::write(dir.join(format!("{:03}.out", n + 1)), body);
}

/// Concatenate recorded samples for `base`, capped at `max_bytes`. Returns
/// `(combined_text, files_used)` or `None` when nothing is recorded.
pub fn read_samples(repo_root: &Path, base: &str, max_bytes: usize) -> Option<(String, usize)> {
    let mut files = out_files(&recordings_dir(repo_root).join(base));
    if files.is_empty() {
        return None;
    }
    files.sort();
    let mut combined = String::new();
    let mut used = 0;
    for f in &files {
        let Ok(content) = std::fs::read_to_string(f) else {
            continue;
        };
        combined.push_str(&content);
        if !combined.ends_with('\n') {
            combined.push('\n');
        }
        used += 1;
        if combined.len() >= max_bytes {
            break;
        }
    }
    (used > 0).then_some((combined, used))
}

/// Per-command capture summary `(command, count, total_bytes)`, biggest first.
pub fn summary(repo_root: &Path) -> Vec<(String, usize, u64)> {
    let Ok(rd) = std::fs::read_dir(recordings_dir(repo_root)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in rd.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let files = out_files(&path);
        if files.is_empty() {
            continue;
        }
        let bytes: u64 = files
            .iter()
            .filter_map(|f| std::fs::metadata(f).ok())
            .map(|m| m.len())
            .sum();
        out.push((name.to_string(), files.len(), bytes));
    }
    out.sort_by_key(|(_, _, bytes)| std::cmp::Reverse(*bytes));
    out
}

/// Base command (first whitespace token) if it is a safe identifier.
fn base_of(command: &str) -> Option<String> {
    sanitize_base(command.split_whitespace().next()?)
}

/// Accept only plain executable identifiers — the value becomes a directory
/// name, so reject path separators, shell metacharacters, and traversal.
fn sanitize_base(s: &str) -> Option<String> {
    let mut chars = s.chars();
    let ok = matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && s.len() <= 64;
    ok.then(|| s.to_string())
}

fn in_scope(session: &Session, base: &str) -> bool {
    match &session.command {
        None => true,
        Some(target) => target == base,
    }
}

fn out_files(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "out"))
                .collect()
        })
        .unwrap_or_default()
}

fn count_captures(dir: &Path) -> usize {
    out_files(dir).len()
}

fn truncate_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n... (truncated at {} bytes)\n", &s[..end], max)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Distinct dir per test name so the parallel test runner can't cross-clobber.
    fn tmp(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("tokenix-rec-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn start_capture_read_stop_roundtrip() {
        let root = tmp("roundtrip");
        start(&root, None).unwrap();
        assert!(is_active(&root));
        assert!(is_in_scope(&root, "cargo build --release"));

        capture(&root, "cargo build", "Compiling tokenix\nFinished\n", "");
        capture(
            &root,
            "cargo build --release",
            "Compiling x\n",
            "warning: unused\n",
        );

        let (text, used) = read_samples(&root, "cargo", 64 * 1024).unwrap();
        assert_eq!(used, 2);
        assert!(text.contains("Compiling tokenix"));
        assert!(text.contains("--- stderr ---"));

        let summary = summary(&root);
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].0, "cargo");
        assert_eq!(summary[0].1, 2);

        stop(&root).unwrap();
        assert!(!is_active(&root));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn empty_output_is_not_captured() {
        let root = tmp("empty");
        start(&root, None).unwrap();
        capture(&root, "true", "   \n", "");
        assert!(read_samples(&root, "true", 1024).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn targeted_session_ignores_other_commands() {
        let root = tmp("targeted");
        start(&root, Some("cargo".to_string())).unwrap();
        assert!(is_in_scope(&root, "cargo test"));
        assert!(!is_in_scope(&root, "npm install"));
        capture(&root, "npm install", "added 12 packages\n", "");
        assert!(read_samples(&root, "npm", 1024).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_unsafe_base_names() {
        assert!(sanitize_base("cargo").is_some());
        assert!(sanitize_base("docker-compose").is_some());
        assert!(sanitize_base("../etc/passwd").is_none());
        assert!(sanitize_base("a/b").is_none());
        assert!(sanitize_base("$(whoami)").is_none());
        assert!(sanitize_base("-rf").is_none());
        assert!(sanitize_base("").is_none());
    }

    #[test]
    fn start_rejects_unsafe_target() {
        let root = tmp("unsafe-target");
        assert!(start(&root, Some("../../etc".to_string())).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
