use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{bail, Result};
use colored::Colorize;

use crate::ui::{self, box_header, format_num};
use crate::{filters, recordings, store};

struct CmdStats {
    base_cmd: String,
    count: usize,
    total_original: i64,
    total_saved: i64,
    unique_commands: HashMap<String, (usize, i64)>,
}

/// Base command for a Bash event. Prefers the stored `command` field (set at
/// log time); falls back to parsing legacy logs' input_preview, which only works
/// when the preview wasn't truncated before `tool_input.command`.
fn base_command(ev: &store::HookEvent) -> Option<String> {
    if !ev.command.is_empty() {
        return ev.command.split_whitespace().next().map(str::to_string);
    }
    extract_base_command(&ev.input_preview)
}

fn extract_base_command(input_preview: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(input_preview).ok()?;
    let cmd = v["tool_input"]["command"].as_str()?;
    cmd.split_whitespace().next().map(str::to_string)
}

fn is_filter_savings_event(e: &store::HookEvent) -> bool {
    matches!(e.tool.as_str(), "Bash" | "PowerShell")
        && matches!(e.phase.as_str(), "post" | "ToolOutputCompressed")
}

/// Reject command names that aren't plain executable identifiers. `base_cmd`
/// reaches `cmd /C <base_cmd> --help` (Windows shell metacharacter injection),
/// a `<base_cmd>.toml` filename (path traversal), and a git branch / PR head.
/// Allow only a leading alphanumeric followed by `[A-Za-z0-9._-]` — no spaces,
/// no `& | ; > < $ ` ( ) `, no path separators.
fn validate_command_name(cmd: &str) -> Result<()> {
    let mut chars = cmd.chars();
    let valid = matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && cmd.len() <= 64;
    if !valid {
        bail!(
            "refusing unsafe command name {cmd:?}: only [A-Za-z0-9._-] (≤64 chars, \
             alphanumeric start) are allowed for filter generation"
        );
    }
    Ok(())
}

fn collect_stats(repo_root: &Path) -> Vec<CmdStats> {
    let events = store::read_hook_log(repo_root);
    let mut map: HashMap<String, CmdStats> = HashMap::new();
    for ev in events.iter().filter(|e| is_filter_savings_event(e)) {
        if let Some(base) = base_command(ev) {
            let entry = map.entry(base.clone()).or_insert(CmdStats {
                base_cmd: base,
                count: 0,
                total_original: 0,
                total_saved: 0,
                unique_commands: HashMap::new(),
            });
            entry.count += 1;
            entry.total_original += ev.original_estimate;
            entry.total_saved += ev.saved_tokens;

            let full_cmd = if !ev.command.is_empty() {
                ev.command.clone()
            } else {
                "(unknown arguments)".to_string()
            };

            let cmd_entry = entry.unique_commands.entry(full_cmd).or_insert((0, 0));
            cmd_entry.0 += 1;
            cmd_entry.1 += ev.original_estimate - ev.saved_tokens;
        }
    }
    let mut stats: Vec<CmdStats> = map.into_values().collect();
    stats.sort_by_key(|s| -(s.total_original - s.total_saved));
    stats.truncate(20);
    stats
}

pub fn cmd_filter_list(index: Option<usize>, repo_root: &Path) -> Result<()> {
    let stats = collect_stats(repo_root);

    if let Some(idx) = index {
        if idx == 0 || idx > stats.len() {
            bail!(
                "invalid index {} — choose between 1 and {}",
                idx,
                stats.len()
            );
        }
        let s = &stats[idx - 1];
        box_header(&format!("filter · {}", s.base_cmd));
        println!(
            "  wasted {}   ·   {} calls",
            format_num(s.total_original - s.total_saved).red().bold(),
            s.count
        );
        println!("\n  {}", "Top unique invocations:".bold());
        let mut unique: Vec<_> = s.unique_commands.iter().collect();
        unique.sort_by_key(|(_, stats)| -stats.1);
        let rows: Vec<Vec<String>> = unique
            .iter()
            .take(10)
            .map(|(cmd, (count, wasted))| {
                vec![
                    count.to_string(),
                    format_num(*wasted),
                    ui::accent(&truncate(cmd, 56)).to_string(),
                ]
            })
            .collect();
        ui::print_table(&["Calls", "Wasted", "Command"], &rows, &[0, 1]);

        println!(
            "\n  {} build a filter from these:  {}",
            "→".cyan(),
            format!("tokenix filter generate {}", s.base_cmd).green()
        );
        println!();
        return Ok(());
    }

    print_stats_table(&stats);
    println!(
        "  {} drill into one:  {}",
        "→".cyan(),
        "tokenix filter list <#>".green()
    );
    Ok(())
}

