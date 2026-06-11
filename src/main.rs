mod artifacts;
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
mod mcp_audit;
mod memory;
mod pack;
mod query;
mod recordings;
mod secrets_scan;
mod store;
mod tui;
mod ui;

use anyhow::Result;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use colored::Colorize;
use std::path::{Path, PathBuf};
use ui::format_num;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
#[command(
    name = "tokenix",
    version = VERSION,
    about = "Local semantic index for LLM token optimization",
    help_template = HELP_TEMPLATE
)]
struct Cli {
    /// Force CPU-only embedding, skipping the GPU even on a GPU-enabled build.
    /// GPU (DirectML/CUDA) is used by default when compiled with that support.
    #[arg(long, global = true)]
    only_cpu: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

/// Top-level help layout: wordmark banner (before-help) → usage → grouped
/// command catalog + examples (after-help). The default flat `{subcommands}`
/// block is intentionally omitted so commands can be grouped by audience.
const HELP_TEMPLATE: &str = "{before-help}{usage-heading} {usage}{after-help}";

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

#[derive(Clone, Copy, ValueEnum, Debug)]
enum AuditAgent {
    #[value(name = "claude")]
    Claude,
    #[value(name = "codex")]
    Codex,
    #[value(name = "copilot")]
    Copilot,
    #[value(name = "antigravity")]
    Antigravity,
    #[value(name = "all")]
    All,
}

#[derive(Clone, Copy, ValueEnum, Debug)]
enum McpProfile {
    Slim,
    Full,
}

#[derive(Copy, Clone, ValueEnum)]
enum ScanAgent {
    #[value(name = "claude")]
    Claude,
    #[value(name = "gemini")]
    Gemini,
    #[value(name = "copilot")]
    Copilot,
    #[value(name = "antigravity")]
    Antigravity,
    #[value(name = "all")]
    All,
}

impl ScanAgent {
    fn as_str(self) -> &'static str {
        match self {
            ScanAgent::Claude => "claude",
            ScanAgent::Gemini => "gemini",
            ScanAgent::Copilot => "copilot",
            ScanAgent::Antigravity => "antigravity",
            ScanAgent::All => "all",
        }
    }
}

#[derive(Copy, Clone, ValueEnum)]
enum GroupBy {
    None,
    Value,
    Rule,
    Agent,
    File,
    Repo,
}

