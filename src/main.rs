mod benchmark;
mod chunker;
mod cmd_filter;
mod compress;
mod daemon;
mod embed;
mod filters;
mod gain;
mod hook;
mod indexer;
mod query;
mod store;
mod mcp;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use colored::Colorize;
use std::path::{Path, PathBuf};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
#[command(name = "tokenix", version = VERSION, about = "Local semantic index for LLM token optimization")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Clone, ValueEnum, Debug)]
enum Tool {
    #[value(name = "claude-code")]
    ClaudeCode,
    #[value(name = "copilot")]
    Copilot,
    #[value(name = "codex")]
    Codex,
    #[value(name = "mcp")]
    Mcp,
    #[value(name = "all")]
    All,
}

#[derive(Subcommand)]
enum Commands {
    /// Index a repository for semantic search
    Index {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(short, long, help = "Force reindex all files")]
        force: bool,
        #[arg(long, help = "Skip if index is fresh (used by session hooks)")]
        if_stale: bool,
    },
    /// Semantic search over the indexed repository
    Query {
        text: String,
        #[arg(short, long, default_value_t = 3000)]
        budget: usize,
        #[arg(long, default_value_t = 20)]
        k: usize,
        #[arg(short, long, help = "Filter to specific file path")]
        file: Option<String>,
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Smart file reader - shows outline by default for large files
    Read {
        file: String,
        #[arg(short, long, help = "Specific symbol to show")]
        symbol: Option<String>,
        #[arg(short, long, help = "Line range e.g. 10-50")]
        lines: Option<String>,
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Show token savings analytics
    Gain {
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
        #[arg(long, help = "Show per-call history")]
        history: bool,
    },
    /// Run a reproducible token-savings and retrieval-quality benchmark
    Benchmark {
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
        #[arg(long, help = "Refresh index metadata before measuring")]
        refresh_index: bool,
        #[arg(
            long,
            default_value_t = 2500,
            help = "Token budget for semantic queries"
        )]
        budget: usize,
    },
    /// Install hook for one or more AI coding tools
    InstallHook {
        #[arg(
            long,
            value_enum,
            default_value = "all",
            help = "Target tool: claude-code | copilot | codex | all"
        )]
        tool: Tool,
        #[arg(
            long = "local",
            help = "For claude-code: install in .claude/settings.json instead of global"
        )]
        local: bool,
    },
    /// Remove tokenix hooks
    RemoveHook {
        #[arg(long, value_enum, default_value = "all")]
        tool: Tool,
        #[arg(long = "local")]
        local: bool,
    },
    /// Show index statistics
    Stats {
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Start the background embedding daemon (keeps model in memory)
    Serve {
        #[arg(
            long,
            help = "TCP port to listen on (default: 47392 or $TOKENIX_DAEMON_PORT)"
        )]
        port: Option<u16>,
    },
    /// Stop the background embedding daemon
    Stop,
    /// Generate and manage per-command output filters
    Filter {
        #[command(subcommand)]
        action: FilterAction,
    },
    /// Hook handler called by AI tools (not for direct use)
    Hook,
    /// PostToolUse hook handler for output compression (not for direct use)
    HookPost,
    /// Run as a Model Context Protocol (MCP) server over stdin/stdout
    Mcp,
}

#[derive(Subcommand)]
enum FilterAction {
    /// List top Bash commands by tokens wasted (no custom filter yet)
    List,
    /// List active user and bundled output filters
    Active,
    /// Generate a TOML filter for a command using an AI CLI
    Generate {
        /// Base command name (e.g. cargo, git). Omit to select from the list.
        command: Option<String>,
    },
}

fn find_repo_root(start: &Path) -> PathBuf {
    let abs = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    store::find_project_root(&abs)
}

