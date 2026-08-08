//! Conversation transcript token-waste auditor.
//!
//! This is intentionally separate from `scan-secrets`: it looks for large
//! assistant-visible blobs in local AI-agent histories, classifies the waste
//! pattern, and points at the tokenix mitigation (filter, hook, or instruction).

use anyhow::Result;
use colored::Colorize;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::chunker::count_tokens;
use crate::filters;
use crate::ui;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
    Copilot,
    OpenAi,
    All,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scenario {
    FullRead,
    LargeCommandOutput,
    BootstrapPrompt,
    HookMetadata,
    ToolSchema,
    DiffDump,
    TestLog,
    TaskContextBlob,
    AgentLogBlob,
    ImageBlob,
    ConnectorJsonBlob,
    PatchBlob,
    BuildArtifactBlob,
    SignatureBlob,
    DocumentationBlob,
    UnknownLarge,
}

impl Scenario {
    fn label(self) -> &'static str {
        match self {
            Scenario::FullRead => "full-read",
            Scenario::LargeCommandOutput => "large-command-output",
            Scenario::BootstrapPrompt => "bootstrap-prompt",
            Scenario::HookMetadata => "hook-metadata",
            Scenario::ToolSchema => "tool-schema",
            Scenario::DiffDump => "diff-dump",
            Scenario::TestLog => "test-log",
            Scenario::TaskContextBlob => "task-context-blob",
            Scenario::AgentLogBlob => "agent-log-blob",
            Scenario::ImageBlob => "image-blob",
            Scenario::ConnectorJsonBlob => "connector-json-blob",
            Scenario::PatchBlob => "patch-blob",
            Scenario::BuildArtifactBlob => "build-artifact-blob",
            Scenario::SignatureBlob => "signature-blob",
            Scenario::DocumentationBlob => "documentation-blob",
            Scenario::UnknownLarge => "unknown-large",
        }
    }

    fn recommendation(self) -> &'static str {
        match self {
            Scenario::FullRead => {
                "Use tokenix read/query first; then symbol or offset/limit reads."
            }
            Scenario::LargeCommandOutput => {
                "Route command through tokenix run and add/adjust an output filter."
            }
            Scenario::BootstrapPrompt => {
                "Reduce installed skills/tools/MCP prompt weight; check prompt-audit."
            }
            Scenario::HookMetadata => {
                "Keep hook success payloads minimal and avoid duplicated updatedInput fields."
            }
            Scenario::ToolSchema => "Disable unused MCP servers or switch to a slim MCP profile.",
            Scenario::DiffDump => "Use git diff --stat/name-only first, then targeted hunks.",
            Scenario::TestLog => {
                "Keep failures, summaries, and first diagnostics; collapse passing noise."
            }
            Scenario::TaskContextBlob => {
                "Pass task metadata/artifacts as indexed references, not full embedded blobs."
            }
            Scenario::AgentLogBlob => {
                "Filter logs before returning them; keep errors plus bounded head/tail windows."
            }
            Scenario::ImageBlob => {
                "Store image attachments by path/hash; do not replay base64 into context."
            }
            Scenario::ConnectorJsonBlob => {
                "Request compact fields/pages from connectors; avoid returning full API payloads."
            }
            Scenario::PatchBlob => {
                "Prefer file patch references or bounded hunks over full patch payloads."
            }
            Scenario::BuildArtifactBlob => {
                "Avoid returning generated/minified build artifacts; inspect sources instead."
            }
            Scenario::SignatureBlob => {
                "Do not replay provider signature/provenance fields into assistant context."
            }
            Scenario::DocumentationBlob => {
                "Use documentation indexes/URLs as references, then fetch only needed pages."
            }
            Scenario::UnknownLarge => {
                "Inspect the preview and add a filter or hook rule for this shape."
            }
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Finding {
    pub agent: String,
    pub scenario: Scenario,
    pub file: String,
    pub line: usize,
    pub json_path: String,
    pub chars: usize,
    pub tokens: usize,
    pub tool: Option<String>,
    pub command: Option<String>,
    pub filter: Option<String>,
    pub preview: String,
}

#[derive(Clone, Debug, Default, Serialize)]
struct ScenarioSummary {
    scenario: String,
    count: usize,
    chars: usize,
    tokens: usize,
    recommendation: String,
}

#[derive(Clone, Debug, Serialize)]
struct Report {
    min_chars: usize,
    scanned_files: usize,
    findings: Vec<Finding>,
    scenarios: Vec<ScenarioSummary>,
}

pub struct Options {
    pub agent: Agent,
    pub min_chars: usize,
    pub limit: usize,
    pub json: bool,
    pub generate: bool,
}

pub fn run(options: Options) -> Result<()> {
    let report = audit(options.agent, options.min_chars, options.limit)?;
    if options.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human(&report);
        if options.generate {
            print_actions(&report);
        }
    }
    Ok(())
}

