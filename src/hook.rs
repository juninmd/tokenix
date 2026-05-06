use anyhow::Result;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::chunker::count_tokens;
use crate::query::{format_results, get_file_outline, query_index};
use crate::store::{get_index_age, log_hook_event, search_by_symbol, HookEvent};

pub const MAX_INDEX_AGE_SECS: f64 = 3600.0;
const MIN_LINES_FOR_OUTLINE: usize = 200;
const MIN_QUERY_WORDS: usize = 3;

/// Normalized hook input used by tokenix.
///
/// Claude Code sends `tool_name` and `tool_input` on stdin.
/// GitHub Copilot hooks currently send `toolName` and `toolArgs` on stdin.
/// Older Copilot-like runners may set TOOL_NAME/TOOL_INPUT environment vars.
#[derive(Deserialize, Debug, Default)]
pub struct HookInput {
    #[serde(default)]
    pub tool_name: String,
    #[serde(default)]
    pub tool_input: serde_json::Value,
}

#[derive(Deserialize, Debug)]
struct CopilotHookInput {
    #[serde(rename = "toolName")]
    tool_name: String,
    #[serde(rename = "toolArgs", default)]
    tool_args: serde_json::Value,
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
        Some(HookInput {
            tool_name,
            tool_input,
        })
    }

    fn from_stdin(raw: &str) -> Option<Self> {
        let clean = raw.trim_start_matches('\u{feff}').trim();
        if clean.is_empty() {
            return None;
        }
        if let Ok(input) = serde_json::from_str::<HookInput>(clean) {
            if !input.tool_name.is_empty() {
                return Some(input);
            }
        }
        serde_json::from_str::<CopilotHookInput>(clean)
            .ok()
            .map(|input| normalize_copilot_input(&input.tool_name, &input.tool_args))
    }
}

fn normalize_copilot_input(tool_name: &str, tool_args: &serde_json::Value) -> HookInput {
    let args = if let Some(raw) = tool_args.as_str() {
        serde_json::from_str(raw).unwrap_or(serde_json::Value::Null)
    } else {
        tool_args.clone()
    };

    match tool_name.to_ascii_lowercase().as_str() {
        "view" | "read" => HookInput {
            tool_name: "Read".to_string(),
            tool_input: normalize_read_args(args),
        },
        "grep" => HookInput {
            tool_name: "Grep".to_string(),
            tool_input: args,
        },
        _ => HookInput {
            tool_name: tool_name.to_string(),
            tool_input: args,
        },
    }
}

fn normalize_read_args(mut args: serde_json::Value) -> serde_json::Value {
    if args.get("file_path").and_then(|v| v.as_str()).is_some() {
        return args;
    }

    let path = args
        .get("path")
        .or_else(|| args.get("file"))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    if let Some(path) = path {
        if let Some(obj) = args.as_object_mut() {
            obj.insert("file_path".to_string(), serde_json::Value::String(path));
        }
    }
    args
}

fn find_repo_root() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    crate::store::find_project_root(&cwd)
}

fn now_ts() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

fn is_semantic_query(pattern: &str) -> bool {
    pattern.split_whitespace().count() >= MIN_QUERY_WORDS
}

/// True if `s` looks like a plain identifier (no regex metacharacters).
fn looks_like_identifier(s: &str) -> bool {
    s.len() >= 2
        && s.chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | ':' | '.'))
}

fn symbol_lookup(pattern: &str, repo_root: &Path) -> Option<String> {
    let conn = crate::store::open_db(repo_root, false).ok()??;
    let matches = search_by_symbol(&conn, pattern).ok()?;
    if matches.is_empty() {
        return None;
    }
    let mut lines = vec![format!(
        "<!-- tokenix: {} symbol match(es) for '{}' -->",
        matches.len(),
        pattern
    )];
    lines.push(String::new());
    for m in &matches {
        lines.push(format!(
            "{}:{} [{}] {}",
            m.path, m.start_line, m.kind, m.symbol
        ));
    }
    lines.push(String::new());
    lines.push(format!(
        "[Use Read with offset/limit or tokenix read --symbol {} to see content]",
        pattern
    ));
    Some(lines.join("\n"))
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
        if p.exists() {
            p.to_path_buf()
        } else {
            repo_root.join(file_path)
        }
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

    // Short identifier-like patterns: try index symbol lookup before falling through
    if !is_semantic_query(pattern) {
        if looks_like_identifier(pattern) {
            if let Some(output) = symbol_lookup(pattern, repo_root) {
                return (true, output);
            }
        }
        return (false, String::new());
    }

    let results = match query_index(repo_root, pattern, 2500, 20, None) {
        Ok(Some(r)) if !r.is_empty() => r,
        _ => return (false, String::new()),
    };

    (true, format_results(&results, pattern))
}

