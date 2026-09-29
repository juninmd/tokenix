use anyhow::Result;
use colored::Colorize;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::chunker::{count_tokens, generate_outline, should_index};

use crate::indexer;
use crate::store::index_staleness;

struct ReadRow {
    path: String,
    lines: usize,
    raw_tokens: usize,
    outline_tokens: usize,
    saved_pct: f64,
}

struct WorkflowCase {
    label: &'static str,
    path: &'static str,
    symbol: &'static str,
}

struct WorkflowRow {
    label: &'static str,
    path: String,
    symbol: &'static str,
    raw_tokens: usize,
    outline_tokens: usize,
    target_tokens: usize,
    total_tokens: usize,
    saved_pct: f64,
    quality_ok: bool,
}

pub fn run_benchmark(repo_root: &Path, refresh_index: bool, json_out: bool) -> Result<()> {
    if json_out {
        return run_benchmark_json(repo_root, refresh_index);
    }

    println!();
    println!("{}", "=== tokenix real benchmark ===".bold());
    println!(
        "{}",
        "Measures token reduction using the actual outline, symbol and filter code.".dimmed()
    );
    println!();

    if refresh_index {
        println!("{}", "Preparing fresh-enough index...".yellow());
        let start = Instant::now();
        let (_result, stats) = indexer::index_repo(repo_root, false, |msg| {
            println!("  {}", msg.dimmed());
        })?;
        println!(
            "  indexed metadata ready in {:.1}s - {} files - {} chunks - {} stored tokens",
            start.elapsed().as_secs_f64(),
            stats.files,
            stats.chunks,
            format_num(stats.total_tokens)
        );
        println!();
    } else if index_needs_refresh(repo_root) {
        println!(
            "{}",
            "Index is stale or missing; benchmark will use available metadata only. Pass --refresh-index to re-index."
                .yellow()
        );
        println!();
    }

    let read_rows = measure_read_reduction(repo_root)?;
    print_read_reduction(&read_rows);

    let workflow_rows = measure_targeted_workflows(repo_root)?;
    print_targeted_workflows(&workflow_rows);

    let cmd_rows = measure_command_compression(repo_root)?;
    print_command_compression(&cmd_rows);

    print_verdict(&read_rows, &workflow_rows, &cmd_rows);
    print_internal_graph_stats(repo_root)?;
    Ok(())
}

fn run_benchmark_json(repo_root: &Path, refresh_index: bool) -> Result<()> {
    if refresh_index {
        let _ = indexer::index_repo(repo_root, false, |_| {})?;
    }
    let read_rows = measure_read_reduction(repo_root)?;
    let workflow_rows = measure_targeted_workflows(repo_root)?;
    let cmd_rows = measure_command_compression(repo_root)?;
    let out = serde_json::json!({
        "read_saved_pct": saved_pct(
            read_rows.iter().map(|r| r.raw_tokens).sum(),
            read_rows.iter().map(|r| r.outline_tokens).sum(),
        ),
        "workflow_saved_pct": saved_pct(
            workflow_rows.iter().map(|r| r.raw_tokens).sum(),
            workflow_rows.iter().map(|r| r.total_tokens).sum(),
        ),
        "command_saved_pct": saved_pct(
            cmd_rows.iter().map(|r| r.vanilla).sum(),
            cmd_rows.iter().map(|r| r.tokenix).sum(),
        ),
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

struct CmdRow {
    cmd: &'static str,
    vanilla: usize,
    tokenix: usize,
}

fn measure_command_compression(repo_root: &Path) -> Result<Vec<CmdRow>> {
    let cases = [
        ("git status --short", sample_git_status(repo_root)),
        ("git log -n 5", sample_git_log(repo_root)),
        ("cargo check", sample_cargo_check()),
        ("ls -R", sample_file_listing(repo_root)),
        ("npm install", sample_npm_install()),
        ("docker compose up", sample_docker_compose()),
    ];

    let mut rows = Vec::new();
    for (cmd_str, vanilla_out) in cases {
        let vanilla_tokens = count_tokens(&vanilla_out);

        let tokenix_out = crate::compress::compress_bash_output(cmd_str, &vanilla_out);
        let tokenix_tokens = count_tokens(&tokenix_out);

        rows.push(CmdRow {
            cmd: cmd_str,
            vanilla: vanilla_tokens,
            tokenix: tokenix_tokens,
        });
    }
    Ok(rows)
}

fn sample_git_status(repo_root: &Path) -> String {
    std::process::Command::new("git")
        .args(["status", "--short"])
        .current_dir(repo_root)
        .output()
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).to_string())
        .filter(|out| !out.trim().is_empty())
        .unwrap_or_else(|| " M src/query.rs\n M src/benchmark.rs\n?? notes.txt\n".to_string())
}

fn sample_git_log(repo_root: &Path) -> String {
    std::process::Command::new("git")
        .args(["log", "-n", "5", "--oneline"])
        .current_dir(repo_root)
        .output()
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).to_string())
        .filter(|out| !out.trim().is_empty())
        .unwrap_or_else(|| {
            [
                "2f9d8a1 improve semantic ranking",
                "8a73c55 add symbol graph traversal",
                "19db3de lower cpu indexing",
                "cd01b91 add codex memory hook",
                "a1f73cc release benchmark docs",
            ]
            .join("\n")
        })
}

