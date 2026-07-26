use anyhow::{anyhow, Result};
use serde::Serialize;
use std::cmp::Reverse;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::chunker::{count_tokens, generate_outline};
use crate::query::query_index;
use crate::store::{get_file_token_counts, open_db};

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum PackProfile {
    Plan,
    Debug,
    Audit,
    Security,
    Review,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum PackFormat {
    Markdown,
    Xml,
    Json,
}

#[derive(Serialize)]
struct JsonPack {
    project: String,
    profile: String,
    budget: usize,
    tokens: usize,
    safety: SafetyReport,
    files: Vec<JsonFile>,
    context: String,
}

#[derive(Serialize)]
struct JsonFile {
    path: String,
    tokens: usize,
    reason: String,
    outline: String,
}

struct PackFile {
    path: String,
    tokens: usize,
    outline: String,
    reason: String,
}

#[derive(Clone, Debug)]
pub struct PackOptions {
    pub profile: PackProfile,
    pub budget: usize,
    pub format: PackFormat,
    pub changed: bool,
    pub since: Option<String>,
    pub token_map: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
struct SafetyReport {
    omitted_sensitive_paths: usize,
    omitted_budget_paths: usize,
}

pub fn build_pack(repo_root: &Path, options: PackOptions) -> Result<String> {
    let conn = open_db(repo_root, false)?
        .ok_or_else(|| anyhow!("Index not found. Please index the workspace first."))?;
    let profile = options.profile;
    let budget = options.budget;
    let task = profile_task(profile);
    let context_budget = (budget / 3).clamp(800, 4_000);
    let context = crate::query::build_task_context_with_mode(
        repo_root,
        task,
        context_mode(profile),
        context_budget,
        4,
    )?;
    let mut selected = selected_paths(repo_root, task, budget / 2)?;
    let mut reasons = std::collections::BTreeMap::<String, String>::new();
    for path in &selected {
        reasons.insert(path.clone(), "semantic".to_string());
    }

    if options.changed || options.since.is_some() {
        for path in changed_paths(repo_root, options.since.as_deref()) {
            if should_pack_path(&path, profile) {
                reasons.insert(path.clone(), "changed".to_string());
                selected.insert(path);
            }
        }
    }

    // Graph centrality (PageRank over the symbol graph) is already computed at
    // index time; use it to decide what fills the remaining slots and, below,
    // what survives the budget. "Biggest file" is a poor proxy for "most
    // important" — a file other code depends on is worth more than a large one.
    let graph_ranks = crate::store::get_file_graph_ranks(&conn).unwrap_or_default();
    let mut by_tokens = get_file_token_counts(&conn)?;
    by_tokens.sort_by(|a, b| {
        let rank_a = graph_ranks
            .get(&a.0.replace('\\', "/"))
            .copied()
            .unwrap_or(0.0);
        let rank_b = graph_ranks
            .get(&b.0.replace('\\', "/"))
            .copied()
            .unwrap_or(0.0);
        rank_b
            .partial_cmp(&rank_a)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| Reverse(a.1).cmp(&Reverse(b.1)))
            .then_with(|| a.0.cmp(&b.0))
    });
    let mut safety = SafetyReport::default();
    for (path, _) in by_tokens {
        if selected.len() >= 24 {
            break;
        }
        if is_sensitive_path(&path.replace('\\', "/").to_ascii_lowercase()) {
            safety.omitted_sensitive_paths += 1;
            continue;
        }
        if should_pack_path(&path, profile) {
            reasons
                .entry(path.clone())
                .or_insert_with(|| "top-file".to_string());
            selected.insert(path);
        }
    }

    // `selected` is a BTreeSet, so iterating it directly meant the budget cut
    // files by *alphabetical order*: a semantic hit or a changed file could be
    // dropped so that an alphabetically-earlier filler file fit. Order by why
    // the file is here, then by graph centrality, then by path for determinism.
    let ordered = order_by_priority(&selected, &reasons, &graph_ranks);

    let mut files = Vec::new();
    let mut used = count_tokens(&context);
    let file_budget = budget.saturating_sub(used).max(400);
    for path in ordered {
        if used >= budget {
            break;
        }
        let full = repo_root.join(&path);
        let Ok(content) = fs::read_to_string(&full) else {
            continue;
        };
        let outline = compact_outline(&path, &content, remaining_file_budget(file_budget, used));
        let tokens = count_tokens(&outline);
        if tokens == 0 || used + tokens > budget {
            safety.omitted_budget_paths += 1;
            continue;
        }
        used += tokens;
        files.push(PackFile {
            reason: reasons
                .remove(&path)
                .unwrap_or_else(|| "selected".to_string()),
            path,
            tokens,
            outline,
        });
    }

    match options.format {
        PackFormat::Markdown => Ok(format_markdown(
            repo_root,
            profile,
            budget,
            used,
            &context,
            &files,
            &safety,
            options.token_map,
        )),
        PackFormat::Xml => Ok(format_xml(
            repo_root,
            profile,
            budget,
            used,
            &context,
            &files,
            &safety,
            options.token_map,
        )),
        PackFormat::Json => format_json(repo_root, profile, budget, used, context, files, safety),
    }
}

/// Rank reasons by how load-bearing they are for the task at hand.
/// `changed` beats `semantic` because a file the user just touched is why they
/// are packing at all; both beat `top-file`, which is only filler.
fn reason_priority(reason: &str) -> u8 {
    match reason {
        "changed" => 0,
        "semantic" => 1,
        _ => 2,
    }
}

/// Order the selected paths so the budget cuts the least important files.
fn order_by_priority(
    selected: &BTreeSet<String>,
    reasons: &std::collections::BTreeMap<String, String>,
    graph_ranks: &std::collections::HashMap<String, f32>,
) -> Vec<String> {
    let mut ordered: Vec<String> = selected.iter().cloned().collect();
    ordered.sort_by(|a, b| {
        let pa = reason_priority(reasons.get(a).map(String::as_str).unwrap_or(""));
        let pb = reason_priority(reasons.get(b).map(String::as_str).unwrap_or(""));
        let ra = graph_ranks
            .get(&a.replace('\\', "/"))
            .copied()
            .unwrap_or(0.0);
        let rb = graph_ranks
            .get(&b.replace('\\', "/"))
            .copied()
            .unwrap_or(0.0);
        pa.cmp(&pb)
            .then_with(|| rb.partial_cmp(&ra).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| a.cmp(b))
    });
    ordered
}

fn selected_paths(repo_root: &Path, task: &str, budget: usize) -> Result<BTreeSet<String>> {
    let results = query_index(repo_root, task, budget.max(4_000), 16, None)?
        .ok_or_else(|| anyhow!("Index not found. Please index the workspace first."))?;
    Ok(results
        .into_iter()
        .filter(|r| should_pack_path(&r.path, PackProfile::Plan))
        .map(|r| r.path)
        .collect())
}

fn compact_outline(path: &str, content: &str, budget: usize) -> String {
    let outline = generate_outline(content, path);
    let mut out = Vec::new();
    let mut used = 0usize;
    for line in outline.lines() {
        let cost = count_tokens(line) + 1;
        if used + cost > budget.max(80) {
            out.push("(outline truncated by tokenix pack budget)");
            break;
        }
        used += cost;
        out.push(line);
    }
    out.join("\n")
}

fn remaining_file_budget(total_file_budget: usize, used: usize) -> usize {
    total_file_budget.saturating_sub(used / 4).clamp(120, 900)
}

fn profile_task(profile: PackProfile) -> &'static str {
    match profile {
        PackProfile::Plan => "repository architecture entry points public interfaces",
        PackProfile::Debug => "failure handling hooks tests diagnostics command output errors",
        PackProfile::Audit => "security supply chain secrets authentication configuration risk",
        PackProfile::Security => "secrets credentials tokens env files security sensitive data",
        PackProfile::Review => "code review regressions tests edge cases public interfaces",
    }
}

