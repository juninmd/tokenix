//! The core loop, end to end against the real binary: index a project, then
//! answer the questions an agent asks (where is X, who calls it, show me only
//! that function) and intercept the tool calls that would waste tokens.

mod common;

use common::{line_count, Sandbox};
use serde_json::json;

fn stat(stats: &str, key: &str) -> Option<u64> {
    stats
        .lines()
        .find_map(|l| l.trim().strip_prefix(key)?.trim().parse().ok())
}

#[test]
fn index_then_stats_reports_every_file() {
    let sb = Sandbox::indexed("stats");
    let run = sb.tokenix(&["stats"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(stat(&run.stdout, "files:"), Some(3), "{}", run.stdout);
    assert!(
        stat(&run.stdout, "chunks:").unwrap_or(0) > 3,
        "{}",
        run.stdout
    );
}

#[test]
fn symbol_graph_answers_where_and_who_calls() {
    let sb = Sandbox::indexed("graph");

    let symbols = sb.tokenix(&["symbols", "apply_tax"]).stdout;
    assert!(
        symbols.contains("src/billing.rs:5-7 [function] apply_tax"),
        "{symbols}"
    );

    let callers = sb.tokenix(&["callers", "apply_tax"]).stdout;
    assert!(callers.contains("invoice_total"), "{callers}");

    let callees = sb.tokenix(&["callees", "main"]).stdout;
    for callee in ["greet", "add", "invoice_total"] {
        assert!(
            callees.contains(callee),
            "main must call {callee}: {callees}"
        );
    }
}

#[test]
fn read_outlines_a_large_file_and_extracts_one_symbol_exactly() {
    let sb = Sandbox::indexed("read");
    let file = sb.path("src/billing.rs");

    let outline = sb.tokenix(&["read", "src/billing.rs"]).stdout;
    assert!(outline.contains("[function] apply_tax"), "{outline}");
    assert!(
        outline.lines().count() < line_count(&file),
        "an outline must be shorter than the file it replaces"
    );

    let body = sb
        .tokenix(&["read", "src/billing.rs", "--symbol", "apply_tax"])
        .stdout;
    assert!(
        body.contains("fn apply_tax(amount: u32) -> u32 {\n    amount + amount / 10\n}"),
        "--symbol must return the exact source: {body}"
    );
    assert!(!body.contains("helper_1"), "and nothing else: {body}");
}

#[test]
fn read_hook_never_replaces_a_read_with_an_outline_or_a_marker() {
    let sb = Sandbox::indexed("hook-read");
    let big = sb.path("src/billing.rs").to_string_lossy().into_owned();
    // An outline hides comments, test bodies and every use of a name, and an
    // A/B with an LLM showed multi-site edits going incomplete because of it.
    // The same holds for the second read of an unchanged file: the agent gets
    // the file, always.
    for attempt in ["first", "second (unchanged)"] {
        let run = sb.hook("Read", json!({ "file_path": big }));
        assert_eq!(
            run.code, 0,
            "{attempt} whole read must pass: {}",
            run.stderr
        );
        assert!(run.stderr.is_empty(), "{attempt}: {}", run.stderr);
    }
    let ranged = sb.hook(
        "Read",
        json!({ "file_path": big, "offset": 1, "limit": 20 }),
    );
    assert_eq!(ranged.code, 0);
}

#[test]
fn grep_hook_never_answers_in_place_of_the_native_grep() {
    let sb = Sandbox::indexed("hook-grep");
    // An identifier used to be answered from the index, hiding every textual
    // match of it; the agent's own grep must always be the one that runs.
    let run = sb.hook("Grep", json!({ "pattern": "apply_tax" }));
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stderr.is_empty(), "{}", run.stderr);
}

#[test]
fn hook_fails_open_without_an_index() {
    let sb = Sandbox::new("hook-no-index");
    sb.write("src/billing.rs", &common::billing_rs());
    let big = sb.path("src/billing.rs").to_string_lossy().into_owned();
    let run = sb.hook("Read", json!({ "file_path": big }));
    assert_eq!(run.code, 0, "no index must never block the agent");
}

#[test]
fn pack_stays_within_its_token_budget() {
    let sb = Sandbox::indexed("pack");
    let run = sb.tokenix(&["pack", "--budget", "300", "--format", "json"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    let pack: serde_json::Value = serde_json::from_str(&run.stdout).expect("pack JSON");
    let tokens = pack["tokens"].as_u64().expect("tokens");
    assert!(tokens > 0 && tokens <= 300, "budget exceeded: {tokens}");
    assert!(!pack["files"].as_array().expect("files").is_empty());
}

/// Every graph command can draw itself, so the answer can go straight into a PR
/// description, a doc or a Graphviz render.
#[test]
fn graph_commands_export_mermaid_dot_and_html() {
    let sb = Sandbox::indexed("export");

    let mermaid = sb
        .tokenix(&["callers", "apply_tax", "--format", "mermaid"])
        .stdout;
    assert!(mermaid.starts_with("```mermaid"), "{mermaid}");
    assert!(mermaid.contains("invoice_total"), "{mermaid}");

    let dot = sb.tokenix(&["callees", "main", "--format", "dot"]).stdout;
    assert!(dot.starts_with("digraph tokenix"), "{dot}");
    assert!(dot.contains("invoice_total"), "{dot}");

    let out = sb.path("impact.html");
    let run = sb.tokenix(&[
        "impact",
        "apply_tax",
        "--format",
        "html",
        "--output",
        out.to_str().unwrap(),
    ]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    let html = std::fs::read_to_string(&out).expect("html written");
    assert!(
        html.contains("<html") && html.contains("apply_tax"),
        "{html}"
    );

    let deps = sb
        .tokenix(&["deps", "src/main.rs", "--format", "mermaid"])
        .stdout;
    assert!(
        deps.starts_with("```mermaid") || deps.starts_with("No imports"),
        "{deps}"
    );

    // A typo is an error, never a silent text answer.
    let typo = sb.tokenix(&["callers", "apply_tax", "--format", "mermiad"]);
    assert_ne!(typo.code, 0);
    assert!(typo.stderr.contains("mermiad"), "{}", typo.stderr);
    // html is a relation-graph format only.
    let blast = sb.tokenix(&["blast", "--format", "html"]);
    assert_ne!(blast.code, 0);
}

/// The MCP server answers the same questions with the same formats.
#[test]
fn mcp_graph_tool_answers_with_locations_and_diagrams() {
    let sb = Sandbox::indexed("mcp-graph");
    let call = |name: &str, arguments: serde_json::Value| -> String {
        let req = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        });
        let run = sb.tokenix_with_stdin(&["mcp"], &format!("{req}\n"));
        let reply: serde_json::Value =
            serde_json::from_str(run.stdout.lines().next().expect("reply")).expect("json");
        reply["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no text in {reply}"))
            .to_string()
    };

    let callers = call(
        "tokenix_graph",
        json!({"symbol": "apply_tax", "direction": "callers"}),
    );
    assert!(callers.contains("invoice_total"), "{callers}");
    assert!(
        !callers.contains("items.iter()"),
        "locations, never code: {callers}"
    );

    let mermaid = call(
        "tokenix_graph",
        json!({"symbol": "apply_tax", "direction": "callers", "format": "mermaid"}),
    );
    assert!(mermaid.starts_with("```mermaid"), "{mermaid}");

    let symbols = call("tokenix_symbols", json!({"query": "apply_tax"}));
    assert!(symbols.contains("src/billing.rs:5-7"), "{symbols}");

    let map = call("tokenix_tokenmap", json!({"top": 3}));
    assert!(map.contains("src/billing.rs"), "{map}");
}

/// `tokenix facts` reads real transcripts: a tool missing in three sessions of
/// this repository, repaired the same way each time, comes out as one line a
/// person can paste — and another repository's sessions are not evidence.
///
/// Unix only: on Windows `dirs::home_dir()` asks the shell API and ignores the
/// environment, so the transcript root cannot be redirected into the sandbox.
#[cfg(all(unix, feature = "forensics"))]
#[test]
fn facts_reports_a_tool_missing_in_three_sessions_with_its_repair() {
    let sb = Sandbox::new("facts");
    let repo = std::fs::canonicalize(&sb.repo).unwrap();
    let projects = sb.home.join(".claude").join("projects");
    let session = |dir: &str, name: &str, cwd: &std::path::Path| {
        let lines = [
            json!({"cwd": cwd, "message": {"content": [
                {"type": "tool_use", "id": "a", "name": "Bash", "input": {"command": "pytest -q"}}
            ]}}),
            json!({"cwd": cwd, "message": {"content": [
                {"type": "tool_result", "tool_use_id": "a",
                 "content": "bash: line 1: pytest: command not found\n"}
            ]}}),
            json!({"cwd": cwd, "message": {"content": [
                {"type": "tool_use", "id": "b", "name": "Bash",
                 "input": {"command": "python -m pytest -q"}}
            ]}}),
            json!({"cwd": cwd, "message": {"content": [
                {"type": "tool_result", "tool_use_id": "b", "content": "4 passed in 0.31s\n"}
            ]}}),
        ];
        let dir = projects.join(dir);
        std::fs::create_dir_all(&dir).unwrap();
        let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(dir.join(name), body).unwrap();
    };
    for name in ["one.jsonl", "two.jsonl", "three.jsonl"] {
        session("this-repo", name, &repo);
    }
    // Same failure, different repository: must not be counted here.
    let elsewhere = sb.home.join("elsewhere");
    for name in ["x.jsonl", "y.jsonl", "z.jsonl"] {
        session("other-repo", name, &elsewhere);
    }

    let home = sb.home.to_string_lossy().into_owned();
    let env = [("HOME", home.as_str())];
    let md = sb.tokenix_with_env(&["facts", "--md"], &env);
    assert_eq!(md.code, 0, "{}", md.stderr);
    assert_eq!(
        md.stdout.trim(),
        "- `pytest` is not on PATH here; `python -m pytest` works."
    );

    let run = sb.tokenix_with_env(&["facts", "--json"], &env);
    let report: serde_json::Value = serde_json::from_str(&run.stdout).expect("json");
    assert_eq!(report["scanned_sessions"], 3, "{report}");
    assert_eq!(report["facts"][0]["failed_sessions"], 3, "{report}");

    // Two sessions are evidence, not a fact.
    std::fs::remove_file(projects.join("this-repo").join("three.jsonl")).unwrap();
    let two = sb.tokenix_with_env(&["facts", "--md"], &env);
    assert_eq!(two.stdout.trim(), "", "{}", two.stdout);
}