fn sample_cargo_check() -> String {
    [
        "    Checking tokenix v0.1.0 (/path/to/tokenix)",
        "warning: unused variable: `candidate`",
        "   --> src/query.rs:214:9",
        "    |",
        "214 |     let candidate = score_path(path);",
        "    |         ^^^^^^^^^ help: if this is intentional, prefix it with an underscore: `_candidate`",
        "error[E0425]: cannot find value `MAX_INDEX_AGE_SECS` in this scope",
        "   --> src/hook.rs:88:42",
        "    |",
        "88  |     if index_staleness(repo_root, MAX_INDEX_AGE_SECS).stale {",
        "    |                                          ^^^^^^^^^^^^^^^^^^ not found in this scope",
        "error: could not compile `tokenix` due to 1 previous error; 1 warning emitted",
    ]
    .join("\n")
}

fn sample_npm_install() -> String {
    [
        "npm warn deprecated inflight@1.0.6: This module is not supported",
        "npm warn deprecated glob@7.2.3: Glob versions prior to v9 are no longer supported",
        "added 1 package, and audited 1281 packages in 12s",
        "143 packages are looking for funding",
        "  run `npm fund` for details",
        "found 0 vulnerabilities",
    ]
    .join("\n")
}

fn sample_docker_compose() -> String {
    [
        "Creating network \"app_default\" with the default driver",
        "Pulling db (postgres:16-alpine)...",
        "Building api",
        "Starting app_db_1    ... done",
        "Starting app_redis_1 ... done",
        "Starting app_api_1   ... done",
        "Attaching to app_db_1, app_redis_1, app_api_1",
        "app_api_1   | listening on :3000",
    ]
    .join("\n")
}

fn sample_file_listing(repo_root: &Path) -> String {
    let out = collect_benchmark_files(repo_root)
        .map(|files| {
            files
                .into_iter()
                .map(|path| rel_path(repo_root, &path))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    if out.trim().is_empty() {
        [
            "src/main.rs",
            "src/query.rs",
            "src/hook.rs",
            "src/chunker.rs",
            "src/compress.rs",
            "benchmark/samples/database_client.ts",
        ]
        .join("\n")
    } else {
        out
    }
}

fn print_command_compression(rows: &[CmdRow]) {
    println!("{}", "5. Command Output Compression".bold());
    println!(
        "  {:<20} {:>10} {:>10} {:>8}",
        "Command", "Vanilla", "tokenix", "Saved"
    );
    println!("  {}", "-".repeat(52).dimmed());

    for row in rows {
        println!(
            "  {:<20} {:>10} {:>10} {:>7.1}%",
            row.cmd,
            format_num(row.vanilla as i64),
            format_num(row.tokenix as i64),
            saved_pct(row.vanilla, row.tokenix),
        );
    }
    println!();
}

fn print_internal_graph_stats(repo_root: &Path) -> Result<()> {
    let conn = crate::store::open_db(repo_root, false)?.unwrap();
    let node_count: i64 = conn.query_row("SELECT COUNT(*) FROM graph_nodes", [], |r| r.get(0))?;
    let edge_count: i64 = conn.query_row("SELECT COUNT(*) FROM graph_edges", [], |r| r.get(0))?;

    println!("{}", "6. Internal Symbol Graph".bold());
    println!("  Nodes (symbols): {}", format_num(node_count).green());
    println!("  Edges (relations): {}", format_num(edge_count).green());
    println!(
        "  - tokenix uses this graph to boost RAG results by resolving callee/caller proximity."
    );
    println!();
    Ok(())
}

fn index_needs_refresh(repo_root: &Path) -> bool {
    index_staleness(repo_root).stale
}

fn collect_benchmark_files(repo_root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for dir in ["src", "benchmark/samples"] {
        let root = repo_root.join(dir);
        if root.exists() {
            collect_files_rec(&root, &mut files)?;
        }
    }
    files.sort();
    Ok(files)
}

fn collect_files_rec(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_files_rec(&path, files)?;
        } else if should_index(&path) {
            files.push(path);
        }
    }
    Ok(())
}

