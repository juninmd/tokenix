use anyhow::Result;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::chunker::count_tokens;
use crate::query::{format_results, get_file_outline, query_index};
use crate::store::{index_staleness, log_hook_event, search_by_symbol, HookEvent};

const MIN_LINES_FOR_OUTLINE: usize = 200;
const MIN_QUERY_WORDS: usize = 3;
const DAEMON_HOOK_TIMEOUT_MS: u64 = 2_000;

/// Read intercept threshold, overridable via `[hook] read_min_lines` in
/// `.tokenix.toml`. Tune down to intercept more reads (saving more tokens) or
/// up for more verbatim file content.
fn min_lines_for_outline() -> usize {
    crate::chunker::hook_config()
        .read_min_lines
        .filter(|n| *n > 0)
        .unwrap_or(MIN_LINES_FOR_OUTLINE)
}

/// Grep intercept threshold, overridable via `[hook] grep_min_words`.
fn min_query_words() -> usize {
    crate::chunker::hook_config()
        .grep_min_words
        .filter(|n| *n > 0)
        .unwrap_or(MIN_QUERY_WORDS)
}

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
    #[serde(skip)]
    raw_tool_name: String,
}

#[derive(Deserialize, Debug)]
struct CopilotHookInput {
    #[serde(rename = "toolName")]
    tool_name: String,
    #[serde(rename = "toolArgs", default)]
    tool_args: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AntigravityHookInput {
    tool_call: AntigravityToolCall,
}

#[derive(Debug, Deserialize)]
struct AntigravityToolCall {
    name: String,
    #[serde(default)]
    args: serde_json::Value,
}

impl HookInput {
    fn from_env() -> Option<Self> {
        // Env vars are a fallback for non-standard tools; Copilot uses stdin.
        // Only support generic TOOL_NAME/TOOL_INPUT (not COPILOT_* which can't normalize).
        let tool_name = std::env::var("TOOL_NAME").ok()?;
        let tool_input_raw = std::env::var("TOOL_INPUT").unwrap_or_default();
        let tool_input = serde_json::from_str(&tool_input_raw).unwrap_or(serde_json::Value::Null);
        Some(HookInput {
            raw_tool_name: tool_name.clone(),
            tool_name,
            tool_input,
        })
    }