fn context_mode(profile: PackProfile) -> crate::query::ContextMode {
    match profile {
        PackProfile::Plan => crate::query::ContextMode::Plan,
        PackProfile::Debug => crate::query::ContextMode::Debug,
        PackProfile::Audit => crate::query::ContextMode::Audit,
        PackProfile::Security => crate::query::ContextMode::Security,
        PackProfile::Review => crate::query::ContextMode::Review,
    }
}

fn should_pack_path(path: &str, profile: PackProfile) -> bool {
    let lower = path.replace('\\', "/").to_ascii_lowercase();
    if is_sensitive_path(&lower) {
        return false;
    }
    if profile == PackProfile::Security {
        return lower.contains("security")
            || lower.contains("auth")
            || lower.contains("hook")
            || lower.contains("mcp")
            || lower.ends_with("cargo.toml")
            || lower.ends_with("deny.toml");
    }
    lower.starts_with("src/")
        || lower.starts_with("docs/")
        || lower.starts_with("scripts/")
        || lower == "readme.md"
        || lower == "agents.md"
        || lower == "cargo.toml"
}

fn is_sensitive_path(lower: &str) -> bool {
    lower.contains("/.env")
        || lower.ends_with(".env")
        || lower.contains("secret")
        || lower.contains("credential")
        || lower.contains("private-key")
        || lower.ends_with(".pem")
        || lower.ends_with(".key")
        || lower.ends_with(".p12")
        || lower.ends_with(".pfx")
        || lower.contains("/target/")
        || lower.contains("/.git/")
}

