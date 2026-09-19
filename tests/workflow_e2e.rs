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
fn grep_finds_a_literal_inside_its_enclosing_symbol() {
    let sb = Sandbox::indexed("grep");
    let run = sb.tokenix(&["grep", "amount / 10"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stdout.contains("src/billing.rs"), "{}", run.stdout);
    assert!(run.stdout.contains("apply_tax"), "{}", run.stdout);
}

#[test]
fn read_hook_outlines_large_files_and_passes_small_or_ranged_reads() {
    let sb = Sandbox::indexed("hook-read");
    let big = sb.path("src/billing.rs").to_string_lossy().into_owned();
    let small = sb.path("src/main.rs").to_string_lossy().into_owned();

    let whole = sb.hook("Read", json!({ "file_path": big }));
    assert_eq!(
        whole.code, 2,
        "a whole read of a 232-line file is intercepted"
    );
    assert!(
        whole.stderr.contains("[function] apply_tax"),
        "{}",
        whole.stderr
    );
    assert!(whole.stderr.lines().count() < line_count(&sb.path("src/billing.rs")));

    let ranged = sb.hook(
        "Read",
        json!({ "file_path": big, "offset": 1, "limit": 20 }),
    );
    assert_eq!(
        ranged.code, 0,
        "an explicit range is what the agent asked for"
    );

    let short = sb.hook("Read", json!({ "file_path": small }));
    assert_eq!(short.code, 0, "files under 200 lines pass through");
}

#[test]
fn grep_hook_resolves_an_identifier_to_its_definition() {
    let sb = Sandbox::indexed("hook-grep");
    let run = sb.hook("Grep", json!({ "pattern": "apply_tax" }));
    assert_eq!(run.code, 2, "{}", run.stderr);
    assert!(run.stderr.contains("src/billing.rs:5"), "{}", run.stderr);
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
