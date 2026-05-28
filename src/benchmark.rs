use anyhow::Result;
use colored::Colorize;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use crate::chunker::{count_tokens, generate_outline, should_index};

use crate::indexer;
use crate::query::{build_task_context, query_index};
use crate::store::{count_stats, index_staleness, open_db};

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
    label: String,
    query: String,
    expected_paths: Vec<String>,
}

struct QueryRow {
    label: String,
    query: String,
    tokens: usize,
    latency_ms: u128,
    top_files: Vec<String>,
    hit_top1: bool,
    hit_top3: bool,
}

struct ContextBenchCase {
    label: String,
    task: String,
    expected_paths: Vec<String>,
}

struct ContextArmRow {
    label: String,
    arm: &'static str,
    tokens: Option<usize>,
    latency_ms: Option<u128>,
    quality_ok: Option<bool>,
    note: &'static str,
}

#[derive(Deserialize)]
struct BenchCases {
    #[serde(default)]
    context: Vec<BenchContextCase>,
}

#[derive(Deserialize)]
struct BenchContextCase {
    label: String,
    task: String,
    expected_path: String,
    #[serde(default)]
    acceptable_paths: Vec<String>,
}

pub fn run_benchmark(
    repo_root: &Path,
    refresh_index: bool,
    query_budget: usize,
    codegraph_path: Option<&Path>,
    cases_path: Option<&Path>,
) -> Result<()> {
    println!();
    println!("{}", "=== tokenix real benchmark ===".bold());
    println!(
        "{}",
        "Measures token reduction and retrieval quality using the actual index/search code."
            .dimmed()
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
            "Index is stale or missing; benchmark will use available metadata only. Pass --refresh-index to re-embed."
                .yellow()
        );
        println!();
    }

    let rtk_available = check_rtk();
    let context_cases = load_context_bench_cases(cases_path)?;

    let read_rows = measure_read_reduction(repo_root)?;
    print_read_reduction(&read_rows);

    let workflow_rows = measure_targeted_workflows(repo_root)?;
    print_targeted_workflows(&workflow_rows);

    let query_rows =
        measure_semantic_quality(repo_root, query_budget, rtk_available, &context_cases)?;
    print_semantic_quality(&query_rows, query_budget);

    let context_rows = measure_context_homologation(repo_root, codegraph_path, &context_cases)?;
    print_context_homologation(&context_rows);

    let cmd_rows = measure_command_compression(repo_root, rtk_available)?;
    print_command_compression(&cmd_rows);

    print_verdict(&read_rows, &workflow_rows, &query_rows, &cmd_rows);
    if let Some(path) = codegraph_path {
        print_codegraph_comparison(repo_root, path, &workflow_rows, &query_rows, &context_rows)?;
    } else {
        print_internal_graph_stats(repo_root)?;
    }
    Ok(())
}

fn default_context_bench_cases() -> Vec<ContextBenchCase> {
    vec![
        ContextBenchCase {
            label: "Hook fail-open".to_string(),
            task: "how does hook fail open when index is stale or missing".to_string(),
            expected_paths: vec!["src/hook.rs".to_string()],
        },
        ContextBenchCase {
            label: "Chunking".to_string(),
            task: "how are rust files chunked into symbols and outlines".to_string(),
            expected_paths: vec!["src/chunker.rs".to_string()],
        },
        ContextBenchCase {
            label: "Database repo".to_string(),
            task: "postgres transaction pool user repository pagination".to_string(),
            expected_paths: vec!["benchmark/samples/database_client.ts".to_string()],
        },
        ContextBenchCase {
            label: "Compression".to_string(),
            task: "how does cargo output compression keep errors".to_string(),
            expected_paths: vec!["src/compress.rs".to_string()],
        },
    ]
}

