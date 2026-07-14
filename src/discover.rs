//! `tokenix discover` — find missed savings in agent transcript history.
//!
//! Scans on-disk agent transcripts (Claude / Codex / Copilot / OpenAI) for
//! shell commands and their recorded outputs, then replays the CURRENT filter
//! set over each output. Unlike estimate tables, savings here are *measured*:
//! `find_filter` + `apply_filter` run over the exact historical output.
//!
//! Three outcomes per command:
//! - a filter matches and the replay shrinks the output → those tokens were
//!   wasted (the hook was missing/disabled when the session ran) and are
//!   recoverable by installing/repairing the hook;
//! - a filter matches but the replay saves ~nothing → the output was already
//!   lean (hook likely active), nothing to do;
//! - no filter matches → ranked candidate for `tokenix filter generate`.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use anyhow::Result;
use colored::Colorize;
use serde::Serialize;
use serde_json::Value;

use crate::chunker::count_tokens;
use crate::conversation_audit::Agent;
use crate::filters;
use crate::ui::{self, box_header, format_num};

pub struct Options {
    pub agent: Agent,
    pub top: usize,
    pub json: bool,
    /// Only scan transcripts modified in the last N days (0 = no limit).
    pub since_days: u64,
}

/// Transcript files above this size are skipped (and counted) — histories can
/// hold multi-GB workflow logs, and a bounded scan beats a surprise 10-minute
/// run. Rerun with `--since-days` narrowed instead of raising this.
const MAX_FILE_BYTES: u64 = 25 * 1024 * 1024;

#[derive(Clone, Serialize)]
struct Bucket {
    label: String,
    filter: Option<String>,
    agents: BTreeSet<String>,
    calls: usize,
    output_tokens: i64,
    replay_saved: i64,
    sample: String,
}

#[derive(Serialize)]
struct Report {
    scanned_files: usize,
    skipped_files: usize,
    commands_seen: usize,
    already_lean: usize,
    recoverable: Vec<Bucket>,
    uncovered: Vec<Bucket>,
}

/// A replay must shrink the output by a meaningful margin to count as
/// recoverable — tiny deltas usually mean the hook already filtered it.
const MIN_RECOVERABLE_TOKENS: i64 = 100;

pub fn run(opts: Options) -> Result<()> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let actives = filters::load_active_filters();
    let defs: Vec<filters::FilterDef> = actives.iter().map(|a| a.filter.clone()).collect();

    let mut recoverable: HashMap<String, Bucket> = HashMap::new();
    let mut uncovered: HashMap<String, Bucket> = HashMap::new();
    let mut scanned_files = 0usize;
    let mut commands_seen = 0usize;
    let mut already_lean = 0usize;
    // find_filter compiles every filter's match_command regex per call, and
    // real histories repeat the same command thousands of times — memoize
    // command → matched filter index so each unique command pays once.
    let mut match_cache: HashMap<String, Option<usize>> = HashMap::new();

    let cutoff = (opts.since_days > 0).then(|| {
        std::time::SystemTime::now() - std::time::Duration::from_secs(opts.since_days * 86_400)
    });
    let mut skipped_files = 0usize;

    for (agent_key, root) in crate::transcripts::roots(&home) {
        if !agent_selected(opts.agent, agent_key) || !root.exists() {
            continue;
        }
        for path in crate::transcripts::transcript_files(&root, agent_key) {
            if let Ok(meta) = std::fs::metadata(&path) {
                let too_old = match (cutoff, meta.modified()) {
                    (Some(cut), Ok(modified)) => modified < cut,
                    _ => false,
                };
                if too_old || meta.len() > MAX_FILE_BYTES {
                    skipped_files += 1;
                    continue;
                }
            }
            scanned_files += 1;
            let Ok(raw) = std::fs::read_to_string(&path) else {
                continue;
            };
            // tool calls and their outputs can land on different lines, so
            // the id → (tool, command) map spans the whole file.
            let mut tool_map: HashMap<String, (String, Option<String>)> = HashMap::new();
            let values: Vec<Value> = if path.extension().and_then(|x| x.to_str()) == Some("json") {
                serde_json::from_str::<Value>(&raw).into_iter().collect()
            } else {
                raw.lines()
                    .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                    .collect()
            };
            for value in &values {
                collect_calls(value, &mut tool_map);
            }
            for value in &values {
                visit_outputs(value, &mut |id: &str, text: String| {
                    let Some((_tool, Some(cmd))) = tool_map.get(id) else {
                        return; // not a shell command output
                    };
                    commands_seen += 1;
                    let raw_tokens = count_tokens(&text) as i64;
                    let hit_idx = *match_cache.entry(cmd.clone()).or_insert_with(|| {
                        filters::find_filter(cmd, &defs)
                            .and_then(|hit| defs.iter().position(|d| std::ptr::eq(d, hit)))
                    });
                    match hit_idx {
                        Some(idx) => {
                            let name = actives[idx].name.clone();
                            let filtered = filters::apply_filter(&text, &defs[idx]);
                            let saved = (raw_tokens - count_tokens(&filtered) as i64).max(0);
                            if saved < MIN_RECOVERABLE_TOKENS {
                                already_lean += 1;
                                return;
                            }
                            let b = recoverable.entry(name.clone()).or_insert_with(|| Bucket {
                                label: name.clone(),
                                filter: Some(name),
                                agents: BTreeSet::new(),
                                calls: 0,
                                output_tokens: 0,
                                replay_saved: 0,
                                sample: cmd.chars().take(80).collect(),
                            });
                            b.agents.insert(agent_key.to_string());
                            b.calls += 1;
                            b.output_tokens += raw_tokens;
                            b.replay_saved += saved;
                        }
                        None => {
                            let label = crate::gain::command_label(cmd);
                            let b = uncovered.entry(label.clone()).or_insert_with(|| Bucket {
                                label,
                                filter: None,
                                agents: BTreeSet::new(),
                                calls: 0,
                                output_tokens: 0,
                                replay_saved: 0,
                                sample: cmd.chars().take(80).collect(),
                            });
                            b.agents.insert(agent_key.to_string());
                            b.calls += 1;
                            b.output_tokens += raw_tokens;
                        }
                    }
                });
            }
        }
    }

    let mut recoverable: Vec<Bucket> = recoverable.into_values().collect();
    recoverable.sort_by_key(|b| std::cmp::Reverse(b.replay_saved));
    recoverable.truncate(opts.top);
    let mut uncovered: Vec<Bucket> = uncovered.into_values().collect();
    uncovered.sort_by_key(|b| std::cmp::Reverse(b.output_tokens));
    uncovered.truncate(opts.top);

    let report = Report {
        scanned_files,
        skipped_files,
        commands_seen,
        already_lean,
        recoverable,
        uncovered,
    };
    if opts.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human(&report);
    }
    Ok(())
}

