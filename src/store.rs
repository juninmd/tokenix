use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Global storage: ~/.tokenix/{project_id}.{db,log}
// ---------------------------------------------------------------------------

fn global_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".tokenix"))
}

/// 16-char hex identifier derived from the canonical project root path.
pub fn project_id(root: &Path) -> String {
    let s = root.to_string_lossy();
    let mut h = sha2::Sha256::new();
    h.update(s.as_bytes());
    hex::encode(&h.finalize()[..8])
}

/// Walk up from `start` looking for VCS/project-root markers.
/// Falls back to `start` itself if nothing is found.
pub fn find_project_root(start: &Path) -> PathBuf {
    let abs = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    let mut current = abs.as_path();
    let markers: &[&str] = &[
        ".git",
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        ".hg",
    ];
    loop {
        if markers.iter().any(|m| current.join(m).exists()) {
            return current.to_path_buf();
        }
        match current.parent() {
            Some(p) => current = p,
            None => return abs,
        }
    }
}

pub fn db_path(repo_root: &Path) -> PathBuf {
    global_dir()
        .map(|d| d.join(format!("{}.db", project_id(repo_root))))
        .unwrap_or_else(|| repo_root.join(".tokenix/index.db"))
}

pub fn log_path(repo_root: &Path) -> PathBuf {
    global_dir()
        .map(|d| d.join(format!("{}.log", project_id(repo_root))))
        .unwrap_or_else(|| repo_root.join(".tokenix/hook.log"))
}

/// Persist a human-readable name (the absolute project path) alongside the DB.
pub fn write_project_name(repo_root: &Path) -> Result<()> {
    if let Some(dir) = global_dir() {
        std::fs::create_dir_all(&dir)?;
        let name_file = dir.join(format!("{}.name", project_id(repo_root)));
        std::fs::write(name_file, repo_root.to_string_lossy().as_bytes())?;
    }
    Ok(())
}

pub fn open_db(repo_root: &Path, create: bool) -> Result<Option<Connection>> {
    let path = db_path(repo_root);
    if !create && !path.exists() {
        return Ok(None);
    }
    if create {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let conn = Connection::open(&path).context("opening sqlite db")?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
    Ok(Some(conn))
}

pub fn init_schema(conn: &Connection, _dim: usize) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS files (
            id INTEGER PRIMARY KEY,
            path TEXT UNIQUE NOT NULL,
            mtime REAL,
            content_hash TEXT
        );
        CREATE TABLE IF NOT EXISTS chunks (
            id INTEGER PRIMARY KEY,
            file_id INTEGER REFERENCES files(id) ON DELETE CASCADE,
            path TEXT NOT NULL,
            start_line INTEGER,
            end_line INTEGER,
            symbol TEXT,
            kind TEXT,
            content TEXT NOT NULL,
            token_count INTEGER
        );
        CREATE TABLE IF NOT EXISTS embeddings (
            chunk_id INTEGER PRIMARY KEY REFERENCES chunks(id) ON DELETE CASCADE,
            embedding BLOB NOT NULL
        );
        CREATE TABLE IF NOT EXISTS embedding_cache (
            content_hash TEXT PRIMARY KEY,
            embedding BLOB NOT NULL,
            updated_at REAL
        );
        CREATE TABLE IF NOT EXISTS graph_nodes (
            chunk_id INTEGER PRIMARY KEY REFERENCES chunks(id) ON DELETE CASCADE,
            file_id INTEGER REFERENCES files(id) ON DELETE CASCADE,
            path TEXT NOT NULL,
            name TEXT NOT NULL,
            kind TEXT,
            start_line INTEGER,
            end_line INTEGER,
            rank REAL NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS graph_edges (
            id INTEGER PRIMARY KEY,
            caller_chunk_id INTEGER REFERENCES chunks(id) ON DELETE CASCADE,
            callee_chunk_id INTEGER REFERENCES chunks(id) ON DELETE CASCADE,
            reference TEXT NOT NULL,
            edge_kind TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS meta (
            key TEXT PRIMARY KEY,
            value TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_chunks_path ON chunks(path);
        CREATE INDEX IF NOT EXISTS idx_chunks_file ON chunks(file_id);
        CREATE INDEX IF NOT EXISTS idx_graph_nodes_name ON graph_nodes(name);
        CREATE INDEX IF NOT EXISTS idx_graph_edges_caller ON graph_edges(caller_chunk_id);
        CREATE INDEX IF NOT EXISTS idx_graph_edges_callee ON graph_edges(callee_chunk_id);

        CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
            content,
            symbol,
            path,
            content='chunks',
            content_rowid='id'
        );

        CREATE TRIGGER IF NOT EXISTS chunks_ai AFTER INSERT ON chunks BEGIN
            INSERT INTO chunks_fts(rowid, content, symbol, path) VALUES (new.id, new.content, new.symbol, new.path);
        END;

        CREATE TRIGGER IF NOT EXISTS chunks_ad AFTER DELETE ON chunks BEGIN
            INSERT INTO chunks_fts(chunks_fts, rowid, content, symbol, path) VALUES ('delete', old.id, old.content, old.symbol, old.path);
        END;

        CREATE TRIGGER IF NOT EXISTS chunks_au AFTER UPDATE ON chunks BEGIN
            INSERT INTO chunks_fts(chunks_fts, rowid, content, symbol, path) VALUES ('delete', old.id, old.content, old.symbol, old.path);
            INSERT INTO chunks_fts(rowid, content, symbol, path) VALUES (new.id, new.content, new.symbol, new.path);
        END;

        INSERT OR IGNORE INTO chunks_fts(rowid, content, symbol, path) SELECT id, content, symbol, path FROM chunks;
        "#,
    )?;
    // Migration for indexes created before graph centrality: add the rank
    // column if it is missing. Errors when the column already exists are
    // expected and ignored.
    let _ = conn.execute(
        "ALTER TABLE graph_nodes ADD COLUMN rank REAL NOT NULL DEFAULT 0",
        [],
    );
    Ok(())
}

pub fn serialize_vec(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

pub fn deserialize_vec(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
        .collect()
}

pub fn upsert_file(conn: &Connection, path: &str, mtime: f64, hash: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO files(path,mtime,content_hash) VALUES(?1,?2,?3)
         ON CONFLICT(path) DO UPDATE SET mtime=excluded.mtime, content_hash=excluded.content_hash",
        params![path, mtime, hash],
    )?;
    let id: i64 = conn.query_row("SELECT id FROM files WHERE path=?1", params![path], |r| {
        r.get(0)
    })?;
    Ok(id)
}

pub fn delete_chunks_for_file(conn: &Connection, file_id: i64) -> Result<()> {
    // Delete embeddings via JOIN since we cascade on file delete
    conn.execute(
        "DELETE FROM embeddings WHERE chunk_id IN (SELECT id FROM chunks WHERE file_id=?1)",
        params![file_id],
    )?;
    conn.execute(
        "DELETE FROM graph_edges WHERE caller_chunk_id IN (SELECT id FROM chunks WHERE file_id=?1)
         OR callee_chunk_id IN (SELECT id FROM chunks WHERE file_id=?1)",
        params![file_id],
    )?;
    conn.execute("DELETE FROM graph_nodes WHERE file_id=?1", params![file_id])?;
    conn.execute("DELETE FROM chunks WHERE file_id=?1", params![file_id])?;
    Ok(())
}

pub fn delete_file(conn: &Connection, file_id: i64) -> Result<()> {
    delete_chunks_for_file(conn, file_id)?;
    conn.execute("DELETE FROM files WHERE id=?1", params![file_id])?;
    Ok(())
}

pub struct NewChunk<'a> {
    pub file_id: i64,
    pub path: &'a str,
    pub start: usize,
    pub end: usize,
    pub symbol: &'a str,
    pub kind: &'a str,
    pub content: &'a str,
    pub token_count: usize,
}