fn load_context_bench_cases(cases_path: Option<&Path>) -> Result<Vec<ContextBenchCase>> {
    let Some(path) = cases_path else {
        return Ok(default_context_bench_cases());
    };
    let raw = std::fs::read_to_string(path)?;
    let cases: BenchCases = toml::from_str(&raw)?;
    if cases.context.is_empty() {
        return Ok(default_context_bench_cases());
    }
    Ok(cases
        .context
        .into_iter()
        .map(|case| {
            let mut expected_paths = vec![case.expected_path];
            expected_paths.extend(case.acceptable_paths);
            expected_paths.sort();
            expected_paths.dedup();
            ContextBenchCase {
                label: case.label,
                task: case.task,
                expected_paths,
            }
        })
        .collect())
}

fn measure_context_homologation(
    repo_root: &Path,
    codegraph_path: Option<&Path>,
    cases: &[ContextBenchCase],
) -> Result<Vec<ContextArmRow>> {
    let mut rows = Vec::new();
    for case in cases {
        let expected_path = case
            .expected_paths
            .first()
            .map(String::as_str)
            .unwrap_or_default();
        let expected_file = repo_root.join(expected_path);
        let start = Instant::now();
        let vanilla = std::fs::read_to_string(&expected_file).unwrap_or_default();
        rows.push(ContextArmRow {
            label: case.label.clone(),
            arm: "vanilla",
            tokens: Some(count_tokens(&vanilla)),
            latency_ms: Some(start.elapsed().as_millis()),
            quality_ok: Some(!vanilla.is_empty()),
            note: "full expected file",
        });

        let start = Instant::now();
        let tokenix = build_task_context(repo_root, &case.task, 1200, 2).unwrap_or_default();
        rows.push(ContextArmRow {
            label: case.label.clone(),
            arm: "tokenix",
            tokens: Some(count_tokens(&tokenix)),
            latency_ms: Some(start.elapsed().as_millis()),
            quality_ok: Some(matches_expected_path(&tokenix, &case.expected_paths)),
            note: "context --budget 1200",
        });

        if let Some(root) = codegraph_path {
            let start = Instant::now();
            let codegraph = run_codegraph_context(root, repo_root, &case.task).unwrap_or_default();
            rows.push(ContextArmRow {
                label: case.label.clone(),
                arm: "codegraph",
                tokens: if codegraph.is_empty() {
                    None
                } else {
                    Some(count_tokens(&codegraph))
                },
                latency_ms: if codegraph.is_empty() {
                    None
                } else {
                    Some(start.elapsed().as_millis())
                },
                quality_ok: if codegraph.is_empty() {
                    None
                } else {
                    Some(matches_expected_path(&codegraph, &case.expected_paths))
                },
                note: "context --max-nodes 12 --max-code 4",
            });
        }

        rows.push(ContextArmRow {
            label: case.label.clone(),
            arm: "rtk",
            tokens: None,
            latency_ms: None,
            quality_ok: None,
            note: "not a code-context tool",
        });
    }
    Ok(rows)
}

fn matches_expected_path(output: &str, expected_paths: &[String]) -> bool {
    expected_paths.iter().any(|path| output.contains(path))
}

