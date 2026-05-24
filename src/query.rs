use anyhow::{anyhow, Result};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use crate::chunker::count_tokens;
use crate::embed::embed_query;
use crate::store::{hybrid_search, open_db, SearchResult};

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

    rerank_results(&mut results, query_text);

    let mut selected = Vec::new();
    let mut used_tokens = 0usize;

    for r in results.into_iter().take(k) {
        let tokens = if r.token_count > 0 {
            r.token_count
        } else {
            count_tokens(&r.content)
        };
        if used_tokens + tokens > budget {
            continue;
        }
        used_tokens += tokens;
        selected.push(r);
    }

    Ok(Some(selected))
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
    let symbol = normalize_text(&result.symbol);
    let content = normalize_text(&result.content);
    let mut boost = 0.0f32;

    for term in terms {
        if path.contains(term) {
            boost += 0.12;
        }
        if !symbol.is_empty() && symbol.contains(term) {
            boost += 0.16;
        }
        if content.contains(term) {
            boost += 0.018;
        }
    }
    boost += domain_boost(&path, &symbol, &content, terms);
    boost.min(0.45)
}

fn domain_boost(path: &str, symbol: &str, content: &str, terms: &[String]) -> f32 {
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
        return 0.18;
    }
    0.0
}

fn query_terms(query: &str) -> Vec<String> {
    normalize_text(query)
        .split_whitespace()
        .filter(|s| s.len() >= 3 && !STOP_WORDS.contains(s))
        .map(str::to_string)
        .collect()
}

fn normalize_text(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
}

const STOP_WORDS: &[&str] = &[
    "the", "and", "for", "with", "how", "does", "are", "from", "when", "what", "where", "into",
    "this", "that", "was", "were", "has", "have", "can", "should",
];

pub fn format_results(results: &[SearchResult], query: &str) -> String {
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
            parts.push(c.content.clone());
            parts.push("```".to_string());
        }
    }

    let total_tokens: usize = results.iter().map(|r| r.token_count).sum();
    parts.push(format!("\n<!-- {} tokens -->", total_tokens));
    parts.join("\n")
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
    let results = query_index(repo_root, task, search_budget, 12, None)?
        .ok_or_else(|| anyhow!("Index not found. Please index the workspace first."))?;

    if results.is_empty() {
        return Ok(format!("No relevant context found for: {}", task));
    }

    let mut out = String::new();
    out.push_str(&format!("<!-- tokenix_context: '{}' -->\n\n", task));
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
    out.push_str(&format_results(&results, task));

    let mut paths = BTreeSet::new();
    for result in &results {
        paths.insert(result.path.clone());
        if paths.len() >= max_files.max(1) {
            break;
        }
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
}
