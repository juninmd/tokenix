//! Incremental indexing must not leave rows for content that is gone from disk.

mod common;

use common::Sandbox;

fn stat(stdout: &str, key: &str) -> Option<usize> {
    stdout
        .lines()
        .find_map(|l| l.trim().strip_prefix(key)?.trim().parse().ok())
}

fn git(sb: &Sandbox, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(&sb.repo)
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

#[test]
fn renamed_file_drops_its_old_path_from_the_index() {
    let sb = Sandbox::indexed("rename");
    git(&sb, &["add", "-A"]);
    git(&sb, &["commit", "-q", "-m", "seed"]);
    git(&sb, &["mv", "src/billing.rs", "src/invoicing.rs"]);

    let run = sb.tokenix(&["index", ".", "--no-embed"]);
    assert_eq!(run.code, 0, "{}", run.stderr);

    let symbols = sb.tokenix(&["symbols", "apply_tax"]).stdout;
    assert!(symbols.contains("src/invoicing.rs"), "{symbols}");
    assert!(!symbols.contains("src/billing.rs"), "ghost row: {symbols}");
    let stats = sb.tokenix(&["stats"]).stdout;
    assert_eq!(stat(&stats, "files:"), Some(3), "{stats}");
}

#[test]
fn file_shrunk_below_the_chunk_minimum_stops_serving_old_chunks() {
    let sb = Sandbox::indexed("shrink");
    sb.write("src/billing.rs", "x\n");

    let run = sb.tokenix(&["index", ".", "--no-embed"]);
    assert_eq!(run.code, 0, "{}", run.stderr);

    let symbols = sb.tokenix(&["symbols", "apply_tax"]).stdout;
    assert!(
        symbols.contains("No symbols found"),
        "stale chunk: {symbols}"
    );
    let stats = sb.tokenix(&["stats"]).stdout;
    assert_eq!(stat(&stats, "files:"), Some(2), "{stats}");

    // Regression guard: the emptied file must not read as perpetually dirty.
    let again = sb.tokenix(&["index", ".", "--no-embed"]);
    assert_eq!(again.code, 0, "{}", again.stderr);
    let stats = sb.tokenix(&["stats"]).stdout;
    assert_eq!(stat(&stats, "files:"), Some(2), "{stats}");
}