fn run_codegraph_context(root: &Path, repo_root: &Path, task: &str) -> Result<String> {
    let candidates = [root.join("dist/bin/codegraph.js")];
    let Some(cli) = candidates.iter().find(|path| path.exists()) else {
        return Ok(String::new());
    };
    let out = std::process::Command::new("node")
        .arg(cli)
        .arg("context")
        .arg(task)
        .arg("--path")
        .arg(repo_root)
        .arg("--max-nodes")
        .arg("12")
        .arg("--max-code")
        .arg("4")
        .output()?;
    if !out.status.success() {
        return Ok(String::new());
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn print_context_homologation(rows: &[ContextArmRow]) {
    println!(
        "{}",
        "4. Context Homologation: Vanilla vs tokenix vs CodeGraph vs RTK".bold()
    );
    println!(
        "  {:<18} {:<10} {:>9} {:>8} {:>7}  Note",
        "Case", "Arm", "Tokens", "ms", "OK"
    );
    println!("  {}", "-".repeat(86).dimmed());
    for row in rows {
        let tokens = row
            .tokens
            .map(|t| format_num(t as i64))
            .unwrap_or_else(|| "n/a".to_string());
        let latency = row
            .latency_ms
            .map(|ms| ms.to_string())
            .unwrap_or_else(|| "n/a".to_string());
        let ok = row
            .quality_ok
            .map(|ok| if ok { "yes".green() } else { "no".red() }.to_string())
            .unwrap_or_else(|| "n/a".to_string());
        println!(
            "  {:<18} {:<10} {:>9} {:>8} {:>7}  {}",
            truncate(&row.label, 18),
            row.arm,
            tokens,
            latency,
            ok,
            row.note.dimmed()
        );
    }

    println!("  {}", "-".repeat(86).dimmed());
    for arm in ["vanilla", "tokenix", "codegraph"] {
        let arm_rows: Vec<&ContextArmRow> = rows
            .iter()
            .filter(|row| row.arm == arm && row.tokens.is_some())
            .collect();
        if arm_rows.is_empty() {
            continue;
        }
        let token_sum: usize = arm_rows.iter().filter_map(|row| row.tokens).sum();
        let hits = arm_rows
            .iter()
            .filter(|row| row.quality_ok == Some(true))
            .count();
        println!(
            "  {:<18} {:<10} {:>9} {:>8} {:>7}",
            "TOTAL",
            arm,
            format_num(token_sum as i64),
            "",
            format!("{hits}/{}", arm_rows.len())
        );
    }
    println!();
}

fn check_rtk() -> bool {
    let cmd = if cfg!(windows) { "rtk.exe" } else { "rtk" };
    std::process::Command::new(cmd)
        .arg("--version")
        .output()
        .is_ok()
}

struct CmdRow {
    cmd: &'static str,
    vanilla: usize,
    tokenix: usize,
    rtk: Option<usize>,
}

fn measure_command_compression(repo_root: &Path, rtk: bool) -> Result<Vec<CmdRow>> {
    let cases = [
        ("git status", sample_git_status(repo_root)),
        ("git log -n 5", sample_git_log(repo_root)),
        ("cargo check", sample_cargo_check()),
        ("ls -R", sample_file_listing(repo_root)),
    ];

    let mut rows = Vec::new();
    for (cmd_str, vanilla_out) in cases {
        let vanilla_tokens = count_tokens(&vanilla_out);

        let tokenix_out = crate::compress::compress_bash_output(cmd_str, &vanilla_out);
        let tokenix_tokens = count_tokens(&tokenix_out);

        let rtk_tokens = rtk
            .then(|| rtk_pipe_filter(cmd_str, &vanilla_out).map(|out| count_tokens(&out)))
            .flatten();

        rows.push(CmdRow {
            cmd: cmd_str,
            vanilla: vanilla_tokens,
            tokenix: tokenix_tokens,
            rtk: rtk_tokens,
        });
    }
    Ok(rows)
}

fn rtk_pipe_filter(cmd: &str, input: &str) -> Option<String> {
    let filter = match cmd {
        "git status" => "git-status",
        "git log -n 5" => "git-log",
        "cargo check" => "cargo",
        "ls -R" => "find",
        _ => return None,
    };
    let exe = if cfg!(windows) { "rtk.exe" } else { "rtk" };
    let mut child = Command::new(exe)
        .args(["pipe", "-f", filter])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.as_mut()?.write_all(input.as_bytes()).ok()?;
    let out = child.wait_with_output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

fn sample_git_status(repo_root: &Path) -> String {
    std::process::Command::new("git")
        .args(["status", "--short"])
        .current_dir(repo_root)
        .output()
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).to_string())
        .filter(|out| !out.trim().is_empty())
        .unwrap_or_else(|| " M src/query.rs\n M src/benchmark.rs\n?? .cgcignore\n".to_string())
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
        "  {:<20} {:>10} {:>10} {:>10} {:>8} {:>9}",
        "Command", "Vanilla", "tokenix", "RTK", "Saved", "vs RTK"
    );
    println!("  {}", "-".repeat(75).dimmed());

    for row in rows {
        let rtk_str = row
            .rtk
            .map(|t| format_num(t as i64))
            .unwrap_or_else(|| "n/a".to_string());
        let vs_rtk = row
            .rtk
            .map(|rtk| if row.tokenix <= rtk { "ok" } else { "behind" })
            .unwrap_or("n/a");
        println!(
            "  {:<20} {:>10} {:>10} {:>10} {:>7.1}% {:>9}",
            row.cmd,
            format_num(row.vanilla as i64),
            format_num(row.tokenix as i64),
            rtk_str,
            saved_pct(row.vanilla, row.tokenix),
            vs_rtk
        );
    }
    println!();
}

