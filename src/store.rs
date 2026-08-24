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

/// Acquire a PID-based index lock for the project.
pub fn acquire_index_lock(repo_root: &Path) -> Result<IndexLockGuard> {
    let global = global_dir().ok_or_else(|| anyhow::anyhow!("cannot determine home dir"))?;
    std::fs::create_dir_all(&global)?;
    let lock_path = global.join(format!("{}.lock", project_id(repo_root)));

    let pid = std::process::id();
    // Atomic acquire: a read-then-write check let two indexers both see no lock
    // and both open the same SQLite DB for writing.
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(mut f) => {
                use std::io::Write;
                f.write_all(pid.to_string().as_bytes())?;
                return Ok(IndexLockGuard {
                    path: lock_path,
                    pid,
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let holder = std::fs::read_to_string(&lock_path)
                    .ok()
                    .and_then(|c| c.trim().parse::<u32>().ok());
                match holder {
                    Some(p) if is_pid_alive(p) => {
                        anyhow::bail!(
                            "index already running (PID {p}). Use `tokenix stop` or wait."
                        );
                    }
                    // Stale lock (holder died, or the file is unreadable/garbage):
                    // clear it and retry the atomic create.
                    _ => {
                        std::fs::remove_file(&lock_path)?;
                    }
                }
            }
            Err(e) => return Err(e.into()),
        }
    }
}

pub struct IndexLockGuard {
    path: PathBuf,
    pid: u32,
}

impl Drop for IndexLockGuard {
    fn drop(&mut self) {
        // Only drop a lock this process still owns — otherwise the first
        // finisher would delete a lock a second indexer had just taken.
        let owned = std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|c| c.trim().parse::<u32>().ok())
            == Some(self.pid);
        if owned {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(target_os = "windows")]
fn is_pid_alive(pid: u32) -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.contains(&format!("{pid}")))
        .unwrap_or(false)
}

#[cfg(not(target_os = "windows"))]
fn is_pid_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
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
    let base = global_dir()
        .map(|d| d.join(format!("{}.db", project_id(repo_root))))
        .unwrap_or_else(|| repo_root.join(".tokenix/index.db"));

    if std::env::var("TOKENIX_BRANCH_AWARE").is_ok_and(|v| v == "true" || v == "1") {
        if let Some(branch) = current_branch(repo_root) {
            let safe = branch.replace(['/', '\\'], "_");
            let branch_path = base.with_extension(format!("{}.db", safe));
            return branch_path;
        }
    }
    base
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

/// One project entry discovered from `~/.tokenix/`.
pub struct ProjectEntry {
    /// Human-readable root path (from the `.name` file, or the id when absent).
    pub label: String,
    /// Filesystem path to the project's hook log (`{id}.log`).
    pub log_path: PathBuf,
}

/// Enumerate every project that has a hook log in `~/.tokenix/`.
/// Returns entries sorted by label for stable output ordering.
pub fn list_all_project_logs() -> Vec<ProjectEntry> {
    let Some(dir) = global_dir() else {
        return vec![];
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return vec![];
    };
    let mut projects: Vec<ProjectEntry> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let stem = path.file_stem()?.to_str()?.to_string();
            // Keep only primary logs (not rotated `.log.1`).
            if path.extension()?.to_str()? != "log" {
                return None;
            }
            let name_path = dir.join(format!("{}.name", stem));
            let label = std::fs::read_to_string(&name_path)
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| stem.clone());
            Some(ProjectEntry {
                label,
                log_path: path,
            })
        })
        .collect();
    projects.sort_by(|a, b| a.label.cmp(&b.label));
    projects
}

/// Read hook events directly from a log path (without requiring a repo root).
pub fn read_hook_log_from_path(log_path: &Path) -> Vec<HookEvent> {
    let mut events = Vec::new();
    let rotated = rotated_log_path(log_path);
    for path in [rotated, log_path.to_path_buf()] {
        if !path.exists() {
            continue;
        }
        events.extend(
            std::fs::read_to_string(&path)
                .unwrap_or_default()
                .lines()
                .filter_map(|l| serde_json::from_str::<HookEvent>(l).ok()),
        );
    }
    events
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
    // foreign_keys is OFF per-connection by default, so the schema's
    // `ON DELETE CASCADE` clauses never fired and deleting a file left orphaned
    // chunks/embeddings/graph rows behind.
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=5000; \
         PRAGMA foreign_keys=ON;",
    )?;
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
        CREATE TABLE IF NOT EXISTS graph_imports (
            id INTEGER PRIMARY KEY,
            source_path TEXT NOT NULL,
            target TEXT NOT NULL,
            resolved_path TEXT,
            kind TEXT NOT NULL,
            line INTEGER
        );
        CREATE INDEX IF NOT EXISTS idx_imports_source ON graph_imports(source_path);
        CREATE INDEX IF NOT EXISTS idx_imports_resolved ON graph_imports(resolved_path);
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
    // Migration for int8-quantized embeddings: rows with a non-NULL scale hold
    // i8 vectors (1 byte/dim); NULL-scale rows are legacy f32 blobs (4 bytes/dim).
    let _ = conn.execute("ALTER TABLE embeddings ADD COLUMN scale REAL", []);
    Ok(())
}

