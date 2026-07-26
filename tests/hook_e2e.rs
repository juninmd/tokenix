//! End-to-end hook tests: spawn the real `tokenix hook` binary with agent
//! payload shapes on stdin and assert the PreToolUse JSON contract it emits.
//!
//! This is the layer where real-world regressions actually happened (Claude's
//! `{stdout,stderr}` tool_response shape, PowerShell tool-name casing, the
//! hook-post silent no-op) — unit tests on inner functions cannot catch a
//! broken end-to-end contract because the handler exits the process.

use std::io::Write;
use std::process::{Command, Stdio};

/// Run `tokenix hook` with `payload` on stdin from an isolated temp cwd (the
/// hook writes its event log relative to the repo root it detects, so tests
/// must not run inside this repository).
fn run_hook(payload: &str) -> (String, i32) {
    let dir = std::env::temp_dir().join(format!(
        "tokenix-hook-e2e-{}-{:x}",
        std::process::id(),
        payload.len()
    ));
    std::fs::create_dir_all(&dir).expect("temp cwd");
    let mut child = Command::new(env!("CARGO_BIN_EXE_tokenix"))
        .arg("hook")
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tokenix hook");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write payload");
    let out = child.wait_with_output().expect("hook exit");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

fn claude_bash_payload(command: &str) -> String {
    format!(
        r#"{{"session_id":"e2e-test","transcript_path":"/tmp/t.jsonl","cwd":"/tmp","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{{"command":{cmd},"description":"e2e"}}}}"#,
        cmd = serde_json::to_string(command).unwrap()
    )
}

fn updated_command(stdout: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    let hso = &v["hookSpecificOutput"];
    assert_eq!(
        hso["hookEventName"], "PreToolUse",
        "rewrite JSON must carry the PreToolUse hookEventName (Claude ignores it otherwise)"
    );
    hso["updatedInput"]["command"].as_str().map(str::to_string)
}

#[test]
fn claude_bash_filtered_command_is_rewritten_to_tokenix_run() {
    let (stdout, code) = run_hook(&claude_bash_payload("terraform plan -out tf.plan"));
    assert_eq!(code, 0);
    let cmd = updated_command(&stdout).expect("expected a rewrite for a filter-matching command");
    assert!(
        cmd.contains(" run ") && cmd.contains("terraform plan -out tf.plan"),
        "must wrap the original command in `tokenix run`: {cmd}"
    );
}

#[test]
fn claude_git_status_is_rewritten_to_short() {
    let (stdout, code) = run_hook(&claude_bash_payload("git status"));
    assert_eq!(code, 0);
    let cmd = updated_command(&stdout).expect("git status must be rewritten");
    assert_eq!(cmd, "git status --short");
}

#[test]
fn tokenix_disabled_prefix_passes_through() {
    let (stdout, code) = run_hook(&claude_bash_payload("TOKENIX_DISABLED=1 terraform plan"));
    assert_eq!(code, 0);
    assert!(
        stdout.trim().is_empty(),
        "bypassed command must not be rewritten, got: {stdout}"
    );
}

#[test]
fn recursive_tokenix_command_passes_through() {
    let (stdout, code) = run_hook(&claude_bash_payload("tokenix run \"git status\""));
    assert_eq!(code, 0);
    assert!(stdout.trim().is_empty(), "no recursion rewrite: {stdout}");
}

#[test]
fn unfiltered_command_passes_through() {
    let (stdout, code) = run_hook(&claude_bash_payload("some-unknown-tool-xyz --flag value"));
    assert_eq!(code, 0);
    assert!(stdout.trim().is_empty(), "no filter → no rewrite: {stdout}");
}

#[test]
fn help_invocation_passes_through() {
    let (stdout, code) = run_hook(&claude_bash_payload("terraform plan --help"));
    assert_eq!(code, 0);
    assert!(
        stdout.trim().is_empty(),
        "help output must never be filtered: {stdout}"
    );
}

#[test]
fn malformed_stdin_fails_open() {
    let (stdout, code) = run_hook("this is not json {");
    assert_eq!(code, 0, "hook must fail open, never block the agent");
    assert!(stdout.trim().is_empty());
}

#[test]
fn empty_tool_name_passes_through() {
    let (stdout, code) =
        run_hook(r#"{"hook_event_name":"PreToolUse","tool_name":"","tool_input":{}}"#);
    assert_eq!(code, 0);
    assert!(stdout.trim().is_empty());
}

/// An uncapped content grep must come back with a `head_limit` injected — and
/// it must work from a temp cwd with no index at all, since the stale-index gate
/// exits before the tool handlers.
#[test]
fn uncapped_content_grep_gets_head_limit() {
    let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"Grep","tool_input":{"pattern":"foo","output_mode":"content","-C":3}}"#;
    let (stdout, code) = run_hook(payload);
    assert_eq!(code, 0);
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).unwrap_or_else(|_| panic!("expected JSON: {stdout}"));
    let updated = &v["hookSpecificOutput"]["updatedInput"];
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    assert!(
        updated["head_limit"].is_number(),
        "grep must be capped: {stdout}"
    );
    assert_eq!(updated["pattern"], "foo", "original args preserved");
    assert_eq!(updated["-C"], 3);
}

#[test]
fn bounded_grep_passes_through_untouched() {
    let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"Grep","tool_input":{"pattern":"foo","output_mode":"content","head_limit":20}}"#;
    let (stdout, code) = run_hook(payload);
    assert_eq!(code, 0);
    assert!(
        stdout.trim().is_empty(),
        "agent-bounded grep must not be rewritten: {stdout}"
    );
}

#[test]
fn files_with_matches_grep_passes_through() {
    let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"Grep","tool_input":{"pattern":"foo","output_mode":"files_with_matches"}}"#;
    let (stdout, code) = run_hook(payload);
    assert_eq!(code, 0);
    assert!(
        stdout.trim().is_empty(),
        "cheap output mode must not be rewritten: {stdout}"
    );
}

/// The PowerShell tool routes through `run --shell pwsh` with native-exe
/// quoting — Windows-only behavior (exact tool name "PowerShell"; the
/// lowercase variants route through the bash path).
#[cfg(windows)]
#[test]
fn powershell_tool_gets_pwsh_shell_rewrite() {
    let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"PowerShell","tool_input":{"command":"Get-Content src/main.rs"}}"#;
    let (stdout, code) = run_hook(payload);
    assert_eq!(code, 0);
    let cmd = updated_command(&stdout).expect("PowerShell command must be rewritten");
    assert!(
        cmd.starts_with("& '") && cmd.contains("run --shell pwsh"),
        "must be a native-exe pwsh call: {cmd}"
    );
    assert!(
        cmd.contains("Get-Content src/main.rs"),
        "original command preserved: {cmd}"
    );
}

#[cfg(windows)]
#[test]
fn powershell_disabled_env_passes_through() {
    let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"PowerShell","tool_input":{"command":"$env:TOKENIX_DISABLED='1'; Get-Content big.log"}}"#;
    let (stdout, code) = run_hook(payload);
    assert_eq!(code, 0);
    assert!(
        stdout.trim().is_empty(),
        "bypass must skip rewrite: {stdout}"
    );
}