/// Returns the tokenix binary path, normalized for use in config files.
/// On Windows returns forward-slash path so shell scripts work cross-platform.
fn tokenix_bin_path() -> Result<String> {
    let exe = std::env::current_exe()?;
    // Normalize to forward slashes so generated shell and JSON configs work on Windows too.
    Ok(exe.to_string_lossy().replace('\\', "/"))
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Index {
            path,
            force,
            if_stale,
        } => {
            // Cap parallelism for indexing: rayon chunking + ONNX embedding.
            // Without limits, large repos max out all CPU cores and freeze the PC.
            #[allow(unused_unsafe)]
            unsafe {
                if std::env::var("RAYON_NUM_THREADS").is_err() {
                    std::env::set_var("RAYON_NUM_THREADS", "4");
                }
                if std::env::var("OMP_NUM_THREADS").is_err() {
                    std::env::set_var("OMP_NUM_THREADS", "2");
                }
            }
            cmd_index(&path, force, if_stale)
        }
        Commands::Query {
            text,
            budget,
            k,
            file,
            path,
        } => cmd_query(&text, budget, k, file.as_deref(), &path),
        Commands::Read {
            file,
            symbol,
            lines,
            path,
        } => cmd_read(&file, symbol.as_deref(), lines.as_deref(), &path),
        Commands::Gain { path, history } => cmd_gain(&path, history),
        Commands::Benchmark {
            path,
            refresh_index,
            budget,
        } => {
            let repo_root = find_repo_root(&path);
            benchmark::run_benchmark(&repo_root, refresh_index, budget)
        }
        Commands::InstallHook { tool, local } => cmd_install_hook(tool, local),
        Commands::RemoveHook { tool, local } => cmd_remove_hook(tool, local),
        Commands::Stats { path } => cmd_stats(&path),
        Commands::Serve { port } => daemon::run_serve(port),
        Commands::Stop => daemon::run_stop(),
        Commands::Filter { action } => {
            let repo_root = find_repo_root(&PathBuf::from("."));
            match action {
                FilterAction::List => cmd_filter::cmd_filter_list(&repo_root),
                FilterAction::Active => cmd_filter::cmd_filter_active(),
                FilterAction::Generate { command } => {
                    cmd_filter::cmd_filter_generate(command, &repo_root)
                }
            }
        }
        Commands::Hook => {
            // Hook is a short-lived subprocess: limit thread pools before any init.
            // OMP_NUM_THREADS controls ONNX Runtime threads on Windows (MS prebuilt uses OpenMP).
            // RAYON_NUM_THREADS prevents rayon from spawning N_CPU worker threads.
            // SAFETY: single-threaded here, no other threads spawned yet.
            #[allow(unused_unsafe)]
            unsafe {
                std::env::set_var("OMP_NUM_THREADS", "1");
                std::env::set_var("RAYON_NUM_THREADS", "1");
            }
            hook::run_hook()
        }
        Commands::HookPost => {
            #[allow(unused_unsafe)]
            unsafe {
                std::env::set_var("RAYON_NUM_THREADS", "1")
            };
            compress::run_hook_post()
        }
        Commands::Mcp => {
            #[allow(unused_unsafe)]
            unsafe {
                std::env::set_var("OMP_NUM_THREADS", "1");
                std::env::set_var("RAYON_NUM_THREADS", "1");
            }
            mcp::run_mcp_server()
        }
    }
}

fn cmd_index(path: &Path, force: bool, if_stale: bool) -> Result<()> {
    let repo_root = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

    if if_stale && !force {
        let staleness = store::index_staleness(&repo_root, hook::MAX_INDEX_AGE_SECS);
        if !staleness.stale {
            return Ok(());
        }
    }

    println!(
        "{} indexing {}",
        "tokenix".bold(),
        repo_root.display().to_string().cyan()
    );

    let start = std::time::Instant::now();
    let (result, stats) = indexer::index_repo(&repo_root, force, |msg| {
        println!("  {}", msg);
    })?;

    println!(
        "\n{} in {:.1}s",
        "Done".green().bold(),
        start.elapsed().as_secs_f64()
    );
    println!(
        "  Files: {} indexed, {} skipped, {} errors",
        result.indexed, result.skipped, result.errors
    );
    println!(
        "  Index: {} chunks, {} tokens stored",
        stats.chunks,
        format_num(stats.total_tokens)
    );
    Ok(())
}

fn cmd_query(text: &str, budget: usize, k: usize, file: Option<&str>, path: &Path) -> Result<()> {
    let repo_root = find_repo_root(path);

    // Try to query via daemon if it's running
    if let Some(output) = daemon::daemon_search(&repo_root, text, k, budget, file) {
        println!("{}", output);
        return Ok(());
    }

    let results = query::query_index(&repo_root, text, budget, k, file)?
        .ok_or_else(|| anyhow::anyhow!("Index not found. Run: tokenix index"))?;
    println!("{}", query::format_results(&results, text));
    Ok(())
}

fn cmd_read(
    file: &str,
    symbol: Option<&str>,
    lines_range: Option<&str>,
    path: &Path,
) -> Result<()> {
    let repo_root = find_repo_root(path);
    let fp = {
        let p = std::path::Path::new(file);
        if p.exists() {
            p.to_path_buf()
        } else {
            repo_root.join(file)
        }
    };

    if !fp.exists() {
        eprintln!("{} {}", "File not found:".red(), file);
        std::process::exit(1);
    }

    let content = std::fs::read_to_string(&fp)?;
    let file_lines: Vec<&str> = content.lines().collect();

    if let Some(range) = lines_range {
        let parts: Vec<&str> = range.split('-').collect();
        if parts.len() == 2 {
            if let (Ok(s), Ok(e)) = (parts[0].parse::<usize>(), parts[1].parse::<usize>()) {
                let slice = file_lines[s.saturating_sub(1)..e.min(file_lines.len())].join("\n");
                println!("{}", slice);
                return Ok(());
            }
        }
        eprintln!("{}", "Invalid --lines format. Use: N-M".red());
        std::process::exit(1);
    }

    let rel = fp
        .strip_prefix(&repo_root)
        .unwrap_or(&fp)
        .to_string_lossy()
        .replace('\\', "/");

    if let Some(sym) = symbol {
        let chunks = chunker::chunk_file(&rel, &content);
        let found: Vec<_> = chunks
            .iter()
            .filter(|c| c.symbol.to_lowercase().contains(&sym.to_lowercase()))
            .collect();
        if found.is_empty() {
            eprintln!("{} '{}'", "Symbol not found:".yellow(), sym);
            std::process::exit(1);
        }
        for c in found {
            println!(
                "# L{}-{} [{}] {}",
                c.start_line, c.end_line, c.kind, c.symbol
            );
            println!("{}", c.content);
        }
        return Ok(());
    }

    if file_lines.len() >= 200 {
        println!("{}", chunker::generate_outline(&content, &rel));
        println!("\nUse --symbol <name> or --lines N-M to read specific parts.");
    } else {
        println!("{}", content);
    }
    Ok(())
}

