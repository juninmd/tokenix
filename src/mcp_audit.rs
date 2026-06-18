//! Multi-agent MCP "prompt weight" auditor.
//!
//! The base system prompt of an AI coding agent (Claude Code, Codex, Copilot,
//! Antigravity) is internal and cannot be read or intercepted via hooks. What we
//! *can* measure is the largest variable cost in the effective system prompt: the
//! tool-definition JSON injected by each enabled MCP server. This module discovers
//! the MCP servers configured for each agent, connects to each one live
//! (`initialize` + `tools/list`), tokenizes the returned tool schemas, adds a
//! static baseline for the agent's native tools, and warns when the total looks
//! bloated.
//!
//! The number is a *relative bloat indicator*, not the exact system-prompt token
//! count (native baseline is approximate, HTTP/SSE servers are not introspected,
//! and `count_tokens` is a ~4-chars/token estimate).

use crate::chunker::count_tokens;
use anyhow::Result;
use colored::Colorize;
use serde_json::{json, Value};
use std::cmp::Reverse;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Per-server live introspection timeout.
const INTROSPECT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    ClaudeCode,
    Codex,
    Copilot,
    OpenCode,
    Antigravity,
}

impl Agent {
    pub fn all() -> [Agent; 5] {
        [
            Agent::ClaudeCode,
            Agent::Codex,
            Agent::Copilot,
            Agent::OpenCode,
            Agent::Antigravity,
        ]
    }

    fn label(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "Claude Code",
            Agent::Codex => "Codex",
            Agent::Copilot => "Copilot",
            Agent::OpenCode => "OpenCode",
            Agent::Antigravity => "Antigravity",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "claude-code",
            Agent::Codex => "codex",
            Agent::Copilot => "copilot",
            Agent::OpenCode => "opencode",
            Agent::Antigravity => "antigravity",
        }
    }

    /// Rough static token cost of the agent's built-in (non-MCP) tools. These are
    /// approximations that drift between agent versions; centralized here so they
    /// are easy to retune. They are NOT measured from a real system prompt.
    fn native_tokens(self) -> usize {
        match self {
            Agent::ClaudeCode => 2500,
            Agent::Codex => 1500,
            Agent::Copilot => 1500,
            Agent::OpenCode => 1500,
            Agent::Antigravity => 1500,
        }
    }
}

enum Transport {
    Stdio {
        command: String,
        args: Vec<String>,
        env: Vec<(String, String)>,
    },
    Http {
        url: String,
    },
}

struct ServerSpec {
    agent: Agent,
    name: String,
    transport: Transport,
}

#[derive(Clone)]
enum Status {
    Ok { tools: usize, tokens: usize },
    Unknown(String),
    Error(String),
}

struct Thresholds {
    tokens: usize,
    servers: usize,
    tools: usize,
}

pub struct AuditSummary {
    pub combined_tokens: usize,
    pub warnings: Vec<String>,
}

impl Thresholds {
    fn load() -> Self {
        Thresholds {
            tokens: env_usize("TOKENIX_AUDIT_WARN_TOKENS", 10_000),
            servers: env_usize("TOKENIX_AUDIT_WARN_SERVERS", 5),
            tools: env_usize("TOKENIX_AUDIT_WARN_TOOLS", 40),
        }
    }
}

