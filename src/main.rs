mod chunker;
mod embed;
mod gain;
mod hook;
mod indexer;
mod query;
mod store;

use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::Colorize;
use std::path::PathBuf;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const OLLAMA_URL: &str = "http://localhost:11434";

#[derive(Parser)]
#[command(name = "tokenix", version = VERSION, about = "Local semantic index for LLM token optimization")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Index a repository for semantic search
    Index {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(short, long, default_value = "nomic-embed-text")]
        model: String,
        #[arg(short, long, help = "Force reindex all files")]
        force: bool,
    },
    /// Semantic search over the indexed repository
    Query {
        text: String,
        #[arg(short, long, default_value_t = 3000)]
        budget: usize,
        #[arg(long, default_value_t = 20)]
        k: usize,
        #[arg(short, long, default_value = "nomic-embed-text")]
        model: String,
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
    /// Install PreToolUse hook in Claude Code settings
    InstallHook {
        #[arg(long = "local", help = "Install in local .claude/settings.json instead of global")]
        local: bool,
    },
    /// Remove tokenix hook from Claude Code settings
    RemoveHook {
        #[arg(long = "local")]
        local: bool,
    },
    /// Show index statistics
    Stats {
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// PreToolUse hook handler (called by Claude Code, not for direct use)
    Hook,
}

fn find_repo_root(start: &PathBuf) -> PathBuf {
    let abs = start.canonicalize().unwrap_or_else(|_| start.clone());
    let mut current = abs.as_path();
    loop {
        if current.join(".tokenix").exists() {
            return current.to_path_buf();
        }
        match current.parent() {
            Some(p) => current = p,
            None => return abs,
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Index { path, model, force } => cmd_index(&path, &model, force),
        Commands::Query { text, budget, k, model, file, path } => {
            cmd_query(&text, budget, k, &model, file.as_deref(), &path)
        }
        Commands::Read { file, symbol, lines, path } => {
            cmd_read(&file, symbol.as_deref(), lines.as_deref(), &path)
        }
        Commands::Gain { path, history } => cmd_gain(&path, history),
        Commands::InstallHook { local } => cmd_install_hook(local),
        Commands::RemoveHook { local } => cmd_remove_hook(local),
        Commands::Stats { path } => cmd_stats(&path),
        Commands::Hook => hook::run_hook(),
    }
}

fn cmd_index(path: &PathBuf, model: &str, force: bool) -> Result<()> {
    let repo_root = path.canonicalize().unwrap_or_else(|_| path.clone());
    println!("{} indexing {}", "tokenix".bold(), repo_root.display().to_string().cyan());

    embed::check_ollama(model, OLLAMA_URL)?;

    let start = std::time::Instant::now();
    let (result, stats) = indexer::index_repo(&repo_root, model, force, |msg| {
        println!("  {}", msg);
    })?;

    println!("\n{} in {:.1}s", "Done".green().bold(), start.elapsed().as_secs_f64());
    println!("  Files: {} indexed, {} skipped, {} errors", result.indexed, result.skipped, result.errors);
    println!("  Index: {} chunks, {} tokens stored", stats.chunks, format_num(stats.total_tokens));
    Ok(())
}

fn cmd_query(text: &str, budget: usize, k: usize, model: &str, file: Option<&str>, path: &PathBuf) -> Result<()> {
    let repo_root = find_repo_root(path);
    let results = query::query_index(&repo_root, text, budget, k, model, file)?
        .ok_or_else(|| anyhow::anyhow!("Index not found. Run: tokenix index"))?;
    println!("{}", query::format_results(&results, text));
    Ok(())
}

fn cmd_read(file: &str, symbol: Option<&str>, lines_range: Option<&str>, path: &PathBuf) -> Result<()> {
    let repo_root = find_repo_root(path);
    let fp = {
        let p = std::path::Path::new(file);
        if p.exists() { p.to_path_buf() } else { repo_root.join(file) }
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

    let rel = fp.strip_prefix(&repo_root).unwrap_or(&fp).to_string_lossy().replace('\\', "/");

    if let Some(sym) = symbol {
        let chunks = chunker::chunk_file(&rel, &content);
        let found: Vec<_> = chunks.iter().filter(|c| c.symbol.to_lowercase().contains(&sym.to_lowercase())).collect();
        if found.is_empty() {
            eprintln!("{} '{}'", "Symbol not found:".yellow(), sym);
            std::process::exit(1);
        }
        for c in found {
            println!("# L{}-{} [{}] {}", c.start_line, c.end_line, c.kind, c.symbol);
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
    println!("  Intercepted        {}", stats.intercepted.to_string().green());
    println!("  Passed through     {}", stats.passed);
    println!("  Tokens saved       {}", format_num(stats.tokens_saved).green().bold());
    println!("  Tokens used        {}", format_num(stats.tokens_used));
    println!("  Reduction          {:.1}%", stats.pct_saved);
    println!("  Cost saved (est.)  ${:.4}", stats.cost_saved_usd);

    if !stats.by_tool.is_empty() {
        println!("\n{}", "By tool:".bold());
        for (tool, count, saved) in &stats.by_tool {
            println!("  {}: {} calls, {} tokens saved", tool, count, format_num(*saved));
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

fn cmd_install_hook(local: bool) -> Result<()> {
    let settings_path = if local {
        std::env::current_dir()?.join(".claude").join("settings.json")
    } else {
        dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Cannot find home directory"))?
            .join(".claude")
            .join("settings.json")
    };

    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let tokenix_cmd = std::env::current_exe()?
        .to_string_lossy()
        .replace('\\', "/")
        + " hook";

    let mut settings: serde_json::Value = if settings_path.exists() {
        let raw = std::fs::read_to_string(&settings_path)?;
        serde_json::from_str(&raw).unwrap_or(serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    let already = settings["hooks"]["PreToolUse"]
        .as_array()
        .map(|arr| arr.iter().any(|h| h.to_string().contains("tokenix")))
        .unwrap_or(false);

    if already {
        println!("{}", "tokenix hook already installed.".yellow());
        return Ok(());
    }

    let new_hook = serde_json::json!({
        "matcher": "Read|Grep",
        "hooks": [{"type": "command", "command": tokenix_cmd}]
    });

    if settings["hooks"]["PreToolUse"].is_array() {
        settings["hooks"]["PreToolUse"].as_array_mut().unwrap().push(new_hook);
    } else {
        settings["hooks"]["PreToolUse"] = serde_json::json!([new_hook]);
    }

    std::fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;
    println!("{} in {}", "Hook installed".green(), settings_path.display());
    println!("Command: {}", tokenix_cmd);
    println!("\nNext steps:");
    println!("  1. cd <your-project>");
    println!("  2. tokenix index .");
    println!("  3. Start a Claude Code session -- tokenix intercepts large reads automatically.");
    Ok(())
}

fn cmd_remove_hook(local: bool) -> Result<()> {
    let settings_path = if local {
        std::env::current_dir()?.join(".claude").join("settings.json")
    } else {
        dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Cannot find home directory"))?
            .join(".claude")
            .join("settings.json")
    };

    if !settings_path.exists() {
        println!("{}", "Settings file not found.".yellow());
        return Ok(());
    }

    let raw = std::fs::read_to_string(&settings_path)?;
    let mut settings: serde_json::Value = serde_json::from_str(&raw)?;

    if let Some(arr) = settings["hooks"]["PreToolUse"].as_array_mut() {
        arr.retain(|h| !h.to_string().contains("tokenix"));
    }

    std::fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;
    println!("{} from {}", "Hook removed".green(), settings_path.display());
    Ok(())
}

fn cmd_stats(path: &PathBuf) -> Result<()> {
    let repo_root = find_repo_root(path);
    let conn = store::open_db(&repo_root, false)?
        .ok_or_else(|| anyhow::anyhow!("No index found. Run: tokenix index"))?;
    let stats = store::count_stats(&conn)?;
    let age = store::get_index_age(&repo_root);
    let age_str = age.map(|a| format!("{}s ago", a as i64)).unwrap_or_else(|| "unknown".to_string());

    println!("\n{} {}", "Index:".bold(), repo_root.join(".tokenix/index.db").display());
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
        if i > 0 && i % 3 == 0 { result.push(','); }
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
