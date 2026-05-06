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

struct CachedEntry {
    id: i64,
    path: String,
    start_line: usize,
    end_line: usize,
    symbol: String,
    kind: String,
    token_count: usize,
    content: String,
    embedding: Vec<f32>,
    norm: f32,
}

struct ProjectCache {
    entries: Vec<CachedEntry>,
    db_mtime: f64,
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
                    content: e.content,
                    embedding: e.embedding,
                    norm,
                }
            })
            .collect();
        Ok(Self { entries, db_mtime })
    }

    fn search(&self, query: &[f32], k: usize) -> Vec<SearchResult> {
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
        scored
            .into_iter()
            .take(k)
            .map(|(sim, i)| {
                let e = &self.entries[i];
                SearchResult {
                    id: e.id,
                    path: e.path.clone(),
                    start_line: e.start_line,
                    end_line: e.end_line,
                    symbol: e.symbol.clone(),
                    kind: e.kind.clone(),
                    content: e.content.clone(),
                    token_count: e.token_count,
                    distance: 1.0 - sim,
                }
            })
            .collect()
    }
}

// ---- Daemon state -----------------------------------------------------------

struct DaemonState {
    cache: Mutex<HashMap<String, ProjectCache>>,
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
        cache: Mutex::new(HashMap::new()),
    });

    let listener = TcpListener::bind(format!("127.0.0.1:{port}"))?;
    eprintln!("[tokenix] daemon pid={} port={port}", std::process::id());

    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let state = Arc::clone(&state);
                std::thread::spawn(move || {
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
            let cached_projects = lock.len();
            let chunks = lock.values().map(|c| c.entries.len()).sum();
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
        Err(e) => {
            return serde_json::to_string(&RespErr {
                ok: false,
                error: e.to_string(),
            })
            .unwrap();
        }
    };

    let root_path = Path::new(project_root);
    let root_key = store::project_id(root_path);
    let db_mtime = store::get_db_mtime(root_path);

    let mut cache_lock = state.cache.lock().unwrap();
    let needs_reload = cache_lock
        .get(&root_key)
        .map(|c| (db_mtime - c.db_mtime).abs() > 0.5)
        .unwrap_or(true);

    if needs_reload {
        match store::open_db(root_path, false) {
            Ok(Some(conn)) => match ProjectCache::load(&conn, db_mtime) {
                Ok(c) => {
                    eprintln!(
                        "[tokenix] cache loaded: {} chunks for {}",
                        c.entries.len(),
                        root_key
                    );
                    cache_lock.insert(root_key.clone(), c);
                }
                Err(e) => {
                    return serde_json::to_string(&RespErr {
                        ok: false,
                        error: format!("cache load: {e}"),
                    })
                    .unwrap();
                }
            },
            _ => {
                return serde_json::to_string(&RespErr {
                    ok: false,
                    error: "db not found".into(),
                })
                .unwrap();
            }
        }
    }

    let mut results = cache_lock[&root_key].search(&query_vec, k);
    drop(cache_lock);

    // Apply token budget (same logic as query.rs::query_index).
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

    let output = format_results(&results, query);
    serde_json::to_string(&RespOk { ok: true, output }).unwrap()
}