fn env_usize(var: &str, default: usize) -> usize {
    std::env::var(var)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Entry point for `tokenix prompt-audit`.
/// `filter` = None audits every agent that has config; Some(agent) audits one.
pub fn run_audit(
    filter: Option<Agent>,
    json_out: bool,
    recommend: bool,
    profile_impact: bool,
    cwd: &Path,
) -> Result<()> {
    let (agents, reports, thresholds) = collect_audit(filter, cwd);
    if json_out {
        print_json(&agents, &reports, &thresholds, recommend, profile_impact);
    } else {
        print_human(&agents, &reports, &thresholds, recommend, profile_impact);
    }
    Ok(())
}

pub fn audit_summary(filter: Option<Agent>, cwd: &Path) -> AuditSummary {
    let (agents, reports, thresholds) = collect_audit(filter, cwd);
    let mut combined_tokens = 0usize;
    let mut warnings = Vec::new();
    for agent in &agents {
        let (_rows, totals) = aggregate(*agent, &reports, &thresholds);
        combined_tokens += totals.native + totals.mcp_tokens;
        for reason in totals.reasons {
            warnings.push(format!("{}: {reason}", agent.label()));
        }
    }
    AuditSummary {
        combined_tokens,
        warnings,
    }
}

fn collect_audit(
    filter: Option<Agent>,
    cwd: &Path,
) -> (Vec<Agent>, Vec<(Agent, String, Status)>, Thresholds) {
    let agents: Vec<Agent> = match filter {
        Some(a) => vec![a],
        None => Agent::all().to_vec(),
    };

    // Discover every configured server, grouped per agent.
    let mut specs: Vec<ServerSpec> = Vec::new();
    for agent in &agents {
        specs.extend(discover(*agent, cwd));
    }

    // Introspect each unique stdio transport once, reuse across agents.
    let mut cache: HashMap<String, Status> = HashMap::new();
    let mut reports: Vec<(Agent, String, Status)> = Vec::new();
    for spec in &specs {
        let status = match &spec.transport {
            Transport::Http { url } => Status::Unknown(format!("HTTP/SSE not introspected: {url}")),
            Transport::Stdio { command, args, env } => {
                let key = format!("{command}\u{0}{}", args.join("\u{0}"));
                cache
                    .entry(key)
                    .or_insert_with(|| introspect_stdio(command, args, env))
                    .clone()
            }
        };
        reports.push((spec.agent, spec.name.clone(), status));
    }

    let thresholds = Thresholds::load();
    (agents, reports, thresholds)
}

// ---------------------------------------------------------------------------
// Config discovery (one source per agent)
// ---------------------------------------------------------------------------

fn discover(agent: Agent, cwd: &Path) -> Vec<ServerSpec> {
    match agent {
        Agent::ClaudeCode => discover_claude(cwd),
        Agent::Codex => discover_codex(),
        Agent::Copilot => discover_copilot(cwd),
        Agent::OpenCode => discover_opencode(cwd),
        Agent::Antigravity => discover_antigravity(),
    }
}

/// Parse a Claude/Antigravity/Copilot-style `{ name: { command|url, args, env } }`
/// JSON map into specs.
fn parse_json_map(agent: Agent, map: &serde_json::Map<String, Value>, out: &mut Vec<ServerSpec>) {
    for (name, val) in map {
        if let Some(spec) = parse_json_server(agent, name, val) {
            out.push(spec);
        }
    }
}

fn parse_json_server(agent: Agent, name: &str, val: &Value) -> Option<ServerSpec> {
    // HTTP/SSE servers carry a `url`; introspection is skipped for them.
    if let Some(url) = val.get("url").and_then(Value::as_str) {
        return Some(ServerSpec {
            agent,
            name: name.to_string(),
            transport: Transport::Http {
                url: url.to_string(),
            },
        });
    }
    let command = val.get("command")?.as_str()?.to_string();
    let args = val
        .get("args")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let env = val
        .get("env")
        .and_then(Value::as_object)
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    Some(ServerSpec {
        agent,
        name: name.to_string(),
        transport: Transport::Stdio { command, args, env },
    })
}

fn read_json(path: &Path) -> Option<Value> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn discover_claude(cwd: &Path) -> Vec<ServerSpec> {
    let mut by_name: HashMap<String, ServerSpec> = HashMap::new();
    let mut disabled: Vec<String> = Vec::new();

    // Project `.mcp.json`
    let repo_root = crate::find_repo_root(cwd);
    if let Some(v) = read_json(&repo_root.join(".mcp.json")) {
        if let Some(map) = v.get("mcpServers").and_then(Value::as_object) {
            let mut tmp = Vec::new();
            parse_json_map(Agent::ClaudeCode, map, &mut tmp);
            for s in tmp {
                by_name.insert(s.name.clone(), s);
            }
        }
    }

    // User `~/.claude.json`: global servers + this project's servers + disabled list
    if let Some(home) = dirs::home_dir() {
        if let Some(v) = read_json(&home.join(".claude.json")) {
            if let Some(map) = v.get("mcpServers").and_then(Value::as_object) {
                let mut tmp = Vec::new();
                parse_json_map(Agent::ClaudeCode, map, &mut tmp);
                for s in tmp {
                    by_name.insert(s.name.clone(), s);
                }
            }
            // Project-scoped block keyed by absolute cwd path.
            if let Some(proj) = v
                .get("projects")
                .and_then(Value::as_object)
                .and_then(|p| p.get(repo_root.to_string_lossy().as_ref()))
            {
                if let Some(map) = proj.get("mcpServers").and_then(Value::as_object) {
                    let mut tmp = Vec::new();
                    parse_json_map(Agent::ClaudeCode, map, &mut tmp);
                    for s in tmp {
                        by_name.insert(s.name.clone(), s);
                    }
                }
                if let Some(arr) = proj.get("disabledMcpjsonServers").and_then(Value::as_array) {
                    disabled.extend(arr.iter().filter_map(|x| x.as_str().map(String::from)));
                }
            }
        }
    }

    by_name
        .into_iter()
        .filter(|(name, _)| !disabled.contains(name))
        .map(|(_, spec)| spec)
        .collect()
}

fn discover_codex() -> Vec<ServerSpec> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let path = home.join(".codex").join("config.toml");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    codex_specs_from_str(&raw)
}

