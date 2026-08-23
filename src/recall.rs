//! Content-addressed output stash + cross-call deduplication.
//!
//! Two problems this solves, both measured in real agent histories:
//!
//! 1. **Compression is one-way.** A filter or cap can drop the one line the
//!    agent needed, and its only recovery is re-running the command — which
//!    pays the raw cost twice. The failure tee covers *failing* commands only;
//!    this stash makes any compressed output recoverable via `tokenix retrieve`.
//! 2. **Agents re-run the same command.** `git status`, `cargo check`, `kubectl
//!    get pods` repeat across a session with byte-identical output, and every
//!    repeat is billed again. When the output is unchanged, a one-line marker
//!    referring back to the earlier call is enough.
//!
//! Deliberately exact-match only (FNV-1a over the compressed bytes). Fuzzy
//! near-duplicate matching would need a similarity threshold, and a wrong
//! collapse silently hides changed output — the expensive failure mode here.

use std::path::PathBuf;

/// Newest N remembered outputs kept in the index.
const RECENT_CAP: usize = 24;
/// Blobs retained on disk (oldest pruned first).
const BLOB_CAP: usize = 60;
/// Below this the marker would not pay for itself.
const DEFAULT_DEDUP_MIN_TOKENS: usize = 200;
/// How long a remembered command output stays authoritative. Long enough to
/// cover a working session's repeated `git status` / `cargo check`, short enough
/// that it cannot point at output from a previous day's session.
const DEFAULT_DEDUP_TTL_SECS: f64 = 3600.0;

fn dedup_enabled() -> bool {
    !std::env::var("TOKENIX_DEDUP").is_ok_and(|v| v == "0")
}

fn dedup_ttl_secs() -> f64 {
    std::env::var("TOKENIX_DEDUP_TTL")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .unwrap_or(DEFAULT_DEDUP_TTL_SECS)
}

fn dedup_min_tokens() -> usize {
    std::env::var("TOKENIX_DEDUP_MIN_TOKENS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(DEFAULT_DEDUP_MIN_TOKENS)
}

fn base_dir() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".tokenix"))
}

fn blob_dir() -> Option<PathBuf> {
    Some(base_dir()?.join("blobs"))
}

fn index_path() -> Option<PathBuf> {
    Some(base_dir()?.join("recent_outputs.json"))
}