pub fn insert_chunk(conn: &Connection, chunk: NewChunk<'_>) -> Result<i64> {
    conn.execute(
        "INSERT INTO chunks(file_id,path,start_line,end_line,symbol,kind,content,token_count)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            chunk.file_id,
            chunk.path,
            chunk.start as i64,
            chunk.end as i64,
            chunk.symbol,
            chunk.kind,
            chunk.content,
            chunk.token_count as i64
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn insert_embedding(conn: &Connection, chunk_id: i64, embedding: &[f32]) -> Result<()> {
    let blob = serialize_vec(embedding);
    conn.execute(
        "INSERT OR REPLACE INTO embeddings(chunk_id,embedding) VALUES(?1,?2)",
        params![chunk_id, blob],
    )?;
    Ok(())
}

pub fn cached_embeddings(
    conn: &Connection,
    content_hashes: &[String],
) -> Result<HashMap<String, Vec<f32>>> {
    const SQLITE_IN_BATCH: usize = 500;

    let mut seen = HashSet::new();
    let unique_hashes: Vec<&str> = content_hashes
        .iter()
        .map(String::as_str)
        .filter(|hash| seen.insert(*hash))
        .collect();

    let mut cached = HashMap::new();
    for batch in unique_hashes.chunks(SQLITE_IN_BATCH) {
        if batch.is_empty() {
            continue;
        }

        let placeholders = batch.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT content_hash, embedding FROM embedding_cache WHERE content_hash IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(batch.iter().copied()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?;

        for row in rows {
            let (hash, bytes) = row?;
            cached.insert(hash, deserialize_vec(&bytes));
        }
    }

    Ok(cached)
}

pub fn upsert_embedding_cache(
    conn: &Connection,
    content_hash: &str,
    embedding: &[f32],
) -> Result<()> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    conn.execute(
        "INSERT INTO embedding_cache(content_hash,embedding,updated_at) VALUES(?1,?2,?3)
         ON CONFLICT(content_hash) DO UPDATE SET embedding=excluded.embedding, updated_at=excluded.updated_at",
        params![content_hash, serialize_vec(embedding), now],
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct GraphNode {
    pub chunk_id: i64,
    pub path: String,
    pub name: String,
    pub kind: String,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone)]
pub struct GraphRelation {
    pub from: GraphNode,
    pub to: GraphNode,
    pub reference: String,
    pub edge_kind: String,
}

pub fn clear_symbol_graph(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM graph_edges", [])?;
    conn.execute("DELETE FROM graph_nodes", [])?;
    Ok(())
}

/// Persist PageRank centrality scores onto graph nodes. Called after the edge
/// set is rebuilt so `search_graph_nodes` can break ties by how central a
/// symbol is in the reference graph.
pub fn set_node_ranks(conn: &Connection, ranks: &[(i64, f32)]) -> Result<()> {
    let mut stmt = conn.prepare("UPDATE graph_nodes SET rank = ?2 WHERE chunk_id = ?1")?;
    for (chunk_id, rank) in ranks {
        stmt.execute(params![chunk_id, rank])?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn insert_graph_node(
    conn: &Connection,
    chunk_id: i64,
    file_id: i64,
    path: &str,
    name: &str,
    kind: &str,
    start_line: usize,
    end_line: usize,
) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO graph_nodes(chunk_id,file_id,path,name,kind,start_line,end_line)
         VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![
            chunk_id,
            file_id,
            path,
            name,
            kind,
            start_line as i64,
            end_line as i64
        ],
    )?;
    Ok(())
}

pub fn insert_graph_edge(
    conn: &Connection,
    caller_chunk_id: i64,
    callee_chunk_id: i64,
    reference: &str,
    edge_kind: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO graph_edges(caller_chunk_id,callee_chunk_id,reference,edge_kind)
         VALUES(?1,?2,?3,?4)",
        params![caller_chunk_id, callee_chunk_id, reference, edge_kind],
    )?;
    Ok(())
}

pub fn search_graph_nodes(conn: &Connection, query: &str, limit: usize) -> Result<Vec<GraphNode>> {
    let pattern = format!("%{}%", query);
    let mut stmt = conn.prepare(
        "SELECT chunk_id,path,name,kind,start_line,end_line
         FROM graph_nodes
         WHERE name = ?1 COLLATE NOCASE OR name LIKE ?2 COLLATE NOCASE OR path LIKE ?2 COLLATE NOCASE
         ORDER BY CASE WHEN name = ?1 COLLATE NOCASE THEN 0 ELSE 1 END, rank DESC, path, start_line
         LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![query, pattern, limit as i64], graph_node_from_row)?;
    Ok(rows.filter_map(|row| row.ok()).collect())
}

pub fn graph_callers(conn: &Connection, symbol: &str, limit: usize) -> Result<Vec<GraphRelation>> {
    graph_relations(conn, symbol, limit, true)
}

pub fn graph_callees(conn: &Connection, symbol: &str, limit: usize) -> Result<Vec<GraphRelation>> {
    graph_relations(conn, symbol, limit, false)
}

pub fn graph_impact(
    conn: &Connection,
    symbol: &str,
    depth: usize,
    limit: usize,
) -> Result<Vec<GraphRelation>> {
    let start_ids: Vec<i64> = search_graph_nodes(conn, symbol, 20)?
        .into_iter()
        .map(|node| node.chunk_id)
        .collect();
    if start_ids.is_empty() {
        return Ok(Vec::new());
    }

    let mut relations = Vec::new();
    let mut frontier = start_ids;
    let mut seen_nodes = std::collections::HashSet::new();
    let mut seen_edges = std::collections::HashSet::new();

    for _ in 0..depth.max(1) {
        let mut next = Vec::new();
        for node_id in &frontier {
            if !seen_nodes.insert(*node_id) {
                continue;
            }
            for relation in relations_for_node(conn, *node_id, true)?
                .into_iter()
                .chain(relations_for_node(conn, *node_id, false)?)
            {
                let edge_key = (
                    relation.from.chunk_id,
                    relation.to.chunk_id,
                    relation.reference.clone(),
                );
                if seen_edges.insert(edge_key) {
                    next.push(relation.from.chunk_id);
                    next.push(relation.to.chunk_id);
                    relations.push(relation);
                    if relations.len() >= limit {
                        return Ok(relations);
                    }
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }

    Ok(relations)
}

fn graph_relations(
    conn: &Connection,
    symbol: &str,
    limit: usize,
    callers: bool,
) -> Result<Vec<GraphRelation>> {
    let nodes = search_graph_nodes(conn, symbol, 20)?;
    let mut relations = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for node in nodes {
        for relation in relations_for_node(conn, node.chunk_id, callers)? {
            let key = (
                relation.from.chunk_id,
                relation.to.chunk_id,
                relation.reference.clone(),
            );
            if seen.insert(key) {
                relations.push(relation);
                if relations.len() >= limit {
                    return Ok(relations);
                }
            }
        }
    }
    Ok(relations)
}

fn relations_for_node(
    conn: &Connection,
    chunk_id: i64,
    callers: bool,
) -> Result<Vec<GraphRelation>> {
    let (where_col, other_col) = if callers {
        ("e.callee_chunk_id", "e.caller_chunk_id")
    } else {
        ("e.caller_chunk_id", "e.callee_chunk_id")
    };
    let sql = format!(
        "SELECT
            from_node.chunk_id, from_node.path, from_node.name, from_node.kind, from_node.start_line, from_node.end_line,
            to_node.chunk_id, to_node.path, to_node.name, to_node.kind, to_node.start_line, to_node.end_line,
            e.reference, e.edge_kind
         FROM graph_edges e
         JOIN graph_nodes from_node ON from_node.chunk_id = e.caller_chunk_id
         JOIN graph_nodes to_node ON to_node.chunk_id = e.callee_chunk_id
         WHERE {where_col} = ?1
         ORDER BY {other_col}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![chunk_id], graph_relation_from_row)?;
    Ok(rows.filter_map(|row| row.ok()).collect())
}

fn graph_node_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<GraphNode> {
    Ok(GraphNode {
        chunk_id: row.get(0)?,
        path: row.get(1)?,
        name: row.get(2)?,
        kind: row.get(3)?,
        start_line: row.get::<_, i64>(4)? as usize,
        end_line: row.get::<_, i64>(5)? as usize,
    })
}

fn graph_relation_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<GraphRelation> {
    Ok(GraphRelation {
        from: GraphNode {
            chunk_id: row.get(0)?,
            path: row.get(1)?,
            name: row.get(2)?,
            kind: row.get(3)?,
            start_line: row.get::<_, i64>(4)? as usize,
            end_line: row.get::<_, i64>(5)? as usize,
        },
        to: GraphNode {
            chunk_id: row.get(6)?,
            path: row.get(7)?,
            name: row.get(8)?,
            kind: row.get(9)?,
            start_line: row.get::<_, i64>(10)? as usize,
            end_line: row.get::<_, i64>(11)? as usize,
        },
        reference: row.get(12)?,
        edge_kind: row.get(13)?,
    })
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct SearchResult {
    pub id: i64,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub symbol: String,
    pub kind: String,
    pub content: String,
    pub token_count: usize,
    pub distance: f32,
}

#[allow(dead_code)]
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

pub fn cosine_similarity_to_bytes(query_vec: &[f32], query_norm: f32, bytes: &[u8]) -> f32 {
    let mut dot = 0.0f32;
    let mut nb = 0.0f32;
    for (i, chunk) in bytes.chunks_exact(4).enumerate() {
        if i >= query_vec.len() {
            break;
        }
        let y = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        dot += query_vec[i] * y;
        nb += y * y;
    }
    let nb_sqrt = nb.sqrt();
    if query_norm == 0.0 || nb_sqrt == 0.0 {
        0.0
    } else {
        dot / (query_norm * nb_sqrt)
    }
}

#[allow(clippy::type_complexity)]
pub fn search_similar(
    conn: &Connection,
    query_vec: &[f32],
    k: usize,
    file_filter: Option<&str>,
) -> Result<Vec<SearchResult>> {
    let rows_data: Vec<(Vec<u8>, i64, String, i64, i64, String, String, String, i64)> =
        if let Some(filter) = file_filter {
            let mut stmt = conn.prepare(
            "SELECT c.id, c.path, c.start_line, c.end_line, c.symbol, c.kind, c.content, c.token_count, e.embedding
             FROM embeddings e JOIN chunks c ON c.id = e.chunk_id
             WHERE instr(c.path, ?1) > 0"
        )?;
            let rows = stmt.query_map(params![filter], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(8)?,
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                ))
            })?;
            let collected: Vec<_> = rows.filter_map(|r| r.ok()).collect();
            collected
        } else {
            let mut stmt = conn.prepare(
            "SELECT c.id, c.path, c.start_line, c.end_line, c.symbol, c.kind, c.content, c.token_count, e.embedding
             FROM embeddings e JOIN chunks c ON c.id = e.chunk_id"
        )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(8)?,
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                ))
            })?;
            let collected: Vec<_> = rows.filter_map(|r| r.ok()).collect();
            collected
        };

    let query_norm: f32 = query_vec.iter().map(|x| x * x).sum::<f32>().sqrt();

    use rayon::prelude::*;
    let mut scored: Vec<(f32, SearchResult)> = rows_data
        .into_par_iter()
        .map(|(blob, id, path, sl, el, symbol, kind, content, tc)| {
            let sim = cosine_similarity_to_bytes(query_vec, query_norm, &blob);
            (
                sim,
                SearchResult {
                    id,
                    path,
                    start_line: sl as usize,
                    end_line: el as usize,
                    symbol,
                    kind,
                    content,
                    token_count: tc as usize,
                    distance: 1.0 - sim,
                },
            )
        })
        .collect();

    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    Ok(scored.into_iter().take(k).map(|(_, r)| r).collect())
}

