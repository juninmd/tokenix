use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;


use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use crate::chunker::count_tokens;
use crate::embed::embed_query;
use crate::query::format_results;
use crate::store::{self, SearchResult};

pub const DEFAULT_PORT: u16 = 47392;
const CONNECT_TIMEOUT_MS: u64 = 200;
const READ_TIMEOUT_MS: u64 = 15_000; // model load on first request can take ~500ms

// ---- In-memory per-project cache --------------------------------------------

const MAX_CACHED_PROJECTS: usize = 3;

struct CachedEntry {
    id: i64,
    path: String,
    start_line: usize,
    end_line: usize,
    symbol: String,
    kind: String,
    token_count: usize,
    // content intentionally omitted — fetched from SQLite for top-K results only
    embedding: Vec<f32>,
    norm: f32,
}

struct ProjectCache {
    entries: Vec<CachedEntry>,
    db_mtime: f64,
    /// Content cache: populated on first fetch, avoids re-hitting SQLite for hot chunks.
    content: HashMap<i64, String>,
}

impl ProjectCache {
    fn load(conn: &rusqlite::Connection, db_mtime: f64) -> Result<Self> {
        let entries = store::load_all_embeddings(conn)?
            .into_iter()
            .map(|e| {
                let norm: f32 = e.embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
                CachedEntry {
                    id: e.id,
                    path: e.path,
                    start_line: e.start_line,
                    end_line: e.end_line,
                    symbol: e.symbol,
                    kind: e.kind,
                    token_count: e.token_count,
                    embedding: e.embedding,
                    norm,
                }
            })
            .collect();
        Ok(Self { entries, db_mtime, content: HashMap::new() })
    }

    /// Cosine search over embedding vectors — returns top-K without content.
    /// Content is fetched from SQLite by the caller for the final result set.
    fn search_ids(&self, query: &[f32], k: usize) -> Vec<(usize, f32)> {
        let q_norm: f32 = query.iter().map(|x| x * x).sum::<f32>().sqrt();
        if q_norm == 0.0 {
            return vec![];
        }
        let mut scored: Vec<(f32, usize)> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let dot: f32 = query.iter().zip(&e.embedding).map(|(a, b)| a * b).sum();
                let sim = if e.norm == 0.0 { 0.0 } else { dot / (q_norm * e.norm) };
                (sim, i)
            })
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.into_iter().take(k).map(|(sim, i)| (i, sim)).collect()
    }
}

// ---- Daemon state (LRU-capped cache) ----------------------------------------

struct CacheState {
    projects: HashMap<String, ProjectCache>,
    /// Keys in LRU order: front = most recently used, back = oldest.
    lru: Vec<String>,
}

impl CacheState {
    fn new() -> Self {
        Self { projects: HashMap::new(), lru: Vec::new() }
    }

    fn touch(&mut self, key: &str) {
        self.lru.retain(|k| k != key);
        self.lru.insert(0, key.to_string());
    }

    fn insert(&mut self, key: String, cache: ProjectCache) {
        // Evict least-recently-used project when at capacity.
        while self.lru.len() >= MAX_CACHED_PROJECTS {
            if let Some(oldest) = self.lru.pop() {
                self.projects.remove(&oldest);
                eprintln!("[tokenix] evicted cache for {oldest} (LRU limit {MAX_CACHED_PROJECTS})");
            }
        }
        self.lru.insert(0, key.clone());
        self.projects.insert(key, cache);
    }
}

struct DaemonState {
    cache: Mutex<CacheState>,
}

// ---- Protocol ---------------------------------------------------------------

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Request {
    Search {
        project_root: String,
        query: String,
        #[serde(default = "default_k")]
        k: usize,
        #[serde(default = "default_budget")]
        budget: usize,
    },
    Health,
}

fn default_k() -> usize {
    20
}
fn default_budget() -> usize {
    2500
}

#[derive(Serialize)]
struct RespOk {
    ok: bool,
    output: String,
}

#[derive(Serialize)]
struct RespErr {
    ok: bool,
    error: String,
}

#[derive(Serialize)]
struct RespHealth {
    ok: bool,
    cached_projects: usize,
    chunks: usize,
}

// ---- Path helpers -----------------------------------------------------------

pub fn daemon_port() -> u16 {
    std::env::var("TOKENIX_DAEMON_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_PORT)
}

fn pid_path() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".tokenix").join("daemon.pid"))
}

fn port_path() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".tokenix").join("daemon.port"))
}

// ---- Server -----------------------------------------------------------------