impl GroupBy {
    fn to_mode(self) -> secrets_scan::GroupMode {
        match self {
            GroupBy::None => secrets_scan::GroupMode::None,
            GroupBy::Value => secrets_scan::GroupMode::Value,
            GroupBy::Rule => secrets_scan::GroupMode::Rule,
            GroupBy::Agent => secrets_scan::GroupMode::Agent,
            GroupBy::File => secrets_scan::GroupMode::File,
            GroupBy::Repo => secrets_scan::GroupMode::Repo,
        }
    }
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
        #[arg(
            long,
            help = "Embedding model id (e.g. nomic-v1.5, bge-small). See `tokenix doctor`"
        )]
        model: Option<String>,
        #[arg(long, help = "Update file chunks and symbol graph without embedding")]
        no_embed: bool,
        #[arg(
            long,
            help = "Keep normal process priority (default lowers it so indexing never starves the PC)"
        )]
        no_low_priority: bool,
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
        #[arg(long, help = "Cross-project search: additional project path(s)")]
        link: Vec<String>,
        #[arg(long, help = "Emit machine-readable JSON instead of text")]
        json: bool,
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
        #[arg(long, value_enum, default_value = "plan")]
        mode: query::ContextMode,
        #[arg(short, long, default_value_t = 1200)]
        budget: usize,
        #[arg(long, default_value_t = 4)]
        max_files: usize,
        #[arg(long, help = "Print per-section token breakdown to stderr")]
        budget_breakdown: bool,
        #[arg(long, help = "Emit machine-readable JSON instead of text")]
        json: bool,
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
        #[arg(long, help = "Emit machine-readable JSON instead of text")]
        json: bool,
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
        #[arg(long, help = "Emit machine-readable JSON instead of text")]
        json: bool,
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Find indexed symbols by name or path
    Symbols {
        query: String,
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
        #[arg(
            short,
            long,
            help = "Filter by symbol kind (function, struct, class, method, ...)"
        )]
        kind: Option<String>,
        #[arg(long, help = "Emit machine-readable JSON instead of text")]
        json: bool,
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Show symbols that call a target symbol
    Callers {
        symbol: String,
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
        #[arg(long, help = "Emit machine-readable JSON instead of text")]
        json: bool,
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Show symbols called by a target symbol
    Callees {
        symbol: String,
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
        #[arg(long, help = "Emit machine-readable JSON instead of text")]
        json: bool,
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
        #[arg(
            long,
            help = "Output format: text | html | mermaid | json",
            default_value = "text"
        )]
        format: String,
        #[arg(
            short,
            long,
            help = "Output file path for html/mermaid format",
            default_value = "impact.html"
        )]
        output: String,
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Show file-level import dependencies of a file
    Deps {
        file: String,
        #[arg(long, help = "Show files that import the target instead")]
        reverse: bool,
        #[arg(long, help = "Follow resolved imports transitively")]
        transitive: bool,
        #[arg(long, help = "Emit machine-readable JSON instead of text")]
        json: bool,
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Trace forward execution flow from an entry point
    Flow {
        symbol: String,
        #[arg(short, long, default_value_t = 3)]
        depth: usize,
        #[arg(short, long, default_value_t = 50)]
        limit: usize,
        #[arg(long, help = "Output format: text | mermaid", default_value = "text")]
        format: String,
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Detect circular dependencies in the symbol graph
    Cycles {
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
        #[arg(
            long,
            help = "Aggregate savings across all indexed projects (ignores --path)"
        )]
        global: bool,
    },
    /// Pack focused repository context for AI tools that cannot call tokenix hooks
    Pack {
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
        #[arg(long, alias = "mode", value_enum, default_value = "plan")]
        profile: pack::PackProfile,
        #[arg(long, default_value_t = 8_000)]
        budget: usize,
        #[arg(long, value_enum, default_value = "markdown")]
        format: pack::PackFormat,
        #[arg(long, help = "Pack only changed files plus focused context")]
        changed: bool,
        #[arg(long, help = "Pack files changed since this git ref")]
        since: Option<String>,
        #[arg(long, help = "Include per-file token map and safety report")]
        token_map: bool,
        #[arg(short, long, help = "Write pack output to a file instead of stdout")]
        output: Option<PathBuf>,
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
        #[arg(long, help = "TOML file with project-specific benchmark cases")]
        cases: Option<PathBuf>,
        #[arg(long, help = "Emit machine-readable benchmark summary")]
        json: bool,
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
    /// Copy this executable to a per-user bin directory on PATH (global install)
    InstallBinary,
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
    /// Inspect or control the background embedding daemon
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
    /// Diagnose embedding backend, GPU availability, model cache, and daemon
    Doctor,
    /// Generate and manage per-command output filters
    Filter {
        #[command(subcommand)]
        action: Option<FilterAction>,
    },
    /// Run a command and compress its output using tokenix filters (used by PreToolUse rewrite)
    Run {
        /// Command to execute
        command: String,
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Audit MCP/tool "weight" of the effective system prompt across AI agents
    PromptAudit {
        /// Which agent to audit (default: all that have config)
        #[arg(long, value_enum, default_value = "all")]
        agent: AuditAgent,
        /// Emit machine-readable JSON instead of a human report
        #[arg(long)]
        json: bool,
        /// Include token-saving recommendations
        #[arg(long)]
        recommend: bool,
        /// Show estimated full vs slim tokenix MCP profile impact
        #[arg(long)]
        profile_impact: bool,
    },
    /// Summarize token economy risks for this session/repository
    SessionAudit {
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
        /// Emit machine-readable JSON instead of a human report
        #[arg(long)]
        json: bool,
        /// Check prompt-cache hygiene inputs such as MCP churn and hook/index freshness
        #[arg(long)]
        cache_hygiene: bool,
    },
    /// Scan AI agent conversation transcripts for exposed credentials (gitleaks-style, no git)
    ScanSecrets {
        /// Which agent's conversations to scan
        #[arg(long, value_enum, default_value = "all")]
        agent: ScanAgent,
        /// Filter findings by a case-insensitive substring (rule, agent, file, or value)
        #[arg(long)]
        filter: Option<String>,
        /// Group the report by detected value, rule, agent, file, or repo
        #[arg(long, value_enum, default_value = "none")]
        group: GroupBy,
        /// Reveal raw secret values instead of redacting them
        #[arg(long)]
        reveal: bool,
        /// Emit machine-readable JSON instead of a human report
        #[arg(long)]
        json: bool,
    },
    /// Manage context artifacts (non-code files in .tokenix/artifacts.json)
    #[command(subcommand)]
    Artifacts(ArtifactsAction),
    /// Hook handler called by AI tools (not for direct use)
    Hook,
    /// PostToolUse hook handler for output compression (not for direct use)
    HookPost,
    /// Run as a Model Context Protocol (MCP) server over stdin/stdout
    Mcp {
        #[arg(long, value_enum, default_value = "full")]
        profile: McpProfile,
    },
}

#[derive(Subcommand)]
enum ArtifactsAction {
    /// List all context artifacts
    List {
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Show the content of a context artifact
    Show {
        name: String,
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
}

#[derive(Subcommand)]
enum DaemonAction {
    /// Show daemon state: pid, port, uptime, model, cache size
    Status,
    /// Stop the running daemon
    Stop,
    /// Stop (if running) and start a fresh daemon
    Restart,
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
    // Build the command via the factory so the wordmark banner and grouped
    // command catalog (both runtime-colored) can be attached as styled help.
    let mut cmd = Cli::command()
        .before_help(banner())
        .after_help(help_catalog());
    let matches = cmd.clone().get_matches();
    let cli = Cli::from_arg_matches(&matches)?;

    // Must be set before the embedding model is first initialized.
    let only_cpu = cli.only_cpu;
    crate::embed::set_force_cpu(only_cpu);

    // Bare `tokenix`: launch the interactive launcher on a TTY (one cursor menu
    // over the human commands), else fall back to the grouped help for pipes/CI.
    let command = match cli.command {
        Some(command) => command,
        None => {
            use std::io::IsTerminal;
            if std::io::stdout().is_terminal() {
                return tui::run();
            }
            cmd.print_help()?;
            println!();
            std::process::exit(0);
        }
    };

    let res = match command {
        Commands::Index {
            path,
            force,
            if_stale,
            cpu_profile,
            jobs,
            embed_batch,
            model,
            no_embed,
            no_low_priority,
        } => {
            if let Some(model) = model.as_deref() {
                if !crate::embed::is_known_model(model) {
                    let ids: Vec<&str> = crate::embed::MODELS.iter().map(|m| m.id).collect();
                    anyhow::bail!("unknown --model '{model}'. Available: {}", ids.join(", "));
                }
                crate::embed::set_active_model(model);
                set_env_override("TOKENIX_EMBED_MODEL", model);
            }
            configure_index_limits(cpu_profile, only_cpu, jobs, embed_batch);
            // PC-friendliness: indexing is the one long CPU-bound run; drop to
            // below-normal priority unless the user opts out (flag or env).
            if !no_low_priority && std::env::var_os("TOKENIX_FOREGROUND").is_none() {
                indexer::lower_process_priority();
            }
            cmd_index(&path, force, if_stale, no_embed)
        }
        Commands::Query {
            text,
            budget,
            k,
            file,
            link,
            json,
            path,
        } => cmd_query(&text, budget, k, file.as_deref(), &link, json, &path),
        Commands::Grep {
            pattern,
            limit,
            ignore_case,
            file,
            path,
        } => cmd_grep(&pattern, limit, ignore_case, file.as_deref(), &path),
        Commands::Context {
            task,
            mode,
            budget,
            max_files,
            budget_breakdown,
            json,
            path,
        } => cmd_context(
            &task,
            mode,
            budget,
            max_files,
            budget_breakdown,
            json,
            &path,
        ),
        Commands::Explore {
            query,
            budget,
            max_symbols,
            budget_breakdown,
            json,
            path,
        } => cmd_explore(&query, budget, max_symbols, budget_breakdown, json, &path),
        Commands::Memory { action } => cmd_memory(action),
        Commands::Read {
            file,
            symbol,
            lines,
            json,
            path,
        } => cmd_read(&file, symbol.as_deref(), lines.as_deref(), json, &path),
        Commands::Symbols {
            query,
            limit,
            kind,
            json,
            path,
        } => cmd_symbols(&query, limit, kind.as_deref(), json, &path),
        Commands::Callers {
            symbol,
            limit,
            json,
            path,
        } => cmd_graph_relations(&symbol, limit, json, &path, true),
        Commands::Callees {
            symbol,
            limit,
            json,
            path,
        } => cmd_graph_relations(&symbol, limit, json, &path, false),
        Commands::Impact {
            symbol,
            depth,
            limit,
            format,
            output,
            path,
        } => cmd_impact(&symbol, depth, limit, &format, &output, &path),
        Commands::Flow {
            symbol,
            depth,
            limit,
            format,
            path,
        } => cmd_flow(&symbol, depth, limit, &format, &path),
        Commands::Deps {
            file,
            reverse,
            transitive,
            json,
            path,
        } => cmd_deps(&file, reverse, transitive, json, &path),
        Commands::Cycles { path } => cmd_cycles(&path),
        Commands::RebuildGraph { path } => cmd_rebuild_graph(&path),
        Commands::Gain {
            path,
            history,
            cost_estimate,
            global,
        } => {
            if global {
                cmd_gain_global(history, cost_estimate)
            } else {
                cmd_gain(&path, history, cost_estimate)
            }
        }
        Commands::Pack {
            path,
            profile,
            budget,
            format,
            changed,
            since,
            token_map,
            output,
        } => cmd_pack(
            &path, profile, budget, format, changed, since, token_map, output,
        ),
        Commands::Benchmark {
            path,
            refresh_index,
            budget,
            cases,
            json,
        } => {
            let repo_root = find_repo_root(&path);
            benchmark::run_benchmark(&repo_root, refresh_index, budget, cases.as_deref(), json)
        }
        Commands::InstallHook { tool, local } => cmd_install_hook(tool, local),
        Commands::InstallBinary => cmd_install_binary(),
        Commands::RemoveHook { tool, local } => cmd_remove_hook(tool, local),
        Commands::Stats { path } => cmd_stats(&path),
        Commands::Tokenmap {
            path,
            format,
            output,
        } => cmd_tokenmap(&path, &format, &output),
        Commands::Serve { port } => daemon::run_serve(port),
        Commands::Stop => daemon::run_stop(),
        Commands::Daemon { action } => match action {
            DaemonAction::Status => daemon::run_status(),
            DaemonAction::Stop => daemon::run_stop(),
            DaemonAction::Restart => daemon::run_restart(),
        },
        Commands::Doctor => doctor::run_doctor(),
        Commands::Filter { action } => {
            let repo_root = find_repo_root(&PathBuf::from("."));
            let Some(action) = action else {
                // Bare `tokenix filter`: interactive browser on a TTY, else the
                // plain list so pipes / CI keep working.
                use std::io::IsTerminal;
                return if std::io::stdout().is_terminal() {
                    tui::run()
                } else {
                    cmd_filter::cmd_filter_list(None, &repo_root)
                };
            };
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
        Commands::PromptAudit {
            agent,
            json,
            recommend,
            profile_impact,
        } => {
            let filter = match agent {
                AuditAgent::Claude => Some(mcp_audit::Agent::ClaudeCode),
                AuditAgent::Codex => Some(mcp_audit::Agent::Codex),
                AuditAgent::Copilot => Some(mcp_audit::Agent::Copilot),
                AuditAgent::Antigravity => Some(mcp_audit::Agent::Antigravity),
                AuditAgent::All => None,
            };
            let cwd = std::env::current_dir()?;
            mcp_audit::run_audit(filter, json, recommend, profile_impact, &cwd)
        }
        Commands::SessionAudit {
            path,
            json,
            cache_hygiene,
        } => cmd_session_audit(&path, json, cache_hygiene),
        Commands::ScanSecrets {
            agent,
            filter,
            group,
            reveal,
            json,
        } => {
            let found = secrets_scan::run(secrets_scan::Options {
                agent: agent.as_str().to_string(),
                json,
                search: filter,
                group: group.to_mode(),
                reveal,
            })?;
            if found > 0 {
                std::process::exit(1);
            }
            Ok(())
        }
        Commands::Artifacts(action) => match action {
            ArtifactsAction::List { path } => cmd_artifacts_list(&path),
            ArtifactsAction::Show { name, path } => cmd_artifacts_show(&path, &name),
        },
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
            if let Err(e) = hook::run_hook() {
                eprintln!("tokenix hook fail-open: {e:?}");
            }
            std::process::exit(0);
        }
        Commands::HookPost => {
            #[allow(unused_unsafe)]
            unsafe {
                std::env::set_var("RAYON_NUM_THREADS", "1")
            };
            if let Err(e) = compress::run_hook_post() {
                eprintln!("tokenix hook-post fail-open: {e:?}");
            }
            std::process::exit(0);
        }
        Commands::Mcp { profile } => {
            #[allow(unused_unsafe)]
            unsafe {
                std::env::set_var("OMP_NUM_THREADS", "1");
                std::env::set_var("RAYON_NUM_THREADS", "1");
            }
            let profile = match profile {
                McpProfile::Slim => mcp::McpProfile::Slim,
                McpProfile::Full => mcp::McpProfile::Full,
            };
            mcp::run_mcp_server(profile)
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
    mode: query::ContextMode,
    budget: usize,
    max_files: usize,
    breakdown: bool,
    json: bool,
    path: &Path,
) -> Result<()> {
    let repo_root = find_repo_root(path);
    let out = query::build_task_context_with_mode(&repo_root, task, mode, budget, max_files)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "task": task,
                "budget": budget,
                "tokens": chunker::count_tokens(&out),
                "context": out,
            }))?
        );
        return Ok(());
    }
    println!("{}", out);
    if breakdown {
        print_budget_breakdown(&out, budget);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_pack(
    path: &Path,
    profile: pack::PackProfile,
    budget: usize,
    format: pack::PackFormat,
    changed: bool,
    since: Option<String>,
    token_map: bool,
    output: Option<PathBuf>,
) -> Result<()> {
    let repo_root = find_repo_root(path);
    let options = pack::PackOptions {
        profile,
        budget,
        format,
        changed,
        since,
        token_map,
    };
    let content = pack::build_pack(&repo_root, options)?;
    pack::write_or_print(output, &content)
}

fn cmd_session_audit(path: &Path, json: bool, cache_hygiene: bool) -> Result<()> {
    let repo_root = find_repo_root(path);
    let conn = store::open_db(&repo_root, false)?;
    let stats = if let Some(conn) = &conn {
        Some(store::count_stats(conn)?)
    } else {
        None
    };
    let stale = conn.as_ref().map(|_| store::index_staleness(&repo_root));
    let age = store::get_index_age(&repo_root);
    let hooks = store::read_hook_log(&repo_root);
    let prompt = mcp_audit::audit_summary(None, &repo_root);

    let mut recommendations = Vec::new();
    if stats.is_none() {
        recommendations.push("index missing: run `tokenix index .`".to_string());
    }
    if stale.as_ref().is_some_and(|s| s.stale) {
        recommendations.push("index stale: run `tokenix index . --if-stale`".to_string());
    }
    if hooks.is_empty() {
        recommendations.push("no hook events found: install hooks or run a hook smoke".to_string());
    }
    if prompt.combined_tokens > 10_000 {
        recommendations.push(format!(
            "MCP/tool prompt weight high (~{} tok): run `tokenix prompt-audit --recommend`",
            prompt.combined_tokens
        ));
    }
    if cache_hygiene && prompt.combined_tokens > 6_000 {
        recommendations.push(
            "cache hygiene: keep MCP/tool profiles stable and prefer `tokenix mcp --profile slim`"
                .to_string(),
        );
    }
    if recommendations.is_empty() {
        recommendations.push("token economy looks healthy for this repository".to_string());
    }

    if json {
        let out = serde_json::json!({
            "project": repo_root.display().to_string(),
            "index": stats.as_ref().map(|s| serde_json::json!({
                "files": s.files,
                "chunks": s.chunks,
                "tokens": s.total_tokens,
                "age_seconds": age,
                "stale": stale.as_ref().map(|s| s.stale).unwrap_or(false),
                "reason": stale.as_ref().map(|s| s.reason.clone()).unwrap_or_default(),
            })),
            "hook_events": hooks.len(),
            "prompt_audit": {
                "combined_tokens": prompt.combined_tokens,
                "warnings": prompt.warnings,
            },
            "cache_hygiene": cache_hygiene.then(|| serde_json::json!({
                "stable_prefix_risk": prompt.combined_tokens > 6_000 || stale.as_ref().is_some_and(|s| s.stale),
                "mcp_profile_hint": "prefer slim tokenix MCP profile for routine sessions",
                "index_stale": stale.as_ref().map(|s| s.stale).unwrap_or(false),
            })),
            "recommendations": recommendations,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    println!("{}", "tokenix session-audit".bold());
    println!("Project: {}", repo_root.display());
    if let Some(stats) = stats {
        println!(
            "Index: {} files, {} chunks, {} tokens{}",
            stats.files,
            stats.chunks,
            format_num(stats.total_tokens),
            age.map(|a| format!(", age {:.0}s", a)).unwrap_or_default()
        );
    } else {
        println!("{}", "Index: missing".yellow());
    }
    if let Some(stale) = &stale {
        let verdict = if stale.stale {
            "stale".yellow()
        } else {
            "fresh".green()
        };
        println!("Staleness: {verdict} ({})", stale.reason);
    }
    println!("Hook events: {}", hooks.len());
    println!(
        "Prompt/tool estimate: ~{} tok ({} warning(s))",
        prompt.combined_tokens,
        prompt.warnings.len()
    );
    if cache_hygiene {
        let stable = prompt.combined_tokens <= 6_000 && !stale.as_ref().is_some_and(|s| s.stale);
        println!(
            "Cache hygiene: {}",
            if stable {
                "stable enough".green()
            } else {
                "review MCP/index churn".yellow()
            }
        );
        println!("  Hint: prefer `tokenix mcp --profile slim` for routine sessions");
    }
    println!("\nRecommendations:");
    for rec in recommendations {
        println!("- {rec}");
    }
    Ok(())
}

fn cmd_artifacts_list(path: &Path) -> Result<()> {
    let repo_root = find_repo_root(path);
    crate::artifacts::list_artifacts(&repo_root)
}

fn cmd_artifacts_show(path: &Path, name: &str) -> Result<()> {
    let repo_root = find_repo_root(path);
    crate::artifacts::show_artifact(&repo_root, name)
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
    json: bool,
    path: &Path,
) -> Result<()> {
    let repo_root = find_repo_root(path);
    let out = query::build_explore_context(&repo_root, query_text, budget, max_symbols)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "query": query_text,
                "budget": budget,
                "tokens": chunker::count_tokens(&out),
                "context": out,
            }))?
        );
        return Ok(());
    }
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

fn cmd_query(
    text: &str,
    budget: usize,
    k: usize,
    file: Option<&str>,
    link: &[String],
    json: bool,
    path: &Path,
) -> Result<()> {
    if k == 0 {
        anyhow::bail!("k must be >= 1");
    }
    let repo_root = find_repo_root(path);

    // Cross-project search: include linked projects
    if !link.is_empty() {
        let mut roots: Vec<PathBuf> = vec![repo_root.clone()];
        for link_path in link {
            roots.push(find_repo_root(Path::new(link_path)));
        }
        let root_refs: Vec<&Path> = roots.iter().map(|p| p.as_path()).collect();
        let results = query::query_index_multi(&root_refs, text, budget, k, file)?
            .ok_or_else(|| anyhow::anyhow!("No indexed projects found. Run: tokenix index"))?;
        print_search_results(&results, text, json)?;
        return Ok(());
    }

    // Try to query via daemon if it's running. The daemon returns pre-formatted
    // text, so JSON output takes the direct path instead.
    if !json {
        if let Some(output) = daemon::daemon_search(&repo_root, text, k, budget, file) {
            println!("{}", output);
            return Ok(());
        }
    }

    let results = query::query_index(&repo_root, text, budget, k, file)?
        .ok_or_else(|| anyhow::anyhow!("Index not found. Run: tokenix index"))?;
    print_search_results(&results, text, json)?;
    Ok(())
}

fn print_search_results(results: &[store::SearchResult], text: &str, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(results)?);
    } else {
        println!("{}", query::format_results(results, text));
    }
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

