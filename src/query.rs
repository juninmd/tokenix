use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;

use crate::chunker::count_tokens;
use crate::embed::embed_query;
use crate::store::{open_db, search_similar, SearchResult};

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
    let mut results = search_similar(&conn, &vec, candidate_k)?;

    if let Some(filter) = file_filter {
        results.retain(|r| r.path.contains(filter));
    }
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
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { ' ' })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SearchResult;

    fn make_result(path: &str, start: usize, end: usize, symbol: &str, content: &str) -> SearchResult {
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