pub fn sanitize_fts_query(query: &str) -> String {
    let mut words = Vec::new();
    for word in query.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-') {
        let trimmed = word.trim();
        if !trimmed.is_empty() {
            let escaped = trimmed.replace('"', "\"\"");
            words.push(format!("\"{}\"", escaped));
        }
    }
    if words.is_empty() {
        "".to_string()
    } else {
        words.join(" OR ")
    }
}

pub fn search_fts(
    conn: &Connection,
    query_text: &str,
    limit: usize,
    file_filter: Option<&str>,
) -> Result<Vec<i64>> {
    let sanitized = sanitize_fts_query(query_text);
    if sanitized.is_empty() {
        return Ok(Vec::new());
    }

    let mut ids = Vec::new();
    if let Some(filter) = file_filter {
        let mut stmt = conn.prepare(
            "SELECT c.id FROM chunks c JOIN chunks_fts f ON c.id = f.rowid
             WHERE chunks_fts MATCH ?1 AND instr(c.path, ?2) > 0 LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![sanitized, filter, limit], |row| {
            row.get::<_, i64>(0)
        })?;
        for id in rows.flatten() {
            ids.push(id);
        }
    } else {
        let mut stmt =
            conn.prepare("SELECT rowid FROM chunks_fts WHERE chunks_fts MATCH ?1 LIMIT ?2")?;
        let rows = stmt.query_map(params![sanitized, limit], |row| row.get::<_, i64>(0))?;
        for id in rows.flatten() {
            ids.push(id);
        }
    }
    Ok(ids)
}