pub fn run_serve(port: Option<u16>) -> Result<()> {
    let port = port.unwrap_or_else(daemon_port);

    if let Some(p) = pid_path() {
        let _ = std::fs::create_dir_all(p.parent().unwrap_or(&p));
        let _ = std::fs::write(&p, std::process::id().to_string());
    }
    if let Some(p) = port_path() {
        let _ = std::fs::create_dir_all(p.parent().unwrap_or(&p));
        let _ = std::fs::write(&p, port.to_string());
    }

    let state = Arc::new(DaemonState {
        cache: Mutex::new(CacheState::new()),
    });

    let pool = Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .map_err(|e| anyhow!("thread pool: {e}"))?,
    );

    let listener = TcpListener::bind(format!("127.0.0.1:{port}"))?;
    eprintln!("[tokenix] daemon pid={} port={port}", std::process::id());

    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let state = Arc::clone(&state);
                let pool = Arc::clone(&pool);
                pool.spawn(move || {
                    if let Err(e) = handle_connection(s, state) {
                        eprintln!("[tokenix] connection error: {e}");
                    }
                });
            }
            Err(e) => eprintln!("[tokenix] accept error: {e}"),
        }
    }
    Ok(())
}

pub fn run_stop() -> Result<()> {
    let pid_str = pid_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .ok_or_else(|| anyhow!("daemon not running (no pid file)"))?;
    let pid: u32 = pid_str.trim().parse()?;

    #[cfg(unix)]
    std::process::Command::new("kill")
        .arg(pid.to_string())
        .status()?;
    #[cfg(windows)]
    std::process::Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .status()?;

    if let Some(p) = pid_path() {
        let _ = std::fs::remove_file(p);
    }
    if let Some(p) = port_path() {
        let _ = std::fs::remove_file(p);
    }
    println!("daemon stopped (pid {pid})");
    Ok(())
}

// ---- Client -----------------------------------------------------------------

/// Send a search request to the running daemon and return the formatted output.
/// Returns `None` if the daemon is not reachable or returns an error.
pub fn daemon_search(project_root: &Path, query: &str, k: usize, budget: usize) -> Option<String> {
    let port = daemon_port();
    let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().ok()?;
    let timeout = Duration::from_millis(CONNECT_TIMEOUT_MS);

    let mut stream = TcpStream::connect_timeout(&addr, timeout).ok()?;
    stream.set_nodelay(true).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(READ_TIMEOUT_MS)))
        .ok()?;

    let req = serde_json::json!({
        "type": "search",
        "project_root": project_root.to_string_lossy(),
        "query": query,
        "k": k,
        "budget": budget,
    });
    stream.write_all(format!("{req}\n").as_bytes()).ok()?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;

    let v: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    if v["ok"].as_bool()? {
        v["output"].as_str().map(str::to_string)
    } else {
        None
    }
}

/// Try to reach the daemon; if not running, spawn it and retry once.
pub fn daemon_search_with_autostart(
    project_root: &Path,
    query: &str,
    k: usize,
    budget: usize,
) -> Option<String> {
    // Fast path: daemon already running.
    if let Some(out) = daemon_search(project_root, query, k, budget) {
        return Some(out);
    }

    // Slow path: spawn daemon and retry.
    if !spawn_daemon() {
        return None;
    }
    std::thread::sleep(Duration::from_millis(800));
    daemon_search(project_root, query, k, budget)
}

fn spawn_daemon() -> bool {
    // Prevent race: if PID file exists and process is alive, skip spawn.
    if let Some(pid_file) = pid_path() {
        if let Ok(s) = std::fs::read_to_string(&pid_file) {
            if let Ok(pid) = s.trim().parse::<u32>() {
                if is_process_alive(pid) {
                    return true;
                }
            }
        }
        // Spawn lock: if another hook is already spawning, wait and skip.
        let lock = pid_file.with_extension("spawning");
        if lock.exists() {
            let stale = std::fs::metadata(&lock)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.elapsed().ok())
                .map(|e| e.as_secs() >= 10)
                .unwrap_or(true);
            if !stale {
                return true; // another hook is already spawning
            }
        }
        let _ = std::fs::write(&lock, std::process::id().to_string());
    }

    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(_) => return false,
    };

    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("serve")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NO_WINDOW
        cmd.creation_flags(0x00000008 | 0x08000000);
    }

    cmd.spawn().is_ok()
}

fn is_process_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let out = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .output();
        if let Ok(o) = out {
            return String::from_utf8_lossy(&o.stdout).contains(&pid.to_string());
        }
        false
    }
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as libc::pid_t, 0) == 0
    }
}

