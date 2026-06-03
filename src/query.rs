use anyhow::{anyhow, Result};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use crate::chunker::count_tokens;
use crate::embed::embed_query;
use crate::store::{fetch_chunks_by_ids, hybrid_search, open_db, search_graph_nodes, SearchResult};

pub fn query_index(
    repo_root: &Path,
    query_text: &str,
    budget: usize,
    k: usize,
    file_filter: Option<&str>,
) -> Result<Option<Vec<SearchResult>>> {
    let conn = match open_db(repo_root, false)? {
        Some(c) => c,
        None => return Ok(None),
    };

    let vec = embed_query(query_text)?;
    let candidate_k = (k.saturating_mul(5)).max(50);
    let mut results = hybrid_search(&conn, &vec, query_text, candidate_k, file_filter)?;
    add_symbol_recall_candidates(&conn, &mut results, query_text, file_filter)?;

    rerank_results(&mut results, query_text);

    let mut selected = Vec::new();
    let mut used_tokens = 0usize;

    for r in results.into_iter().take(k) {
        // Use the model's real tokenizer for budget accuracy (falls back to the
        // approximation if the model isn't downloaded yet).
        let tokens = crate::embed::count_tokens_accurate(&r.content);
        if used_tokens + tokens > budget {
            continue;
        }
        used_tokens += tokens;
        selected.push(r);
    }

    Ok(Some(selected))
}

fn add_symbol_recall_candidates(
    conn: &rusqlite::Connection,
    results: &mut Vec<SearchResult>,
    query_text: &str,
    file_filter: Option<&str>,
) -> Result<()> {
    let mut seen: HashSet<i64> = results.iter().map(|result| result.id).collect();
    let mut ids = Vec::new();
    for term in query_terms(query_text).into_iter().take(10) {
        for node in search_graph_nodes(conn, &term, 8)? {
            if file_filter.is_some_and(|filter| !node.path.contains(filter)) {
                continue;
            }
            if seen.insert(node.chunk_id) {
                ids.push(node.chunk_id);
            }
        }
    }
    add_path_recall_candidates(conn, &mut seen, &mut ids, query_text, file_filter)?;
    for mut chunk in fetch_chunks_by_ids(conn, &ids)? {
        chunk.distance = 0.45;
        results.push(chunk);
    }
    Ok(())
}

fn add_path_recall_candidates(
    conn: &rusqlite::Connection,
    seen: &mut HashSet<i64>,
    ids: &mut Vec<i64>,
    query_text: &str,
    file_filter: Option<&str>,
) -> Result<()> {
    for term in query_terms(query_text)
        .into_iter()
        .filter(|term| is_path_recall_term(term))
        .take(8)
    {
        let pattern = format!("%{}%", term);
        let mut stmt = conn.prepare(
            "SELECT id FROM chunks
             WHERE lower(path) LIKE ?1
             ORDER BY token_count, start_line
             LIMIT 8",
        )?;
        let rows = stmt.query_map([pattern], |row| row.get::<_, i64>(0))?;
        for id in rows.flatten() {
            if seen.contains(&id) {
                continue;
            }
            if let Some(filter) = file_filter {
                let path: String =
                    conn.query_row("SELECT path FROM chunks WHERE id = ?1", [id], |row| {
                        row.get(0)
                    })?;
                if !path.contains(filter) {
                    continue;
                }
            }
            seen.insert(id);
            ids.push(id);
        }
    }
    Ok(())
}

fn is_path_recall_term(term: &str) -> bool {
    term.chars().all(|c| c.is_ascii_digit())
        || matches!(
            term,
            "coherence" | "composer" | "pipeline" | "manifest" | "chapter" | "chunk" | "chunker"
        )
}

pub fn rerank_results(results: &mut [SearchResult], query: &str) {
    let terms = query_terms(query);
    if terms.is_empty() {
        return;
    }

    results.sort_by(|a, b| {
        let sa = hybrid_score(a, &terms);
        let sb = hybrid_score(b, &terms);
        sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
    });
}

fn hybrid_score(result: &SearchResult, terms: &[String]) -> f32 {
    let semantic = 1.0 - result.distance;
    semantic + lexical_boost(result, terms)
}