fn agent_selected(agent: Agent, key: &str) -> bool {
    match agent {
        Agent::All => true,
        Agent::Claude => key == "claude",
        Agent::Codex => key == "codex",
        Agent::Copilot => key == "copilot",
        Agent::OpenAi => key == "openai",
    }
}

/// Map tool-call ids to (tool name, shell command). Handles Claude
/// (`tool_use` + `input.command`), Codex/OpenAI (`function_call` +
/// JSON-string `arguments.command`, possibly `["bash","-lc",script]`).
fn collect_calls(value: &Value, tool_map: &mut HashMap<String, (String, Option<String>)>) {
    if let Value::Object(map) = value {
        let kind = map.get("type").and_then(Value::as_str);
        if kind == Some("function_call") {
            if let Some(id) = map.get("call_id").and_then(Value::as_str) {
                let name = map
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("function_call")
                    .to_string();
                let command = map
                    .get("arguments")
                    .and_then(Value::as_str)
                    .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                    .as_ref()
                    .and_then(|v| v.get("command"))
                    .and_then(command_text);
                tool_map.insert(id.to_string(), (name, command));
            }
        }
        if kind == Some("tool_use") {
            if let Some(id) = map.get("id").and_then(Value::as_str) {
                let name = map
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("tool_use")
                    .to_string();
                let command = map
                    .get("input")
                    .and_then(|input| input.get("command"))
                    .and_then(command_text);
                tool_map.insert(id.to_string(), (name, command));
            }
        }
        for v in map.values() {
            collect_calls(v, tool_map);
        }
    } else if let Value::Array(items) = value {
        for v in items {
            collect_calls(v, tool_map);
        }
    }
}

/// Extract a command line from either a plain string or the argv-array shape
/// Codex uses (`["bash","-lc","cargo test"]` → `cargo test`).
fn command_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Array(items) => {
            let parts: Vec<&str> = items.iter().filter_map(Value::as_str).collect();
            if parts.is_empty() {
                return None;
            }
            if parts.len() >= 3
                && matches!(parts[0], "bash" | "sh" | "zsh" | "pwsh" | "powershell")
                && matches!(parts[1], "-lc" | "-c" | "-Command")
            {
                return Some(parts[2..].join(" "));
            }
            Some(parts.join(" "))
        }
        _ => None,
    }
}

/// Find tool outputs (`function_call_output.output` / `tool_result.content`)
/// and hand (tool-call id, full text) to the callback. `content` may be a
/// plain string or Claude's array of `{type:"text", text}` parts.
fn visit_outputs(value: &Value, on_output: &mut impl FnMut(&str, String)) {
    match value {
        Value::Object(map) => {
            let kind = map.get("type").and_then(Value::as_str);
            let pair = match kind {
                Some("function_call_output") => Some(("call_id", "output")),
                Some("tool_result") => Some(("tool_use_id", "content")),
                _ => None,
            };
            if let Some((id_key, text_key)) = pair {
                if let (Some(id), Some(text)) = (
                    map.get(id_key).and_then(Value::as_str),
                    map.get(text_key).and_then(text_of),
                ) {
                    on_output(id, text);
                    return;
                }
            }
            for v in map.values() {
                visit_outputs(v, on_output);
            }
        }
        Value::Array(items) => {
            for v in items {
                visit_outputs(v, on_output);
            }
        }
        _ => {}
    }
}

fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Array(items) => {
            let mut out = String::new();
            for it in items {
                if let Some(t) = it.get("text").and_then(Value::as_str) {
                    out.push_str(t);
                    out.push('\n');
                }
            }
            (!out.is_empty()).then_some(out)
        }
        _ => None,
    }
}

fn print_human(report: &Report) {
    box_header("discover · missed savings in transcript history");
    println!(
        "  scanned {} transcript file(s) ({} skipped: too old/large) · {} shell command outputs · {} already lean",
        report.scanned_files, report.skipped_files, report.commands_seen, report.already_lean
    );

    if report.recoverable.is_empty() && report.uncovered.is_empty() {
        println!(
            "\n  {}",
            "No missed savings found — the hook is catching your workload.".green()
        );
        return;
    }

    if !report.recoverable.is_empty() {
        let total: i64 = report.recoverable.iter().map(|b| b.replay_saved).sum();
        println!();
        println!(
            "  {}  (filter exists — these ran unfiltered; install/repair the hook)",
            "RECOVERABLE".bold().underline()
        );
        let rows: Vec<Vec<String>> = report
            .recoverable
            .iter()
            .map(|b| {
                vec![
                    b.label.clone(),
                    b.calls.to_string(),
                    format_num(b.output_tokens),
                    format_num(b.replay_saved),
                    b.agents.iter().cloned().collect::<Vec<_>>().join(","),
                ]
            })
            .collect();
        ui::print_table(
            &["Filter", "Calls", "Raw tokens", "Saved if hooked", "Agents"],
            &rows,
            &[1, 2, 3],
        );
        println!(
            "  {} measured by replaying apply_filter over the historical output — total {}",
            "→".cyan(),
            format_num(total).green().bold()
        );
        println!(
            "  {} fix: {}",
            "→".cyan(),
            "tokenix install-hook --tool all".green()
        );
    }

    if !report.uncovered.is_empty() {
        println!();
        println!(
            "  {}  (no filter yet — biggest candidates for a new filter)",
            "UNCOVERED".bold().underline()
        );
        let rows: Vec<Vec<String>> = report
            .uncovered
            .iter()
            .map(|b| {
                vec![
                    b.label.clone(),
                    b.calls.to_string(),
                    format_num(b.output_tokens),
                    b.agents.iter().cloned().collect::<Vec<_>>().join(","),
                ]
            })
            .collect();
        ui::print_table(
            &["Command", "Calls", "Output tokens", "Agents"],
            &rows,
            &[1, 2],
        );
        println!(
            "  {} create one:  {}",
            "→".cyan(),
            "tokenix filter generate <command>".green()
        );
    }
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_text_handles_string_and_argv_shapes() {
        assert_eq!(
            command_text(&serde_json::json!("cargo test")),
            Some("cargo test".to_string())
        );
        assert_eq!(
            command_text(&serde_json::json!(["bash", "-lc", "cargo test --quiet"])),
            Some("cargo test --quiet".to_string())
        );
        assert_eq!(
            command_text(&serde_json::json!(["git", "status"])),
            Some("git status".to_string())
        );
        assert_eq!(command_text(&serde_json::json!(42)), None);
    }

    #[test]
    fn visit_outputs_extracts_string_and_text_array_content() {
        let line = serde_json::json!({
            "message": { "content": [
                { "type": "tool_result", "tool_use_id": "t1", "content": "plain output" },
                { "type": "tool_result", "tool_use_id": "t2",
                  "content": [ { "type": "text", "text": "part one" },
                               { "type": "text", "text": "part two" } ] }
            ]}
        });
        let mut seen: Vec<(String, String)> = Vec::new();
        visit_outputs(&line, &mut |id, text| seen.push((id.to_string(), text)));
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0], ("t1".to_string(), "plain output".to_string()));
        assert_eq!(seen[1].0, "t2");
        assert_eq!(seen[1].1, "part one\npart two\n");
    }

    #[test]
    fn collect_calls_maps_claude_and_codex_shapes() {
        let mut map = HashMap::new();
        collect_calls(
            &serde_json::json!({ "message": { "content": [
                { "type": "tool_use", "id": "t1", "name": "Bash",
                  "input": { "command": "git status" } }
            ]}}),
            &mut map,
        );
        collect_calls(
            &serde_json::json!({ "type": "function_call", "call_id": "c1", "name": "shell",
                "arguments": "{\"command\":[\"bash\",\"-lc\",\"cargo build\"]}" }),
            &mut map,
        );
        assert_eq!(
            map.get("t1"),
            Some(&("Bash".to_string(), Some("git status".to_string())))
        );
        assert_eq!(
            map.get("c1"),
            Some(&("shell".to_string(), Some("cargo build".to_string())))
        );
    }
}