fn cmd_symbols(
    query: &str,
    limit: usize,
    kind: Option<&str>,
    json: bool,
    path: &Path,
) -> Result<()> {
    if limit == 0 {
        anyhow::bail!("limit must be >= 1");
    }
    let conn = open_existing_index(path)?;
    let nodes = store::search_graph_nodes_kind(&conn, query, limit, kind)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&nodes)?);
        return Ok(());
    }
    println!(
        "{}",
        graph::format_nodes(&nodes, &format!("Symbols matching `{query}`"))
    );
    Ok(())
}

fn cmd_graph_relations(
    symbol: &str,
    limit: usize,
    json: bool,
    path: &Path,
    callers: bool,
) -> Result<()> {
    if limit == 0 {
        anyhow::bail!("limit must be >= 1");
    }
    let conn = open_existing_index(path)?;
    let relations = if callers {
        store::graph_callers(&conn, symbol, limit)?
    } else {
        store::graph_callees(&conn, symbol, limit)?
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&relations)?);
        return Ok(());
    }
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
    if format_str.eq_ignore_ascii_case("json") {
        println!("{}", serde_json::to_string_pretty(&relations)?);
    } else if format_str.eq_ignore_ascii_case("html") {
        let html =
            graph::export_relations_to_html(&relations, &format!("Impact graph for `{symbol}`"));
        std::fs::write(output, html)?;
        println!("{} HTML graph exported to {}", "ok".green(), output);
    } else if format_str.eq_ignore_ascii_case("mermaid") {
        let mermaid =
            graph::format_relations_mermaid(&relations, &format!("Impact graph for `{symbol}`"));
        if output != "impact.html" {
            std::fs::write(output, &mermaid)?;
            println!("{} Mermaid diagram exported to {}", "ok".green(), output);
        } else {
            println!("{}", mermaid);
        }
    } else {
        println!(
            "{}",
            graph::format_relations(&relations, &format!("Impact graph for `{symbol}`"))
        );
    }
    Ok(())
}

