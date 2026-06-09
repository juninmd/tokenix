//! `tokenix scan-secrets` — gitleaks-style credential scan of AI agent
//! conversation transcripts (Claude Code, Gemini CLI, Copilot CLI, Antigravity).
//!
//! Unlike `gitleaks --no-git`, which scans a working tree, this walks the local
//! conversation logs each agent writes under the user's home directory and flags
//! anything that looks like a live credential pasted into the dialogue. Findings
//! are redacted by default; `--reveal` opts into printing raw values (with a
//! stderr warning) for triage.

use anyhow::Result;
use colored::Colorize;
use regex::Regex;
use rust_embed::Embed;
use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Skip files larger than this; conversation logs stay well under it and the cap
/// avoids reading a stray multi-GB blob into memory.
const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;

/// Text extensions worth scanning. Binary agent artifacts (e.g. `.pb`) are skipped.
const TEXT_EXTS: &[&str] = &[
    "jsonl", "json", "ndjson", "log", "md", "txt", "yaml", "yml", "pbtxt",
];

/// Bundled default rule files, embedded at build time. User and per-project
/// rules layer on top at runtime (see [`load_rules`]).
#[derive(Embed)]
#[folder = "assets/secret-rules"]
#[include = "*.toml"]
struct BundledRules;

/// A rule as declared in TOML. See `assets/secret-rules/default.toml` for the schema.
#[derive(Deserialize)]
struct RuleSpec {
    id: String,
    pattern: String,
    #[serde(default)]
    capture: usize,
    #[serde(default)]
    min_entropy: f64,
}

#[derive(Deserialize)]
struct RuleFile {
    #[serde(default)]
    rules: Vec<RuleSpec>,
}

/// A compiled rule ready to match against transcript lines.
struct Rule {
    id: String,
    re: Regex,
    /// Capture group holding the secret (0 = whole match).
    capture: usize,
    /// Minimum Shannon entropy (bits/char) for the secret; 0 disables the gate.
    min_entropy: f64,
}

/// User rule overrides live here; same `[[rules]]` shape as the bundled defaults.
fn user_rules_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tokenix")
        .join("secret-rules")
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

/// Compile rule specs into matchers. Later specs with a duplicate `id` override
/// earlier ones (so user/local rules win over bundled defaults of the same name).
/// An invalid regex is skipped with a stderr warning rather than aborting the scan.
fn compile_rules(specs: Vec<RuleSpec>) -> Vec<Rule> {
    let mut order: Vec<String> = Vec::new();
    let mut by_id: std::collections::HashMap<String, Rule> = std::collections::HashMap::new();
    for spec in specs {
        let re = match Regex::new(&spec.pattern) {
            Ok(re) => re,
            Err(e) => {
                eprintln!(
                    "{} skipping secret rule '{}': invalid regex ({e})",
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
                capture: spec.capture,
                min_entropy: spec.min_entropy,
            },
        );
    }
    order
        .into_iter()
        .filter_map(|id| by_id.remove(&id))
        .collect()
}

/// Only the embedded defaults — used by tests for deterministic behavior.
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

/// Effective ruleset: bundled defaults, then `<repo>/.tokenix/secret-rules/`,
/// then `~/.tokenix/secret-rules/` — later sources override earlier ids and add
/// new ones, so the generic bundled rule keeps running last unless overridden.
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
        .join("secret-rules");
    read_toml_dir(&local, &mut specs);
    read_toml_dir(&user_rules_dir(), &mut specs);
    compile_rules(specs)
}

/// One credential hit. The raw `secret` is retained so `--reveal` can print it;
/// every default code path uses `redacted` only.
struct RawMatch {
    line: usize,
    rule: String,
    secret: String,
    redacted: String,
    length: usize,
    /// Repository (working dir) the secret was exposed in, when recoverable.
    repo: Option<String>,
    /// Git branch active at the time, when recorded.
    branch: Option<String>,
}

/// A finding tied to a concrete agent + file.
struct Finding {
    agent: &'static str,
    file: PathBuf,
    line: usize,
    rule: String,
    secret: String,
    redacted: String,
    length: usize,
    repo: Option<String>,
    branch: Option<String>,
}

/// Shannon entropy in bits/char over the byte distribution of `s`.
fn shannon_entropy(s: &str) -> f64 {
    if s.is_empty() {
        return 0.0;
    }
    let mut counts = [0usize; 256];
    for &b in s.as_bytes() {
        counts[b as usize] += 1;
    }
    let len = s.len() as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / len;
            -p * p.log2()
        })
        .sum()
}

