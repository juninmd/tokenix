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

// ---- SIMD-friendly dot product (chunks of 8 floats → AVX2 auto-vectorized) --

#[inline]
fn dot_product(a: &[f32], b: &[f32]) -> f32 {
    let chunks = a.chunks_exact(8).zip(b.chunks_exact(8));
    let mut sum: f32 = chunks
        .map(|(ca, cb)| ca.iter().zip(cb.iter()).map(|(x, y)| x * y).sum::<f32>())
        .sum();
    // handle remainder (768 % 8 == 0, so this is a no-op for nomic-embed-text-v1.5)
    let rem_a = a.chunks_exact(8).remainder();
    let rem_b = b.chunks_exact(8).remainder();
    sum += rem_a
        .iter()
        .zip(rem_b.iter())
        .map(|(x, y)| x * y)
        .sum::<f32>();
    sum
}

// ---- In-memory per-project cache --------------------------------------------

const MAX_CACHED_PROJECTS: usize = 3;

/// Above this many cached chunks, the cosine scan is parallelized across cores.
/// Below it, the rayon overhead outweighs the gain, so a plain loop is used.
const PARALLEL_SCAN_THRESHOLD: usize = 2000;

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
                let norm: f32 = dot_product(&e.embedding, &e.embedding).sqrt();
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
        Ok(Self {
            entries,
            db_mtime,
            content: HashMap::new(),
        })
    }

    fn search_ids(&self, query: &[f32], k: usize, file_filter: Option<&str>) -> Vec<(usize, f32)> {
        let q_norm: f32 = dot_product(query, query).sqrt();
        if q_norm == 0.0 {
            return vec![];
        }
        let score = |i: usize, e: &CachedEntry| -> Option<(f32, usize)> {
            if let Some(filter) = file_filter {
                if !e.path.contains(filter) {
                    return None;
                }
            }
            let dot = dot_product(query, &e.embedding);
            let sim = if e.norm == 0.0 {
                0.0
            } else {
                dot / (q_norm * e.norm)
            };
            Some((sim, i))
        };
        // Parallelize the O(N) scan on large projects; stay single-threaded on
        // small ones where rayon's overhead would dominate.
        let mut scored: Vec<(f32, usize)> = if self.entries.len() >= PARALLEL_SCAN_THRESHOLD {
            use rayon::prelude::*;
            self.entries
                .par_iter()
                .enumerate()
                .filter_map(|(i, e)| score(i, e))
                .collect()
        } else {
            self.entries
                .iter()
                .enumerate()
                .filter_map(|(i, e)| score(i, e))
                .collect()
        };
        // Top-k via partial selection: O(N) instead of O(N log N) full sort.
        // Only the k best need ordering; the rest are discarded.
        let desc = |a: &(f32, usize), b: &(f32, usize)| {
            b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal)
        };
        if k < scored.len() {
            scored.select_nth_unstable_by(k, desc);
            scored.truncate(k);
        }
        scored.sort_by(desc);
        scored.into_iter().map(|(sim, i)| (i, sim)).collect()
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
        Self {
            projects: HashMap::new(),
            lru: Vec::new(),
        }
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
        file: Option<String>,
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
    // Must be set before ONNX Runtime initializes its thread pool (reads OMP_NUM_THREADS at init).
    #[allow(unused_unsafe)]
    unsafe {
        if std::env::var("OMP_NUM_THREADS").is_err() {
            std::env::set_var("OMP_NUM_THREADS", "2");
        }
    }

    // The daemon is often autostarted by a hook that set RAYON_NUM_THREADS=1.
    // Override (before the global rayon pool is first built) so the cosine scan
    // can use multiple cores on large projects. Bounded to avoid oversubscribing
    // the 4-thread connection pool.
    #[allow(unused_unsafe)]
    unsafe {
        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .min(8);
        std::env::set_var("RAYON_NUM_THREADS", threads.to_string());
    }

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

    // On Linux, sockets enter TIME_WAIT after close. Without SO_REUSEADDR a
    // rapid daemon restart (crash → respawn) fails with "Address already in use"
    // for up to 60 s. Windows sets this by default; Unix does not.
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        unsafe {
            let opt: libc::c_int = 1;
            libc::setsockopt(
                listener.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_REUSEADDR,
                &opt as *const libc::c_int as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
        }
    }

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
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
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