fn cmd_gain(path: &Path, history: bool) -> Result<()> {
    let repo_root = find_repo_root(path);
    let stats = gain::compute_gain(&repo_root);

    let project_name = repo_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("project");

    // ── header ────────────────────────────────────────────────────────────────
    let inner = format!(" tokenix gain  ·  {} ", project_name);
    let width = inner.len().max(64);
    let pad = width - inner.len();
    println!("\n{}", format!("╭{}╮", "─".repeat(width)).bright_black());
    println!(
        "{}{}{}{}",
        "│".bright_black(),
        inner.bold(),
        " ".repeat(pad),
        "│".bright_black()
    );
    println!("{}", format!("╰{}╯", "─".repeat(width)).bright_black());

    // ── token summary + hook calls (side by side) ─────────────────────────────
    println!();
    let bar = reduction_bar(stats.pct_saved, 18);
    let intercept_pct = if stats.total_calls > 0 {
        (stats.intercepted as f64 / stats.total_calls as f64) * 100.0
    } else {
        0.0
    };

    println!(
        "  {}                              {}",
        "TOKEN SUMMARY".bold().underline(),
        "HOOK CALLS".bold().underline()
    );
    println!(
        "  {:<26} {:>14}    {:<22} {:>8}",
        "Original (would-be)",
        format_num(stats.tokens_original).yellow(),
        "Total",
        format_num(stats.total_calls as i64)
    );
    println!(
        "  {:<26} {:>14}    {:<22} {:>8}",
        "After optimization",
        format_num(stats.tokens_used).cyan(),
        "Intercepted",
        format!(
            "{}  ({:.0}%)",
            format_num(stats.intercepted as i64).green(),
            intercept_pct
        )
    );
    println!(
        "  {:<26} {:>14}    {:<22} {:>8}",
        "Saved",
        format_num(stats.tokens_saved).green().bold(),
        "Passed through",
        format_num(stats.passed as i64).dimmed()
    );
    println!(
        "  {:<26} {:>14}",
        "Reduction",
        format!("{:.1}%  {}", stats.pct_saved, bar).green().bold()
    );

    // ── cost table ────────────────────────────────────────────────────────────
    println!();
    println!(
        "  {}",
        "COST ESTIMATE  (input tokens · USD)".bold().underline()
    );
    println!(
        "  {}",
        format!(
            "  Prices per 1M input tokens from public provider pricing pages. Collected: {}.",
            gain::PRICING_COLLECTED_AT
        )
        .dimmed()
    );
    println!();

    let col_model = 27usize;
    let col_price = 9usize;
    let col_val = 12usize;

    let sep = format!(
        "    {}  {}  {}  {}  {}",
        "─".repeat(col_model),
        "─".repeat(col_price),
        "─".repeat(col_val),
        "─".repeat(col_val),
        "─".repeat(col_val)
    );
    // table header
    println!(
        "  {}",
        format!(
            "    {:<col_model$}  {:>col_price$}  {:>col_val$}  {:>col_val$}  {:>col_val$}",
            "Model",
            "$/1M in",
            "Without",
            "With",
            "Saved",
            col_model = col_model,
            col_price = col_price,
            col_val = col_val
        )
        .bold()
        .bright_black()
    );
    println!("  {}", sep.bright_black());

    for row in &stats.cost_rows {
        let marker = if row.reference { " ★" } else { "  " };
        let name = format!("{}{}", row.model, marker);
        let price_str = {
            let m = gain::MODELS.iter().find(|m| m.name == row.model).unwrap();
            format!("${:.2}", m.input_per_1m)
        };
        let without = format!("${:.4}", row.without_usd);
        let with_ = format!("${:.4}", row.with_usd);
        let saved = format!("${:.4}", row.saved_usd);

        let line = format!(
            "    {:<col_model$}  {:>col_price$}  {:>col_val$}  {:>col_val$}  {:>col_val$}",
            name,
            price_str,
            without,
            with_,
            saved,
            col_model = col_model,
            col_price = col_price,
            col_val = col_val
        );
        if row.reference {
            println!("  {}", line.bold());
        } else {
            println!("  {}", line);
        }
    }

    println!("  {}", sep.bright_black());
    println!(
        "  {}",
        format!(
            "    ★ reference model · prices collected {}",
            gain::PRICING_COLLECTED_AT
        )
        .dimmed()
    );

    // ── by tool / by phase ────────────────────────────────────────────────────
    if !stats.by_tool.is_empty() {
        println!();
        println!("  {}", "BY TOOL".bold().underline());
        for (tool, count, saved) in &stats.by_tool {
            let bar = mini_bar(*saved, stats.tokens_saved, 20);
            println!(
                "  {:<14} {:>5} calls   {} {}",
                tool.bold(),
                count,
                format_num(*saved).green(),
                bar.bright_black()
            );
        }
    }

    if stats.by_phase.len() > 1 {
        println!();
        println!("  {}", "BY PHASE".bold().underline());
        for (phase, count, saved) in &stats.by_phase {
            let (label, detail) = match phase.as_str() {
                "pre" => ("PreToolUse ", "Read / Grep intercepts"),
                "post" => ("PostToolUse", "Bash / ListDirectory compression"),
                other => (other, ""),
            };
            println!(
                "  {}  {:>5} calls   {}  {}",
                label.bold(),
                count,
                format_num(*saved).green(),
                detail.dimmed()
            );
        }
    }

    // ── history ───────────────────────────────────────────────────────────────
    if history {
        let events = store::read_hook_log(&repo_root);
        let show = events.len().min(20);
        println!();
        println!("  {}", format!("LAST {} EVENTS", show).bold().underline());
        for e in events.iter().rev().take(show) {
            let ts = format_ts(e.ts);
            let action = if e.action == "intercepted" {
                format!("{:<11}", "intercepted").green().to_string()
            } else {
                format!("{:<11}", "pass").dimmed().to_string()
            };
            let phase = match e.phase.as_str() {
                "pre" => "pre ".dimmed().to_string(),
                "post" => "post".dimmed().to_string(),
                other => other.dimmed().to_string(),
            };
            println!(
                "  {} {} {:<8} {}  saved {}",
                ts.bright_black(),
                phase,
                e.tool.bold(),
                action,
                format_num(e.saved_tokens).green()
            );
        }
    }

    println!();
    Ok(())
}

