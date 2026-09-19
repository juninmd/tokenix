//! Guarantees that make tokenix safe to leave on: failures are never masked,
//! repo-controlled config needs consent, credentials are not persisted, and
//! the index follows edits without anyone re-running `tokenix index`.

mod common;

use common::Sandbox;

#[test]
fn run_keeps_the_exit_code_and_the_failure_output() {
    let sb = Sandbox::new("run");
    let failed = sb.tokenix(&["run", "echo boom-failure && exit 3"]);
    assert_eq!(failed.code, 3, "the agent must see the real exit code");
    assert!(failed.stdout.contains("boom-failure"), "{}", failed.stdout);

    let ok = sb.tokenix(&["run", "echo all-good"]);
    assert_eq!(ok.code, 0);
    assert!(ok.stdout.contains("all-good"));
}

#[test]
fn repo_filters_apply_only_after_trust_and_an_edit_revokes_it() {
    let sb = Sandbox::new("trust");
    sb.write(
        ".tokenix/filters/demo.toml",
        "[filters.demo]\nmatch_command = \"^echo keep-line\"\nstrip_lines_matching = [\"noise-line\"]\n",
    );
    let cmd = "echo keep-line && echo noise-line";

    let untrusted = sb.tokenix(&["run", cmd]).stdout;
    assert!(
        untrusted.contains("noise-line"),
        "a clone's filter must not run unapproved"
    );

    assert_eq!(sb.tokenix(&["trust"]).code, 0);
    let filtered = sb.tokenix(&["run", cmd]).stdout;
    assert!(filtered.contains("keep-line"), "{filtered}");
    assert!(
        !filtered.contains("noise-line"),
        "an approved filter applies: {filtered}"
    );

    let path = sb.path(".tokenix/filters/demo.toml");
    let edited = std::fs::read_to_string(&path).unwrap() + "# edited\n";
    std::fs::write(&path, edited).unwrap();
    let revoked = sb.tokenix(&["run", cmd]).stdout;
    assert!(
        revoked.contains("noise-line"),
        "any edit revokes trust: {revoked}"
    );
}

#[test]
fn memory_roundtrips_notes_and_refuses_credentials() {
    let sb = Sandbox::new("memory");
    assert_eq!(
        sb.tokenix(&["memory", "add", "prefer small focused diffs"])
            .code,
        0
    );

    let key = "AKIAIOSFODNN7EXAMPLE"; // gitleaks:allow AWS docs example key
    let refused = sb.tokenix(&["memory", "add", &format!("deploy with {key}")]);
    assert_ne!(refused.code, 0, "a bare credential must be refused");

    let list = sb.tokenix(&["memory", "list"]).stdout;
    assert!(list.contains("prefer small focused diffs"), "{list}");
    assert!(!list.contains(key), "{list}");
}

#[test]
fn an_edited_file_is_searchable_without_reindexing() {
    let sb = Sandbox::indexed("freshness");
    let main = sb.path("src/main.rs");
    let edited = std::fs::read_to_string(&main).unwrap()
        + "\nfn late_added_symbol() -> u8 {\n    let answer = 6 * 7;\n    answer\n}\n";
    std::fs::write(&main, edited).unwrap();

    let run = sb.tokenix(&["symbols", "late_added_symbol"]);
    assert!(
        run.stdout.contains("src/main.rs") && run.stdout.contains("late_added_symbol"),
        "{}",
        run.stdout
    );
}

#[test]
fn files_that_never_get_indexed_do_not_refresh_on_every_query() {
    let sb = Sandbox::indexed("no-perpetual-refresh");
    sb.write("notes/empty.md", "");

    sb.tokenix(&["symbols", "greet"]);
    let second = sb.tokenix(&["symbols", "greet"]);
    assert!(
        !second.stderr.contains("index refreshed"),
        "an empty file must not cost a refresh on every command: {}",
        second.stderr
    );
}
