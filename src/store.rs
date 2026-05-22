use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::collections::HashMap;
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
        CREATE TABLE IF NOT EXISTS meta (
            key TEXT PRIMARY KEY,
            value TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_chunks_path ON chunks(path);
        CREATE INDEX IF NOT EXISTS idx_chunks_file ON chunks(file_id);

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
    let mut i = 0;
    for chunk in bytes.chunks_exact(4) {
        if i >= query_vec.len() {
            break;
        }
        let y = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        dot += query_vec[i] * y;
        nb += y * y;
        i += 1;
    }
    let nb_sqrt = nb.sqrt();
    if query_norm == 0.0 || nb_sqrt == 0.0 {
        0.0
    } else {
        dot / (query_norm * nb_sqrt)
    }
}

pub fn search_similar(
    conn: &Connection,
    query_vec: &[f32],
    k: usize,
    file_filter: Option<&str>,
) -> Result<Vec<SearchResult>> {
    let rows_data: Vec<(Vec<u8>, i64, String, i64, i64, String, String, String, i64)> = if let Some(filter) = file_filter {
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
             WHERE chunks_fts MATCH ?1 AND instr(c.path, ?2) > 0 LIMIT ?3"
        )?;
        let rows = stmt.query_map(params![sanitized, filter, limit], |row| {
            row.get::<_, i64>(0)
        })?;
        for r in rows {
            if let Ok(id) = r {
                ids.push(id);
            }
        }
    } else {
        let mut stmt = conn.prepare(
            "SELECT rowid FROM chunks_fts WHERE chunks_fts MATCH ?1 LIMIT ?2"
        )?;
        let rows = stmt.query_map(params![sanitized, limit], |row| {
            row.get::<_, i64>(0)
        })?;
        for r in rows {
            if let Ok(id) = r {
                ids.push(id);
            }
        }
    }
    Ok(ids)
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
        rrf_scores.entry(*id)
            .and_modify(|s| *s += score)
            .or_insert(score);
    }

    let mut sorted_candidates: Vec<(i64, f32)> = rrf_scores.into_iter().collect();
    sorted_candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let top_candidates: Vec<(i64, f32)> = sorted_candidates.into_iter().take(k * 2).collect();
    if top_candidates.is_empty() {
        return Ok(Vec::new());
    }

    let mut dense_map: HashMap<i64, SearchResult> = dense_results.into_iter().map(|r| (r.id, r)).collect();

    let missing_ids: Vec<i64> = top_candidates.iter()
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

    final_results.sort_by(|a, b| a.distance.partial_cmp(&b.distance).unwrap_or(std::cmp::Ordering::Equal));

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

pub fn index_staleness(repo_root: &Path, max_age_secs: f64) -> IndexStaleness {
    let conn = match open_db(repo_root, false) {
        Ok(Some(c)) => c,
        _ => {
            return IndexStaleness {
                stale: true,
                reason: "missing".to_string(),
            }
        }
    };

    let indexed_at = meta_value(&conn, "indexed_at").and_then(|v| v.parse::<f64>().ok());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs_f64());
    let age_secs = indexed_at.and_then(|ts| now.map(|n| n - ts));

    if indexed_at.is_none() {
        return IndexStaleness {
            stale: true,
            reason: "missing indexed_at".to_string(),
        };
    }

    if let Some(current) = git_fingerprint(repo_root) {
        match meta_value(&conn, "git_fingerprint") {
            Some(stored) if stored == current => {}
            Some(_) => {
                return IndexStaleness {
                    stale: true,
                    reason: "git HEAD changed".to_string(),
                }
            }
            None => {
                return IndexStaleness {
                    stale: true,
                    reason: "missing git fingerprint".to_string(),
                }
            }
        }
    }

    if let Some(age) = age_secs {
        if age > max_age_secs {
            return IndexStaleness {
                stale: true,
                reason: format!("stale ({}s old)", age as i64),
            };
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
        ).unwrap();

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
}
