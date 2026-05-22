use anyhow::Result;
use colored::Colorize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::chunker::{count_tokens, generate_outline, should_index};
use crate::hook::MAX_INDEX_AGE_SECS;
use crate::indexer;
use crate::query::query_index;
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

struct QueryCase {
    label: &'static str,
    query: &'static str,
    expected_paths: &'static [&'static str],
}

struct QueryRow {
    label: &'static str,
    query: &'static str,
    tokens: usize,
    latency_ms: u128,
    top_files: Vec<String>,
    hit_top1: bool,
    hit_top3: bool,
}

pub fn run_benchmark(repo_root: &Path, refresh_index: bool, query_budget: usize) -> Result<()> {
    println!();
    println!("{}", "=== tokenix real benchmark ===".bold());
    println!(
        "{}",
        "Measures token reduction and retrieval quality using the actual index/search code."
            .dimmed()
    );
    println!();

    if refresh_index || index_needs_refresh(repo_root) {
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
    }

    let read_rows = measure_read_reduction(repo_root)?;
    print_read_reduction(&read_rows);

    let workflow_rows = measure_targeted_workflows(repo_root)?;
    print_targeted_workflows(&workflow_rows);

    let query_rows = measure_semantic_quality(repo_root, query_budget)?;
    print_semantic_quality(&query_rows, query_budget);

    print_verdict(&read_rows, &workflow_rows, &query_rows);
    Ok(())
}

fn index_needs_refresh(repo_root: &Path) -> bool {
    index_staleness(repo_root, MAX_INDEX_AGE_SECS).stale
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

fn measure_semantic_quality(repo_root: &Path, query_budget: usize) -> Result<Vec<QueryRow>> {
    let cases = [
        QueryCase {
            label: "Hook behavior",
            query: "how does hook fail open when index is stale or missing",
            expected_paths: &["src/hook.rs"],
        },
        QueryCase {
            label: "Chunking",
            query: "how are rust files chunked into symbols and outlines",
            expected_paths: &["src/chunker.rs"],
        },
        QueryCase {
            label: "Vector search",
            query: "how is cosine similarity search implemented in sqlite",
            expected_paths: &["src/store.rs"],
        },
        QueryCase {
            label: "Savings analytics",
            query: "how are token savings calculated from hook log",
            expected_paths: &["src/gain.rs", "src/store.rs"],
        },
        QueryCase {
            label: "Output compression",
            query: "how does cargo output compression keep errors",
            expected_paths: &["src/compress.rs"],
        },
        QueryCase {
            label: "Authentication sample",
            query: "jwt validation refresh token revocation role dependency",
            expected_paths: &["benchmark/samples/auth_middleware.py"],
        },
        QueryCase {
            label: "Database sample",
            query: "postgres transaction pool user repository pagination",
            expected_paths: &["benchmark/samples/database_client.ts"],
        },
    ];

    let mut rows = Vec::new();
    for case in cases {
        let start = Instant::now();
        let results =
            query_index(repo_root, case.query, query_budget, 20, None)?.unwrap_or_default();
        let latency_ms = start.elapsed().as_millis();
        let tokens: usize = results.iter().map(|r| r.token_count).sum();
        let top_files = unique_files(results.iter().map(|r| r.path.as_str()));
        let hit_top1 = top_files
            .first()
            .map(|p| path_expected(p, case.expected_paths))
            .unwrap_or(false);
        let hit_top3 = top_files
            .iter()
            .take(3)
            .any(|p| path_expected(p, case.expected_paths));
        rows.push(QueryRow {
            label: case.label,
            query: case.query,
            tokens,
            latency_ms,
            top_files,
            hit_top1,
            hit_top3,
        });
    }
    Ok(rows)
}

fn unique_files<'a>(paths: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for path in paths {
        if seen.insert(path.to_string()) {
            out.push(path.to_string());
        }
    }
    out
}

fn path_expected(path: &str, expected: &[&str]) -> bool {
    expected.contains(&path)
}

fn print_read_reduction(rows: &[ReadRow]) {
    println!("{}", "1. Read Interception: Gross Token Reduction".bold());
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

fn print_semantic_quality(rows: &[QueryRow], query_budget: usize) {
    println!("{}", "3. Semantic Search Quality".bold());
    println!(
        "  Budget: {} tokens/query. Hit@1 means the first returned file is expected; Hit@3 allows the first three files.",
        format_num(query_budget as i64)
    );
    println!(
        "  {:<22} {:>8} {:>8} {:>7} {:>7}  Top files",
        "Case", "Tokens", "ms", "Hit@1", "Hit@3"
    );
    println!("  {}", "-".repeat(104).dimmed());

    let mut hit1 = 0usize;
    let mut hit3 = 0usize;
    for row in rows {
        if row.hit_top1 {
            hit1 += 1;
        }
        if row.hit_top3 {
            hit3 += 1;
        }
        let top = row
            .top_files
            .iter()
            .take(3)
            .map(|p| truncate(p, 28))
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "  {:<22} {:>8} {:>8} {:>7} {:>7}  {}",
            truncate(row.label, 22),
            format_num(row.tokens as i64),
            row.latency_ms,
            yes_no(row.hit_top1),
            yes_no(row.hit_top3),
            top
        );
        println!("    {}", row.query.dimmed());
    }

    println!("  {}", "-".repeat(104).dimmed());
    println!(
        "  {:<22} {:>8} {:>8} {:>7} {:>7}",
        "TOTAL",
        "",
        "",
        format!("{}/{}", hit1, rows.len()),
        format!("{}/{}", hit3, rows.len())
    );
    println!();
}

fn print_verdict(read_rows: &[ReadRow], workflow_rows: &[WorkflowRow], query_rows: &[QueryRow]) {
    let read_raw: usize = read_rows.iter().map(|r| r.raw_tokens).sum();
    let read_outline: usize = read_rows.iter().map(|r| r.outline_tokens).sum();
    let flow_raw: usize = workflow_rows.iter().map(|r| r.raw_tokens).sum();
    let flow_tokenix: usize = workflow_rows.iter().map(|r| r.total_tokens).sum();
    let flow_ok = workflow_rows.iter().filter(|r| r.quality_ok).count();
    let hit3 = query_rows.iter().filter(|r| r.hit_top3).count();

    println!("{}", "Verdict".bold());
    println!(
        "  Read-only exploration saved {:.1}% ({}) tokens on large files.",
        saved_pct(read_raw, read_outline),
        format_num((read_raw.saturating_sub(read_outline)) as i64)
    );
    println!(
        "  Targeted workflows saved {:.1}% ({}) tokens while resolving {}/{} expected symbols.",
        saved_pct(flow_raw, flow_tokenix),
        format_num((flow_raw.saturating_sub(flow_tokenix)) as i64),
        flow_ok,
        workflow_rows.len()
    );
    println!(
        "  Semantic search found an expected file in the top 3 for {}/{} labeled queries.",
        hit3,
        query_rows.len()
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

fn yes_no(value: bool) -> colored::ColoredString {
    if value {
        "yes".green()
    } else {
        "no".red()
    }
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