pub fn daemon_search(
    project_root: &Path,
    query: &str,
    k: usize,
    budget: usize,
    file_filter: Option<&str>,
) -> Option<String> {
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
        "file": file_filter,
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
    file_filter: Option<&str>,
) -> Option<String> {
    // Fast path: daemon already running.
    if let Some(out) = daemon_search(project_root, query, k, budget, file_filter) {
        return Some(out);
    }

    // Slow path: spawn daemon and retry.
    if !spawn_daemon() {
        return None;
    }
    std::thread::sleep(Duration::from_millis(800));
    daemon_search(project_root, query, k, budget, file_filter)
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

    // On Unix, detach from the terminal session so the daemon survives after the
    // spawning hook process exits. Without setsid() the child stays in the same
    // process group and receives SIGHUP when the parent's session ends.
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt as _;
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
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
    {
        // kill(pid, 0) returns 0 if process exists and we have permission,
        // -1+ESRCH if it does not exist, -1+EPERM if it exists but we lack
        // permission (another user's process). EPERM means alive.
        let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
        if rc == 0 {
            return true;
        }
        // Read errno portably: glibc's __errno_location is not available on macOS.
        // EPERM (process owned by another user) still means the process is alive.
        std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
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
            file,
        }) => search_handler(&state, &project_root, &query, k, budget, file.as_deref()),
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
    file_filter: Option<&str>,
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

    // Run sparse FTS5 search (holds no cache lock, query is fast)
    let sparse_limit = (k.saturating_mul(5)).max(50);
    let sparse_ids = store::search_fts(&conn, query, sparse_limit, file_filter).unwrap_or_default();

    // Acquire lock, reload cache if stale, run cosine search + RRF merge, release lock.
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
        let candidate_k = (k.saturating_mul(5)).max(50);
        let dense_results = pc.search_ids(&query_vec, candidate_k, file_filter);

        let mut rrf_scores: HashMap<i64, f32> = HashMap::new();
        let mut dense_map: HashMap<i64, (usize, f32)> = HashMap::new();

        for (rank, &(idx, sim)) in dense_results.iter().enumerate() {
            let id = pc.entries[idx].id;
            dense_map.insert(id, (idx, sim));
            let score = 1.0 / (60.0 + rank as f32);
            rrf_scores.insert(id, score);
        }

        for (rank, id) in sparse_ids.iter().enumerate() {
            let score = 1.0 / (60.0 + rank as f32);
            rrf_scores
                .entry(*id)
                .and_modify(|s| *s += score)
                .or_insert(score);
        }

        let mut sorted_candidates: Vec<(i64, f32)> = rrf_scores.into_iter().collect();
        sorted_candidates
            .sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let top_candidates: Vec<(i64, f32)> =
            sorted_candidates.into_iter().take(candidate_k).collect();

        let mut top_ids_mapped = Vec::new();
        for (id, rrf_score) in top_candidates {
            if let Some(&(idx, _)) = dense_map.get(&id) {
                top_ids_mapped.push((idx, rrf_score, id));
            } else if let Some(idx) = pc.entries.iter().position(|e| e.id == id) {
                top_ids_mapped.push((idx, rrf_score, id));
            }
        }
        top_ids_mapped
    };

    // Populate content: check in-memory cache first, fetch missing from SQLite.
    let chunk_ids: Vec<i64> = top_ids.iter().map(|(_, _, id)| *id).collect();

    let mut cache_lock = state.cache.lock().unwrap();
    let pc = match cache_lock.projects.get_mut(&root_key) {
        Some(p) => p,
        None => return err_json("cache evicted during search".into()),
    };

    let missing: Vec<i64> = chunk_ids
        .iter()
        .copied()
        .filter(|id| !pc.content.contains_key(id))
        .collect();
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
        .map(|(idx, rrf_score, id)| {
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
                distance: 1.0 - rrf_score,
            }
        })
        .collect();
    drop(cache_lock);

    crate::query::rerank_results(&mut results, query);

    let mut budget_left = budget;
    results.retain(|r| {
        let t = if r.token_count > 0 {
            r.token_count
        } else {
            count_tokens(&r.content)
        };
        if budget_left >= t {
            budget_left -= t;
            true
        } else {
            false
        }
    });
    results.truncate(k);

    let output = format_results(&results, query);
    serde_json::to_string(&RespOk { ok: true, output }).unwrap()
}

fn err_json(msg: String) -> String {
    serde_json::to_string(&RespErr {
        ok: false,
        error: msg,
    })
    .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dot_product() {
        let a = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        let b = vec![1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0];
        assert_eq!(dot_product(&a, &b), 36.0);

        let c = vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let d = vec![0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        assert_eq!(dot_product(&c, &d), 0.0);
    }

    #[test]
    fn test_cache_state_lru() {
        let mut state = CacheState::new();
        assert_eq!(state.lru.len(), 0);

        let pc1 = ProjectCache {
            entries: vec![],
            db_mtime: 1.0,
            content: HashMap::new(),
        };
        let pc2 = ProjectCache {
            entries: vec![],
            db_mtime: 2.0,
            content: HashMap::new(),
        };
        let pc3 = ProjectCache {
            entries: vec![],
            db_mtime: 3.0,
            content: HashMap::new(),
        };
        let pc4 = ProjectCache {
            entries: vec![],
            db_mtime: 4.0,
            content: HashMap::new(),
        };

        state.insert("p1".to_string(), pc1);
        state.insert("p2".to_string(), pc2);
        state.insert("p3".to_string(), pc3);

        assert_eq!(state.lru, vec!["p3", "p2", "p1"]);

        // Touch p1 to make it most recent
        state.touch("p1");
        assert_eq!(state.lru, vec!["p1", "p3", "p2"]);

        // Insert p4, causing eviction of the oldest (p2)
        state.insert("p4".to_string(), pc4);
        assert_eq!(state.lru, vec!["p4", "p1", "p3"]);
        assert!(!state.projects.contains_key("p2"));
        assert!(state.projects.contains_key("p1"));
        assert!(state.projects.contains_key("p3"));
        assert!(state.projects.contains_key("p4"));
    }

    #[test]
    fn test_daemon_port() {
        std::env::set_var("TOKENIX_DAEMON_PORT", "12345");
        assert_eq!(daemon_port(), 12345);
        std::env::remove_var("TOKENIX_DAEMON_PORT");
        assert_eq!(daemon_port(), DEFAULT_PORT);
    }
}