/// Scenarios whose waste is a shell command's output — i.e. a `tokenix filter`
/// could catch it. Blob/prompt scenarios are not command-driven, so a generated
/// filter would not help them.
fn is_command_scenario(s: Scenario) -> bool {
    matches!(
        s,
        Scenario::LargeCommandOutput | Scenario::DiffDump | Scenario::TestLog
    )
}

/// Reduce a full command line to the program name a filter keys on: first token,
/// path and `.exe` suffix stripped (`/usr/bin/kubectl get pods` -> `kubectl`).
fn program_name(cmd: &str) -> String {
    cmd.split_whitespace()
        .next()
        .unwrap_or("")
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .trim_end_matches(".exe")
        .to_string()
}

/// Closes the audit -> action loop: for command-output waste that has no filter
/// yet, print the exact `tokenix filter generate` invocation, ranked by tokens.
fn print_actions(report: &Report) {
    use std::collections::HashMap;
    let mut by_prog: HashMap<String, (usize, usize)> = HashMap::new();
    for f in &report.findings {
        if f.filter.is_some() || !is_command_scenario(f.scenario) {
            continue;
        }
        let Some(cmd) = f.command.as_deref() else {
            continue;
        };
        let prog = program_name(cmd);
        if prog.is_empty() {
            continue;
        }
        let e = by_prog.entry(prog).or_insert((0, 0));
        e.0 += f.tokens;
        e.1 += 1;
    }

    println!("\n{}", "next actions".bold());
    if by_prog.is_empty() {
        println!(
            "  {}",
            "no command-output findings lack a filter — the top waste here is not \
             filter-fixable (base64/image blobs are handled by output redaction; see \
             the recommendations above)"
                .dimmed()
        );
        return;
    }
    let mut ranked: Vec<(String, (usize, usize))> = by_prog.into_iter().collect();
    ranked.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then(a.0.cmp(&b.0)));
    for (prog, (tokens, count)) in ranked {
        println!(
            "  tokenix filter generate '{}'   {}",
            prog.cyan(),
            format!(
                "# ~{} tokens across {} finding(s)",
                crate::ui::format_num(tokens as i64),
                count
            )
            .dimmed()
        );
    }
}

fn audit(agent: Agent, min_chars: usize, limit: usize) -> Result<Report> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let filters = filters::load_all_filters();
    let mut scanned_files = 0usize;
    let mut findings = Vec::new();

    for (agent_key, root) in roots(&home, agent) {
        if !root.exists() {
            continue;
        }
        for path in crate::transcripts::transcript_files(&root, agent_key) {
            scanned_files += 1;
            scan_jsonl_file(&path, agent_key, min_chars, &filters, &mut findings);
            scan_text_file(&path, agent_key, min_chars, &filters, &mut findings);
        }
    }

    if matches!(agent, Agent::All | Agent::Copilot) {
        scan_copilot_sqlite(&home, min_chars, &filters, &mut findings)?;
    }

    findings.sort_by_key(|f| std::cmp::Reverse(f.chars));
    if findings.len() > limit {
        findings.truncate(limit);
    }

    let scenarios = summarize(&findings);
    Ok(Report {
        min_chars,
        scanned_files,
        findings,
        scenarios,
    })
}

fn roots(home: &Path, agent: Agent) -> Vec<(&'static str, PathBuf)> {
    crate::transcripts::roots(home)
        .into_iter()
        .filter(|(key, _)| match agent {
            Agent::All => true,
            Agent::Claude => *key == "claude",
            Agent::Codex => *key == "codex",
            Agent::Copilot => *key == "copilot",
            Agent::OpenAi => *key == "openai",
        })
        .collect()
}

