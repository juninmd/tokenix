//! Shared enumeration of local AI-agent transcript files.
//!
//! Several commands (`conversation-audit`, `usage`, secrets/egress scans) need
//! the same list of on-disk history files per agent. This is the single source
//! of truth so the directory layout lives in one place.

use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// `(agent_key, root_dir)` pairs for every supported agent. Callers should skip
/// roots whose directory does not exist.
pub fn roots(home: &Path) -> Vec<(&'static str, PathBuf)> {
    vec![
        ("claude", home.join(".claude").join("projects")),
        ("codex", home.join(".codex").join("sessions")),
        ("copilot", home.join(".copilot").join("session-state")),
        ("copilot", home.join(".copilot").join("logs")),
        ("openai", home.join(".openai")),
    ]
}

/// Walk a single agent root and return its transcript files. Copilot package
/// internals are skipped unless the filename looks like a session.
pub fn transcript_files(root: &Path, agent: &str) -> Vec<PathBuf> {
    WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| {
            let p = e.into_path();
            let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("");
            let name = p.file_name().and_then(|x| x.to_str()).unwrap_or("");
            let keep = matches!(ext, "jsonl" | "json" | "log" | "txt")
                && !(agent == "copilot"
                    && p.components().any(|c| c.as_os_str() == "pkg")
                    && !name.contains("session"));
            keep.then_some(p)
        })
        .collect()
}