fn cmd_flow(symbol: &str, depth: usize, limit: usize, format_str: &str, path: &Path) -> Result<()> {
    let conn = open_existing_index(path)?;
    let relations = store::graph_flow(&conn, symbol, depth, limit)?;
    if format_str.eq_ignore_ascii_case("mermaid") {
        println!(
            "{}",
            graph::format_relations_mermaid(&relations, &format!("Call flow from `{symbol}`"))
        );
    } else {
        println!(
            "{}",
            graph::format_relations(&relations, &format!("Call flow from `{symbol}`"))
        );
    }
    Ok(())
}

fn cmd_cycles(path: &Path) -> Result<()> {
    let conn = open_existing_index(path)?;
    let edges = store::load_all_graph_edges(&conn)?;
    let cycles = graph::detect_cycles(&edges);
    println!("{}", graph::format_cycles(&cycles));
    Ok(())
}

fn cmd_rebuild_graph(path: &Path) -> Result<()> {
    let repo_root = find_repo_root(path);
    let conn = open_existing_index(path)?;
    // Idempotent: brings pre-upgrade DBs up to date (e.g. graph_imports table).
    store::init_schema(&conn, 768)?;
    graph::rebuild_symbol_graph(&conn)?;
    let imports = graph::rebuild_import_graph(&conn, &repo_root)?;
    println!(
        "{}",
        format!("Symbol graph rebuilt; import graph: {imports} edge(s)").green()
    );
    Ok(())
}