fn scan_jsonl_file(
    path: &Path,
    agent: &str,
    min_chars: usize,
    filters: &[filters::FilterDef],
    findings: &mut Vec<Finding>,
) {
    // Bound the read: transcripts are normally a few MB, but a stray multi-GB
    // file would be pulled entirely into memory (the walker applies no size
    // filter). `scan-secrets` already caps at the same size.
    const MAX_TRANSCRIPT_BYTES: u64 = 50 * 1024 * 1024;
    if std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) > MAX_TRANSCRIPT_BYTES {
        return;
    }
    let Ok(raw) = std::fs::read_to_string(path) else {
        return;
    };

    let mut tool_map: HashMap<String, (String, Option<String>)> = HashMap::new();
    if path.extension().and_then(|x| x.to_str()) == Some("json") {
        if let Ok(value) = serde_json::from_str::<Value>(&raw) {
            collect_tool_calls(&value, &mut tool_map);
            collect_large_strings(
                &value,
                String::new(),
                path,
                1,
                agent,
                min_chars,
                &tool_map,
                filters,
                findings,
            );
            return;
        }
    }

    for (idx, line) in raw.lines().enumerate() {
        let line_no = idx + 1;
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };

        collect_tool_calls(&value, &mut tool_map);
        collect_large_strings(
            &value,
            String::new(),
            path,
            line_no,
            agent,
            min_chars,
            &tool_map,
            filters,
            findings,
        );
    }
}

fn scan_text_file(
    path: &Path,
    agent: &str,
    min_chars: usize,
    filters: &[filters::FilterDef],
    findings: &mut Vec<Finding>,
) {
    let ext = path.extension().and_then(|x| x.to_str()).unwrap_or("");
    if matches!(ext, "jsonl" | "json") {
        return;
    }
    // Bound the read: transcripts are normally a few MB, but a stray multi-GB
    // file would be pulled entirely into memory (the walker applies no size
    // filter). `scan-secrets` already caps at the same size.
    const MAX_TRANSCRIPT_BYTES: u64 = 50 * 1024 * 1024;
    if std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) > MAX_TRANSCRIPT_BYTES {
        return;
    }
    let Ok(raw) = std::fs::read_to_string(path) else {
        return;
    };
    if raw.len() < min_chars {
        return;
    }
    push_finding(
        agent,
        classify("", &raw, None),
        path,
        1,
        "",
        None,
        None,
        filters,
        &raw,
        findings,
    );
}