    fn from_stdin(raw: &str) -> Option<Self> {
        let clean = raw.trim_start_matches('\u{feff}').trim();
        if clean.is_empty() {
            return None;
        }
        if let Ok(input) = serde_json::from_str::<AntigravityHookInput>(clean) {
            let tool_name = canonical_tool_name(&input.tool_call.name);
            let tool_input = normalize_tool_input(&tool_name, input.tool_call.args);
            return Some(HookInput {
                raw_tool_name: input.tool_call.name,
                tool_name,
                tool_input,
            });
        }
        if let Ok(input) = serde_json::from_str::<HookInput>(clean) {
            if !input.tool_name.is_empty() {
                // Canonicalize here too: newer Copilot/Codex builds send the snake_case
                // `tool_name`/`tool_input` shape with harness names like "view", which
                // must still map to Read/Grep (and `path` -> `file_path`) or the tool
                // is never recognized and interception is silently skipped.
                let tool_name = canonical_tool_name(&input.tool_name);
                let tool_input = normalize_tool_input(&tool_name, input.tool_input);
                return Some(HookInput {
                    raw_tool_name: input.tool_name,
                    tool_name,
                    tool_input,
                });
            }
        }
        serde_json::from_str::<CopilotHookInput>(clean)
            .ok()
            .map(|input| normalize_copilot_input(&input.tool_name, &input.tool_args))
    }
}

/// Map a harness-specific tool name to tokenix's canonical name.
///
/// Claude Code already sends `Read`/`Grep`/`Bash`; GitHub Copilot (and some Codex
/// builds) send `view`/`read`/`grep` in either the legacy `toolName` field or the
/// newer snake_case `tool_name` field. Canonicalizing in one place means
/// interception no longer depends on which field shape or casing a harness uses.
/// Unrecognized names (Bash variants, Edit, etc.) pass through unchanged.
fn canonical_tool_name(name: &str) -> String {
    match name.to_ascii_lowercase().as_str() {
        "read" | "view" | "read_file" | "view_file" => "Read".to_string(),
        "grep" | "grep_search" => "Grep".to_string(),
        _ => name.to_string(),
    }
}

fn normalize_copilot_input(tool_name: &str, tool_args: &serde_json::Value) -> HookInput {
    let args = if let Some(raw) = tool_args.as_str() {
        serde_json::from_str(raw).unwrap_or(serde_json::Value::Null)
    } else {
        tool_args.clone()
    };

    let tool_name = canonical_tool_name(tool_name);
    let tool_input = normalize_tool_input(&tool_name, args);
    HookInput {
        raw_tool_name: tool_name.clone(),
        tool_name,
        tool_input,
    }
}

fn normalize_tool_input(tool_name: &str, args: serde_json::Value) -> serde_json::Value {
    match tool_name {
        "Read" => normalize_read_args(args),
        "Grep" => normalize_grep_args(args),
        _ => args,
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

fn normalize_grep_args(mut args: serde_json::Value) -> serde_json::Value {
    if args.get("pattern").and_then(|v| v.as_str()).is_some() {
        return args;
    }

    let pattern = args
        .get("query")
        .or_else(|| args.get("regex"))
        .or_else(|| args.get("search"))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    if let Some(pattern) = pattern {
        if let Some(obj) = args.as_object_mut() {
            obj.insert("pattern".to_string(), serde_json::Value::String(pattern));
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
    pattern.split_whitespace().count() >= min_query_words()
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

fn handle_read(tool_input: &serde_json::Value, repo_root: &Path) -> (bool, String, String) {
    let file_path = match tool_input["file_path"].as_str() {
        Some(p) => p,
        None => return (false, String::new(), "missing file_path".to_string()),
    };

    // Let targeted reads through (offset or limit already specified with a numeric value)
    let has_offset = tool_input["offset"].is_number();
    let has_limit = tool_input["limit"].is_number();
    if has_offset || has_limit {
        return (
            false,
            String::new(),
            "targeted read (offset/limit specified)".to_string(),
        );
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
        return (
            false,
            String::new(),
            format!("file not found: {}", file_path),
        );
    }

    let content = match std::fs::read_to_string(&full_path) {
        Ok(c) => c,
        Err(_) => return (false, String::new(), format!("read error: {}", file_path)),
    };

    // Re-read suppression: if the agent already received these exact bytes
    // recently, resending the file is pure waste. Unlike the outline path this
    // works for any language, since it does not parse the file.
    //
    // Only reads that actually delivered the *full content* are remembered — a
    // read answered with an outline must stay re-readable, or the agent could
    // never get past the outline. `remember_full_read` is therefore called on
    // the pass-through paths only.
    let recall_path = full_path.to_string_lossy().replace('\\', "/");
    let content_tokens = count_tokens(&content);
    let now = now_ts();
    if let Some(hit) = crate::recall::find_recent_read(&recall_path, &content, content_tokens, now)
    {
        return (
            true,
            crate::recall::read_marker(&hit, now),
            "unchanged since a recent full read".to_string(),
        );
    }
    let remember_full_read =
        || crate::recall::remember_read(&recall_path, &content, content_tokens, now);

    let line_count = content.lines().count();
    let min_lines = min_lines_for_outline();
    if line_count < min_lines {
        remember_full_read();
        return (
            false,
            String::new(),
            format!("small file ({} < {} lines)", line_count, min_lines),
        );
    }

    let outline = match get_file_outline(&full_path) {
        Some(o) => o,
        None => {
            remember_full_read();
            return (
                false,
                String::new(),
                "failed to generate outline".to_string(),
            );
        }
    };

    let rel = full_path
        .strip_prefix(repo_root)
        .unwrap_or(&full_path)
        .to_string_lossy()
        .replace('\\', "/");

    // Extensions the outliner (`chunker::detect_lang`) actually supports — keep in
    // sync with it. C/C++, VB and SQL were previously excluded here, so their large
    // files passed through full even though `generate_outline` can outline them; the
    // >=30% savings gate below still rejects any outline that isn't worth it.
    let ext = full_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let is_code = matches!(
        ext.as_str(),
        "rs" | "py"
            | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "mjs"
            | "cjs"
            | "go"
            | "c"
            | "cpp"
            | "h"
            | "hpp"
            | "cc"
            | "cxx"
            | "bas"
            | "cls"
            | "ctl"
            | "frm"
            | "sql"
            | "fnc"
            | "trg"
            | "pkg"
            | "prc"
            | "tab"
            | "vw"
    );

    if !is_code {
        return (
            false,
            String::new(),
            format!("unsupported language: .{}", ext),
        );
    }

    // Only intercept when the outline is materially smaller than the file. For
    // files of many tiny symbols the outline can be ~as large as (or larger than)
    // the source, so intercepting would cost ~full price for the outline AND force
    // a re-read for the bodies — a net token loss. Require >=30% savings.
    let file_tokens = count_tokens(&content) as i64;
    let outline_tokens = count_tokens(&outline) as i64;
    if file_tokens <= 0 || (file_tokens - outline_tokens) * 100 < file_tokens * 30 {
        return (
            false,
            String::new(),
            format!(
                "outline saves <30% ({} vs {} tokens) — passing through",
                outline_tokens, file_tokens
            ),
        );
    }

    let msg = format!(
        "{}\n\n[tokenix] File has {} lines. Showing symbol outline above.\n\
        To read a specific symbol: tokenix read {} --symbol <name>\n\
        To read specific lines:   use Read with offset/limit parameters.",
        outline, line_count, rel
    );
    (true, msg, "generated symbol outline".to_string())
}

/// Returns a path to use as a soft lock file for concurrent embed protection.
fn embed_lock_path() -> Option<std::path::PathBuf> {
    Some(dirs::cache_dir()?.join("tokenix").join("embed.lock"))
}

/// Best-effort concurrency guard for the ONNX embed call.
///
/// The fastembed model uses ~293 MB per process (130 MB model + ORT runtime).
/// If Claude Code fires parallel Grep hooks, each would load a separate model instance.
/// This guard makes concurrent semantic Greps fall through (exit 0) so the original
/// grep runs instead — limiting peak tokenix memory to one model instance at a time.
///
/// Implementation: timestamp file. Not atomically safe, but good enough for the
/// typical pattern of a few concurrent hooks per second. Stale locks (>30s) are
/// automatically overridden so a crashed process never permanently blocks the feature.
fn try_acquire_embed_slot() -> bool {
    let path = match embed_lock_path() {
        Some(p) => p,
        None => return true, // can't determine path → proceed anyway
    };

    if path.exists() {
        let stale = std::fs::metadata(&path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .map(|e| e.as_secs() >= 30)
            .unwrap_or(true);

        if !stale {
            return false; // another embed is in progress
        }
        // Remove stale lock before attempting to re-acquire
        let _ = std::fs::remove_file(&path);
    }

    let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
    // create_new is atomic: exactly one concurrent caller succeeds
    use std::io::Write;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map(|mut f| {
            let _ = f.write_all(std::process::id().to_string().as_bytes());
        })
        .is_ok()
}

fn release_embed_slot() {
    if let Some(path) = embed_lock_path() {
        let _ = std::fs::remove_file(path);
    }
}

fn index_has_embeddings(repo_root: &Path) -> bool {
    let Some(conn) = crate::store::open_db(repo_root, false).ok().flatten() else {
        return false;
    };
    conn.query_row("SELECT 1 FROM embeddings LIMIT 1", [], |_| Ok(()))
        .is_ok()
}

fn daemon_search_with_hook_timeout(
    repo_root: &Path,
    pattern: &str,
    k: usize,
    budget: usize,
    file_filter: Option<&str>,
) -> Option<String> {
    let (tx, rx) = mpsc::channel();
    let repo_root = repo_root.to_path_buf();
    let pattern = pattern.to_string();
    let file_filter = file_filter.map(str::to_string);

    std::thread::spawn(move || {
        let out = crate::daemon::daemon_search_with_autostart(
            &repo_root,
            &pattern,
            k,
            budget,
            file_filter.as_deref(),
        );
        let _ = tx.send(out);
    });

    rx.recv_timeout(Duration::from_millis(DAEMON_HOOK_TIMEOUT_MS))
        .ok()
        .flatten()
}

fn handle_grep(tool_input: &serde_json::Value, repo_root: &Path) -> (bool, String, String) {
    let pattern = match tool_input["pattern"].as_str() {
        Some(p) => p,
        None => return (false, String::new(), "missing pattern".to_string()),
    };

    // Short identifier-like patterns: try index symbol lookup before falling through
    if !is_semantic_query(pattern) {
        if looks_like_identifier(pattern) {
            if let Some(output) = symbol_lookup(pattern, repo_root) {
                return (
                    true,
                    output,
                    format!("matched symbol exact lookup: {}", pattern),
                );
            }
        }
        return (
            false,
            String::new(),
            format!("lexical query: '{}'", pattern),
        );
    }

    if !index_has_embeddings(repo_root) {
        return (
            false,
            String::new(),
            "semantic index has no embeddings".to_string(),
        );
    }

    // Try daemon first: model stays resident, ~30ms vs ~430ms cold embed.
    if let Some(output) = daemon_search_with_hook_timeout(repo_root, pattern, 20, 2500, None) {
        return (true, output, "semantic search via daemon".to_string());
    }

    // Fallback: direct embed (daemon not running or failed to start).
    // Guard prevents N×293MB spikes from parallel hook processes.
    if !try_acquire_embed_slot() {
        return (
            false,
            String::new(),
            "ONNX embed model slot locked".to_string(),
        );
    }
    let results = match query_index(repo_root, pattern, 2500, 20, None) {
        Ok(Some(r)) if !r.is_empty() => r,
        _ => {
            release_embed_slot();
            return (
                false,
                String::new(),
                "semantic search returned empty results".to_string(),
            );
        }
    };
    release_embed_slot();
    (
        true,
        format_results(&results, pattern),
        "semantic search via in-process embed".to_string(),
    )
}

fn measured_original_tokens(
    tool_name: &str,
    tool_input: &serde_json::Value,
    repo_root: &Path,
) -> Option<i64> {
    if tool_name == "Read" {
        if let Some(fp) = tool_input["file_path"].as_str() {
            let p = Path::new(fp);
            let full = if p.exists() {
                p.to_path_buf()
            } else {
                repo_root.join(fp)
            };
            if let Ok(content) = std::fs::read_to_string(&full) {
                return Some(count_tokens(&content) as i64);
            }
        }
    }
    None
}

fn original_tokens_for_log(
    tool_name: &str,
    tool_input: &serde_json::Value,
    repo_root: &Path,
    actual_tokens: i64,
) -> i64 {
    // If we cannot measure the original tool output before interception, keep
    // gain honest: log the context cost but claim zero saved tokens.
    measured_original_tokens(tool_name, tool_input, repo_root).unwrap_or(actual_tokens)
}

/// Build the PreToolUse JSON output that rewrites a Bash command's input.
/// `hookEventName` is required by Claude Code or the whole `hookSpecificOutput`
/// is ignored and the rewrite silently does not apply.
fn bash_rewrite_output(
    input: &HookInput,
    rewritten: &str,
    reason: &str,
    antigravity: bool,
) -> serde_json::Value {
    if antigravity {
        let mut args = input.tool_input.clone();
        if let Some(obj) = args.as_object_mut() {
            for key in ["command", "CommandLine", "commandLine", "command_line"] {
                obj.insert(
                    key.to_string(),
                    serde_json::Value::String(rewritten.to_string()),
                );
            }
        }
        return serde_json::json!({
            "decision": "allow",
            "reason": reason,
            "overwrite": {
                "name": input.raw_tool_name,
                "args": args
            }
        });
    }

    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "permissionDecisionReason": reason,
            "updatedInput": {
                "command": rewritten,
                "CommandLine": rewritten,
                "commandLine": rewritten,
                "command_line": rewritten,
            }
        }
    })
}

/// Build a PreToolUse output that hands the tool back a *modified argument set*
/// (as opposed to `bash_rewrite_output`, which only rewrites a command string).
fn input_rewrite_output(
    input: &HookInput,
    updated: serde_json::Value,
    reason: &str,
    antigravity: bool,
) -> serde_json::Value {
    if antigravity {
        return serde_json::json!({
            "decision": "allow",
            "reason": reason,
            "overwrite": {
                "name": input.raw_tool_name,
                "args": updated
            }
        });
    }

    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "permissionDecisionReason": reason,
            "updatedInput": updated
        }
    })
}

/// Cap injected into an uncapped content-mode Grep. Measured motivation: a
/// single unbounded lexical Grep over a large repo cost ~937k tokens because
/// `handle_grep` only intercepts semantic/symbol queries and passes every other
/// pattern through untouched. Override with `TOKENIX_GREP_HEAD_LIMIT`; `0`
/// disables the cap.
const DEFAULT_GREP_HEAD_LIMIT: i64 = 100;

fn grep_head_limit() -> i64 {
    std::env::var("TOKENIX_GREP_HEAD_LIMIT")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(DEFAULT_GREP_HEAD_LIMIT)
}

/// Returns the full replacement tool input with a `head_limit` injected, when a
/// lexical Grep asks for match *content* without bounding how much comes back.
///
/// Deliberately narrow: modes that emit one line per file (`files_with_matches`,
/// `count`) are already cheap, and an agent that set its own `head_limit` has
/// bounded itself — both pass through untouched.
fn grep_cap_input(tool_input: &serde_json::Value) -> Option<(serde_json::Value, String)> {
    let limit = grep_head_limit();
    if limit <= 0 {
        return None;
    }
    if tool_input["head_limit"].is_number() {
        return None;
    }
    if tool_input["output_mode"].as_str() != Some("content") {
        return None;
    }

    let mut updated = tool_input.clone();
    updated.as_object_mut()?.insert(
        "head_limit".to_string(),
        serde_json::Value::Number(limit.into()),
    );
    Some((
        updated,
        format!("tokenix: capped uncapped content grep at head_limit={limit}"),
    ))
}

/// Emit the capped-grep rewrite and exit, when the input qualifies. Returns
/// normally (doing nothing) for any other tool or an already-bounded grep.
fn try_grep_cap(input: &HookInput, repo_root: &Path, antigravity: bool, raw_stdin: &str) {
    if input.tool_name != "Grep" {
        return;
    }
    let Some((updated, reason)) = grep_cap_input(&input.tool_input) else {
        return;
    };
    let out = input_rewrite_output(input, updated, &reason, antigravity);
    let _ = log_hook_event(
        repo_root,
        &HookEvent {
            ts: now_ts(),
            tool: input.tool_name.clone(),
            action: "intercepted".to_string(),
            phase: "pre".to_string(),
            reason,
            // Savings are unmeasurable here — the unbounded output never runs —
            // so log zero rather than inflating `gain`.
            saved_tokens: 0,
            actual_tokens: 0,
            original_estimate: 0,
            input_preview: raw_stdin.chars().take(200).collect(),
            command: String::new(),
        },
    );
    println!("{}", serde_json::to_string(&out).unwrap_or_default());
    exit_success();
}

fn pass_through(antigravity: bool) -> ! {
    if antigravity {
        println!(r#"{{"decision":"allow","reason":"tokenix pass-through"}}"#);
    }
    std::process::exit(0);
}

fn exit_success() -> ! {
    std::process::exit(0);
}

/// Quote a string for use as a shell argument (bash and PowerShell native-exe calls).
/// Wraps in double quotes and escapes internal double-quotes as `\"`.
fn shell_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\\\""))
}

