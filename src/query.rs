use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;

use crate::chunker::count_tokens;
use crate::embed::get_embedding;
use crate::store::{open_db, search_similar, SearchResult};

const OLLAMA_URL: &str = "http://localhost:11434";

pub fn query_index(
    repo_root: &Path,
    query_text: &str,
    budget: usize,
    k: usize,
    model: &str,
    file_filter: Option<&str>,
) -> Result<Option<Vec<SearchResult>>> {
    let conn = match open_db(repo_root, false)? {
        Some(c) => c,
        None => return Ok(None),
    };

    let vec = get_embedding(query_text, model, OLLAMA_URL)?;
    let mut results = search_similar(&conn, &vec, k)?;

    if let Some(filter) = file_filter {
        results.retain(|r| r.path.contains(filter));
    }

    let mut selected = Vec::new();
    let mut used_tokens = 0usize;

    for r in results {
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
