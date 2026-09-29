use anyhow::Result;
use colored::Colorize;

use crate::ui::{kv, section};

/// `tokenix doctor` — diagnose the install: bundled and user filters, and any
/// active recording session. Read-only.
pub fn run_doctor() -> Result<()> {
    println!();
    println!("{}", "tokenix doctor".bold());
    println!("{}", "  environment diagnostics".dimmed());
    println!();

    section("Build");
    kv("version", env!("CARGO_PKG_VERSION"));
    println!();

    section("Filters");
    let bundled = crate::filters::load_bundled_filters_named().len();
    let cases = crate::filters::bundled_test_case_count();
    kv(
        "bundled",
        &format!("{bundled} filters · {cases} embedded golden cases"),
    );
    let named: Vec<(String, crate::filters::FilterDef)> = crate::filters::load_user_filters_named()
        .into_iter()
        .chain(crate::filters::load_local_filters_named())
        .collect();
    let mut issue_count = 0usize;
    for (name, def) in &named {
        let issues = crate::filters::semantic_filter_issues(def)
            .into_iter()
            .chain(crate::filters::regex_issues(def));
        for issue in issues {
            kv(name, &issue);
            issue_count += 1;
        }
    }
    if issue_count == 0 {
        kv(
            "user/local filters",
            &format!("{} loaded, no config issues", named.len()),
        );
    }
    println!();

    section("Recordings");
    match std::env::current_dir() {
        Ok(cwd) => {
            let root = crate::store::find_project_root(&cwd);
            if crate::recordings::is_active(&root) {
                let summary = crate::recordings::summary(&root);
                let captures: usize = summary.iter().map(|(_, n, _)| n).sum();
                let bytes: u64 = summary.iter().map(|(_, _, b)| b).sum();
                kv(
                    "session",
                    &format!(
                        "● active — {} command(s), {} capture(s), {:.0} KB",
                        summary.len(),
                        captures,
                        bytes as f64 / 1024.0
                    ),
                );
                kv("stop with", "tokenix filter record stop");
            } else {
                kv("session", "none — start with `tokenix filter record start`");
            }
        }
        Err(_) => kv("session", "unknown (no working directory)"),
    }
    println!();

    Ok(())
}