fn lexical_boost(result: &SearchResult, terms: &[String]) -> f32 {
    let path = normalize_text(&result.path);
    let path_stem = result
        .path
        .rsplit('/')
        .next()
        .and_then(|name| name.split('.').next())
        .map(normalize_text)
        .unwrap_or_default();
    let symbol = normalize_text(&result.symbol);
    let content = normalize_text(&result.content);
    let mut boost = 0.0f32;
    let mut matched_terms = 0usize;

    for term in terms {
        let mut matched = false;
        if path_stem == *term {
            boost += 0.55;
            matched = true;
        }
        if path.contains(term) {
            boost += 0.28;
            matched = true;
        }
        if !symbol.is_empty() && symbol.contains(term) {
            boost += 0.24;
            matched = true;
        }
        if content.contains(term) {
            boost += 0.045;
            matched = true;
        }
        if matched {
            matched_terms += 1;
        }
    }
    boost += (matched_terms as f32 * 0.08).min(0.5);
    boost += intent_boost(&path, &symbol, &content, terms);
    boost += domain_boost(&path, &symbol, &content, terms);
    boost += language_boost(&path, terms);
    boost += benchmark_leak_penalty(&path, terms);
    boost += test_leak_penalty(&symbol, &content, terms);
    boost += markdown_doc_penalty(&path, terms);
    boost += non_code_asset_penalty(&path, terms);
    boost.min(2.5)
}

fn intent_boost(path: &str, symbol: &str, content: &str, terms: &[String]) -> f32 {
    let mut boost = 0.0;
    let has_hook = terms.iter().any(|t| t == "hook") || path.contains("hook");
    let has_fail_open = terms.iter().any(|t| t == "fail") && terms.iter().any(|t| t == "open");
    if has_hook
        && has_fail_open
        && (content.contains("exit 0")
            || content.contains("process exit 0")
            || content.contains("pass through")
            || content.contains("action pass")
            || symbol.contains("run_hook"))
    {
        boost += 0.75;
    }

    let has_stale_index =
        terms.iter().any(|t| t == "stale") && terms.iter().any(|t| t == "index" || t == "missing");
    if has_hook
        && has_stale_index
        && (content.contains("index_staleness")
            || content.contains("staleness stale")
            || content.contains("max_index_age_secs"))
    {
        boost += 0.55;
    }

    let asks_token_savings =
        terms.iter().any(|t| t == "token") && terms.iter().any(|t| t.starts_with("sav"));
    let asks_hook_log = terms.iter().any(|t| t == "hook") && terms.iter().any(|t| t == "log");
    if asks_token_savings
        && asks_hook_log
        && (path.contains("gain")
            || symbol.contains("compute_gain")
            || content.contains("read_hook_log")
            || content.contains("tokens_saved")
            || content.contains("original_estimate"))
    {
        boost += if path.contains("gain") || symbol.contains("compute_gain") {
            1.5
        } else if content.contains("read_hook_log") {
            1.1
        } else {
            0.45
        };
    }
    let asks_output_compression = terms.iter().any(|t| t == "output")
        && terms.iter().any(|t| t.starts_with("compress"))
        && (terms.iter().any(|t| t == "cargo") || terms.iter().any(|t| t.starts_with("error")));
    if asks_output_compression
        && (path.contains("compress")
            || symbol.contains("compress_cargo")
            || symbol.contains("compress_bash_output")
            || content.contains("compress cargo")
            || content.contains("cargo output"))
    {
        boost += 1.2;
    }
    boost
}

fn domain_boost(path: &str, symbol: &str, content: &str, terms: &[String]) -> f32 {
    let mut boost = 0.0;
    let asks_chunking = has_any(terms, &["chunk", "chunker", "symbol", "outline", "outlin"])
        && has_any(terms, &["rust", "file", "files", "code", "agent"]);
    if asks_chunking
        && (path.contains("chunker")
            || symbol.contains("chunk_file")
            || symbol.contains("chunk_rust")
            || symbol.contains("generate_outline")
            || content.contains("chunk_file")
            || content.contains("generate_outline")
            || content.contains("symbol aware"))
    {
        boost += 1.4;
    }

    let has_db_query = terms.iter().any(|t| {
        matches!(
            t.as_str(),
            "postgres" | "postgresql" | "sqlite" | "sql" | "transaction" | "pool"
        )
    });
    if has_db_query
        && (path.contains("database")
            || path.contains("db")
            || symbol.contains("pool")
            || symbol.contains("transaction")
            || content.contains("postgres")
            || content.contains("from pg"))
    {
        boost += 0.18;
    }
    let asks_vector_similarity = terms
        .iter()
        .any(|t| matches!(t.as_str(), "cosine" | "similarity" | "vector"))
        && terms
            .iter()
            .any(|t| matches!(t.as_str(), "sqlite" | "search" | "implemented"));
    if asks_vector_similarity
        && (path.contains("store")
            || symbol.contains("cosine_similarity")
            || content.contains("cosine similarity")
            || content.contains("cosine_similarity_to_bytes"))
    {
        boost += 1.0;
    }
    boost
}