pub fn serialize_vec(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

pub fn deserialize_vec(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect()
}

/// Symmetric int8 quantization: `scale = max|x| / 127`, each element stored as
/// one signed byte. 4x smaller than f32 with near-lossless cosine similarity
/// (the per-vector scale cancels out of the cosine entirely).
pub fn quantize_q8(v: &[f32]) -> (Vec<u8>, f32) {
    let max_abs = v.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    let scale = if max_abs == 0.0 { 1.0 } else { max_abs / 127.0 };
    let data = v
        .iter()
        .map(|x| (x / scale).round().clamp(-127.0, 127.0) as i8 as u8)
        .collect();
    (data, scale)
}

/// Cosine similarity between an f32 query and an i8-quantized document vector.
/// The document's quantization scale cancels in the cosine, so only the raw
/// i8 bytes are needed.
pub fn cosine_similarity_to_q8(query_vec: &[f32], query_norm: f32, bytes: &[u8]) -> f32 {
    let mut dot = 0.0f32;
    let mut nb = 0.0f32;
    for (i, &b) in bytes.iter().enumerate() {
        if i >= query_vec.len() {
            break;
        }
        let y = b as i8 as f32;
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
    let (blob, scale) = quantize_q8(embedding);
    conn.execute(
        "INSERT OR REPLACE INTO embeddings(chunk_id,embedding,scale) VALUES(?1,?2,?3)",
        params![chunk_id, blob, scale],
    )?;
    Ok(())
}

/// One-time, embedding-free migration: re-encode legacy f32 rows to int8.
/// Returns the number of converted rows. Cheap (pure CPU re-encode), so it
/// runs opportunistically at index time.
pub fn backfill_quantized_embeddings(conn: &Connection) -> Result<usize> {
    // Migrate in bounded batches. Materializing the whole legacy table pulled
    // every f32 blob into RAM at once (~3 KB per chunk — over a GB on a large
    // old index) right before Phase 1 starts allocating.
    const BATCH: usize = 5_000;
    let mut count = 0usize;
    loop {
        let legacy: Vec<(i64, Vec<u8>)> = {
            let mut stmt = conn.prepare(
                "SELECT chunk_id, embedding FROM embeddings WHERE scale IS NULL LIMIT ?1",
            )?;
            let rows = stmt.query_map(params![BATCH as i64], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
            })?;
            rows.filter_map(|r| r.ok()).collect()
        };
        if legacy.is_empty() {
            break;
        }
        count += legacy.len();
        migrate_legacy_batch(conn, legacy)?;
    }
    if count == 0 {
        return Ok(0);
    }
    // VACUUM rewrites the entire DB and needs ~2x the disk; make it opt-in
    // rather than a mandatory blocking step at the start of every index run.
    if std::env::var("TOKENIX_VACUUM_AFTER_MIGRATION").as_deref() == Ok("1") {
        if let Err(e) = conn.execute_batch("VACUUM") {
            eprintln!("tokenix: VACUUM after embedding migration failed: {e}");
        }
    }
    Ok(count)
}

fn migrate_legacy_batch(conn: &Connection, legacy: Vec<(i64, Vec<u8>)>) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    for (chunk_id, blob) in legacy {
        let (q8, scale) = quantize_q8(&deserialize_vec(&blob));
        // Roll back explicitly on error: a bare `?` here left the transaction
        // open on the connection, so every later write joined a doomed
        // transaction (or hit "cannot start a transaction within a transaction").
        if let Err(e) = conn.execute(
            "UPDATE embeddings SET embedding=?2, scale=?3 WHERE chunk_id=?1",
            params![chunk_id, q8, scale],
        ) {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(e.into());
        }
    }
    conn.execute_batch("COMMIT")?;
    Ok(())
}

/// Read-only probe: does this index have the `scale` column yet? Query paths
/// open old DBs without running migrations (init_schema runs only at index
/// time), so SELECTs must degrade to the legacy f32 layout when it is missing.
fn embeddings_have_scale(conn: &Connection) -> bool {
    conn.prepare("SELECT scale FROM embeddings LIMIT 0").is_ok()
}