fn cmd_deps(file: &str, reverse: bool, transitive: bool, json: bool, path: &Path) -> Result<()> {
    let conn = open_existing_index(path)?;
    let mut edges = store::file_imports(&conn, file, reverse)?;

    if transitive {
        let mut seen = std::collections::HashSet::new();
        let mut frontier: Vec<String> = edges
            .iter()
            .filter_map(|e| {
                if reverse {
                    Some(e.source_path.clone())
                } else {
                    e.resolved_path.clone()
                }
            })
            .collect();
        while let Some(next) = frontier.pop() {
            if !seen.insert(next.clone()) {
                continue;
            }
            for e in store::file_imports(&conn, &next, reverse)? {
                let hop = if reverse {
                    Some(e.source_path.clone())
                } else {
                    e.resolved_path.clone()
                };
                if let Some(h) = hop {
                    if !seen.contains(&h) {
                        frontier.push(h);
                    }
                }
                edges.push(e);
            }
        }
        edges.sort_by(|a, b| (&a.source_path, a.line).cmp(&(&b.source_path, b.line)));
        edges.dedup_by(|a, b| {
            a.source_path == b.source_path && a.target == b.target && a.line == b.line
        });
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&edges)?);
        return Ok(());
    }
    let title = if reverse {
        format!("Files importing `{file}`")
    } else {
        format!("Imports of `{file}`")
    };
    println!("{}", graph::format_imports(&edges, &title, reverse));
    Ok(())
}