pub fn cmd_filter_active() -> Result<()> {
    let filters = filters::load_active_filters();
    if filters.is_empty() {
        println!("{}", "No active filters found.".yellow());
        return Ok(());
    }

    box_header("filter · active output filters");
    let rows: Vec<Vec<String>> = filters
        .into_iter()
        .map(|f| {
            let desc = f.filter.description.unwrap_or_default();
            vec![
                truncate(&f.name, 28),
                f.source.to_string(),
                truncate(&f.filter.match_command, 52),
                truncate(&desc, 42),
            ]
        })
        .collect();
    ui::print_table(
        &["Name", "Source", "Match command", "Description"],
        &rows,
        &[],
    );
    println!();
    Ok(())
}

fn print_stats_table(stats: &[CmdStats]) {
    if stats.is_empty() {
        println!("No Bash hook events found. Run some commands to populate the log.");
        return;
    }
    box_header("filter · commands by tokens wasted");
    let rows: Vec<Vec<String>> = stats
        .iter()
        .enumerate()
        .map(|(i, s)| {
            vec![
                (i + 1).to_string(),
                s.base_cmd.clone(),
                s.count.to_string(),
                format_num(s.total_original - s.total_saved),
                format_num(s.total_saved),
            ]
        })
        .collect();
    ui::print_table(
        &["#", "Command", "Calls", "Tokens Wasted", "Tokens Saved"],
        &rows,
        &[0, 2, 3, 4],
    );
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    format!("{}~", s.chars().take(keep).collect::<String>())
}

fn human_bytes(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    if n >= MB {
        format!("{:.1} MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.1} KB", n as f64 / KB as f64)
    } else {
        format!("{} B", n)
    }
}

pub fn cmd_filter_generate(command: Option<String>, repo_root: &Path) -> Result<()> {
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
            let line = line.trim();
            if line.is_empty() {
                bail!(
                    "Interactive selection unavailable. Specify the command:\n\
                     tokenix filter generate <command>\n\
                     Example: tokenix filter generate cargo"
                );
            }
            let idx: usize = line.parse().unwrap_or(0);
            if idx == 0 || idx > stats.len() {
                bail!("invalid selection");
            }
            stats[idx - 1].base_cmd.clone()
        }
    };

    // Security gate: base_cmd reaches a shell, a filename, and git/PR args.
    validate_command_name(&base_cmd)?;

    box_header(&format!("filter generate · {}", base_cmd));

    // Prefer real outputs captured during a `tokenix filter record` session —
    // they give the AI diverse, realistic noise instead of a single re-run.
    let sample =
        if let Some((recorded, used)) = recordings::read_samples(repo_root, &base_cmd, 64 * 1024) {
            println!(
                "  {} using {} recorded sample(s) from `tokenix filter record`",
                "→".cyan(),
                used
            );
            recorded
        } else {
            // No recordings — re-run the latest real invocation from the log, or the
            // base command as-is (NEVER `--help`; it has the wrong noise profile).
            let full_cmd_to_run = find_latest_unfiltered_command(repo_root, &base_cmd)
                .unwrap_or_else(|| base_cmd.clone());
            println!(
                "  {} running `{}` for sample output...",
                "→".cyan(),
                full_cmd_to_run
            );
            run_command_sample(&full_cmd_to_run)
        };

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
    println!(
        "{} asking {} to generate filter...",
        "→".cyan(),
        chosen_cli.0.green()
    );
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
    println!(
        "\n{} (first 30 lines):",
        format!("Sample output for `{}`", cmd).bold()
    );
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
            println!(
                "Paste your sample output, then enter a line with just a single dot (.) to finish:"
            );
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