fn reduction_bar(pct: f64, width: usize) -> String {
    let filled = ((pct / 100.0) * width as f64).round() as usize;
    let filled = filled.min(width);
    let empty = width - filled;
    format!("[{}{}]", "█".repeat(filled), "░".repeat(empty))
}

fn mini_bar(value: i64, total: i64, width: usize) -> String {
    if total == 0 {
        return "─".repeat(width);
    }
    let filled = ((value as f64 / total as f64) * width as f64).round() as usize;
    let filled = filled.min(width);
    format!("{}{}", "▓".repeat(filled), "░".repeat(width - filled))
}

// install-hook

fn cmd_install_hook(tool: Tool, local: bool) -> Result<()> {
    match tool {
        Tool::ClaudeCode => install_claude_code(local)?,
        Tool::Copilot => install_copilot()?,
        Tool::Codex => install_codex()?,
        Tool::Mcp => install_mcp_server()?,
        Tool::All => {
            install_claude_code(local)?;
            install_copilot()?;
            install_codex()?;
            install_mcp_server()?;
        }
    }
    Ok(())
}

fn install_claude_code(local: bool) -> Result<()> {
    let settings_path = claude_settings_path(local)?;

    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let tokenix_bin = tokenix_bin_path()?;

    let mut settings: serde_json::Value = if settings_path.exists() {
        let raw = std::fs::read_to_string(&settings_path)?;
        serde_json::from_str(&raw).unwrap_or(serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    let removed_legacy_auto_index = remove_legacy_claude_auto_index_hook(&mut settings);

    let pre_already = settings["hooks"]["PreToolUse"]
        .as_array()
        .map(|arr| arr.iter().any(|h| h.to_string().contains("tokenix")))
        .unwrap_or(false);

    let post_already = settings["hooks"]["PostToolUse"]
        .as_array()
        .map(|arr| arr.iter().any(|h| h.to_string().contains("tokenix")))
        .unwrap_or(false);

    if pre_already && post_already && !removed_legacy_auto_index {
        println!("{} Claude Code hooks already installed.", "~".yellow());
        return Ok(());
    }

    if !pre_already {
        let hook = serde_json::json!({
            "matcher": "Read|Grep",
            "hooks": [{"type": "command", "command": format!("{} hook", tokenix_bin)}]
        });
        if settings["hooks"]["PreToolUse"].is_array() {
            settings["hooks"]["PreToolUse"]
                .as_array_mut()
                .unwrap()
                .push(hook);
        } else {
            settings["hooks"]["PreToolUse"] = serde_json::json!([hook]);
        }
    }

    if !post_already {
        let hook = serde_json::json!({
            "matcher": "Bash|ListDirectory",
            "hooks": [{"type": "command", "command": format!("{} hook-post", tokenix_bin)}]
        });
        if settings["hooks"]["PostToolUse"].is_array() {
            settings["hooks"]["PostToolUse"]
                .as_array_mut()
                .unwrap()
                .push(hook);
        } else {
            settings["hooks"]["PostToolUse"] = serde_json::json!([hook]);
        }
    }

    std::fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;
    println!(
        "{} Claude Code  ->  {}",
        "ok".green(),
        settings_path.display()
    );
    println!("  PreToolUse:  {} hook", tokenix_bin);
    println!("  PostToolUse: {} hook-post", tokenix_bin);
    if removed_legacy_auto_index {
        println!("  Removed legacy UserPromptSubmit auto-index hook");
    }
    Ok(())
}

fn remove_legacy_claude_auto_index_hook(settings: &mut serde_json::Value) -> bool {
    let Some(entries) = settings["hooks"]["UserPromptSubmit"].as_array_mut() else {
        return false;
    };

    let mut changed = false;
    for entry in entries.iter_mut() {
        let Some(hooks) = entry["hooks"].as_array_mut() else {
            continue;
        };
        let before = hooks.len();
        hooks.retain(|hook| {
            let text = hook.to_string();
            !(text.contains("tokenix") && text.contains("--if-stale") && text.contains("index"))
        });
        changed |= hooks.len() != before;
    }

    let before = entries.len();
    entries.retain(|entry| {
        entry["hooks"]
            .as_array()
            .map(|hooks| !hooks.is_empty())
            .unwrap_or(true)
    });
    changed |= entries.len() != before;
    changed
}

fn install_copilot() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let github_dir = cwd.join(".github");
    std::fs::create_dir_all(&github_dir)?;

    // 1. copilot-instructions.md - repository custom instructions
    let instructions_path = github_dir.join("copilot-instructions.md");
    let tokenix_bin = tokenix_bin_path()?;
    let instructions = format!(
        r#"# tokenix - Semantic Context Tool

This repository is indexed by **tokenix** for token-efficient code understanding.

## Required workflow before reading files

Use tokenix first whenever you need code context:

```bash
tokenix query "what you need to understand"
tokenix read <file>
tokenix read <file> --symbol <name>
tokenix read <file> --lines N-M
```

Only read a full file directly after tokenix shows that the file is small, or after a targeted `--symbol` / `--lines` read is not enough.

## High-signal examples

```bash
tokenix query "how does authentication work"
tokenix query "where is JWT validated" --budget 2000
tokenix read src/auth/middleware.rs --symbol validate_token
```

Use `tokenix gain --history` to inspect estimated savings from hook events.

tokenix binary: `{tokenix_bin}`
Index location: `~/.tokenix/<project-id>.db` (global, one DB per project)

"#
    );

    let already_instructions = instructions_path.exists();
    std::fs::write(&instructions_path, &instructions)?;
    if already_instructions {
        println!(
            "{} Copilot instructions updated  ->  {}",
            "ok".green(),
            instructions_path.display()
        );
    } else {
        println!(
            "{} Copilot instructions  ->  {}",
            "ok".green(),
            instructions_path.display()
        );
    }

    // 2. hooks/hooks.json - preToolUse hook for Copilot agent/workspace mode
    let hooks_dir = github_dir.join("hooks");
    std::fs::create_dir_all(&hooks_dir)?;
    let hooks_path = hooks_dir.join("hooks.json");

    // Use platform-specific command fields; Copilot passes tool context via env vars
    let hook_bash = format!("{} hook", tokenix_bin);
    let hook_ps = format!("{} hook", tokenix_bin);

    let hooks_json = serde_json::json!({
        "version": 1,
        "hooks": {
            "preToolUse": [{
                "type": "command",
                "bash": hook_bash,
                "powershell": hook_ps,
                "timeoutSec": 10
            }]
        }
    });

    let already_hooks = hooks_path.exists();
    std::fs::write(&hooks_path, serde_json::to_string_pretty(&hooks_json)?)?;
    if already_hooks {
        println!(
            "{} Copilot hooks updated         ->  {}",
            "ok".green(),
            hooks_path.display()
        );
    } else {
        println!(
            "{} Copilot hooks                 ->  {}",
            "ok".green(),
            hooks_path.display()
        );
    }

    println!("  Note: commit .github/ to enable for all contributors.");
    Ok(())
}

fn install_codex() -> Result<()> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot find home directory"))?;
    let codex_dir = home.join(".codex");
    std::fs::create_dir_all(&codex_dir)?;

    let tokenix_bin = tokenix_bin_path()?;

    // ~/.codex/instructions.md - compatibility instructions for older setups.
    let instructions_path = codex_dir.join("instructions.md");
    let existing = if instructions_path.exists() {
        std::fs::read_to_string(&instructions_path).unwrap_or_default()
    } else {
        String::new()
    };

    let marker = "<!-- tokenix -->";
    let block = format!(
        r#"{marker}
## tokenix - Semantic Context Tool

This repo is indexed by tokenix. Use tokenix before reading full files:

```bash
tokenix query "natural language query"
tokenix read <file>
tokenix read <file> --symbol <name>
tokenix read <file> --lines N-M
```

Only read full files directly after tokenix shows that the file is small, or after a targeted symbol/line read is insufficient.

tokenix binary: `{tokenix_bin}`
{marker}
"#
    );

    let new_content = if existing.contains(marker) {
        // Replace existing tokenix block
        let start = existing.find(marker).unwrap();
        let end = existing[start + marker.len()..]
            .find(marker)
            .map(|i| start + marker.len() + i + marker.len())
            .unwrap_or(existing.len());
        format!("{}{}{}", &existing[..start], block, &existing[end..])
    } else {
        format!("{}\n{}", existing.trim_end(), block)
    };

    std::fs::write(&instructions_path, new_content)?;
    println!(
        "{} Codex instructions  ->  {}",
        "ok".green(),
        instructions_path.display()
    );

    // Shell wrappers: ~/.codex/tokenix-init.sh (bash/zsh) and tokenix-init.ps1 (PowerShell)
    let sh_path = codex_dir.join("tokenix-init.sh");
    let sh_content = format!(
        r#"#!/usr/bin/env sh
# tokenix shell helpers - source this in your shell profile
# Add to ~/.bashrc or ~/.zshrc: source ~/.codex/tokenix-init.sh

# tx-read: smart file reader (outline for large files, full content for small)
tx-read() {{
    "{tokenix_bin}" read "$@"
}}

# tx-query: semantic search
tx-query() {{
    "{tokenix_bin}" query "$@"
}}
"#
    );
    std::fs::write(&sh_path, &sh_content)?;
    println!(
        "{} Codex shell helpers ->  {}",
        "ok".green(),
        sh_path.display()
    );

    let ps1_path = codex_dir.join("tokenix-init.ps1");
    let ps1_content = format!(
        r#"# tokenix shell helpers for PowerShell
# Add to your $PROFILE: . ~/.codex/tokenix-init.ps1

function tx-read {{ & "{tokenix_bin}" read @args }}
function tx-query {{ & "{tokenix_bin}" query @args }}
"#
    );
    std::fs::write(&ps1_path, &ps1_content)?;
    println!(
        "{} Codex PS1 helpers   ->  {}",
        "ok".green(),
        ps1_path.display()
    );

    let hook_ps1_path = codex_dir.join("tokenix-codex-hook.ps1");
    std::fs::write(&hook_ps1_path, codex_hook_ps1(&tokenix_bin))?;
    println!(
        "{} Codex hook wrapper ->  {}",
        "ok".green(),
        hook_ps1_path.display()
    );

    let hooks_path = codex_dir.join("hooks.json");
    install_codex_hooks_json(&hooks_path, &hook_ps1_path)?;
    println!(
        "{} Codex hooks        ->  {}",
        "ok".green(),
        hooks_path.display()
    );

    println!("  To activate shell helpers:");
    println!("    bash/zsh:   echo 'source ~/.codex/tokenix-init.sh' >> ~/.bashrc");
    println!("    PowerShell: echo '. ~/.codex/tokenix-init.ps1' >> $PROFILE");
    Ok(())
}