fn cmd_read(
    file: &str,
    symbol: Option<&str>,
    lines_range: Option<&str>,
    json: bool,
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
        anyhow::bail!("File not found: {}", file);
    }

    let content = std::fs::read_to_string(&fp)?;
    let file_lines: Vec<&str> = content.lines().collect();

    let rel = fp
        .strip_prefix(&repo_root)
        .unwrap_or(&fp)
        .to_string_lossy()
        .replace('\\', "/");

    if let Some(range) = lines_range {
        let parts: Vec<&str> = range.split('-').collect();
        if parts.len() == 2 {
            if let (Ok(s), Ok(e)) = (parts[0].parse::<usize>(), parts[1].parse::<usize>()) {
                let total = file_lines.len();
                if s == 0 || e == 0 || s > total || e > total || s > e {
                    anyhow::bail!("Invalid line range. File has {} lines (1-{})", total, total);
                }
                let slice = file_lines[s - 1..e].join("\n");
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "path": rel, "mode": "lines", "start": s, "end": e,
                            "content": slice,
                        }))?
                    );
                } else {
                    println!("{}", slice);
                }
                return Ok(());
            }
        }
        anyhow::bail!("Invalid --lines format. Use: N-M");
    }

    if let Some(sym) = symbol {
        let chunks = chunker::chunk_file(&rel, &content);
        let found: Vec<_> = chunks
            .iter()
            .filter(|c| c.symbol.to_lowercase().contains(&sym.to_lowercase()))
            .collect();
        if found.is_empty() {
            anyhow::bail!("Symbol not found: '{}'", sym);
        }
        if json {
            println!("{}", serde_json::to_string_pretty(&found)?);
            return Ok(());
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

    if json {
        // Symbol outline as data: chunk metadata without bodies keeps the
        // payload small regardless of file size.
        let outline: Vec<serde_json::Value> = chunker::chunk_file(&rel, &content)
            .iter()
            .map(|c| {
                serde_json::json!({
                    "start_line": c.start_line, "end_line": c.end_line,
                    "kind": c.kind, "symbol": c.symbol,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "path": rel, "mode": "outline", "lines": file_lines.len(),
                "symbols": outline,
            }))?
        );
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
    ui::box_header(&format!("tokenix gain  ·  {}", project_name));

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
    let bar = format!("[{}]", ui::bar(stats.pct_saved / 100.0, 18));
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

    // ── savings by source ─────────────────────────────────────────────────────
    println!();
    println!("  {}", "BY SOURCE".bold().underline());
    for (label, desc, saved, calls) in [
        (
            "Semantic index",
            "Read/Grep → outlines & index queries",
            stats.index_saved,
            stats.index_calls,
        ),
        (
            "Command filters",
            "Bash output compressed by filters",
            stats.filter_saved,
            stats.filter_calls,
        ),
    ] {
        let pct = if stats.tokens_saved > 0 {
            saved as f64 / stats.tokens_saved as f64 * 100.0
        } else {
            0.0
        };
        let bar = ui::bar(saved as f64 / stats.tokens_saved.max(1) as f64, 20);
        println!(
            "  {:<16} {:>5} calls   {} {}  {}  {}",
            label.bold(),
            calls,
            format_num(saved).green(),
            format!("({:.0}%)", pct).dimmed(),
            bar.bright_black(),
            desc.dimmed()
        );
    }

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

        let rows: Vec<Vec<String>> = stats
            .cost_rows
            .iter()
            .map(|row| {
                let m = gain::MODELS.iter().find(|m| m.name == row.model).unwrap();
                let marker = if row.reference { " ★" } else { "" };
                vec![
                    format!("{}{}", row.model, marker),
                    format!("${:.2}", m.input_per_1m),
                    format!("${:.4}", row.without_usd),
                    format!("${:.4}", row.with_usd),
                    ui::ok(&format!("${:.4}", row.saved_usd)).to_string(),
                ]
            })
            .collect();
        ui::print_table(
            &["Model", "$/1M in", "Without", "With", "Saved"],
            &rows,
            &[1, 2, 3, 4],
        );
        println!(
            "  {}",
            format!(
                "  ★ reference model · prices collected {}",
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
            let bar = ui::bar(*saved as f64 / stats.tokens_saved.max(1) as f64, 20);
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
                "ToolOutputCompressed" => ("ToolCompressed", "Bash command rewrite + compress"),
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

    // ── by command ────────────────────────────────────────────────────────────
    print_by_command(&stats.by_command, stats.tokens_saved);

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

fn print_by_command(by_command: &[(String, usize, i64)], tokens_saved: i64) {
    const TOP_N: usize = 15;
    let visible: Vec<_> = by_command.iter().take(TOP_N).collect();
    if visible.is_empty() {
        return;
    }
    println!();
    println!("  {}", "BY COMMAND  (Bash filters)".bold().underline());
    for (cmd, count, saved) in &visible {
        let bar = ui::bar(*saved as f64 / tokens_saved.max(1) as f64, 20);
        let pct = if tokens_saved > 0 {
            (*saved as f64 / tokens_saved as f64) * 100.0
        } else {
            0.0
        };
        let avg = if *count > 0 {
            *saved / *count as i64
        } else {
            0
        };
        println!(
            "  {:<28} {:>4}×   {} {}  {}",
            cmd.bold(),
            count,
            format_num(*saved).green(),
            format!("({:.0}% · avg {}/call)", pct, format_num(avg)).dimmed(),
            bar.bright_black()
        );
    }
    if by_command.len() > TOP_N {
        println!(
            "  {}",
            format!("  … +{} more commands", by_command.len() - TOP_N).dimmed()
        );
    }
}

fn cmd_gain_global(history: bool, cost_estimate: bool) -> Result<()> {
    let global = gain::compute_global_gain();
    let stats = &global.aggregate;

    // ── header ────────────────────────────────────────────────────────────────
    ui::box_header("tokenix gain  ·  ALL PROJECTS");

    if stats.total_calls == 0 {
        println!(
            "{}",
            "  No hook events found in any project. Run `tokenix install-hook` first.".yellow()
        );
        return Ok(());
    }

    // ── aggregate token summary ───────────────────────────────────────────────
    println!();
    let bar = format!("[{}]", ui::bar(stats.pct_saved / 100.0, 18));
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

    // ── cost estimate ─────────────────────────────────────────────────────────
    if cost_estimate {
        println!();
        println!(
            "  {}",
            "COST ESTIMATE  (input tokens · USD)".bold().underline()
        );
        let rows: Vec<Vec<String>> = stats
            .cost_rows
            .iter()
            .map(|row| {
                let m = gain::MODELS.iter().find(|m| m.name == row.model).unwrap();
                let marker = if row.reference { " ★" } else { "" };
                vec![
                    format!("{}{}", row.model, marker),
                    format!("${:.2}", m.input_per_1m),
                    format!("${:.4}", row.without_usd),
                    format!("${:.4}", row.with_usd),
                    ui::ok(&format!("${:.4}", row.saved_usd)).to_string(),
                ]
            })
            .collect();
        ui::print_table(
            &["Model", "$/1M in", "Without", "With", "Saved"],
            &rows,
            &[1, 2, 3, 4],
        );
        println!(
            "  {}",
            format!(
                "  ★ reference model · prices collected {}",
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

    // ── by tool ───────────────────────────────────────────────────────────────
    if !stats.by_tool.is_empty() {
        println!();
        println!("  {}", "BY TOOL".bold().underline());
        for (tool, count, saved) in &stats.by_tool {
            let bar = ui::bar(*saved as f64 / stats.tokens_saved.max(1) as f64, 20);
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

    // ── by command ────────────────────────────────────────────────────────────
    print_by_command(&stats.by_command, stats.tokens_saved);

    // ── per-project table ─────────────────────────────────────────────────────
    if !global.projects.is_empty() {
        const MAX_PROJECTS: usize = 20;
        println!();
        println!("  {}", "BY PROJECT".bold().underline());
        let max_saved = global
            .projects
            .iter()
            .map(|(_, s, _, _)| *s)
            .max()
            .unwrap_or(1)
            .max(1);
        let visible = global.projects.iter().take(MAX_PROJECTS);
        for (label, saved, total, intercepted) in visible {
            let bar = ui::bar(*saved as f64 / max_saved.max(1) as f64, 16);
            let pct = if *total > 0 {
                (*intercepted as f64 / *total as f64) * 100.0
            } else {
                0.0
            };
            // Show only the last two path components to keep lines compact.
            let short_label = {
                let p = std::path::Path::new(label.as_str());
                let parts: Vec<_> = p.components().collect();
                if parts.len() >= 2 {
                    let tail = parts[parts.len() - 2..]
                        .iter()
                        .map(|c| c.as_os_str().to_string_lossy())
                        .collect::<Vec<_>>()
                        .join("/");
                    tail
                } else {
                    label.clone()
                }
            };
            println!(
                "  {:<36} {} saved   {} {}",
                short_label.bold(),
                format_num(*saved).green(),
                format!("{}/{} intercepted ({:.0}%)", intercepted, total, pct).dimmed(),
                bar.bright_black()
            );
        }
        if global.projects.len() > MAX_PROJECTS {
            println!(
                "  {}",
                format!(
                    "  … +{} more projects",
                    global.projects.len() - MAX_PROJECTS
                )
                .dimmed()
            );
        }
    }

    // ── history ───────────────────────────────────────────────────────────────
    if history {
        // Show last 30 events across all projects, most recent first.
        let mut all_events: Vec<(String, store::HookEvent)> = store::list_all_project_logs()
            .into_iter()
            .flat_map(|entry| {
                let label = {
                    let p = std::path::Path::new(&entry.label);
                    p.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| entry.label.clone())
                };
                store::read_hook_log_from_path(&entry.log_path)
                    .into_iter()
                    .map(move |e| (label.clone(), e))
            })
            .collect();
        all_events.sort_by(|a, b| {
            b.1.ts
                .partial_cmp(&a.1.ts)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let show = all_events.len().min(30);
        println!();
        println!(
            "  {}",
            format!("LAST {} EVENTS  (all projects)", show)
                .bold()
                .underline()
        );
        for (proj, e) in all_events.iter().take(show) {
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
            let cmd_str = if !e.command.is_empty() {
                format!("  ({})", e.command).dimmed().to_string()
            } else if !e.reason.is_empty() {
                format!("  ({})", e.reason).dimmed().to_string()
            } else {
                String::new()
            };
            println!(
                "  {} {} {:<8} {}  {}  {} {}",
                ts.bright_black(),
                phase,
                e.tool.bold(),
                action,
                saved_str,
                format!("[{}]", proj).bright_black(),
                cmd_str
            );
        }
    }

    println!();
    Ok(())
}

// install-binary

/// Per-user global bin directory where `install-binary` places the executable:
/// `%LOCALAPPDATA%\tokenix\bin` on Windows, `~/.local/bin` on Linux/macOS.
pub fn global_bin_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|h| h.join("AppData").join("Local")))
            .map(|p| p.join("tokenix").join("bin"))
    } else {
        dirs::home_dir().map(|h| h.join(".local").join("bin"))
    }
}

/// Whether `dir` is already one of the entries on the current PATH.
pub fn dir_on_path(dir: &Path) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d == dir))
        .unwrap_or(false)
}

/// Copy the running executable into the per-user bin directory and make sure
/// that directory is reachable from PATH, so `tokenix` works from any
/// directory and any agent without a project-relative path.
fn cmd_install_binary() -> Result<()> {
    let exe = std::env::current_exe()?;
    let Some(bin_dir) = global_bin_dir() else {
        anyhow::bail!("cannot resolve the per-user bin directory (no home dir)");
    };
    let target = bin_dir.join(if cfg!(windows) {
        "tokenix.exe"
    } else {
        "tokenix"
    });

    // Copying the file onto itself fails on Windows (file in use) and is a
    // no-op anyway, so detect "already running from the install location".
    let already_there = matches!(
        (exe.canonicalize(), target.canonicalize()),
        (Ok(a), Ok(b)) if a == b
    );
    if already_there {
        println!("{} Already installed at {}", "ok".green(), target.display());
    } else {
        std::fs::create_dir_all(&bin_dir)?;
        std::fs::copy(&exe, &target).map_err(|e| {
            anyhow::anyhow!(
                "cannot copy to {} ({e}) — close any running tokenix from that location and retry",
                target.display()
            )
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&target)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&target, perms)?;
        }
        println!("{} Installed to {}", "ok".green(), target.display());
    }

    if dir_on_path(&bin_dir) {
        println!("  PATH: already configured — run `tokenix` from anywhere.");
        return Ok(());
    }

    #[cfg(windows)]
    {
        add_to_windows_user_path(&bin_dir)?;
        println!("  PATH: added for your user — restart terminals to pick it up.");
    }
    #[cfg(not(windows))]
    {
        println!("  PATH: add this line to your shell profile (~/.bashrc or ~/.zshrc):");
        println!("    export PATH=\"$HOME/.local/bin:$PATH\"");
    }
    Ok(())
}

/// Append `dir` to the user-scoped PATH via PowerShell. `setx` truncates PATH
/// at 1024 chars and downgrades REG_EXPAND_SZ; the .NET Environment API does
/// neither, so it is the safe way to persist the change.
#[cfg(windows)]
fn add_to_windows_user_path(dir: &Path) -> Result<()> {
    let dir_s = dir.display().to_string().replace('\'', "''");
    let script = format!(
        "$p = [Environment]::GetEnvironmentVariable('Path', 'User'); \
         if ($null -eq $p) {{ $p = '' }}; \
         if (($p -split ';') -notcontains '{dir_s}') {{ \
           [Environment]::SetEnvironmentVariable('Path', ($p.TrimEnd(';') + ';{dir_s}').TrimStart(';'), 'User') }}"
    );
    let status = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .status()?;
    anyhow::ensure!(status.success(), "PowerShell exited with {status}");
    Ok(())
}

// install-hook

fn cmd_install_hook(tool: Tool, local: bool) -> Result<()> {
    match tool {
        Tool::ClaudeCode => install_claude_code(local)?,
        Tool::Copilot => install_copilot(local)?,
        Tool::Codex => install_codex()?,
        Tool::Mcp => install_mcp_server()?,
        Tool::Gemini => install_copilot(local)?,
        Tool::All => {
            install_claude_code(local)?;
            install_copilot(local)?;
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

fn install_copilot(local: bool) -> Result<()> {
    // Global path: ~/.copilot/hooks/tokenix.json (user-level, picked up by VS Code for all repos).
    // Local path:  .github/hooks/hooks.json       (workspace-level, commit to share with team).
    // copilot-instructions.md is a workspace-only feature; only written for local installs.

    // `tokenix` must resolve from PATH in any committed file so it works on every clone.
    let tokenix_bin = "tokenix";
    let hook_cmd = format!("{tokenix_bin} hook");
    let hook_post_cmd = format!("{tokenix_bin} hook-post");

    let hooks_json = serde_json::json!({
        "hooks": {
            "PreToolUse": [{
                "type": "command",
                "command": hook_cmd,
                "windows": hook_cmd,
                "timeout": 10
            }],
            "PostToolUse": [{
                "type": "command",
                "command": hook_post_cmd,
                "windows": hook_post_cmd,
                "timeout": 10
            }]
        }
    });

    if local {
        let cwd = std::env::current_dir()?;
        let repo_root = store::find_project_root(&cwd);
        let github_dir = repo_root.join(".github");
        std::fs::create_dir_all(&github_dir)?;

        // copilot-instructions.md — workspace custom instructions (committed to repo)
        let instructions_path = github_dir.join("copilot-instructions.md");
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

        let hooks_dir = github_dir.join("hooks");
        std::fs::create_dir_all(&hooks_dir)?;
        let hooks_path = hooks_dir.join("hooks.json");

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

        println!("  PreToolUse:  {tokenix_bin} hook       (Read/Grep/Bash interception)");
        println!("  PostToolUse: {tokenix_bin} hook-post  (Bash/ListDirectory output compression)");
        println!("  Note: commit .github/ to enable for all contributors.");
    } else {
        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot find home directory"))?;
        let hooks_dir = home.join(".copilot").join("hooks");
        std::fs::create_dir_all(&hooks_dir)?;
        let hooks_path = hooks_dir.join("tokenix.json");

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

        println!("  PreToolUse:  {tokenix_bin} hook       (Read/Grep/Bash interception)");
        println!("  PostToolUse: {tokenix_bin} hook-post  (Bash/ListDirectory output compression)");
        println!("  Note: applies to all repos for this user (user-level hooks).");
    }

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
        Tool::Copilot => remove_copilot(local)?,
        Tool::Codex => remove_codex()?,
        Tool::Mcp => remove_mcp_server()?,
        Tool::Gemini => remove_copilot(local)?,
        Tool::All => {
            remove_claude_code(local)?;
            remove_copilot(local)?;
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
    // Use the non-vivifying helper: indexing `settings["hooks"]["PostToolUse"]`
    // with IndexMut would INSERT a `"PostToolUse": null` for a missing key, which
    // is exactly the null hook event the install path strips (Claude Code chokes
    // on null events). get_mut chains never create keys.
    remove_tokenix_hook_entries(&mut settings, "PreToolUse");
    remove_tokenix_hook_entries(&mut settings, "PostToolUse");
    std::fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;
    println!(
        "{} Claude Code hooks removed from {}",
        "ok".green(),
        settings_path.display()
    );
    Ok(())
}

fn remove_copilot(local: bool) -> Result<()> {
    if local {
        let cwd = std::env::current_dir()?;
        let repo_root = store::find_project_root(&cwd);
        let instructions = repo_root.join(".github/copilot-instructions.md");
        let hooks = repo_root.join(".github/hooks/hooks.json");
        for path in [&instructions, &hooks] {
            if path.exists() {
                std::fs::remove_file(path)?;
                println!("{} Removed {}", "ok".green(), path.display());
            }
        }
    } else {
        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot find home directory"))?;
        let hooks = home.join(".copilot/hooks/tokenix.json");
        if hooks.exists() {
            std::fs::remove_file(&hooks)?;
            println!("{} Removed {}", "ok".green(), hooks.display());
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
    ui::box_header(&format!("tokenix stats  ·  {}", repo_root.display()));
    ui::kv("id", &id);
    ui::kv("index", &db.display().to_string());
    ui::kv("files", &stats.files.to_string());
    ui::kv("chunks", &stats.chunks.to_string());
    ui::kv("tokens", &format_num(stats.total_tokens));
    ui::kv("age", &age_str);
    println!();
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
    for (file_path, tokens) in &file_counts {
        let parts: Vec<&str> = file_path.split('/').filter(|s| !s.is_empty()).collect();
        root.insert(&parts, *tokens);
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

    /// Children ordered by token weight (heaviest first) so the expensive
    /// paths surface at the top of every level instead of alphabetically.
    fn children_by_tokens(node: &TreeNode) -> Vec<&TreeNode> {
        let mut children: Vec<&TreeNode> = node.children.values().collect();
        children.sort_by_key(|c| std::cmp::Reverse(c.token_count));
        children
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

        let children = children_by_tokens(node);
        let count = children.len();
        let new_prefix = if is_last {
            format!("{}    ", prefix)
        } else {
            format!("{}│   ", prefix)
        };

        for (i, child) in children.into_iter().enumerate() {
            let is_child_last = i == count - 1;
            print_node(child, &new_prefix, is_child_last, total_tokens);
        }
    }

    println!(
        "\n{} ({} tokens)",
        root.name.bold(),
        format_num(root.token_count)
    );
    let children = children_by_tokens(&root);
    let count = children.len();
    for (i, child) in children.into_iter().enumerate() {
        let is_child_last = i == count - 1;
        print_node(child, "", is_child_last, root.token_count);
    }

    // Top files: the quickest answer to "where do my tokens go?" without
    // scanning the whole tree.
    let mut top: Vec<&(String, i64)> = file_counts.iter().collect();
    top.sort_by_key(|(_, t)| std::cmp::Reverse(*t));
    if !top.is_empty() {
        println!("\n{}", "top files by tokens".bold());
        for (path, tokens) in top.iter().take(10) {
            let pct = if root.token_count > 0 {
                *tokens as f64 / root.token_count as f64 * 100.0
            } else {
                0.0
            };
            println!(
                "  {} {:>9}  {:>5.1}%  {}",
                visual_bar(*tokens, root.token_count, 8),
                format_num(*tokens),
                pct,
                path
            );
        }
    }
    println!();

    Ok(())
}

/// The neon "tokenix" wordmark (shared by `--help`'s banner and the TUI dashboard).
pub(crate) const WORDMARK: [&str; 5] = [
    r" _        _              _      ",
    r"| |_ ___ | | _____ _ __ (_)_  __",
    r"| __/ _ \| |/ / _ \ '_ \| \ \/ /",
    r"| || (_) |   <  __/ | | | |>  < ",
    r" \__\___/|_|\_\___|_| |_|_/_/\_\",
];

/// The tagline shown under the wordmark.
pub(crate) const TAGLINE: &str = "minimize LLM context, keep the signal";

/// Logo banner: the neon "tokenix" wordmark + a one-line tagline, inspired by
/// tokenix-logo.png. Colors auto-disable on non-TTY / NO_COLOR via `colored`.
fn banner() -> String {
    let mut out = String::new();
    for line in WORDMARK {
        out.push_str(&format!("{}\n", line.bright_cyan().bold()));
    }
    out.push_str(&format!(" {}  {}", "($)".yellow(), TAGLINE));
    out
}

/// Audience-grouped command catalog + examples, rendered as clap `after-help`.
/// Commands the LLM/agent drives are separated from operator commands so each
/// reader sees the surface that matters to them.
fn help_catalog() -> String {
    // (command, args, one-line description)
    let ai: &[(&str, &str, &str)] = &[
        (
            "context",
            "<task>",
            "Build focused task context in one call",
        ),
        (
            "explore",
            "<symbol>",
            "Graph-aware related symbols + source",
        ),
        ("query", "<text>", "Semantic search over the indexed repo"),
        (
            "grep",
            "<pattern>",
            "Exact regex/literal search (no embedding)",
        ),
        ("read", "<file>", "Smart reader: outline for large files"),
        ("symbols", "<name>", "Find indexed symbols by name or path"),
        ("callers", "<symbol>", "Symbols that call the target"),
        ("callees", "<symbol>", "Symbols the target calls"),
        ("impact", "<symbol>", "Bidirectional impact graph"),
        (
            "flow",
            "<symbol>",
            "Forward execution flow from an entry point",
        ),
        ("pack", "", "Bundle focused context for hookless tools"),
        ("memory", "", "Store/list agent preference memory"),
    ];
    let human: &[(&str, &str, &str)] = &[
        ("index", "[path]", "Index a repository for semantic search"),
        (
            "install-hook",
            "",
            "Wire tokenix into Claude / Copilot / Codex",
        ),
        ("remove-hook", "", "Remove tokenix hooks"),
        ("doctor", "", "Diagnose embedding backend, GPU, daemon"),
        (
            "serve / stop",
            "",
            "Run/stop the background embedding daemon",
        ),
        ("gain", "", "Token-savings analytics (--cost-estimate)"),
        ("stats", "", "Index statistics"),
        ("tokenmap", "", "Token counts per file/folder tree"),
        (
            "benchmark",
            "",
            "Token-savings & retrieval-quality benchmark",
        ),
        ("filter", "", "Manage per-command output filters"),
        (
            "prompt-audit",
            "",
            "Audit MCP/tool prompt weight across agents",
        ),
        (
            "session-audit",
            "",
            "Token-economy risks for this session/repo",
        ),
        (
            "scan-secrets",
            "",
            "Scan AI agent conversations for exposed credentials",
        ),
        ("artifacts", "", "Manage non-code context artifacts"),
        ("cycles", "", "Detect circular dependencies"),
        ("rebuild-graph", "", "Rebuild the symbol graph from chunks"),
    ];

    let mut out = String::new();
    let render = |out: &mut String, rows: &[(&str, &str, &str)]| {
        for (cmd, args, desc) in rows {
            let invocation = if args.is_empty() {
                cmd.to_string()
            } else {
                format!("{cmd} {args}")
            };
            out.push_str(&format!(
                "  {:<26} {}\n",
                ui::accent(&invocation),
                desc.dimmed()
            ));
        }
    };

    out.push_str(&format!(
        "{}\n",
        "🤖 AI AGENT COMMANDS  (token-lean retrieval the LLM/hooks drive)"
            .bold()
            .underline()
    ));
    render(&mut out, ai);

    out.push_str(&format!(
        "\n{}\n",
        "🧑 HUMAN COMMANDS  (setup, ops & analytics you run yourself)"
            .bold()
            .underline()
    ));
    render(&mut out, human);

    out.push_str(&format!(
        "\n{} {}\n",
        "⚙  INTERNAL".bold(),
        "hook · hook-post · mcp · run  (invoked by hooks/agents, not by hand)".dimmed()
    ));

    out.push_str(&format!("\n{}\n", "EXAMPLES".bold().underline()));
    out.push_str(&format!(
        "  {}\n",
        "# AI agents — token-lean retrieval".dimmed()
    ));
    for ex in [
        "tokenix context \"add rate limiting to the API\"",
        "tokenix query \"where is JWT validated\" --budget 2000",
        "tokenix read src/auth.rs --symbol validate_token",
        "tokenix explore TokenStore",
    ] {
        out.push_str(&format!("  {}\n", ui::accent(ex)));
    }
    out.push_str(&format!("\n  {}\n", "# Humans — setup & insight".dimmed()));
    for (ex, note) in [
        ("tokenix index .", "build the index"),
        ("tokenix install-hook --tool all", "wire into your AI tools"),
        ("tokenix gain --cost-estimate", "see tokens & $ saved"),
        ("tokenix doctor", "check GPU / model / daemon"),
    ] {
        out.push_str(&format!(
            "  {:<36} {}\n",
            ui::accent(ex),
            format!("# {note}").dimmed()
        ));
    }

    out.push_str(&format!(
        "\n{}  {}   {}  {}\n",
        "Global:".bold(),
        ui::accent("--only-cpu"),
        "Details:".bold(),
        ui::accent("tokenix <command> --help")
    ));
    out
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
    fn remove_claude_hooks_keeps_foreign_and_adds_no_null_event() {
        // Regression: remove-hook used `settings["hooks"]["PostToolUse"]` (IndexMut),
        // which INSERTS `"PostToolUse": null` when the key is absent — the exact null
        // event the install path strips (Claude Code chokes on it). The fix routes
        // both events through the get_mut-based helper, mirroring remove_claude_code.
        let mut settings = serde_json::json!({
            "model": "opus",
            "hooks": {
                "PreToolUse": [
                    { "matcher": "^Edit$", "hooks": [{"type": "command", "command": "other"}] },
                    { "matcher": "^(Read|Grep|Bash)$", "hooks": [{"type": "command", "command": "\"/x/tokenix\" hook"}] }
                ]
            }
        });

        remove_tokenix_hook_entries(&mut settings, "PreToolUse");
        remove_tokenix_hook_entries(&mut settings, "PostToolUse");

        let pre = settings["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 1, "foreign hook must be preserved");
        assert!(pre[0].to_string().contains("Edit"));
        assert!(
            !settings["hooks"]
                .as_object()
                .unwrap()
                .contains_key("PostToolUse"),
            "must not create a null PostToolUse event"
        );
        assert_eq!(settings["model"], "opus", "unrelated keys preserved");
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