fn changed_paths(repo_root: &Path, since: Option<&str>) -> BTreeSet<String> {
    let args: Vec<&str> = if let Some(base) = since {
        vec!["diff", "--name-only", base, "--"]
    } else {
        vec!["status", "--short"]
    };
    let Ok(output) = std::process::Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .output()
    else {
        return BTreeSet::new();
    };
    if !output.status.success() {
        return BTreeSet::new();
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .filter_map(|line| {
            let path = if since.is_some() {
                line.trim()
            } else if line.len() >= 4 {
                line[3..].trim()
            } else {
                ""
            };
            (!path.is_empty()).then(|| path.replace('\\', "/"))
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn format_markdown(
    repo_root: &Path,
    profile: PackProfile,
    budget: usize,
    used: usize,
    context: &str,
    files: &[PackFile],
    safety: &SafetyReport,
    token_map: bool,
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# tokenix pack\n\nProject: `{}`\nProfile: `{:?}`\nBudget: `{}` tokens\nPacked: `{}` tokens\n\n",
        repo_root.display(),
        profile,
        budget,
        used
    ));
    out.push_str("## Focused Context\n\n");
    out.push_str(context);
    out.push_str("\n\n## Safety Report\n");
    out.push_str(&format!(
        "- Sensitive paths omitted: {}\n- Budget omissions: {}\n",
        safety.omitted_sensitive_paths, safety.omitted_budget_paths
    ));
    if token_map {
        out.push_str("\n## Token Map\n");
        for file in files {
            out.push_str(&format!(
                "- {}: ~{} tok ({})\n",
                file.path, file.tokens, file.reason
            ));
        }
    }
    out.push_str("\n\n## Repository Map\n");
    for file in files {
        out.push_str(&format!(
            "\n### {} (~{} tok, {})\n",
            file.path, file.tokens, file.reason
        ));
        out.push_str("```text\n");
        out.push_str(&file.outline);
        out.push_str("\n```\n");
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn format_xml(
    repo_root: &Path,
    profile: PackProfile,
    budget: usize,
    used: usize,
    context: &str,
    files: &[PackFile],
    safety: &SafetyReport,
    token_map: bool,
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "<tokenix-pack project=\"{}\" profile=\"{:?}\" budget=\"{}\" tokens=\"{}\">\n",
        escape_xml(&repo_root.display().to_string()),
        profile,
        budget,
        used
    ));
    out.push_str("  <context><![CDATA[");
    out.push_str(context);
    out.push_str("]]></context>\n");
    out.push_str(&format!(
        "  <safety omitted_sensitive_paths=\"{}\" omitted_budget_paths=\"{}\" />\n",
        safety.omitted_sensitive_paths, safety.omitted_budget_paths
    ));
    if token_map {
        out.push_str("  <token-map>\n");
        for file in files {
            out.push_str(&format!(
                "    <entry path=\"{}\" tokens=\"{}\" reason=\"{}\" />\n",
                escape_xml(&file.path),
                file.tokens,
                escape_xml(&file.reason)
            ));
        }
        out.push_str("  </token-map>\n");
    }
    out.push_str("  <files>\n");
    for file in files {
        out.push_str(&format!(
            "    <file path=\"{}\" tokens=\"{}\" reason=\"{}\"><![CDATA[{}]]></file>\n",
            escape_xml(&file.path),
            file.tokens,
            escape_xml(&file.reason),
            file.outline
        ));
    }
    out.push_str("  </files>\n</tokenix-pack>\n");
    out
}

fn format_json(
    repo_root: &Path,
    profile: PackProfile,
    budget: usize,
    used: usize,
    context: String,
    files: Vec<PackFile>,
    safety: SafetyReport,
) -> Result<String> {
    let files = files
        .into_iter()
        .map(|file| JsonFile {
            path: file.path,
            tokens: file.tokens,
            reason: file.reason,
            outline: file.outline,
        })
        .collect();
    let pack = JsonPack {
        project: repo_root.display().to_string(),
        profile: format!("{profile:?}"),
        budget,
        tokens: used,
        safety,
        files,
        context,
    };
    Ok(serde_json::to_string_pretty(&pack)?)
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub fn write_or_print(output: Option<PathBuf>, content: &str) -> Result<()> {
    if let Some(path) = output {
        fs::write(&path, content)?;
        println!("ok pack written: {}", path.display());
    } else {
        println!("{content}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reasons_of(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(p, r)| (p.to_string(), r.to_string()))
            .collect()
    }

    #[test]
    fn budget_order_puts_changed_and_semantic_before_filler() {
        // The defect this pins: `selected` is a BTreeSet, so the budget used to
        // cut files alphabetically — `zzz.rs` (a changed file) lost to `aaa.rs`
        // (filler) purely because of its name.
        let selected: BTreeSet<String> = ["aaa.rs", "mmm.rs", "zzz.rs"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let reasons = reasons_of(&[
            ("aaa.rs", "top-file"),
            ("mmm.rs", "semantic"),
            ("zzz.rs", "changed"),
        ]);
        let ordered = order_by_priority(&selected, &reasons, &std::collections::HashMap::new());
        assert_eq!(ordered, vec!["zzz.rs", "mmm.rs", "aaa.rs"]);
    }

    #[test]
    fn graph_rank_breaks_ties_within_the_same_reason() {
        let selected: BTreeSet<String> = ["a.rs", "b.rs", "c.rs"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let reasons = reasons_of(&[
            ("a.rs", "semantic"),
            ("b.rs", "semantic"),
            ("c.rs", "semantic"),
        ]);
        let ranks: std::collections::HashMap<String, f32> = [
            ("a.rs".to_string(), 0.01),
            ("b.rs".to_string(), 0.90),
            ("c.rs".to_string(), 0.40),
        ]
        .into_iter()
        .collect();
        // The file other code depends on wins, regardless of filename.
        let ordered = order_by_priority(&selected, &reasons, &ranks);
        assert_eq!(ordered, vec!["b.rs", "c.rs", "a.rs"]);
    }

    #[test]
    fn ordering_is_deterministic_without_graph_ranks() {
        // No index graph (fresh repo): fall back to a stable alphabetical order
        // rather than an arbitrary one.
        let selected: BTreeSet<String> = ["b.rs", "a.rs"].iter().map(|s| s.to_string()).collect();
        let reasons = reasons_of(&[("a.rs", "semantic"), ("b.rs", "semantic")]);
        let ranks = std::collections::HashMap::new();
        assert_eq!(
            order_by_priority(&selected, &reasons, &ranks),
            vec!["a.rs", "b.rs"]
        );
    }

    #[test]
    fn unknown_reason_sorts_as_filler() {
        let selected: BTreeSet<String> = ["a.rs", "b.rs"].iter().map(|s| s.to_string()).collect();
        let reasons = reasons_of(&[("a.rs", "mystery"), ("b.rs", "semantic")]);
        let ordered = order_by_priority(&selected, &reasons, &std::collections::HashMap::new());
        assert_eq!(ordered, vec!["b.rs", "a.rs"]);
    }

    #[test]
    fn excludes_sensitive_paths() {
        assert!(!should_pack_path("src/.env", PackProfile::Plan));
        assert!(!should_pack_path(
            "config/private-key.pem",
            PackProfile::Plan
        ));
        assert!(should_pack_path("src/main.rs", PackProfile::Plan));
    }

    #[test]
    fn xml_escapes_attributes() {
        assert_eq!(escape_xml("a&b<c>\""), "a&amp;b&lt;c&gt;&quot;");
    }
}