fn codex_hook_ps1(tokenix_bin: &str) -> String {
    format!(
        r#"param(
  [Parameter(Mandatory = $true)]
  [ValidateSet("pre", "post")]
  [string]$Phase
)

$ErrorActionPreference = "SilentlyContinue"
$inputJson = [Console]::In.ReadToEnd()
$tokenix = "{tokenix_bin}"
$subcommand = if ($Phase -eq "post") {{ "hook-post" }} else {{ "hook" }}

$psi = [System.Diagnostics.ProcessStartInfo]::new()
$psi.FileName = $tokenix
$psi.Arguments = $subcommand
$psi.UseShellExecute = $false
$psi.RedirectStandardInput = $true
$psi.RedirectStandardOutput = $true
$psi.RedirectStandardError = $true

$proc = [System.Diagnostics.Process]::Start($psi)
$proc.StandardInput.Write($inputJson)
$proc.StandardInput.Close()

$stdoutTask = $proc.StandardOutput.ReadToEndAsync()
$stderrTask = $proc.StandardError.ReadToEndAsync()

if (-not $proc.WaitForExit(10000)) {{
  try {{ $proc.Kill() }} catch {{}}
  exit 0
}}

$stdout = $stdoutTask.GetAwaiter().GetResult()
$stderr = $stderrTask.GetAwaiter().GetResult()

if ($proc.ExitCode -eq 2) {{
  if ($Phase -eq "post") {{
    exit 0
  }}
  if (-not [string]::IsNullOrWhiteSpace($stderr)) {{
    [Console]::Error.Write($stderr)
  }} elseif (-not [string]::IsNullOrWhiteSpace($stdout)) {{
    [Console]::Error.Write($stdout)
  }}
  exit 2
}}

exit 0
"#
    )
}