/// Scan indexed chunk content with a regular expression — no embedding, no
/// ranking. Returns chunks whose content matches `pattern`, ordered by path and
/// start line so output reads top-to-bottom like a file. This is the exact /
/// literal fallback for when semantic recall is not what the user wants.
pub fn search_regex(
    conn: &Connection,
    pattern: &str,
    limit: usize,
    file_filter: Option<&str>,
    case_insensitive: bool,
) -> Result<Vec<SearchResult>> {
    let full_pattern = if case_insensitive {
        format!("(?i){pattern}")
    } else {
        pattern.to_string()
    };
    let re = regex::Regex::new(&full_pattern).context("compiling search regex")?;

    let mut stmt = conn.prepare(
        "SELECT id, path, start_line, end_line, symbol, kind, content, token_count
         FROM chunks ORDER BY path, start_line",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(SearchResult {
            id: row.get::<_, i64>(0)?,
            path: row.get::<_, String>(1)?,
            start_line: row.get::<_, i64>(2)? as usize,
            end_line: row.get::<_, i64>(3)? as usize,
            symbol: row.get::<_, String>(4)?,
            kind: row.get::<_, String>(5)?,
            content: row.get::<_, String>(6)?,
            token_count: row.get::<_, i64>(7)? as usize,
            distance: 0.0,
        })
    })?;

    let mut results = Vec::new();
    for r in rows {
        let chunk = r?;
        if file_filter.is_some_and(|f| !chunk.path.contains(f)) {
            continue;
        }
        if re.is_match(&chunk.content) {
            results.push(chunk);
            if results.len() >= limit {
                break;
            }
        }
    }
    Ok(results)
}