fn print_internal_graph_stats(repo_root: &Path) -> Result<()> {
    let conn = crate::store::open_db(repo_root, false)?.unwrap();
    let node_count: i64 = conn.query_row("SELECT COUNT(*) FROM graph_nodes", [], |r| r.get(0))?;
    let edge_count: i64 = conn.query_row("SELECT COUNT(*) FROM graph_edges", [], |r| r.get(0))?;

    println!("{}", "5. Internal Symbol Graph (CodeGraph baseline)".bold());
    println!("  Nodes (symbols): {}", format_num(node_count).green());
    println!("  Edges (relations): {}", format_num(edge_count).green());
    println!(
        "  - tokenix uses this graph to boost RAG results by resolving callee/caller proximity."
    );
    println!("  - CodeGraph MCP servers typically offer similar structural context.");
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

fn measure_semantic_quality(
    repo_root: &Path,
    query_budget: usize,
    _rtk: bool,
    context_cases: &[ContextBenchCase],
) -> Result<Vec<QueryRow>> {
    let cases = if context_cases.is_empty() {
        default_query_cases()
    } else {
        context_cases
            .iter()
            .map(|case| QueryCase {
                label: case.label.clone(),
                query: case.task.clone(),
                expected_paths: case.expected_paths.clone(),
            })
            .collect()
    };

    let mut rows = Vec::new();
    for case in cases {
        let start = Instant::now();
        let results =
            query_index(repo_root, &case.query, query_budget, 20, None)?.unwrap_or_default();
        let latency_ms = start.elapsed().as_millis();
        let tokens: usize = results.iter().map(|r| r.token_count).sum();
        let top_files = unique_files(results.iter().map(|r| r.path.as_str()));
        let hit_top1 = top_files
            .first()
            .map(|p| path_expected(p, &case.expected_paths))
            .unwrap_or(false);
        let hit_top3 = top_files
            .iter()
            .take(3)
            .any(|p| path_expected(p, &case.expected_paths));

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

fn default_query_cases() -> Vec<QueryCase> {
    vec![
        QueryCase {
            label: "Hook behavior".to_string(),
            query: "how does hook fail open when index is stale or missing".to_string(),
            expected_paths: vec!["src/hook.rs".to_string()],
        },
        QueryCase {
            label: "Chunking".to_string(),
            query: "how are rust files chunked into symbols and outlines".to_string(),
            expected_paths: vec!["src/chunker.rs".to_string()],
        },
        QueryCase {
            label: "Vector search".to_string(),
            query: "how is cosine similarity search implemented in sqlite".to_string(),
            expected_paths: vec!["src/store.rs".to_string()],
        },
        QueryCase {
            label: "Savings analytics".to_string(),
            query: "how are token savings calculated from hook log".to_string(),
            expected_paths: vec!["src/gain.rs".to_string(), "src/store.rs".to_string()],
        },
        QueryCase {
            label: "Output compression".to_string(),
            query: "how does cargo output compression keep errors".to_string(),
            expected_paths: vec!["src/compress.rs".to_string()],
        },
        QueryCase {
            label: "Authentication sample".to_string(),
            query: "jwt validation refresh token revocation role dependency".to_string(),
            expected_paths: vec!["benchmark/samples/auth_middleware.py".to_string()],
        },
        QueryCase {
            label: "Database sample".to_string(),
            query: "postgres transaction pool user repository pagination".to_string(),
            expected_paths: vec!["benchmark/samples/database_client.ts".to_string()],
        },
    ]
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

fn path_expected(path: &str, expected: &[String]) -> bool {
    expected.iter().any(|candidate| candidate == path)
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
            truncate(&row.label, 22),
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

fn print_verdict(
    read_rows: &[ReadRow],
    workflow_rows: &[WorkflowRow],
    query_rows: &[QueryRow],
    cmd_rows: &[CmdRow],
) {
    let read_raw: usize = read_rows.iter().map(|r| r.raw_tokens).sum();
    let read_outline: usize = read_rows.iter().map(|r| r.outline_tokens).sum();
    let flow_raw: usize = workflow_rows.iter().map(|r| r.raw_tokens).sum();
    let flow_tokenix: usize = workflow_rows.iter().map(|r| r.total_tokens).sum();
    let hit3 = query_rows.iter().filter(|r| r.hit_top3).count();
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
    println!(
        "  Semantic search found an expected file in the top 3 for {}/{} labeled queries.",
        hit3,
        query_rows.len()
    );
    println!();
}

fn print_codegraph_comparison(
    repo_root: &Path,
    codegraph_path: &Path,
    workflow_rows: &[WorkflowRow],
    _query_rows: &[QueryRow],
    context_rows: &[ContextArmRow],
) -> Result<()> {
    let flow_raw: usize = workflow_rows.iter().map(|r| r.raw_tokens).sum();
    let flow_tokenix: usize = workflow_rows.iter().map(|r| r.total_tokens).sum();
    let readme = read_readme(codegraph_path);
    let codegraph_cli = [codegraph_path.join("dist/bin/codegraph.js")]
        .into_iter()
        .find(|path| path.exists());
    let tokenix_stats = open_db(repo_root, false)?
        .as_ref()
        .and_then(|conn| count_stats(conn).ok());
    let codegraph_stats = codegraph_cli
        .as_ref()
        .and_then(|cli| codegraph_status(cli, repo_root).ok())
        .flatten();

    println!("{}", "6. CodeGraph Comparison".bold());
    if codegraph_cli.is_some() {
        println!("  Status: {}", "local CLI available".green());
    } else {
        println!(
            "  Status: {}",
            "local CLI not built; static repo signals only".yellow()
        );
    }

    println!("  {}", "-".repeat(91).dimmed());
    println!(
        "  {:<28} {:<28} {:<28}",
        "Capability", "tokenix (measured)", "CodeGraph (measured/est)"
    );
    println!("  {}", "-".repeat(91).dimmed());

    let avg_tokens = |arm: &str| {
        let rows: Vec<&ContextArmRow> = context_rows
            .iter()
            .filter(|row| row.arm == arm && row.tokens.is_some())
            .collect();
        if rows.is_empty() {
            "n/a".to_string()
        } else {
            format_num(
                (rows.iter().filter_map(|row| row.tokens).sum::<usize>() / rows.len()) as i64,
            )
        }
    };
    let avg_latency = |arm: &str| {
        let rows: Vec<&ContextArmRow> = context_rows
            .iter()
            .filter(|row| row.arm == arm && row.latency_ms.is_some())
            .collect();
        if rows.is_empty() {
            "n/a".to_string()
        } else {
            format!(
                "{}ms",
                rows.iter().filter_map(|row| row.latency_ms).sum::<u128>() / rows.len() as u128
            )
        }
    };

    println!(
        "  {:<28} {:<28} {:<28}",
        "Context Tokens (avg)",
        avg_tokens("tokenix"),
        avg_tokens("codegraph")
    );
    println!(
        "  {:<28} {:<28} {:<28}",
        "Token reduction (workflow)",
        if workflow_rows.is_empty() {
            "n/a".to_string()
        } else {
            format!("{:.1}%", saved_pct(flow_raw, flow_tokenix))
        },
        "not applicable (graph-based)"
    );
    println!(
        "  {:<28} {:<28} {:<28}",
        "Context quality",
        quality_summary(context_rows, "tokenix"),
        quality_summary(context_rows, "codegraph")
    );
    println!(
        "  {:<28} {:<28} {:<28}",
        "Search Latency",
        avg_latency("tokenix"),
        avg_latency("codegraph")
    );
    println!(
        "  {:<28} {:<28} {:<28}",
        "Indexed files",
        tokenix_stats
            .as_ref()
            .map(|stats| format_num(stats.files))
            .unwrap_or_else(|| "n/a".to_string()),
        codegraph_stats
            .as_ref()
            .map(|stats| format_num(stats.files))
            .unwrap_or_else(|| "n/a".to_string())
    );
    println!(
        "  {:<28} {:<28} {:<28}",
        "Index shape",
        tokenix_stats
            .as_ref()
            .map(|stats| format!("{} chunks", format_num(stats.chunks)))
            .unwrap_or_else(|| "n/a".to_string()),
        codegraph_stats
            .as_ref()
            .map(|stats| {
                format!(
                    "{} nodes / {} edges",
                    format_num(stats.nodes),
                    format_num(stats.edges)
                )
            })
            .unwrap_or_else(|| "n/a".to_string())
    );
    println!(
        "  {:<28} {:<28} {:<28}",
        "MCP/context feature",
        "native CLI context",
        yes_no_plain(contains_any(&readme, &["mcp", "context", "graph"]))
    );
    println!();
    Ok(())
}

fn quality_summary(rows: &[ContextArmRow], arm: &str) -> String {
    let measured: Vec<&ContextArmRow> = rows
        .iter()
        .filter(|row| row.arm == arm && row.quality_ok.is_some())
        .collect();
    if measured.is_empty() {
        return "n/a".to_string();
    }
    let ok = measured
        .iter()
        .filter(|row| row.quality_ok == Some(true))
        .count();
    format!("{}/{}", ok, measured.len())
}

struct CodeGraphStatus {
    files: i64,
    nodes: i64,
    edges: i64,
}

fn codegraph_status(cli: &Path, repo_root: &Path) -> Result<Option<CodeGraphStatus>> {
    let out = Command::new("node")
        .arg(cli)
        .arg("status")
        .arg(repo_root)
        .output()?;
    if !out.status.success() {
        return Ok(None);
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok(Some(CodeGraphStatus {
        files: parse_status_count(&stdout, "Files:").unwrap_or(0),
        nodes: parse_status_count(&stdout, "Nodes:").unwrap_or(0),
        edges: parse_status_count(&stdout, "Edges:").unwrap_or(0),
    }))
}

fn parse_status_count(stdout: &str, label: &str) -> Option<i64> {
    stdout
        .lines()
        .find_map(|line| line.split_once(label).map(|(_, value)| value))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.replace(',', "").parse().ok())
}

fn read_readme(root: &Path) -> String {
    for name in ["README.md", "readme.md", "README"] {
        let path = root.join(name);
        if let Ok(content) = std::fs::read_to_string(path) {
            return content.to_ascii_lowercase();
        }
    }
    String::new()
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn yes_no_plain(value: bool) -> &'static str {
    if value {
        "claimed"
    } else {
        "not found"
    }
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