fn install_codex_hooks_json(hooks_path: &Path, hook_ps1_path: &Path) -> Result<()> {
    let mut hooks: serde_json::Value = if hooks_path.exists() {
        let raw = std::fs::read_to_string(hooks_path)?;
        serde_json::from_str(&raw).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    if !hooks["hooks"].is_object() {
        hooks["hooks"] = serde_json::json!({});
    }

    let command = format!(
        "powershell -NoProfile -ExecutionPolicy Bypass -File \"{}\"",
        hook_ps1_path.to_string_lossy().replace('\\', "/")
    );

    upsert_codex_hook(
        &mut hooks["hooks"]["PreToolUse"],
        serde_json::json!({
            "matcher": "^(Read|Grep)$",
            "hooks": [{
                "type": "command",
                "command": format!("{command} pre"),
                "timeout": 10
            }]
        }),
    );
    upsert_codex_hook(
        &mut hooks["hooks"]["PostToolUse"],
        serde_json::json!({
            "matcher": "^(Bash|ListDirectory)$",
            "hooks": [{
                "type": "command",
                "command": format!("{command} post"),
                "timeout": 10
            }]
        }),
    );

    std::fs::write(hooks_path, serde_json::to_string_pretty(&hooks)?)?;
    Ok(())
}

fn upsert_codex_hook(slot: &mut serde_json::Value, hook: serde_json::Value) {
    if !slot.is_array() {
        *slot = serde_json::json!([]);
    }
    let arr = slot.as_array_mut().unwrap();
    arr.retain(|entry| !entry.to_string().contains("tokenix-codex-hook.ps1"));
    arr.push(hook);
}

// remove-hook

fn cmd_remove_hook(tool: Tool, local: bool) -> Result<()> {
    match tool {
        Tool::ClaudeCode => remove_claude_code(local)?,
        Tool::Copilot => remove_copilot()?,
        Tool::Codex => remove_codex()?,
        Tool::Mcp => remove_mcp_server()?,
        Tool::All => {
            remove_claude_code(local)?;
            remove_copilot()?;
            remove_codex()?;
            remove_mcp_server()?;
        }
    }
    Ok(())
}

fn remove_claude_code(local: bool) -> Result<()> {
    let settings_path = claude_settings_path(local)?;
    if !settings_path.exists() {
        println!("{} Claude Code settings not found.", "~".yellow());
        return Ok(());
    }
    let raw = std::fs::read_to_string(&settings_path)?;
    let mut settings: serde_json::Value = serde_json::from_str(&raw)?;
    if let Some(arr) = settings["hooks"]["PreToolUse"].as_array_mut() {
        arr.retain(|h| !h.to_string().contains("tokenix"));
    }
    if let Some(arr) = settings["hooks"]["PostToolUse"].as_array_mut() {
        arr.retain(|h| !h.to_string().contains("tokenix"));
    }
    std::fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;
    println!(
        "{} Claude Code hooks removed from {}",
        "ok".green(),
        settings_path.display()
    );
    Ok(())
}

fn remove_copilot() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let instructions = cwd.join(".github/copilot-instructions.md");
    let hooks = cwd.join(".github/hooks/hooks.json");
    for path in [&instructions, &hooks] {
        if path.exists() {
            std::fs::remove_file(path)?;
            println!("{} Removed {}", "ok".green(), path.display());
        }
    }
    Ok(())
}