/// FNV-1a: no dependency, stable across runs, and collision risk is irrelevant
/// here because a hit is verified against the stored blob before it is used.
pub fn digest(s: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    format!("{hash:016x}")
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct RecentOutput {
    /// Digest of the *compressed* output — what a later call is matched against.
    pub key: String,
    /// Digest of the *raw* output — what `tokenix retrieve` should hand back,
    /// since the point of recovery is seeing what compression dropped.
    #[serde(default)]
    pub raw_key: String,
    pub command: String,
    pub ts: f64,
    pub tokens: usize,
    /// Project this output came from. Empty on entries written by an older
    /// version, which therefore never match a scoped lookup — correct, since
    /// their origin is unknown.
    #[serde(default)]
    pub project: String,
}

fn load_index() -> Vec<RecentOutput> {
    let Some(path) = index_path() else {
        return Vec::new();
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save_index(entries: &[RecentOutput]) {
    let Some(path) = index_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(raw) = serde_json::to_string(entries) {
        let _ = std::fs::write(&path, raw);
        crate::store::restrict_to_owner(&path);
    }
}

/// Persist `content` under its digest and return the key. Existing blobs are
/// left untouched (same content ⇒ same key ⇒ nothing to rewrite).
/// Blobs are stored verbatim, unlike the hook log and the failure tee: `retrieve`
/// promises the *exact* original bytes, and `find_identical` verifies a candidate
/// against the stored blob before collapsing anything — redacting here would
/// break both. Confidentiality is handled with file permissions instead.
pub fn stash(content: &str) -> Option<String> {
    let key = digest(content);
    let dir = blob_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    crate::store::restrict_to_owner(&dir);
    let path = dir.join(format!("{key}.txt"));
    if !path.exists() {
        std::fs::write(&path, content).ok()?;
        crate::store::restrict_to_owner(&path);
        prune_blobs(&dir);
    }
    Some(key)
}

/// Read back a stashed output. `None` when the key is unknown or was pruned.
pub fn retrieve(key: &str) -> Option<String> {
    // Reject path separators so a key can never escape the blob directory.
    if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    std::fs::read_to_string(blob_dir()?.join(format!("{key}.txt"))).ok()
}

fn prune_blobs(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut blobs: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let modified = e.metadata().ok()?.modified().ok()?;
            Some((modified, path))
        })
        .collect();
    if blobs.len() <= BLOB_CAP {
        return;
    }
    blobs.sort_by_key(|(t, _)| *t);
    for (_, old) in &blobs[..blobs.len() - BLOB_CAP] {
        let _ = std::fs::remove_file(old);
    }
}

/// Record that `command` produced `compressed` (from `raw`), so a later
/// identical run can be collapsed and the raw text stays recoverable.
///
/// Returns the key for the **raw** blob — the one a recovery hint must advertise,
/// since the point of recovery is seeing what compression dropped.
pub fn remember(
    project: &str,
    command: &str,
    compressed: &str,
    raw: &str,
    tokens: usize,
    ts: f64,
) -> Option<String> {
    let key = stash(compressed)?;
    let raw_key = stash(raw).unwrap_or_else(|| key.clone());
    let mut index = load_index();
    index.retain(|e| e.key != key);
    index.insert(
        0,
        RecentOutput {
            key: key.clone(),
            raw_key: raw_key.clone(),
            command: command.chars().take(120).collect(),
            ts,
            tokens,
            project: project.to_string(),
        },
    );
    index.truncate(RECENT_CAP);
    save_index(&index);
    Some(raw_key)
}

/// May this remembered output stand in for a fresh run right now?
///
/// Kept separate from the disk lookup so the rule is testable without touching
/// `~/.tokenix`. Two conditions, both required by what the marker asserts —
/// "identical to an earlier run, unchanged": the entry must come from the same
/// checkout, and it must be recent enough that the earlier copy is plausibly
/// still in the conversation.
fn reusable(entry: &RecentOutput, project: &str, now: f64) -> bool {
    entry.project == project && now - entry.ts <= dedup_ttl_secs()
}

/// Look for an earlier call whose output was byte-identical to `content`.
/// The hit is verified against the stored blob, so a hash collision or a pruned
/// blob degrades to "no match" instead of a wrong collapse.
///
/// Scoped to `project` and to `dedup_ttl_secs()` because the marker asserts the
/// output "is already in this conversation". The index lives in `~/.tokenix` and
/// outlives both the session and the repository: without these two filters a
/// `git status` from another checkout, or from yesterday, could be collapsed into
/// a pointer to output the agent had never seen.
pub fn find_identical(
    project: &str,
    content: &str,
    tokens: usize,
    now: f64,
) -> Option<RecentOutput> {
    if !dedup_enabled() || tokens < dedup_min_tokens() {
        return None;
    }
    let key = digest(content);
    let mut hit = load_index()
        .into_iter()
        .find(|e| e.key == key && reusable(e, project, now))?;
    if retrieve(&hit.key).as_deref() != Some(content) {
        return None;
    }
    // The marker prefers `raw_key` (the pre-compression blob), but that blob is
    // pruned independently of this one. Advertising a key `tokenix retrieve`
    // cannot resolve sends the agent to a dead end, so fall back to the verified
    // key when the raw blob is gone.
    if !hit.raw_key.is_empty() && retrieve(&hit.raw_key).is_none() {
        hit.raw_key = String::new();
    }
    Some(hit)
}

/// The line that replaces a repeated output. Carries what the agent needs to
/// decide: which earlier command it matched, how long ago, and how to get the
/// text back without re-running anything.
pub fn dedup_marker(hit: &RecentOutput, now: f64) -> String {
    let age = (now - hit.ts).max(0.0) as u64;
    let ago = if age < 90 {
        format!("{age}s ago")
    } else if age < 5400 {
        format!("{}m ago", age / 60)
    } else {
        format!("{}h ago", age / 3600)
    };
    let key = if hit.raw_key.is_empty() {
        &hit.key
    } else {
        &hit.raw_key
    };
    format!(
        "[tokenix: output identical to `{}` from {} ({} tokens). Unchanged — run `tokenix retrieve {}` for the full text.]",
        hit.command, ago, hit.tokens, key
    )
}

// ---------------------------------------------------------------------------
// Re-read suppression
// ---------------------------------------------------------------------------
//
// A 400-line file costs its full price on every Read. Agents re-read the same
// unchanged file several times per session (competitors: read-once, semantic
// cache MCP). When the content digest is unchanged since the last read, the
// second read can be answered with a pointer instead of the file.
//
// The risk is context compaction: the earlier copy may no longer be in the
// window. Two guards make that cheap rather than harmful — a short TTL, and a
// stash key that returns the exact bytes via `tokenix retrieve`.

/// How long a remembered read stays authoritative.
const DEFAULT_READ_TTL_SECS: f64 = 900.0;
/// Below this, resending the file is cheaper than the marker + recovery risk.
const DEFAULT_READ_MIN_TOKENS: usize = 1_500;

fn read_dedup_enabled() -> bool {
    !std::env::var("TOKENIX_READ_DEDUP").is_ok_and(|v| v == "0")
}

fn read_ttl_secs() -> f64 {
    std::env::var("TOKENIX_READ_DEDUP_TTL")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .unwrap_or(DEFAULT_READ_TTL_SECS)
}

fn read_min_tokens() -> usize {
    std::env::var("TOKENIX_READ_DEDUP_MIN_TOKENS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(DEFAULT_READ_MIN_TOKENS)
}

fn reads_path() -> Option<PathBuf> {
    Some(base_dir()?.join("recent_reads.json"))
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct RecentRead {
    pub path: String,
    /// Digest of the file content as of that read — an edit invalidates it.
    pub content_key: String,
    pub ts: f64,
    pub tokens: usize,
}

fn load_reads() -> Vec<RecentRead> {
    let Some(path) = reads_path() else {
        return Vec::new();
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save_reads(entries: &[RecentRead]) {
    let Some(path) = reads_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(raw) = serde_json::to_string(entries) {
        let _ = std::fs::write(path, raw);
    }
}

/// Record that `path` was read with this exact content.
pub fn remember_read(path: &str, content: &str, tokens: usize, ts: f64) {
    let Some(content_key) = stash(content) else {
        return;
    };
    let mut reads = load_reads();
    reads.retain(|r| r.path != path);
    reads.insert(
        0,
        RecentRead {
            path: path.to_string(),
            content_key,
            ts,
            tokens,
        },
    );
    reads.truncate(RECENT_CAP);
    save_reads(&reads);
}

/// Was this exact file content already delivered recently? Returns the entry
/// whose stash key recovers the bytes.
pub fn find_recent_read(path: &str, content: &str, tokens: usize, now: f64) -> Option<RecentRead> {
    if !read_dedup_enabled() || tokens < read_min_tokens() {
        return None;
    }
    let key = digest(content);
    let hit = load_reads()
        .into_iter()
        .find(|r| r.path == path && r.content_key == key)?;
    if now - hit.ts > read_ttl_secs() {
        return None;
    }
    // Verify against the stash: a pruned blob means recovery is impossible, so
    // suppressing the read would be a one-way loss.
    if retrieve(&hit.content_key).as_deref() != Some(content) {
        return None;
    }
    Some(hit)
}

/// The message that replaces a redundant read.
pub fn read_marker(hit: &RecentRead, now: f64) -> String {
    let mins = ((now - hit.ts).max(0.0) / 60.0).round() as u64;
    format!(
        "[tokenix] `{}` is unchanged since you read it {}m ago ({} tokens) — it is already in this conversation.\n\
         If you no longer have it: `tokenix retrieve {}` returns the exact bytes, or Read with offset/limit for a slice.",
        hit.path, mins, hit.tokens, hit.content_key
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_marker_points_at_recovery() {
        let hit = RecentRead {
            path: "src/main.rs".to_string(),
            content_key: "deadbeef".to_string(),
            ts: 0.0,
            tokens: 3000,
        };
        let marker = read_marker(&hit, 300.0);
        assert!(marker.contains("src/main.rs"));
        assert!(marker.contains("5m ago"));
        assert!(marker.contains("tokenix retrieve deadbeef"));
        assert!(marker.contains("offset/limit"));
    }

    #[test]
    fn small_reads_are_never_suppressed() {
        assert!(find_recent_read("x.rs", "fn main() {}", 5, 0.0).is_none());
    }

    #[test]
    fn digest_is_stable_and_content_sensitive() {
        assert_eq!(digest("hello"), digest("hello"));
        assert_ne!(digest("hello"), digest("hellp"));
        assert_eq!(digest("hello").len(), 16);
    }

    #[test]
    fn retrieve_rejects_path_traversal_keys() {
        // Keys come back through a marker the model may echo — never let one
        // address a file outside the blob directory.
        assert!(retrieve("../../etc/passwd").is_none());
        assert!(retrieve("a/b").is_none());
        assert!(retrieve("").is_none());
    }

    #[test]
    fn dedup_marker_mentions_command_key_and_recovery() {
        let hit = RecentOutput {
            key: "compressedkey".to_string(),
            raw_key: "abc123".to_string(),
            command: "git status".to_string(),
            ts: 1000.0,
            tokens: 420,
            project: "/repo".to_string(),
        };
        let marker = dedup_marker(&hit, 1120.0);
        assert!(marker.contains("git status"));
        assert!(marker.contains("2m ago"));
        assert!(marker.contains("tokenix retrieve abc123"));
    }

    #[test]
    fn dedup_marker_uses_seconds_then_minutes_then_hours() {
        let hit = RecentOutput {
            key: "k".to_string(),
            raw_key: "k".to_string(),
            command: "c".to_string(),
            ts: 0.0,
            tokens: 1,
            project: "/repo".to_string(),
        };
        assert!(dedup_marker(&hit, 30.0).contains("30s ago"));
        assert!(dedup_marker(&hit, 600.0).contains("10m ago"));
        assert!(dedup_marker(&hit, 7200.0).contains("2h ago"));
    }

    #[test]
    fn short_output_is_never_deduped() {
        // Below the threshold the marker would cost more than the output.
        assert!(find_identical("/repo", "tiny", 3, 0.0).is_none());
    }

    #[test]
    fn dedup_is_scoped_to_one_project_and_expires() {
        // Both guards exist because the marker claims the output is already in
        // this conversation. Neither a different checkout nor a run from an
        // earlier session can honour that claim.
        let entry = RecentOutput {
            key: "k".to_string(),
            raw_key: "k".to_string(),
            command: "git status".to_string(),
            ts: 1_000.0,
            tokens: 900,
            project: "/repo-a".to_string(),
        };
        let ttl = dedup_ttl_secs();

        assert!(reusable(&entry, "/repo-a", 1_000.0 + ttl / 2.0));
        assert!(
            !reusable(&entry, "/repo-b", 1_000.0),
            "another checkout must not reuse this entry"
        );
        assert!(
            !reusable(&entry, "/repo-a", 1_000.0 + ttl + 1.0),
            "past the TTL the earlier output is no longer assumed to be in context"
        );
    }

    #[test]
    fn entries_written_before_project_scoping_never_match() {
        // `project` deserializes to "" for an index written by an older build.
        // Those entries have unknown origin, so they must not be reused.
        let legacy = RecentOutput {
            key: "k".to_string(),
            raw_key: "k".to_string(),
            command: "git status".to_string(),
            ts: 1_000.0,
            tokens: 900,
            project: String::new(),
        };
        assert!(!reusable(&legacy, "/repo-a", 1_000.0));
    }
}
