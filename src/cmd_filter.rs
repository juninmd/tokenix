use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{bail, Result};
use colored::Colorize;

use crate::{filters, store};

struct CmdStats {
    base_cmd: String,
    count: usize,
    total_original: i64,
    total_saved: i64,
}

fn extract_base_command(input_preview: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(input_preview).ok()?;
    let cmd = v["tool_input"]["command"].as_str()?;
    cmd.split_whitespace().next().map(str::to_string)
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

fn collect_stats(repo_root: &PathBuf) -> Vec<CmdStats> {
    let events = store::read_hook_log(repo_root);
    let mut map: HashMap<String, CmdStats> = HashMap::new();
    for ev in events.iter().filter(|e| e.tool == "Bash" && e.phase == "post") {
        if let Some(cmd) = extract_base_command(&ev.input_preview) {
            let entry = map.entry(cmd.clone()).or_insert(CmdStats {
                base_cmd: cmd,
                count: 0,
                total_original: 0,
                total_saved: 0,
            });
            entry.count += 1;
            entry.total_original += ev.original_estimate;
            entry.total_saved += ev.saved_tokens;
        }
    }
    let mut stats: Vec<CmdStats> = map.into_values().collect();
    stats.sort_by_key(|s| -(s.total_original - s.total_saved));
    stats.truncate(20);
    stats
}

pub fn cmd_filter_list(repo_root: &PathBuf) -> Result<()> {
    let stats = collect_stats(repo_root);
    print_stats_table(&stats);
    Ok(())
}

pub fn cmd_filter_active() -> Result<()> {
    let filters = filters::load_active_filters();
    if filters.is_empty() {
        println!("{}", "No active filters found.".yellow());
        return Ok(());
    }

    println!();
    println!("{}", "ACTIVE OUTPUT FILTERS".bold().underline());
    println!(
        "  {:<28} {:<8} {:<52} {}",
        "Name", "Source", "Match command", "Description"
    );
    println!("  {}", "-".repeat(118).bright_black());

    for f in filters {
        let desc = f.filter.description.unwrap_or_default();
        println!(
            "  {:<28} {:<8} {:<52} {}",
            truncate(&f.name, 28),
            f.source,
            truncate(&f.filter.match_command, 52),
            truncate(&desc, 42)
        );
    }
    println!();
    Ok(())
}

fn print_stats_table(stats: &[CmdStats]) {
    if stats.is_empty() {
        println!("No Bash hook events found. Run some commands to populate the log.");
        return;
    }
    println!("{}", "Top Bash commands by tokens wasted:".bold());
    println!(
        "{:<4} {:<18} {:>6} {:>15} {:>13}",
        "#", "Command", "Calls", "Tokens Wasted", "Tokens Saved"
    );
    println!("{}", "-".repeat(62));
    for (i, s) in stats.iter().enumerate() {
        println!(
            "{:<4} {:<18} {:>6} {:>15} {:>13}",
            i + 1,
            s.base_cmd,
            s.count,
            format_num(s.total_original - s.total_saved),
            format_num(s.total_saved),
        );
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    format!("{}~", s.chars().take(keep).collect::<String>())
}

pub fn cmd_filter_generate(command: Option<String>, repo_root: &PathBuf) -> Result<()> {
    let base_cmd = match command {
        Some(c) => c,
        None => {
            let stats = collect_stats(repo_root);
            print_stats_table(&stats);
            if stats.is_empty() {
                return Ok(());
            }
            print!("\nSelect command to generate filter (1-{}): ", stats.len());
            io::stdout().flush()?;
            let mut line = String::new();
            io::stdin().lock().read_line(&mut line)?;
            let idx: usize = line.trim().parse().unwrap_or(0);
            if idx == 0 || idx > stats.len() {
                bail!("invalid selection");
            }
            stats[idx - 1].base_cmd.clone()
        }
    };

    // Get sample output by running `<cmd> --help`
    println!("\n{} running `{} --help` for sample output...", "→".cyan(), base_cmd);
    let sample = run_command_sample(&base_cmd);

    // Show preview and let user confirm or replace
    let sample = preview_and_confirm_sample(&base_cmd, sample)?;
    if sample.is_empty() {
        return Ok(());
    }

    // Detect available AI CLIs
    let clis = detect_ai_clis();
    if clis.is_empty() {
        bail!(
            "No AI CLI found. Install one of: claude (Claude Code), gemini, codex\n\
             Claude Code: https://claude.ai/code"
        );
    }

    let chosen_cli = if clis.len() == 1 {
        println!("Using AI CLI: {}", clis[0].0.green());
        clis[0].clone()
    } else {
        println!("\nAvailable AI CLIs:");
        for (i, (name, _)) in clis.iter().enumerate() {
            println!("  [{}] {}", i + 1, name);
        }
        print!("Select CLI (1-{}): ", clis.len());
        io::stdout().flush()?;
        let mut line = String::new();
        io::stdin().lock().read_line(&mut line)?;
        let idx: usize = line.trim().parse::<usize>().unwrap_or(1).saturating_sub(1);
        clis.get(idx).cloned().unwrap_or_else(|| clis[0].clone())
    };

    // Build and send prompt
    println!("{} asking {} to generate filter...", "→".cyan(), chosen_cli.0.green());
    let prompt = filters::build_filter_prompt(&base_cmd, &sample);
    let toml_output = invoke_ai_cli(&chosen_cli.0, &chosen_cli.1, &prompt)?;
    let toml_clean = extract_toml_from_response(&toml_output);

    // Show only the extracted TOML (not AI prose)
    println!("\n{}", "Generated filter:".bold());
    println!("{}", "─".repeat(60));
    println!("{}", toml_clean.cyan());
    println!("{}", "─".repeat(60));

    if toml::from_str::<toml::Value>(&toml_clean).is_err() {
        println!("{} TOML is invalid — edit before saving.", "⚠".yellow());
        println!("  Raw AI response saved to stderr for reference.");
        eprintln!("\n--- raw AI response ---\n{}\n---", toml_output.trim());
    }

    // Confirm save
    print!("\nSave to ~/.tokenix/filters/{}.toml? [Y/n]: ", base_cmd);
    io::stdout().flush()?;
    let mut ans = String::new();
    io::stdin().lock().read_line(&mut ans)?;
    if ans.trim().eq_ignore_ascii_case("n") {
        println!("Discarded.");
        return Ok(());
    }

    let dir = filters::filters_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.toml", base_cmd));
    std::fs::write(&path, toml_clean.trim())?;
    println!("{} Saved to {}", "✓".green(), path.display());

    // Offer PR contribution
    print!("\nContribute this filter to tokenix? [y/N]: ");
    io::stdout().flush()?;
    let mut ans = String::new();
    io::stdin().lock().read_line(&mut ans)?;
    if ans.trim().eq_ignore_ascii_case("y") {
        contribute_filter(&base_cmd, toml_clean.trim());
    }

    Ok(())
}

fn preview_and_confirm_sample(cmd: &str, sample: String) -> Result<String> {
    let preview_lines: Vec<&str> = sample.lines().take(30).collect();
    println!("\n{} (first 30 lines):", format!("Sample output for `{}`", cmd).bold());
    println!("{}", "─".repeat(60));
    for line in &preview_lines {
        println!("{}", line);
    }
    let total = sample.lines().count();
    if total > 30 {
        println!("{}", format!("  ... ({} more lines)", total - 30).dimmed());
    }
    println!("{}", "─".repeat(60));

    print!("\n[U]se this sample  [P]aste your own  [Q]uit: ");
    io::stdout().flush()?;
    let mut ans = String::new();
    io::stdin().lock().read_line(&mut ans)?;
    match ans.trim().to_lowercase().as_str() {
        "u" | "" => Ok(sample),
        "p" => {
            println!("Paste your sample output, then enter a line with just a single dot (.) to finish:");
            let mut pasted = String::new();
            let stdin = io::stdin();
            for line in stdin.lock().lines() {
                let line = line?;
                if line.trim() == "." {
                    break;
                }
                pasted.push_str(&line);
                pasted.push('\n');
            }
            Ok(pasted)
        }
        _ => Ok(String::new()),
    }
}

fn run_command_sample(cmd: &str) -> String {
    let output = if cfg!(windows) {
        Command::new("cmd")
            .args(["/C", cmd, "--help"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
    } else {
        Command::new(cmd)
            .arg("--help")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
    };

    match output {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            let stderr = String::from_utf8_lossy(&o.stderr);
            let combined = if stdout.is_empty() {
                stderr.to_string()
            } else {
                stdout.to_string()
            };
            // Cap at 150 lines
            combined
                .lines()
                .take(150)
                .collect::<Vec<_>>()
                .join("\n")
        }
        Err(_) => format!("(could not run `{} --help`)", cmd),
    }
}

/// Returns Vec of (name, invoke_flag) for detected, working AI CLIs.
fn detect_ai_clis() -> Vec<(String, String)> {
    // flag: how to pass the prompt as an argument
    let candidates = [("claude", "-p"), ("gemini", "-p"), ("codex", "-p")];
    let mut found = Vec::new();
    for (name, flag) in candidates {
        if is_cli_available(name) {
            found.push((name.to_string(), flag.to_string()));
        }
    }
    found
}

/// Probe the CLI by running `--version` — filters out stale shims in PATH.
fn is_cli_available(name: &str) -> bool {
    let ok = if cfg!(windows) {
        Command::new("cmd")
            .args(["/C", name, "--version"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
    } else {
        Command::new(name)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
    };
    ok.map(|s| s.success()).unwrap_or(false)
}

pub fn is_gh_available() -> bool {
    is_cli_available("gh")
}

fn invoke_ai_cli(name: &str, flag: &str, prompt: &str) -> Result<String> {
    // On Windows, CLIs are often .cmd/.bat wrappers — must invoke via cmd /C.
    // Rust's Command API passes args directly without shell interpretation,
    // so special chars in prompt are safe.
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", name, flag, prompt]);
        c
    } else {
        let mut c = Command::new(name);
        c.args([flag, prompt]);
        c
    };
    let child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to start {}: {}", name, e))?;

    let output = child.wait_with_output()?;
    if output.stdout.is_empty() {
        bail!("{} returned no output", name);
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Extract TOML from AI response — handles prose + fences, partial fences, bare TOML.
fn extract_toml_from_response(s: &str) -> String {
    // 1. Find ```toml...``` block anywhere (AI often wraps in markdown)
    if let Some(start) = s.find("```toml") {
        let after = &s[start + 7..];
        let body_start = after.find('\n').map(|i| i + 1).unwrap_or(0);
        let body = &after[body_start..];
        let end = body.find("```").unwrap_or(body.len());
        return body[..end].trim().to_string();
    }

    // 2. Find ``` block that contains a [filters. section
    if let Some(start) = s.find("```\n") {
        let after = &s[start + 4..];
        let end = after.find("```").unwrap_or(after.len());
        let candidate = after[..end].trim().to_string();
        if candidate.contains("[filters.") {
            return candidate;
        }
    }

    // 3. Find bare [filters. section — skip any leading prose
    if let Some(start) = s.find("[filters.") {
        return s[start..].trim().to_string();
    }

    s.trim().to_string()
}

fn contribute_filter(cmd: &str, toml_content: &str) {
    if !is_gh_available() {
        println!("{} gh CLI not found — manual steps:", "⚠".yellow());
        print_contribution_instructions(cmd, toml_content);
        return;
    }
    if let Err(e) = create_pr(cmd, toml_content) {
        println!("{} PR failed: {} — manual steps:", "⚠".yellow(), e);
        print_contribution_instructions(cmd, toml_content);
    }
}

fn create_pr(cmd: &str, toml_content: &str) -> Result<()> {
    let tmp = std::env::temp_dir().join(format!("tokenix-filter-{}", cmd));
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp)?;
    }
    std::fs::create_dir_all(&tmp)?;

    println!("{} forking juninmd/tokenix...", "→".cyan());
    gh_run(&["repo", "fork", "juninmd/tokenix", "--clone"], &tmp)?;

    let repo = tmp.join("tokenix");
    let branch = format!("filter-{}", cmd);

    git_run(&["-C", repo.to_str().unwrap(), "checkout", "-b", &branch])?;

    let filters_dir = repo.join("filters");
    std::fs::create_dir_all(&filters_dir)?;
    std::fs::write(filters_dir.join(format!("{}.toml", cmd)), toml_content)?;

    git_run(&["-C", repo.to_str().unwrap(), "add", &format!("filters/{}.toml", cmd)])?;
    git_run(&[
        "-C", repo.to_str().unwrap(),
        "commit", "-m", &format!("filter: add {} filter", cmd),
    ])?;
    git_run(&["-C", repo.to_str().unwrap(), "push", "origin", &branch])?;

    println!("{} creating PR...", "→".cyan());
    let title = format!("filter: add {} filter", cmd);
    let body = format!(
        "New community filter for `{cmd}`.\n\nGenerated by `tokenix filter generate {cmd}`.\n\n```toml\n{toml_content}\n```\n"
    );
    gh_run(&[
        "pr", "create",
        "--repo", "juninmd/tokenix",
        "--title", &title,
        "--body", &body,
        "--base", "main",
        "--head", &branch,
    ], &repo)?;

    println!("{} PR created at github.com/juninmd/tokenix/pulls", "✓".green());
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(())
}

/// Run a `gh` subcommand, optionally in a working directory.
fn gh_run(args: &[&str], cwd: &std::path::Path) -> Result<()> {
    let ok = if cfg!(windows) {
        let mut full = vec!["/C", "gh"];
        full.extend_from_slice(args);
        Command::new("cmd").args(&full).current_dir(cwd).status()?
    } else {
        Command::new("gh").args(args).current_dir(cwd).status()?
    };
    if ok.success() { Ok(()) } else { bail!("gh {:?} failed", args) }
}

/// Run a `git` subcommand (no working-dir needed; uses -C flag instead).
fn git_run(args: &[&str]) -> Result<()> {
    let ok = if cfg!(windows) {
        let mut full = vec!["/C", "git"];
        full.extend_from_slice(args);
        Command::new("cmd").args(&full).status()?
    } else {
        Command::new("git").args(args).status()?
    };
    if ok.success() { Ok(()) } else { bail!("git {:?} failed", args) }
}

fn print_contribution_instructions(cmd: &str, toml_content: &str) {
    println!("  1. Fork https://github.com/juninmd/tokenix");
    println!("  2. Create file: filters/{}.toml", cmd);
    println!("{}", "─".repeat(60));
    println!("{}", toml_content);
    println!("{}", "─".repeat(60));
    println!("  3. PR title: \"filter: add {} filter\"", cmd);
}