fn codex_specs_from_str(raw: &str) -> Vec<ServerSpec> {
    let Ok(val) = toml::from_str::<toml::Value>(raw) else {
        return Vec::new();
    };
    let Some(table) = val.get("mcp_servers").and_then(toml::Value::as_table) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for (name, entry) in table {
        if let Some(url) = entry.get("url").and_then(toml::Value::as_str) {
            out.push(ServerSpec {
                agent: Agent::Codex,
                name: name.clone(),
                transport: Transport::Http {
                    url: url.to_string(),
                },
            });
            continue;
        }
        let Some(command) = entry.get("command").and_then(toml::Value::as_str) else {
            continue;
        };
        let args = entry
            .get("args")
            .and_then(toml::Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let env = entry
            .get("env")
            .and_then(toml::Value::as_table)
            .map(|t| {
                t.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        out.push(ServerSpec {
            agent: Agent::Codex,
            name: name.clone(),
            transport: Transport::Stdio {
                command: command.to_string(),
                args,
                env,
            },
        });
    }
    out
}

fn discover_antigravity() -> Vec<ServerSpec> {
    let Ok(path) = crate::mcp_config_path() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(v) = read_json(&path) {
        if let Some(map) = v.get("mcpServers").and_then(Value::as_object) {
            parse_json_map(Agent::Antigravity, map, &mut out);
        }
    }
    out
}

fn discover_opencode(cwd: &Path) -> Vec<ServerSpec> {
    let repo_root = crate::find_repo_root(cwd);
    let path = repo_root.join("opencode.json");
    let Some(v) = read_json(&path) else {
        return Vec::new();
    };
    let Some(map) = v.get("mcp").and_then(Value::as_object) else {
        return Vec::new();
    };
    opencode_specs_from_map(map)
}

fn opencode_specs_from_map(map: &serde_json::Map<String, Value>) -> Vec<ServerSpec> {
    let mut out = Vec::new();
    for (name, entry) in map {
        match entry.get("type").and_then(Value::as_str) {
            Some("remote") => {
                if let Some(url) = entry.get("url").and_then(Value::as_str) {
                    out.push(ServerSpec {
                        agent: Agent::OpenCode,
                        name: name.clone(),
                        transport: Transport::Http {
                            url: url.to_string(),
                        },
                    });
                }
            }
            Some("local") => {
                let Some(command) = entry
                    .get("command")
                    .and_then(Value::as_array)
                    .and_then(|arr| arr.first())
                    .and_then(Value::as_str)
                else {
                    continue;
                };
                let args = entry
                    .get("command")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .skip(1)
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let env = entry
                    .get("environment")
                    .and_then(Value::as_object)
                    .map(|o| {
                        o.iter()
                            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                            .collect()
                    })
                    .unwrap_or_default();
                out.push(ServerSpec {
                    agent: Agent::OpenCode,
                    name: name.clone(),
                    transport: Transport::Stdio {
                        command: command.to_string(),
                        args,
                        env,
                    },
                });
            }
            _ => {}
        }
    }
    out
}

/// Copilot config discovery is best-effort across the known VS Code / CLI
/// locations. The server map may live under `servers` (VS Code) or `mcpServers`.
fn discover_copilot(cwd: &Path) -> Vec<ServerSpec> {
    let repo_root = crate::find_repo_root(cwd);
    let mut candidates = vec![repo_root.join(".vscode").join("mcp.json")];
    if let Some(cfg) = dirs::config_dir() {
        // Windows: %APPDATA%\Code\User\mcp.json ; Linux: ~/.config/Code/User/mcp.json
        candidates.push(cfg.join("Code").join("User").join("mcp.json"));
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".copilot").join("mcp-config.json"));
    }

    let mut by_name: HashMap<String, ServerSpec> = HashMap::new();
    for path in candidates {
        let Some(v) = read_json(&path) else {
            continue;
        };
        let map = v
            .get("servers")
            .and_then(Value::as_object)
            .or_else(|| v.get("mcpServers").and_then(Value::as_object));
        if let Some(map) = map {
            let mut tmp = Vec::new();
            parse_json_map(Agent::Copilot, map, &mut tmp);
            for s in tmp {
                by_name.insert(s.name.clone(), s);
            }
        }
    }
    by_name.into_values().collect()
}

// ---------------------------------------------------------------------------
// Minimal synchronous MCP stdio client
// ---------------------------------------------------------------------------

fn build_command(command: &str, args: &[String]) -> Command {
    #[cfg(windows)]
    {
        // npx/uvx/etc. are .cmd shims that CreateProcess can't launch directly;
        // route bare commands through cmd.exe.
        let lower = command.to_ascii_lowercase();
        let direct = lower.ends_with(".exe")
            || lower.ends_with(".bat")
            || lower.ends_with(".cmd")
            || command.contains('/')
            || command.contains('\\');
        if !direct {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(command);
            c.args(args);
            return c;
        }
    }
    let mut c = Command::new(command);
    c.args(args);
    c
}

/// Spawn an MCP server, run `initialize` + `tools/list`, and tokenize the
/// returned tool schemas. Failures are captured (never panic) so one bad server
/// doesn't abort the audit.
fn introspect_stdio(command: &str, args: &[String], env: &[(String, String)]) -> Status {
    let mut cmd = build_command(command, args);
    cmd.envs(env.iter().cloned())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Status::Error(format!("spawn failed: {e}")),
    };

    let mut stdin = match child.stdin.take() {
        Some(s) => s,
        None => return Status::Error("no stdin".to_string()),
    };
    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => return Status::Error("no stdout".to_string()),
    };

    let (tx, rx) = mpsc::channel::<String>();
    thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            match line {
                Ok(l) => {
                    if tx.send(l).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let result = (|| -> Result<Status, String> {
        // initialize
        send(
            &mut stdin,
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {"name": "tokenix-audit", "version": env!("CARGO_PKG_VERSION")}
                }
            }),
        )?;
        await_response(&rx, 1)?;

        // initialized notification (no id)
        send(
            &mut stdin,
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        )?;

        // tools/list, following pagination
        let mut tools = 0usize;
        let mut tokens = 0usize;
        let mut cursor: Option<String> = None;
        let mut id = 2i64;
        loop {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            send(
                &mut stdin,
                &json!({"jsonrpc": "2.0", "id": id, "method": "tools/list", "params": params}),
            )?;
            let result = await_response(&rx, id)?;
            if let Some(arr) = result.get("tools").and_then(Value::as_array) {
                tools += arr.len();
                for tool in arr {
                    tokens += count_tokens(&tool.to_string());
                }
            }
            match result.get("nextCursor").and_then(Value::as_str) {
                Some(c) => {
                    cursor = Some(c.to_string());
                    id += 1;
                }
                None => break,
            }
        }
        Ok(Status::Ok { tools, tokens })
    })();

    let _ = child.kill();
    let _ = child.wait();

    match result {
        Ok(s) => s,
        Err(e) => Status::Error(e),
    }
}