pub fn fetch_chunks_by_ids(conn: &Connection, ids: &[i64]) -> Result<Vec<SearchResult>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders: Vec<String> = (1..=ids.len()).map(|i| format!("?{}", i)).collect();
    let query_str = format!(
        "SELECT id, path, start_line, end_line, symbol, kind, content, token_count
         FROM chunks WHERE id IN ({})",
        placeholders.join(",")
    );
    let mut stmt = conn.prepare(&query_str)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(ids), |row| {
        Ok(SearchResult {
            id: row.get::<_, i64>(0)?,
            path: row.get::<_, String>(1)?,
            start_line: row.get::<_, i64>(2)? as usize,
            end_line: row.get::<_, i64>(3)? as usize,
            symbol: row.get::<_, String>(4)?,
            kind: row.get::<_, String>(5)?,
            content: row.get::<_, String>(6)?,
            token_count: row.get::<_, i64>(7)? as usize,
            distance: 1.0,
        })
    })?;
    let mut results = Vec::new();
    for r in rows {
        results.push(r?);
    }
    Ok(results)
}

pub fn hybrid_search(
    conn: &Connection,
    query_vec: &[f32],
    query_text: &str,
    k: usize,
    file_filter: Option<&str>,
) -> Result<Vec<SearchResult>> {
    let dense_limit = 100.max(k * 2);
    let dense_results = search_similar(conn, query_vec, dense_limit, file_filter)?;

    let sparse_limit = 100.max(k * 2);
    let sparse_ids = search_fts(conn, query_text, sparse_limit, file_filter)?;

    let mut rrf_scores: HashMap<i64, f32> = HashMap::new();

    for (rank, res) in dense_results.iter().enumerate() {
        let score = 1.0 / (60.0 + rank as f32);
        rrf_scores.insert(res.id, score);
    }

    for (rank, id) in sparse_ids.iter().enumerate() {
        let score = 1.0 / (60.0 + rank as f32);
        rrf_scores
            .entry(*id)
            .and_modify(|s| *s += score)
            .or_insert(score);
    }

    let mut sorted_candidates: Vec<(i64, f32)> = rrf_scores.into_iter().collect();
    sorted_candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let top_candidates: Vec<(i64, f32)> = sorted_candidates.into_iter().take(k * 2).collect();
    if top_candidates.is_empty() {
        return Ok(Vec::new());
    }

    let mut dense_map: HashMap<i64, SearchResult> =
        dense_results.into_iter().map(|r| (r.id, r)).collect();

    let missing_ids: Vec<i64> = top_candidates
        .iter()
        .map(|(id, _)| *id)
        .filter(|id| !dense_map.contains_key(id))
        .collect();

    if !missing_ids.is_empty() {
        let chunks_fetched = fetch_chunks_by_ids(conn, &missing_ids)?;
        for chunk in chunks_fetched {
            dense_map.insert(chunk.id, chunk);
        }
    }

    let mut final_results = Vec::new();
    for (id, rrf_score) in top_candidates {
        if let Some(mut result) = dense_map.remove(&id) {
            result.distance = 1.0 - rrf_score;
            final_results.push(result);
        }
    }

    final_results.sort_by(|a, b| {
        a.distance
            .partial_cmp(&b.distance)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(final_results.into_iter().take(k).collect())
}

#[allow(dead_code)]
pub struct SymbolMatch {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub kind: String,
    pub symbol: String,
}

/// Find chunks whose symbol name contains `pattern` (case-insensitive substring).
/// Returns up to 20 matches ordered by path + start_line.
pub fn search_by_symbol(conn: &Connection, pattern: &str) -> Result<Vec<SymbolMatch>> {
    let like = format!("%{}%", pattern.to_lowercase());
    let mut stmt = conn.prepare(
        "SELECT path, start_line, end_line, kind, symbol FROM chunks
         WHERE lower(symbol) LIKE ?1 AND symbol != ''
         ORDER BY path, start_line LIMIT 20",
    )?;
    let results = stmt
        .query_map(params![like], |row| {
            Ok(SymbolMatch {
                path: row.get(0)?,
                start_line: row.get::<_, i64>(1)? as usize,
                end_line: row.get::<_, i64>(2)? as usize,
                kind: row.get(3)?,
                symbol: row.get(4)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(results)
}

/// Load all known file records into a HashMap for fast skip-detection during parallel indexing.
pub fn load_all_file_info(conn: &Connection) -> Result<HashMap<String, (i64, f64, String)>> {
    let mut stmt = conn.prepare("SELECT id, path, mtime, content_hash FROM files")?;
    let mut map = HashMap::new();
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, f64>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    for row in rows.filter_map(|r| r.ok()) {
        let (id, path, mtime, hash) = row;
        map.insert(path, (id, mtime, hash));
    }
    Ok(map)
}

#[allow(dead_code)]
pub fn get_file_info(conn: &Connection, path: &str) -> Result<Option<(i64, f64, String)>> {
    let mut stmt = conn.prepare("SELECT id, mtime, content_hash FROM files WHERE path=?1")?;
    let res = stmt.query_row(params![path], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, f64>(1)?,
            r.get::<_, String>(2)?,
        ))
    });
    match res {
        Ok(v) => Ok(Some(v)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub struct IndexStats {
    pub files: i64,
    pub chunks: i64,
    pub total_tokens: i64,
}

pub struct IndexStaleness {
    pub stale: bool,
    pub reason: String,
}

pub fn count_stats(conn: &Connection) -> Result<IndexStats> {
    let files: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
    let chunks: i64 = conn.query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))?;
    let tokens: i64 =
        conn.query_row("SELECT COALESCE(SUM(token_count),0) FROM chunks", [], |r| {
            r.get(0)
        })?;
    Ok(IndexStats {
        files,
        chunks,
        total_tokens: tokens,
    })
}

pub fn get_index_age(repo_root: &Path) -> Option<f64> {
    let conn = open_db(repo_root, false).ok()??;
    let val: String = conn
        .query_row("SELECT value FROM meta WHERE key='indexed_at'", [], |r| {
            r.get(0)
        })
        .ok()?;
    let indexed_at: f64 = val.parse().ok()?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs_f64();
    Some(now - indexed_at)
}

pub fn index_staleness(repo_root: &Path) -> IndexStaleness {
    let conn = match open_db(repo_root, false) {
        Ok(Some(c)) => c,
        _ => {
            return IndexStaleness {
                stale: true,
                reason: "missing".to_string(),
            }
        }
    };

    if meta_value(&conn, "indexed_at").is_none() {
        return IndexStaleness {
            stale: true,
            reason: "missing indexed_at".to_string(),
        };
    }

    if let Some(current) = git_fingerprint(repo_root) {
        match meta_value(&conn, "git_fingerprint") {
            Some(stored) if stored == current => {}
            Some(stored) => {
                if let (Some(stored_head), Some(current_head)) = (
                    stored.split(':').next_back(),
                    current.split(':').next_back(),
                ) {
                    if let (Some(diff), Some(status)) = (
                        git_output(
                            repo_root,
                            &["diff", "--name-only", stored_head, current_head],
                        ),
                        git_output(repo_root, &["status", "--porcelain"]),
                    ) {
                        if diff.trim().is_empty()
                            && status.trim().is_empty()
                            && set_meta(&conn, "git_fingerprint", &current).is_ok()
                        {
                            let now = SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs_f64();
                            let _ = set_meta(&conn, "indexed_at", &now.to_string());
                            return IndexStaleness {
                                stale: false,
                                reason: "git HEAD changed but code is identical (fingerprint auto-updated)".to_string(),
                            };
                        }
                    }
                }
                return IndexStaleness {
                    stale: true,
                    reason: "git HEAD changed".to_string(),
                };
            }
            None => {
                return IndexStaleness {
                    stale: true,
                    reason: "missing git fingerprint".to_string(),
                }
            }
        }
    }

    IndexStaleness {
        stale: false,
        reason: "fresh".to_string(),
    }
}

pub fn write_index_meta(conn: &Connection, repo_root: &Path, indexed_at: f64) -> Result<()> {
    set_meta(conn, "indexed_at", &indexed_at.to_string())?;
    if let Some(fp) = git_fingerprint(repo_root) {
        set_meta(conn, "git_fingerprint", &fp)?;
    }
    Ok(())
}

fn meta_value(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
        .ok()
}

fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO meta(key,value) VALUES(?1,?2)",
        params![key, value],
    )?;
    Ok(())
}

fn git_fingerprint(repo_root: &Path) -> Option<String> {
    let head = git_output(repo_root, &["rev-parse", "HEAD"])?;
    let branch = git_output(repo_root, &["branch", "--show-current"]).unwrap_or_default();
    let worktree = git_output(repo_root, &["rev-parse", "--show-toplevel"])
        .unwrap_or_else(|| repo_root.to_string_lossy().to_string());
    Some(format!(
        "{}:{}:{}",
        worktree.trim(),
        branch.trim(),
        head.trim()
    ))
}

fn git_output(repo_root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HookEvent {
    pub ts: f64,
    pub tool: String,
    pub action: String,
    pub reason: String,
    pub saved_tokens: i64,
    pub actual_tokens: i64,
    pub original_estimate: i64,
    pub input_preview: String,
    #[serde(default = "default_phase")]
    pub phase: String,
    /// Parsed base command for Bash events; empty for non-Bash. Stored directly
    /// so `filter list` need not re-parse the (truncated) input_preview.
    #[serde(default)]
    pub command: String,
}

fn default_phase() -> String {
    "pre".to_string()
}

pub fn log_hook_event(repo_root: &Path, event: &HookEvent) -> Result<()> {
    let log = log_path(repo_root);
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent)?;
    }
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)?;
    writeln!(f, "{}", serde_json::to_string(event)?)?;
    Ok(())
}