fn measure_read_reduction(repo_root: &Path) -> Result<Vec<ReadRow>> {
    let mut rows = Vec::new();
    for path in collect_benchmark_files(repo_root)? {
        let content = std::fs::read_to_string(&path)?;
        let lines = content.lines().count();
        if lines < 200 {
            continue;
        }
        let rel = rel_path(repo_root, &path);
        let outline = generate_outline(&content, &rel);
        let raw_tokens = count_tokens(&content);
        let outline_tokens = count_tokens(&outline);
        rows.push(ReadRow {
            path: rel,
            lines,
            raw_tokens,
            outline_tokens,
            saved_pct: saved_pct(raw_tokens, outline_tokens),
        });
    }
    Ok(rows)
}

fn measure_targeted_workflows(repo_root: &Path) -> Result<Vec<WorkflowRow>> {
    let cases = [
        WorkflowCase {
            label: "Hook fail-open logic",
            path: "src/hook.rs",
            symbol: "run_hook",
        },
        WorkflowCase {
            label: "Chunking algorithm",
            path: "src/chunker.rs",
            symbol: "chunk_rust",
        },
        WorkflowCase {
            label: "SQLite vector search",
            path: "src/store.rs",
            symbol: "search_similar",
        },
        WorkflowCase {
            label: "Rust service workflow",
            path: "benchmark/samples/user_service.rs",
            symbol: "UserService",
        },
        WorkflowCase {
            label: "TS repository workflow",
            path: "benchmark/samples/database_client.ts",
            symbol: "UserRepository",
        },
        WorkflowCase {
            label: "Go auth middleware",
            path: "benchmark/samples/api_handler.go",
            symbol: "authMiddleware",
        },
    ];

    let mut rows = Vec::new();
    for case in cases {
        let path = repo_root.join(case.path);
        if !path.exists() {
            continue;
        }
        let content = std::fs::read_to_string(&path)?;
        let outline = generate_outline(&content, case.path);
        let target = symbol_content(case.path, &content, case.symbol);
        let raw_tokens = count_tokens(&content);
        let outline_tokens = count_tokens(&outline);
        let target_tokens = count_tokens(&target);
        let total_tokens = outline_tokens + target_tokens;
        rows.push(WorkflowRow {
            label: case.label,
            path: case.path.to_string(),
            symbol: case.symbol,
            raw_tokens,
            outline_tokens,
            target_tokens,
            total_tokens,
            saved_pct: saved_pct(raw_tokens, total_tokens),
            quality_ok: target.to_lowercase().contains(&case.symbol.to_lowercase()),
        });
    }
    Ok(rows)
}

fn symbol_content(path: &str, content: &str, symbol: &str) -> String {
    let needle = symbol.to_lowercase();
    crate::chunker::chunk_file(path, content)
        .into_iter()
        .filter(|chunk| chunk.symbol.to_lowercase().contains(&needle))
        .map(|chunk| chunk.content)
        .collect::<Vec<_>>()
        .join("\n")
}

fn print_read_reduction(rows: &[ReadRow]) {
    println!("{}", "1. Read Interception: Gross Token Reduction".bold());
    if rows.is_empty() {
        println!(
            "  {}",
            "No default large-file cases found in this repository.".dimmed()
        );
        println!();
        return;
    }
    println!(
        "  {:<42} {:>6} {:>10} {:>10} {:>8}",
        "File", "Lines", "Raw", "Outline", "Saved"
    );
    println!("  {}", "-".repeat(83).dimmed());

    let mut raw_total = 0usize;
    let mut outline_total = 0usize;
    for row in rows {
        raw_total += row.raw_tokens;
        outline_total += row.outline_tokens;
        println!(
            "  {:<42} {:>6} {:>10} {:>10} {:>7.1}%",
            truncate(&row.path, 42),
            row.lines,
            format_num(row.raw_tokens as i64),
            format_num(row.outline_tokens as i64),
            row.saved_pct
        );
    }

    println!("  {}", "-".repeat(83).dimmed());
    println!(
        "  {:<42} {:>6} {:>10} {:>10} {:>7.1}%",
        "TOTAL",
        rows.len(),
        format_num(raw_total as i64),
        format_num(outline_total as i64),
        saved_pct(raw_total, outline_total)
    );
    println!();
}

