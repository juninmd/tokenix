use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
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
    let (blocks_a, rem_a) = a.as_chunks::<8>();
    let (blocks_b, rem_b) = b.as_chunks::<8>();
    let mut sum: f32 = blocks_a
        .iter()
        .zip(blocks_b.iter())
        .map(|(ca, cb)| ca.iter().zip(cb.iter()).map(|(x, y)| x * y).sum::<f32>())
        .sum();
    // handle remainder (768 % 8 == 0, so this is a no-op for nomic-embed-text-v1.5)
    sum += rem_a
        .iter()
        .zip(rem_b.iter())
        .map(|(x, y)| x * y)
        .sum::<f32>();
    sum
}

/// Dot product of an f32 query against an int8-quantized document vector.
/// The i8→f32 widening vectorizes; cache traffic is 4x lower than f32×f32.
#[inline]
fn dot_product_q8(a: &[f32], b: &[i8]) -> f32 {
    let (blocks_a, rem_a) = a.as_chunks::<8>();
    let (blocks_b, rem_b) = b.as_chunks::<8>();
    let mut sum: f32 = blocks_a
        .iter()
        .zip(blocks_b.iter())
        .map(|(ca, cb)| {
            ca.iter()
                .zip(cb.iter())
                .map(|(x, y)| x * *y as f32)
                .sum::<f32>()
        })
        .sum();
    sum += rem_a
        .iter()
        .zip(rem_b.iter())
        .map(|(x, y)| x * *y as f32)
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
    embedding: Vec<i8>,
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
                let norm: f32 = e
                    .embedding_q8
                    .iter()
                    .map(|&x| (x as f32) * (x as f32))
                    .sum::<f32>()
                    .sqrt();
                CachedEntry {
                    id: e.id,
                    path: e.path,
                    start_line: e.start_line,
                    end_line: e.end_line,
                    symbol: e.symbol,
                    kind: e.kind,
                    token_count: e.token_count,
                    embedding: e.embedding_q8,
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
            let dot = dot_product_q8(query, &e.embedding);
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
    /// `RwLock`, not `Mutex`: a search holds this for the whole O(N) cosine scan
    /// over a project's embeddings, which under a mutex blocked every other
    /// client — including `health`/`status` — for the duration of the heaviest
    /// query. Reads now run concurrently; only a cache reload takes the writer.
    cache: RwLock<CacheState>,
    started: std::time::Instant,
    port: u16,
    /// Capability token every `Search` must present. Regenerated per daemon
    /// start, so a stale token from a previous generation is rejected.
    token: String,
}

// ---- Protocol ---------------------------------------------------------------

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Request {
    Search {
        /// Capability token from `~/.tokenix/daemon.token`. Required: `Search`
        /// returns indexed source for an arbitrary `project_root`.
        #[serde(default)]
        token: String,
        project_root: String,
        query: String,
        #[serde(default = "default_k")]
        k: usize,
        #[serde(default = "default_budget")]
        budget: usize,
        file: Option<String>,
    },
    Health,
    Status,
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

#[derive(Serialize, Deserialize)]
pub struct DaemonStatus {
    pub ok: bool,
    pub pid: u32,
    pub port: u16,
    pub uptime_secs: u64,
    pub cached_projects: usize,
    pub chunks: usize,
    /// Estimated bytes held by cached embedding vectors (f32) across projects.
    pub cache_bytes: u64,
    pub model: String,
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

fn token_path() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".tokenix").join("daemon.token"))
}

/// A 32-hex-char secret shared between this user's tokenix processes.
///
/// `127.0.0.1` is not an access control: on a shared host (build box, CI runner,
/// jump host) every local account can reach the port, and `Search` returns
/// indexed *source code* for any `project_root` the caller names. The token file
/// is created `0600`, so possession of it means "runs as the user who owns the
/// index" — which is exactly the intended audience.
///
/// Not a cryptographic protocol: it is a capability file, so unpredictability is
/// all that is required of the value.
fn generate_token() -> String {
    use sha2::{Digest, Sha256};
    // /dev/urandom where it exists; otherwise mix sources an unrelated local
    // process cannot observe or replay (nanosecond clock, pid, two live
    // addresses) and hash them.
    //
    // Read exactly 16 bytes: `/dev/urandom` is an endless stream, so a
    // read-to-EOF helper never returns.
    #[cfg(unix)]
    {
        use std::io::Read;
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            let mut buf = [0u8; 16];
            if f.read_exact(&mut buf).is_ok() {
                return hex::encode(buf);
            }
        }
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let stack = &nanos as *const u128 as usize;
    let heap = Box::new(0u8);
    let heap_addr = &*heap as *const u8 as usize;
    let digest = Sha256::digest(
        format!("{nanos}:{}:{stack:x}:{heap_addr:x}", std::process::id()).as_bytes(),
    );
    hex::encode(&digest[..16])
}