fn remove_codex() -> Result<()> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot find home directory"))?;
    let marker = "<!-- tokenix -->";
    let instructions = home.join(".codex/instructions.md");
    if instructions.exists() {
        let content = std::fs::read_to_string(&instructions)?;
        if let Some(start) = content.find(marker) {
            let end = content[start + marker.len()..]
                .find(marker)
                .map(|i| start + marker.len() + i + marker.len())
                .unwrap_or(content.len());
            let new = format!("{}{}", &content[..start], &content[end..]);
            std::fs::write(&instructions, new)?;
            println!("{} Codex instructions cleaned", "ok".green());
        }
    }
    for helper in [
        "tokenix-init.sh",
        "tokenix-init.ps1",
        "tokenix-codex-hook.ps1",
    ] {
        let p = home.join(".codex").join(helper);
        if p.exists() {
            std::fs::remove_file(&p)?;
            println!("{} Removed {}", "ok".green(), p.display());
        }
    }
    let hooks = home.join(".codex/hooks.json");
    if hooks.exists() {
        let raw = std::fs::read_to_string(&hooks)?;
        let mut json: serde_json::Value = serde_json::from_str(&raw)?;
        remove_codex_hooks_json(&mut json);
        std::fs::write(&hooks, serde_json::to_string_pretty(&json)?)?;
        println!("{} Codex hooks cleaned", "ok".green());
    }
    Ok(())
}

fn remove_codex_hooks_json(json: &mut serde_json::Value) {
    for phase in ["PreToolUse", "PostToolUse"] {
        let Some(arr) = json["hooks"][phase].as_array_mut() else {
            continue;
        };
        arr.retain(|entry| !entry.to_string().contains("tokenix-codex-hook.ps1"));
    }
}

