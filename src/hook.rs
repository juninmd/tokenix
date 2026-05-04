use anyhow::Result;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::chunker::count_tokens;
use crate::query::{format_results, get_file_outline, query_index};
use crate::store::{get_index_age, log_hook_event, HookEvent};

const MAX_INDEX_AGE_SECS: f64 = 3600.0;
const MIN_LINES_FOR_OUTLINE: usize = 200;
const MIN_QUERY_WORDS: usize = 3;

/// Claude Code sends JSON on stdin.
/// Copilot sets COPILOT_TOOL_NAME + COPILOT_TOOL_INPUT env vars (agent mode).
/// We support both.
#[derive(Deserialize, Debug, Default)]
pub struct HookInput {
    #[serde(default)]
    pub tool_name: String,
    #[serde(default)]
    pub tool_input: serde_json::Value,
}

impl HookInput {
    fn from_env() -> Option<Self> {
        let tool_name = std::env::var("COPILOT_TOOL_NAME")
            .or_else(|_| std::env::var("TOOL_NAME"))
            .ok()?;
        let tool_input_raw = std::env::var("COPILOT_TOOL_INPUT")
            .or_else(|_| std::env::var("TOOL_INPUT"))
            .unwrap_or_default();
        let tool_input = serde_json::from_str(&tool_input_raw).unwrap_or(serde_json::Value::Null);
        Some(HookInput { tool_name, tool_input })
    }

    fn from_stdin(raw: &str) -> Option<Self> {
        let clean = raw.trim_start_matches('\u{feff}').trim();
        if clean.is_empty() { return None; }
        serde_json::from_str(clean).ok()
    }
}

fn find_repo_root() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut current = cwd.as_path();
    loop {
        if current.join(".tokenix").exists() {
            return current.to_path_buf();
        }
        match current.parent() {
            Some(p) => current = p,
            None => return cwd,
        }
    }
}

fn now_ts() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64()
}

fn is_semantic_query(pattern: &str) -> bool {
    pattern.split_whitespace().count() >= MIN_QUERY_WORDS
}

fn handle_read(tool_input: &serde_json::Value, repo_root: &Path) -> (bool, String) {
    let file_path = match tool_input["file_path"].as_str() {
        Some(p) => p,
        None => return (false, String::new()),
    };

    // Let targeted reads through (offset or limit already specified)
    if !tool_input["offset"].is_null() || !tool_input["limit"].is_null() {
        return (false, String::new());
    }

    let full_path = {
        let p = Path::new(file_path);
        if p.exists() { p.to_path_buf() } else { repo_root.join(file_path) }
    };

    if !full_path.exists() {
        return (false, String::new());
    }

    let content = match std::fs::read_to_string(&full_path) {
        Ok(c) => c,
        Err(_) => return (false, String::new()),
    };

    let line_count = content.lines().count();
    if line_count < MIN_LINES_FOR_OUTLINE {
        return (false, String::new());
    }

    let outline = match get_file_outline(&full_path) {
        Some(o) => o,
        None => return (false, String::new()),
    };

    let rel = full_path
        .strip_prefix(repo_root)
        .unwrap_or(&full_path)
        .to_string_lossy()
        .replace('\\', "/");

    let msg = format!(
        "{}\n\n[tokenix] File has {} lines. Showing symbol outline above.\n\
        To read a specific symbol: tokenix read {} --symbol <name>\n\
        To read specific lines:   use Read with offset/limit parameters.",
        outline, line_count, rel
    );
    (true, msg)
}

fn handle_grep(tool_input: &serde_json::Value, repo_root: &Path) -> (bool, String) {
    let pattern = match tool_input["pattern"].as_str() {
        Some(p) => p,
        None => return (false, String::new()),
    };

    if !is_semantic_query(pattern) {
        return (false, String::new());
    }

    let results = match query_index(repo_root, pattern, 2500, 20, "nomic-embed-text", None) {
        Ok(Some(r)) if !r.is_empty() => r,
        _ => return (false, String::new()),
    };

    (true, format_results(&results, pattern))
}

fn estimate_original_tokens(tool_name: &str, tool_input: &serde_json::Value, repo_root: &Path) -> i64 {
    if tool_name == "Read" {
        if let Some(fp) = tool_input["file_path"].as_str() {
            let p = Path::new(fp);
            let full = if p.exists() { p.to_path_buf() } else { repo_root.join(fp) };
            if let Ok(content) = std::fs::read_to_string(&full) {
                return count_tokens(&content) as i64;
            }
        }
    }
    800
}

pub fn run_hook() -> Result<()> {
    // Read input: prefer env vars (Copilot), fall back to stdin (Claude Code / Codex)
    let raw_stdin = std::io::read_to_string(std::io::stdin()).unwrap_or_default();

    let input = HookInput::from_env()
        .or_else(|| HookInput::from_stdin(&raw_stdin))
        .unwrap_or_default();

    // Unknown or unsupported tool — pass through silently
    if input.tool_name != "Read" && input.tool_name != "Grep" {
        std::process::exit(0);
    }

    let repo_root = find_repo_root();

    let age = get_index_age(&repo_root);
    let index_missing = age.is_none();
    let index_stale = age.map(|a| a > MAX_INDEX_AGE_SECS).unwrap_or(false);

    if index_missing || index_stale {
        let reason = if index_missing {
            "missing".to_string()
        } else {
            format!("stale ({}s old)", age.unwrap() as i64)
        };
        let _ = log_hook_event(&repo_root, &HookEvent {
            ts: now_ts(), tool: input.tool_name, action: "pass".to_string(),
            reason, saved_tokens: 0, actual_tokens: 0, original_estimate: 0,
            input_preview: String::new(),
        });
        std::process::exit(0);
    }

    let (intercepted, output) = match input.tool_name.as_str() {
        "Read" => handle_read(&input.tool_input, &repo_root),
        "Grep" => handle_grep(&input.tool_input, &repo_root),
        _      => (false, String::new()),
    };

    if !intercepted {
        let _ = log_hook_event(&repo_root, &HookEvent {
            ts: now_ts(), tool: input.tool_name, action: "pass".to_string(),
            reason: "not intercepted".to_string(),
            saved_tokens: 0, actual_tokens: 0, original_estimate: 0,
            input_preview: String::new(),
        });
        std::process::exit(0);
    }

    let original_tokens = estimate_original_tokens(&input.tool_name, &input.tool_input, &repo_root);
    let actual_tokens = count_tokens(&output) as i64;
    let saved = (original_tokens - actual_tokens).max(0);

    let _ = log_hook_event(&repo_root, &HookEvent {
        ts: now_ts(),
        tool: input.tool_name.clone(),
        action: "intercepted".to_string(),
        reason: String::new(),
        saved_tokens: saved,
        actual_tokens,
        original_estimate: original_tokens,
        input_preview: raw_stdin.chars().take(200).collect(),
    });

    println!("{}", output);
    std::process::exit(2);
}