/// Write a fresh token for this daemon generation and return it. A failure to
/// persist is fatal for the token, not for the daemon: `None` means clients
/// cannot authenticate, so `run_serve` refuses to start rather than silently
/// serving an unauthenticated socket.
fn write_token() -> Option<String> {
    let path = token_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok()?;
        crate::store::restrict_to_owner(parent);
    }
    let token = generate_token();
    std::fs::write(&path, &token).ok()?;
    crate::store::restrict_to_owner(&path);
    Some(token)
}

/// Read the running daemon's token. `None` when no daemon has started yet.
fn read_token() -> Option<String> {
    let raw = std::fs::read_to_string(token_path()?).ok()?;
    let token = raw.trim().to_string();
    (!token.is_empty()).then_some(token)
}

/// Constant-time-ish comparison. The token is a local capability rather than a
/// network secret, but comparing lengths first and folding every byte avoids
/// leaking a prefix through timing to another local process.
fn token_matches(expected: &str, got: &str) -> bool {
    if expected.len() != got.len() {
        return false;
    }
    expected
        .bytes()
        .zip(got.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
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

    // Refuse to serve without a token rather than fall back to an open socket:
    // an unauthenticated daemon exposes indexed source to every local account,
    // and a silent downgrade is the kind of failure nobody notices.
    let token = write_token()
        .ok_or_else(|| anyhow!("could not write ~/.tokenix/daemon.token; refusing to serve"))?;

    let state = Arc::new(DaemonState {
        cache: RwLock::new(CacheState::new()),
        started: std::time::Instant::now(),
        port,
        token,
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

/// Query the running daemon's status over TCP. None = not reachable.
fn fetch_status() -> Option<DaemonStatus> {
    let port = daemon_port();
    let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().ok()?;
    let mut stream =
        TcpStream::connect_timeout(&addr, Duration::from_millis(CONNECT_TIMEOUT_MS)).ok()?;
    stream.set_nodelay(true).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(READ_TIMEOUT_MS)))
        .ok()?;
    stream.write_all(b"{\"type\":\"status\"}\n").ok()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    serde_json::from_str::<DaemonStatus>(line.trim())
        .ok()
        .filter(|s| s.ok)
}

pub fn run_status() -> Result<()> {
    match fetch_status() {
        Some(s) => {
            println!("daemon: running");
            println!("  pid: {}", s.pid);
            println!("  port: {}", s.port);
            println!("  uptime: {}s", s.uptime_secs);
            println!("  model: {}", s.model);
            println!(
                "  cache: {} project(s), {} chunks, ~{:.1} MB embeddings",
                s.cached_projects,
                s.chunks,
                s.cache_bytes as f64 / 1_048_576.0
            );
        }
        None => {
            // Distinguish "not running" from "stale pid file left behind".
            let stale_pid = pid_path()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .and_then(|s| s.trim().parse::<u32>().ok())
                .filter(|pid| !is_process_alive(*pid));
            if stale_pid.is_some() {
                println!("daemon: not running (stale pid file; will be replaced on next start)");
            } else {
                println!(
                    "daemon: not running (auto-starts on first query, or run `tokenix serve`)"
                );
            }
        }
    }
    Ok(())
}

pub fn run_restart() -> Result<()> {
    if fetch_status().is_some() {
        run_stop()?;
        // Give the OS a moment to release the port before respawning.
        std::thread::sleep(Duration::from_millis(300));
    }
    if !spawn_daemon() {
        anyhow::bail!("failed to spawn daemon");
    }
    std::thread::sleep(Duration::from_millis(800));
    match fetch_status() {
        Some(s) => println!("daemon restarted (pid {}, port {})", s.pid, s.port),
        None => println!("daemon spawned; still warming up (check `tokenix daemon status`)"),
    }
    Ok(())
}

pub fn run_stop() -> Result<()> {
    let pid_str = pid_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .ok_or_else(|| anyhow!("daemon not running (no pid file)"))?;
    let pid: u32 = pid_str.trim().parse()?;

    // Never force-kill a pid we have not confirmed is tokenix: the pid file
    // outlives a crash, and the OS recycles pids.
    if !is_tokenix_process(pid) {
        if let Some(p) = pid_path() {
            let _ = std::fs::remove_file(p);
        }
        if let Some(p) = port_path() {
            let _ = std::fs::remove_file(p);
        }
        println!("daemon not running (stale pid file for {pid} removed)");
        return Ok(());
    }

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
    // Drop the capability with the daemon it belonged to.
    if let Some(p) = token_path() {
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
        // Empty when no daemon has ever started; the server rejects it, and the
        // caller falls back to in-process embedding exactly as it does when the
        // daemon is unreachable.
        "token": read_token().unwrap_or_default(),
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
    let mut spawn_lock: Option<PathBuf> = None;
    // Prevent race: if PID file exists and it really is our daemon, skip spawn.
    if let Some(pid_file) = pid_path() {
        if let Ok(s) = std::fs::read_to_string(&pid_file) {
            if let Ok(pid) = s.trim().parse::<u32>() {
                if is_tokenix_process(pid) {
                    return true;
                }
            }
        }
        // Spawn lock, acquired atomically: `exists()` then `write()` let two
        // hooks both see no lock and both spawn, the second dying on
        // EADDRINUSE after a ~130 MB process start.
        let lock = pid_file.with_extension("spawning");
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
        {
            Ok(mut f) => {
                use std::io::Write;
                let _ = f.write_all(std::process::id().to_string().as_bytes());
                spawn_lock = Some(lock);
            }
            Err(_) => {
                let stale = std::fs::metadata(&lock)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.elapsed().ok())
                    .map(|e| e.as_secs() >= 10)
                    .unwrap_or(true);
                if !stale {
                    return true; // another hook is already spawning
                }
                // Stale lock from a crashed spawn: take it over.
                let _ = std::fs::remove_file(&lock);
                let _ = std::fs::write(&lock, std::process::id().to_string());
                spawn_lock = Some(lock);
            }
        }
    }
    // Always release the lock — leaving it behind blocked every other hook from
    // spawning for the full staleness window after a failed attempt.
    let _release = SpawnLockGuard(spawn_lock);

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

struct SpawnLockGuard(Option<PathBuf>);

impl Drop for SpawnLockGuard {
    fn drop(&mut self) {
        if let Some(p) = &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// True when `pid` is alive AND is a tokenix process. The pid file survives a
/// crash, so trusting it alone meant `tokenix stop` could `kill -9` whatever
/// unrelated process later inherited that pid, and `spawn_daemon` would treat
/// it as a healthy daemon and never respawn.
fn is_tokenix_process(pid: u32) -> bool {
    if !is_process_alive(pid) {
        return false;
    }
    #[cfg(windows)]
    {
        let out = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .output();
        match out {
            Ok(o) => String::from_utf8_lossy(&o.stdout)
                .to_ascii_lowercase()
                .contains("tokenix"),
            Err(_) => false,
        }
    }
    #[cfg(unix)]
    {
        if let Ok(exe) = std::fs::read_link(format!("/proc/{pid}/exe")) {
            return exe
                .file_name()
                .map(|n| n.to_string_lossy().contains("tokenix"))
                .unwrap_or(false);
        }
        // macOS and other non-procfs systems.
        std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "comm="])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("tokenix"))
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    #[test]
    fn own_process_is_recognized_as_tokenix() {
        // The test binary is `tokenix-<hash>.exe`, which is what the daemon's
        // pid-identity check must accept; a recycled unrelated pid must not be.
        assert!(is_tokenix_process(std::process::id()));
    }
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
    let reader = BufReader::new(stream);
    // Bound the request: `read_line` on a raw socket will happily buffer until
    // the peer stops sending, so a single client could exhaust memory.
    const MAX_REQUEST_BYTES: u64 = 1024 * 1024;
    let mut line = String::new();
    let mut reader = reader.take(MAX_REQUEST_BYTES);
    reader.read_line(&mut line)?;

    let response_str = match serde_json::from_str::<Request>(line.trim()) {
        Ok(Request::Health) => {
            let lock = state.cache.read().unwrap();
            let cached_projects = lock.projects.len();
            let chunks = lock.projects.values().map(|c| c.entries.len()).sum();
            serde_json::to_string(&RespHealth {
                ok: true,
                cached_projects,
                chunks,
            })?
        }
        Ok(Request::Status) => {
            let lock = state.cache.read().unwrap();
            let cached_projects = lock.projects.len();
            let chunks: usize = lock.projects.values().map(|c| c.entries.len()).sum();
            let cache_bytes: u64 = lock
                .projects
                .values()
                .flat_map(|c| c.entries.iter())
                .map(|e| e.embedding.len() as u64)
                .sum();
            serde_json::to_string(&DaemonStatus {
                ok: true,
                pid: std::process::id(),
                port: state.port,
                uptime_secs: state.started.elapsed().as_secs(),
                cached_projects,
                chunks,
                cache_bytes,
                model: crate::embed::active_model_id(),
            })?
        }
        Ok(Request::Search {
            token,
            project_root,
            query,
            k,
            budget,
            file,
        }) if !token_matches(&state.token, &token) => {
            let _ = (project_root, query, k, budget, file);
            serde_json::to_string(&RespErr {
                ok: false,
                error: "unauthorized: missing or stale token (see ~/.tokenix/daemon.token)"
                    .to_string(),
            })?
        }
        Ok(Request::Search {
            project_root,
            query,
            k,
            budget,
            file,
            ..
        }) => {
            // Clamp client-supplied sizing: `k` drives the FTS/cosine scan and
            // `budget` the formatting, so unbounded values let any local process
            // pin the daemon's CPU and memory.
            const MAX_K: usize = 500;
            const MAX_BUDGET: usize = 200_000;
            search_handler(
                &state,
                &project_root,
                &query,
                k.clamp(1, MAX_K),
                budget.min(MAX_BUDGET),
                file.as_deref(),
            )
        }
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
    // Use the model the project's index was built with (this handler thread only).
    if let Some(model_id) = store::index_model_id(Path::new(project_root)) {
        crate::embed::set_active_model(&model_id);
    }
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
    let sparse_results =
        store::search_fts(&conn, query, sparse_limit, file_filter).unwrap_or_default();

    // Reload under the writer only when actually stale; the scan itself runs
    // under a shared read lock so concurrent searches do not serialize.
    let needs_reload = {
        let r = state.cache.read().unwrap();
        r.projects
            .get(&root_key)
            .map(|c| db_mtime != c.db_mtime)
            .unwrap_or(true)
    };
    if needs_reload {
        // Load outside the lock — this reads the entire embeddings table.
        match ProjectCache::load(&conn, db_mtime) {
            Ok(c) => {
                eprintln!(
                    "[tokenix] cache loaded: {} chunks for {}",
                    c.entries.len(),
                    root_key
                );
                state.cache.write().unwrap().insert(root_key.clone(), c);
            }
            Err(e) => return err_json(format!("cache load: {e}")),
        }
    } else {
        state.cache.write().unwrap().touch(&root_key);
    }

    let top_ids: Vec<(usize, f32, i64)> = {
        let cache_lock = state.cache.read().unwrap();
        // A concurrent eviction can drop the project between the reload above
        // and this read; fall back to an empty result rather than panicking on
        // the index.
        let Some(pc) = cache_lock.projects.get(&root_key) else {
            return err_json("cache evicted mid-request; retry".into());
        };
        let candidate_k = (k.saturating_mul(5)).max(50);
        let dense_results = pc.search_ids(&query_vec, candidate_k, file_filter);

        let mut rrf_scores: HashMap<i64, f32> = HashMap::new();
        let mut dense_map: HashMap<i64, (usize, f32)> = HashMap::new();

        for (rank, &(idx, sim)) in dense_results.iter().enumerate() {
            let id = pc.entries[idx].id;
            dense_map.insert(id, (idx, sim));
            let score = 1.0 / (store::RRF_K + rank as f32);
            rrf_scores.insert(id, score);
        }

        for (rank, (id, bm25_score)) in sparse_results.iter().enumerate() {
            let rrf_position = 1.0 / (store::RRF_K + rank as f32);
            let bm25_normalized = (*bm25_score).max(0.0) / (1.0 + bm25_score.max(0.0));
            let score = rrf_position + store::BM25_WEIGHT * bm25_normalized;
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

    // Writer: this path memoizes fetched chunk content back into the cache.
    let mut cache_lock = state.cache.write().unwrap();
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
    fn generated_tokens_are_hex_and_unpredictable() {
        let a = generate_token();
        let b = generate_token();
        assert_eq!(a.len(), 32, "{a}");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()), "{a}");
        assert_ne!(a, b, "two daemons must not share a capability");
    }

    #[test]
    fn token_comparison_rejects_wrong_and_truncated_values() {
        let expected = "a".repeat(32);
        assert!(token_matches(&expected, &expected));
        assert!(!token_matches(&expected, ""), "empty must never pass");
        assert!(
            !token_matches(&expected, &"a".repeat(31)),
            "a prefix must never pass"
        );
        assert!(!token_matches(&expected, &"b".repeat(32)));
    }

    #[test]
    fn search_without_the_token_is_rejected() {
        // The wire contract: `token` defaults to empty when absent, so an older
        // client (or any other local process) lands on the unauthorized arm
        // rather than reaching `search_handler`.
        let req: Request =
            serde_json::from_str(r#"{"type":"search","project_root":"/repo","query":"anything"}"#)
                .expect("parses without a token field");
        let Request::Search { token, .. } = req else {
            panic!("expected a search request");
        };
        assert!(token.is_empty());
        assert!(!token_matches(&"a".repeat(32), &token));
    }

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
