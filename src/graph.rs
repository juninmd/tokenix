use anyhow::Result;
use regex::Regex;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

use crate::store::{self, GraphNode, GraphRelation};

const KEYWORDS: &[&str] = &[
    "if", "for", "while", "loop", "match", "return", "fn", "function", "class", "struct", "enum",
    "trait", "impl", "let", "const", "static", "async", "await", "where", "switch", "catch",
    "println", "format", "vec", "Some", "None", "Ok", "Err", "new", "default", "clone", "unwrap",
    "expect", "insert", "get", "push", "len", "is_empty",
];

#[derive(Debug, Clone)]
struct ChunkSymbol {
    chunk_id: i64,
    file_id: i64,
    path: String,
    name: String,
    kind: String,
    start_line: usize,
    end_line: usize,
    content: String,
}

pub fn rebuild_symbol_graph(conn: &Connection) -> Result<()> {
    let chunks = load_symbol_chunks(conn)?;
    store::clear_symbol_graph(conn)?;

    for chunk in &chunks {
        store::insert_graph_node(
            conn,
            chunk.chunk_id,
            chunk.file_id,
            &chunk.path,
            &chunk.name,
            &chunk.kind,
            chunk.start_line,
            chunk.end_line,
        )?;
    }

    let mut by_name: HashMap<String, Vec<i64>> = HashMap::new();
    for chunk in &chunks {
        by_name
            .entry(normalize_name(&chunk.name))
            .or_default()
            .push(chunk.chunk_id);
    }

    let mut inserted = HashSet::new();
    for chunk in &chunks {
        for reference in extract_references(&chunk.content) {
            let key = normalize_name(&reference);
            let short_key = reference
                .rsplit("::")
                .next()
                .or_else(|| reference.rsplit('.').next())
                .map(normalize_name)
                .unwrap_or_else(|| key.clone());
            let targets = if let Some(targets) = by_name.get(&key) {
                targets
            } else if is_keyword(&short_key) {
                continue;
            } else if let Some(targets) = by_name.get(&short_key) {
                targets
            } else {
                continue;
            };
            for target in targets {
                if *target == chunk.chunk_id {
                    continue;
                }
                if inserted.insert((chunk.chunk_id, *target)) {
                    store::insert_graph_edge(
                        conn,
                        chunk.chunk_id,
                        *target,
                        &reference,
                        "references",
                    )?;
                }
            }
        }
    }

    Ok(())
}

pub fn format_nodes(nodes: &[GraphNode], title: &str) -> String {
    if nodes.is_empty() {
        return format!("No symbols found for: {title}");
    }
    let mut out = format!("## {title}\n");
    for node in nodes {
        out.push_str(&format!(
            "- {}:{}-{} [{}] {}\n",
            node.path, node.start_line, node.end_line, node.kind, node.name
        ));
    }
    out
}

pub fn format_relations(relations: &[GraphRelation], title: &str) -> String {
    if relations.is_empty() {
        return format!("No graph relationships found for: {title}");
    }
    let mut out = format!("## {title}\n");
    for rel in relations {
        out.push_str(&format!(
            "- {}:{} [{}] {} -> {}:{} [{}] {} via `{}` ({})\n",
            rel.from.path,
            rel.from.start_line,
            rel.from.kind,
            rel.from.name,
            rel.to.path,
            rel.to.start_line,
            rel.to.kind,
            rel.to.name,
            rel.reference,
            rel.edge_kind
        ));
    }
    out
}

fn load_symbol_chunks(conn: &Connection) -> Result<Vec<ChunkSymbol>> {
    let mut stmt = conn.prepare(
        "SELECT id, file_id, path, symbol, kind, start_line, end_line, content
         FROM chunks
         WHERE symbol IS NOT NULL AND symbol != '' AND symbol != 'anonymous'",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(ChunkSymbol {
            chunk_id: row.get(0)?,
            file_id: row.get(1)?,
            path: row.get(2)?,
            name: row.get(3)?,
            kind: row.get(4)?,
            start_line: row.get::<_, i64>(5)? as usize,
            end_line: row.get::<_, i64>(6)? as usize,
            content: row.get(7)?,
        })
    })?;
    Ok(rows.filter_map(|row| row.ok()).collect())
}

fn extract_references(content: &str) -> Vec<String> {
    let call_re = Regex::new(r"\b([A-Za-z_][A-Za-z0-9_]*)\s*\(").unwrap();
    let path_call_re =
        Regex::new(r"\b([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)+)\s*\(").unwrap();
    let method_call_re = Regex::new(r"\.\s*([A-Za-z_][A-Za-z0-9_]*)\s*\(").unwrap();

    let mut refs = HashSet::new();
    for cap in path_call_re.captures_iter(content) {
        if let Some(name) = cap.get(1).map(|m| m.as_str()) {
            refs.insert(name.to_string());
        }
    }
    for cap in method_call_re.captures_iter(content) {
        if let Some(name) = cap.get(1).map(|m| m.as_str()) {
            if !is_keyword(name) {
                refs.insert(name.to_string());
            }
        }
    }
    for cap in call_re.captures_iter(content) {
        if let Some(name) = cap.get(1).map(|m| m.as_str()) {
            if !is_keyword(name) {
                refs.insert(name.to_string());
            }
        }
    }

    let mut refs: Vec<String> = refs.into_iter().collect();
    refs.sort();
    refs
}

fn normalize_name(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

fn is_keyword(name: &str) -> bool {
    KEYWORDS.iter().any(|kw| kw.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{init_schema, insert_chunk, upsert_file, NewChunk};
    use rusqlite::Connection;

    #[test]
    fn extracts_function_and_method_references() {
        let refs =
            extract_references("fn a() { foo(); user.save(); crate::bar::baz(); if ready() {} }");
        assert!(refs.contains(&"foo".to_string()));
        assert!(refs.contains(&"save".to_string()));
        assert!(refs.contains(&"crate::bar::baz".to_string()));
        assert!(refs.contains(&"ready".to_string()));
        assert!(!refs.contains(&"if".to_string()));
    }

    #[test]
    fn rebuild_symbol_graph_links_callers_and_callees() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn, 4).unwrap();
        let file_id = upsert_file(&conn, "src/main.rs", 1.0, "abc").unwrap();
        insert_chunk(
            &conn,
            NewChunk {
                file_id,
                path: "src/main.rs",
                start: 1,
                end: 3,
                symbol: "caller",
                kind: "function",
                content: "fn caller() { callee(); }",
                token_count: 6,
            },
        )
        .unwrap();
        insert_chunk(
            &conn,
            NewChunk {
                file_id,
                path: "src/main.rs",
                start: 5,
                end: 7,
                symbol: "callee",
                kind: "function",
                content: "fn callee() {}",
                token_count: 4,
            },
        )
        .unwrap();

        rebuild_symbol_graph(&conn).unwrap();

        let callers = store::graph_callers(&conn, "callee", 10).unwrap();
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0].from.name, "caller");
        assert_eq!(callers[0].to.name, "callee");

        let callees = store::graph_callees(&conn, "caller", 10).unwrap();
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0].to.name, "callee");
    }
}
