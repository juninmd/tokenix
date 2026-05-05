use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const DB_FILE: &str = ".tokenix/index.db";
const LOG_FILE: &str = ".tokenix/hook.log";

pub fn db_path(repo_root: &Path) -> PathBuf {
    repo_root.join(DB_FILE)
}

pub fn log_path(repo_root: &Path) -> PathBuf {
    repo_root.join(LOG_FILE)
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

pub fn insert_chunk(
    conn: &Connection,
    file_id: i64,
    path: &str,
    start: usize,
    end: usize,
    symbol: &str,
    kind: &str,
    content: &str,
    token_count: usize,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO chunks(file_id,path,start_line,end_line,symbol,kind,content,token_count)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            file_id,
            path,
            start as i64,
            end as i64,
            symbol,
            kind,
            content,
            token_count as i64
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

pub fn search_similar(conn: &Connection, query_vec: &[f32], k: usize) -> Result<Vec<SearchResult>> {
    let mut stmt = conn.prepare(
        "SELECT c.id, c.path, c.start_line, c.end_line, c.symbol, c.kind, c.content, c.token_count, e.embedding
         FROM embeddings e JOIN chunks c ON c.id = e.chunk_id"
    )?;

    let mut scored: Vec<(f32, SearchResult)> = stmt
        .query_map([], |row| {
            let blob: Vec<u8> = row.get(8)?;
            Ok((
                blob,
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, i64>(7)?,
            ))
        })?
        .filter_map(|r| r.ok())
        .map(|(blob, id, path, sl, el, symbol, kind, content, tc)| {
            let emb = deserialize_vec(&blob);
            let sim = cosine_similarity(query_vec, &emb);
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