fn estimate_original_tokens(
    tool_name: &str,
    tool_input: &serde_json::Value,
    repo_root: &Path,
) -> i64 {
    if tool_name == "Read" {
        if let Some(fp) = tool_input["file_path"].as_str() {
            let p = Path::new(fp);
            let full = if p.exists() {
                p.to_path_buf()
            } else {
                repo_root.join(fp)
            };
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

    // Unknown or unsupported tool: pass through silently.
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
        let _ = log_hook_event(
            &repo_root,
            &HookEvent {
                ts: now_ts(),
                tool: input.tool_name,
                action: "pass".to_string(),
                phase: "pre".to_string(),
                reason,
                saved_tokens: 0,
                actual_tokens: 0,
                original_estimate: 0,
                input_preview: String::new(),
            },
        );
        std::process::exit(0);
    }

    let (intercepted, output) = match input.tool_name.as_str() {
        "Read" => handle_read(&input.tool_input, &repo_root),
        "Grep" => handle_grep(&input.tool_input, &repo_root),
        _ => (false, String::new()),
    };

    if !intercepted {
        let _ = log_hook_event(
            &repo_root,
            &HookEvent {
                ts: now_ts(),
                tool: input.tool_name,
                action: "pass".to_string(),
                phase: "pre".to_string(),
                reason: "not intercepted".to_string(),
                saved_tokens: 0,
                actual_tokens: 0,
                original_estimate: 0,
                input_preview: String::new(),
            },
        );
        std::process::exit(0);
    }

    let original_tokens = estimate_original_tokens(&input.tool_name, &input.tool_input, &repo_root);
    let actual_tokens = count_tokens(&output) as i64;
    let saved = (original_tokens - actual_tokens).max(0);

    let _ = log_hook_event(
        &repo_root,
        &HookEvent {
            ts: now_ts(),
            tool: input.tool_name.clone(),
            action: "intercepted".to_string(),
            phase: "pre".to_string(),
            reason: String::new(),
            saved_tokens: saved,
            actual_tokens,
            original_estimate: original_tokens,
            input_preview: raw_stdin.chars().take(200).collect(),
        },
    );

    eprintln!("{}", output);
    std::process::exit(2);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_claude_input() {
        let raw = r#"{"tool_name":"Read","tool_input":{"file_path":"src/main.rs"}}"#;
        let input = HookInput::from_stdin(raw).unwrap();
        assert_eq!(input.tool_name, "Read");
        assert_eq!(input.tool_input["file_path"], "src/main.rs");
    }

    #[test]
    fn parses_copilot_view_input() {
        let raw = r#"{"toolName":"view","toolArgs":"{\"path\":\"src/main.rs\"}"}"#;
        let input = HookInput::from_stdin(raw).unwrap();
        assert_eq!(input.tool_name, "Read");
        assert_eq!(input.tool_input["file_path"], "src/main.rs");
    }

    #[test]
    fn empty_stdin_returns_none() {
        assert!(HookInput::from_stdin("").is_none());
        assert!(HookInput::from_stdin("   ").is_none());
    }

    #[test]
    fn bom_prefix_stripped() {
        let raw = "\u{feff}{\"tool_name\":\"Grep\",\"tool_input\":{\"pattern\":\"how does auth work\"}}";
        let input = HookInput::from_stdin(raw).unwrap();
        assert_eq!(input.tool_name, "Grep");
    }

    #[test]
    fn unknown_tool_parses_as_is() {
        let raw = r#"{"tool_name":"Edit","tool_input":{"file_path":"x.rs"}}"#;
        let input = HookInput::from_stdin(raw).unwrap();
        assert_eq!(input.tool_name, "Edit");
    }

    #[test]
    fn is_semantic_query_requires_3_words() {
        assert!(!is_semantic_query("fn main"));
        assert!(!is_semantic_query("embed_query"));
        assert!(is_semantic_query("how does embedding work"));
        assert!(is_semantic_query("database connection pool"));
    }

    #[test]
    fn looks_like_identifier_rules() {
        assert!(looks_like_identifier("embed_query"));
        assert!(looks_like_identifier("MyStruct::new"));
        assert!(!looks_like_identifier("a")); // too short
        assert!(!looks_like_identifier("foo bar")); // has space
        assert!(!looks_like_identifier("fn.*main")); // regex meta
    }

    #[test]
    fn copilot_grep_normalized() {
        let raw = r#"{"toolName":"grep","toolArgs":{"pattern":"how does auth work"}}"#;
        let input = HookInput::from_stdin(raw).unwrap();
        assert_eq!(input.tool_name, "Grep");
        assert_eq!(input.tool_input["pattern"], "how does auth work");
    }

    #[test]
    fn copilot_read_with_path_key() {
        let raw = r#"{"toolName":"view","toolArgs":{"path":"src/lib.rs"}}"#;
        let input = HookInput::from_stdin(raw).unwrap();
        assert_eq!(input.tool_name, "Read");
        assert_eq!(input.tool_input["file_path"], "src/lib.rs");
    }
}