// ---- Connection handler (server side) ---------------------------------------

fn handle_connection(stream: TcpStream, state: Arc<DaemonState>) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_millis(READ_TIMEOUT_MS)))?;
    stream.set_nodelay(true)?;

    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;

    let response_str = match serde_json::from_str::<Request>(line.trim()) {
        Ok(Request::Health) => {
            let lock = state.cache.lock().unwrap();
            let cached_projects = lock.projects.len();
            let chunks = lock.projects.values().map(|c| c.entries.len()).sum();
            serde_json::to_string(&RespHealth {
                ok: true,
                cached_projects,
                chunks,
            })?
        }
        Ok(Request::Search {
            project_root,
            query,
            k,
            budget,
        }) => search_handler(&state, &project_root, &query, k, budget),
        Err(e) => serde_json::to_string(&RespErr {
            ok: false,
            error: e.to_string(),
        })?,
    };

    writer.write_all(response_str.as_bytes())?;
    writer.write_all(b"\n")?;
    Ok(())
}

fn search_handler(
    state: &DaemonState,
    project_root: &str,
    query: &str,
    k: usize,
    budget: usize,
) -> String {
    // Embed outside the lock — takes 50-100ms, holds no mutex.
    let query_vec = match embed_query(query) {
        Ok(v) => v,
        Err(e) => return err_json(e.to_string()),
    };

    let root_path = Path::new(project_root);
    let root_key = store::project_id(root_path);
    let db_mtime = store::get_db_mtime(root_path);

    // Open DB connection (needed for cache reload and content fetch).
    let conn = match store::open_db(root_path, false) {
        Ok(Some(c)) => c,
        _ => return err_json("db not found".into()),
    };

    // Acquire lock, reload cache if stale, run cosine search, release lock.
    let top_ids: Vec<(usize, f32, i64)> = {
        let mut cache_lock = state.cache.lock().unwrap();
        let needs_reload = cache_lock
            .projects
            .get(&root_key)
            .map(|c| (db_mtime - c.db_mtime).abs() > 0.5)
            .unwrap_or(true);

        if needs_reload {
            match ProjectCache::load(&conn, db_mtime) {
                Ok(c) => {
                    eprintln!(
                        "[tokenix] cache loaded: {} chunks for {}",
                        c.entries.len(),
                        root_key
                    );
                    cache_lock.insert(root_key.clone(), c);
                }
                Err(e) => return err_json(format!("cache load: {e}")),
            }
        } else {
            cache_lock.touch(&root_key);
        }

        let pc = &cache_lock.projects[&root_key];
        pc.search_ids(&query_vec, k)
            .into_iter()
            .map(|(idx, sim)| (idx, sim, pc.entries[idx].id))
            .collect()
    };

    // Populate content: check in-memory cache first, fetch missing from SQLite.
    let chunk_ids: Vec<i64> = top_ids.iter().map(|(_, _, id)| *id).collect();

    let mut cache_lock = state.cache.lock().unwrap();
    let pc = match cache_lock.projects.get_mut(&root_key) {
        Some(p) => p,
        None => return err_json("cache evicted during search".into()),
    };

    let missing: Vec<i64> = chunk_ids.iter().copied().filter(|id| !pc.content.contains_key(id)).collect();
    if !missing.is_empty() {
        if let Ok(fetched) = store::fetch_chunks_content(&conn, &missing) {
            if pc.content.len() > 1000 {
                pc.content.clear();
            }
            pc.content.extend(fetched);
        }
    }

    let mut results: Vec<SearchResult> = top_ids
        .iter()
        .map(|(idx, sim, id)| {
            let e = &pc.entries[*idx];
            SearchResult {
                id: e.id,
                path: e.path.clone(),
                start_line: e.start_line,
                end_line: e.end_line,
                symbol: e.symbol.clone(),
                kind: e.kind.clone(),
                content: pc.content.get(id).cloned().unwrap_or_default(),
                token_count: e.token_count,
                distance: 1.0 - sim,
            }
        })
        .collect();
    drop(cache_lock);

    let mut budget_left = budget;
    results.retain(|r| {
        let t = if r.token_count > 0 { r.token_count } else { count_tokens(&r.content) };
        if budget_left >= t { budget_left -= t; true } else { false }
    });

    let output = format_results(&results, query);
    serde_json::to_string(&RespOk { ok: true, output }).unwrap()
}

fn err_json(msg: String) -> String {
    serde_json::to_string(&RespErr { ok: false, error: msg }).unwrap()
}
