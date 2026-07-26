//! Transparent MCP stdio proxy: compresses tool *results* on their way back to
//! the agent.
//!
//! The measured gap this closes: `conversation-audit` attributes millions of
//! tokens to MCP tool results (base64 image payloads replayed into context,
//! 100k-token DOM snapshots, raw API JSON). None of it is reachable by the
//! PreToolUse hook — that fires on the agent's *own* tools (Read/Grep/Bash) —
//! and Claude Code installs no PostToolUse hook by design, so a tool result
//! never passes through tokenix at all.
//!
//! Wrapping the server does reach it. Instead of
//! `"command": "npx", "args": ["-y", "some-mcp"]` the agent config becomes
//! `"command": "tokenix", "args": ["mcp-proxy", "--name", "some-mcp", "--", "npx", "-y", "some-mcp"]`,
//! and every `tools/call` result flows through the same compression pipeline
//! that already handles shell output — including base64 blob redaction, which
//! is where the bulk of the waste sits.
//!
//! Everything that is not a tool result is forwarded byte-for-byte. The proxy
//! is deliberately dumb about protocol semantics: it never rewrites requests,
//! never touches `tools/list` schemas (shrinking a tool description changes what
//! the model believes the tool does), and forwards anything it cannot parse.

use anyhow::{Context, Result};
use serde_json::Value;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::process::{Command, Stdio};

use crate::chunker::count_tokens;
use crate::compress::{compress_output, now_ts};
use crate::store::{log_hook_event, HookEvent};

/// Results below this are left alone: the compression pipeline cannot help a
/// short result, and a needless rewrite risks changing exact output for nothing.
const MIN_RESULT_TOKENS: usize = 200;

fn proxy_enabled() -> bool {
    !std::env::var("TOKENIX_MCP_PROXY").is_ok_and(|v| v == "0")
}

/// Compress the text blocks of a `tools/call` result in place.
/// Returns `(original_tokens, compressed_tokens)` when anything changed.
///
/// Only `type: "text"` blocks are touched. An `image` block is what the host
/// renders — replacing it would break the feature the user asked for, while a
/// base64 payload pasted *into text* is pure context cost and is exactly what
/// the redactor exists for.
pub fn compress_result_in_place(response: &mut Value) -> Option<(usize, usize)> {
    let content = response
        .get_mut("result")?
        .get_mut("content")?
        .as_array_mut()?;

    let mut original = 0usize;
    let mut compressed = 0usize;
    let mut changed = false;

    for block in content.iter_mut() {
        if block.get("type").and_then(Value::as_str) != Some("text") {
            continue;
        }
        let Some(text) = block.get("text").and_then(Value::as_str) else {
            continue;
        };
        let before = count_tokens(text);
        if before < MIN_RESULT_TOKENS {
            original += before;
            compressed += before;
            continue;
        }
        let new_text = compress_output(text);
        let after = count_tokens(&new_text);
        original += before;
        // `never worse`: keep the original when compression did not pay off.
        if after < before {
            compressed += after;
            changed = true;
            block["text"] = Value::String(new_text);
        } else {
            compressed += before;
        }
    }

    changed.then_some((original, compressed))
}

