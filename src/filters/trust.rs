//! Trust gate for repo-controlled inputs: nothing a cloned repository ships
//! is acted on until `tokenix trust` records its SHA-256.

use std::collections::HashMap;
use std::path::PathBuf;

/// Path of the JSON trust store gating repo-local filter files.
pub fn trust_store_path() -> PathBuf {
    crate::store::global_dir()
        .unwrap_or_else(|| PathBuf::from(".tokenix"))
        .join("trusted_filters.json")
}

/// Every repo-controlled file that tokenix will *act on* — rewrite command
/// output with, scan with, or spawn a process from — keyed by a repo-relative
/// label and mapped to its SHA-256.
///
/// The gate covers more than `.tokenix/filters` because more than filters is
/// dangerous in a cloned repository:
///
/// * `.tokenix/filters/*.toml` — decides what the agent gets to see.
/// * `.tokenix/secret-rules/*.toml`, `.tokenix/egress-rules/*.toml` — add
///   scanner rules; a `.+` pattern would blank every tool result the
///   PostToolUse redactor touches, or bury real egress in noise.
/// * `.mcp.json`, `opencode.json`, `.vscode/mcp.json` — `prompt-audit` and
///   `session-audit` introspect MCP servers by **spawning** them, so a repo
///   that ships one of these picks the command that runs.
pub fn repo_input_hashes(root: &std::path::Path) -> std::collections::BTreeMap<String, String> {
    use sha2::{Digest, Sha256};
    let mut hashes = std::collections::BTreeMap::new();
    let mut add = |label: String, path: &std::path::Path| {
        if let Ok(content) = std::fs::read(path) {
            hashes.insert(label, hex::encode(Sha256::digest(&content)));
        }
    };

    for sub in ["filters", "secret-rules", "egress-rules"] {
        let dir = root.join(".tokenix").join(sub);
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                    continue;
                }
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    add(format!(".tokenix/{sub}/{name}"), &path);
                }
            }
        }
    }

    for rel in [".mcp.json", "opencode.json"] {
        add(rel.to_string(), &root.join(rel));
    }
    add(
        ".vscode/mcp.json".to_string(),
        &root.join(".vscode").join("mcp.json"),
    );

    hashes
}

fn load_trust_store() -> HashMap<String, std::collections::BTreeMap<String, String>> {
    std::fs::read_to_string(trust_store_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// True when every repo-controlled input matches the hash recorded by
/// `tokenix trust` for this repo. New, edited, or never-trusted files fail
/// closed: a cloned repository must not be able to rewrite command output,
/// silence the secret scanner, or choose a command tokenix spawns until a
/// human approves it.
pub fn repo_inputs_trusted(root: &std::path::Path) -> bool {
    let current = repo_input_hashes(root);
    if current.is_empty() {
        return true; // nothing to trust
    }
    let store = load_trust_store();
    match store.get(&root.to_string_lossy().to_string()) {
        Some(trusted) => *trusted == current,
        None => false,
    }
}

/// Record the current repo-input hashes as trusted (or remove the entry with
/// `trust = false`). Returns the number of files affected.
pub fn set_repo_inputs_trust(root: &std::path::Path, trust: bool) -> std::io::Result<usize> {
    let mut store = load_trust_store();
    let key = root.to_string_lossy().to_string();
    let count;
    if trust {
        let current = repo_input_hashes(root);
        count = current.len();
        store.insert(key, current);
    } else {
        count = store.remove(&key).map(|m| m.len()).unwrap_or(0);
    }
    let path = trust_store_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&store).unwrap_or_default(),
    )?;
    crate::store::restrict_to_owner(&path);
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_input_hashes_cover_every_executable_repo_input() {
        // The gate is only as wide as this list: anything a cloned repo can put
        // in the tree that tokenix will rewrite output with, scan with, or
        // spawn from must be hashed, or `tokenix trust` approves less than it
        // appears to.
        let dir = std::env::temp_dir().join(format!("tokenix-gate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".tokenix").join("filters")).unwrap();
        std::fs::create_dir_all(dir.join(".tokenix").join("secret-rules")).unwrap();
        std::fs::create_dir_all(dir.join(".tokenix").join("egress-rules")).unwrap();
        std::fs::create_dir_all(dir.join(".vscode")).unwrap();
        std::fs::write(dir.join(".tokenix/filters/a.toml"), "x").unwrap();
        std::fs::write(dir.join(".tokenix/secret-rules/b.toml"), "x").unwrap();
        std::fs::write(dir.join(".tokenix/egress-rules/c.toml"), "x").unwrap();
        std::fs::write(dir.join(".mcp.json"), "{}").unwrap();
        std::fs::write(dir.join("opencode.json"), "{}").unwrap();
        std::fs::write(dir.join(".vscode/mcp.json"), "{}").unwrap();

        let hashes = repo_input_hashes(&dir);
        for expected in [
            ".tokenix/filters/a.toml",
            ".tokenix/secret-rules/b.toml",
            ".tokenix/egress-rules/c.toml",
            ".mcp.json",
            "opencode.json",
            ".vscode/mcp.json",
        ] {
            assert!(
                hashes.contains_key(expected),
                "{expected} not gated: {hashes:?}"
            );
        }
        // Never trusted by default: a fresh clone must fail closed.
        assert!(
            !repo_inputs_trusted(&dir),
            "a repo with executable inputs must not be trusted until approved"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