/// Redact a secret: short ones are fully masked; longer ones keep a 4-char prefix
/// and 2-char suffix so distinct leaks stay distinguishable without exposure.
fn redact(secret: &str) -> String {
    let chars: Vec<char> = secret.chars().collect();
    let n = chars.len();
    if n <= 8 {
        return "*".repeat(n);
    }
    let prefix: String = chars[..4].iter().collect();
    let suffix: String = chars[n - 2..].iter().collect();
    format!("{prefix}******{suffix}")
}

/// Pull `cwd` (repo) and `gitBranch` from a transcript line when it is a JSON
/// object carrying them (Claude Code records one per message). Cheap-guarded so
/// JSON is only parsed for lines that actually contain a `cwd` key.
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

/// Core matcher — pure over `content`, so the ruleset is unit-testable without a
/// filesystem. Dedups by `(line, redacted)`; specific rules precede the generic
/// one in `rules()`, so the specific label wins on a shared line. Each line's
/// `cwd`/`gitBranch` (if present) is attached so findings can be attributed to a
/// repository.
fn scan_content(content: &str, rules: &[Rule]) -> Vec<RawMatch> {
    let mut out = Vec::new();
    let mut seen: HashSet<(usize, String)> = HashSet::new();
    for (idx, line) in content.lines().enumerate() {
        let mut line_hits: Vec<RawMatch> = Vec::new();
        for rule in rules {
            for caps in rule.re.captures_iter(line) {
                let Some(m) = caps.get(rule.capture) else {
                    continue;
                };
                let secret = m.as_str();
                if rule.min_entropy > 0.0 && shannon_entropy(secret) < rule.min_entropy {
                    continue;
                }
                let redacted = redact(secret);
                if seen.insert((idx + 1, redacted.clone())) {
                    line_hits.push(RawMatch {
                        line: idx + 1,
                        rule: rule.id.clone(),
                        secret: secret.to_string(),
                        redacted,
                        length: secret.chars().count(),
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

fn has_text_ext(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| TEXT_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// A root directory to walk, optionally restricted to specific file basenames
/// (used to keep Gemini's shared `tmp/` scan focused on chat logs).
struct Root {
    dir: PathBuf,
    only_basenames: Option<&'static [&'static str]>,
}

struct AgentScan {
    name: &'static str,
    roots: Vec<Root>,
}

/// Conversation-transcript locations per agent, relative to the user's home.
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

/// Scan the selected agents, returning findings plus a per-agent scanned-file count.
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
                let repo = m.repo.or_else(|| fallback_repo(agent.name, &file, home));
                findings.push(Finding {
                    agent: agent.name,
                    file: file.clone(),
                    line: m.line,
                    rule: m.rule,
                    secret: m.secret,
                    redacted: m.redacted,
                    length: m.length,
                    repo,
                    branch: m.branch,
                });
            }
        }
    }
    (findings, counts)
}

/// Best-effort project attribution when a line carries no `cwd`: the project
/// directory the transcript lives in. For Claude that is the slugified repo path
/// under `projects/` (e.g. `D--Solutions-pessoal-foo`); for Gemini the named
/// `tmp/<project>` / `history/<project>` dir. Returned with a `~slug:` / `~dir:`
/// marker so it is never confused with a real, exact `cwd` path.
fn fallback_repo(agent: &str, file: &Path, home: &Path) -> Option<String> {
    let rel = file.strip_prefix(home).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    let after = |anchor: &str| -> Option<String> {
        parts
            .iter()
            .position(|p| p == anchor)
            .and_then(|i| parts.get(i + 1))
            .cloned()
    };
    match agent {
        "claude" => after("projects").map(|slug| format!("~slug:{slug}")),
        "gemini" => after("tmp")
            .or_else(|| after("history"))
            .map(|dir| format!("~dir:{dir}")),
        _ => None,
    }
}

/// Replace the home prefix with `~` for compact, non-identifying output.
fn tilde(home: &Path, p: &Path) -> String {
    p.strip_prefix(home)
        .map(|rest| format!("~/{}", rest.display()))
        .unwrap_or_else(|_| p.display().to_string())
        .replace('\\', "/")
}

/// Repository key for grouping/attribution; `<unknown repo>` when unrecoverable.
fn repo_of(f: &Finding) -> String {
    f.repo
        .clone()
        .unwrap_or_else(|| "<unknown repo>".to_string())
}

/// `repo @ branch` display label, or just `repo`, when a repo is known.
fn repo_branch_label(f: &Finding) -> Option<String> {
    f.repo.as_ref().map(|r| match &f.branch {
        Some(b) => format!("{r} @ {b}"),
        None => r.clone(),
    })
}

/// How the human report groups findings.
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum GroupMode {
    None,
    Value,
    Rule,
    Agent,
    File,
    Repo,
}

/// Display + filter options for `scan-secrets`.
pub struct Options {
    /// Agent filter: "all" or a single agent name.
    pub agent: String,
    pub json: bool,
    /// Case-insensitive substring filter over rule / agent / file / value.
    pub search: Option<String>,
    pub group: GroupMode,
    /// Reveal raw secrets instead of redacting them.
    pub reveal: bool,
}

/// Group findings preserving first-seen order of keys.
fn group_by(
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

/// Entry point for `tokenix scan-secrets`. Returns the finding count so the
/// caller can exit non-zero (gitleaks-style) when credentials are present.
pub fn run(opts: Options) -> Result<usize> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("cannot resolve home directory"))?;
    let (mut findings, counts) = scan(&home, &opts.agent);

    // Search filter over rule id, agent, file path, and the (redacted) value.
    if let Some(q) = opts.search.as_ref().map(|s| s.to_lowercase()) {
        findings.retain(|f| {
            f.rule.to_lowercase().contains(&q)
                || f.agent.contains(q.as_str())
                || tilde(&home, &f.file).to_lowercase().contains(&q)
                || f.redacted.to_lowercase().contains(&q)
                || f.repo
                    .as_deref()
                    .is_some_and(|r| r.to_lowercase().contains(&q))
                || f.branch
                    .as_deref()
                    .is_some_and(|b| b.to_lowercase().contains(&q))
        });
    }

    let shown = |f: &Finding| -> String {
        if opts.reveal {
            f.secret.clone()
        } else {
            f.redacted.clone()
        }
    };

    if opts.json {
        let out = serde_json::json!({
            "scanned": counts.iter().map(|(a, n)| serde_json::json!({"agent": a, "files": n})).collect::<Vec<_>>(),
            "count": findings.len(),
            "findings": findings.iter().map(|f| {
                let mut o = serde_json::json!({
                    "agent": f.agent,
                    "file": tilde(&home, &f.file),
                    "line": f.line,
                    "rule": f.rule,
                    "redacted": f.redacted,
                    "length": f.length,
                });
                if let Some(r) = &f.repo {
                    o["repo"] = serde_json::Value::String(r.clone());
                }
                if let Some(b) = &f.branch {
                    o["branch"] = serde_json::Value::String(b.clone());
                }
                if opts.reveal {
                    o["secret"] = serde_json::Value::String(f.secret.clone());
                }
                o
            }).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(findings.len());
    }

    println!(
        "\n{}\n",
        "🔑 tokenix scan-secrets — credential scan of AI agent conversations"
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
            "\n  {} no credentials found in {} conversation files{}\n",
            "✓".green().bold(),
            scanned,
            suffix
        );
        return Ok(0);
    }

    if opts.reveal {
        eprintln!(
            "{}",
            "  ⚠ --reveal: raw secrets are printed below. Do not share this output."
                .red()
                .bold()
        );
    }
    println!(
        "\n  {} {} potential credential(s) found\n",
        "⚠".yellow().bold(),
        findings.len()
    );

    match opts.group {
        GroupMode::None => {
            for f in &findings {
                println!(
                    "  {:<11} {}:{}",
                    f.agent.cyan(),
                    tilde(&home, &f.file),
                    f.line
                );
                if let Some(rl) = repo_branch_label(f) {
                    println!("    {} {}", "repo:".dimmed(), rl.green());
                }
                println!(
                    "    [{}]  {}  {}",
                    f.rule.yellow(),
                    shown(f).red().bold(),
                    format!("({} chars)", f.length).dimmed()
                );
            }
        }
        GroupMode::Value => {
            // One block per distinct secret, with its occurrence locations.
            let groups = group_by(&findings, |f| format!("{}\u{1}{}", f.rule, f.redacted));
            for (_, members) in &groups {
                let first = members[0];
                let mut agents: Vec<&str> = members.iter().map(|m| m.agent).collect();
                agents.sort_unstable();
                agents.dedup();
                println!(
                    "  [{}]  {}  {}",
                    first.rule.yellow(),
                    shown(first).red().bold(),
                    format!(
                        "({}× · {} chars · {})",
                        members.len(),
                        first.length,
                        agents.join(",")
                    )
                    .dimmed()
                );
                let mut repos: Vec<String> =
                    members.iter().filter_map(|m| m.repo.clone()).collect();
                repos.sort_unstable();
                repos.dedup();
                if !repos.is_empty() {
                    println!("      {} {}", "repos:".dimmed(), repos.join(", ").green());
                }
                for m in members.iter().take(8) {
                    println!(
                        "      {}",
                        format!("{}:{}", tilde(&home, &m.file), m.line).dimmed()
                    );
                }
                if members.len() > 8 {
                    println!(
                        "      {}",
                        format!("… +{} more", members.len() - 8).dimmed()
                    );
                }
            }
        }
        GroupMode::Rule | GroupMode::Agent | GroupMode::File | GroupMode::Repo => {
            let groups = match opts.group {
                GroupMode::Rule => group_by(&findings, |f| f.rule.clone()),
                GroupMode::Agent => group_by(&findings, |f| f.agent.to_string()),
                GroupMode::Repo => group_by(&findings, repo_of),
                _ => group_by(&findings, |f| tilde(&home, &f.file)),
            };
            for (label, members) in &groups {
                println!(
                    "  {} {}",
                    label.bold().cyan(),
                    format!("({})", members.len()).dimmed()
                );
                for m in members {
                    // Omit the dimension we grouped on to keep lines compact.
                    let coords = match opts.group {
                        GroupMode::Rule => {
                            format!("{}  {}:{}", m.agent, tilde(&home, &m.file), m.line)
                        }
                        GroupMode::Agent => {
                            format!("[{}]  {}:{}", m.rule, tilde(&home, &m.file), m.line)
                        }
                        GroupMode::Repo => {
                            format!(
                                "[{}]  {}  {}:{}",
                                m.rule,
                                m.agent,
                                tilde(&home, &m.file),
                                m.line
                            )
                        }
                        _ => format!("[{}]  line {}", m.rule, m.line),
                    };
                    // Attribute the repo inline except when it is the grouping key.
                    let repo_suffix = if opts.group != GroupMode::Repo {
                        m.repo
                            .as_deref()
                            .map(|r| format!("  ({r})"))
                            .unwrap_or_default()
                    } else {
                        String::new()
                    };
                    println!(
                        "    {}  {}{}",
                        coords.dimmed(),
                        shown(m).red().bold(),
                        repo_suffix.dimmed()
                    );
                }
            }
        }
    }

    if !opts.reveal {
        println!(
            "\n  {}\n",
            "Findings are redacted. Re-run with --reveal to show raw values. Rotate any real credential.".dimmed()
        );
    } else {
        println!();
    }
    Ok(findings.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(agent: &'static str, rule: &str, redacted: &str) -> Finding {
        Finding {
            agent,
            file: PathBuf::from(format!("{agent}.jsonl")),
            line: 1,
            rule: rule.to_string(),
            secret: format!("{redacted}-raw"),
            redacted: redacted.to_string(),
            length: redacted.len(),
            repo: None,
            branch: None,
        }
    }

    #[test]
    fn extract_line_meta_reads_cwd_and_branch() {
        let line = r#"{"type":"user","cwd":"D:\\repos\\foo","gitBranch":"main","message":"AKIAIOSFODNN7EXAMPLE"}"#;
        let (repo, branch) = extract_line_meta(line);
        assert_eq!(repo.as_deref(), Some("D:\\repos\\foo"));
        assert_eq!(branch.as_deref(), Some("main"));
    }

    #[test]
    fn extract_line_meta_skips_lines_without_cwd() {
        assert_eq!(extract_line_meta("just a plain log line"), (None, None));
        // Empty gitBranch is treated as absent.
        let (repo, branch) = extract_line_meta(r#"{"cwd":"/x","gitBranch":""}"#);
        assert_eq!(repo.as_deref(), Some("/x"));
        assert_eq!(branch, None);
    }

    #[test]
    fn scan_content_attaches_repo_from_same_line() {
        let line = r#"{"cwd":"/srv/app","gitBranch":"dev","secret":"ghp_0123456789abcdefghijklmnopqrstuvwxyzAB"}"#;
        let hits = scan_content(line, &bundled_rules());
        let gh = hits.iter().find(|h| h.rule == "github-token").unwrap();
        assert_eq!(gh.repo.as_deref(), Some("/srv/app"));
        assert_eq!(gh.branch.as_deref(), Some("dev"));
    }

    #[test]
    fn group_by_preserves_first_seen_order_and_collects_members() {
        let findings = vec![
            finding("claude", "aws", "AKIA**LE"),
            finding("gemini", "jwt", "eyJ**zz"),
            finding("copilot", "aws", "AKIA**LE"),
        ];
        let groups = group_by(&findings, |f| f.rule.clone());
        let labels: Vec<&str> = groups.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(labels, vec!["aws", "jwt"], "first-seen order kept");
        assert_eq!(groups[0].1.len(), 2, "both aws findings grouped");
        assert_eq!(groups[1].1.len(), 1);
    }

    #[test]
    fn detects_high_signal_credentials() {
        let content = concat!(
            "user pasted AKIAIOSFODNN7EXAMPLE into the chat\n",
            "and the key sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123 too\n",
            "github ghp_0123456789abcdefghijklmnopqrstuvwxyzAB\n",
        );
        let hits = scan_content(content, &bundled_rules());
        let rules_hit: Vec<&str> = hits.iter().map(|h| h.rule.as_str()).collect();
        assert!(rules_hit.contains(&"aws-access-key-id"), "{rules_hit:?}");
        assert!(rules_hit.contains(&"llm-api-key"), "{rules_hit:?}");
        assert!(rules_hit.contains(&"github-token"), "{rules_hit:?}");
    }

    #[test]
    fn redaction_never_exposes_full_secret() {
        let hits = scan_content("aws=AKIAIOSFODNN7EXAMPLE\n", &bundled_rules());
        let aws = hits.iter().find(|h| h.rule == "aws-access-key-id").unwrap();
        assert!(!aws.redacted.contains("IOSFODNN7"));
        assert_eq!(aws.redacted, "AKIA******LE");
        assert_eq!(aws.length, 20);
    }

    #[test]
    fn generic_rule_ignores_low_entropy_values() {
        // A repetitive placeholder has low entropy and must not be flagged.
        let hits = scan_content("password = aaaaaaaaaaaaaaaaaa\n", &bundled_rules());
        assert!(
            hits.is_empty(),
            "low-entropy placeholder should be ignored: {:?}",
            hits.iter().map(|h| h.rule.as_str()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn generic_rule_flags_high_entropy_assignment() {
        let hits = scan_content("API_KEY: 9f8Xq2Lp7Zr4Tn1Bv6Kd3Mw\n", &bundled_rules());
        assert!(hits.iter().any(|h| h.rule == "generic-secret-assignment"));
    }

    #[test]
    fn clean_conversation_yields_no_findings() {
        let hits = scan_content(
            "how do I configure the API client?\nuse the SDK.\n",
            &bundled_rules(),
        );
        assert!(hits.is_empty());
    }

    #[test]
    fn bundled_rules_compile_and_are_nonempty() {
        // Golden: every pattern in assets/secret-rules/*.toml must compile (no
        // rule silently dropped), and the generic entropy-gated rule must
        // survive parsing with its capture group.
        let mut spec_count = 0;
        for file in BundledRules::iter() {
            let asset = BundledRules::get(&file).expect("embedded asset readable");
            let content = std::str::from_utf8(&asset.data).expect("utf8 rule file");
            spec_count += parse_rule_specs(content).len();
        }
        let rules = bundled_rules();
        assert!(spec_count >= 40, "expected a broad bundled ruleset");
        assert_eq!(
            rules.len(),
            spec_count,
            "every bundled rule must compile — an invalid regex was dropped"
        );
        let generic = rules
            .iter()
            .find(|r| r.id == "generic-secret-assignment")
            .expect("generic rule present");
        assert_eq!(generic.capture, 1);
        assert!(generic.min_entropy > 0.0);
    }

    #[test]
    fn compile_rules_lets_later_specs_override_by_id() {
        // A user rule reusing a bundled id replaces it; unknown ids are added.
        let specs = vec![
            RuleSpec {
                id: "dup".into(),
                pattern: "AAAA".into(),
                capture: 0,
                min_entropy: 0.0,
            },
            RuleSpec {
                id: "dup".into(),
                pattern: "BBBB".into(),
                capture: 0,
                min_entropy: 0.0,
            },
        ];
        let rules = compile_rules(specs);
        assert_eq!(rules.len(), 1);
        assert!(rules[0].re.is_match("BBBB"));
        assert!(!rules[0].re.is_match("AAAA"));
    }

    #[test]
    fn compile_rules_skips_invalid_regex() {
        let specs = vec![RuleSpec {
            id: "bad".into(),
            pattern: "(".into(),
            capture: 0,
            min_entropy: 0.0,
        }];
        assert!(compile_rules(specs).is_empty());
    }
}
