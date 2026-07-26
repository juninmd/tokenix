//! End-to-end tests for `tokenix mcp-proxy`, driving the real binary.
//!
//! Scope split, deliberately: these cover the **transport contract** (JSON-RPC
//! forwarded intact, ids preserved, process lifecycle), because that is what
//! only a real process pair can prove. The compression behaviour itself is
//! covered by the unit tests on `compress_result_in_place`, which exercise the
//! base64/short-result/image-block/never-worse cases directly.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The child is tokenix's own MCP server, so the handshake is a real one.
fn proxy_wrapping_own_server() -> std::process::Child {
    Command::new(env!("CARGO_BIN_EXE_tokenix"))
        .args([
            "mcp-proxy",
            "--name",
            "self",
            "--",
            env!("CARGO_BIN_EXE_tokenix"),
            "mcp",
            "--profile",
            "slim",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn proxy")
}

#[test]
fn proxy_forwards_a_real_handshake_and_tool_list() {
    let mut child = proxy_wrapping_own_server();

    {
        let stdin = child.stdin.as_mut().expect("proxy stdin");
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{}}}}"#
        )
        .unwrap();
        writeln!(stdin, r#"{{"jsonrpc":"2.0","id":2,"method":"tools/list"}}"#).unwrap();
        stdin.flush().unwrap();
    }

    let stdout = child.stdout.take().expect("proxy stdout");
    let mut lines = BufReader::new(stdout).lines();

    let init: serde_json::Value =
        serde_json::from_str(&lines.next().expect("init line").expect("io")).expect("init json");
    assert_eq!(init["id"], 1, "ids must survive the proxy");
    assert_eq!(
        init["result"]["serverInfo"]["name"], "tokenix",
        "handshake must be forwarded untouched: {init}"
    );

    let list: serde_json::Value =
        serde_json::from_str(&lines.next().expect("tools line").expect("io")).expect("tools json");
    assert_eq!(list["id"], 2);
    let tools = list["result"]["tools"].as_array().expect("tools array");
    assert!(!tools.is_empty(), "tool schemas must pass through");
    assert!(
        tools[0]["inputSchema"].is_object(),
        "schemas must not be rewritten — shortening them changes tool semantics"
    );

    let _ = child.kill();
    let _ = child.wait();
}

/// Regression: the proxy used to join its stdin-pump thread at shutdown. That
/// thread blocks reading the host's stdin, which a live host never closes, so a
/// dead child left the proxy alive forever as an orphan.
#[test]
fn proxy_exits_when_the_child_exits() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_tokenix"))
        .args([
            "mcp-proxy",
            "--",
            env!("CARGO_BIN_EXE_tokenix"),
            "--version",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn proxy");

    // Host stdin stays open, exactly as a real MCP host would keep it.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match child.try_wait().expect("try_wait") {
            Some(_) => break,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                panic!("proxy did not exit after its child exited (orphaned)");
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}