#[allow(clippy::too_many_arguments)]
fn collect_large_strings(
    value: &Value,
    path_key: String,
    file: &Path,
    line: usize,
    agent: &str,
    min_chars: usize,
    tool_map: &HashMap<String, (String, Option<String>)>,
    filters: &[filters::FilterDef],
    findings: &mut Vec<Finding>,
) {
    match value {
        Value::String(s) if s.len() >= min_chars => {
            let tool = tool_from_path(&path_key);
            let command = None;
            let scenario = classify(&path_key, s, command.as_deref().or(tool.as_deref()));
            push_finding(
                agent, scenario, file, line, &path_key, tool, command, filters, s, findings,
            );
        }
        Value::String(_) => {}
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                collect_large_strings(
                    item,
                    format!("{path_key}/{i}"),
                    file,
                    line,
                    agent,
                    min_chars,
                    tool_map,
                    filters,
                    findings,
                );
            }
        }
        Value::Object(map) => {
            if push_tool_output_if_large(
                map, file, line, agent, min_chars, tool_map, filters, findings,
            ) {
                return;
            }
            for (k, v) in map {
                collect_large_strings(
                    v,
                    format!("{path_key}/{k}"),
                    file,
                    line,
                    agent,
                    min_chars,
                    tool_map,
                    filters,
                    findings,
                );
            }
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn push_tool_output_if_large(
    map: &serde_json::Map<String, Value>,
    file: &Path,
    line: usize,
    agent: &str,
    min_chars: usize,
    tool_map: &HashMap<String, (String, Option<String>)>,
    filters: &[filters::FilterDef],
    findings: &mut Vec<Finding>,
) -> bool {
    let kind = map.get("type").and_then(Value::as_str);

    if kind == Some("response_item") {
        if let Some(Value::Object(payload)) = map.get("payload") {
            return push_tool_output_if_large(
                payload, file, line, agent, min_chars, tool_map, filters, findings,
            );
        }
    }

    let (id_key, text_key) = match kind {
        Some("function_call_output") => ("call_id", "output"),
        Some("tool_result") => ("tool_use_id", "content"),
        _ => return false,
    };
    let Some(text) = map.get(text_key).and_then(Value::as_str) else {
        return false;
    };
    if text.len() < min_chars {
        return false;
    }

    let mapped = map
        .get(id_key)
        .and_then(Value::as_str)
        .and_then(|id| tool_map.get(id));
    let tool = mapped
        .map(|(t, _)| t.clone())
        .or_else(|| Some("tool-output".to_string()));
    let command = mapped.and_then(|(_, c)| c.clone());
    let scenario = classify(text_key, text, command.as_deref().or(tool.as_deref()));
    push_finding(
        agent, scenario, file, line, text_key, tool, command, filters, text, findings,
    );
    true
}

fn collect_tool_calls(value: &Value, tool_map: &mut HashMap<String, (String, Option<String>)>) {
    if let Value::Object(map) = value {
        let kind = map.get("type").and_then(Value::as_str);
        if kind == Some("response_item") {
            if let Some(payload) = map.get("payload") {
                collect_tool_calls(payload, tool_map);
            }
        }
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
                    .and_then(extract_command_arg);
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
                    .and_then(Value::as_str)
                    .map(str::to_string);
                tool_map.insert(id.to_string(), (name, command));
            }
        }
        for v in map.values() {
            collect_tool_calls(v, tool_map);
        }
    }
}

fn extract_command_arg(raw: &str) -> Option<String> {
    serde_json::from_str::<Value>(raw).ok().and_then(|v| {
        v.get("command")
            .or_else(|| v.get("CommandLine"))
            .or_else(|| v.get("commandLine"))
            .and_then(Value::as_str)
            .map(str::to_string)
    })
}

fn tool_from_path(path_key: &str) -> Option<String> {
    let lower = path_key.to_ascii_lowercase();
    if lower.contains("tool_result") || lower.contains("function_call_output") {
        Some("tool-output".to_string())
    } else if lower.contains("base_instructions") || lower.contains("developer_instructions") {
        Some("system-prompt".to_string())
    } else if lower.contains("skill_listing") {
        Some("skill-listing".to_string())
    } else {
        None
    }
}