fn find_latest_unfiltered_command(repo_root: &Path, base_cmd: &str) -> Option<String> {
    let log_path = repo_root.join(".tokenix").join("unfiltered_cmds.log");
    if !log_path.exists() {
        return None;
    }

    let content = std::fs::read_to_string(&log_path).ok()?;
    let mut latest_match = None;

    for line in content.lines() {
        let trimmed = line.trim();
        // Match exact command or command with arguments
        if trimmed == base_cmd || trimmed.starts_with(&format!("{} ", base_cmd)) {
            latest_match = Some(trimmed.to_string());
        }
    }

    latest_match
}

fn run_command_sample(full_cmd: &str) -> String {
    let output = if cfg!(windows) {
        Command::new("cmd")
            .args(["/C", full_cmd])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
    } else {
        Command::new("sh")
            .args(["-c", full_cmd])
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
            combined.lines().take(150).collect::<Vec<_>>().join("\n")
        }
        Err(_) => format!("(could not run `{}`)", full_cmd),
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

    git_run(&[
        "-C",
        repo.to_str().unwrap(),
        "add",
        &format!("filters/{}.toml", cmd),
    ])?;
    git_run(&[
        "-C",
        repo.to_str().unwrap(),
        "commit",
        "-m",
        &format!("filter: add {} filter", cmd),
    ])?;
    git_run(&["-C", repo.to_str().unwrap(), "push", "origin", &branch])?;

    println!("{} creating PR...", "→".cyan());
    let title = format!("filter: add {} filter", cmd);
    let body = format!(
        "New community filter for `{cmd}`.\n\nGenerated by `tokenix filter generate {cmd}`.\n\n```toml\n{toml_content}\n```\n"
    );
    gh_run(
        &[
            "pr",
            "create",
            "--repo",
            "juninmd/tokenix",
            "--title",
            &title,
            "--body",
            &body,
            "--base",
            "main",
            "--head",
            &branch,
        ],
        &repo,
    )?;

    println!(
        "{} PR created at github.com/juninmd/tokenix/pulls",
        "✓".green()
    );
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
    if ok.success() {
        Ok(())
    } else {
        bail!("gh {:?} failed", args)
    }
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
    if ok.success() {
        Ok(())
    } else {
        bail!("git {:?} failed", args)
    }
}

fn print_contribution_instructions(cmd: &str, toml_content: &str) {
    println!("  1. Fork https://github.com/juninmd/tokenix");
    println!("  2. Create file: filters/{}.toml", cmd);
    println!("{}", "─".repeat(60));
    println!("{}", toml_content);
    println!("{}", "─".repeat(60));
    println!("  3. PR title: \"filter: add {} filter\"", cmd);
}

pub fn cmd_filter_record_start(command: Option<String>, repo_root: &Path) -> Result<()> {
    if recordings::is_active(repo_root) {
        println!(
            "{} A recording session is already running. Stop it first with \
             `tokenix filter record stop`.",
            "⚠".yellow()
        );
        return Ok(());
    }
    if let Some(c) = &command {
        validate_command_name(c)?;
    }
    let session = recordings::start(repo_root, command)?;
    let scope = match &session.command {
        Some(c) => format!("`{}`", c).bold().to_string(),
        None => "all commands".bold().to_string(),
    };

    box_header("filter record · started");
    println!("  {} capturing {} output", "●".green(), scope);
    println!("  {} {}", "into".dimmed(), ".tokenix/recordings".cyan());
    println!("\n  Run your commands as usual, then finish with:");
    println!("    {}", "tokenix filter record stop".green());
    println!();
    Ok(())
}