fn send(stdin: &mut impl Write, msg: &Value) -> Result<(), String> {
    let line = format!("{msg}\n");
    stdin
        .write_all(line.as_bytes())
        .and_then(|_| stdin.flush())
        .map_err(|e| format!("write failed: {e}"))
}

/// Read newline-delimited JSON-RPC until a message with `id` arrives (ignoring
/// notifications and other ids), bounded by `INTROSPECT_TIMEOUT`.
fn await_response(rx: &mpsc::Receiver<String>, id: i64) -> Result<Value, String> {
    let start = Instant::now();
    loop {
        let remaining = INTROSPECT_TIMEOUT
            .checked_sub(start.elapsed())
            .ok_or("timeout")?;
        match rx.recv_timeout(remaining) {
            Ok(line) => {
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if v.get("id").and_then(Value::as_i64) == Some(id) {
                    if let Some(err) = v.get("error") {
                        return Err(format!("server error: {err}"));
                    }
                    return Ok(v.get("result").cloned().unwrap_or_else(|| json!({})));
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => return Err("timeout".to_string()),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("server closed connection".to_string())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Reporting
// ---------------------------------------------------------------------------

struct AgentTotals {
    servers: usize,
    tools: usize,
    mcp_tokens: usize,
    native: usize,
    warn: bool,
    reasons: Vec<String>,
}

fn aggregate(
    agent: Agent,
    reports: &[(Agent, String, Status)],
    th: &Thresholds,
) -> (Vec<(String, Status)>, AgentTotals) {
    let mut rows: Vec<(String, Status)> = reports
        .iter()
        .filter(|(a, _, _)| *a == agent)
        .map(|(_, name, status)| (name.clone(), status.clone()))
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));

    let mut tools = 0;
    let mut mcp_tokens = 0;
    for (_, status) in &rows {
        if let Status::Ok { tools: t, tokens } = status {
            tools += t;
            mcp_tokens += tokens;
        }
    }
    let native = agent.native_tokens();
    let mut reasons = Vec::new();
    if mcp_tokens > th.tokens {
        reasons.push(format!("~{mcp_tokens} MCP tool tokens > {}", th.tokens));
    }
    if rows.len() > th.servers {
        reasons.push(format!("{} servers > {}", rows.len(), th.servers));
    }
    if tools > th.tools {
        reasons.push(format!("{tools} tools > {}", th.tools));
    }
    let totals = AgentTotals {
        servers: rows.len(),
        tools,
        mcp_tokens,
        native,
        warn: !reasons.is_empty(),
        reasons,
    };
    (rows, totals)
}

fn print_human(
    agents: &[Agent],
    reports: &[(Agent, String, Status)],
    th: &Thresholds,
    recommend: bool,
    profile_impact: bool,
) {
    println!("{}", "tokenix prompt-audit — MCP/tool weight".bold());
    println!(
        "{}",
        "estimate of the variable system-prompt cost per agent\n".dimmed()
    );

    let mut grand_tokens = 0usize;
    let mut any = false;
    for agent in agents {
        let (rows, totals) = aggregate(*agent, reports, th);
        if rows.is_empty() {
            println!(
                "{}  {}",
                agent.label().bold(),
                "(no MCP config found)".dimmed()
            );
            println!();
            continue;
        }
        any = true;
        let verdict = if totals.warn {
            "WARN".yellow().bold()
        } else {
            "ok".green().bold()
        };
        println!("{}  [{}]", agent.label().bold(), verdict);
        for (name, status) in &rows {
            let detail = match status {
                Status::Ok { tools, tokens } => format!("{tools} tools, ~{tokens} tok").normal(),
                Status::Unknown(why) => format!("unknown ({why})").dimmed(),
                Status::Error(why) => format!("error ({why})").red(),
            };
            println!("    {:<24} {}", name, detail);
        }
        let total = totals.native + totals.mcp_tokens;
        grand_tokens += total;
        println!(
            "    {} {} servers, {} tools, ~{} MCP tok + ~{} native = {}",
            "└".dimmed(),
            totals.servers,
            totals.tools,
            totals.mcp_tokens,
            totals.native,
            format!("~{total} tok").bold()
        );
        for reason in &totals.reasons {
            println!("      {} {}", "⚠".yellow(), reason.yellow());
        }
        if recommend {
            for rec in recommendations_for_agent(*agent, &rows, &totals, th) {
                println!("      {} {}", "rec:".cyan(), rec.cyan());
            }
        }
        println!();
    }

    if any {
        println!("{} ~{} tok", "combined estimate:".bold(), grand_tokens);
    }
    if profile_impact {
        print_profile_impact();
    }
    print_caveats();
}

fn print_caveats() {
    let lines = [
        "Caveats: native-tool baseline is a static approximation; HTTP/SSE servers",
        "are not introspected (shown as unknown); token counts use a ~4-chars/token",
        "estimate. Treat this as a relative bloat indicator, not the exact prompt size.",
    ];
    println!();
    for l in lines {
        println!("{}", l.dimmed());
    }
}

fn print_json(
    agents: &[Agent],
    reports: &[(Agent, String, Status)],
    th: &Thresholds,
    recommend: bool,
    profile_impact: bool,
) {
    let mut agents_json = Vec::new();
    let mut grand_tokens = 0usize;
    for agent in agents {
        let (rows, totals) = aggregate(*agent, reports, th);
        let servers: Vec<Value> = rows
            .iter()
            .map(|(name, status)| match status {
                Status::Ok { tools, tokens } => json!({
                    "name": name, "status": "ok", "tools": tools, "tokens": tokens
                }),
                Status::Unknown(why) => json!({
                    "name": name, "status": "unknown", "reason": why
                }),
                Status::Error(why) => json!({
                    "name": name, "status": "error", "reason": why
                }),
            })
            .collect();
        let total = totals.native + totals.mcp_tokens;
        grand_tokens += total;
        let mut agent_json = json!({
            "agent": agent.key(),
            "label": agent.label(),
            "has_config": !rows.is_empty(),
            "servers": servers,
            "server_count": totals.servers,
            "tool_count": totals.tools,
            "mcp_tokens": totals.mcp_tokens,
            "native_tokens": totals.native,
            "total_tokens": total,
            "warn": totals.warn,
            "reasons": totals.reasons,
        });
        if recommend {
            agent_json["recommendations"] =
                json!(recommendations_for_agent(*agent, &rows, &totals, th));
        }
        agents_json.push(agent_json);
    }
    let out = json!({
        "agents": agents_json,
        "combined_tokens": grand_tokens,
        "tokenix_profile_impact": profile_impact.then(profile_impact_json),
        "thresholds": {
            "tokens": th.tokens, "servers": th.servers, "tools": th.tools
        }
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
}

fn tokenix_profile_counts() -> (usize, usize, usize) {
    let slim_tools = crate::mcp::tool_schema_tokens(crate::mcp::McpProfile::Slim);
    let full_tools = crate::mcp::tool_schema_tokens(crate::mcp::McpProfile::Full);
    let saved = full_tools.saturating_sub(slim_tools);
    (full_tools, slim_tools, saved)
}

fn print_profile_impact() {
    let (full, slim, saved) = tokenix_profile_counts();
    println!();
    println!("{}", "tokenix MCP profile impact".bold());
    println!("  full: ~{full} tool-schema tok");
    println!("  slim: ~{slim} tool-schema tok");
    println!(
        "  saved: ~{} tok ({:.1}%)",
        saved,
        if full > 0 {
            (saved as f64 / full as f64) * 100.0
        } else {
            0.0
        }
    );
}

fn profile_impact_json() -> Value {
    let (full, slim, saved) = tokenix_profile_counts();
    json!({
        "full_tokens": full,
        "slim_tokens": slim,
        "saved_tokens": saved,
        "saved_pct": if full > 0 { (saved as f64 / full as f64) * 100.0 } else { 0.0 }
    })
}

fn recommendations_for_agent(
    agent: Agent,
    rows: &[(String, Status)],
    totals: &AgentTotals,
    th: &Thresholds,
) -> Vec<String> {
    let mut recs = Vec::new();
    if totals.mcp_tokens > th.tokens || totals.tools > th.tools {
        recs.push(
            "enable progressive tool discovery or disable rarely used MCP servers".to_string(),
        );
    }
    if totals.servers > th.servers {
        recs.push(format!(
            "reduce {} MCP servers to {} or fewer for routine sessions",
            totals.servers, th.servers
        ));
    }
    let mut heavy: Vec<_> = rows
        .iter()
        .filter_map(|(name, status)| match status {
            Status::Ok { tokens, .. } => Some((name.as_str(), *tokens)),
            _ => None,
        })
        .collect();
    heavy.sort_by_key(|row| Reverse(row.1));
    for (name, tokens) in heavy.into_iter().take(2) {
        if tokens > 1_500 {
            recs.push(format!(
                "review `{name}` first: ~{tokens} tool-schema tokens"
            ));
        }
    }
    if rows
        .iter()
        .any(|(_, status)| matches!(status, Status::Unknown(_)))
    {
        recs.push(
            "HTTP/SSE servers were not introspected; count them as unknown prompt risk".to_string(),
        );
    }
    if agent == Agent::Antigravity
        && rows
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("tokenix"))
    {
        recs.push(
            "run `tokenix mcp --profile slim` when the host supports a smaller tool surface"
                .to_string(),
        );
    }
    recs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stdio_server() {
        let val = json!({"command": "npx", "args": ["-y", "srv"], "env": {"K": "v"}});
        let spec = parse_json_server(Agent::ClaudeCode, "srv", &val).unwrap();
        match spec.transport {
            Transport::Stdio { command, args, env } => {
                assert_eq!(command, "npx");
                assert_eq!(args, vec!["-y", "srv"]);
                assert_eq!(env, vec![("K".to_string(), "v".to_string())]);
            }
            _ => panic!("expected stdio"),
        }
    }

    #[test]
    fn parses_http_server() {
        let val = json!({"type": "http", "url": "https://example.com/mcp"});
        let spec = parse_json_server(Agent::Copilot, "remote", &val).unwrap();
        assert!(matches!(spec.transport, Transport::Http { .. }));
    }

    #[test]
    fn parses_codex_toml() {
        let raw = r#"
[mcp_servers.docs]
command = "uvx"
args = ["mcp-docs"]

[mcp_servers.docs.env]
TOKEN = "abc"
"#;
        let specs = codex_specs_from_str(raw);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "docs");
        match &specs[0].transport {
            Transport::Stdio { command, args, env } => {
                assert_eq!(command, "uvx");
                assert_eq!(args, &vec!["mcp-docs".to_string()]);
                assert_eq!(env, &vec![("TOKEN".to_string(), "abc".to_string())]);
            }
            _ => panic!("expected stdio"),
        }
    }

    #[test]
    fn parses_opencode_local_mcp() {
        let val = json!({
            "tokenix": {
                "type": "local",
                "command": ["tokenix", "mcp"],
                "environment": {"TOKENIX_PROFILE": "slim"}
            }
        });
        let specs = opencode_specs_from_map(val.as_object().unwrap());
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "tokenix");
        match &specs[0].transport {
            Transport::Stdio { command, args, env } => {
                assert_eq!(command, "tokenix");
                assert_eq!(args, &vec!["mcp".to_string()]);
                assert_eq!(
                    env,
                    &vec![("TOKENIX_PROFILE".to_string(), "slim".to_string())]
                );
            }
            _ => panic!("expected stdio"),
        }
    }

    #[test]
    fn aggregate_flags_token_bloat() {
        let reports = vec![
            (
                Agent::ClaudeCode,
                "a".to_string(),
                Status::Ok {
                    tools: 10,
                    tokens: 9000,
                },
            ),
            (
                Agent::ClaudeCode,
                "b".to_string(),
                Status::Ok {
                    tools: 5,
                    tokens: 5000,
                },
            ),
        ];
        let th = Thresholds {
            tokens: 10_000,
            servers: 5,
            tools: 40,
        };
        let (rows, totals) = aggregate(Agent::ClaudeCode, &reports, &th);
        assert_eq!(rows.len(), 2);
        assert_eq!(totals.tools, 15);
        assert_eq!(totals.mcp_tokens, 14_000);
        assert!(totals.warn);
        assert_eq!(totals.reasons.len(), 1); // only the token threshold trips
    }

    #[test]
    fn aggregate_ok_when_under_thresholds() {
        let reports = vec![(
            Agent::Codex,
            "x".to_string(),
            Status::Ok {
                tools: 3,
                tokens: 500,
            },
        )];
        let th = Thresholds {
            tokens: 10_000,
            servers: 5,
            tools: 40,
        };
        let (_, totals) = aggregate(Agent::Codex, &reports, &th);
        assert!(!totals.warn);
        assert!(totals.reasons.is_empty());
    }
}
