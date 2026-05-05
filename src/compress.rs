use anyhow::Result;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::chunker::count_tokens;
use crate::store::{log_hook_event, HookEvent};

const POST_HOOK_TOOLS: &[&str] = &["Bash", "ListDirectory"];

pub fn compress_output(s: &str) -> String {
    // JSON compaction first: if output is pure JSON or NDJSON, compact and return early.
    // The other transforms (ANSI, emoji, blank lines) don't apply to JSON.
    let compacted = compact_json(s);
    if compacted != s {
        return compacted;
    }
    let s = strip_ansi(s);
    let s = remove_emojis(&s);
    let s = collapse_blank_lines(&s);
    group_repeated_lines(&s)
}

/// Compact pretty-printed JSON (pure JSON or NDJSON) into single-line form.
/// Returns the original string unchanged if not JSON or if already compact.
fn compact_json(s: &str) -> String {
    let trimmed = s.trim();

    // Case 1: entire output is a JSON object or array
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Ok(compact) = serde_json::to_string(&v) {
                if compact.len() < trimmed.len() {
                    return if s.ends_with('\n') {
                        compact + "\n"
                    } else {
                        compact
                    };
                }
            }
        }
    }

    // Case 2: NDJSON — every non-empty line is a JSON object
    let lines: Vec<&str> = trimmed.lines().collect();
    if lines.len() > 1
        && lines.iter().all(|l| {
            let t = l.trim();
            t.is_empty()
                || (t.starts_with('{')
                    && serde_json::from_str::<serde_json::Value>(t).is_ok())
        })
    {
        let compacted: String = lines
            .iter()
            .filter_map(|l| {
                let t = l.trim();
                if t.is_empty() {
                    return None;
                }
                Some(
                    serde_json::from_str::<serde_json::Value>(t)
                        .and_then(|v| serde_json::to_string(&v))
                        .unwrap_or_else(|_| t.to_string()),
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let result = if s.ends_with('\n') {
            compacted + "\n"
        } else {
            compacted
        };
        if result.len() < s.len() {
            return result;
        }
    }

    s.to_string()
}

/// Remove ANSI/VT100 escape sequences (CSI, OSC, and single-char sequences).
fn strip_ansi(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut result: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != 0x1b {
            result.push(bytes[i]);
            i += 1;
            continue;
        }
        i += 1;
        if i >= bytes.len() {
            break;
        }
        match bytes[i] {
            b'[' => {
                i += 1;
                // CSI: skip until final byte (0x40–0x7E)
                while i < bytes.len() {
                    let b = bytes[i];
                    i += 1;
                    if (0x40..=0x7E).contains(&b) {
                        break;
                    }
                }
            }
            b']' => {
                i += 1;
                // OSC: skip until BEL or ST (ESC \)
                while i < bytes.len() {
                    if bytes[i] == 0x07 {
                        i += 1;
                        break;
                    }
                    if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            }
            _ => {
                i += 1; // single-char sequence: ESC + one byte
            }
        }
    }
    // ANSI sequences are pure ASCII; remaining bytes are still valid UTF-8.
    String::from_utf8(result).unwrap_or_default()
}

/// Remove emoji characters by unicode code-point range.
fn remove_emojis(s: &str) -> String {
    s.chars().filter(|&c| !is_emoji_char(c)).collect()
}

fn is_emoji_char(c: char) -> bool {
    matches!(c,
        '\u{1F000}'..='\u{1FFFF}' // Emoticons, misc symbols and pictographs, transport, etc.
        | '\u{2600}'..='\u{26FF}' // Misc symbols (☀☁⚡ etc.)
        | '\u{2700}'..='\u{27BF}' // Dingbats (✈✉✔ etc.)
        | '\u{FE00}'..='\u{FE0F}' // Variation selectors (emoji presentation)
        | '\u{200D}'              // Zero-width joiner (emoji combiner)
        | '\u{20E3}'              // Combining enclosing keycap
    )
}

/// Collapse 3+ consecutive newlines down to 2 (one blank line between paragraphs).
fn collapse_blank_lines(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut newline_run = 0usize;
    for c in s.chars() {
        if c == '\n' {
            newline_run += 1;
            if newline_run <= 2 {
                result.push('\n');
            }
        } else {
            newline_run = 0;
            result.push(c);
        }
    }
    result
}

/// Group consecutive identical lines that appear 3+ times into one line + annotation.
/// Lines appearing 1–2 times in a row are left unchanged.
fn group_repeated_lines(s: &str) -> String {
    let trailing_newline = s.ends_with('\n');
    let source = if trailing_newline {
        &s[..s.len() - 1]
    } else {
        s
    };
    let lines: Vec<&str> = source.split('\n').collect();
    let mut result = String::with_capacity(s.len());
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let mut end = i + 1;
        while end < lines.len() && lines[end] == line {
            end += 1;
        }
        let count = end - i;
        if count >= 3 {
            result.push_str(line);
            result.push('\n');
            result.push_str(&format!("[repeated {}x]\n", count - 1));
        } else {
            for _ in 0..count {
                result.push_str(line);
                result.push('\n');
            }
        }
        i = end;
    }
    if !trailing_newline && result.ends_with('\n') {
        result.pop();
    }
    result
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
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

/// Extract plain text from a PostToolUse tool_response value.
/// Handles: plain string, {"output": "..."}, and content-array format.
fn extract_response_text(response: &serde_json::Value) -> Option<String> {
    if let Some(s) = response.as_str() {
        return Some(s.to_string());
    }
    if let Some(s) = response["output"].as_str() {
        return Some(s.to_string());
    }
    if let Some(arr) = response["content"].as_array() {
        let text: String = arr
            .iter()
            .filter_map(|item| {
                if item["type"].as_str() == Some("text") {
                    item["text"].as_str().map(str::to_string)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !text.is_empty() {
            return Some(text);
        }
    }
    None
}

pub fn run_hook_post() -> Result<()> {
    let raw_stdin = std::io::read_to_string(std::io::stdin()).unwrap_or_default();
    let clean = raw_stdin.trim_start_matches('\u{feff}').trim();

    let v: serde_json::Value = match serde_json::from_str(clean) {
        Ok(v) => v,
        Err(_) => std::process::exit(0),
    };

    let tool_name = v["tool_name"].as_str().unwrap_or("").to_string();
    if !POST_HOOK_TOOLS.contains(&tool_name.as_str()) {
        std::process::exit(0);
    }

    let text = match extract_response_text(&v["tool_response"]) {
        Some(t) if !t.is_empty() => t,
        _ => std::process::exit(0),
    };

    let compressed = compress_output(&text);

    if compressed == text {
        std::process::exit(0);
    }

    let repo_root = find_repo_root();
    let original_tokens = count_tokens(&text) as i64;
    let actual_tokens = count_tokens(&compressed) as i64;
    let saved = (original_tokens - actual_tokens).max(0);

    let _ = log_hook_event(
        &repo_root,
        &HookEvent {
            ts: now_ts(),
            tool: tool_name,
            action: "intercepted".to_string(),
            phase: "post".to_string(),
            reason: String::new(),
            saved_tokens: saved,
            actual_tokens,
            original_estimate: original_tokens,
            input_preview: clean.chars().take(200).collect(),
        },
    );

    println!("{}", compressed);
    std::process::exit(2);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_ansi_colors() {
        assert_eq!(strip_ansi("\x1b[32mOK\x1b[0m"), "OK");
        assert_eq!(strip_ansi("\x1b[1;31mError\x1b[0m: bad"), "Error: bad");
    }

    #[test]
    fn strips_osc_sequences() {
        assert_eq!(strip_ansi("\x1b]0;title\x07text"), "text");
    }

    #[test]
    fn removes_emojis() {
        assert_eq!(remove_emojis("🚀 Build done"), " Build done");
        assert_eq!(remove_emojis("no emojis here"), "no emojis here");
    }

    #[test]
    fn collapses_blank_lines() {
        let input = "a\n\n\n\n\nb";
        let output = collapse_blank_lines(input);
        assert_eq!(output, "a\n\nb");
    }

    #[test]
    fn groups_repeated_lines() {
        let input = "line1\nline1\nline1\nline1\nline2\n";
        let output = group_repeated_lines(input);
        assert_eq!(output, "line1\n[repeated 3x]\nline2\n");
    }

    #[test]
    fn does_not_group_two_identical_lines() {
        let input = "a\na\nb\n";
        assert_eq!(group_repeated_lines(input), "a\na\nb\n");
    }

    #[test]
    fn compacts_pretty_json_object() {
        let input = "{\n  \"status\": \"ok\",\n  \"count\": 42\n}\n";
        let output = compact_json(input);
        // key order is not guaranteed; verify it compacted (shorter) and parses to same value
        assert!(output.len() < input.len(), "should be shorter");
        assert!(output.ends_with('\n'));
        let v_in: serde_json::Value = serde_json::from_str(input.trim()).unwrap();
        let v_out: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
        assert_eq!(v_in, v_out);
    }

    #[test]
    fn compacts_pretty_json_array() {
        let input = "[\n  1,\n  2,\n  3\n]";
        let output = compact_json(input);
        assert_eq!(output, "[1,2,3]");
    }

    #[test]
    fn passes_through_already_compact_json() {
        let input = "{\"a\":1}\n";
        assert_eq!(compact_json(input), input);
    }

    #[test]
    fn compacts_ndjson() {
        let input = "{ \"level\": \"info\", \"msg\": \"started\" }\n{ \"level\": \"error\", \"msg\": \"failed\" }\n";
        let output = compact_json(input);
        assert_eq!(output, "{\"level\":\"info\",\"msg\":\"started\"}\n{\"level\":\"error\",\"msg\":\"failed\"}\n");
    }

    #[test]
    fn passes_through_plain_text() {
        let input = "On branch main\nnothing to commit\n";
        assert_eq!(compact_json(input), input);
    }

    #[test]
    fn compress_is_idempotent_on_clean_input() {
        let clean = "hello\nworld\n";
        assert_eq!(compress_output(clean), clean);
    }

    #[test]
    fn full_compression_pipeline() {
        let input = "\x1b[32m🚀 Starting\x1b[0m\n\n\n\nline\nline\nline\nline\ndone\n";
        let output = compress_output(input);
        assert!(output.contains("Starting"));
        assert!(!output.contains("\x1b["));
        assert!(!output.contains('🚀'));
        assert!(output.contains("[repeated"));
        // no triple blank lines
        assert!(!output.contains("\n\n\n"));
    }
}