pub fn read_hook_log(repo_root: &Path) -> Vec<HookEvent> {
    let log = log_path(repo_root);
    if !log.exists() {
        return vec![];
    }
    std::fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

// ---- Daemon helpers ---------------------------------------------------------

pub struct EmbeddingEntry {
    pub id: i64,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub symbol: String,
    pub kind: String,
    pub token_count: usize,
    pub embedding: Vec<f32>,
}

/// Load embeddings + metadata (no chunk content) for the daemon cache.
/// Content is fetched on-demand via fetch_chunks_content() for top-K results only.
pub fn load_all_embeddings(conn: &Connection) -> Result<Vec<EmbeddingEntry>> {
    let mut stmt = conn.prepare(
        "SELECT c.id, c.path, c.start_line, c.end_line, c.symbol, c.kind, \
                c.token_count, e.embedding \
         FROM embeddings e JOIN chunks c ON c.id = e.chunk_id",
    )?;
    let entries = stmt
        .query_map([], |row| {
            let blob: Vec<u8> = row.get(7)?;
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
                blob,
            ))
        })?
        .filter_map(|r| r.ok())
        .map(
            |(id, path, sl, el, symbol, kind, tc, blob)| EmbeddingEntry {
                id,
                path,
                start_line: sl as usize,
                end_line: el as usize,
                symbol,
                kind,
                token_count: tc as usize,
                embedding: deserialize_vec(&blob),
            },
        )
        .collect();
    Ok(entries)
}

/// Fetch chunk content for a set of IDs — used after cosine search to hydrate top-K results.
pub fn fetch_chunks_content(
    conn: &Connection,
    ids: &[i64],
) -> Result<std::collections::HashMap<i64, String>> {
    if ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!("SELECT id, content FROM chunks WHERE id IN ({placeholders})");
    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<rusqlite::types::Value> = ids
        .iter()
        .map(|id| rusqlite::types::Value::Integer(*id))
        .collect();
    let result = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(result)
}