/// Quote a string as a PowerShell single-quoted literal: no `$`/backtick
/// expansion, internal single quotes doubled. Safe for passing an arbitrary
/// command string as one argument to a native exe via `& 'exe' ... 'arg'`.
fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn is_bash_tool(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "bash"
            | "powershell"
            | "cmd"
            | "shell"
            | "run_shell_command"
            | "default_api:run_shell_command"
            | "run_in_terminal"
            | "default_api:run_in_terminal"
            | "run_command"
            | "default_api:run_command"
            | "get_terminal_output"
            | "default_api:get_terminal_output"
    )
}

/// Decide whether a tool invocation must be handled by the PowerShell-aware path
/// (`& 'exe' run --shell pwsh 'cmd'`).
///
/// Claude Code's dedicated `PowerShell` tool always uses it. On Windows, generic
/// command/shell tools (Antigravity's `run_command`/`run_in_terminal`, and the same
/// names from other harnesses) also use it, because Windows executes them under
/// powershell/pwsh and the cmd-style `"exe" run "cmd"` rewrite causes parse errors.
///
/// Crucially this is gated on the tool being a command/shell tool (`is_bash_tool`).
/// Read/Grep are routed through this same hook under Antigravity (canonicalized from
/// `read_file`/`grep_search`); they must reach `handle_read`/`handle_grep`, not the
/// empty-command pass-through of the PowerShell branch.
fn should_route_powershell(tool_name: &str, antigravity: bool, is_windows: bool) -> bool {
    if tool_name == "PowerShell" || tool_name.eq_ignore_ascii_case("powershell") {
        return true;
    }
    if !is_windows || !is_bash_tool(tool_name) {
        return false;
    }
    antigravity
        || matches!(
            tool_name,
            "run_command"
                | "default_api:run_command"
                | "run_in_terminal"
                | "default_api:run_in_terminal"
        )
}