fn print_targeted_workflows(rows: &[WorkflowRow]) {
    println!("{}", "2. Targeted Workflow: Outline + Symbol Read".bold());
    println!("{}", "  Baseline is reading the full file once. tokenix cost is outline plus the target symbol chunk.".dimmed());
    if rows.is_empty() {
        println!(
            "  {}",
            "No default symbol workflow cases found in this repository.".dimmed()
        );
        println!();
        return;
    }
    println!(
        "  {:<24} {:>9} {:>9} {:>9} {:>9} {:>8} {:>5}",
        "Task", "Raw", "Outline", "Target", "Total", "Saved", "OK"
    );
    println!("  {}", "-".repeat(85).dimmed());

    let mut raw_total = 0usize;
    let mut tokenix_total = 0usize;
    let mut ok_total = 0usize;
    for row in rows {
        raw_total += row.raw_tokens;
        tokenix_total += row.total_tokens;
        if row.quality_ok {
            ok_total += 1;
        }
        println!(
            "  {:<24} {:>9} {:>9} {:>9} {:>9} {:>7.1}% {:>5}",
            truncate(row.label, 24),
            format_num(row.raw_tokens as i64),
            format_num(row.outline_tokens as i64),
            format_num(row.target_tokens as i64),
            format_num(row.total_tokens as i64),
            row.saved_pct,
            if row.quality_ok {
                "yes".green()
            } else {
                "no".red()
            }
        );
        println!(
            "    {} -> --symbol {}",
            truncate(&row.path, 54).dimmed(),
            row.symbol.dimmed()
        );
    }

    println!("  {}", "-".repeat(85).dimmed());
    println!(
        "  {:<24} {:>9} {:>9} {:>9} {:>9} {:>7.1}% {:>5}",
        "TOTAL",
        format_num(raw_total as i64),
        "",
        "",
        format_num(tokenix_total as i64),
        saved_pct(raw_total, tokenix_total),
        format!("{}/{}", ok_total, rows.len())
    );
    println!();
}

fn print_verdict(read_rows: &[ReadRow], workflow_rows: &[WorkflowRow], cmd_rows: &[CmdRow]) {
    let read_raw: usize = read_rows.iter().map(|r| r.raw_tokens).sum();
    let read_outline: usize = read_rows.iter().map(|r| r.outline_tokens).sum();
    let flow_raw: usize = workflow_rows.iter().map(|r| r.raw_tokens).sum();
    let flow_tokenix: usize = workflow_rows.iter().map(|r| r.total_tokens).sum();
    let cmd_vanilla: usize = cmd_rows.iter().map(|r| r.vanilla).sum();
    let cmd_tokenix: usize = cmd_rows.iter().map(|r| r.tokenix).sum();

    println!("{}", "Verdict".bold());
    if read_rows.is_empty() {
        println!("  Read-only exploration: n/a for this benchmark case set.");
    } else {
        println!(
            "  Read-only exploration saved {:.1}% ({}) tokens on large files (Vanilla baseline).",
            saved_pct(read_raw, read_outline),
            format_num((read_raw.saturating_sub(read_outline)) as i64)
        );
    }
    if workflow_rows.is_empty() {
        println!("  Targeted workflows: n/a for this benchmark case set.");
    } else {
        println!(
            "  Targeted workflows saved {:.1}% ({}) tokens vs reading full files.",
            saved_pct(flow_raw, flow_tokenix),
            format_num((flow_raw.saturating_sub(flow_tokenix)) as i64)
        );
    }
    println!(
        "  Command output compression saved {:.1}% ({}) tokens on common tools.",
        saved_pct(cmd_vanilla, cmd_tokenix),
        format_num((cmd_vanilla.saturating_sub(cmd_tokenix)) as i64)
    );
    println!();
}

fn rel_path(repo_root: &Path, path: &Path) -> String {
    path.strip_prefix(repo_root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn saved_pct(before: usize, after: usize) -> f64 {
    if before == 0 {
        0.0
    } else {
        (1.0 - after as f64 / before as f64) * 100.0
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    format!("{}~", s.chars().take(keep).collect::<String>())
}

fn format_num(n: i64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_pct_handles_zero_baseline() {
        assert_eq!(saved_pct(0, 0), 0.0);
        assert_eq!(saved_pct(100, 25), 75.0);
        assert_eq!(saved_pct(100, 100), 0.0);
    }

    #[test]
    fn format_num_groups_thousands() {
        assert_eq!(format_num(1_234_567), "1,234,567");
        assert_eq!(format_num(42), "42");
    }
}