fn classify(path_key: &str, text: &str, command_or_tool: Option<&str>) -> Scenario {
    let lower_path = path_key.to_ascii_lowercase();
    let lower_text = text
        .chars()
        .take(600)
        .collect::<String>()
        .to_ascii_lowercase();
    let lower_cmd = command_or_tool.unwrap_or("").to_ascii_lowercase();

    if lower_path.contains("base_instructions")
        || lower_path.contains("developer_instructions")
        || lower_path.contains("skill_listing")
        || lower_text.contains("# desired oververbosity")
        || lower_text.contains("you are codex")
        || lower_text.contains("base directory for this skill:")
        || lower_text.contains("# update config skill")
        || lower_text.contains("project mika")
        || lower_text.contains("you output only a thread title")
    {
        return Scenario::BootstrapPrompt;
    }
    if lower_path.contains("signature") {
        return Scenario::SignatureBlob;
    }
    if lower_text.starts_with("data:image/")
        || lower_text.starts_with("/9j/")
        || lower_path.contains("image_url")
        || lower_path.contains("/source/data")
        || lower_path.contains("/file/base64")
    {
        return Scenario::ImageBlob;
    }
    if lower_text.contains("documentation index")
        || lower_text.contains("llms.txt")
        || lower_text.contains("use this file to discover all available pages")
    {
        return Scenario::DocumentationBlob;
    }
    if lower_text.contains("<div id=\"app\"")
        || lower_text.contains("<!doctype html")
        || lower_text.contains(".vitepress/dist/")
        || lower_text.contains("diagramcode")
        || lower_path.contains("rawsvg")
    {
        return Scenario::BuildArtifactBlob;
    }
    if lower_path.contains("/payload/result/ok/content")
        && (lower_text.starts_with("{\"issues\"")
            || lower_text.starts_with("{\"repositories\"")
            || lower_text.starts_with("{\"data\""))
    {
        return Scenario::ConnectorJsonBlob;
    }
    if lower_path == "output"
        && lower_text.contains(" output: {\"")
        && (lower_text.contains("\"issues\"")
            || lower_text.contains("\"repositories\"")
            || lower_text.contains("\"data\""))
    {
        return Scenario::ConnectorJsonBlob;
    }
    if lower_text.starts_with("{\"data\":")
        && (lower_text.contains("\"enabled\"")
            || lower_text.contains("\"authenticated\"")
            || lower_text.contains("\"loginurl\""))
    {
        return Scenario::ConnectorJsonBlob;
    }
    if lower_path.contains("/payload/input")
        && (lower_text.contains("*** begin patch")
            || lower_text.contains("*** update file")
            || lower_text.contains("*** add file"))
    {
        return Scenario::PatchBlob;
    }
    if lower_text.contains("=== task detail ===")
        || lower_text.contains("=== task artifacts ===")
        || lower_text.contains("context bundle")
        || lower_path.contains("task_artifact")
        || lower_path.contains("task_detail")
    {
        return Scenario::TaskContextBlob;
    }
    if lower_text.contains("litellm proxy:")
        || lower_text.contains("---log snapshot---")
        || lower_text.contains("=== homolog start ===")
        || lower_text.contains("/.local/share/opencode/log/")
        || lower_text.contains("defaulted container")
        || lower_text.contains("select-string :")
        || lower_text.starts_with("error 20")
        || lower_text.contains("service=llm")
    {
        return Scenario::AgentLogBlob;
    }
    if lower_text.contains("\"hookspecificoutput\"")
        || lower_text.contains("permissiondecision")
        || lower_path.contains("hook_success")
    {
        return Scenario::HookMetadata;
    }
    if lower_text.contains("\"input_schema\"")
        || lower_text.contains("\"tool\"")
        || lower_text.contains("mcp")
    {
        return Scenario::ToolSchema;
    }
    if lower_cmd.contains("git diff")
        || lower_text.contains("diff --git ")
        || lower_text.contains("@@")
    {
        return Scenario::DiffDump;
    }
    if lower_text.starts_with("grep:") {
        return Scenario::LargeCommandOutput;
    }
    if lower_text.contains("terraform")
        || lower_text.contains("refreshing state")
        || lower_text.contains("module.")
    {
        return Scenario::LargeCommandOutput;
    }
    if lower_cmd.contains("test")
        || lower_text.contains("running ")
        || lower_text.contains("test result:")
        || lower_text.contains("turbo run test")
        || lower_text.contains("failed")
    {
        return Scenario::TestLog;
    }
    if lower_cmd.contains("kubectl")
        || lower_cmd.contains("git ")
        || lower_cmd.contains("npm ")
        || lower_cmd.contains("pnpm ")
        || lower_cmd.contains("cargo ")
        || lower_cmd.contains("node ")
        || lower_cmd.contains("tree")
        || lower_cmd.contains("gh run")
        || lower_path.contains("function_call_output")
        || lower_path.contains("tool_result")
    {
        return Scenario::LargeCommandOutput;
    }
    if looks_like_numbered_file(text) || lower_cmd == "read" || lower_cmd.contains("read") {
        return Scenario::FullRead;
    }
    Scenario::UnknownLarge
}

fn looks_like_numbered_file(text: &str) -> bool {
    let mut numbered = 0usize;
    for line in text.lines().take(30) {
        let t = line.trim_start();
        let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0 && t.chars().nth(digits).is_some_and(|c| c == '\t' || c == ' ') {
            numbered += 1;
        }
    }
    numbered >= 5
}