/// Return the DB file's mtime as secs-since-epoch, or 0.0 on error.
pub fn get_db_mtime(repo_root: &Path) -> f64 {
    std::fs::metadata(db_path(repo_root))
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub fn get_file_token_counts(conn: &Connection) -> Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT files.path, COALESCE(SUM(chunks.token_count), 0)
         FROM files
         LEFT JOIN chunks ON files.id = chunks.file_id
         GROUP BY files.id",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    let mut res = Vec::new();
    for row in rows {
        res.push(row?);
    }
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_similarity_to_bytes() {
        let q = vec![1.0, 2.0, 3.0, 4.0];
        let b = vec![0.5, -1.0, 2.0, 1.5];
        let q_norm = q.iter().map(|x| x * x).sum::<f32>().sqrt();

        let sim1 = cosine_similarity(&q, &b);
        let bytes = serialize_vec(&b);
        let sim2 = cosine_similarity_to_bytes(&q, q_norm, &bytes);

        assert!((sim1 - sim2).abs() < 1e-6);
    }

    #[test]
    fn test_cached_embeddings_batch_lookup() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn, 4).unwrap();

        let first = vec![1.0, 2.0, 3.0, 4.0];
        let second = vec![0.25, 0.5, 0.75, 1.0];
        upsert_embedding_cache(&conn, "hash-a", &first).unwrap();
        upsert_embedding_cache(&conn, "hash-b", &second).unwrap();

        let hashes = vec![
            "hash-a".to_string(),
            "missing".to_string(),
            "hash-a".to_string(),
            "hash-b".to_string(),
        ];
        let cached = cached_embeddings(&conn, &hashes).unwrap();

        assert_eq!(cached.len(), 2);
        assert_eq!(cached.get("hash-a").unwrap(), &first);
        assert_eq!(cached.get("hash-b").unwrap(), &second);
        assert!(!cached.contains_key("missing"));
    }

    #[test]
    fn test_get_file_token_counts() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn, 4).unwrap();

        let file_id1 = upsert_file(&conn, "src/main.rs", 123.45, "hash1").unwrap();
        let file_id2 = upsert_file(&conn, "src/lib.rs", 123.45, "hash2").unwrap();

        insert_chunk(
            &conn,
            NewChunk {
                file_id: file_id1,
                path: "src/main.rs",
                start: 1,
                end: 10,
                symbol: "main",
                kind: "function",
                content: "fn main() {}",
                token_count: 5,
            },
        )
        .unwrap();

        insert_chunk(
            &conn,
            NewChunk {
                file_id: file_id1,
                path: "src/main.rs",
                start: 11,
                end: 20,
                symbol: "helper",
                kind: "function",
                content: "fn helper() {}",
                token_count: 10,
            },
        )
        .unwrap();

        insert_chunk(
            &conn,
            NewChunk {
                file_id: file_id2,
                path: "src/lib.rs",
                start: 1,
                end: 5,
                symbol: "lib_func",
                kind: "function",
                content: "fn lib_func() {}",
                token_count: 7,
            },
        )
        .unwrap();

        let counts = get_file_token_counts(&conn).unwrap();
        assert_eq!(counts.len(), 2);

        let main_count = counts
            .iter()
            .find(|(path, _)| path == "src/main.rs")
            .unwrap()
            .1;
        let lib_count = counts
            .iter()
            .find(|(path, _)| path == "src/lib.rs")
            .unwrap()
            .1;

        assert_eq!(main_count, 15);
        assert_eq!(lib_count, 7);
    }

    #[test]
    fn test_hybrid_search() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn, 4).unwrap();

        // 1. Insert file
        let file_id = upsert_file(&conn, "src/main.rs", 123.45, "abcde").unwrap();

        // 2. Insert chunk
        let chunk = NewChunk {
            file_id,
            path: "src/main.rs",
            start: 1,
            end: 10,
            symbol: "my_cool_function",
            kind: "function",
            content: "fn my_cool_function() { println!(\"hello fts5 hybrid search\"); }",
            token_count: 15,
        };
        let chunk_id = insert_chunk(&conn, chunk).unwrap();

        // 3. Insert embedding
        let embedding = vec![0.5, 0.5, 0.5, 0.5];
        conn.execute(
            "INSERT INTO embeddings(chunk_id, embedding) VALUES(?1, ?2)",
            params![chunk_id, serialize_vec(&embedding)],
        )
        .unwrap();

        // 4. Test search_fts
        let sparse_ids = search_fts(&conn, "fts5 hybrid", 10, None).unwrap();
        assert_eq!(sparse_ids, vec![chunk_id]);

        // 5. Test hybrid_search
        let query_vec = vec![0.6, 0.6, 0.6, 0.6];
        let results = hybrid_search(&conn, &query_vec, "hello search", 10, None).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, chunk_id);
        assert_eq!(results[0].symbol, "my_cool_function");
    }

    #[test]
    fn search_regex_matches_literal_and_respects_case_and_filter() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn, 4).unwrap();
        let file_id = upsert_file(&conn, "src/main.rs", 1.0, "h1").unwrap();
        let other_id = upsert_file(&conn, "src/lib.rs", 1.0, "h2").unwrap();
        let a = insert_chunk(
            &conn,
            NewChunk {
                file_id,
                path: "src/main.rs",
                start: 1,
                end: 2,
                symbol: "alpha",
                kind: "function",
                content: "fn alpha() { let TOKEN = 1; }",
                token_count: 9,
            },
        )
        .unwrap();
        insert_chunk(
            &conn,
            NewChunk {
                file_id: other_id,
                path: "src/lib.rs",
                start: 1,
                end: 2,
                symbol: "beta",
                kind: "function",
                content: "fn beta() {}",
                token_count: 4,
            },
        )
        .unwrap();

        // Regex metacharacters honored.
        let hits = search_regex(&conn, r"alpha\(\)", 10, None, false).unwrap();
        assert_eq!(hits.iter().map(|r| r.id).collect::<Vec<_>>(), vec![a]);

        // Case-sensitive miss, case-insensitive hit.
        assert!(search_regex(&conn, "token", 10, None, false).unwrap().is_empty());
        assert_eq!(search_regex(&conn, "token", 10, None, true).unwrap().len(), 1);

        // File filter scopes results.
        assert!(search_regex(&conn, "fn ", 10, Some("lib.rs"), false)
            .unwrap()
            .iter()
            .all(|r| r.path == "src/lib.rs"));
    }

    /// Retrieval quality gate. Builds a tiny in-memory index from labeled
    /// (path, content) docs using the REAL embedding model, then runs labeled
    /// queries through `hybrid_search` and asserts Hit@1 / Hit@3 thresholds.
    /// No repo walk, no daemon — just a handful of short embeds, so it is safe
    /// to run. Model-gated so the default offline `cargo test` stays fast.
    #[test]
    #[cfg_attr(
        not(feature = "model-tests"),
        ignore = "needs model download; run with --features model-tests"
    )]
    fn retrieval_eval_meets_hit_rate_thresholds() {
        // Mirror the indexer's embedded-text format: "<path>\n<content>".
        let docs: &[(&str, &str)] = &[
            (
                "src/auth.rs",
                "fn validate_jwt_token(token: &str) -> bool { verify the signature and expiry of a json web token }",
            ),
            (
                "src/db.rs",
                "struct ConnectionPool { establishes postgres database connections and reuses them via pooling }",
            ),
            (
                "src/cache.rs",
                "fn evict_lru_entry() { remove the least recently used item from the in-memory cache }",
            ),
            (
                "src/http.rs",
                "async fn handle_request(req: Request) { route an incoming http request to the right handler }",
            ),
            (
                "src/math.rs",
                "fn dot_product(a: &[f32], b: &[f32]) -> f32 { sum of the elementwise multiplication of two vectors }",
            ),
        ];
        let queries: &[(&str, &str)] = &[
            ("how are json web tokens validated", "src/auth.rs"),
            ("database connection pooling", "src/db.rs"),
            ("least recently used cache eviction", "src/cache.rs"),
            ("routing incoming http requests", "src/http.rs"),
            ("vector dot product", "src/math.rs"),
        ];

        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn, 768).unwrap();

        let texts: Vec<String> = docs
            .iter()
            .map(|(path, content)| format!("{path}\n{content}"))
            .collect();
        let embeddings = crate::embed::embed_documents(&texts).expect("embed docs");

        for (i, (path, content)) in docs.iter().enumerate() {
            let file_id = upsert_file(&conn, path, i as f64, "hash").unwrap();
            let chunk_id = insert_chunk(
                &conn,
                NewChunk {
                    file_id,
                    path,
                    start: 1,
                    end: 1,
                    symbol: "",
                    kind: "function",
                    content,
                    token_count: 10,
                },
            )
            .unwrap();
            insert_embedding(&conn, chunk_id, &embeddings[i]).unwrap();
        }

        let mut hit1 = 0usize;
        let mut hit3 = 0usize;
        for (query, expected) in queries {
            let qvec = crate::embed::embed_query(query).expect("embed query");
            let results = hybrid_search(&conn, &qvec, query, 3, None).unwrap();
            if results.first().is_some_and(|r| &r.path == expected) {
                hit1 += 1;
            }
            if results.iter().any(|r| &r.path == expected) {
                hit3 += 1;
            }
        }

        let n = queries.len();
        let hit1_rate = hit1 as f32 / n as f32;
        let hit3_rate = hit3 as f32 / n as f32;
        assert!(
            hit1_rate >= 0.8,
            "Hit@1 {hit1_rate:.2} below 0.80 threshold ({hit1}/{n})"
        );
        assert!(
            hit3_rate >= 1.0,
            "Hit@3 {hit3_rate:.2} below 1.00 threshold ({hit3}/{n})"
        );
    }

    #[test]
    fn test_index_staleness_fingerprint_auto_update() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("tokenix-test-{}", now));
        std::fs::create_dir_all(&temp_dir).unwrap();

        // Initialize a dummy git repo
        let run_git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&temp_dir)
                .output()
                .unwrap();
        };
        run_git(&["init"]);
        run_git(&["config", "user.name", "Test"]);
        run_git(&["config", "user.email", "test@example.com"]);

        let test_file = temp_dir.join("test.txt");
        std::fs::write(&test_file, "hello").unwrap();
        run_git(&["add", "test.txt"]);
        run_git(&["commit", "-m", "initial commit"]);

        // Open the tokenix db for this repo
        let conn = open_db(&temp_dir, true).unwrap().unwrap();
        init_schema(&conn, 4).unwrap();

        // Write the initial metadata
        let initial_fp = git_fingerprint(&temp_dir).unwrap();
        set_meta(&conn, "git_fingerprint", &initial_fp).unwrap();
        set_meta(&conn, "indexed_at", "12345.6").unwrap();
        drop(conn);

        // Make another commit with NO file changes (e.g. empty commit)
        run_git(&["commit", "--allow-empty", "-m", "second commit"]);

        // Now run index_staleness. It should detect that the branch/commit changed but files are identical,
        // and automatically update the stored fingerprint and mark it as fresh (stale: false)!
        let staleness = index_staleness(&temp_dir);
        assert!(
            !staleness.stale,
            "Should not be stale since code is identical: {:?}",
            staleness.reason
        );

        // Verify the database has the updated fingerprint
        let conn2 = open_db(&temp_dir, false).unwrap().unwrap();
        let current_fp = git_fingerprint(&temp_dir).unwrap();
        let stored_fp = meta_value(&conn2, "git_fingerprint").unwrap();
        assert_eq!(stored_fp, current_fp);

        // Clean up
        let _ = std::fs::remove_dir_all(temp_dir);
    }
}
