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

/// `~/.tokenix`, or `$TOKENIX_HOME` when set, so homologation runs and
/// scripted checks keep their state out of the user's real home. Every
/// machine-wide tokenix path must derive from here.
pub fn global_dir() -> Option<PathBuf> {
    home_override(std::env::var_os("TOKENIX_HOME")).or_else(default_home)
}

#[cfg(not(test))]
fn default_home() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".tokenix"))
}

/// Unit tests never see the developer's `~/.tokenix`: its user filters shadow
/// the bundled ones under test, and its logs and caches are not theirs to write.
#[cfg(test)]
fn default_home() -> Option<PathBuf> {
    Some(std::env::temp_dir().join(format!("tokenix-unit-{}", std::process::id())))
}

/// Only an absolute `TOKENIX_HOME` counts: a relative one resolves against the
/// agent's current directory, scattering state across repositories.
fn home_override(value: Option<std::ffi::OsString>) -> Option<PathBuf> {
    value.map(PathBuf::from).filter(|p| p.is_absolute())
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
/// Nearest ancestor that was indexed or carries a project marker. An index
/// counts first: `tokenix index <dir>` roots at `<dir>` even when a parent
/// (often the home directory) holds a `package.json`.
pub fn find_project_root(start: &Path) -> PathBuf {
    find_project_root_with(start, has_index)
}

/// `.name` is written by every index run, including branch-aware ones whose
/// DB file carries a branch suffix.
fn has_index(dir: &Path) -> bool {
    let Some(global) = global_dir() else {
        return false;
    };
    let id = project_id(dir);
    ["db", "name"]
        .iter()
        .any(|ext| global.join(format!("{id}.{ext}")).exists())
}

fn find_project_root_with(start: &Path, is_indexed: impl Fn(&Path) -> bool) -> PathBuf {
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
        if is_indexed(current) || markers.iter().any(|m| current.join(m).exists()) {
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
    // Embeddings were removed: drop the vector tables an older index still
    // carries. DROP only frees pages, so flag the file for `vacuum_if_flagged`,
    // which a full `tokenix index` runs (never an inline query refresh).
    let had_legacy: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('embeddings','embedding_cache')",
        [],
        |r| r.get(0),
    )?;
    if had_legacy > 0 {
        conn.execute_batch(
            "DROP TABLE IF EXISTS embeddings; DROP TABLE IF EXISTS embedding_cache;",
        )?;
        set_meta(conn, "needs_vacuum", "1")?;
    }
    Ok(())
}

/// Reclaim the space left by dropped legacy vector tables. Returns whether a
/// VACUUM ran. A failure keeps the flag set so the next full index retries.
pub fn vacuum_if_flagged(conn: &Connection) -> Result<bool> {
    if meta_value(conn, "needs_vacuum").as_deref() != Some("1") {
        return Ok(false);
    }
    conn.execute_batch("VACUUM")?;
    set_meta(conn, "needs_vacuum", "0")?;
    Ok(true)
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
    /// Lower is better; `1 - fused rank score`.
    pub distance: f32,
}

/// Reciprocal-rank-fusion damping constant (the classic k=60) shared by every
/// ranking path (store, query graph/path recall).
pub const RRF_K: f32 = 60.0;
/// Weight of the normalized BM25 score blended into the sparse-side RRF term.
pub const BM25_WEIGHT: f32 = 0.3;

/// Ranked full-text search over chunks: BM25 fused with the FTS position via
/// reciprocal rank, best (lowest `distance`) first.
pub fn lexical_search(
    conn: &Connection,
    query_text: &str,
    k: usize,
    file_filter: Option<&str>,
) -> Result<Vec<SearchResult>> {
    let sparse_limit = 100.max(k * 2);
    let sparse_results = search_fts(conn, query_text, sparse_limit, file_filter)?;

    let scored: Vec<(i64, f32)> = sparse_results
        .iter()
        .enumerate()
        .map(|(rank, (id, bm25_score))| {
            let rrf_position = 1.0 / (RRF_K + rank as f32);
            let bm25_normalized = (*bm25_score).max(0.0) / (1.0 + bm25_score.max(0.0));
            (*id, rrf_position + BM25_WEIGHT * bm25_normalized)
        })
        .take(k * 2)
        .collect();
    if scored.is_empty() {
        return Ok(Vec::new());
    }

    let ids: Vec<i64> = scored.iter().map(|(id, _)| *id).collect();
    let mut by_id: HashMap<i64, SearchResult> = fetch_chunks_by_ids(conn, &ids)?
        .into_iter()
        .map(|r| (r.id, r))
        .collect();

    let mut results: Vec<SearchResult> = scored
        .into_iter()
        .filter_map(|(id, score)| {
            by_id.remove(&id).map(|mut r| {
                r.distance = 1.0 - score;
                r
            })
        })
        .collect();
    results.sort_by(|a, b| {
        a.distance
            .partial_cmp(&b.distance)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(k);
    Ok(results)
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
    // Windows has no chmod. The caller's files live under the user profile,
    // whose default ACL already grants only the user, SYSTEM and the local
    // Administrators group — an unprivileged *other* account cannot read them.
    // Tightening further (icacls /inheritance:r) risks locking the owner out of
    // their own index if the explicit grant fails, so this stays a no-op and
    // the docs say so rather than claiming a 0600 that never happened.
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

    #[test]
    fn tokenix_home_override_must_be_absolute() {
        // A relative value resolves against whatever directory the agent is in,
        // which scatters state across repos and can put the trust store inside
        // one of them.
        for rejected in ["", ".tokenix", "repo/.tokenix"] {
            assert_eq!(home_override(Some(rejected.into())), None, "{rejected:?}");
        }
        assert_eq!(home_override(None), None);
        let abs = std::env::temp_dir().join("tokenix-home");
        assert_eq!(home_override(Some(abs.clone().into())), Some(abs));
    }

    #[test]
    fn an_indexed_dir_is_the_root_even_under_a_marked_ancestor() {
        // `tokenix index <dir>` stores the index under <dir>, but a marker-less
        // repo below a folder holding `package.json` (a home directory, often)
        // used to resolve to that ancestor, so every later lookup missed it.
        let base = std::env::temp_dir().join(format!("tokenix-root-{}", std::process::id()));
        let repo = base.join("repo");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(base.join("package.json"), "{}").unwrap();
        let indexed = repo.canonicalize().unwrap();
        let marked = base.canonicalize().unwrap();

        let root = find_project_root_with(&repo.join("src"), |p| p == indexed);
        let unindexed = find_project_root_with(&repo.join("src"), |_| false);
        let _ = std::fs::remove_dir_all(&base);

        assert_eq!(root, indexed, "the indexed dir must win");
        assert_eq!(
            unindexed, marked,
            "without an index, the nearest marker still decides"
        );
    }

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
    fn init_schema_reclaims_the_space_of_legacy_vectors() {
        let dir = std::env::temp_dir().join(format!("tokenix_vac_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("legacy.db");
        let _ = std::fs::remove_file(&path);
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE embeddings(chunk_id INTEGER PRIMARY KEY, embedding BLOB, scale REAL);
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i < 200)
             INSERT INTO embeddings SELECT i, zeroblob(20000), 1.0 FROM n;",
        )
        .unwrap();
        let before = std::fs::metadata(&path).unwrap().len();
        init_schema(&conn, 4).unwrap();
        assert!(vacuum_if_flagged(&conn).unwrap());
        assert!(
            !vacuum_if_flagged(&conn).unwrap(),
            "flag is cleared after a VACUUM"
        );
        drop(conn);
        let after = std::fs::metadata(&path).unwrap().len();
        assert!(
            after < before / 4,
            "index should shrink once vectors are gone: {before} -> {after}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn init_schema_drops_legacy_embedding_tables() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE embeddings(chunk_id INTEGER PRIMARY KEY, embedding BLOB, scale REAL);
             CREATE TABLE embedding_cache(content_hash TEXT PRIMARY KEY, embedding BLOB, updated_at REAL);
             INSERT INTO embeddings VALUES(1, zeroblob(8), 1.0);",
        )
        .unwrap();
        init_schema(&conn, 4).unwrap();
        let left: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('embeddings','embedding_cache')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(left, 0, "legacy vector tables must not survive a re-index");
        // The rest of the index still works on the migrated database.
        let file_id = upsert_file(&conn, "src/a.rs", 1.0, "h").unwrap();
        assert!(delete_chunks_for_file(&conn, file_id).is_ok());
    }
    #[test]
    fn test_lexical_search() {
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

        // 3. Test search_fts
        let sparse_results = search_fts(&conn, "fts5 hybrid", 10, None).unwrap();
        assert_eq!(sparse_results.len(), 1);
        assert_eq!(sparse_results[0].0, chunk_id);
        assert!(sparse_results[0].1 > 0.0, "BM25 score should be positive");

        // 4. Test lexical_search
        let results = lexical_search(&conn, "hello search", 10, None).unwrap();
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
