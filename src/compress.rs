use anyhow::Result;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::chunker::count_tokens;
use crate::store::{log_hook_event, HookEvent};

const POST_HOOK_TOOLS: &[&str] = &["Bash", "ListDirectory"];
const BASH_MAX_LINES: usize = 100;
const BASH_HEAD_LINES: usize = 40;
const BASH_TAIL_LINES: usize = 15;

/// Bash-aware compression: checks user TOML filters first, then built-in heuristics.
fn log_unfiltered_cmd(cmd: &str) {
    if cmd.is_empty() {
        return;
    }
    // Write to the global ~/.tokenix/ dir, not the project dir, to avoid
    // accidentally committing internal tokenix logs.
    let log_path = match dirs::home_dir() {
        Some(h) => h.join(".tokenix").join("unfiltered_cmds.log"),
        None => return,
    };
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let entry = format!("{}\n", cmd);
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        let _ = f.write_all(entry.as_bytes());
    }
}

pub fn compress_bash_output(cmd: &str, s: &str) -> String {
    // User-defined TOML filters take priority over built-in heuristics.
    let user_filters = crate::filters::load_all_filters();
    if let Some(f) = crate::filters::find_filter(cmd, &user_filters) {
        return crate::filters::apply_filter(s, f);
    }

    // No filter matched — record for later analysis (tokenix filter list).
    log_unfiltered_cmd(cmd);

    let base = compress_output(s);
    let lines: Vec<&str> = base.lines().collect();

    // Cargo: always try (it filters signal from noise regardless of total length)
    if is_cargo_output(&lines) {
        let cargo_out = compress_cargo(&lines);
        if cargo_out.len() < base.len() {
            return cargo_out;
        }
    }

    if is_path_listing_command(cmd) {
        let listing_out = compress_path_listing(&lines);
        if listing_out.len() < base.len() {
            return listing_out;
        }
    }

    if lines.len() <= BASH_MAX_LINES {
        return base;
    }

    if is_git_log(&lines) {
        return compress_git_log(&lines);
    }

    truncate_head_tail(&lines, BASH_HEAD_LINES, BASH_TAIL_LINES)
}

fn is_path_listing_command(cmd: &str) -> bool {
    let trimmed = cmd.trim();
    // Plain ls / ls with flags / recursive find
    trimmed == "ls"
        || trimmed == "ls -R"
        || trimmed.starts_with("ls ")
        // POSIX find
        || trimmed.starts_with("find ")
        // Windows cmd / PowerShell dir
        || trimmed == "dir"
        || trimmed.starts_with("dir ")
        // PowerShell Get-ChildItem and its aliases
        || trimmed.starts_with("Get-ChildItem")
        || trimmed.starts_with("get-childitem")
        || trimmed == "gci"
        || trimmed.starts_with("gci ")
        // tree command (Unix and Windows)
        || trimmed == "tree"
        || trimmed.starts_with("tree ")
}

fn is_cargo_output(lines: &[&str]) -> bool {
    lines.iter().take(50).any(|l| {
        let t = l.trim();
        t.starts_with("Compiling ")
            || t.starts_with("Finished ")
            || t.starts_with("error[E")
            || t.contains("test result:")
    })
}

fn compress_cargo(lines: &[&str]) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut in_diagnostic = false;
    let mut warning_count: u32 = 0;
    const MAX_WARNINGS: u32 = 5;

    for line in lines {
        let t = line.trim();
        let is_error = (t.starts_with("error[") || t == "error" || t.starts_with("error: "))
            && !t.starts_with("error_");
        let is_warning = t.starts_with("warning[") || t.starts_with("warning: ");
        let is_context = t.starts_with("-->")
            || (t.starts_with('|') && t.len() > 1)
            || t.starts_with("= note:")
            || t.starts_with("= help:")
            || t.starts_with("help:");
        let is_summary = t.starts_with("Finished ")
            || t.starts_with("error: aborting")
            || t.contains("test result:")
            || t.starts_with("running ")
            || t.starts_with("FAILED")
            || (t.starts_with("test ") && (t.ends_with("ok") || t.ends_with("FAILED")));

        if is_error {
            out.push(line);
            in_diagnostic = true;
        } else if is_warning && warning_count < MAX_WARNINGS {
            out.push(line);
            in_diagnostic = true;
            warning_count += 1;
        } else if is_context && in_diagnostic {
            out.push(line);
        } else if is_summary {
            out.push(line);
            in_diagnostic = false;
        } else {
            in_diagnostic = false;
        }
    }

    if warning_count >= MAX_WARNINGS {
        out.push("  ... (additional warnings omitted)");
    }

    out.join("\n")
}