pub fn cmd_filter_record_stop(repo_root: &Path) -> Result<()> {
    if !recordings::is_active(repo_root) {
        println!("{} No active recording session.", "⚠".yellow());
        return Ok(());
    }
    recordings::stop(repo_root)?;
    let summary = recordings::summary(repo_root);

    box_header("filter record · stopped");
    if summary.is_empty() {
        println!("  {} No command output was captured.", "⚠".yellow());
        println!(
            "  {}",
            "Did the recorded commands produce output? Is the hook installed?".dimmed()
        );
        println!();
        return Ok(());
    }

    println!(
        "  {:<20} {:>9} {:>12}",
        "Command".bold(),
        "Captures".bold(),
        "Size".bold()
    );
    println!("  {}", "─".repeat(43).bright_black());
    for (cmd, count, bytes) in &summary {
        println!(
            "  {:<20} {:>9} {:>12}",
            truncate(cmd, 20),
            count,
            human_bytes(*bytes)
        );
    }

    println!(
        "\n  {} turn these into a filter:  {}",
        "→".cyan(),
        format!("tokenix filter generate {}", summary[0].0).green()
    );
    println!();
    Ok(())
}

pub fn cmd_filter_record_status(repo_root: &Path) -> Result<()> {
    match recordings::active_session(repo_root) {
        Some(s) => {
            let scope = match &s.command {
                Some(c) => format!("`{}`", c).bold().to_string(),
                None => "all commands".bold().to_string(),
            };
            box_header("filter record · active");
            println!("  {} recording {}", "●".green(), scope);
        }
        None => {
            box_header("filter record · idle");
            println!(
                "  {} no active session — start one with `tokenix filter record start`",
                "○".yellow()
            );
        }
    }

    let summary = recordings::summary(repo_root);
    if !summary.is_empty() {
        println!("\n  {}", "Captured so far:".bold());
        for (cmd, count, bytes) in &summary {
            println!(
                "    {:<18} {:>4} captures  {:>10}",
                truncate(cmd, 18),
                count,
                human_bytes(*bytes)
            );
        }
    }
    println!();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_command_name_accepts_real_commands() {
        for ok in [
            "cargo",
            "npm",
            "git",
            "uv",
            "docker-compose",
            "go.test",
            "a",
        ] {
            assert!(validate_command_name(ok).is_ok(), "{ok} should be allowed");
        }
    }

    fn ev(command: &str, input_preview: &str) -> store::HookEvent {
        store::HookEvent {
            ts: 0.0,
            tool: "Bash".to_string(),
            action: "intercepted".to_string(),
            reason: String::new(),
            saved_tokens: 0,
            actual_tokens: 0,
            original_estimate: 0,
            input_preview: input_preview.to_string(),
            phase: "post".to_string(),
            command: command.to_string(),
        }
    }

    #[test]
    fn base_command_prefers_stored_command_field() {
        // The real Claude payload front-loads session_id/cwd, so input_preview is
        // truncated before tool_input.command — the stored field must win.
        let truncated = r#"{"session_id":"abc","transcript_path":"x","cwd":"y","#;
        assert_eq!(
            base_command(&ev("cargo build --release", truncated)),
            Some("cargo".to_string())
        );
    }

    #[test]
    fn base_command_falls_back_to_legacy_preview() {
        let legacy = r#"{"tool_input":{"command":"git status"}}"#;
        assert_eq!(base_command(&ev("", legacy)), Some("git".to_string()));
    }

    #[test]
    fn filter_stats_accept_current_power_shell_compression_events() {
        let mut current = ev("git status", "");
        current.tool = "PowerShell".to_string();
        current.phase = "ToolOutputCompressed".to_string();
        assert!(is_filter_savings_event(&current));

        let mut rewrite_marker = ev("git status", "");
        rewrite_marker.phase = "pre".to_string();
        assert!(!is_filter_savings_event(&rewrite_marker));
    }

    #[test]
    fn validate_command_name_rejects_injection_and_traversal() {
        for bad in [
            "cargo & calc",     // command chaining
            "foo|bar",          // pipe
            "rm;ls",            // semicolon
            "$(whoami)",        // substitution
            "../../etc/passwd", // path traversal
            "a/b",              // path separator
            "-rf",              // leading dash (flag injection)
            "",                 // empty
            "foo bar",          // space
        ] {
            assert!(
                validate_command_name(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }
}