fn mcp_config_path() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot find home directory"))?;
    Ok(home.join(".gemini").join("antigravity-cli").join("mcp_config.json"))
}

fn install_mcp_server() -> Result<()> {
    let config_path = mcp_config_path()?;
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let tokenix_bin = tokenix_bin_path()?;

    let mut config: serde_json::Value = if config_path.exists() {
        let raw = std::fs::read_to_string(&config_path)?;
        serde_json::from_str(&raw).unwrap_or(serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    if !config["mcpServers"].is_object() {
        config["mcpServers"] = serde_json::json!({});
    }

    config["mcpServers"]["tokenix"] = serde_json::json!({
        "command": tokenix_bin,
        "args": ["mcp"]
    });

    std::fs::write(&config_path, serde_json::to_string_pretty(&config)?)?;
    println!(
        "{} Antigravity CLI MCP server registered at {}",
        "ok".green(),
        config_path.display()
    );
    Ok(())
}

fn remove_mcp_server() -> Result<()> {
    let config_path = mcp_config_path()?;
    if !config_path.exists() {
        return Ok(());
    }

    let raw = std::fs::read_to_string(&config_path)?;
    let mut config: serde_json::Value = serde_json::from_str(&raw)?;

    if let Some(servers) = config["mcpServers"].as_object_mut() {
        if servers.remove("tokenix").is_some() {
            std::fs::write(&config_path, serde_json::to_string_pretty(&config)?)?;
            println!(
                "{} Antigravity CLI MCP server unregistered from {}",
                "ok".green(),
                config_path.display()
            );
        }
    }
    Ok(())
}

// helpers

fn claude_settings_path(local: bool) -> Result<PathBuf> {
    if local {
        Ok(std::env::current_dir()?
            .join(".claude")
            .join("settings.json"))
    } else {
        Ok(dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Cannot find home directory"))?
            .join(".claude")
            .join("settings.json"))
    }
}

fn cmd_stats(path: &Path) -> Result<()> {
    let repo_root = find_repo_root(path);
    let conn = store::open_db(&repo_root, false)?
        .ok_or_else(|| anyhow::anyhow!("No index found. Run: tokenix index"))?;
    let stats = store::count_stats(&conn)?;
    let age = store::get_index_age(&repo_root);
    let age_str = age
        .map(|a| format!("{}s ago", a as i64))
        .unwrap_or_else(|| "unknown".to_string());

    let db = store::db_path(&repo_root);
    let id = store::project_id(&repo_root);
    println!("\n{} {}", "Project:".bold(), repo_root.display());
    println!("  ID:     {}", id);
    println!("  Index:  {}", db.display());
    println!("  Files:  {}", stats.files);
    println!("  Chunks: {}", stats.chunks);
    println!("  Tokens: {}", format_num(stats.total_tokens));
    println!("  Age:    {}", age_str);
    Ok(())
}

fn format_num(n: i64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result.chars().rev().collect()
}

fn format_ts(ts: f64) -> String {
    let secs = ts as u64;
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{:02}:{:02}:{:02}", h, m, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_wrapper_fails_post_open_but_preserves_pre_intercepts() {
        let ps1 = codex_hook_ps1("C:/tokenix/tokenix.exe");

        assert!(ps1.contains("if ($Phase -eq \"post\")"));
        assert!(ps1.contains("exit 0"));
        assert!(ps1.contains("exit 2"));
        assert!(ps1.contains("ReadToEndAsync()"));
    }

    #[test]
    fn codex_hook_json_preserves_unrelated_hooks() {
        let mut json = serde_json::json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "Other",
                    "hooks": [{"type": "command", "command": "other pre"}]
                }]
            }
        });

        upsert_codex_hook(
            &mut json["hooks"]["PreToolUse"],
            serde_json::json!({
                "matcher": "^(Read|Grep)$",
                "hooks": [{"type": "command", "command": "powershell tokenix-codex-hook.ps1 pre"}]
            }),
        );

        let arr = json["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert!(arr
            .iter()
            .any(|entry| entry.to_string().contains("other pre")));
        assert!(arr
            .iter()
            .any(|entry| entry.to_string().contains("tokenix-codex-hook.ps1")));
    }

    #[test]
    fn remove_codex_hook_json_removes_only_tokenix_entries() {
        let mut json = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {"hooks": [{"command": "other pre"}]},
                    {"hooks": [{"command": "powershell tokenix-codex-hook.ps1 pre"}]}
                ],
                "PostToolUse": [
                    {"hooks": [{"command": "other post"}]},
                    {"hooks": [{"command": "powershell tokenix-codex-hook.ps1 post"}]}
                ]
            }
        });

        remove_codex_hooks_json(&mut json);

        assert_eq!(json["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
        assert_eq!(json["hooks"]["PostToolUse"].as_array().unwrap().len(), 1);
        assert!(json.to_string().contains("other pre"));
        assert!(json.to_string().contains("other post"));
        assert!(!json.to_string().contains("tokenix-codex-hook.ps1"));
    }
}