fn is_git_log(lines: &[&str]) -> bool {
    lines.iter().take(5).any(|l| l.starts_with("commit "))
}

fn compress_git_log(lines: &[&str]) -> String {
    const MAX_COMMITS: usize = 20;
    let mut commit_count: usize = 0;
    let mut keep_until: usize = 0;

    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("commit ") {
            commit_count += 1;
            if commit_count > MAX_COMMITS {
                break;
            }
        }
        keep_until = i + 1;
    }

    if keep_until >= lines.len() {
        return lines.join("\n");
    }
    let omitted = lines.len() - keep_until;
    format!(
        "{}\n[... {} more lines omitted (>{} commits)]",
        lines[..keep_until].join("\n"),
        omitted,
        MAX_COMMITS
    )
}

fn compress_path_listing(lines: &[&str]) -> String {
    let paths: Vec<&str> = lines
        .iter()
        .map(|line| line.trim())
        .filter(|line| {
            !line.is_empty()
                && !line.ends_with(':')
                && !line.contains(" -> ")
                && (line.contains('/') || line.contains('\\'))
        })
        .collect();
    if paths.len() < 4 {
        return lines.join("\n");
    }

    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    for path in paths {
        let normalized = path.replace('\\', "/");
        let dir = normalized
            .rsplit_once('/')
            .map(|(dir, _)| dir)
            .unwrap_or(".")
            .to_string();
        *counts.entry(dir).or_insert(0) += 1;
    }

    let total_files: usize = counts.values().sum();
    let mut out = vec![format!("{} files in {} dirs:", total_files, counts.len())];
    for (dir, count) in counts {
        out.push(format!("{}/ ({})", dir, count));
    }
    out.join("\n")
}

fn truncate_head_tail(lines: &[&str], head: usize, tail: usize) -> String {
    let total = lines.len();
    if total <= head + tail {
        return lines.join("\n");
    }
    let omitted = total - head - tail;
    format!(
        "{}\n[... {} lines omitted ...]\n{}",
        lines[..head].join("\n"),
        omitted,
        lines[total - tail..].join("\n")
    )
}

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

    // Case 0: too short to be meaningful JSON
    if trimmed.len() < 2 {
        return s.to_string();
    }

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

    // Case 2: NDJSON — every non-empty line is a JSON object or array
    let lines: Vec<&str> = trimmed.lines().collect();
    if lines.len() > 1
        && lines.iter().all(|l| {
            let t = l.trim();
            t.is_empty()
                || (t.starts_with('{') && serde_json::from_str::<serde_json::Value>(t).is_ok())
                || (t.starts_with('[') && serde_json::from_str::<serde_json::Value>(t).is_ok())
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
pub(crate) fn strip_ansi(s: &str) -> String {
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
/// Also performs fuzzy grouping for common patterns (e.g., progress bars, file listings).
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

        // 1. Exact match grouping
        let mut end = i + 1;
        while end < lines.len() && lines[end] == line {
            end += 1;
        }
        let count = end - i;
        if count >= 3 {
            result.push_str(line);
            result.push('\n');
            result.push_str(&format!("[repeated {}x]\n", count - 1));
            i = end;
            continue;
        }

        // 2. Fuzzy grouping (similarity)
        if let Some(fuzzy_count) = try_fuzzy_group(&lines, i) {
            if fuzzy_count >= 3 {
                result.push_str(line);
                result.push_str(" ... (and ");
                result.push_str(&(fuzzy_count - 1).to_string());
                result.push_str(" similar lines)\n");
                i += fuzzy_count;
                continue;
            }
        }

        result.push_str(line);
        result.push('\n');
        i += 1;
    }
    if !trailing_newline && result.ends_with('\n') {
        result.pop();
    }
    result
}

fn try_fuzzy_group(lines: &[&str], start: usize) -> Option<usize> {
    let line = lines[start];
    if line.len() < 5 {
        return None;
    }

    // Patterns for fuzzy grouping:
    let prefixes = [
        "Removing ",
        "Compiling ",
        "Installing ",
        "Download ",
        "Extracting ",
        "Checked ",
        "test ",
    ];

    for prefix in prefixes {
        if line.starts_with(prefix) {
            if prefix == "test " && !line.contains(" ... ok") {
                continue;
            }
            let mut count = 1;
            for next_line in lines.iter().skip(start + 1) {
                if next_line.starts_with(prefix) {
                    count += 1;
                } else {
                    break;
                }
            }
            if count >= 3 {
                return Some(count);
            }
        }
    }
    None
}

fn find_repo_root() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    crate::store::find_project_root(&cwd)
}