#[allow(clippy::too_many_arguments)]
fn push_finding(
    agent: &str,
    scenario: Scenario,
    file: &Path,
    line: usize,
    json_path: &str,
    tool: Option<String>,
    command: Option<String>,
    filters: &[filters::FilterDef],
    text: &str,
    findings: &mut Vec<Finding>,
) {
    let filter = command
        .as_deref()
        .and_then(|cmd| filters::find_filter(cmd, filters))
        .and_then(|f| f.description.clone())
        .or_else(|| {
            command
                .as_deref()
                .and_then(|cmd| filters::find_filter(cmd, filters))
                .map(|_| "matched filter".to_string())
        });
    findings.push(Finding {
        agent: agent.to_string(),
        scenario,
        file: file.display().to_string(),
        line,
        json_path: json_path.to_string(),
        chars: text.len(),
        tokens: count_tokens(text),
        tool,
        // Commands are transcript data too: `git push https://<token>@…` and
        // `curl -H "Authorization: Bearer …"` are exactly what shows up here.
        // The human path already trims to 76 chars; JSON printed them in full.
        command: command.as_deref().map(redact_credentials),
        filter,
        preview: preview(text),
    });
}

fn preview(text: &str) -> String {
    redact_credentials(
        &text
            .chars()
            .take(180)
            .collect::<String>()
            .replace(['\r', '\n', '\t'], " "),
    )
}

/// Mask credential shapes before anything from a transcript is printed or
/// serialized. This report is routinely piped into logs and pastes, and
/// transcripts carry `https://<token>@host`, `Authorization: Bearer …` and
/// `token=…` verbatim — `scan-secrets` redacts by default, so must this.
pub fn redact_credentials(s: &str) -> String {
    use std::sync::OnceLock;
    static RULES: OnceLock<Vec<regex::Regex>> = OnceLock::new();
    let rules = RULES.get_or_init(|| {
        [
            // userinfo in a URL: https://user:token@host
            r"(?i)://[^/\s@]+@",
            // Authorization / api-key headers
            r"(?i)(authorization|x-api-key|api-key)\s*:\s*\S+",
            r"(?i)\bbearer\s+[A-Za-z0-9._\-]+",
            // token=… / key=… / password=… in query strings or env assignments
            r#"(?i)\b(token|api[_-]?key|secret|password|passwd|pwd)\s*[=:]\s*[^\s&"']+"#,
            // PEM bodies
            r"-----BEGIN [A-Z ]*PRIVATE KEY-----",
        ]
        .iter()
        .filter_map(|p| regex::Regex::new(p).ok())
        .collect()
    });
    let mut out = s.to_string();
    for (i, re) in rules.iter().enumerate() {
        out = match i {
            0 => re.replace_all(&out, "://[REDACTED]@").into_owned(),
            _ => re.replace_all(&out, "[REDACTED]").into_owned(),
        };
    }
    out
}

fn summarize(findings: &[Finding]) -> Vec<ScenarioSummary> {
    let mut by: HashMap<Scenario, ScenarioSummary> = HashMap::new();
    for f in findings {
        let entry = by.entry(f.scenario).or_insert_with(|| ScenarioSummary {
            scenario: f.scenario.label().to_string(),
            recommendation: f.scenario.recommendation().to_string(),
            ..ScenarioSummary::default()
        });
        entry.count += 1;
        entry.chars += f.chars;
        entry.tokens += f.tokens;
    }
    let mut out: Vec<_> = by.into_values().collect();
    out.sort_by_key(|s| std::cmp::Reverse(s.tokens));
    out
}

fn print_human(report: &Report) {
    ui::box_header("conversation-audit");
    println!(
        "  scanned: {} files · min {} chars · findings {}",
        report.scanned_files,
        report.min_chars,
        report.findings.len()
    );
    println!();
    for s in &report.scenarios {
        println!(
            "  {:<22} {:>4} hits  {:>8} tokens  {}",
            s.scenario.cyan(),
            s.count,
            crate::ui::format_num(s.tokens as i64).yellow(),
            s.recommendation.dimmed()
        );
    }
    if !report.findings.is_empty() {
        println!("\n{}", "top findings".bold());
    }
    for f in report.findings.iter().take(20) {
        let filter = f
            .filter
            .as_deref()
            .map(|x| format!(" filter={x}"))
            .unwrap_or_default();
        let command = f
            .command
            .as_deref()
            .map(|x| format!(" cmd={}", trim(x, 76)))
            .unwrap_or_default();
        println!(
            "  {} {} tokens={} chars={}{}{}",
            f.agent.dimmed(),
            f.scenario.label().bold(),
            crate::ui::format_num(f.tokens as i64),
            crate::ui::format_num(f.chars as i64),
            filter.dimmed(),
            command.dimmed()
        );
        println!("    {}:{}", trim(&f.file, 100), f.line);
        if !f.json_path.is_empty() {
            println!("    path={}", trim(&f.json_path, 100).dimmed());
        }
        println!("    {}", f.preview.dimmed());
    }
}