/// (quantized_rows, total_rows) — used by `doctor` to report migration coverage.
pub fn quantization_coverage(conn: &Connection) -> Result<(i64, i64)> {
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM embeddings", [], |r| r.get(0))?;
    if !embeddings_have_scale(conn) {
        return Ok((0, total));
    }
    let quantized: i64 = conn.query_row(
        "SELECT COUNT(*) FROM embeddings WHERE scale IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    Ok((quantized, total))
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

/// Age out cache entries no recent index run touched. Nothing ever deleted from
/// this table — every chunk hash ever seen was kept forever, so a repo with
/// churn grew the DB by ~3 KB per historical chunk with no ceiling.
pub fn prune_embedding_cache(conn: &Connection, max_age_days: f64) -> Result<usize> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let cutoff = now - max_age_days * 86_400.0;
    let removed = conn.execute(
        "DELETE FROM embedding_cache WHERE updated_at IS NOT NULL AND updated_at < ?1",
        params![cutoff],
    )?;
    Ok(removed)
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct GraphNode {
    pub chunk_id: i64,
    pub path: String,
    pub name: String,
    pub kind: String,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
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

// ---- File-level import graph --------------------------------------------------

#[derive(Debug, Clone, serde::Serialize)]
pub struct ImportEdge {
    pub source_path: String,
    /// Module text as written in the source (`crate::store`, `./utils`, `os.path`).
    pub target: String,
    /// Repo-relative file the import resolves to; None = external dependency.
    pub resolved_path: Option<String>,
    pub kind: String,
    pub line: usize,
}

pub fn clear_import_graph(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM graph_imports", [])?;
    Ok(())
}

pub fn insert_import(conn: &Connection, edge: &ImportEdge) -> Result<()> {
    conn.execute(
        "INSERT INTO graph_imports(source_path,target,resolved_path,kind,line) VALUES(?1,?2,?3,?4,?5)",
        params![
            edge.source_path,
            edge.target,
            edge.resolved_path,
            edge.kind,
            edge.line as i64
        ],
    )?;
    Ok(())
}

/// Outgoing imports of `path` (reverse=false) or files importing `path`
/// (reverse=true). Matches by path substring so `deps indexer.rs` works.
pub fn file_imports(conn: &Connection, path: &str, reverse: bool) -> Result<Vec<ImportEdge>> {
    let sql = if reverse {
        "SELECT source_path, target, resolved_path, kind, line FROM graph_imports
         WHERE resolved_path IS NOT NULL AND instr(resolved_path, ?1) > 0
         ORDER BY source_path, line"
    } else {
        "SELECT source_path, target, resolved_path, kind, line FROM graph_imports
         WHERE instr(source_path, ?1) > 0
         ORDER BY source_path, line"
    };
    let mut stmt = conn.prepare(sql).map_err(|_| {
        anyhow::anyhow!("Import graph not built yet. Run: tokenix index (or rebuild-graph)")
    })?;
    let rows = stmt.query_map(params![path], |row| {
        Ok(ImportEdge {
            source_path: row.get(0)?,
            target: row.get(1)?,
            resolved_path: row.get(2)?,
            kind: row.get(3)?,
            line: row.get::<_, i64>(4)? as usize,
        })
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// All indexed file paths — the resolution universe for import targets.
pub fn all_file_paths(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT path FROM files")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    Ok(rows.filter_map(|r| r.ok()).collect())
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
    search_graph_nodes_kind(conn, query, limit, None)
}

pub fn search_graph_nodes_kind(
    conn: &Connection,
    query: &str,
    limit: usize,
    kind: Option<&str>,
) -> Result<Vec<GraphNode>> {
    let pattern = format!("%{}%", query);
    let query_limit = (limit.max(1) * 4) as i64;
    let mut stmt = conn.prepare(
        "SELECT chunk_id,path,name,kind,start_line,end_line
         FROM graph_nodes
         WHERE (name = ?1 COLLATE NOCASE OR name LIKE ?2 COLLATE NOCASE OR path LIKE ?2 COLLATE NOCASE)
           AND (?4 IS NULL OR kind = ?4 COLLATE NOCASE)
         ORDER BY CASE WHEN name = ?1 COLLATE NOCASE THEN 0 ELSE 1 END, rank DESC, path, start_line
         LIMIT ?3",
    )?;
    let rows = stmt.query_map(
        params![query, pattern, query_limit, kind],
        graph_node_from_row,
    )?;
    let mut seen = HashSet::new();
    let mut nodes = Vec::new();
    for row in rows.filter_map(|row| row.ok()) {
        let key = (
            row.path.clone(),
            row.name.clone(),
            row.kind.clone(),
            row.start_line,
            row.end_line,
        );
        if seen.insert(key) {
            nodes.push(row);
            if nodes.len() >= limit {
                break;
            }
        }
    }
    Ok(nodes)
}

/// Every symbol declared in one file, ordered by position. Used to map changed
/// line ranges back onto symbols (`tokenix blast`).
pub fn graph_nodes_for_path(conn: &Connection, path: &str) -> Result<Vec<GraphNode>> {
    let mut stmt = conn.prepare(
        "SELECT chunk_id,path,name,kind,start_line,end_line
         FROM graph_nodes WHERE path = ?1 ORDER BY start_line",
    )?;
    let rows = stmt.query_map(params![path], |row| {
        Ok(GraphNode {
            chunk_id: row.get(0)?,
            path: row.get(1)?,
            name: row.get(2)?,
            kind: row.get(3)?,
            start_line: row.get::<_, i64>(4)? as usize,
            end_line: row.get::<_, i64>(5)? as usize,
        })
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// (chunk_id, name, path) for every graph node — the incremental rebuild's
/// table-backed resolution map.
pub fn all_graph_node_names(conn: &Connection) -> Result<Vec<(i64, String, String)>> {
    let mut stmt = conn.prepare("SELECT chunk_id, name, path FROM graph_nodes")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Bare (caller, callee) pairs for whole-graph PageRank recomputation.
pub fn all_graph_edge_pairs(conn: &Connection) -> Result<Vec<(i64, i64)>> {
    let mut stmt = conn.prepare("SELECT caller_chunk_id, callee_chunk_id FROM graph_edges")?;
    let rows = stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?;
    Ok(rows.filter_map(|r| r.ok()).collect())
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
    let start_ids: Vec<i64> = exact_nodes_if_present(search_graph_nodes(conn, symbol, 20)?, symbol)
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
                let edge_key = graph_relation_key(&relation);
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

/// Load all graph edges as (caller_id, caller_name, callee_id, callee_name) tuples.
/// Used for circular dependency detection.
/// Graph edge enriched with each endpoint's symbol name and `path:line` location.
/// Tuple order: (caller_id, caller_name, caller_loc, callee_id, callee_name, callee_loc).
pub type GraphEdgeRow = (i64, String, String, i64, String, String);

pub fn load_all_graph_edges(conn: &Connection) -> Result<Vec<GraphEdgeRow>> {
    let mut stmt = conn.prepare(
        "SELECT e.caller_chunk_id, from_node.name, from_node.path, from_node.start_line,
                e.callee_chunk_id, to_node.name, to_node.path, to_node.start_line
         FROM graph_edges e
         JOIN graph_nodes from_node ON from_node.chunk_id = e.caller_chunk_id
         JOIN graph_nodes to_node ON to_node.chunk_id = e.callee_chunk_id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            format!("{}:{}", row.get::<_, String>(2)?, row.get::<_, i64>(3)?),
            row.get::<_, i64>(4)?,
            row.get::<_, String>(5)?,
            format!("{}:{}", row.get::<_, String>(6)?, row.get::<_, i64>(7)?),
        ))
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Forward-only call-flow tracing from entry points matching `symbol`.
/// Follows callee edges only (caller → callee), expanding outward by depth.
pub fn graph_flow(
    conn: &Connection,
    symbol: &str,
    depth: usize,
    limit: usize,
) -> Result<Vec<GraphRelation>> {
    let start_ids: Vec<i64> = exact_nodes_if_present(search_graph_nodes(conn, symbol, 20)?, symbol)
        .into_iter()
        .map(|node| node.chunk_id)
        .collect();
    if start_ids.is_empty() {
        return Ok(Vec::new());
    }

    let mut relations = Vec::new();
    let mut seen_nodes = std::collections::HashSet::new();
    let mut seen_edges = std::collections::HashSet::new();
    let mut frontier: Vec<i64> = start_ids;

    for _ in 0..depth.max(1) {
        let mut next = Vec::new();
        for node_id in &frontier {
            if !seen_nodes.insert(*node_id) {
                continue;
            }
            // Follow callee edges only (forward direction)
            for relation in relations_for_node(conn, *node_id, false)? {
                let edge_key = graph_relation_key(&relation);
                if seen_edges.insert(edge_key) {
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
    let nodes = exact_nodes_if_present(search_graph_nodes(conn, symbol, 20)?, symbol);
    let mut relations = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for node in nodes {
        for relation in relations_for_node(conn, node.chunk_id, callers)? {
            if seen.insert(graph_relation_key(&relation)) {
                relations.push(relation);
                if relations.len() >= limit {
                    return Ok(relations);
                }
            }
        }
    }
    Ok(relations)
}

fn exact_nodes_if_present(mut nodes: Vec<GraphNode>, symbol: &str) -> Vec<GraphNode> {
    if nodes
        .iter()
        .any(|node| node.name.eq_ignore_ascii_case(symbol))
    {
        nodes.retain(|node| node.name.eq_ignore_ascii_case(symbol));
    }
    nodes
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

fn graph_relation_key(
    relation: &GraphRelation,
) -> (String, usize, String, String, usize, String, String) {
    (
        relation.from.path.clone(),
        relation.from.start_line,
        relation.from.name.clone(),
        relation.to.path.clone(),
        relation.to.start_line,
        relation.to.name.clone(),
        relation.reference.clone(),
    )
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

#[derive(Debug, Clone, serde::Serialize)]
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
    for (i, chunk) in bytes.as_chunks::<4>().0.iter().enumerate() {
        if i >= query_vec.len() {
            break;
        }
        let y = f32::from_le_bytes(*chunk);
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
    type RowData = (
        Vec<u8>,
        Option<f64>,
        i64,
        String,
        i64,
        i64,
        String,
        String,
        String,
        i64,
    );
    // Pre-migration DBs have no `scale` column; select NULL so every row takes
    // the legacy f32 path until `tokenix index` migrates the file.
    let scale_expr = if embeddings_have_scale(conn) {
        "e.scale"
    } else {
        "NULL"
    };
    let rows_data: Vec<RowData> = if let Some(filter) = file_filter {
        let mut stmt = conn.prepare(&format!(
            "SELECT c.id, c.path, c.start_line, c.end_line, c.symbol, c.kind, c.content, c.token_count, e.embedding, {scale_expr}
             FROM embeddings e JOIN chunks c ON c.id = e.chunk_id
             WHERE instr(c.path, ?1) > 0"
        ))?;
        let rows = stmt.query_map(params![filter], |row| {
            Ok((
                row.get::<_, Vec<u8>>(8)?,
                row.get::<_, Option<f64>>(9)?,
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
        let mut stmt = conn.prepare(&format!(
            "SELECT c.id, c.path, c.start_line, c.end_line, c.symbol, c.kind, c.content, c.token_count, e.embedding, {scale_expr}
             FROM embeddings e JOIN chunks c ON c.id = e.chunk_id"
        ))?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, Vec<u8>>(8)?,
                row.get::<_, Option<f64>>(9)?,
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
        .map(
            |(blob, scale, id, path, sl, el, symbol, kind, content, tc)| {
                // scale set → int8-quantized row; NULL → legacy f32 blob.
                let sim = if scale.is_some() {
                    cosine_similarity_to_q8(query_vec, query_norm, &blob)
                } else {
                    cosine_similarity_to_bytes(query_vec, query_norm, &blob)
                };
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
            },
        )
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
) -> Result<Vec<(i64, f32)>> {
    let sanitized = sanitize_fts_query(query_text);
    if sanitized.is_empty() {
        return Ok(Vec::new());
    }

    let mut results = Vec::new();
    if let Some(filter) = file_filter {
        let mut stmt = conn.prepare(
            "SELECT c.id, rank FROM chunks c JOIN chunks_fts f ON c.id = f.rowid
             WHERE chunks_fts MATCH ?1 AND instr(c.path, ?2) > 0 ORDER BY rank LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            params![sanitized, filter, i64::try_from(limit).unwrap_or(i64::MAX)],
            |row| {
                let id: i64 = row.get(0)?;
                let rank: f32 = row.get(1)?;
                Ok((id, -rank)) // Negate: FTS5 rank is negative (lower=better)
            },
        )?;
        for row in rows {
            results.push(row?);
        }
    } else {
        let mut stmt = conn.prepare(
            "SELECT rowid, rank FROM chunks_fts WHERE chunks_fts MATCH ?1 ORDER BY rank LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            params![sanitized, i64::try_from(limit).unwrap_or(i64::MAX)],
            |row| {
                let id: i64 = row.get(0)?;
                let rank: f32 = row.get(1)?;
                Ok((id, -rank)) // Negate: FTS5 rank is negative (lower=better)
            },
        )?;
        for row in rows {
            results.push(row?);
        }
    }
    Ok(results)
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

/// Reciprocal-rank-fusion damping constant (the classic k=60) shared by every
/// hybrid ranking path (store, daemon, query graph/path recall).
pub const RRF_K: f32 = 60.0;
/// Weight of the normalized BM25 score blended into the sparse-side RRF term.
pub const BM25_WEIGHT: f32 = 0.3;

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
    let sparse_results = search_fts(conn, query_text, sparse_limit, file_filter)?;

    let mut rrf_scores: HashMap<i64, f32> = HashMap::new();

    for (rank, res) in dense_results.iter().enumerate() {
        let score = 1.0 / (RRF_K + rank as f32);
        rrf_scores.insert(res.id, score);
    }

    for (rank, (id, bm25_score)) in sparse_results.iter().enumerate() {
        // Combine position-based RRF with BM25 score
        let rrf_position = 1.0 / (RRF_K + rank as f32);
        let bm25_normalized = (*bm25_score).max(0.0) / (1.0 + bm25_score.max(0.0));
        let score = rrf_position + BM25_WEIGHT * bm25_normalized;
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

/// How many indexed files still need embeddings, for callers that only have the
/// repo root. `0` when there is no index — nothing to backfill.
///
/// Deliberately *not* part of [`index_staleness`]: those files are perfectly
/// usable for text search, the symbol graph and read interception, and reporting
/// them as stale would make the hook fail open and stop saving tokens. It only
/// means a full `tokenix index` still has work to do.
pub fn pending_embed_count(repo_root: &Path) -> i64 {
    match open_db(repo_root, false) {
        Ok(Some(conn)) => pending_embed_files(&conn),
        _ => 0,
    }
}

/// Files written by a `--no-embed` run or an inline freshness refresh: their
/// chunks are searchable as text but carry no vectors yet.
pub fn pending_embed_files(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM files WHERE content_hash LIKE ?1",
        params![format!("{}%", crate::indexer::NO_EMBED_HASH_PREFIX)],
        |r| r.get(0),
    )
    .unwrap_or(0)
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

    // A model change only makes the index stale when a model is *explicitly*
    // requested via TOKENIX_EMBED_MODEL. Otherwise queries use whatever model the
    // index was built with (read from meta), so a differing local default must not
    // invalidate a perfectly usable index (e.g. a hook with no env set).
    if let Ok(desired_raw) = std::env::var("TOKENIX_EMBED_MODEL") {
        let desired_raw = desired_raw.trim();
        if !desired_raw.is_empty() {
            let desired = crate::embed::spec_for(desired_raw).id;
            let stored = meta_value(&conn, "embed_model")
                .unwrap_or_else(|| crate::embed::DEFAULT_MODEL_ID.to_string());
            if stored != desired {
                return IndexStaleness {
                    stale: true,
                    reason: format!("embedding model changed ({stored} -> {desired})"),
                };
            }
        }
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
    set_meta(conn, "embed_model", &crate::embed::active_model_id())?;
    if let Some(fp) = git_fingerprint(repo_root) {
        set_meta(conn, "git_fingerprint", &fp)?;
    }
    Ok(())
}

/// The embedding model id the index was built with. Defaults to the historical
/// model for indexes created before model stamping. `None` if there is no index.
pub fn index_model_id(repo_root: &Path) -> Option<String> {
    let conn = open_db(repo_root, false).ok()??;
    Some(
        meta_value(&conn, "embed_model")
            .unwrap_or_else(|| crate::embed::DEFAULT_MODEL_ID.to_string()),
    )
}

pub fn meta_value(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
        .ok()
}

pub fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
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

fn current_branch(repo_root: &Path) -> Option<String> {
    git_output(repo_root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .filter(|b| !b.is_empty() && b != "HEAD")
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

/// Rotate the NDJSON hook log past this size; one rotated generation is kept
/// so `gain` still sees recent history while the log stays bounded.
const HOOK_LOG_MAX_BYTES: u64 = 5_000_000;

/// Restrict a path tokenix created to its owner (`0600` for files, `0700` for
/// directories). Everything under `~/.tokenix` is derived from command lines and
/// command output, so on a shared host the default umask made one user's shell
/// history readable by every other account. No-op on Windows, where the parent
/// profile directory already carries the equivalent ACL.
pub fn restrict_to_owner(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let Ok(meta) = std::fs::metadata(path) else {
            return;
        };
        let mode = if meta.is_dir() { 0o700 } else { 0o600 };
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

/// Copy of `event` with credential shapes masked out of the two free-text fields.
///
/// `command` and `input_preview` are verbatim shell input, and shell input is
/// where credentials actually live: `curl -H "Authorization: Bearer …"`,
/// `psql postgres://user:pw@host`, `export API_KEY=…`. The log is long-lived
/// (rotated, not deleted), machine-wide, and read back by `gain`, so the raw
/// values had no business being on disk. `scan-secrets` and `session-audit`
/// already redact before printing — the writer must do the same.
fn redacted_event(event: &HookEvent) -> HookEvent {
    use crate::conversation_audit::redact_credentials;
    HookEvent {
        ts: event.ts,
        tool: event.tool.clone(),
        action: event.action.clone(),
        reason: event.reason.clone(),
        saved_tokens: event.saved_tokens,
        actual_tokens: event.actual_tokens,
        original_estimate: event.original_estimate,
        input_preview: redact_credentials(&event.input_preview),
        phase: event.phase.clone(),
        command: redact_credentials(&event.command),
    }
}

pub fn log_hook_event(repo_root: &Path, event: &HookEvent) -> Result<()> {
    let event = &redacted_event(event);
    let log = log_path(repo_root);
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent)?;
        restrict_to_owner(parent);
    }
    // Fail-open like the rest of the hook path: a rotation error must never
    // block logging (worst case the log keeps growing until the next attempt).
    if std::fs::metadata(&log).is_ok_and(|m| m.len() > HOOK_LOG_MAX_BYTES) {
        // Serialize rotation across hook processes: every command spawns its
        // own tokenix, so two of them could both `remove_file` + `rename` and
        // lose a whole generation of events. Whoever wins the atomic lock
        // rotates; the others just append this round.
        let guard = log.with_extension("rotating");
        if let Ok(f) = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&guard)
        {
            drop(f);
            // Re-check under the lock: a racing process may have just rotated.
            if std::fs::metadata(&log).is_ok_and(|m| m.len() > HOOK_LOG_MAX_BYTES) {
                let rotated = rotated_log_path(&log);
                let _ = std::fs::remove_file(&rotated);
                let _ = std::fs::rename(&log, &rotated);
            }
            let _ = std::fs::remove_file(&guard);
        } else if std::fs::metadata(&guard)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|e| e.as_secs() > 30)
        {
            // Crashed mid-rotation: clear the stale guard for the next caller.
            let _ = std::fs::remove_file(&guard);
        }
    }
    use std::io::Write;
    let existed = log.exists();
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)?;
    writeln!(f, "{}", serde_json::to_string(event)?)?;
    if !existed {
        restrict_to_owner(&log);
    }
    Ok(())
}

fn rotated_log_path(log: &Path) -> PathBuf {
    let mut name = log.file_name().unwrap_or_default().to_os_string();
    name.push(".1");
    log.with_file_name(name)
}

pub fn read_hook_log(repo_root: &Path) -> Vec<HookEvent> {
    let log = log_path(repo_root);
    let mut events = Vec::new();
    // Rotated generation first so events stay in chronological order.
    for path in [rotated_log_path(&log), log] {
        if !path.exists() {
            continue;
        }
        events.extend(
            std::fs::read_to_string(&path)
                .unwrap_or_default()
                .lines()
                .filter_map(|l| serde_json::from_str::<HookEvent>(l).ok()),
        );
    }
    events
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
    /// Int8-quantized vector (1 byte/dim). Legacy f32 rows are quantized at
    /// load so the daemon cache holds 4x less RAM uniformly.
    pub embedding_q8: Vec<i8>,
}

/// Load embeddings + metadata (no chunk content) for the daemon cache.
/// Content is fetched on-demand via fetch_chunks_content() for top-K results only.
pub fn load_all_embeddings(conn: &Connection) -> Result<Vec<EmbeddingEntry>> {
    let scale_expr = if embeddings_have_scale(conn) {
        "e.scale"
    } else {
        "NULL"
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT c.id, c.path, c.start_line, c.end_line, c.symbol, c.kind, \
                c.token_count, e.embedding, {scale_expr} \
         FROM embeddings e JOIN chunks c ON c.id = e.chunk_id",
    ))?;
    let entries = stmt
        .query_map([], |row| {
            let blob: Vec<u8> = row.get(7)?;
            let scale: Option<f64> = row.get(8)?;
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
                blob,
                scale,
            ))
        })?
        .filter_map(|r| r.ok())
        .map(|(id, path, sl, el, symbol, kind, tc, blob, scale)| {
            let embedding_q8: Vec<i8> = if scale.is_some() {
                blob.into_iter().map(|b| b as i8).collect()
            } else {
                let (q8, _) = quantize_q8(&deserialize_vec(&blob));
                q8.into_iter().map(|b| b as i8).collect()
            };
            EmbeddingEntry {
                id,
                path,
                start_line: sl as usize,
                end_line: el as usize,
                symbol,
                kind,
                token_count: tc as usize,
                embedding_q8,
            }
        })
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

/// Per-file graph centrality: the maximum PageRank of any symbol the file
/// defines. Used to decide what survives a token budget — a file other code
/// depends on is worth more than a file that merely sorts earlier by name.
/// Files with no graph nodes are absent from the map (treated as rank 0).
pub fn get_file_graph_ranks(conn: &Connection) -> Result<HashMap<String, f32>> {
    let mut stmt = conn.prepare(
        "SELECT chunks.path, MAX(graph_nodes.rank)
         FROM graph_nodes
         JOIN chunks ON chunks.id = graph_nodes.chunk_id
         GROUP BY chunks.path",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f32>(1)?)))?;
    let mut res = HashMap::new();
    for row in rows {
        let (path, rank) = row?;
        res.insert(path.replace('\\', "/"), rank);
    }
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event_with(command: &str, preview: &str) -> HookEvent {
        HookEvent {
            ts: 1.0,
            tool: "Bash".to_string(),
            action: "intercepted".to_string(),
            reason: "r".to_string(),
            saved_tokens: 10,
            actual_tokens: 5,
            original_estimate: 15,
            input_preview: preview.to_string(),
            phase: "pre".to_string(),
            command: command.to_string(),
        }
    }

    #[test]
    fn logged_events_mask_credentials_in_command_and_preview() {
        // The hook log is machine-wide, rotated rather than deleted, and read
        // back by `gain`. Shell input is exactly where credentials live, so the
        // raw value must never be what lands on disk.
        let e = event_with(
            "curl -H \"Authorization: Bearer sk-live-abc123\" https://api.example.com",
            "git push https://user:ghp_secret@github.com/org/repo.git",
        );
        let red = redacted_event(&e);
        assert!(!red.command.contains("sk-live-abc123"), "{}", red.command);
        assert!(red.command.contains("[REDACTED]"));
        assert!(
            !red.input_preview.contains("ghp_secret"),
            "{}",
            red.input_preview
        );
        // Measurements and routing fields must survive untouched, or `gain`
        // silently changes meaning.
        assert_eq!(red.saved_tokens, 10);
        assert_eq!(red.actual_tokens, 5);
        assert_eq!(red.original_estimate, 15);
        assert_eq!(red.action, "intercepted");
        assert_eq!(red.tool, "Bash");
        assert_eq!(red.phase, "pre");
    }

    #[test]
    fn redaction_leaves_ordinary_commands_byte_identical() {
        // `filter list` groups by this string; rewriting a benign command would
        // fragment the buckets.
        let e = event_with("cargo test --locked", "cargo test --locked");
        let red = redacted_event(&e);
        assert_eq!(red.command, "cargo test --locked");
        assert_eq!(red.input_preview, "cargo test --locked");
    }

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
    fn test_q8_cosine_matches_f32() {
        // Pseudo-embedding with mixed signs/magnitudes; q8 cosine must track f32.
        let doc: Vec<f32> = (0..768)
            .map(|i| ((i as f32 * 0.37).sin() * 0.04) - 0.01)
            .collect();
        let query: Vec<f32> = (0..768)
            .map(|i| ((i as f32 * 0.29).cos() * 0.05) + 0.005)
            .collect();
        let q_norm = query.iter().map(|x| x * x).sum::<f32>().sqrt();

        let exact = cosine_similarity(&query, &doc);
        let (q8, scale) = quantize_q8(&doc);
        assert!(scale > 0.0);
        assert_eq!(q8.len(), doc.len());
        let approx = cosine_similarity_to_q8(&query, q_norm, &q8);

        assert!(
            (exact - approx).abs() < 0.01,
            "q8 cosine drifted: exact={exact} approx={approx}"
        );
    }

    #[test]
    fn test_backfill_quantizes_legacy_rows() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn, 4).unwrap();

        let file_id = upsert_file(&conn, "src/a.rs", 1.0, "h").unwrap();
        let chunk_id = insert_chunk(
            &conn,
            NewChunk {
                file_id,
                path: "src/a.rs",
                start: 1,
                end: 5,
                symbol: "f",
                kind: "function",
                content: "fn f() {}",
                token_count: 3,
            },
        )
        .unwrap();
        // Legacy f32 row (scale NULL), as written by pre-quantization builds.
        let v = vec![0.1f32, -0.2, 0.3, -0.4];
        conn.execute(
            "INSERT INTO embeddings(chunk_id, embedding) VALUES(?1, ?2)",
            params![chunk_id, serialize_vec(&v)],
        )
        .unwrap();

        assert_eq!(backfill_quantized_embeddings(&conn).unwrap(), 1);
        assert_eq!(backfill_quantized_embeddings(&conn).unwrap(), 0); // idempotent
        let (quantized, total) = quantization_coverage(&conn).unwrap();
        assert_eq!((quantized, total), (1, 1));

        // Search still ranks the migrated row correctly via the q8 path.
        let results = search_similar(&conn, &v, 1, None).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, chunk_id);
        assert!(results[0].distance < 0.01, "self-similarity ~1.0");
    }

    #[test]
    fn test_hook_log_rotation() {
        let repo = std::env::temp_dir().join(format!("tokenix_test_rotate_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&repo);
        let log = log_path(&repo);
        if let Some(parent) = log.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        // Seed an oversized log so the next append rotates it.
        std::fs::write(&log, vec![b'x'; (HOOK_LOG_MAX_BYTES + 1) as usize]).unwrap();

        let ev = HookEvent {
            ts: 1.0,
            tool: "Read".to_string(),
            action: "pass".to_string(),
            reason: String::new(),
            saved_tokens: 0,
            actual_tokens: 0,
            original_estimate: 0,
            input_preview: String::new(),
            phase: "pre".to_string(),
            command: String::new(),
        };
        log_hook_event(&repo, &ev).unwrap();

        let rotated = rotated_log_path(&log);
        assert!(rotated.exists(), "oversized log must rotate to .1");
        assert!(
            std::fs::metadata(&log).unwrap().len() < 1_000,
            "fresh log only holds the new event"
        );
        // Both generations are read; the rotated junk lines are skipped.
        let events = read_hook_log(&repo);
        assert_eq!(events.len(), 1);

        let _ = std::fs::remove_file(&log);
        let _ = std::fs::remove_file(&rotated);
        let _ = std::fs::remove_dir_all(&repo);
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
        let sparse_results = search_fts(&conn, "fts5 hybrid", 10, None).unwrap();
        assert_eq!(sparse_results.len(), 1);
        assert_eq!(sparse_results[0].0, chunk_id);
        assert!(sparse_results[0].1 > 0.0, "BM25 score should be positive");

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
        assert!(search_regex(&conn, "token", 10, None, false)
            .unwrap()
            .is_empty());
        assert_eq!(
            search_regex(&conn, "token", 10, None, true).unwrap().len(),
            1
        );

        // File filter scopes results.
        assert!(search_regex(&conn, "fn ", 10, Some("lib.rs"), false)
            .unwrap()
            .iter()
            .all(|r| r.path == "src/lib.rs"));
    }

    #[test]
    fn graph_search_and_relations_dedupe_duplicate_visible_symbols() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn, 4).unwrap();
        let file_id = upsert_file(&conn, "src/hook.rs", 1.0, "h1").unwrap();
        let caller_id = upsert_file(&conn, "src/main.rs", 1.0, "h2").unwrap();
        let first_run_hook = insert_chunk(
            &conn,
            NewChunk {
                file_id,
                path: "src/hook.rs",
                start: 478,
                end: 714,
                symbol: "run_hook",
                kind: "function",
                content: "fn run_hook() {}",
                token_count: 4,
            },
        )
        .unwrap();
        let second_run_hook = insert_chunk(
            &conn,
            NewChunk {
                file_id,
                path: "src/hook.rs",
                start: 478,
                end: 714,
                symbol: "run_hook",
                kind: "function",
                content: "fn run_hook() {}",
                token_count: 4,
            },
        )
        .unwrap();
        let main = insert_chunk(
            &conn,
            NewChunk {
                file_id: caller_id,
                path: "src/main.rs",
                start: 401,
                end: 420,
                symbol: "main",
                kind: "function",
                content: "fn main() { hook::run_hook(); }",
                token_count: 8,
            },
        )
        .unwrap();
        let run_hook_post = insert_chunk(
            &conn,
            NewChunk {
                file_id,
                path: "src/compress.rs",
                start: 612,
                end: 679,
                symbol: "run_hook_post",
                kind: "function",
                content: "fn run_hook_post() {}",
                token_count: 5,
            },
        )
        .unwrap();

        insert_graph_node(
            &conn,
            first_run_hook,
            file_id,
            "src/hook.rs",
            "run_hook",
            "function",
            478,
            714,
        )
        .unwrap();
        insert_graph_node(
            &conn,
            second_run_hook,
            file_id,
            "src/hook.rs",
            "run_hook",
            "function",
            478,
            714,
        )
        .unwrap();
        insert_graph_node(
            &conn,
            main,
            caller_id,
            "src/main.rs",
            "main",
            "function",
            401,
            420,
        )
        .unwrap();
        insert_graph_node(
            &conn,
            run_hook_post,
            file_id,
            "src/compress.rs",
            "run_hook_post",
            "function",
            612,
            679,
        )
        .unwrap();
        insert_graph_edge(&conn, main, first_run_hook, "hook::run_hook", "references").unwrap();
        insert_graph_edge(&conn, main, second_run_hook, "hook::run_hook", "references").unwrap();
        insert_graph_edge(&conn, main, run_hook_post, "run_hook_post", "references").unwrap();

        let nodes = search_graph_nodes(&conn, "run_hook", 10).unwrap();
        assert_eq!(
            nodes.iter().filter(|node| node.name == "run_hook").count(),
            1
        );
        assert!(nodes.iter().any(|node| node.name == "run_hook_post"));

        let callers = graph_callers(&conn, "run_hook", 10).unwrap();
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0].from.name, "main");
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