pub fn now_ts() -> f64 {
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
    // Claude Code Bash tool_response: { stdout, stderr, interrupted, ... }.
    let stdout = response["stdout"].as_str().unwrap_or("");
    let stderr = response["stderr"].as_str().unwrap_or("");
    if !stdout.is_empty() || !stderr.is_empty() {
        let mut combined = stdout.to_string();
        if !stderr.is_empty() {
            if !combined.is_empty() {
                combined.push('\n');
            }
            combined.push_str(stderr);
        }
        return Some(combined);
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

/// Output dialect for a PostToolUse hook, selected by which agent invoked it.
#[derive(Debug, PartialEq)]
enum PostDialect {
    /// Claude Code / Codex: PostToolUse cannot replace or shorten a tool result,
    /// so compression here is a no-op — exit 0 silently without logging savings.
    ClaudeNoop,
    /// GitHub Copilot CLI: print `{"modifiedResult":{...}}` JSON on stdout, exit 0.
    CopilotJson,
}

/// A PostToolUse payload normalized across Claude Code and Copilot CLI formats.
struct PostHookInput {
    tool_name: String,
    command: String,
    text: String,
    dialect: PostDialect,
}

/// Decode Copilot's `toolArgs`, which may arrive as a JSON-encoded string or an object.
fn decode_tool_args(v: &serde_json::Value) -> serde_json::Value {
    match v.as_str() {
        Some(raw) => serde_json::from_str(raw).unwrap_or(serde_json::Value::Null),
        None => v.clone(),
    }
}

/// Map an agent's shell/list tool name onto tokenix's internal POST_HOOK_TOOLS name.
/// Copilot uses `bash`/`powershell`; Claude Code already uses `Bash`/`ListDirectory`.
fn normalize_post_tool(name: &str) -> String {
    match name.to_ascii_lowercase().as_str() {
        "bash"
        | "powershell"
        | "shell"
        | "run_shell_command"
        | "default_api:run_shell_command"
        | "run_command"
        | "default_api:run_command"
        | "get_terminal_output"
        | "default_api:get_terminal_output" => "Bash".to_string(),
        "listdirectory" | "default_api:list_directory" => "ListDirectory".to_string(),
        _ => name.to_string(),
    }
}

/// Extract the LLM-facing text from a Copilot `toolResult` (object or string,
/// camelCase `textResultForLlm` or VS Code snake_case `text_result_for_llm`).
fn extract_copilot_result(tr: &serde_json::Value) -> Option<String> {
    if let Some(s) = tr.as_str() {
        return Some(s.to_string());
    }
    tr["textResultForLlm"]
        .as_str()
        .or_else(|| tr["text_result_for_llm"].as_str())
        .map(str::to_string)
}

/// Normalize a PostToolUse payload. Copilot CLI sends camelCase `toolName`/`toolResult`;
/// Claude Code sends snake_case `tool_name`/`tool_response`.
fn parse_post_input(v: &serde_json::Value) -> Option<PostHookInput> {
    // GitHub Copilot CLI: camelCase toolName + toolResult + (maybe string-encoded) toolArgs.
    if let Some(raw_name) = v["toolName"].as_str() {
        let args = decode_tool_args(&v["toolArgs"]);
        let command = args["command"]
            .as_str()
            .or_else(|| args["CommandLine"].as_str())
            .or_else(|| args["commandLine"].as_str())
            .or_else(|| args["command_line"].as_str())
            .unwrap_or("")
            .to_string();
        return Some(PostHookInput {
            tool_name: normalize_post_tool(raw_name),
            command,
            text: extract_copilot_result(&v["toolResult"])?,
            dialect: PostDialect::CopilotJson,
        });
    }

    // Claude Code / Codex: snake_case tool_name + tool_response.
    let raw_name = v["tool_name"].as_str()?;
    let command = v["tool_input"]["command"]
        .as_str()
        .or_else(|| v["tool_input"]["CommandLine"].as_str())
        .or_else(|| v["tool_input"]["commandLine"].as_str())
        .or_else(|| v["tool_input"]["command_line"].as_str())
        .unwrap_or("")
        .to_string();
    Some(PostHookInput {
        tool_name: normalize_post_tool(raw_name),
        command,
        text: extract_response_text(&v["tool_response"])?,
        dialect: PostDialect::ClaudeNoop,
    })
}

pub fn run_hook_post() -> Result<()> {
    let raw_stdin = std::io::read_to_string(std::io::stdin()).unwrap_or_default();
    let clean = raw_stdin.trim_start_matches('\u{feff}').trim();

    let v: serde_json::Value = match serde_json::from_str(clean) {
        Ok(v) => v,
        Err(_) => std::process::exit(0),
    };

    let input = match parse_post_input(&v) {
        Some(i) if !i.text.is_empty() && POST_HOOK_TOOLS.contains(&i.tool_name.as_str()) => i,
        _ => std::process::exit(0),
    };

    let compressed = if input.tool_name == "Bash" {
        compress_bash_output(&input.command, &input.text)
    } else {
        compress_output(&input.text)
    };

    if compressed == input.text {
        std::process::exit(0);
    }

    // Claude Code PostToolUse hooks cannot shorten or replace a tool result:
    // exit 2 surfaces stderr (not stdout) as a blocking error, and the supported
    // `hookSpecificOutput.additionalContext` only appends next to the original
    // output. So compressing Bash output here can never reduce the tokens Claude
    // Code sends to the model. Exit 0 silently to avoid the empty-stderr blocking
    // error, and do NOT log savings the model never actually receives. Real Bash
    // compression must move to a PreToolUse command rewrite (run the command
    // through tokenix before execution), the way rtk wraps `rtk <cmd>`.
    if input.dialect == PostDialect::ClaudeNoop {
        std::process::exit(0);
    }

    // Only Copilot reaches here, and its modifiedResult JSON genuinely replaces
    // the tool result — so the logged savings are real for this dialect.
    let repo_root = find_repo_root();
    let original_tokens = count_tokens(&input.text) as i64;
    let actual_tokens = count_tokens(&compressed) as i64;
    let saved = (original_tokens - actual_tokens).max(0);

    let _ = log_hook_event(
        &repo_root,
        &HookEvent {
            ts: now_ts(),
            tool: input.tool_name,
            action: "intercepted".to_string(),
            phase: "post".to_string(),
            reason: String::new(),
            saved_tokens: saved,
            actual_tokens,
            original_estimate: original_tokens,
            input_preview: clean.chars().take(200).collect(),
            command: input.command,
        },
    );

    let out = serde_json::json!({
        "modifiedResult": {
            "resultType": "success",
            "textResultForLlm": compressed,
        }
    });
    println!("{}", serde_json::to_string(&out).unwrap_or_default());
    std::process::exit(0);
}

pub fn run_command_and_compress(command_str: &str) -> Result<i32> {
    let mut cmd = if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", command_str]);
        c
    } else {
        let mut c = std::process::Command::new("sh");
        c.args(["-c", command_str]);
        c
    };

    // Capture stdout and stderr
    let output = cmd.output()?;

    let stdout_raw = String::from_utf8_lossy(&output.stdout);
    let stderr_raw = String::from_utf8_lossy(&output.stderr);

    // Apply tokenix compression to stdout and stderr
    let stdout_compressed = compress_bash_output(command_str, &stdout_raw);
    let stderr_compressed = compress_bash_output(command_str, &stderr_raw);

    // Print to standard streams
    print!("{}", stdout_compressed);
    eprint!("{}", stderr_compressed);

    // Write log event of the actual execution savings
    let repo_root = find_repo_root();
    // Capture raw output if a `tokenix filter record` session is active.
    crate::recordings::capture(&repo_root, command_str, &stdout_raw, &stderr_raw);
    let original_tokens = (count_tokens(&stdout_raw) + count_tokens(&stderr_raw)) as i64;
    let actual_tokens =
        (count_tokens(&stdout_compressed) + count_tokens(&stderr_compressed)) as i64;
    let saved = (original_tokens - actual_tokens).max(0);

    if saved > 0 {
        let _ = log_hook_event(
            &repo_root,
            &HookEvent {
                ts: now_ts(),
                tool: "Bash".to_string(),
                action: "intercepted".to_string(),
                phase: "ToolOutputCompressed".to_string(),
                reason: "compressed command output".to_string(),
                saved_tokens: saved,
                actual_tokens,
                original_estimate: original_tokens,
                input_preview: command_str.chars().take(200).collect(),
                command: command_str.to_string(),
            },
        );
    }

    Ok(output.status.code().unwrap_or(0))
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
        assert_eq!(
            output,
            "{\"level\":\"info\",\"msg\":\"started\"}\n{\"level\":\"error\",\"msg\":\"failed\"}\n"
        );
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
        assert!(!output.contains("\n\n\n"));
    }

    #[test]
    fn bash_short_output_passes_through() {
        let input = "hello\nworld\n";
        assert_eq!(compress_bash_output("", input), input);
    }

    #[test]
    fn bash_generic_truncation_over_100_lines() {
        let lines: String = (1..=150).map(|i| format!("line {}\n", i)).collect();
        let out = compress_bash_output("", &lines);
        assert!(out.contains("lines omitted"), "should truncate: {}", out);
        assert!(out.contains("line 1\n"));
        assert!(out.contains("line 150"));
    }

    #[test]
    fn bash_path_listing_groups_by_directory() {
        let input = [
            "src/main.rs",
            "src/query.rs",
            "src/hook.rs",
            "benchmark/samples/database_client.ts",
        ]
        .join("\n");

        let out = compress_bash_output("ls -R", &input);
        assert!(out.contains("4 files in 2 dirs"), "output: {}", out);
        assert!(out.contains("src/ (3)"), "output: {}", out);
        assert!(out.contains("benchmark/samples/ (1)"), "output: {}", out);
    }

    #[test]
    fn bash_cargo_extracts_errors() {
        let mut input = String::new();
        for i in 0..60 {
            input.push_str(&format!("Compiling crate{} v0.1.0\n", i));
        }
        input.push_str("error[E0425]: cannot find value `foo`\n");
        input.push_str("  --> src/main.rs:3:5\n");
        input.push_str("   |\n");
        input.push_str("3  |     foo();\n");
        input.push_str("error: aborting due to 1 previous error\n");
        input.push_str("Finished dev in 1.23s\n");

        let out = compress_bash_output("", &input);
        assert!(out.contains("error[E0425]"), "should keep error: {}", out);
        assert!(out.contains("Finished"), "should keep summary: {}", out);
        assert!(
            !out.contains("Compiling crate0"),
            "should strip Compiling lines"
        );
    }

    #[test]
    fn bash_git_log_truncated_after_20_commits() {
        let mut input = String::new();
        for i in 0..30 {
            input.push_str(&format!("commit {:040}\n", i));
            input.push_str("Author: Test\nDate: Today\n\n    message\n\n");
        }
        let out = compress_bash_output("", &input);
        assert!(out.contains("lines omitted"), "should truncate: {}", out);
    }

    #[test]
    fn parses_claude_post_input() {
        let v = serde_json::json!({
            "tool_name": "Bash",
            "tool_input": {"command": "git status"},
            "tool_response": "On branch main\n"
        });
        let input = parse_post_input(&v).unwrap();
        assert_eq!(input.tool_name, "Bash");
        assert_eq!(input.command, "git status");
        assert_eq!(input.text, "On branch main\n");
        assert_eq!(input.dialect, PostDialect::ClaudeNoop);
    }

    #[test]
    fn parses_claude_bash_stdout_stderr_shape() {
        // Real Claude Code Bash PostToolUse payload: tool_response is an object
        // with stdout/stderr, not `output`/`content`.
        let v = serde_json::json!({
            "tool_name": "Bash",
            "tool_input": {"command": "npm install"},
            "tool_response": {
                "stdout": "added 120 packages in 3s\n",
                "stderr": "npm warn deprecated foo\n",
                "interrupted": false,
                "isImage": false
            }
        });
        let input = parse_post_input(&v).unwrap();
        assert_eq!(input.tool_name, "Bash");
        assert_eq!(input.command, "npm install");
        assert!(input.text.contains("added 120 packages"));
        assert!(input.text.contains("npm warn deprecated foo"));
        assert_eq!(input.dialect, PostDialect::ClaudeNoop);
    }

    #[test]
    fn parses_copilot_post_input_camelcase() {
        let v = serde_json::json!({
            "toolName": "bash",
            "toolArgs": {"command": "git diff"},
            "toolResult": {"resultType": "success", "textResultForLlm": "diff output"}
        });
        let input = parse_post_input(&v).unwrap();
        assert_eq!(input.tool_name, "Bash"); // bash → Bash
        assert_eq!(input.command, "git diff");
        assert_eq!(input.text, "diff output");
        assert_eq!(input.dialect, PostDialect::CopilotJson);
    }

    #[test]
    fn parses_copilot_post_input_string_encoded_args() {
        // Copilot may send toolArgs as a JSON-encoded string.
        let v = serde_json::json!({
            "toolName": "powershell",
            "toolArgs": "{\"command\":\"git status\"}",
            "toolResult": {"textResultForLlm": "status output"}
        });
        let input = parse_post_input(&v).unwrap();
        assert_eq!(input.tool_name, "Bash"); // powershell → Bash
        assert_eq!(input.command, "git status");
        assert_eq!(input.text, "status output");
    }

    #[test]
    fn extract_copilot_result_handles_both_casings() {
        let camel = serde_json::json!({"textResultForLlm": "a"});
        let snake = serde_json::json!({"text_result_for_llm": "b"});
        let plain = serde_json::Value::String("c".to_string());
        assert_eq!(extract_copilot_result(&camel).as_deref(), Some("a"));
        assert_eq!(extract_copilot_result(&snake).as_deref(), Some("b"));
        assert_eq!(extract_copilot_result(&plain).as_deref(), Some("c"));
    }

    #[test]
    fn normalize_post_tool_maps_shells_to_bash() {
        assert_eq!(normalize_post_tool("bash"), "Bash");
        assert_eq!(normalize_post_tool("powershell"), "Bash");
        assert_eq!(normalize_post_tool("Bash"), "Bash"); // Claude casing preserved
        assert_eq!(normalize_post_tool("run_command"), "Bash");
        assert_eq!(normalize_post_tool("default_api:run_command"), "Bash");
        assert_eq!(normalize_post_tool("ListDirectory"), "ListDirectory");
        assert_eq!(normalize_post_tool("view"), "view"); // unmapped → unchanged
    }

    #[test]
    fn parses_claude_post_input_run_command() {
        let v = serde_json::json!({
            "tool_name": "default_api:run_command",
            "tool_input": {"CommandLine": "git diff"},
            "tool_response": "diff output"
        });
        let input = parse_post_input(&v).unwrap();
        assert_eq!(input.tool_name, "Bash");
        assert_eq!(input.command, "git diff");
        assert_eq!(input.text, "diff output");
        assert_eq!(input.dialect, PostDialect::ClaudeNoop);
    }
}