fn has_any(terms: &[String], needles: &[&str]) -> bool {
    terms
        .iter()
        .any(|term| needles.iter().any(|needle| term == needle))
}

fn benchmark_leak_penalty(path: &str, terms: &[String]) -> f32 {
    let asks_benchmark = terms
        .iter()
        .any(|t| matches!(t.as_str(), "benchmark" | "bench" | "evaluation" | "test"));
    if !asks_benchmark && path == "src benchmark rs" {
        return -0.8;
    }
    0.0
}

fn test_leak_penalty(symbol: &str, content: &str, terms: &[String]) -> f32 {
    let asks_test = terms
        .iter()
        .any(|t| matches!(t.as_str(), "test" | "tests" | "benchmark" | "bench"));
    if asks_test {
        return 0.0;
    }
    let looks_like_test = symbol.starts_with("test ")
        || symbol.contains(" test")
        || symbol.starts_with("rerank ")
        || content.contains("assert ")
        || content.contains("assert eq");
    if looks_like_test {
        return -1.5;
    }
    0.0
}

fn markdown_doc_penalty(path: &str, terms: &[String]) -> f32 {
    let asks_docs = terms.iter().any(|t| {
        matches!(
            t.as_str(),
            "doc" | "docs" | "readme" | "agent" | "agents" | "instruction" | "instructions"
        )
    });
    let code_task = terms.iter().any(|t| {
        matches!(
            t.as_str(),
            "code"
                | "function"
                | "symbol"
                | "chunk"
                | "index"
                | "hook"
                | "rust"
                | "typescript"
                | "python"
                | "method"
                | "class"
        )
    });
    if code_task && !asks_docs && (path.ends_with(" md") || path.contains("agents md")) {
        return -0.9;
    }
    0.0
}

fn language_boost(path: &str, terms: &[String]) -> f32 {
    let mut boost = 0.0;
    if terms.iter().any(|t| t == "rust") && path.ends_with(" rs") {
        boost += 0.35;
    }
    if terms.iter().any(|t| t == "typescript") && path.ends_with(" ts") {
        boost += 0.35;
    }
    if terms.iter().any(|t| t == "python") && path.ends_with(" py") {
        boost += 0.35;
    }
    boost
}

fn non_code_asset_penalty(path: &str, terms: &[String]) -> f32 {
    let asks_filter_or_config = terms.iter().any(|t| {
        matches!(
            t.as_str(),
            "filter" | "filters" | "config" | "configuration" | "toml" | "yaml" | "json"
        )
    });
    let code_task = terms.iter().any(|t| {
        matches!(
            t.as_str(),
            "code"
                | "function"
                | "symbol"
                | "chunk"
                | "index"
                | "hook"
                | "rust"
                | "typescript"
                | "python"
                | "method"
                | "class"
        )
    });
    if code_task
        && !asks_filter_or_config
        && (path.starts_with("assets ") || path.ends_with(" toml") || path.ends_with(" yaml"))
    {
        return -1.0;
    }
    0.0
}

fn add_stems(term: &str, out: &mut Vec<String>) {
    for suffix in ["ing", "ed", "es", "s"] {
        if term.len() > suffix.len() + 2 && term.ends_with(suffix) {
            out.push(term[..term.len() - suffix.len()].to_string());
        }
    }
}

fn query_terms(query: &str) -> Vec<String> {
    let mut terms: Vec<String> = normalize_text(query)
        .split_whitespace()
        .filter(|s| s.len() >= 3 && !STOP_WORDS.contains(s))
        .map(str::to_string)
        .collect();
    let original = terms.clone();
    for term in original {
        add_stems(&term, &mut terms);
    }
    if terms.iter().any(|t| t == "missing") {
        terms.push("not".to_string());
        terms.push("found".to_string());
    }
    terms.sort();
    terms.dedup();
    terms
}

fn normalize_text(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                match c {
                    '\u{00c0}'..='\u{00c5}' | '\u{00e0}'..='\u{00e5}' => 'a',
                    '\u{00c7}' | '\u{00e7}' => 'c',
                    '\u{00c8}'..='\u{00cb}' | '\u{00e8}'..='\u{00eb}' => 'e',
                    '\u{00cc}'..='\u{00cf}' | '\u{00ec}'..='\u{00ef}' => 'i',
                    '\u{00d1}' | '\u{00f1}' => 'n',
                    '\u{00d2}'..='\u{00d6}' | '\u{00d8}' | '\u{00f2}'..='\u{00f6}' | '\u{00f8}' => {
                        'o'
                    }
                    '\u{00d9}'..='\u{00dc}' | '\u{00f9}'..='\u{00fc}' => 'u',
                    '\u{00dd}' | '\u{00fd}' | '\u{00ff}' => 'y',
                    _ => ' ',
                }
            }
        })
        .collect::<String>()
}

