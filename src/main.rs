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

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use colored::Colorize;
use std::path::PathBuf;

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
        #[arg(long, help = "TCP port to listen on (default: 47392 or $TOKENIX_DAEMON_PORT)")]
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
}

#[derive(Subcommand)]
enum FilterAction {
    /// List top Bash commands by tokens wasted (no custom filter yet)
    List,
    /// Generate a TOML filter for a command using an AI CLI
    Generate {
        /// Base command name (e.g. cargo, git). Omit to select from the list.
        command: Option<String>,
    },
}

fn find_repo_root(start: &PathBuf) -> PathBuf {
    let abs = start.canonicalize().unwrap_or_else(|_| start.clone());
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
        Commands::Index { path, force, if_stale } => cmd_index(&path, force, if_stale),
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
        Commands::InstallHook { tool, local } => cmd_install_hook(tool, local),
        Commands::RemoveHook { tool, local } => cmd_remove_hook(tool, local),
        Commands::Stats { path } => cmd_stats(&path),
        Commands::Serve { port } => daemon::run_serve(port),
        Commands::Stop => daemon::run_stop(),
        Commands::Filter { action } => {
            let repo_root = find_repo_root(&PathBuf::from("."));
            match action {
                FilterAction::List => cmd_filter::cmd_filter_list(&repo_root),
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
            unsafe { std::env::set_var("RAYON_NUM_THREADS", "1") };
            compress::run_hook_post()
        }
    }
}

fn cmd_index(path: &PathBuf, force: bool, if_stale: bool) -> Result<()> {
    let repo_root = path.canonicalize().unwrap_or_else(|_| path.clone());

    if if_stale && !force {
        let age = store::get_index_age(&repo_root);
        let stale = age.map(|a| a > hook::MAX_INDEX_AGE_SECS).unwrap_or(true);
        if !stale {
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

fn cmd_query(
    text: &str,
    budget: usize,
    k: usize,
    file: Option<&str>,
    path: &PathBuf,
) -> Result<()> {
    let repo_root = find_repo_root(path);
    let results = query::query_index(&repo_root, text, budget, k, file)?
        .ok_or_else(|| anyhow::anyhow!("Index not found. Run: tokenix index"))?;
    println!("{}", query::format_results(&results, text));
    Ok(())
}

fn cmd_read(
    file: &str,
    symbol: Option<&str>,
    lines_range: Option<&str>,
    path: &PathBuf,
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

fn cmd_gain(path: &PathBuf, history: bool) -> Result<()> {
    let repo_root = find_repo_root(path);
    let stats = gain::compute_gain(&repo_root);

    println!("\n{} -- {}\n", "tokenix gain".bold(), repo_root.display());
    println!("  Total hook calls   {}", stats.total_calls);
    println!(
        "  Intercepted        {}",
        stats.intercepted.to_string().green()
    );
    println!("  Passed through     {}", stats.passed);
    println!(
        "  Tokens saved       {}",
        format_num(stats.tokens_saved).green().bold()
    );
    println!("  Tokens used        {}", format_num(stats.tokens_used));
    println!("  Reduction          {:.1}%", stats.pct_saved);
    println!("  Cost saved (est.)  ${:.4}", stats.cost_saved_usd);

    if !stats.by_tool.is_empty() {
        println!("\n{}", "By tool:".bold());
        for (tool, count, saved) in &stats.by_tool {
            println!(
                "  {}: {} calls, {} tokens saved",
                tool,
                count,
                format_num(*saved)
            );
        }
    }

    if stats.by_phase.len() > 1 {
        println!("\n{}", "By phase:".bold());
        for (phase, count, saved) in &stats.by_phase {
            let label = match phase.as_str() {
                "pre" => "PreToolUse  (Read/Grep intercepts)",
                "post" => "PostToolUse (Bash/ListDirectory compression)",
                _ => phase.as_str(),
            };
            println!("  {}: {} calls, {} tokens saved", label, count, format_num(*saved));
        }
    }

    if history {
        let events = store::read_hook_log(&repo_root);
        let total = events.len();
        println!("\n{}", format!("Last {} events:", total.min(20)).bold());
        for e in events.iter().rev().take(20) {
            let ts = format_ts(e.ts);
            let action = if e.action == "intercepted" {
                "intercepted".green().to_string()
            } else {
                "pass      ".dimmed().to_string()
            };
            println!("  {} {:5} {} saved={}", ts, e.tool, action, e.saved_tokens);
        }
    }
    Ok(())
}

// install-hook

fn cmd_install_hook(tool: Tool, local: bool) -> Result<()> {
    match tool {
        Tool::ClaudeCode => install_claude_code(local)?,
        Tool::Copilot => install_copilot()?,
        Tool::Codex => install_codex()?,
        Tool::All => {
            install_claude_code(local)?;
            install_copilot()?;
            install_codex()?;
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

    let pre_already = settings["hooks"]["PreToolUse"]
        .as_array()
        .map(|arr| arr.iter().any(|h| h.to_string().contains("tokenix")))
        .unwrap_or(false);

    let post_already = settings["hooks"]["PostToolUse"]
        .as_array()
        .map(|arr| arr.iter().any(|h| h.to_string().contains("tokenix")))
        .unwrap_or(false);

    if pre_already && post_already {
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
    Ok(())
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

    println!("  To activate shell helpers:");
    println!("    bash/zsh:   echo 'source ~/.codex/tokenix-init.sh' >> ~/.bashrc");
    println!("    PowerShell: echo '. ~/.codex/tokenix-init.ps1' >> $PROFILE");
    Ok(())
}

// remove-hook

fn cmd_remove_hook(tool: Tool, local: bool) -> Result<()> {
    match tool {
        Tool::ClaudeCode => remove_claude_code(local)?,
        Tool::Copilot => remove_copilot()?,
        Tool::Codex => remove_codex()?,
        Tool::All => {
            remove_claude_code(local)?;
            remove_copilot()?;
            remove_codex()?;
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
    for helper in ["tokenix-init.sh", "tokenix-init.ps1"] {
        let p = home.join(".codex").join(helper);
        if p.exists() {
            std::fs::remove_file(&p)?;
            println!("{} Removed {}", "ok".green(), p.display());
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

fn cmd_stats(path: &PathBuf) -> Result<()> {
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
