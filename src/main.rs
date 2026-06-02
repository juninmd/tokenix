mod benchmark;
mod chunker;
mod cmd_filter;
mod compress;
mod daemon;
mod doctor;
mod embed;
mod filters;
mod gain;
mod graph;
mod hook;
mod indexer;
mod mcp;
mod memory;
mod query;
mod recordings;
mod store;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use colored::Colorize;
use std::path::{Path, PathBuf};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
#[command(name = "tokenix", version = VERSION, about = "Local semantic index for LLM token optimization")]
struct Cli {
    /// Force CPU-only embedding, skipping the GPU even on a GPU-enabled build.
    /// GPU (DirectML/CUDA) is used by default when compiled with that support.
    #[arg(long, global = true)]
    only_cpu: bool,

    #[command(subcommand)]
    command: Commands,
}

/// CPU usage profile for indexing. Embedding batch size (which drives peak
/// memory) stays bounded across all profiles; profiles mainly scale thread use.
#[derive(Clone, Copy, ValueEnum, Debug, Default)]
enum CpuProfile {
    /// 1 worker, 1 ONNX thread, tiny batches — minimal footprint.
    Low,
    /// Use available cores with a bounded ONNX thread count (safe default).
    #[default]
    Default,
    /// Use cores aggressively (higher ONNX thread cap) for strong machines.
    Max,
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
    #[value(name = "gemini")]
    Gemini,
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
        #[arg(
            long,
            value_enum,
            default_value = "default",
            help = "CPU usage profile: low | default | max"
        )]
        cpu_profile: CpuProfile,
        #[arg(
            long,
            help = "Max rayon worker threads for chunking/search during indexing"
        )]
        jobs: Option<usize>,
        #[arg(long, help = "Embedding batch size for indexing")]
        embed_batch: Option<usize>,
        #[arg(long, help = "Update file chunks and symbol graph without embedding")]
        no_embed: bool,
    },
    /// Semantic search over the indexed repository
    Query {
        text: String,
        #[arg(short, long, default_value_t = 1200)]
        budget: usize,
        #[arg(long, default_value_t = 20)]
        k: usize,
        #[arg(short, long, help = "Filter to specific file path")]
        file: Option<String>,
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Exact regex/literal search over indexed content (no embedding)
    Grep {
        pattern: String,
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
        #[arg(short = 'i', long, help = "Case-insensitive match")]
        ignore_case: bool,
        #[arg(short, long, help = "Filter to specific file path")]
        file: Option<String>,
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Build focused task context in one call
    Context {
        task: String,
        #[arg(short, long, default_value_t = 1200)]
        budget: usize,
        #[arg(long, default_value_t = 4)]
        max_files: usize,
        #[arg(long, help = "Print per-section token breakdown to stderr")]
        budget_breakdown: bool,
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Explore related symbols and source in one graph-aware call
    Explore {
        query: String,
        #[arg(short, long, default_value_t = 1200)]
        budget: usize,
        #[arg(long, default_value_t = 8)]
        max_symbols: usize,
        #[arg(long, help = "Print per-section token breakdown to stderr")]
        budget_breakdown: bool,
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Store or list user preference memory
    Memory {
        #[command(subcommand)]
        action: MemoryAction,
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
    /// Find indexed symbols by name or path
    Symbols {
        query: String,
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Show symbols that call a target symbol
    Callers {
        symbol: String,
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Show symbols called by a target symbol
    Callees {
        symbol: String,
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Show a bidirectional impact graph around a symbol
    Impact {
        symbol: String,
        #[arg(short, long, default_value_t = 2)]
        depth: usize,
        #[arg(short, long, default_value_t = 50)]
        limit: usize,
        #[arg(long, help = "Output format: text | html", default_value = "text")]
        format: String,
        #[arg(
            short,
            long,
            help = "Output file path for html format",
            default_value = "impact.html"
        )]
        output: String,
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Rebuild only the symbol graph from existing indexed chunks
    RebuildGraph {
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Show token savings analytics
    Gain {
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
        #[arg(long, help = "Show per-call history")]
        history: bool,
        #[arg(long, help = "Show the per-model cost-estimate table")]
        cost_estimate: bool,
    },
    /// Run a reproducible token-savings and retrieval-quality benchmark
    Benchmark {
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
        #[arg(long, help = "Refresh index metadata before measuring")]
        refresh_index: bool,
        #[arg(
            long,
            default_value_t = 1200,
            help = "Token budget for semantic queries"
        )]
        budget: usize,
        #[arg(
            long,
            help = "Path to a local CodeGraph checkout for a lightweight comparison"
        )]
        compare_codegraph: Option<PathBuf>,
        #[arg(long, help = "TOML file with project-specific benchmark cases")]
        cases: Option<PathBuf>,
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
            help = "For claude-code: install in .claude/settings.local.json instead of global"
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
    /// Show a directory tree map with token counts per file/folder
    Tokenmap {
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
        #[arg(long, help = "Output format: text | html", default_value = "text")]
        format: String,
        #[arg(
            short,
            long,
            help = "Output file path for html format",
            default_value = "tokenmap.html"
        )]
        output: String,
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
    /// Diagnose embedding backend, GPU availability, model cache, and daemon
    Doctor,
    /// Generate and manage per-command output filters
    Filter {
        #[command(subcommand)]
        action: FilterAction,
    },
    /// Run a command and compress its output using tokenix filters (used by PreToolUse rewrite)
    Run {
        /// Command to execute
        command: String,
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Hook handler called by AI tools (not for direct use)
    Hook,
    /// PostToolUse hook handler for output compression (not for direct use)
    HookPost,
    /// Run as a Model Context Protocol (MCP) server over stdin/stdout
    Mcp,
}

#[derive(Subcommand)]
enum MemoryAction {
    /// Add a preference to global or project memory
    Add {
        text: String,
        #[arg(long, help = "Save under ~/.tokenix/memory/preferences.md")]
        global: bool,
        #[arg(long, help = "Save under this project's preference memory")]
        project: bool,
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// List saved preferences
    List {
        #[arg(long, help = "Only show global preferences")]
        global: bool,
        #[arg(long, help = "Only show project preferences")]
        project: bool,
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Remove saved preferences matching text
    Remove {
        query: String,
        #[arg(long, help = "Remove from global preferences")]
        global: bool,
        #[arg(long, help = "Remove from project preferences")]
        project: bool,
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Replace saved preferences matching text
    Edit {
        query: String,
        replacement: String,
        #[arg(long, help = "Edit global preferences")]
        global: bool,
        #[arg(long, help = "Edit project preferences")]
        project: bool,
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
}

#[derive(Subcommand)]
enum FilterAction {
    /// List top Bash commands by tokens wasted (no custom filter yet)
    List {
        /// Optional index from the list to see detailed command invocations
        index: Option<usize>,
    },
    /// List active user and bundled output filters
    Active,
    /// Generate a TOML filter for a command using an AI CLI
    Generate {
        /// Base command name (e.g. cargo, git). Omit to select from the list.
        command: Option<String>,
    },
    /// Record real command output into .tokenix/recordings for richer generation
    Record {
        #[command(subcommand)]
        action: RecordAction,
    },
}

#[derive(Subcommand)]
enum RecordAction {
    /// Start capturing command output (optionally limited to one command)
    Start {
        /// Base command to capture (e.g. cargo). Omit to capture everything.
        command: Option<String>,
    },
    /// Stop capturing and show a summary of what was recorded
    Stop,
    /// Show whether a session is active and what has been captured
    Status,
}

fn find_repo_root(start: &Path) -> PathBuf {
    store::find_project_root(start)
}

/// Returns the tokenix binary path, normalized for use in config files.
/// On Windows returns forward-slash path so shell scripts work cross-platform.
fn tokenix_bin_path() -> Result<String> {
    let exe = std::env::current_exe()?;
    // Normalize to forward slashes so generated shell and JSON configs work on Windows too.
    Ok(exe.to_string_lossy().replace('\\', "/"))
}

fn hook_command(tokenix_bin: &str, subcommand: &str) -> String {
    format!("\"{}\" {}", tokenix_bin, subcommand)
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Must be set before the embedding model is first initialized.
    let only_cpu = cli.only_cpu;
    crate::embed::set_force_cpu(only_cpu);

    let res = match cli.command {
        Commands::Index {
            path,
            force,
            if_stale,
            cpu_profile,
            jobs,
            embed_batch,
            no_embed,
        } => {
            configure_index_limits(cpu_profile, only_cpu, jobs, embed_batch);
            cmd_index(&path, force, if_stale, no_embed)
        }
        Commands::Query {
            text,
            budget,
            k,
            file,
            path,
        } => cmd_query(&text, budget, k, file.as_deref(), &path),
        Commands::Grep {
            pattern,
            limit,
            ignore_case,
            file,
            path,
        } => cmd_grep(&pattern, limit, ignore_case, file.as_deref(), &path),
        Commands::Context {
            task,
            budget,
            max_files,
            budget_breakdown,
            path,
        } => cmd_context(&task, budget, max_files, budget_breakdown, &path),
        Commands::Explore {
            query,
            budget,
            max_symbols,
            budget_breakdown,
            path,
        } => cmd_explore(&query, budget, max_symbols, budget_breakdown, &path),
        Commands::Memory { action } => cmd_memory(action),
        Commands::Read {
            file,
            symbol,
            lines,
            path,
        } => cmd_read(&file, symbol.as_deref(), lines.as_deref(), &path),
        Commands::Symbols { query, limit, path } => cmd_symbols(&query, limit, &path),
        Commands::Callers {
            symbol,
            limit,
            path,
        } => cmd_graph_relations(&symbol, limit, &path, true),
        Commands::Callees {
            symbol,
            limit,
            path,
        } => cmd_graph_relations(&symbol, limit, &path, false),
        Commands::Impact {
            symbol,
            depth,
            limit,
            format,
            output,
            path,
        } => cmd_impact(&symbol, depth, limit, &format, &output, &path),
        Commands::RebuildGraph { path } => cmd_rebuild_graph(&path),
        Commands::Gain {
            path,
            history,
            cost_estimate,
        } => cmd_gain(&path, history, cost_estimate),
        Commands::Benchmark {
            path,
            refresh_index,
            budget,
            compare_codegraph,
            cases,
        } => {
            let repo_root = find_repo_root(&path);
            benchmark::run_benchmark(
                &repo_root,
                refresh_index,
                budget,
                compare_codegraph.as_deref(),
                cases.as_deref(),
            )
        }
        Commands::InstallHook { tool, local } => cmd_install_hook(tool, local),
        Commands::RemoveHook { tool, local } => cmd_remove_hook(tool, local),
        Commands::Stats { path } => cmd_stats(&path),
        Commands::Tokenmap {
            path,
            format,
            output,
        } => cmd_tokenmap(&path, &format, &output),
        Commands::Serve { port } => daemon::run_serve(port),
        Commands::Stop => daemon::run_stop(),
        Commands::Doctor => doctor::run_doctor(),
        Commands::Filter { action } => {
            let repo_root = find_repo_root(&PathBuf::from("."));
            match action {
                FilterAction::List { index } => cmd_filter::cmd_filter_list(index, &repo_root),
                FilterAction::Active => cmd_filter::cmd_filter_active(),
                FilterAction::Generate { command } => {
                    cmd_filter::cmd_filter_generate(command, &repo_root)
                }
                FilterAction::Record { action } => match action {
                    RecordAction::Start { command } => {
                        cmd_filter::cmd_filter_record_start(command, &repo_root)
                    }
                    RecordAction::Stop => cmd_filter::cmd_filter_record_stop(&repo_root),
                    RecordAction::Status => cmd_filter::cmd_filter_record_status(&repo_root),
                },
            }
        }
        Commands::Run { command, path: _ } => {
            let code = compress::run_command_and_compress(&command)?;
            std::process::exit(code);
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
    };

    if let Err(ref e) = res {
        eprintln!("Error: {:?}", e);
        std::process::exit(1);
    } else {
        std::process::exit(0);
    }
}

fn set_env_default(key: &str, value: impl ToString) {
    #[allow(unused_unsafe)]
    unsafe {
        if std::env::var(key).is_err() {
            std::env::set_var(key, value.to_string());
        }
    }
}

fn set_env_override(key: &str, value: impl ToString) {
    #[allow(unused_unsafe)]
    unsafe {
        std::env::set_var(key, value.to_string());
    }
}

fn configure_index_limits(
    profile: CpuProfile,
    only_cpu: bool,
    jobs: Option<usize>,
    embed_batch: Option<usize>,
) {
    if matches!(profile, CpuProfile::Low) {
        set_env_override("RAYON_NUM_THREADS", jobs.unwrap_or(1).max(1));
        set_env_override("OMP_NUM_THREADS", 1);
        set_env_override("TOKENIX_EMBED_BATCH", embed_batch.unwrap_or(8).max(1));
        return;
    }

    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let rayon_threads = jobs.unwrap_or(cpus).max(1);
    set_env_default("RAYON_NUM_THREADS", rayon_threads);
    // Cap ONNX OpenMP threads: `default` stays conservative; `max` lets a
    // strong CPU use more cores. Memory is bounded by batch size, not threads.
    let omp_cap = match profile {
        CpuProfile::Max => 16,
        _ => 8,
    };
    set_env_default("OMP_NUM_THREADS", rayon_threads.min(omp_cap));

    // Embedding batch size drives ONNX peak memory (the historical PC-freeze
    // cause on CPU). Keep CPU small (≈2.8 GB RAM) and GPU moderate (fits 8 GB
    // VRAM). GPU build run with --only-cpu falls back to the CPU default.
    let gpu_active = cfg!(any(feature = "cuda", feature = "directml")) && !only_cpu;
    if let Some(batch) = embed_batch {
        set_env_override("TOKENIX_EMBED_BATCH", batch.max(1));
    } else if gpu_active {
        set_env_default("TOKENIX_EMBED_BATCH", 64);
    } else {
        set_env_default("TOKENIX_EMBED_BATCH", 16);
    }
}

fn cmd_index(path: &Path, force: bool, if_stale: bool, no_embed: bool) -> Result<()> {
    if !path.exists() {
        eprintln!(
            "{} Path does not exist: {}",
            "warning:".yellow(),
            path.display()
        );
    }
    let repo_root = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

    if if_stale && !force {
        let staleness = store::index_staleness(&repo_root);
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
    let mut progress = |msg: &str| println!("  {}", msg);
    let (result, stats) = indexer::index_repo_with_options(
        &repo_root,
        indexer::IndexOptions { force, no_embed },
        &mut progress,
    )?;

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

fn cmd_context(
    task: &str,
    budget: usize,
    max_files: usize,
    breakdown: bool,
    path: &Path,
) -> Result<()> {
    let repo_root = find_repo_root(path);
    let out = query::build_task_context(&repo_root, task, budget, max_files)?;
    println!("{}", out);
    if breakdown {
        print_budget_breakdown(&out, budget);
    }
    Ok(())
}

/// Print a per-section token breakdown of a generated context to stderr, so the
/// agent-facing stdout stays clean. Shared by `context` and `explore`.
fn print_budget_breakdown(context: &str, budget: usize) {
    let sections = query::budget_breakdown(context);
    let total: usize = sections.iter().map(|(_, t)| *t).sum();
    eprintln!("\ntokenix budget breakdown ({total}/{budget} tokens):");
    for (section, tokens) in &sections {
        let pct = if total > 0 {
            (*tokens as f64 / total as f64) * 100.0
        } else {
            0.0
        };
        eprintln!("  {section:<22} {tokens:>6}  ({pct:.0}%)");
    }
}

fn cmd_explore(
    query_text: &str,
    budget: usize,
    max_symbols: usize,
    breakdown: bool,
    path: &Path,
) -> Result<()> {
    let repo_root = find_repo_root(path);
    let out = query::build_explore_context(&repo_root, query_text, budget, max_symbols)?;
    println!("{}", out);
    if breakdown {
        print_budget_breakdown(&out, budget);
    }
    Ok(())
}

fn cmd_memory(action: MemoryAction) -> Result<()> {
    match action {
        MemoryAction::Add {
            text,
            global,
            project,
            path,
        } => {
            if global && project {
                anyhow::bail!("Use --global OR --project, not both");
            }
            let repo_root = find_repo_root(&path);
            let mut saved = Vec::new();
            if global {
                saved.push(memory::add_preference(
                    &repo_root,
                    memory::PreferenceScope::Global,
                    &text,
                )?);
            }
            if project || !global {
                saved.push(memory::add_preference(
                    &repo_root,
                    memory::PreferenceScope::Project,
                    &text,
                )?);
            }
            for path in saved {
                println!("saved {}", path.display());
            }
            Ok(())
        }
        MemoryAction::List {
            global,
            project,
            path,
        } => {
            let repo_root = find_repo_root(&path);
            let include_global = global || !project;
            let include_project = project || !global;
            println!(
                "{}",
                memory::list_preferences(&repo_root, include_global, include_project)?
            );
            Ok(())
        }
        MemoryAction::Remove {
            query,
            global,
            project,
            path,
        } => {
            let repo_root = find_repo_root(&path);
            for scope in selected_memory_scopes(global, project) {
                let (path, count) = memory::remove_preference(&repo_root, scope, &query)?;
                println!("removed {} from {}", count, path.display());
            }
            Ok(())
        }
        MemoryAction::Edit {
            query,
            replacement,
            global,
            project,
            path,
        } => {
            let repo_root = find_repo_root(&path);
            for scope in selected_memory_scopes(global, project) {
                let (path, count) =
                    memory::edit_preference(&repo_root, scope, &query, &replacement)?;
                println!("edited {} in {}", count, path.display());
            }
            Ok(())
        }
    }
}

fn selected_memory_scopes(global: bool, project: bool) -> Vec<memory::PreferenceScope> {
    if global && project {
        vec![
            memory::PreferenceScope::Global,
            memory::PreferenceScope::Project,
        ]
    } else if global {
        vec![memory::PreferenceScope::Global]
    } else {
        vec![memory::PreferenceScope::Project]
    }
}

fn cmd_query(text: &str, budget: usize, k: usize, file: Option<&str>, path: &Path) -> Result<()> {
    if k == 0 {
        anyhow::bail!("k must be >= 1");
    }
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

fn cmd_grep(
    pattern: &str,
    limit: usize,
    ignore_case: bool,
    file: Option<&str>,
    path: &Path,
) -> Result<()> {
    let conn = open_existing_index(path)?;
    let results = store::search_regex(&conn, pattern, limit, file, ignore_case)?;
    println!("{}", query::format_results(&results, pattern));
    Ok(())
}

fn open_existing_index(path: &Path) -> Result<rusqlite::Connection> {
    let repo_root = find_repo_root(path);
    store::open_db(&repo_root, false)?
        .ok_or_else(|| anyhow::anyhow!("Index not found. Run: tokenix index"))
}

fn cmd_symbols(query: &str, limit: usize, path: &Path) -> Result<()> {
    if limit == 0 {
        anyhow::bail!("limit must be >= 1");
    }
    let conn = open_existing_index(path)?;
    let nodes = store::search_graph_nodes(&conn, query, limit)?;
    println!(
        "{}",
        graph::format_nodes(&nodes, &format!("Symbols matching `{query}`"))
    );
    Ok(())
}

fn cmd_graph_relations(symbol: &str, limit: usize, path: &Path, callers: bool) -> Result<()> {
    if limit == 0 {
        anyhow::bail!("limit must be >= 1");
    }
    let conn = open_existing_index(path)?;
    let relations = if callers {
        store::graph_callers(&conn, symbol, limit)?
    } else {
        store::graph_callees(&conn, symbol, limit)?
    };
    let title = if callers {
        format!("Callers of `{symbol}`")
    } else {
        format!("Callees of `{symbol}`")
    };
    println!("{}", graph::format_relations(&relations, &title));
    Ok(())
}

fn cmd_impact(
    symbol: &str,
    depth: usize,
    limit: usize,
    format_str: &str,
    output: &str,
    path: &Path,
) -> Result<()> {
    let conn = open_existing_index(path)?;
    let relations = store::graph_impact(&conn, symbol, depth, limit)?;
    if format_str.eq_ignore_ascii_case("html") {
        let html =
            graph::export_relations_to_html(&relations, &format!("Impact graph for `{symbol}`"));
        std::fs::write(output, html)?;
        println!("{} HTML graph exported to {}", "ok".green(), output);
    } else {
        println!(
            "{}",
            graph::format_relations(&relations, &format!("Impact graph for `{symbol}`"))
        );
    }
    Ok(())
}

fn cmd_rebuild_graph(path: &Path) -> Result<()> {
    let conn = open_existing_index(path)?;
    graph::rebuild_symbol_graph(&conn)?;
    println!("{}", "Symbol graph rebuilt from indexed chunks".green());
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
                let total = file_lines.len();
                if s == 0 || e == 0 || s > total || e > total || s > e {
                    eprintln!(
                        "{}",
                        format!("Invalid line range. File has {total} lines (1-{total})").red()
                    );
                    std::process::exit(1);
                }
                let slice = file_lines[s - 1..e].join("\n");
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

fn cmd_gain(path: &Path, history: bool, cost_estimate: bool) -> Result<()> {
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

    if stats.total_calls == 0 {
        println!("{}", "  No hook events found in this project.".yellow());
        println!("\n  Possible causes:");
        println!("  1. tokenix hook is not installed for your AI tool");
        println!("     Run `tokenix install-hook` to fix this.");
        println!("  2. The AI tool is not calling the hook yet");
        println!("     Try reading a file or running a command in Claude Code / Copilot.");
        println!("  3. The hook is running in a different directory");
        println!("     Run `tokenix gain` in the project root (where .git exists).");
        return Ok(());
    }

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
    if cost_estimate {
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
    } else {
        println!();
        println!(
            "  {}",
            "Run with --cost-estimate to show the per-model cost table.".dimmed()
        );
    }

    // ── by tool / by phase ────────────────────────────────────────────────────
    if !stats.by_tool.is_empty() {
        println!();
        println!("  {}", "BY TOOL".bold().underline());
        for (tool, count, saved) in &stats.by_tool {
            let bar = mini_bar(*saved, stats.tokens_saved, 20);
            let pct = if stats.tokens_saved > 0 {
                (*saved as f64 / stats.tokens_saved as f64) * 100.0
            } else {
                0.0
            };
            let avg = if *count > 0 {
                *saved / *count as i64
            } else {
                0
            };
            println!(
                "  {:<14} {:>5} calls   {} {}  {}",
                tool.bold(),
                count,
                format_num(*saved).green(),
                format!("({:.0}% · avg {}/call)", pct, format_num(avg)).dimmed(),
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
            let pct = if stats.tokens_saved > 0 {
                (*saved as f64 / stats.tokens_saved as f64) * 100.0
            } else {
                0.0
            };
            println!(
                "  {}  {:>5} calls   {} {}  {}",
                label.bold(),
                count,
                format_num(*saved).green(),
                format!("({:.0}%)", pct).dimmed(),
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
            let saved_str = if e.saved_tokens > 0 {
                format!("saved {:>6}", format_num(e.saved_tokens))
                    .green()
                    .to_string()
            } else {
                format!("saved {:>6}", "0").dimmed().to_string()
            };
            let reason_str = if !e.reason.is_empty() {
                format!("  ({})", e.reason).dimmed().to_string()
            } else if !e.command.is_empty() {
                format!("  (cmd: {})", e.command).dimmed().to_string()
            } else {
                String::new()
            };
            println!(
                "  {} {} {:<8} {}  {} {}",
                ts.bright_black(),
                phase,
                e.tool.bold(),
                action,
                saved_str,
                reason_str
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
        Tool::Gemini => install_copilot()?,
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

    ensure_hooks_object(&mut settings);
    let removed_legacy_auto_index = remove_legacy_claude_auto_index_hook(&mut settings);
    remove_null_hook_event(&mut settings, "UserPromptSubmit");
    remove_null_hook_event(&mut settings, "PostToolUse");

    // Clean up existing tokenix hook configuration to ensure a clean reinstallation.
    remove_tokenix_hook_entries(&mut settings, "PreToolUse");
    remove_tokenix_hook_entries(&mut settings, "PostToolUse");

    // Claude Code uses matcher groups. Keep interception on canonical tool names
    // that tokenix can safely rewrite or compress before execution.
    let matcher = "^(Read|Grep|Bash|grep_search|run_in_terminal)$";
    let hook = serde_json::json!({
        "matcher": matcher,
        "hooks": [{"type": "command", "command": hook_command(&tokenix_bin, "hook"), "timeout": 10}]
    });
    if settings["hooks"]["PreToolUse"].is_array() {
        settings["hooks"]["PreToolUse"]
            .as_array_mut()
            .unwrap()
            .push(hook);
    } else {
        settings["hooks"]["PreToolUse"] = serde_json::json!([hook]);
    }

    std::fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;
    println!(
        "{} Claude Code  ->  {}",
        "ok".green(),
        settings_path.display()
    );
    println!(
        "  PreToolUse:  {} hook (Read/Grep/Bash interception)",
        tokenix_bin
    );
    if removed_legacy_auto_index {
        println!("  Removed legacy UserPromptSubmit auto-index hook");
    }
    Ok(())
}

fn remove_tokenix_hook_entries(settings: &mut serde_json::Value, event: &str) -> bool {
    let Some(arr) = settings
        .get_mut("hooks")
        .and_then(|hooks| hooks.get_mut(event))
        .and_then(|event| event.as_array_mut())
    else {
        return false;
    };
    let before = arr.len();
    arr.retain(|h| !h.to_string().contains("tokenix"));
    arr.len() != before
}

fn ensure_hooks_object(settings: &mut serde_json::Value) {
    if !settings
        .get("hooks")
        .is_some_and(serde_json::Value::is_object)
    {
        settings["hooks"] = serde_json::json!({});
    }
}

fn remove_null_hook_event(settings: &mut serde_json::Value, event: &str) -> bool {
    let Some(hooks) = settings.get_mut("hooks").and_then(|v| v.as_object_mut()) else {
        return false;
    };
    if hooks.get(event).is_some_and(serde_json::Value::is_null) {
        hooks.remove(event);
        return true;
    }
    false
}

fn remove_legacy_claude_auto_index_hook(settings: &mut serde_json::Value) -> bool {
    let Some(entries) = settings
        .get_mut("hooks")
        .and_then(|hooks| hooks.get_mut("UserPromptSubmit"))
        .and_then(|event| event.as_array_mut())
    else {
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
    // `.github/` is committed and shared across the team, so every generated
    // reference must resolve `tokenix` from PATH rather than baking the absolute exe
    // path of the machine that ran install-hook (which would be invalid on every
    // other clone and in CI). A bare name resolves on Windows (tokenix.exe via
    // PATHEXT), Linux, and macOS alike.
    let tokenix_bin = "tokenix";
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

    // 2. hooks/hooks.json - VS Code workspace hooks for token-efficient reads and
    // shell rewrites. Bash compression is handled through PreToolUse updatedInput.
    let hooks_dir = github_dir.join("hooks");
    std::fs::create_dir_all(&hooks_dir)?;
    let hooks_path = hooks_dir.join("hooks.json");

    let hook_cmd = format!("{tokenix_bin} hook");

    let hooks_json = serde_json::json!({
        "hooks": {
            "PreToolUse": [{
                "type": "command",
                "command": hook_cmd,
                "windows": hook_cmd,
                "timeout": 10
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

    println!(
        "  PreToolUse:  {} hook       (Read/Grep/Bash interception)",
        tokenix_bin
    );
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

    let hooks_path = codex_dir.join("hooks.json");

    #[cfg(windows)]
    {
        let hook_ps1_path = codex_dir.join("tokenix-codex-hook.ps1");
        std::fs::write(&hook_ps1_path, codex_hook_ps1(&tokenix_bin))?;
        println!(
            "{} Codex hook wrapper ->  {}",
            "ok".green(),
            hook_ps1_path.display()
        );
        install_codex_hooks_json_windows(&hooks_path, &hook_ps1_path)?;
    }

    #[cfg(not(windows))]
    install_codex_hooks_json_unix(&hooks_path, &tokenix_bin)?;

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

#[cfg(windows)]
fn codex_hook_ps1(tokenix_bin: &str) -> String {
    format!(
        r#"param(
  [Parameter(Mandatory = $true)]
    [ValidateSet("pre")]
  [string]$Phase
)

$ErrorActionPreference = "SilentlyContinue"
$inputJson = [Console]::In.ReadToEnd()
$tokenix = "{tokenix_bin}"
$subcommand = "hook"

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

#[cfg(windows)]
fn install_codex_hooks_json_windows(hooks_path: &Path, hook_ps1_path: &Path) -> Result<()> {
    let mut hooks = load_codex_hooks_json(hooks_path);
    let command = format!(
        "powershell -NoProfile -ExecutionPolicy Bypass -File \"{}\"",
        hook_ps1_path.to_string_lossy().replace('\\', "/")
    );
    upsert_codex_hook(
        &mut hooks["hooks"]["PreToolUse"],
        serde_json::json!({
            "matcher": "^(Bash|run_in_terminal|grep_search)$",
            "hooks": [{"type": "command", "command": format!("{command} pre"), "timeout": 10}]
        }),
    );
    std::fs::write(hooks_path, serde_json::to_string_pretty(&hooks)?)?;
    Ok(())
}

#[cfg(not(windows))]
fn install_codex_hooks_json_unix(hooks_path: &Path, tokenix_bin: &str) -> Result<()> {
    let mut hooks = load_codex_hooks_json(hooks_path);
    upsert_codex_hook(
        &mut hooks["hooks"]["PreToolUse"],
        serde_json::json!({
            "matcher": "^(Bash|run_in_terminal|grep_search)$",
            "hooks": [{"type": "command", "command": hook_command(tokenix_bin, "hook"), "timeout": 10}]
        }),
    );
    std::fs::write(hooks_path, serde_json::to_string_pretty(&hooks)?)?;
    Ok(())
}

fn load_codex_hooks_json(hooks_path: &Path) -> serde_json::Value {
    let mut hooks: serde_json::Value = if hooks_path.exists() {
        let raw = std::fs::read_to_string(hooks_path).unwrap_or_default();
        serde_json::from_str(&raw).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };
    if !hooks["hooks"].is_object() {
        hooks["hooks"] = serde_json::json!({});
    }
    hooks
}

fn upsert_codex_hook(slot: &mut serde_json::Value, hook: serde_json::Value) {
    if !slot.is_array() {
        *slot = serde_json::json!([]);
    }
    let arr = slot.as_array_mut().unwrap();
    // Remove any prior tokenix codex hook (Windows .ps1 path or Unix direct binary)
    arr.retain(|entry| {
        let s = entry.to_string();
        !s.contains("tokenix-codex-hook.ps1")
            && !s.contains("tokenix hook")
            && !s.contains("tokenix hook-post")
    });
    arr.push(hook);
}

// remove-hook

fn cmd_remove_hook(tool: Tool, local: bool) -> Result<()> {
    match tool {
        Tool::ClaudeCode => remove_claude_code(local)?,
        Tool::Copilot => remove_copilot()?,
        Tool::Codex => remove_codex()?,
        Tool::Mcp => remove_mcp_server()?,
        Tool::Gemini => remove_copilot()?,
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
        arr.retain(|entry| {
            let s = entry.to_string();
            !s.contains("tokenix-codex-hook.ps1") && !s.contains("tokenix hook")
        });
    }
}

fn mcp_config_path() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot find home directory"))?;
    Ok(home
        .join(".gemini")
        .join("antigravity-cli")
        .join("mcp_config.json"))
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
            .join("settings.local.json"))
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

fn cmd_tokenmap(path: &Path, format_opt: &str, output_path: &str) -> Result<()> {
    if format_opt != "html" && format_opt != "text" {
        anyhow::bail!("Invalid format '{format_opt}'. Use: text | html");
    }
    let repo_root = find_repo_root(path);
    let conn = store::open_db(&repo_root, false)?
        .ok_or_else(|| anyhow::anyhow!("No index found. Run: tokenix index"))?;

    let file_counts = store::get_file_token_counts(&conn)?;
    if file_counts.is_empty() {
        println!("No files found in index. Run: tokenix index");
        return Ok(());
    }

    let root_name = repo_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(".")
        .to_string();

    struct TreeNode {
        name: String,
        token_count: i64,
        is_file: bool,
        children: std::collections::BTreeMap<String, TreeNode>,
    }

    impl TreeNode {
        fn new(name: String, is_file: bool) -> Self {
            TreeNode {
                name,
                token_count: 0,
                is_file,
                children: std::collections::BTreeMap::new(),
            }
        }

        fn insert(&mut self, path_parts: &[&str], tokens: i64) {
            self.token_count += tokens;
            if path_parts.is_empty() {
                return;
            }

            let name = path_parts[0];
            let is_last = path_parts.len() == 1;

            let child = self
                .children
                .entry(name.to_string())
                .or_insert_with(|| TreeNode::new(name.to_string(), is_last));

            child.insert(&path_parts[1..], tokens);
        }
    }

    let mut root = TreeNode::new(root_name, false);
    for (file_path, tokens) in file_counts {
        let parts: Vec<&str> = file_path.split('/').filter(|s| !s.is_empty()).collect();
        root.insert(&parts, tokens);
    }

    if format_opt == "html" {
        #[derive(serde::Serialize)]
        struct EChartsNode {
            name: String,
            value: i64,
            #[serde(skip_serializing_if = "Vec::is_empty")]
            children: Vec<EChartsNode>,
        }

        fn to_echarts_node(node: &TreeNode) -> EChartsNode {
            let mut children = Vec::new();
            for child in node.children.values() {
                children.push(to_echarts_node(child));
            }
            EChartsNode {
                name: node.name.clone(),
                value: node.token_count,
                children,
            }
        }

        let chart_data_serialized = serde_json::to_string(&to_echarts_node(&root))?;
        let project_path_str = repo_root.to_string_lossy().replace('\\', "/");

        let html_template = include_str!("../assets/tokenmap_template.html")
            .replace("{{PROJECT_PATH}}", &project_path_str)
            .replace("{{TOTAL_TOKENS}}", &format_num(root.token_count))
            .replace("{{TOTAL_TOKENS_RAW}}", &root.token_count.to_string())
            .replace("{{CHART_DATA}}", &chart_data_serialized);

        std::fs::write(output_path, html_template)?;
        println!(
            "{} HTML Token Map successfully generated: {}",
            "ok".green(),
            output_path.bold().cyan()
        );
        return Ok(());
    }

    fn visual_bar(value: i64, total: i64, width: usize) -> String {
        if total == 0 {
            return format!("[{}]", "░".repeat(width));
        }
        let ratio = value as f64 / total as f64;
        let filled = (ratio * width as f64).round() as usize;
        let filled = filled.min(width);
        let empty = width - filled;

        let bar_text = format!("{}{}", "█".repeat(filled), "░".repeat(empty));
        let colored_bar = if ratio > 0.4 {
            bar_text.red()
        } else if ratio > 0.15 {
            bar_text.yellow()
        } else {
            bar_text.green()
        };

        format!("[{}]", colored_bar)
    }

    fn print_node(node: &TreeNode, prefix: &str, is_last: bool, total_tokens: i64) {
        let name_style = if node.is_file {
            node.name.normal()
        } else {
            node.name.bold().blue()
        };

        let percentage = if total_tokens > 0 {
            (node.token_count as f64 / total_tokens as f64) * 100.0
        } else {
            0.0
        };

        let bar = visual_bar(node.token_count, total_tokens, 8);
        let connector = if is_last { "└── " } else { "├── " };
        println!(
            "{}{} {} {} ({} tokens, {:.1}%)",
            prefix,
            connector,
            bar,
            name_style,
            format_num(node.token_count),
            percentage
        );

        let count = node.children.len();
        let new_prefix = if is_last {
            format!("{}    ", prefix)
        } else {
            format!("{}│   ", prefix)
        };

        for (i, child) in node.children.values().enumerate() {
            let is_child_last = i == count - 1;
            print_node(child, &new_prefix, is_child_last, total_tokens);
        }
    }

    println!(
        "\n{} ({} tokens)",
        root.name.bold(),
        format_num(root.token_count)
    );
    let count = root.children.len();
    for (i, child) in root.children.values().enumerate() {
        let is_child_last = i == count - 1;
        print_node(child, "", is_child_last, root.token_count);
    }
    println!();

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
    #[cfg(windows)] // codex_hook_ps1 is Windows-only; the test must be too.
    fn codex_wrapper_runs_pre_intercepts() {
        let ps1 = codex_hook_ps1("C:/tokenix/tokenix.exe");

        assert!(ps1.contains("ValidateSet(\"pre\")"));
        assert!(ps1.contains("$subcommand = \"hook\""));
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
                "matcher": "^Bash$",
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
    fn legacy_claude_auto_index_cleanup_does_not_create_null_event() {
        let mut settings = serde_json::json!({});

        let changed = remove_legacy_claude_auto_index_hook(&mut settings);

        assert!(!changed);
        assert_eq!(settings, serde_json::json!({}));
    }

    #[test]
    fn tokenix_hook_cleanup_does_not_create_null_event() {
        let mut settings = serde_json::json!({"hooks": {}});

        let changed = remove_tokenix_hook_entries(&mut settings, "PostToolUse");

        assert!(!changed);
        assert_eq!(settings, serde_json::json!({"hooks": {}}));
    }

    #[test]
    fn null_user_prompt_submit_hook_event_is_removed() {
        let mut settings = serde_json::json!({
            "hooks": {
                "UserPromptSubmit": null,
                "PreToolUse": []
            }
        });

        let changed = remove_null_hook_event(&mut settings, "UserPromptSubmit");

        assert!(changed);
        assert_eq!(
            settings,
            serde_json::json!({
                "hooks": {
                    "PreToolUse": []
                }
            })
        );
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

    #[test]
    fn remove_codex_hook_json_removes_unix_style_hooks() {
        let mut json = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {"hooks": [{"command": "other pre"}]},
                    {"hooks": [{"command": "/usr/local/bin/tokenix hook"}]}
                ],
                "PostToolUse": [
                    {"hooks": [{"command": "other post"}]},
                    {"hooks": [{"command": "/usr/local/bin/tokenix hook-post"}]}
                ]
            }
        });

        remove_codex_hooks_json(&mut json);

        assert_eq!(json["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
        assert_eq!(json["hooks"]["PostToolUse"].as_array().unwrap().len(), 1);
        assert!(json.to_string().contains("other pre"));
        assert!(json.to_string().contains("other post"));
        assert!(!json.to_string().contains("tokenix hook"));
        assert!(!json.to_string().contains("tokenix hook-post"));
    }
}