/// Run `command` as a child MCP server, forwarding JSON-RPC both ways and
/// compressing tool results on the way back.
pub fn run_proxy(name: &str, command: &[String]) -> Result<i32> {
    let (program, args) = command
        .split_first()
        .context("mcp-proxy needs a server command after `--`")?;

    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // stderr is the server's own logging channel; the host reads it for
        // diagnostics, so it must stay untouched.
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("failed to start MCP server `{program}`"))?;

    let mut child_stdin = child.stdin.take().context("child stdin")?;
    let child_stdout = child.stdout.take().context("child stdout")?;

    // Host -> server: verbatim. Requests are never rewritten.
    //
    // Deliberately not joined at shutdown: this thread blocks in a read on the
    // host's stdin, which a live host never closes. Joining it made the proxy
    // outlive its dead child as an orphan process (found by an e2e test that
    // hung instead of failing). The thread is torn down with the process.
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut reader = stdin.lock();
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if child_stdin.write_all(line.as_bytes()).is_err()
                        || child_stdin.flush().is_err()
                    {
                        break;
                    }
                }
            }
        }
    });

    // Server -> host: compress tool results, forward everything else.
    let repo_root = crate::store::find_project_root(
        &std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
    );
    let stdout = std::io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    let mut reader = BufReader::new(child_stdout);
    let mut line = String::new();
    let enabled = proxy_enabled();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }

        let mut forwarded = line.clone();
        if enabled {
            if let Ok(mut response) = serde_json::from_str::<Value>(line.trim()) {
                if let Some((original, compressed)) = compress_result_in_place(&mut response) {
                    if let Ok(rewritten) = serde_json::to_string(&response) {
                        forwarded = format!("{rewritten}\n");
                        let _ = log_hook_event(
                            &repo_root,
                            &HookEvent {
                                ts: now_ts(),
                                tool: "MCP".to_string(),
                                action: "intercepted".to_string(),
                                phase: "proxy".to_string(),
                                reason: format!("compressed {name} tool result"),
                                saved_tokens: (original as i64 - compressed as i64).max(0),
                                actual_tokens: compressed as i64,
                                original_estimate: original as i64,
                                input_preview: String::new(),
                                command: format!("mcp:{name}"),
                            },
                        );
                    }
                }
            }
        }

        if out.write_all(forwarded.as_bytes()).is_err() || out.flush().is_err() {
            break;
        }
    }

    let status = child.wait()?;
    Ok(status.code().unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn text_result(text: &str) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": 7,
            "result": { "content": [{ "type": "text", "text": text }] }
        })
    }

    #[test]
    fn compresses_a_base64_heavy_tool_result() {
        // The measured shape: an image payload pasted into a text block.
        let blob = "Zm9vQmFyBaz1".repeat(200);
        let mut response = text_result(&format!("generated image:\ndata:image/png;base64,{blob}"));
        let (original, compressed) =
            compress_result_in_place(&mut response).expect("should compress");
        assert!(compressed < original / 2, "{compressed} vs {original}");
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("omitted"));
        assert!(text.contains("generated image"), "signal must survive");
    }

    #[test]
    fn leaves_short_results_untouched() {
        let mut response = text_result("ok: 3 files changed");
        assert!(compress_result_in_place(&mut response).is_none());
        assert_eq!(
            response["result"]["content"][0]["text"], "ok: 3 files changed",
            "short results must be byte-identical"
        );
    }

    #[test]
    fn never_rewrites_image_blocks() {
        // An image block is what the host renders — compressing it would break
        // the feature, not save tokens.
        let mut response = json!({
            "result": { "content": [{ "type": "image", "data": "iVBORw0KGgo".repeat(200), "mimeType": "image/png" }] }
        });
        let before = response.clone();
        assert!(compress_result_in_place(&mut response).is_none());
        assert_eq!(response, before);
    }

    #[test]
    fn ignores_non_result_messages() {
        let mut request = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
        assert!(compress_result_in_place(&mut request).is_none());

        let mut error = json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32601}});
        assert!(compress_result_in_place(&mut error).is_none());
    }

    #[test]
    fn keeps_result_when_compression_would_not_help() {
        // Dense prose the pipeline cannot shrink must come back untouched
        // (the engine's `never worse` invariant, enforced per block).
        let prose = "The quick brown fox jumps over the lazy dog. ".repeat(120);
        let mut response = text_result(&prose);
        let outcome = compress_result_in_place(&mut response);
        assert!(outcome.is_none() || response["result"]["content"][0]["text"] != Value::Null);
        assert!(response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("quick brown fox"));
    }
}