fn trim(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out = s.chars().take(max.saturating_sub(3)).collect::<String>();
    out.push_str("...");
    out
}

fn scan_copilot_sqlite(
    home: &Path,
    min_chars: usize,
    filters: &[filters::FilterDef],
    findings: &mut Vec<Finding>,
) -> Result<()> {
    let Some(db) = copilot_db_path(home) else {
        return Ok(());
    };
    let conn = match rusqlite::Connection::open_with_flags(
        &db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(c) => c,
        Err(_) => return Ok(()),
    };
    scan_copilot_table(
        &conn,
        &db,
        "turns",
        &["user_message", "assistant_response"],
        min_chars,
        filters,
        findings,
    );
    scan_copilot_table(
        &conn,
        &db,
        "checkpoints",
        &["title", "overview", "history", "work_done"],
        min_chars,
        filters,
        findings,
    );
    Ok(())
}

fn copilot_db_path(home: &Path) -> Option<PathBuf> {
    if let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) {
        let db = appdata
            .join("Code")
            .join("User")
            .join("globalStorage")
            .join("github.copilot-chat")
            .join("session-store.db");
        if db.exists() {
            return Some(db);
        }
    }
    let db = home.join(".copilot").join("session-store.db");
    db.exists().then_some(db)
}

fn scan_copilot_table(
    conn: &rusqlite::Connection,
    db: &Path,
    table: &str,
    columns: &[&str],
    min_chars: usize,
    filters: &[filters::FilterDef],
    findings: &mut Vec<Finding>,
) {
    let Ok(mut stmt) = conn.prepare(&format!("SELECT {} FROM {table}", columns.join(","))) else {
        return;
    };
    let Ok(rows) = stmt.query_map([], |row| {
        let mut values = Vec::new();
        for i in 0..columns.len() {
            values.push(row.get::<_, Option<String>>(i).unwrap_or_default());
        }
        Ok(values)
    }) else {
        return;
    };
    for (row_idx, row) in rows.filter_map(|r| r.ok()).enumerate() {
        for text in row.into_iter().flatten() {
            if text.len() < min_chars {
                continue;
            }
            let scenario = classify(table, &text, None);
            push_finding(
                "copilot",
                scenario,
                db,
                row_idx + 1,
                table,
                Some(table.to_string()),
                None,
                filters,
                &text,
                findings,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_full_read_from_numbered_lines() {
        let text = (1..=10)
            .map(|i| format!("{i}\tfn item_{i}() {{}}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(classify("", &text, Some("Read")), Scenario::FullRead);
    }

    #[test]
    fn classifies_bootstrap_prompt() {
        assert_eq!(
            classify("/payload/base_instructions/text", "You are Codex", None),
            Scenario::BootstrapPrompt
        );
    }

    #[test]
    fn program_name_strips_path_args_and_exe() {
        assert_eq!(program_name("/usr/bin/kubectl get pods"), "kubectl");
        assert_eq!(program_name("C:\\tools\\rg.exe -n foo"), "rg");
        assert_eq!(program_name("git diff HEAD"), "git");
        assert_eq!(program_name(""), "");
    }

    #[test]
    fn only_command_scenarios_are_filter_actionable() {
        assert!(is_command_scenario(Scenario::LargeCommandOutput));
        assert!(is_command_scenario(Scenario::DiffDump));
        assert!(!is_command_scenario(Scenario::ImageBlob));
        assert!(!is_command_scenario(Scenario::BootstrapPrompt));
    }

    #[test]
    fn classifies_command_output_from_call_output_path() {
        assert_eq!(
            classify(
                "/payload/output",
                "kubectl get pods\n".repeat(400).as_str(),
                Some("kubectl get pods")
            ),
            Scenario::LargeCommandOutput
        );
    }
}