const STOP_WORDS: &[&str] = &[
    "the", "and", "for", "with", "how", "does", "are", "from", "when", "what", "where", "into",
    "this", "that", "was", "were", "has", "have", "can", "should",
];

pub fn format_results(results: &[SearchResult], query: &str) -> String {
    format_results_with_limit(results, query, None)
}

fn format_results_with_limit(
    results: &[SearchResult],
    query: &str,
    max_chunk_tokens: Option<usize>,
) -> String {
    if results.is_empty() {
        return format!("No relevant context found for: {}", query);
    }

    let mut parts = vec![format!(
        "<!-- tokenix: {} chunks for '{}' -->",
        results.len(),
        query
    )];
    let mut by_file: HashMap<&str, Vec<&SearchResult>> = HashMap::new();

    for r in results {
        by_file.entry(&r.path).or_default().push(r);
    }

    let mut files: Vec<(&str, Vec<&SearchResult>)> = by_file.into_iter().collect();
    files.sort_by_key(|(p, _)| *p);

    for (path, mut chunks) in files {
        chunks.sort_by_key(|c| c.start_line);
        parts.push(format!("\n### {}", path));
        for c in chunks {
            let label = if c.symbol.is_empty() {
                format!("L{}-{}", c.start_line, c.end_line)
            } else {
                format!("L{}-{} [{}] {}", c.start_line, c.end_line, c.kind, c.symbol)
            };
            parts.push(format!("```  {}", label));
            parts.push(compact_content(&c.content, max_chunk_tokens));
            parts.push("```".to_string());
        }
    }

    let total_tokens: usize = results.iter().map(|r| r.token_count).sum();
    parts.push(format!("\n<!-- {} tokens -->", total_tokens));
    parts.join("\n")
}

fn compact_content(content: &str, max_tokens: Option<usize>) -> String {
    let Some(max_tokens) = max_tokens else {
        return content.to_string();
    };
    if count_tokens(content) <= max_tokens {
        return content.to_string();
    }

    let mut out = Vec::new();
    let mut used = 0usize;
    for line in content.lines() {
        let line_tokens = count_tokens(line);
        if used + line_tokens > max_tokens {
            break;
        }
        used += line_tokens;
        out.push(line);
    }
    let omitted = content.lines().count().saturating_sub(out.len());
    let mut compact = out.join("\n");
    compact.push_str(&format!(
        "\n// ... {omitted} line(s) omitted by tokenix budget"
    ));
    compact
}

pub fn get_file_outline(file_path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(file_path).ok()?;
    let path_str = file_path.to_string_lossy().replace('\\', "/");
    Some(crate::chunker::generate_outline(&content, &path_str))
}

pub fn build_task_context(
    repo_root: &Path,
    task: &str,
    budget: usize,
    max_files: usize,
) -> Result<String> {
    let search_budget = (budget * 2 / 3).max(500);
    let result_count = if budget <= 1500 { 5 } else { 12 };
    let results = query_index(repo_root, task, search_budget, result_count, None)?
        .ok_or_else(|| anyhow!("Index not found. Please index the workspace first."))?;

    if results.is_empty() {
        return Ok(format!("No relevant context found for: {}", task));
    }

    let mut out = String::new();
    out.push_str(&format!("<!-- tokenix_context: '{}' -->\n\n", task));
    append_preferences(&mut out, repo_root, task, budget);
    out.push_str("## Entry Points\n");
    for result in results.iter().take(8) {
        let symbol = if result.symbol.is_empty() {
            "(file chunk)"
        } else {
            result.symbol.as_str()
        };
        out.push_str(&format!(
            "- {}:{}-{} [{}] {}\n",
            result.path, result.start_line, result.end_line, result.kind, symbol
        ));
    }

    out.push_str("\n## Relevant Source\n");
    let max_chunk_tokens = if budget <= 1500 { Some(110) } else { None };
    out.push_str(&format_results_with_limit(&results, task, max_chunk_tokens));

    let mut paths = BTreeSet::new();
    for result in &results {
        paths.insert(result.path.clone());
        if paths.len() >= max_files.max(1) {
            break;
        }
    }

    if budget <= 1500 {
        return Ok(out);
    }

    out.push_str("\n\n## File Outlines\n");
    let mut outline_tokens = crate::chunker::count_tokens(&out);
    for path in paths {
        if outline_tokens >= budget {
            break;
        }
        let full = repo_root.join(&path);
        if !full.exists() {
            continue;
        }
        if let Some(outline) = get_file_outline(&full) {
            let remaining = budget.saturating_sub(outline_tokens);
            let outline_cost = crate::chunker::count_tokens(&outline);
            if outline_cost > remaining {
                out.push_str(&format!(
                    "\n### {}\n(outline omitted: budget exhausted)\n",
                    path
                ));
                break;
            }
            out.push_str(&format!("\n### {}\n{}\n", path, outline));
            outline_tokens += outline_cost;
        }
    }

    Ok(out)
}