pub fn run_hook(antigravity: bool) -> Result<()> {
    // Read input: prefer env vars (Copilot), fall back to stdin (Claude Code / Codex)
    let raw_stdin = std::io::read_to_string(std::io::stdin()).unwrap_or_default();

    let input = HookInput::from_env()
        .or_else(|| HookInput::from_stdin(&raw_stdin))
        .unwrap_or_default();

    let repo_root = find_repo_root();

    let is_powershell = should_route_powershell(&input.tool_name, antigravity, cfg!(windows));
    let is_bash = is_bash_tool(&input.tool_name) && !is_powershell;
    let is_supported =
        input.tool_name == "Read" || input.tool_name == "Grep" || is_bash || is_powershell;

    if input.tool_name.is_empty() {
        pass_through(antigravity);
    }

    if !is_supported {
        let _ = log_hook_event(
            &repo_root,
            &HookEvent {
                ts: now_ts(),
                tool: input.tool_name,
                action: "pass".to_string(),
                phase: "pre".to_string(),
                reason: "unsupported tool".to_string(),
                saved_tokens: 0,
                actual_tokens: 0,
                original_estimate: 0,
                input_preview: raw_stdin.chars().take(200).collect(),
                command: String::new(),
            },
        );
        pass_through(antigravity);
    }

    if is_bash {
        let command = input.tool_input["command"]
            .as_str()
            .or_else(|| input.tool_input["CommandLine"].as_str())
            .or_else(|| input.tool_input["commandLine"].as_str())
            .or_else(|| input.tool_input["command_line"].as_str())
            .unwrap_or("")
            .trim();

        if command.is_empty() {
            pass_through(antigravity);
        }

        // Avoid infinite recursion: do not rewrite if it's already a tokenix command execution
        if command.contains("tokenix") {
            pass_through(antigravity);
        }

        // Per-command escape hatch: a TOKENIX_DISABLED=1 env prefix means
        // "give me the raw output this one time" — skip every rewrite. The
        // bypass is logged so `tokenix gain` can flag overuse.
        if command.contains("TOKENIX_DISABLED=1") {
            let _ = log_hook_event(
                &repo_root,
                &HookEvent {
                    ts: now_ts(),
                    tool: "Bash".to_string(),
                    action: "bypassed".to_string(),
                    phase: "pre".to_string(),
                    reason: "TOKENIX_DISABLED=1".to_string(),
                    saved_tokens: 0,
                    actual_tokens: 0,
                    original_estimate: 0,
                    input_preview: command.chars().take(200).collect(),
                    command: command.to_string(),
                },
            );
            pass_through(antigravity);
        }

        // Recording session active: route every in-scope command through
        // `tokenix run` so its raw output is captured to .tokenix/recordings.
        // PreToolUse is the only path that sees command output under Claude Code,
        // so this is what makes `tokenix filter record` feed filter generation.
        if crate::recordings::is_in_scope(&repo_root, command) {
            let exe_path = std::env::current_exe()
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|_| "tokenix".to_string());
            let rewritten = format!("{} run {}", shell_quote(&exe_path), shell_quote(command));
            let out = bash_rewrite_output(
                &input,
                &rewritten,
                "recording: capturing output for filter generation",
                antigravity,
            );

            let _ = log_hook_event(
                &repo_root,
                &HookEvent {
                    ts: now_ts(),
                    tool: "Bash".to_string(),
                    action: "intercepted".to_string(),
                    phase: "pre".to_string(),
                    reason: "recording capture".to_string(),
                    saved_tokens: 0,
                    actual_tokens: 0,
                    original_estimate: 0,
                    input_preview: command.chars().take(200).collect(),
                    command: command.to_string(),
                },
            );

            println!("{}", serde_json::to_string(&out).unwrap_or_default());
            exit_success();
        }

        // 1. Optimize git status -> git status --short. Let it pass through natively
        // on the second pass when the short flag is already present.
        let is_short_git_status = (command.starts_with("git status")
            || command.starts_with("git  status"))
            && (command.contains("-s") || command.contains("--short"));
        if is_short_git_status {
            pass_through(antigravity);
        }

        let status_re = regex::Regex::new(r"^git\s+status(\s+.*)?$").unwrap();
        if status_re.is_match(command) && !command.contains("-") {
            let trimmed = command.strip_prefix("git status").unwrap_or("").trim();
            let rewritten = if trimmed.is_empty() {
                "git status --short".to_string()
            } else {
                format!("git status --short {}", trimmed)
            };

            let out = bash_rewrite_output(
                &input,
                &rewritten,
                "rewrite git status to git status --short for token efficiency",
                antigravity,
            );

            let _ = log_hook_event(
                &repo_root,
                &HookEvent {
                    ts: now_ts(),
                    tool: "Bash".to_string(),
                    action: "intercepted".to_string(),
                    phase: "pre".to_string(),
                    reason: "rewrote git status to git status --short".to_string(),
                    saved_tokens: 0,
                    actual_tokens: 0,
                    original_estimate: 0,
                    input_preview: command.chars().take(200).collect(),
                    command: command.to_string(),
                },
            );

            println!("{}", serde_json::to_string(&out).unwrap_or_default());
            exit_success();
        }

        // 2. Otherwise check for other active filters to wrap in tokenix run
        let filters = crate::filters::load_all_filters();
        let unwrapped =
            crate::filters::unwrap_shell_runner(command).unwrap_or_else(|| command.to_string());

        if crate::filters::find_filter(&unwrapped, &filters).is_some() {
            let exe_path = std::env::current_exe()
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|_| "tokenix".to_string());

            let rewritten = format!("{} run {}", shell_quote(&exe_path), shell_quote(command));

            let out = bash_rewrite_output(
                &input,
                &rewritten,
                "wrapped in tokenix compression run",
                antigravity,
            );

            let _ = log_hook_event(
                &repo_root,
                &HookEvent {
                    ts: now_ts(),
                    tool: "Bash".to_string(),
                    action: "intercepted".to_string(),
                    phase: "pre".to_string(),
                    reason: "rewrote command to tokenix run".to_string(),
                    saved_tokens: 0,
                    actual_tokens: 0,
                    original_estimate: 0,
                    input_preview: command.chars().take(200).collect(),
                    command: command.to_string(),
                },
            );

            println!("{}", serde_json::to_string(&out).unwrap_or_default());
            exit_success();
        }

        pass_through(antigravity);
    }

    if is_powershell {
        let command = input.tool_input["command"]
            .as_str()
            .or_else(|| input.tool_input["CommandLine"].as_str())
            .or_else(|| input.tool_input["commandLine"].as_str())
            .or_else(|| input.tool_input["command_line"].as_str())
            .unwrap_or("")
            .trim();

        if command.is_empty() {
            pass_through(antigravity);
        }
        // Avoid recursion: the rewrite itself invokes tokenix under pwsh.
        if command.contains("tokenix") {
            pass_through(antigravity);
        }

        // Same per-command escape hatch as the Bash path ($env:TOKENIX_DISABLED
        // or an inline TOKENIX_DISABLED=1 marker).
        if command.contains("TOKENIX_DISABLED") {
            let _ = log_hook_event(
                &repo_root,
                &HookEvent {
                    ts: now_ts(),
                    tool: "PowerShell".to_string(),
                    action: "bypassed".to_string(),
                    phase: "pre".to_string(),
                    reason: "TOKENIX_DISABLED".to_string(),
                    saved_tokens: 0,
                    actual_tokens: 0,
                    original_estimate: 0,
                    input_preview: command.chars().take(200).collect(),
                    command: command.to_string(),
                },
            );
            pass_through(antigravity);
        }

        let filters = crate::filters::load_all_filters();
        if crate::filters::find_filter(command, &filters).is_some() {
            let exe_path = std::env::current_exe()
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|_| "tokenix".to_string());

            // Rewrite as a native-exe call in PowerShell syntax; `tokenix run`
            // re-executes the original command under pwsh and compresses output.
            let rewritten = format!(
                "& {} run --shell pwsh {}",
                ps_quote(&exe_path),
                ps_quote(command)
            );

            let out = bash_rewrite_output(
                &input,
                &rewritten,
                "wrapped in tokenix compression run (pwsh)",
                antigravity,
            );

            let _ = log_hook_event(
                &repo_root,
                &HookEvent {
                    ts: now_ts(),
                    tool: "PowerShell".to_string(),
                    action: "intercepted".to_string(),
                    phase: "pre".to_string(),
                    reason: "rewrote command to tokenix run (pwsh)".to_string(),
                    saved_tokens: 0,
                    actual_tokens: 0,
                    original_estimate: 0,
                    input_preview: command.chars().take(200).collect(),
                    command: command.to_string(),
                },
            );

            println!("{}", serde_json::to_string(&out).unwrap_or_default());
            exit_success();
        }

        pass_through(antigravity);
    }

    let staleness = index_staleness(&repo_root);

    if staleness.stale {
        // Bounding an unbounded grep needs no index, so a stale/missing index
        // must not disable it — this is where most repos actually sit.
        try_grep_cap(&input, &repo_root, antigravity, &raw_stdin);
        let _ = log_hook_event(
            &repo_root,
            &HookEvent {
                ts: now_ts(),
                tool: input.tool_name,
                action: "pass".to_string(),
                phase: "pre".to_string(),
                reason: staleness.reason,
                saved_tokens: 0,
                actual_tokens: 0,
                original_estimate: 0,
                input_preview: String::new(),
                command: String::new(),
            },
        );
        pass_through(antigravity);
    }

    let (intercepted, output, reason) = match input.tool_name.as_str() {
        "Read" => handle_read(&input.tool_input, &repo_root),
        "Grep" => handle_grep(&input.tool_input, &repo_root),
        _ => (false, String::new(), "unsupported tool".to_string()),
    };

    if !intercepted {
        // A lexical Grep is not something tokenix can answer from the index, but
        // it can still be bounded before it dumps every match into the context.
        try_grep_cap(&input, &repo_root, antigravity, &raw_stdin);

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
                command: String::new(),
            },
        );
        pass_through(antigravity);
    }

    let actual_tokens = count_tokens(&output) as i64;
    let original_tokens = original_tokens_for_log(
        &input.tool_name,
        &input.tool_input,
        &repo_root,
        actual_tokens,
    );
    let saved = (original_tokens - actual_tokens).max(0);

    let _ = log_hook_event(
        &repo_root,
        &HookEvent {
            ts: now_ts(),
            tool: input.tool_name.clone(),
            action: "intercepted".to_string(),
            phase: "pre".to_string(),
            reason,
            saved_tokens: saved,
            actual_tokens,
            original_estimate: original_tokens,
            input_preview: raw_stdin.chars().take(200).collect(),
            command: String::new(),
        },
    );

    if antigravity {
        println!(
            "{}",
            serde_json::json!({
                "decision": "deny",
                "reason": output
            })
        );
        std::process::exit(0);
    }

    eprintln!("{}", output);
    std::process::exit(2);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powershell_routing_targets_command_tools_not_read_grep() {
        // Claude's dedicated PowerShell tool always routes to pwsh, any platform.
        assert!(should_route_powershell("PowerShell", false, false));
        assert!(should_route_powershell("powershell", false, true));

        // On Windows, Antigravity command tools route to pwsh.
        assert!(should_route_powershell("run_command", true, true));
        assert!(should_route_powershell("run_in_terminal", true, true));

        // Regression: Read/Grep under Antigravity-on-Windows must NOT route to the
        // pwsh branch, or they bypass handle_read/handle_grep and pass through.
        assert!(!should_route_powershell("Read", true, true));
        assert!(!should_route_powershell("Grep", true, true));

        // Claude's Bash tool on Windows stays on the bash path (runs under cmd/git-bash).
        assert!(!should_route_powershell("Bash", false, true));

        // Off Windows, generic command tools stay on the bash path.
        assert!(!should_route_powershell("run_command", true, false));
    }

    #[test]
    fn ps_quote_wraps_and_escapes_single_quotes() {
        assert_eq!(ps_quote("Get-ChildItem"), "'Get-ChildItem'");
        // Internal single quotes are doubled (PowerShell literal escaping).
        assert_eq!(
            ps_quote("Select-String -Pattern 'foo'"),
            "'Select-String -Pattern ''foo'''"
        );
        // $ and backtick stay literal inside a single-quoted PS string.
        assert_eq!(ps_quote("$env:PATH"), "'$env:PATH'");
    }

    #[test]
    fn ps_rewrite_is_valid_powershell_call() {
        // The rewrite must be a native-exe call (`& 'exe' run --shell pwsh 'cmd'`)
        // that PowerShell can parse, with the original command as one literal arg.
        let exe = "C:/tokenix/bin/tokenix.exe";
        let cmd = "Get-ChildItem | Select-String 'todo'";
        let rewritten = format!("& {} run --shell pwsh {}", ps_quote(exe), ps_quote(cmd));
        assert_eq!(
            rewritten,
            "& 'C:/tokenix/bin/tokenix.exe' run --shell pwsh 'Get-ChildItem | Select-String ''todo'''"
        );
    }

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
        let raw =
            "\u{feff}{\"tool_name\":\"Grep\",\"tool_input\":{\"pattern\":\"how does auth work\"}}";
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
    fn grep_search_query_normalized() {
        let raw = r#"{"toolName":"grep_search","toolArgs":{"query":"how does auth work"}}"#;
        let input = HookInput::from_stdin(raw).unwrap();
        assert_eq!(input.tool_name, "Grep");
        assert_eq!(input.tool_input["pattern"], "how does auth work");
    }

    #[test]
    fn snake_case_grep_search_regex_normalized() {
        let raw = r#"{"tool_name":"grep_search","tool_input":{"regex":"fn.*main"}}"#;
        let input = HookInput::from_stdin(raw).unwrap();
        assert_eq!(input.tool_name, "Grep");
        assert_eq!(input.tool_input["pattern"], "fn.*main");
    }

    #[test]
    fn run_in_terminal_is_bash_tool() {
        assert!(is_bash_tool("run_in_terminal"));
        assert!(is_bash_tool("default_api:run_in_terminal"));
    }

    #[test]
    fn copilot_read_with_path_key() {
        let raw = r#"{"toolName":"view","toolArgs":{"path":"src/lib.rs"}}"#;
        let input = HookInput::from_stdin(raw).unwrap();
        assert_eq!(input.tool_name, "Read");
        assert_eq!(input.tool_input["file_path"], "src/lib.rs");
    }

    #[test]
    fn snake_case_view_normalized_to_read() {
        // Newer Copilot/Codex builds use the snake_case shape with harness tool
        // names; it must canonicalize just like the legacy camelCase shape does.
        let raw = r#"{"tool_name":"view","tool_input":{"path":"src/main.rs"}}"#;
        let input = HookInput::from_stdin(raw).unwrap();
        assert_eq!(input.tool_name, "Read");
        assert_eq!(input.tool_input["file_path"], "src/main.rs");
    }

    #[test]
    fn antigravity_tool_call_normalizes_read_file() {
        let raw = r#"{"toolCall":{"name":"read_file","args":{"path":"src/main.rs"}}}"#;
        let input = HookInput::from_stdin(raw).unwrap();
        assert_eq!(input.tool_name, "Read");
        assert_eq!(input.tool_input["file_path"], "src/main.rs");
    }

    #[test]
    fn read_intercepts_only_when_outline_saves_tokens() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("tokenix_read_hook_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);

        // Many small-but-indexable one-line functions: the per-symbol outline is
        // ~as large as the file, so intercepting would not save tokens and would
        // force a re-read. handle_read must pass through (intercepted == false).
        let dense = dir.join("dense.rs");
        let mut f = std::fs::File::create(&dense).unwrap();
        for i in 0..220 {
            writeln!(f, "pub fn s{i}(x: i64, y: i64) -> i64 {{ x + y + {i} }}").unwrap();
        }
        drop(f);
        let input = serde_json::json!({ "file_path": dense.to_string_lossy() });
        let (intercepted, _, reason) = handle_read(&input, &dir);
        assert!(
            !intercepted,
            "dense small-symbol file should pass through, got: {reason}"
        );

        // A few large functions: the outline is far smaller than the file → intercept.
        let sparse = dir.join("sparse.rs");
        let mut f = std::fs::File::create(&sparse).unwrap();
        for i in 0..6 {
            writeln!(f, "pub fn big{i}(x: i64) -> i64 {{").unwrap();
            for j in 0..50 {
                writeln!(
                    f,
                    "    let v{j} = x + {j} * {i}; // body line padding the function"
                )
                .unwrap();
            }
            writeln!(f, "    x\n}}").unwrap();
        }
        drop(f);
        let input = serde_json::json!({ "file_path": sparse.to_string_lossy() });
        let (intercepted, _, reason) = handle_read(&input, &dir);
        assert!(
            intercepted,
            "large-body file should be intercepted, got: {reason}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn grep_intercepts_do_not_claim_measured_savings() {
        let input = serde_json::json!({ "pattern": "how does auth work" });
        assert_eq!(
            original_tokens_for_log("Grep", &input, Path::new("."), 1234),
            1234
        );
    }

    #[test]
    fn unmeasured_intercepts_are_neutral_not_estimated() {
        let input = serde_json::json!({ "pattern": "anything" });
        assert_eq!(
            measured_original_tokens("Grep", &input, Path::new(".")),
            None
        );
        assert_eq!(
            original_tokens_for_log("Grep", &input, Path::new("."), 77),
            77
        );
    }

    #[test]
    fn grep_cap_injects_head_limit_on_uncapped_content_grep() {
        let args = serde_json::json!({"pattern": "foo", "output_mode": "content", "-C": 3});
        let (updated, reason) = grep_cap_input(&args).expect("should cap");
        assert_eq!(updated["head_limit"], DEFAULT_GREP_HEAD_LIMIT);
        // Every original field survives — updatedInput replaces the whole input.
        assert_eq!(updated["pattern"], "foo");
        assert_eq!(updated["-C"], 3);
        assert!(reason.contains("head_limit"));
    }

    #[test]
    fn grep_cap_respects_agent_supplied_limit() {
        let args = serde_json::json!({"pattern": "foo", "output_mode": "content", "head_limit": 5});
        assert!(grep_cap_input(&args).is_none());
    }

    #[test]
    fn grep_cap_skips_cheap_output_modes() {
        for mode in ["files_with_matches", "count"] {
            let args = serde_json::json!({"pattern": "foo", "output_mode": mode});
            assert!(grep_cap_input(&args).is_none(), "mode {mode} should pass");
        }
        // No explicit mode: the tool default is not content, so nothing to cap.
        assert!(grep_cap_input(&serde_json::json!({"pattern": "foo"})).is_none());
    }

    #[test]
    fn grep_cap_emits_valid_pretooluse_rewrite() {
        let input = HookInput {
            tool_name: "Grep".to_string(),
            tool_input: serde_json::json!({"pattern": "foo", "output_mode": "content"}),
            raw_tool_name: "Grep".to_string(),
        };
        let (updated, reason) = grep_cap_input(&input.tool_input).unwrap();
        let out = input_rewrite_output(&input, updated, &reason, false);
        let hso = &out["hookSpecificOutput"];
        assert_eq!(hso["hookEventName"], "PreToolUse");
        assert_eq!(hso["permissionDecision"], "allow");
        assert_eq!(hso["updatedInput"]["head_limit"], DEFAULT_GREP_HEAD_LIMIT);
        assert_eq!(hso["updatedInput"]["pattern"], "foo");
    }

    #[test]
    fn grep_cap_antigravity_uses_overwrite_shape() {
        let input = HookInput {
            tool_name: "Grep".to_string(),
            tool_input: serde_json::json!({"pattern": "foo", "output_mode": "content"}),
            raw_tool_name: "grep_search".to_string(),
        };
        let (updated, reason) = grep_cap_input(&input.tool_input).unwrap();
        let out = input_rewrite_output(&input, updated, &reason, true);
        assert_eq!(out["decision"], "allow");
        assert_eq!(out["overwrite"]["name"], "grep_search");
        assert_eq!(
            out["overwrite"]["args"]["head_limit"],
            DEFAULT_GREP_HEAD_LIMIT
        );
    }

    #[test]
    fn bash_rewrite_output_has_required_hook_event_name() {
        // Claude Code ignores `hookSpecificOutput` (so the rewrite never applies)
        // unless `hookEventName` is present and set to "PreToolUse".
        let input = HookInput {
            tool_name: "Bash".to_string(),
            tool_input: serde_json::json!({"command": "git status"}),
            raw_tool_name: "Bash".to_string(),
        };
        let out = bash_rewrite_output(&input, "git status --short", "test reason", false);
        let hso = &out["hookSpecificOutput"];
        assert_eq!(hso["hookEventName"], "PreToolUse");
        assert_eq!(hso["permissionDecision"], "allow");
        assert_eq!(hso["permissionDecisionReason"], "test reason");
        assert_eq!(hso["updatedInput"]["command"], "git status --short");
        // Aliases for non-Claude harnesses (Copilot/Codex) carry the same value.
        assert_eq!(hso["updatedInput"]["CommandLine"], "git status --short");
        assert_eq!(hso["updatedInput"]["commandLine"], "git status --short");
        assert_eq!(hso["updatedInput"]["command_line"], "git status --short");
    }

    #[test]
    fn antigravity_bash_rewrite_uses_native_allow_and_overwrite() {
        let input = HookInput::from_stdin(
            r#"{"toolCall":{"name":"run_command","args":{"CommandLine":"git status","Cwd":"."}}}"#,
        )
        .unwrap();
        let out = bash_rewrite_output(&input, "git status --short", "test reason", true);
        assert_eq!(out["decision"], "allow");
        assert_eq!(out["reason"], "test reason");
        assert_eq!(out["overwrite"]["name"], "run_command");
        assert_eq!(
            out["overwrite"]["args"]["CommandLine"],
            "git status --short"
        );
        assert_eq!(out["overwrite"]["args"]["Cwd"], ".");
    }
}