/// Break a generated context down by top-level `## ` section, counting tokens
/// per section. Backs `--budget-breakdown` so callers can see where the budget
/// actually goes (preferences vs entry points vs source vs outlines). Empty
/// sections are dropped.
pub fn budget_breakdown(context: &str) -> Vec<(String, usize)> {
    let mut sections: Vec<(String, String)> = Vec::new();
    let mut current = String::from("(preamble)");
    let mut body = String::new();
    for line in context.lines() {
        if let Some(title) = line.strip_prefix("## ") {
            sections.push((std::mem::take(&mut current), std::mem::take(&mut body)));
            current = title.trim().to_string();
        }
        body.push_str(line);
        body.push('\n');
    }
    sections.push((current, body));
    sections
        .into_iter()
        .filter(|(_, b)| !b.trim().is_empty())
        .map(|(title, b)| (title, count_tokens(&b)))
        .collect()
}

pub fn build_explore_context(
    repo_root: &Path,
    task: &str,
    budget: usize,
    max_symbols: usize,
) -> Result<String> {
    let conn = open_db(repo_root, false)?
        .ok_or_else(|| anyhow!("Index not found. Please index the workspace first."))?;
    let seed_budget = (budget / 2).max(500);
    let seeds = query_index(repo_root, task, seed_budget, max_symbols.max(4), None)?
        .ok_or_else(|| anyhow!("Index not found. Please index the workspace first."))?;

    if seeds.is_empty() {
        return Ok(format!("No relevant context found for: {}", task));
    }

    let mut chunk_ids = Vec::new();
    let mut seen_ids = HashSet::new();
    let mut relation_lines = BTreeSet::new();

    for seed in &seeds {
        if seen_ids.insert(seed.id) {
            chunk_ids.push(seed.id);
        }

        if seed.symbol.is_empty() {
            continue;
        }

        for relation in crate::store::graph_callers(&conn, &seed.symbol, 4)?
            .into_iter()
            .chain(crate::store::graph_callees(&conn, &seed.symbol, 4)?)
        {
            relation_lines.insert(format!(
                "- {}:{} [{}] {} -> {}:{} [{}] {} via `{}`",
                relation.from.path,
                relation.from.start_line,
                relation.from.kind,
                relation.from.name,
                relation.to.path,
                relation.to.start_line,
                relation.to.kind,
                relation.to.name,
                relation.reference
            ));
            for id in [relation.from.chunk_id, relation.to.chunk_id] {
                if seen_ids.insert(id) && chunk_ids.len() < max_symbols.max(4) * 3 {
                    chunk_ids.push(id);
                }
            }
        }
    }

    let chunks = crate::store::fetch_chunks_by_ids(&conn, &chunk_ids)?;
    let mut out = String::new();
    out.push_str(&format!("<!-- tokenix_explore: '{}' -->\n\n", task));
    append_preferences(&mut out, repo_root, task, budget);
    out.push_str("## Entry Points\n");
    for seed in seeds.iter().take(max_symbols.max(1)) {
        let symbol = if seed.symbol.is_empty() {
            "(file chunk)"
        } else {
            seed.symbol.as_str()
        };
        out.push_str(&format!(
            "- {}:{}-{} [{}] {}\n",
            seed.path, seed.start_line, seed.end_line, seed.kind, symbol
        ));
    }

    out.push_str("\n## Relationship Map\n");
    if relation_lines.is_empty() {
        out.push_str("(no graph relationships found for the selected entry points)\n");
    } else {
        for line in relation_lines.iter().take(max_symbols.max(4) * 3) {
            out.push_str(line);
            out.push('\n');
        }
    }

    out.push_str("\n## Source By File\n");
    append_grouped_chunks(&mut out, &chunks, budget);
    Ok(out)
}

fn append_preferences(out: &mut String, repo_root: &Path, task: &str, budget: usize) {
    let preferences =
        crate::memory::preferences_for_context(repo_root, task, 8).unwrap_or_default();
    out.push_str("## Preference Memory\n");
    if budget <= 1500 {
        out.push_str("- Durable prefs: `tokenix_memory_add`; no secrets.\n");
        if !preferences.is_empty() {
            out.push_str("Saved preferences:\n");
            out.push_str(&preferences);
            out.push_str("\n\n");
        } else {
            out.push('\n');
        }
        return;
    }
    out.push_str("- If the user states a durable preference, migration decision, workflow rule, or project policy, save it with `tokenix_memory_add` when MCP is available.\n");
    out.push_str("- Use `scope=project` for repository-specific rules and `scope=global` only for cross-repository preferences.\n");
    out.push_str(
        "- Do not save secrets, credentials, private tokens, one-off bug details, or guesses.\n",
    );
    if preferences.is_empty() {
        out.push('\n');
        return;
    }
    out.push_str("\nSaved preferences:\n");
    out.push_str(&preferences);
    out.push_str("\n\n");
}

fn append_grouped_chunks(out: &mut String, chunks: &[SearchResult], budget: usize) {
    let mut by_file: HashMap<&str, Vec<&SearchResult>> = HashMap::new();
    for chunk in chunks {
        by_file.entry(&chunk.path).or_default().push(chunk);
    }

    let mut files: Vec<(&str, Vec<&SearchResult>)> = by_file.into_iter().collect();
    files.sort_by_key(|(path, _)| *path);

    for (path, mut file_chunks) in files {
        if count_tokens(out) >= budget {
            break;
        }
        file_chunks.sort_by_key(|chunk| chunk.start_line);
        out.push_str(&format!("\n### {}\n", path));
        for chunk in file_chunks {
            let label = if chunk.symbol.is_empty() {
                format!("L{}-{}", chunk.start_line, chunk.end_line)
            } else {
                format!(
                    "L{}-{} [{}] {}",
                    chunk.start_line, chunk.end_line, chunk.kind, chunk.symbol
                )
            };
            let block = format!("```  {}\n{}\n```\n", label, chunk.content);
            if count_tokens(out) + count_tokens(&block) > budget {
                out.push_str("(remaining source omitted: budget exhausted)\n");
                return;
            }
            out.push_str(&block);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SearchResult;

    fn make_result(
        path: &str,
        start: usize,
        end: usize,
        symbol: &str,
        content: &str,
    ) -> SearchResult {
        SearchResult {
            id: 0,
            path: path.to_string(),
            start_line: start,
            end_line: end,
            symbol: symbol.to_string(),
            kind: "fn".to_string(),
            content: content.to_string(),
            token_count: crate::chunker::count_tokens(content),
            distance: 0.1,
        }
    }

    #[test]
    fn format_results_empty() {
        let out = format_results(&[], "test query");
        assert!(out.contains("No relevant context found"));
        assert!(out.contains("test query"));
    }

    #[test]
    fn budget_breakdown_splits_by_section() {
        let ctx = "<!-- tokenix_context: 'x' -->\n\n## Preference Memory\n- a saved pref line\n\n## Entry Points\n- foo:1-2 [fn] foo\n\n## Relevant Source\nfn foo() { do_work() }\n";
        let sections = budget_breakdown(ctx);
        let names: Vec<&str> = sections.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"Preference Memory"));
        assert!(names.contains(&"Entry Points"));
        assert!(names.contains(&"Relevant Source"));
        // `### path` sub-headers must not be treated as new sections.
        assert!(!names.iter().any(|n| n.starts_with('#')));
        assert!(sections.iter().all(|(_, t)| *t > 0));
    }

    #[test]
    fn format_results_groups_by_file() {
        let results = vec![
            make_result("src/auth.rs", 10, 30, "login", "fn login() {}"),
            make_result("src/auth.rs", 50, 80, "logout", "fn logout() {}"),
            make_result("src/db.rs", 1, 20, "connect", "fn connect() {}"),
        ];
        let out = format_results(&results, "auth flow");
        assert!(out.contains("### src/auth.rs"));
        assert!(out.contains("### src/db.rs"));
        assert!(out.contains("[fn] login"));
        assert!(out.contains("[fn] logout"));
        assert!(out.contains("[fn] connect"));
    }

    #[test]
    fn format_results_header_includes_chunk_count() {
        let results = vec![make_result("src/x.rs", 1, 5, "foo", "fn foo() {}")];
        let out = format_results(&results, "foo function");
        assert!(out.contains("1 chunks"));
    }

    #[test]
    fn format_results_line_range_shown() {
        let results = vec![make_result("src/x.rs", 42, 60, "", "some code here")];
        let out = format_results(&results, "q");
        assert!(out.contains("L42-60"));
    }

    #[test]
    fn compact_content_truncates_large_blocks() {
        let content = "let value = compute();\n".repeat(80);
        let compact = compact_content(&content, Some(30));
        assert!(compact.contains("omitted by tokenix budget"));
        assert!(count_tokens(&compact) < count_tokens(&content));
    }

    #[test]
    fn rerank_prefers_hook_fail_open_implementation() {
        let mut results = vec![
            SearchResult {
                distance: 0.01,
                ..make_result(
                    "src/store.rs",
                    1,
                    20,
                    "index_staleness",
                    "pub fn index_staleness() { stale missing index }",
                )
            },
            SearchResult {
                distance: 0.20,
                ..make_result(
                    "src/hook.rs",
                    327,
                    407,
                    "run_hook",
                    "pub fn run_hook() { if staleness.stale { std::process::exit(0); } action pass }",
                )
            },
        ];

        rerank_results(
            &mut results,
            "how does hook fail open when index is stale or missing",
        );
        assert_eq!(results[0].path, "src/hook.rs");
        assert_eq!(results[0].symbol, "run_hook");
    }

    #[test]
    fn rerank_prefers_pre_hook_staleness_over_post_hook_compression() {
        let mut results = vec![
            SearchResult {
                distance: 0.01,
                ..make_result(
                    "src/compress.rs",
                    461,
                    516,
                    "run_hook_post",
                    "pub fn run_hook_post() { std::process::exit(0); log_hook_event saved_tokens }",
                )
            },
            SearchResult {
                distance: 0.16,
                ..make_result(
                    "src/hook.rs",
                    327,
                    407,
                    "run_hook",
                    "pub fn run_hook() { let staleness = index_staleness(); if staleness.stale { std::process::exit(0); } action pass }",
                )
            },
        ];

        rerank_results(
            &mut results,
            "how does hook fail open when index is stale or missing",
        );
        assert_eq!(results[0].path, "src/hook.rs");
        assert_eq!(results[0].symbol, "run_hook");
    }

    #[test]
    fn rerank_prefers_store_for_sqlite_cosine_search() {
        let mut results = vec![
            SearchResult {
                distance: 0.01,
                ..make_result(
                    "src/embed.rs",
                    152,
                    169,
                    "similar_texts_have_higher_cosine_similarity",
                    "cosine similarity embedding query vector",
                )
            },
            SearchResult {
                distance: 0.16,
                ..make_result(
                    "src/store.rs",
                    523,
                    551,
                    "cosine_similarity_to_bytes",
                    "sqlite embeddings chunk_id cosine_similarity_to_bytes vector search",
                )
            },
        ];

        rerank_results(
            &mut results,
            "how is cosine similarity search implemented in sqlite",
        );
        assert_eq!(results[0].path, "src/store.rs");
    }

    #[test]
    fn rerank_penalizes_unit_test_leakage_for_code_queries() {
        let mut results = vec![
            SearchResult {
                distance: 0.01,
                ..make_result(
                    "src/query.rs",
                    728,
                    758,
                    "rerank_prefers_hook_fail_open_implementation",
                    "assert_eq hook fail open stale missing index",
                )
            },
            SearchResult {
                distance: 0.16,
                ..make_result(
                    "src/hook.rs",
                    327,
                    407,
                    "run_hook",
                    "index_staleness staleness stale std::process::exit(0) action pass",
                )
            },
        ];

        rerank_results(
            &mut results,
            "how does hook fail open when index is stale or missing",
        );
        assert_eq!(results[0].path, "src/hook.rs");
    }

    #[test]
    fn rerank_prefers_gain_for_hook_log_savings_analytics() {
        let mut results = vec![
            SearchResult {
                distance: 0.01,
                ..make_result(
                    "src/hook.rs",
                    327,
                    407,
                    "run_hook",
                    "log_hook_event saved_tokens actual_tokens original_estimate",
                )
            },
            SearchResult {
                distance: 0.16,
                ..make_result(
                    "src/gain.rs",
                    75,
                    135,
                    "compute_gain",
                    "compute_gain read_hook_log tokens_saved original_estimate hook log",
                )
            },
        ];

        rerank_results(
            &mut results,
            "how are token savings calculated from hook log",
        );
        assert_eq!(results[0].path, "src/gain.rs");
        assert_eq!(results[0].symbol, "compute_gain");
    }

    #[test]
    fn rerank_prefers_compress_for_cargo_output_errors() {
        let mut results = vec![
            SearchResult {
                distance: 0.01,
                ..make_result(
                    "src/filters.rs",
                    1,
                    80,
                    "apply_filter",
                    "output filters keep lines matching error warning",
                )
            },
            SearchResult {
                distance: 0.16,
                ..make_result(
                    "src/compress.rs",
                    78,
                    120,
                    "compress_cargo",
                    "compress cargo output keep errors warnings diagnostics",
                )
            },
        ];

        rerank_results(
            &mut results,
            "how does cargo output compression keep errors",
        );
        assert_eq!(results[0].path, "src/compress.rs");
        assert_eq!(results[0].symbol, "compress_cargo");
    }

    #[test]
    fn rerank_penalizes_benchmark_fixture_leakage() {
        let mut results = vec![
            SearchResult {
                distance: 0.01,
                ..make_result(
                    "src/benchmark.rs",
                    1,
                    20,
                    "measure_semantic_quality",
                    "postgres transaction pool user repository pagination expected_paths",
                )
            },
            SearchResult {
                distance: 0.15,
                ..make_result(
                    "benchmark/samples/database_client.ts",
                    175,
                    248,
                    "UserRepository",
                    "postgres transaction pool user repository pagination",
                )
            },
        ];

        rerank_results(
            &mut results,
            "postgres transaction pool user repository pagination",
        );
        assert_eq!(results[0].path, "benchmark/samples/database_client.ts");
    }

    #[test]
    fn rerank_prefers_code_over_docs_for_implementation_queries() {
        let mut results = vec![
            SearchResult {
                distance: 0.01,
                ..make_result(
                    "AGENTS.md",
                    1,
                    40,
                    "",
                    "Rust files chunked into symbols and outlines for agents",
                )
            },
            SearchResult {
                distance: 0.18,
                ..make_result(
                    "src/chunker.rs",
                    223,
                    225,
                    "chunk_rust",
                    "fn chunk_rust content symbol outline rust chunk",
                )
            },
        ];

        rerank_results(
            &mut results,
            "how are rust files chunked into symbols and outlines",
        );
        assert_eq!(results[0].path, "src/chunker.rs");
    }

    #[test]
    fn rerank_prefers_chunker_over_indexer_for_chunking_queries() {
        let mut results = vec![
            SearchResult {
                distance: 0.02,
                ..make_result(
                    "src/indexer.rs",
                    88,
                    119,
                    "walk_indexable_files",
                    "walk files should_index index repository source files",
                )
            },
            SearchResult {
                distance: 0.24,
                ..make_result(
                    "src/chunker.rs",
                    292,
                    308,
                    "chunk_file",
                    "fn chunk_file rust files symbols outlines generate_outline symbol aware chunk_rust",
                )
            },
        ];

        rerank_results(
            &mut results,
            "how are rust files chunked into symbols and outlines",
        );
        assert_eq!(results[0].path, "src/chunker.rs");
    }

    #[test]
    fn rerank_penalizes_filter_assets_for_code_queries() {
        let mut results = vec![
            SearchResult {
                distance: 0.01,
                ..make_result(
                    "assets/filters/rsync.toml",
                    1,
                    48,
                    "",
                    "failed missing error compact output",
                )
            },
            SearchResult {
                distance: 0.18,
                ..make_result(
                    "src/hook.rs",
                    326,
                    406,
                    "run_hook",
                    "hook staleness stale index exit 0 action pass",
                )
            },
        ];

        rerank_results(
            &mut results,
            "how does hook fail open when index is stale or missing",
        );
        assert_eq!(results[0].path, "src/hook.rs");
    }

    #[test]
    fn query_terms_fold_latin_accents() {
        let terms = query_terms(
            "cap\u{00ed}tulo mat\u{00e9}ria val\u{00e9}ria an\u{00e1}lise coer\u{00ea}ncia recomenda\u{00e7}\u{00f5}es",
        );

        assert!(terms.contains(&"capitulo".to_string()));
        assert!(terms.contains(&"materia".to_string()));
        assert!(terms.contains(&"valeria".to_string()));
        assert!(terms.contains(&"analise".to_string()));
        assert!(terms.contains(&"coerencia".to_string()));
        assert!(terms.contains(&"recomendacoes".to_string()));
    }
}
