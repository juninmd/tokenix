use std::collections::HashMap;
use std::path::PathBuf;

use regex::Regex;
use rust_embed::Embed;
use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct MatchOutput {
    pub pattern: String,
    pub message: String,
    /// Guard: when set, the short-circuit to `message` is skipped if the output
    /// also matches this regex. Prevents masking errors/warnings that appear
    /// alongside a success marker (e.g. "total size is" present, but so is "error").
    #[serde(default)]
    pub unless: Option<String>,
}

/// See `FilterDef::uniform_success`.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct UniformSuccess {
    /// The per-item pass shape every significant line must match.
    pub pattern: String,
    /// Summary to emit. `{n}` is replaced with the number of matched items,
    /// so the agent gets a real count instead of a bare "all clear".
    pub message: String,
    /// Lines to ignore entirely when deciding uniformity (banners, totals).
    #[serde(default)]
    pub ignore_lines: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct FilterDef {
    #[allow(dead_code)]
    pub description: Option<String>,
    pub match_command: String,
    /// When set, this filter is skipped (candidate falls through to the next
    /// filter or raw passthrough) if the matched candidate ALSO matches this
    /// regex. Use for commands where certain flags mean the agent already
    /// bounded its own output (e.g. `Get-Content -Tail`) and further
    /// compression would be redundant or could clip output the agent
    /// explicitly asked for. Only sees the matched segment/candidate text,
    /// not cross-segment context (e.g. a stripped-off pipe target).
    #[serde(default)]
    pub skip_when_matches: Option<String>,
    /// When true, a `max_lines`/`head_lines`/`tail_lines`/`truncate_lines_at`
    /// cut that actually drops content appends a `[... N ... ]` marker, so
    /// the agent knows it saw a partial view. Off by default — most bundled
    /// filters compress noise the agent doesn't need to know was cut; opt in
    /// per-filter where a silent cut could mislead (e.g. full-file reads).
    #[serde(default)]
    pub notify_on_truncate: bool,
    #[serde(default)]
    pub strip_ansi: bool,
    #[serde(default)]
    pub strip_lines_matching: Vec<String>,
    #[serde(default)]
    pub keep_lines_matching: Vec<String>,
    pub max_lines: Option<usize>,
    pub head_lines: Option<usize>,
    pub tail_lines: Option<usize>,
    pub on_empty: Option<String>,
    /// When the filter reduces *non-empty* command output to nothing — usually an
    /// unexpected output shape the keep/extract rules don't recognize (e.g.
    /// `git log --oneline` against a filter tuned for the full log format) — emit a
    /// bounded, ANSI-stripped view of the real output instead of `on_empty`.
    /// Opt-in so summary filters (cargo test all-pass → "ok") keep their behavior;
    /// only format-specific filters that would otherwise report a *false* "nothing
    /// here" (git-log, git-diff) set it.
    #[serde(default)]
    pub passthrough_when_emptied: bool,
    #[serde(default)]
    pub match_output: Vec<MatchOutput>,
    /// Whole-run success assertion for tools that report **per item**
    /// (`PASS - policy`, `ok 1 - test`, `[200] url`, `file is valid`).
    ///
    /// `match_output` is the wrong tool for those: it fires when *some* line
    /// matches, so a run where item 3 failed would still be reported as
    /// success. This collapses only when EVERY significant line matches the
    /// pass shape and at least one did — a single dissenting line (a failure,
    /// a warning, an unrecognized shape) leaves the normal pipeline in charge.
    /// That makes the summary a claim about the run, not about one line.
    pub uniform_success: Option<UniformSuccess>,
    pub truncate_lines_at: Option<usize>,
    #[serde(default)]
    #[allow(dead_code)]
    pub filter_stderr: bool,

    /// Regex replacement rules: each entry is [pattern, replacement].
    /// Applied after line filtering, before sizing. Enables custom transformations
    /// like shortening paths, normalizing timestamps, etc.
    #[serde(default)]
    pub replace_patterns: Vec<[String; 2]>,

    /// Extract only content between start/end markers (inclusive).
    /// Useful for pulling out specific sections like test failures, error blocks, etc.
    #[serde(default)]
    pub extract_sections: Vec<ExtractSection>,

    /// Semantic filter: keep only lines semantically relevant to a query.
    /// Uses embeddings to score relevance. Requires daemon or in-process embed.
    #[serde(default)]
    pub semantic_filter: Option<SemanticFilterDef>,

    /// Deduplicate similar blocks (not just exact lines).
    /// Groups consecutive blocks by structural similarity.
    #[serde(default)]
    pub deduplicate_blocks: Option<DeduplicateBlocksDef>,

    /// Intelligent JSON summarization beyond simple compaction.
    /// Extracts key fields, summarizes arrays, preserves structure.
    #[serde(default)]
    pub summarize_json: Option<SummarizeJsonDef>,

    /// Hard token budget: truncate intelligently to stay under token limit.
    /// Prioritizes head/tail/errors/semantic relevance.
    pub token_budget: Option<usize>,

    /// Failure policy applied when the run wrapper observed a nonzero exit:
    /// "passthrough" emits the raw output untouched; "tail:N" emits the last
    /// N raw lines. Unset = normal filtering (success sentinels are still
    /// suppressed on failure — see `apply_filter_with_exit`).
    #[serde(default)]
    pub on_failure: Option<String>,

    /// Lines matching any of these regexes survive every sizing cut
    /// (max/head/tail), even beyond the line budget — for verdict lines that
    /// must never be clipped (error summaries, totals).
    #[serde(default)]
    pub priority_lines: Vec<String>,

    /// Per-category caps: keep only the first `max` lines matching `pattern`,
    /// replacing the overflow with a single count marker. Bounds repetitive
    /// classes independently (e.g. 20 errors + 5 warnings) instead of a flat
    /// positional cut that may spend the whole budget on one class.
    #[serde(default)]
    pub category_caps: Vec<CategoryCap>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct CategoryCap {
    pub pattern: String,
    pub max: usize,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ExtractSection {
    pub start_pattern: String,
    pub end_pattern: String,
    #[serde(default)]
    pub include_markers: bool,
    #[serde(default)]
    pub max_matches: Option<usize>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SemanticFilterDef {
    /// Query to score relevance against (e.g., "error", "test failure", "build output")
    pub query: String,
    /// Minimum cosine similarity to keep (0.0-1.0)
    #[serde(default = "default_semantic_threshold")]
    pub threshold: f32,
    /// Always keep lines matching these patterns regardless of score
    #[serde(default)]
    pub always_keep: Vec<String>,
    /// Model to use (defaults to index model)
    pub model: Option<String>,
}

fn default_semantic_threshold() -> f32 {
    0.3
}

#[derive(Debug, Deserialize, Clone)]
pub struct DeduplicateBlocksDef {
    /// Minimum lines per block to consider for deduplication
    #[serde(default = "default_min_block_lines")]
    pub min_block_lines: usize,
    /// Similarity threshold for block comparison (0.0-1.0)
    #[serde(default = "default_block_similarity")]
    pub similarity: f32,
    /// Regex to identify block boundaries (default: blank line)
    #[serde(default)]
    pub block_delimiter: Option<String>,
}

fn default_min_block_lines() -> usize {
    3
}

fn default_block_similarity() -> f32 {
    0.8
}

#[derive(Debug, Deserialize, Clone)]
pub struct SummarizeJsonDef {
    /// Max array elements to show before summarizing
    #[serde(default = "default_max_array_items")]
    pub max_array_items: usize,
    /// Max object depth to traverse
    #[serde(default = "default_max_depth")]
    pub max_depth: usize,
    /// Fields to always include (dot notation for nested)
    #[serde(default)]
    pub always_include: Vec<String>,
    /// Fields to exclude
    #[serde(default)]
    pub exclude: Vec<String>,
}

fn default_max_array_items() -> usize {
    10
}

fn default_max_depth() -> usize {
    3
}

#[derive(Debug, Deserialize)]
struct FilterFile {
    #[serde(default)]
    filters: HashMap<String, FilterDef>,
}

pub struct ActiveFilter {
    pub name: String,
    pub source: &'static str,
    pub filter: FilterDef,
}

#[derive(Embed)]
#[folder = "assets/filters"]
#[include = "*.toml"]
// Rebuild trigger for new filters
struct BundledFilters;

pub fn filters_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tokenix")
        .join("filters")
}

/// Content up to the first `[[tests.…]]` block. Golden cases are ~60% of the
/// bytes in the bundled corpus and are never needed to *use* a filter, so the
/// loader lexes only the head. Returns `None` when there is no test block.
fn filter_defs_head(content: &str) -> Option<&str> {
    let mut offset = 0usize;
    for line in content.split_inclusive('\n') {
        let t = line.trim_start();
        if t.starts_with("[[tests") || t.starts_with("[tests") {
            return Some(&content[..offset]);
        }
        offset += line.len();
    }
    None
}

fn parse_filter_file_named(content: &str) -> Vec<(String, FilterDef)> {
    // Fast path: parse only the definitions. Falls through to the full parse
    // when the head does not parse (a multi-line value containing a `[[tests`
    // line), yields nothing, or — the silent case — declares FEWER filters
    // than the whole file, which means the cut landed before a later
    // `[filters.x]` table and would have dropped it without any error.
    if let Some(head) = filter_defs_head(content) {
        if let Ok(f) = toml::from_str::<FilterFile>(head) {
            if !f.filters.is_empty() {
                return f.filters.into_iter().collect();
            }
        }
    }
    match toml::from_str::<FilterFile>(content) {
        Ok(f) => f.filters.into_iter().collect(),
        Err(e) => {
            // Fail loudly: FilterDef rejects unknown fields, so a typo'd key
            // must surface instead of silently disabling the whole file.
            eprintln!("tokenix: skipping invalid filter file: {e}");
            Vec::new()
        }
    }
}

pub fn load_user_filters() -> Vec<FilterDef> {
    load_user_filters_named()
        .into_iter()
        .map(|(_, f)| f)
        .collect()
}

pub fn load_user_filters_named() -> Vec<(String, FilterDef)> {
    let dir = filters_dir();
    if !dir.exists() {
        return vec![];
    }
    let mut result = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("toml") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    result.extend(parse_filter_file_named(&content));
                }
            }
        }
    }
    result
}

/// Path of the JSON trust store gating repo-local filter files.
pub fn trust_store_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tokenix")
        .join("trusted_filters.json")
}

/// SHA-256 of every `.toml` under `<repo>/.tokenix/filters`, keyed by file name.
pub fn local_filter_hashes(root: &std::path::Path) -> std::collections::BTreeMap<String, String> {
    use sha2::{Digest, Sha256};
    let mut hashes = std::collections::BTreeMap::new();
    let dir = root.join(".tokenix").join("filters");
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("toml") {
                if let (Some(name), Ok(content)) = (
                    path.file_name().and_then(|n| n.to_str()),
                    std::fs::read(&path),
                ) {
                    hashes.insert(name.to_string(), hex::encode(Sha256::digest(&content)));
                }
            }
        }
    }
    hashes
}

fn load_trust_store() -> HashMap<String, std::collections::BTreeMap<String, String>> {
    std::fs::read_to_string(trust_store_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// True when every current repo-local filter file matches the hash recorded
/// by `tokenix trust` for this repo. New, edited, or never-trusted files fail
/// closed: repo-local filters control what the agent gets to see, so a cloned
/// repository must not be able to rewrite command output until a human
/// approves its filters.
pub fn local_filters_trusted(root: &std::path::Path) -> bool {
    let current = local_filter_hashes(root);
    if current.is_empty() {
        return true; // nothing to trust
    }
    let store = load_trust_store();
    match store.get(&root.to_string_lossy().to_string()) {
        Some(trusted) => *trusted == current,
        None => false,
    }
}

/// Record the current repo-local filter hashes as trusted (or remove the
/// entry with `trust = false`). Returns the number of files affected.
pub fn set_local_filters_trust(root: &std::path::Path, trust: bool) -> std::io::Result<usize> {
    let mut store = load_trust_store();
    let key = root.to_string_lossy().to_string();
    let count;
    if trust {
        let current = local_filter_hashes(root);
        count = current.len();
        store.insert(key, current);
    } else {
        count = store.remove(&key).map(|m| m.len()).unwrap_or(0);
    }
    let path = trust_store_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&store).unwrap_or_default(),
    )?;
    Ok(count)
}

/// This repo's `.tokenix/filters` directory.
pub fn local_filters_dir() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    crate::store::find_project_root(&cwd)
        .join(".tokenix")
        .join("filters")
}

pub fn load_local_filters_named() -> Vec<(String, FilterDef)> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = crate::store::find_project_root(&cwd);
    let dir = root.join(".tokenix").join("filters");
    if !dir.exists() {
        return vec![];
    }
    // Trust gate: silently skip untrusted repo-local filters in the hot path
    // (a warning here would leak into every filtered command's stderr).
    // `tokenix trust` reports the pending state interactively.
    if !local_filters_trusted(&root) {
        return vec![];
    }
    let mut result = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("toml") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    result.extend(parse_filter_file_named(&content));
                }
            }
        }
    }
    result
}

pub fn load_local_filters() -> Vec<FilterDef> {
    load_local_filters_named()
        .into_iter()
        .map(|(_, f)| f)
        .collect()
}

pub fn load_bundled_filters() -> Vec<FilterDef> {
    load_bundled_filters_named()
        .into_iter()
        .map(|(_, f)| f)
        .collect()
}

pub fn load_bundled_filters_named() -> Vec<(String, FilterDef)> {
    BundledFilters::iter()
        .filter_map(|name| {
            let file = BundledFilters::get(&name)?;
            let content = std::str::from_utf8(file.data.as_ref()).ok()?;
            Some(parse_filter_file_named(content))
        })
        .flatten()
        .collect()
}

/// Prefilters of every `match_command` in one filter file, read straight off
/// the raw TOML text. `None` means "cannot be prefiltered" (a pattern shape
/// this cheap scan does not recognize) and the file is always parsed.
///
/// The scan only ever produces a *shorter* literal than the parsed pattern
/// would (escapes stop it early), so it can add work but never skip a file
/// that would have matched.
fn scan_bundled_anchors(content: &str) -> Option<Vec<Prefilter>> {
    let mut anchors = Vec::new();
    let mut headers = 0usize;
    for line in content.lines() {
        let t = line.trim_start();
        if t.starts_with("[[tests") || t.starts_with("[tests") {
            break; // golden cases live after the filter definitions
        }
        if t.starts_with("[filters.") {
            headers += 1;
            continue;
        }
        let Some(rest) = t.strip_prefix("match_command") else {
            continue;
        };
        let rest = rest.trim_start().strip_prefix('=')?.trim_start();
        let quote = rest.chars().next()?;
        if quote != '"' && quote != '\'' {
            return None; // multi-line/exotic string: fall back to parsing
        }
        let body = &rest[quote.len_utf8()..];
        if quote == '\'' {
            // Literal string: no escapes to undo.
            anchors.push(prefilter_for(&body[..body.find('\'')?])?);
            continue;
        }
        let end = {
            let mut prev_escape = false;
            let mut found = None;
            for (i, c) in body.char_indices() {
                if prev_escape {
                    prev_escape = false;
                    continue;
                }
                match c {
                    '\\' => prev_escape = true,
                    '"' => {
                        found = Some(i);
                        break;
                    }
                    _ => {}
                }
            }
            found?
        };
        // Basic string: undo the escapes a regex actually uses. Any other
        // escape is left as-is, which can only stop the literal run earlier.
        let unescaped = body[..end].replace("\\\\", "\\").replace("\\\"", "\"");
        anchors.push(prefilter_for(&unescaped)?);
    }
    // Every `[filters.x]` block must have contributed exactly one prefilter,
    // otherwise the scan missed a pattern and the file must be parsed.
    (headers > 0 && anchors.len() == headers).then_some(anchors)
}

fn any_allows(anchors: &Option<Vec<Prefilter>>, candidates: &[String]) -> bool {
    match anchors {
        None => true,
        Some(anchors) => anchors
            .iter()
            .any(|a| candidates.iter().any(|c| a.allows(c))),
    }
}

/// One user filter file as recorded in the on-disk prefilter index. `mtime`
/// and `len` are the validity stamp: any drift rebuilds the whole index.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
struct UserFilterEntry {
    file: String,
    /// Nanoseconds since the epoch — see `user_filter_stats`.
    mtime: u128,
    len: u64,
    /// `None` = pattern shape the scanner cannot prefilter; always parsed.
    prefilters: Option<Vec<Prefilter>>,
}

fn user_filter_index_path(dir: &std::path::Path) -> PathBuf {
    dir.join(".prefilter-index.json")
}

/// `(name, mtime, len)` for every `*.toml` in the user filter dir, sorted by
/// name. One directory scan, no file reads.
///
/// `mtime` is nanoseconds, not seconds: two edits inside the same second that
/// keep the byte length identical (a one-character pattern fix) would leave a
/// second-resolution stamp unchanged, and the filter would silently keep
/// matching by its stale prefilter.
fn user_filter_stats(dir: &std::path::Path) -> Vec<(String, u128, u64)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut stats: Vec<(String, u128, u64)> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("toml") {
                return None;
            }
            let name = path.file_name()?.to_str()?.to_string();
            let meta = e.metadata().ok()?;
            let mtime = meta
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_nanos();
            Some((name, mtime, meta.len()))
        })
        .collect();
    stats.sort();
    stats
}

/// Prefilter index for the user filter dir, rebuilt whenever the directory
/// listing drifts from what was recorded (added/removed/edited file).
///
/// Validity is decided from metadata only — reading every file is exactly the
/// cost this index exists to avoid. Residual limitation: two writes that keep
/// the byte length identical *and* land inside the same filesystem timestamp
/// tick (~15 ms on Windows) are indistinguishable. The blast radius is bounded
/// to a **missed** match — the `FilterDef` itself is always re-parsed from disk
/// for entries the prefilter admits, so a stale entry can cost compression but
/// can never apply the wrong filter to a command.
fn user_filter_index(dir: &std::path::Path) -> Vec<UserFilterEntry> {
    let stats = user_filter_stats(dir);
    let cached: Vec<UserFilterEntry> = std::fs::read_to_string(user_filter_index_path(dir))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let matches_disk = cached.len() == stats.len()
        && cached
            .iter()
            .zip(&stats)
            .all(|(c, (name, mtime, len))| c.file == *name && c.mtime == *mtime && c.len == *len);
    if matches_disk {
        return cached;
    }

    use rayon::prelude::*;
    let rebuilt: Vec<UserFilterEntry> = stats
        .par_iter()
        .map(|(name, mtime, len)| UserFilterEntry {
            file: name.clone(),
            mtime: *mtime,
            len: *len,
            prefilters: std::fs::read_to_string(dir.join(name))
                .ok()
                .and_then(|c| scan_bundled_anchors(&c)),
        })
        .collect();
    if let Ok(json) = serde_json::to_string(&rebuilt) {
        let path = user_filter_index_path(dir);
        // Per-process temp name: parallel hooks otherwise write the SAME temp
        // file concurrently and rename an interleaved, corrupt index into
        // place. (A corrupt index self-heals via the rebuild path, but it
        // makes every hook pay the rebuild until someone wins the race.)
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        // Best effort: a failed write only costs the next run a rebuild.
        if std::fs::write(&tmp, json).is_ok() && std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
    rebuilt
}

/// User filters (`~/.tokenix/filters`) that can match `candidates`.
///
/// A user with hundreds of generated filters otherwise pays ~15 ms of file
/// I/O on every hook call just to discover that one of them matches. The
/// index keeps the per-file prefilters on disk, so a hook only opens the
/// files that can actually match.
fn load_user_filters_matching(candidates: &[String]) -> Vec<FilterDef> {
    let dir = filters_dir();
    if !dir.exists() {
        return vec![];
    }
    use rayon::prelude::*;
    user_filter_index(&dir)
        .par_iter()
        .filter(|e| any_allows(&e.prefilters, candidates))
        .filter_map(|e| {
            let content = std::fs::read_to_string(dir.join(&e.file)).ok()?;
            Some(parse_filter_file_named(&content))
        })
        .flatten()
        .map(|(_, f)| f)
        .collect()
}

/// Bundled files indexed by their prefilters, built once per process.
fn bundled_anchor_index() -> &'static [(String, Option<Vec<Prefilter>>)] {
    use std::sync::OnceLock;
    static INDEX: OnceLock<Vec<(String, Option<Vec<Prefilter>>)>> = OnceLock::new();
    INDEX.get_or_init(|| {
        BundledFilters::iter()
            .filter_map(|name| {
                let file = BundledFilters::get(&name)?;
                let content = std::str::from_utf8(file.data.as_ref()).ok()?;
                Some((name.to_string(), scan_bundled_anchors(content)))
            })
            .collect()
    })
}

/// Bundled filters whose file can possibly match one of `candidates`.
///
/// Parsing all 528 bundled TOML files costs ~29 ms; a hook process only ever
/// needs the handful whose literals appear in the command it was handed. The
/// prefilter is the same necessary condition `find_filter` applies per
/// pattern, so the surviving set contains every filter the full load would
/// have matched.
fn load_bundled_filters_matching(candidates: &[String]) -> Vec<FilterDef> {
    bundled_anchor_index()
        .iter()
        .filter(|(_, anchors)| any_allows(anchors, candidates))
        .filter_map(|(name, _)| {
            let file = BundledFilters::get(name)?;
            let content = std::str::from_utf8(file.data.as_ref()).ok()?;
            Some(parse_filter_file_named(content))
        })
        .flatten()
        .map(|(_, f)| f)
        .collect()
}

/// Local + user filters (always parsed — a handful of files) plus only the
/// bundled filters that can match `cmd`. Drop-in replacement for
/// `load_all_filters()` on the single-command hot path (hook rewrite,
/// `tokenix run`), where the full bundled set is never needed.
pub fn load_filters_for_command(cmd: &str) -> Vec<FilterDef> {
    let candidates = prioritized_candidates(cmd);
    let mut all = load_local_filters();
    all.extend(load_user_filters_matching(&candidates));
    all.extend(load_bundled_filters_matching(&candidates));
    all
}

/// First embedded `[[tests.<name>]].input` of one filter file, keyed by filter
/// name.
fn samples_in(content: &str) -> HashMap<String, String> {
    #[derive(Deserialize)]
    struct SampleCase {
        input: String,
    }
    #[derive(Deserialize)]
    struct TestsOnly {
        #[serde(default)]
        tests: HashMap<String, Vec<SampleCase>>,
    }
    let mut map = HashMap::new();
    if let Ok(parsed) = toml::from_str::<TestsOnly>(content) {
        for (fname, cases) in parsed.tests {
            if let Some(first) = cases.into_iter().next() {
                map.entry(fname).or_insert(first.input);
            }
        }
    }
    map
}

/// Representative sample output per filter name, used by the shell's preview
/// (input → `apply_filter` → output).
///
/// Covers user and project filters too, not just bundled ones: a filter the
/// user just generated carries its own `[[tests]]` and would otherwise show
/// "no embedded sample" in the one place built to inspect it. Local overrides
/// user overrides bundled, matching filter precedence.
pub fn sample_inputs() -> HashMap<String, String> {
    let mut map = HashMap::new();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = crate::store::find_project_root(&cwd);
    // Untrusted repo-local files are not just skipped as filters — their
    // samples must not be shown either, or a cloned repo could put its own
    // text in the preview pane under a bundled filter's name.
    let mut dirs = vec![filters_dir()];
    if local_filters_trusted(&root) {
        dirs.insert(0, local_filters_dir());
    }
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(&path) {
                for (name, input) in samples_in(&content) {
                    map.entry(name).or_insert(input);
                }
            }
        }
    }
    for name in BundledFilters::iter() {
        let Some(file) = BundledFilters::get(&name) else {
            continue;
        };
        let Ok(content) = std::str::from_utf8(file.data.as_ref()) else {
            continue;
        };
        for (name, input) in samples_in(content) {
            map.entry(name).or_insert(input);
        }
    }
    map
}

/// Bundled-filter inventory for `tokenix doctor`: total embedded golden test
/// cases across every bundled filter file.
pub fn bundled_test_case_count() -> usize {
    #[derive(Deserialize)]
    struct TestsOnly {
        #[serde(default)]
        tests: HashMap<String, Vec<toml::Value>>,
    }
    let mut count = 0;
    for name in BundledFilters::iter() {
        let Some(file) = BundledFilters::get(&name) else {
            continue;
        };
        let Ok(content) = std::str::from_utf8(file.data.as_ref()) else {
            continue;
        };
        if let Ok(parsed) = toml::from_str::<TestsOnly>(content) {
            count += parsed.tests.values().map(|v| v.len()).sum::<usize>();
        }
    }
    count
}

pub fn load_active_filters() -> Vec<ActiveFilter> {
    let mut result: Vec<ActiveFilter> = load_local_filters_named()
        .into_iter()
        .map(|(name, filter)| ActiveFilter {
            name,
            source: "local",
            filter,
        })
        .collect();
    result.extend(
        load_user_filters_named()
            .into_iter()
            .map(|(name, filter)| ActiveFilter {
                name,
                source: "user",
                filter,
            }),
    );
    result.extend(
        load_bundled_filters_named()
            .into_iter()
            .map(|(name, filter)| ActiveFilter {
                name,
                source: "bundled",
                filter,
            }),
    );
    result
}

/// Returns local filters (highest priority), then user filters, then bundled filters as fallback.
pub fn load_all_filters() -> Vec<FilterDef> {
    let mut all = load_local_filters();
    all.extend(load_user_filters());
    all.extend(load_bundled_filters());
    all
}

/// Config problems in a filter's semantic_filter section (unknown model,
/// out-of-range threshold). Used by `tokenix doctor`; empty = healthy.
pub fn semantic_filter_issues(f: &FilterDef) -> Vec<String> {
    let mut issues = Vec::new();
    if let Some(sem) = &f.semantic_filter {
        if let Some(model) = &sem.model {
            if !crate::embed::is_known_model(model) {
                issues.push(format!(
                    "semantic_filter.model '{}' unknown — falls back to '{}'",
                    model,
                    crate::embed::DEFAULT_MODEL_ID
                ));
            }
        }
        if !(0.0..=1.0).contains(&sem.threshold) {
            issues.push(format!(
                "semantic_filter.threshold {} outside 0.0-1.0",
                sem.threshold
            ));
        }
    }
    issues
}

/// Process-wide compiled-regex cache. The hook is a short-lived process that
/// evaluates hundreds of `match_command` patterns against a handful of
/// candidate strings — each pattern must compile at most once per process,
/// not once per (pattern × candidate) pair.
fn cached_regex(pattern: &str) -> Option<Regex> {
    use std::sync::{OnceLock, RwLock};
    static CACHE: OnceLock<RwLock<HashMap<String, Option<Regex>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| RwLock::new(HashMap::new()));
    if let Some(hit) = cache.read().ok().and_then(|c| c.get(pattern).cloned()) {
        return hit;
    }
    let compiled = Regex::new(pattern).ok();
    if let Ok(mut c) = cache.write() {
        c.insert(pattern.to_string(), compiled.clone());
    }
    compiled
}

/// Length of the leading run of characters that are literal in a regex.
/// `.` and `+` are metacharacters and must end the run — `^go.mod` matches
/// `goXmod`, so treating the `.` as literal would let the prefilter reject a
/// real match.
fn literal_run(s: &str) -> usize {
    s.find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-')))
        .unwrap_or(s.len())
}

/// True when `pattern` has a top-level `|` alternation — a `|` outside any
/// group or character class, and not escaped.
///
/// This is what makes a leading-literal prefilter unsound: in
/// `^Select-String\b|\|\s*Select-String\b` only the FIRST branch is anchored,
/// so `Get-Content x | Select-String ERROR` is a real match that does not
/// start with the literal. No single literal is necessary for such a pattern.
fn has_top_level_alternation(pattern: &str) -> bool {
    let (mut depth, mut in_class, mut escaped) = (0i32, false, false);
    for c in pattern.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' => escaped = true,
            '[' if !in_class => in_class = true,
            ']' if in_class => in_class = false,
            '(' if !in_class => depth += 1,
            ')' if !in_class => depth -= 1,
            '|' if !in_class && depth <= 0 => return true,
            _ => {}
        }
    }
    false
}

/// Literal start-anchor of a pattern (`^cargo\s+test` → `cargo`) when the
/// pattern begins with `^` followed by a plain literal. Cheap NECESSARY
/// condition: a candidate that doesn't start with the literal can never match
/// the anchored regex, so its compilation is skipped entirely.
fn literal_anchor(pattern: &str) -> Option<&str> {
    let rest = pattern.strip_prefix('^')?;
    let end = literal_run(rest);
    (end >= 2).then(|| &rest[..end])
}

/// A cheap necessary condition for a pattern to match a candidate. Evaluating
/// it costs a substring scan; failing it skips a regex compilation (~100 µs
/// cold), which is what a short-lived hook process spends most of its time on.
#[derive(Debug, Clone, PartialEq, Deserialize, serde::Serialize)]
enum Prefilter {
    /// Anchored pattern: the candidate must start with this literal.
    StartsWith(String),
    /// Unanchored but mandatory literal: the candidate must contain it.
    /// `bool` = case-insensitive (pattern carries a leading `(?i)`).
    Contains(String, bool),
}

impl Prefilter {
    fn allows(&self, candidate: &str) -> bool {
        match self {
            Prefilter::StartsWith(lit) => candidate.starts_with(lit.as_str()),
            Prefilter::Contains(lit, false) => candidate.contains(lit.as_str()),
            Prefilter::Contains(lit, true) => candidate.to_lowercase().contains(lit.as_str()),
        }
    }
}

/// First literal that every match of `pattern` must contain, for the shapes
/// this repo's filters actually use: a leading `\b`, an optional group
/// (`^(npx\s+)?eslint`), and `(?i)` flags. Everything else returns `None` and
/// is evaluated by compiling the regex — the prefilter may only skip work.
fn prefilter_for(pattern: &str) -> Option<Prefilter> {
    // Checked once, up front: with a top-level alternation no single literal
    // is mandatory, so neither the anchor nor the contains form is sound.
    if has_top_level_alternation(pattern) {
        return None;
    }
    if let Some(anchor) = literal_anchor(pattern) {
        return Some(Prefilter::StartsWith(anchor.to_string()));
    }
    let mut s = pattern;
    let mut ci = false;
    if let Some(rest) = s.strip_prefix("(?i)") {
        ci = true;
        s = rest;
    }
    // Walk past zero-width assertions and optional groups: none of them can
    // consume the mandatory literal that follows.
    loop {
        if let Some(r) = s.strip_prefix('^') {
            s = r;
        } else if let Some(r) = s.strip_prefix("\\b") {
            s = r;
        } else if s.starts_with('(') {
            let close = s.find(')')?;
            if s[1..close].contains('(') {
                return None; // nested group: out of scope for this scanner
            }
            match s[close + 1..].strip_prefix('?') {
                Some(rest) => s = rest, // optional group: skip it
                None => return None,    // mandatory group (alternation)
            }
        } else {
            break;
        }
    }
    let end = literal_run(s);
    let mut lit = &s[..end];
    // A quantifier right after the run makes its last character optional.
    if matches!(s[end..].chars().next(), Some('?') | Some('*') | Some('{')) {
        lit = &lit[..lit.len().saturating_sub(1)];
    }
    (lit.len() >= 3).then(|| {
        Prefilter::Contains(
            if ci {
                lit.to_lowercase()
            } else {
                lit.to_string()
            },
            ci,
        )
    })
}

/// Candidate strings a command is matched against, most specific first:
/// per-segment (last segment first, effective form then raw), then the whole
/// compound command. Shared by `find_filter` and the lazy bundled loader so
/// both see exactly the same candidate set.
fn prioritized_candidates(cmd: &str) -> Vec<String> {
    let shell_body = unwrap_shell_runner(cmd);
    let base = shell_body.as_deref().unwrap_or(cmd);
    let segments = split_on_operators(base);

    let mut candidates = Vec::new();

    // 1. Segment-level candidates: last segment first
    for segment in segments.iter().rev() {
        let effective = get_effective_command(segment);
        push_unique(&mut candidates, &effective);
        push_unique(&mut candidates, segment);
    }

    // 2. Full compound candidates
    let effective_full = get_effective_command(cmd);
    push_unique(&mut candidates, &effective_full);
    if let Some(body) = &shell_body {
        let effective_body = get_effective_command(body);
        push_unique(&mut candidates, &effective_body);
        push_unique(&mut candidates, body);
    }
    push_unique(&mut candidates, cmd);
    candidates
}

pub fn find_filter<'a>(cmd: &str, filters: &'a [FilterDef]) -> Option<&'a FilterDef> {
    let effective = get_effective_command(cmd);
    let tokens = tokenize_command(&effective);

    // 1. Help flags bypass: let raw help outputs pass through unfiltered
    let has_help = tokens.iter().any(|t| {
        let t_lower = t.to_lowercase();
        t == "-h"
            || t == "-help"
            || t_lower == "--help"
            || t == "/h"
            || t == "/?"
            || t_lower == "help"
            || t_lower.starts_with("--help-")
            || t_lower.starts_with("-help-")
    });
    if has_help {
        return None;
    }

    // 2. Version flags bypass: version output is short and shouldn't be masked as success.
    // Lowercase `-v` is treated as a version flag only for tools where it actually queries version
    // (e.g. git, node, docker, npm), to avoid falsely bypassing verbose runs (e.g. cargo, pytest, python).
    let mut has_version = false;
    for (i, t) in tokens.iter().enumerate() {
        let t_lower = t.to_lowercase();
        if t_lower == "--version" || t_lower == "version" {
            has_version = true;
            break;
        }
        if t == "-V" {
            has_version = true;
            break;
        }
        if t == "-v" && i > 0 {
            let prev_tool = std::path::Path::new(&tokens[i - 1])
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or(&tokens[i - 1])
                .to_lowercase();
            let prev_tool = prev_tool.strip_suffix(".exe").unwrap_or(&prev_tool);
            if matches!(
                prev_tool,
                "git" | "node" | "docker" | "npm" | "npx" | "pnpm" | "yarn" | "bun"
            ) {
                has_version = true;
                break;
            }
        }
    }
    if !has_version && !tokens.is_empty() && (tokens[0] == "version" || tokens[0] == "--version") {
        has_version = true;
    }

    if has_version && tokens.len() <= 3 {
        return None;
    }

    // 3. Debug/verbose flags bypass: keep troubleshooting logs intact
    let has_debug_or_verbose = tokens.iter().any(|t| {
        let t_lower = t.to_lowercase();
        t == "-vv"
            || t == "-vvv"
            || t_lower == "--debug"
            || t_lower == "--verbose"
            || t_lower == "--trace"
            || t_lower.starts_with("--log-level=debug")
            || t_lower.starts_with("--log-level=trace")
    });
    if has_debug_or_verbose {
        return None;
    }

    // 4. YAML check to prevent breaking YAML outputs
    let has_yaml = tokens.iter().any(|t| {
        let t_lower = t.to_lowercase();
        t_lower == "--yaml" || t_lower == "-o=yaml" || t_lower == "yaml"
    }) || effective.contains("-o yaml")
        || effective.contains("--format yaml")
        || effective.contains("--format=yaml");
    if has_yaml {
        return None;
    }

    // 5. JSON check to prevent breaking JSON outputs unless explicitly handled by the filter
    let has_json = tokens.iter().enumerate().any(|(i, t)| {
        let t_lower = t.to_lowercase();
        if t_lower == "--json" || t_lower == "-o=json" || t_lower == "json" {
            true
        } else if t == "-j" {
            if i > 0 {
                let prev_tool = std::path::Path::new(&tokens[i - 1])
                    .file_name()
                    .and_then(|f| f.to_str())
                    .unwrap_or(&tokens[i - 1])
                    .to_lowercase();
                let prev_tool = prev_tool.strip_suffix(".exe").unwrap_or(&prev_tool);
                !matches!(
                    prev_tool,
                    "cargo" | "make" | "ninja" | "cmake" | "mvn" | "gradle" | "build"
                )
            } else {
                true
            }
        } else {
            false
        }
    }) || effective.contains("-o json")
        || effective.contains("--format json")
        || effective.contains("--format=json")
        || effective.contains("--message-format=json")
        || effective.contains("--message-format json");

    let prioritized_candidates = prioritized_candidates(cmd);

    // Find the first filter that matches any prioritized candidate, resolving collisions
    // by picking the one with the longest (most specific) match_command pattern.
    for candidate in &prioritized_candidates {
        let mut best_match: Option<&'a FilterDef> = None;
        let mut max_len = 0;

        for f in filters {
            // Hot-path prefilter: skip compiling patterns whose mandatory
            // literal can't match this candidate (the common case — one
            // command vs hundreds of tool-anchored patterns).
            if let Some(pf) = prefilter_for(&f.match_command) {
                if !pf.allows(candidate) {
                    continue;
                }
            }
            if let Some(re) = cached_regex(&f.match_command) {
                if re.is_match(candidate) {
                    // JSON bypass: if command asks for JSON output, but filter does not support summarizing JSON, bypass
                    if has_json && f.summarize_json.is_none() {
                        continue;
                    }
                    // Per-filter skip: candidate matches a form the filter opts out of
                    // (e.g. already-bounded reads via -Tail, or piped into another command).
                    if let Some(skip) = &f.skip_when_matches {
                        if let Some(skip_re) = cached_regex(skip) {
                            if skip_re.is_match(candidate) {
                                continue;
                            }
                        }
                    }
                    let pattern_len = f.match_command.len();
                    if pattern_len > max_len {
                        max_len = pattern_len;
                        best_match = Some(f);
                    }
                }
            }
        }
        if let Some(f) = best_match {
            return Some(f);
        }
    }
    None
}

pub fn tokenize_command(command: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaping = false;

    for c in command.trim().chars() {
        if escaping {
            current.push(c);
            escaping = false;
            continue;
        }

        if c == '\\' {
            escaping = true;
            continue;
        }

        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else {
                current.push(c);
            }
            continue;
        }

        if c == '\'' || c == '"' {
            quote = Some(c);
            continue;
        }

        if c.is_whitespace() {
            if !current.is_empty() {
                tokens.push(current);
                current = String::new();
            }
            continue;
        }

        current.push(c);
    }

    if escaping {
        current.push('\\');
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}

pub fn unwrap_shell_runner(cmd: &str) -> Option<String> {
    let argv = tokenize_command(cmd);
    if argv.is_empty() {
        return None;
    }

    let first = &argv[0];
    let first_path = std::path::Path::new(first);
    let launcher_name = first_path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(first)
        .to_lowercase();
    let launcher_name_no_ext = launcher_name.strip_suffix(".exe").unwrap_or(&launcher_name);

    let is_shell = matches!(
        launcher_name_no_ext,
        "bash"
            | "sh"
            | "zsh"
            | "fish"
            | "dash"
            | "ksh"
            | "mksh"
            | "ash"
            | "csh"
            | "tcsh"
            | "cmd"
            | "powershell"
            | "pwsh"
    );

    if !is_shell {
        return None;
    }

    for i in 1..(argv.len().saturating_sub(1)) {
        let arg = &argv[i];
        let is_command_flag = if launcher_name_no_ext == "cmd" {
            arg.eq_ignore_ascii_case("/c") || arg.eq_ignore_ascii_case("-c")
        } else if launcher_name_no_ext == "powershell" || launcher_name_no_ext == "pwsh" {
            arg.eq_ignore_ascii_case("-c")
                || arg.eq_ignore_ascii_case("-command")
                || arg.eq_ignore_ascii_case("--command")
        } else {
            arg.starts_with('-') && arg.contains('c')
        };

        if is_command_flag {
            return Some(argv[i + 1].trim().to_string());
        }
    }

    None
}

fn is_env_assignment(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    if !bytes[0].is_ascii_alphabetic() && bytes[0] != b'_' {
        return false;
    }
    let mut i = 1;
    while i < bytes.len() {
        if bytes[i] == b'=' {
            return i > 0;
        }
        if !bytes[i].is_ascii_alphanumeric() && bytes[i] != b'_' {
            return false;
        }
        i += 1;
    }
    false
}

fn strip_leading_env_assignments(argv: &[String]) -> Vec<String> {
    let mut index = 0;
    while index < argv.len() && is_env_assignment(&argv[index]) {
        index += 1;
    }

    if index < argv.len() {
        let cmd_path = std::path::Path::new(&argv[index]);
        let cmd_name = cmd_path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or(&argv[index]);
        if cmd_name == "env" || cmd_name == "cross-env" {
            index += 1;
            while index < argv.len() {
                let arg = &argv[index];
                if arg == "--" {
                    index += 1;
                    break;
                }
                if is_env_assignment(arg) {
                    index += 1;
                    continue;
                }
                if arg == "-i" || arg == "-0" || arg == "--ignore-environment" || arg == "--debug" {
                    index += 1;
                    continue;
                }
                if arg == "-u"
                    || arg == "--unset"
                    || arg == "-C"
                    || arg == "--chdir"
                    || arg == "-S"
                    || arg == "--split-string"
                {
                    index += 2;
                    continue;
                }
                if arg.starts_with("--unset=")
                    || arg.starts_with("--chdir=")
                    || arg.starts_with("--split-string=")
                {
                    index += 1;
                    continue;
                }
                break;
            }
        }
    }

    argv[index..].to_vec()
}

/// Strip known Unix command-timing / resource-limit wrappers that prefix the
/// real command without altering its behaviour for filter-matching purposes:
///
/// - `timeout [OPTS] DURATION CMD`  (GNU coreutils)
/// - `time CMD`
/// - `nice [-n N] CMD`
/// - `ionice [-c C] [-n N] [-t] CMD`
///
/// Each wrapper is peeled in a loop so stacked prefixes like
/// `timeout 30 nice -n 10 pnpm run test` resolve to `pnpm run test`.
fn strip_leading_wrappers(argv: &[String]) -> Vec<String> {
    let mut index = 0;

    loop {
        if index >= argv.len() {
            break;
        }
        let name_raw = std::path::Path::new(&argv[index])
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or(&argv[index])
            .to_lowercase();
        let name = name_raw.strip_suffix(".exe").unwrap_or(&name_raw);

        match name {
            // `& CMD` — PowerShell call operator
            "&" => {
                index += 1;
            }
            // `time CMD` — single token prefix
            "time" => {
                index += 1;
            }
            // `nice [-n N | --adjustment[=]N] CMD`
            "nice" => {
                index += 1;
                if index < argv.len() {
                    let a = &argv[index];
                    if a == "-n" || a == "--adjustment" {
                        index += 2;
                    } else if a.starts_with("--adjustment=") {
                        index += 1;
                    }
                }
            }
            // `ionice [-c C] [-n N] [-t] CMD`
            "ionice" => {
                index += 1;
                while index < argv.len() {
                    let a = &argv[index];
                    if (a == "-c" || a == "-n") && index + 1 < argv.len() {
                        index += 2;
                    } else if a == "-t" {
                        index += 1;
                    } else {
                        break;
                    }
                }
            }
            // `timeout [OPTS] DURATION CMD`
            // Options: --foreground, --preserve-status, --verbose, -k DUR, -s SIG
            "timeout" => {
                index += 1;
                let mut found_duration = false;
                while index < argv.len() {
                    let a = &argv[index];
                    if matches!(
                        a.as_str(),
                        "--foreground" | "--preserve-status" | "--verbose"
                    ) {
                        index += 1;
                        continue;
                    }
                    if (a == "-k" || a == "--kill-after" || a == "-s" || a == "--signal")
                        && index + 1 < argv.len()
                    {
                        index += 2;
                        continue;
                    }
                    if a.starts_with("--kill-after=") || a.starts_with("--signal=") {
                        index += 1;
                        continue;
                    }
                    if a.starts_with('-') {
                        index += 1;
                        continue;
                    }
                    // First non-option argument is the DURATION — skip it.
                    index += 1;
                    found_duration = true;
                    break;
                }
                if !found_duration {
                    // Malformed `timeout` invocation — stop peeling.
                    break;
                }
            }
            _ => break,
        }
    }

    argv[index..].to_vec()
}

/// Drop a leading package-runner prefix so the inner tool's filter matches:
/// `uv run pytest` -> `pytest`, `python -m ruff check` -> `ruff check`,
/// `bunx biome check` -> `biome check`, `npx tsc` -> `tsc`,
/// `pnpm exec eslint` / `pnpm dlx`/`yarn dlx`/`bun x`/`deno run`/`deno task`.
/// Returns the tail after the runner, or the input unchanged.
fn strip_package_runner(argv: &[String]) -> &[String] {
    if argv.is_empty() {
        return argv;
    }
    let t0 = std::path::Path::new(&argv[0])
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(&argv[0])
        .to_lowercase();
    let t0 = t0.strip_suffix(".exe").unwrap_or(&t0);

    let mut start_idx = 0;

    // 1. Detect the runner and set the start index after the runner tokens
    if matches!(t0, "npx" | "bunx" | "uvx") && argv.len() > 1 {
        start_idx = 1;
    } else if argv.len() > 2 {
        let t1 = argv[1].as_str();
        let pair = (t0, t1);
        if matches!(
            pair,
            ("uv", "run")
                | ("pnpm", "exec")
                | ("pnpm", "dlx")
                | ("yarn", "dlx")
                | ("bun", "x")
                | ("deno", "run")
                | ("deno", "task")
                | ("bundle", "exec")
        ) || (matches!(t0, "python" | "python3" | "py") && t1 == "-m")
        {
            start_idx = 2;
        }
    }

    if start_idx == 0 {
        return argv;
    }

    // 2. Skip any options/flags belonging to the runner itself (e.g. npx --no-install, uv run --with requests)
    let mut idx = start_idx;
    while idx < argv.len() {
        let arg = &argv[idx];
        if arg.starts_with('-') {
            if arg == "--" {
                idx += 1;
                break; // double dash indicates end of runner options
            }
            if (arg == "-p" || arg == "--package" || arg == "--with" || arg == "--import")
                && idx + 1 < argv.len()
            {
                idx += 2;
            } else {
                idx += 1;
            }
        } else {
            break;
        }
    }

    if idx < argv.len() {
        &argv[idx..]
    } else {
        &argv[start_idx..] // fallback
    }
}

/// Drop tool-global options that sit *between* a subcommand tool and its
/// subcommand, so a filter anchored on `^git\s+add` still matches
/// `git -C /repo -c user.name=x add .`. Without this, idiomatic invocations
/// like `git -C dir`, `kubectl -n ns`, `docker -H host` or `cargo +nightly`
/// bypass every subcommand filter and ship raw output.
///
/// Returns the tool name followed by the first non-option token onward, e.g.
/// `["git", "-C", "dir", "add", "."]` -> `["git", "add", "."]`. Tools not in
/// the recognized set are returned unchanged.
fn strip_subcommand_global_opts(argv: &[String]) -> Vec<String> {
    if argv.is_empty() {
        return Vec::new();
    }
    let tool = std::path::Path::new(&argv[0])
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(&argv[0])
        .to_lowercase();
    let tool = tool.strip_suffix(".exe").unwrap_or(&tool);

    // (flags taking a following value, boolean flags)
    let (valued, boolean): (&[&str], &[&str]) = match tool {
        "git" => (
            &[
                "-C",
                "-c",
                "--git-dir",
                "--work-tree",
                "--exec-path",
                "--namespace",
                "--super-prefix",
            ],
            &[
                "-p",
                "-P",
                "--paginate",
                "--no-pager",
                "--bare",
                "--no-replace-objects",
                "--literal-pathspecs",
                "--glob-pathspecs",
                "--noglob-pathspecs",
                "--icase-pathspecs",
                "--no-optional-locks",
            ],
        ),
        "kubectl" => (
            &[
                "-n",
                "--namespace",
                "--context",
                "--kubeconfig",
                "-s",
                "--server",
                "--token",
                "--as",
                "--as-group",
                "--cluster",
                "--user",
                "--cache-dir",
                "--request-timeout",
                "--client-certificate",
                "--client-key",
                "--certificate-authority",
                "--tls-server-name",
            ],
            &["--insecure-skip-tls-verify"],
        ),
        "docker" => (
            &[
                "-H",
                "--host",
                "--context",
                "--config",
                "-l",
                "--log-level",
                "--tlscacert",
                "--tlscert",
                "--tlskey",
            ],
            &["-D", "--debug", "--tls", "--tlsverify"],
        ),
        "terraform" | "tofu" => (&["-chdir"], &[]),
        "cargo" => (&[], &[]),
        "pnpm" => (
            &[
                "-C",
                "--dir",
                "--filter",
                "--reporter",
                "--store-dir",
                "--virtual-store-dir",
                "--loglevel",
            ],
            &[
                "-w",
                "--workspace",
                "-r",
                "--recursive",
                "--prod",
                "-D",
                "--dev",
                "--no-optional",
                "--frozen-lockfile",
                "--silent",
            ],
        ),
        "bun" => (
            &[
                "--cwd",
                "-c",
                "--config",
                "--filter",
                "-p",
                "--port",
                "--env-file",
                "--profile",
            ],
            &[
                "--watch",
                "--hot",
                "--smol",
                "--no-buffer",
                "-v",
                "--version",
            ],
        ),
        _ => return argv.to_vec(),
    };

    let mut i = 1;
    while i < argv.len() {
        let a = &argv[i];
        // `cargo +nightly test`
        if tool == "cargo" && a.starts_with('+') {
            i += 1;
            continue;
        }
        if a.starts_with("--") {
            if a.contains('=') {
                i += 1; // --opt=value
                continue;
            }
            if valued.contains(&a.as_str()) {
                i += if i + 1 < argv.len() { 2 } else { 1 }; // --opt value
                continue;
            }
            if boolean.contains(&a.as_str()) {
                i += 1;
                continue;
            }
            break;
        } else if a.len() >= 2 && a.starts_with('-') {
            if let Some((key, _)) = a.split_once('=') {
                if valued.contains(&key) {
                    i += 1; // -opt=value
                    continue;
                }
            }
            if valued.contains(&a.as_str()) {
                i += if i + 1 < argv.len() { 2 } else { 1 }; // -C dir
                continue;
            }
            if boolean.contains(&a.as_str()) {
                i += 1;
                continue;
            }
            break;
        } else {
            break; // subcommand reached
        }
    }

    if i == 1 {
        return argv.to_vec();
    }
    let mut out = Vec::with_capacity(1 + argv.len() - i);
    out.push(argv[0].clone());
    out.extend_from_slice(&argv[i..]);
    out
}

fn strip_cd_and_operators(mut argv: &[String]) -> &[String] {
    for _ in 0..8 {
        if argv.is_empty() {
            break;
        }
        let first = &argv[0];
        if first == "cd" || first == "pushd" {
            if argv.len() >= 2 && (argv[1] == "&&" || argv[1] == ";") {
                argv = &argv[2..];
                continue;
            }
            if argv.len() >= 3 && (argv[2] == "&&" || argv[2] == ";") {
                argv = &argv[3..];
                continue;
            }
        }
        break;
    }
    argv
}

pub fn get_effective_command(cmd: &str) -> String {
    let mut current = cmd.trim().to_string();

    for _ in 0..16 {
        let unwrapped = unwrap_shell_runner(&current);
        if let Some(inner) = unwrapped {
            current = inner;
            continue;
        }

        let tokens = tokenize_command(&current);
        if tokens.is_empty() {
            break;
        }

        let stripped_env = strip_leading_env_assignments(&tokens);
        let stripped_wrappers = strip_leading_wrappers(&stripped_env);
        let stripped_cd = strip_cd_and_operators(&stripped_wrappers);
        let stripped_runner = strip_package_runner(stripped_cd);
        let stripped_opts = strip_subcommand_global_opts(stripped_runner);

        if stripped_opts.len() == tokens.len() {
            break;
        }

        current = stripped_opts.join(" ");
    }

    current
}

/// Split a shell command into segments on the operators `&&`, `||`, `;` and the
/// pipe `|`, quote- and escape-aware. Operators are recognized regardless of
/// surrounding whitespace, so `a;b` and `a ; b` segment identically.
/// Quoted operators (e.g. `echo "a;b"`) are left intact.
pub fn split_on_operators(cmd: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaping = false;

    let chars: Vec<char> = cmd.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];

        if escaping {
            current.push(c);
            escaping = false;
            i += 1;
            continue;
        }
        if c == '\\' {
            current.push(c);
            escaping = true;
            i += 1;
            continue;
        }
        if let Some(q) = quote {
            current.push(c);
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if c == '\'' || c == '"' {
            quote = Some(c);
            current.push(c);
            i += 1;
            continue;
        }

        let next = chars.get(i + 1).copied();
        // Two-char operators `&&` / `||` first, so the trailing `|` of `||`
        // is not mistaken for a pipe split.
        if (c == '&' && next == Some('&')) || (c == '|' && next == Some('|')) {
            push_segment(&mut segments, &mut current);
            i += 2;
            continue;
        }
        if c == ';' || c == '|' {
            push_segment(&mut segments, &mut current);
            i += 1;
            continue;
        }

        current.push(c);
        i += 1;
    }
    push_segment(&mut segments, &mut current);
    segments
}

fn push_segment(segments: &mut Vec<String>, current: &mut String) {
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        segments.push(trimmed.to_string());
    }
    current.clear();
}

fn push_unique(candidates: &mut Vec<String>, candidate: &str) {
    let trimmed = candidate.trim();
    if !trimmed.is_empty() && !candidates.iter().any(|c| c == trimmed) {
        candidates.push(trimmed.to_string());
    }
}

#[cfg(test)]
pub fn derive_command_candidates(cmd: &str) -> Vec<String> {
    let mut candidates = Vec::new();

    push_unique(&mut candidates, cmd);

    let shell_body = unwrap_shell_runner(cmd);
    if let Some(body) = &shell_body {
        push_unique(&mut candidates, body);
    }

    push_unique(&mut candidates, &get_effective_command(cmd));

    // Operator-aware segmentation: split compound commands and add
    // each segment plus its effective form, so a filter anchored on its base
    // command matches regardless of position or spacing — e.g. `cd x;gitleaks`,
    // `npm i && gitleaks`, or `producer | gitleaks`.
    let mut bases = vec![cmd.to_string()];
    if let Some(body) = shell_body {
        bases.push(body);
    }
    for base in &bases {
        for segment in split_on_operators(base) {
            let effective = get_effective_command(&segment);
            push_unique(&mut candidates, &segment);
            push_unique(&mut candidates, &effective);
        }
    }

    candidates
}

/// Final safety net: a filter must never make the output more expensive than
/// the raw it replaces (a short raw beaten by a longer success message or an
/// omission notice). Byte length is a cheap, monotone proxy for tokens here.
/// Genuinely empty raw is exempt so `on_empty` sentinels still fire for
/// commands that print nothing.
/// `Some(summary)` when every significant line of `output` matches the pass
/// shape and at least one did. `None` means the run is not uniform — a single
/// dissenting line is enough — and the normal pipeline must handle it.
fn uniform_summary(output: &str, cfg: &UniformSuccess) -> Option<String> {
    let pass = cached_regex(&cfg.pattern)?;
    let ignore: Vec<Regex> = cfg
        .ignore_lines
        .iter()
        .filter_map(|p| cached_regex(p))
        .collect();
    let mut matched = 0usize;
    for line in output.lines() {
        if line.trim().is_empty() || ignore.iter().any(|re| re.is_match(line)) {
            continue;
        }
        if !pass.is_match(line) {
            return None;
        }
        matched += 1;
    }
    (matched > 0).then(|| cfg.message.replace("{n}", &matched.to_string()))
}

fn never_worse(raw: &str, filtered: String) -> String {
    let raw_trim = raw.trim_end();
    if !raw_trim.is_empty() && filtered.len() > raw_trim.len() {
        return raw_trim.to_string();
    }
    filtered
}

pub fn apply_filter(output: &str, f: &FilterDef) -> String {
    apply_filter_with_exit(output, f, None)
}

/// `exit_ok = Some(false)` means the run wrapper observed a nonzero exit.
/// On failure: the filter's `on_failure` policy ("passthrough" | "tail:N")
/// takes over if set; otherwise normal filtering runs but success sentinels
/// (`match_output` messages, `on_empty`) are suppressed — text heuristics
/// alone miss failing commands with quiet or unusual output.
pub fn apply_filter_with_exit(output: &str, f: &FilterDef, exit_ok: Option<bool>) -> String {
    let failed = exit_ok == Some(false);
    if failed {
        if let Some(policy) = &f.on_failure {
            if policy == "passthrough" {
                return output.trim_end().to_string();
            }
            if let Some(n) = policy
                .strip_prefix("tail:")
                .and_then(|n| n.parse::<usize>().ok())
            {
                let lines: Vec<&str> = output.trim_end().lines().collect();
                let start = lines.len().saturating_sub(n);
                return lines[start..].join("\n");
            }
        }
    }

    // match_output short-circuits before any other transformation. These are
    // success sentinels by convention — never fire them on a known failure.
    if !failed {
        for mo in &f.match_output {
            if let Some(re) = cached_regex(&mo.pattern) {
                if re.is_match(output) {
                    // `unless` guard: do not short-circuit when the output also matches
                    // this pattern, so errors/warnings are never masked as success.
                    if let Some(unless) = &mo.unless {
                        if cached_regex(unless)
                            .map(|u| u.is_match(output))
                            .unwrap_or(false)
                        {
                            continue;
                        }
                    }
                    return never_worse(output, mo.message.clone());
                }
            }
        }
    }

    let s = if f.strip_ansi {
        crate::compress::strip_ansi(output)
    } else {
        output.to_string()
    };

    // Whole-run success: every significant line agrees, so the summary is a
    // statement about the run. Evaluated on the ANSI-stripped text (these
    // tools colorize their per-item lines) and suppressed on a known failure
    // like every other success sentinel.
    if !failed {
        if let Some(uniform) = &f.uniform_success {
            if let Some(msg) = uniform_summary(&s, uniform) {
                return never_worse(output, msg);
            }
        }
    }

    let mut lines: Vec<String> = s.lines().map(|l| l.to_string()).collect();

    if !f.strip_lines_matching.is_empty() {
        let patterns: Vec<Regex> = f
            .strip_lines_matching
            .iter()
            .filter_map(|p| cached_regex(p))
            .collect();
        lines.retain(|l| !patterns.iter().any(|re| re.is_match(l)));
    }

    if !f.keep_lines_matching.is_empty() {
        let patterns: Vec<Regex> = f
            .keep_lines_matching
            .iter()
            .filter_map(|p| cached_regex(p))
            .collect();
        lines.retain(|l| patterns.iter().any(|re| re.is_match(l)));
    }

    // NEW: extract_sections - extract content between markers
    if !f.extract_sections.is_empty() {
        lines = apply_extract_sections(lines, &f.extract_sections);
    }

    // NEW: replace_patterns - regex replacements
    if !f.replace_patterns.is_empty() {
        lines = apply_replace_patterns(lines, &f.replace_patterns);
    }

    // category_caps - bound repetitive line classes independently
    if !f.category_caps.is_empty() {
        lines = apply_category_caps(lines, &f.category_caps);
    }

    // NEW: deduplicate_blocks - structural deduplication
    if let Some(dedup) = &f.deduplicate_blocks {
        lines = apply_deduplicate_blocks(lines, dedup);
    }

    // NEW: semantic_filter - embedding-based relevance filtering
    if let Some(semantic) = &f.semantic_filter {
        lines = apply_semantic_filter(lines, semantic);
    }

    // NEW: summarize_json - intelligent JSON summarization
    if let Some(summarize) = &f.summarize_json {
        lines = apply_summarize_json(lines, summarize);
    }

    let (mut lines, omitted_by_sizing) = apply_sizing(lines, f);
    if f.notify_on_truncate && omitted_by_sizing > 0 {
        lines.push(format!(
            "[... {omitted_by_sizing} lines omitted (line limit) ...]"
        ));
    }

    // NEW: token_budget - hard token limit with smart truncation
    let mut result = if let Some(max_len) = f.truncate_lines_at {
        let mut truncated_line_count = 0usize;
        let joined = lines
            .iter()
            .map(|l| {
                let t = truncate_at_char_boundary(l, max_len);
                if t.len() < l.len() {
                    truncated_line_count += 1;
                }
                t
            })
            .collect::<Vec<_>>()
            .join("\n");
        if f.notify_on_truncate && truncated_line_count > 0 {
            format!(
                "{joined}\n[... {truncated_line_count} line(s) truncated to {max_len} chars ...]"
            )
        } else {
            joined
        }
    } else {
        lines.join("\n")
    };

    if let Some(budget) = f.token_budget {
        result = apply_token_budget(&result, budget);
    }

    if result.trim().is_empty() {
        // Fall back to a bounded view of the real output (instead of `on_empty`)
        // when filtering emptied a non-empty output AND either:
        //  - the filter opted in via `passthrough_when_emptied`, or
        //  - the original output carries a generic failure signal that the
        //    per-tool keep/strip rules didn't recognize. Without this guard a
        //    failed build/test/deploy whose error text doesn't match the
        //    tool's native error format would be masked as the success
        //    `on_empty` message — the same "never mask errors" rule the
        //    `match_output.unless` guard enforces.
        let masks_failure = f.on_empty.is_some() && (failed || output_has_failure_signal(output));
        if !output.trim().is_empty() && (f.passthrough_when_emptied || masks_failure) {
            let cap = f.max_lines.unwrap_or(40);
            let fallback: Vec<String> = s
                .lines()
                .take(cap)
                .map(|l| match f.truncate_lines_at {
                    Some(n) => truncate_at_char_boundary(l, n).to_string(),
                    None => l.to_string(),
                })
                .collect();
            let fb_text = fallback.join("\n");
            return never_worse(
                output,
                match f.token_budget {
                    Some(budget) => apply_token_budget(&fb_text, budget),
                    None => fb_text,
                },
            );
        }
        if let Some(msg) = &f.on_empty {
            // A failed command must never earn the success sentinel, even
            // when it printed nothing — the empty result stands on its own.
            if !failed {
                return never_worse(output, msg.clone());
            }
        }
    }
    never_worse(output, result)
}

/// Strict detection of an unambiguous command-failure signal in raw output.
/// Used as a safety net so a filter never reports its success `on_empty`
/// message for output that actually describes a failure. Patterns are
/// deliberately anchored/cased to avoid tripping on benign mentions like
/// "0 errors", "no failures", or "error: 0".
fn output_has_failure_signal(output: &str) -> bool {
    use std::sync::OnceLock;
    static FAILURE: OnceLock<Regex> = OnceLock::new();
    let re = FAILURE.get_or_init(|| {
        Regex::new(
            r"(?m)^\s*(?:(?i:error|fatal|panic|panicked|exception|stderr|err)\b|FAILED\b|FAIL\b|---\s*FAIL\b|Traceback \(most recent call last\)|Unhandled exception|Exception in thread\b)|\b(?i:failed with exit code|exited with status|exited with code|exit status|exit code|exit)\b\s*[:=]?\s*[1-9]\d*|\b(?:SIGSEGV|SIGABRT|SIGILL|SIGBUS|AssertionError|NullPointerException|Segmentation fault|(?i:Command failed|command not found|failed to compile))\b|\[(?i:error|fatal|panic|failed|fail)\]|level=(?i:error|fatal|panic)|\b(?i:err)(?:!|:)"
        )
        .expect("failure-signal regex compiles")
    });
    re.is_match(output)
}

fn apply_extract_sections(lines: Vec<String>, sections: &[ExtractSection]) -> Vec<String> {
    let mut result = Vec::new();
    let content = lines.join("\n");

    for section in sections {
        let start_re = match cached_regex(&section.start_pattern) {
            Some(r) => r,
            None => continue,
        };
        let end_re = match cached_regex(&section.end_pattern) {
            Some(r) => r,
            None => continue,
        };

        let mut matches = 0;
        let max_matches = section.max_matches.unwrap_or(usize::MAX);

        let mut in_section = false;
        let mut section_lines = Vec::new();

        for line in content.lines() {
            let start_match = start_re.is_match(line);
            let end_match = end_re.is_match(line);

            if start_match && !in_section {
                in_section = true;
                if section.include_markers {
                    section_lines.push(line.to_string());
                }
                continue;
            }

            if in_section {
                if section.include_markers || !end_match {
                    section_lines.push(line.to_string());
                }
                if end_match {
                    result.append(&mut section_lines);
                    matches += 1;
                    in_section = false;
                    if matches >= max_matches {
                        break;
                    }
                }
            }
        }

        // Handle unclosed section
        if in_section && section.include_markers {
            result.extend(section_lines);
        }
    }

    if result.is_empty() {
        lines
    } else {
        result
    }
}

fn apply_replace_patterns(lines: Vec<String>, patterns: &[[String; 2]]) -> Vec<String> {
    // Compile once per pattern, not once per (pattern × line).
    let compiled: Vec<(Regex, &str)> = patterns
        .iter()
        .filter_map(|[pattern, replacement]| {
            cached_regex(pattern).map(|re| (re, replacement.as_str()))
        })
        .collect();
    if compiled.is_empty() {
        return lines;
    }
    lines
        .into_iter()
        .map(|mut line| {
            for (re, replacement) in &compiled {
                line = re.replace_all(&line, *replacement).to_string();
            }
            line
        })
        .collect()
}

fn apply_deduplicate_blocks(lines: Vec<String>, dedup: &DeduplicateBlocksDef) -> Vec<String> {
    let delimiter = dedup.block_delimiter.as_deref().unwrap_or(r"^\s*$");
    let delim_re = match cached_regex(delimiter) {
        Some(r) => r,
        None => return lines,
    };

    let mut blocks: Vec<Vec<String>> = Vec::new();
    let mut current_block = Vec::new();

    for line in &lines {
        if delim_re.is_match(line) && !current_block.is_empty() {
            if current_block.len() >= dedup.min_block_lines {
                blocks.push(current_block);
            }
            current_block = Vec::new();
        } else {
            current_block.push(line.clone());
        }
    }
    if !current_block.is_empty() && current_block.len() >= dedup.min_block_lines {
        blocks.push(current_block);
    }

    if blocks.len() < 2 {
        return lines;
    }

    let mut result = Vec::new();
    let mut i = 0;
    while i < blocks.len() {
        let block = &blocks[i];
        result.extend(block.iter().cloned());

        // Check next blocks for similarity
        let mut j = i + 1;
        let mut similar_count = 0;
        while j < blocks.len() {
            if blocks_similar(block, &blocks[j], dedup.similarity) {
                similar_count += 1;
                j += 1;
            } else {
                break;
            }
        }

        if similar_count > 0 {
            result.push(format!(
                "[... {} similar block(s) omitted ...]",
                similar_count
            ));
            i = j;
        } else {
            i += 1;
        }
    }

    result
}

fn blocks_similar(a: &[String], b: &[String], threshold: f32) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let matches = a.iter().zip(b.iter()).filter(|(x, y)| x == y).count();
    (matches as f32 / a.len() as f32) >= threshold
}

fn apply_semantic_filter(lines: Vec<String>, semantic: &SemanticFilterDef) -> Vec<String> {
    // Try to use real embeddings via daemon or in-process
    if let Ok(filtered) = apply_semantic_filter_with_embeddings(&lines, semantic) {
        return filtered;
    }

    // Fallback: keyword-based heuristic
    apply_semantic_filter_keyword_fallback(lines, semantic)
}

fn apply_semantic_filter_with_embeddings(
    lines: &[String],
    semantic: &SemanticFilterDef,
) -> Result<Vec<String>, anyhow::Error> {
    use crate::embed::{embed_query, set_active_model};

    // Set model if specified
    if let Some(model) = &semantic.model {
        if !crate::embed::is_known_model(model) {
            eprintln!(
                "[tokenix] warning: semantic_filter.model '{}' is unknown; falling back to '{}'",
                model,
                crate::embed::DEFAULT_MODEL_ID
            );
        }
        set_active_model(model);
    }

    // Embed the query
    let query_vec = embed_query(&semantic.query)?;

    // Embed each line (or small groups) and compute similarity
    let always_keep_patterns: Vec<Regex> = semantic
        .always_keep
        .iter()
        .filter_map(|p| cached_regex(p))
        .collect();

    let mut results = Vec::new();

    for line in lines {
        // Always keep lines matching always_keep patterns
        if always_keep_patterns.iter().any(|re| re.is_match(line)) {
            results.push(line.clone());
            continue;
        }

        // Skip very short lines
        if line.trim().len() < 5 {
            continue;
        }

        // Embed the line
        let line_vec = embed_query(line)?;

        // Compute cosine similarity
        let similarity = cosine_similarity(&query_vec, &line_vec);

        if similarity >= semantic.threshold {
            results.push(line.clone());
        }
    }

    Ok(results)
}

fn apply_semantic_filter_keyword_fallback(
    lines: Vec<String>,
    semantic: &SemanticFilterDef,
) -> Vec<String> {
    let query_terms: Vec<&str> = semantic.query.split_whitespace().collect();
    let always_keep_patterns: Vec<Regex> = semantic
        .always_keep
        .iter()
        .filter_map(|p| cached_regex(p))
        .collect();

    lines
        .into_iter()
        .filter(|line| {
            if always_keep_patterns.iter().any(|re| re.is_match(line)) {
                return true;
            }
            // Simple keyword overlap as proxy for semantic relevance
            let line_lower = line.to_lowercase();
            query_terms
                .iter()
                .any(|term| line_lower.contains(&term.to_lowercase()))
        })
        .collect()
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}

fn apply_summarize_json(lines: Vec<String>, summarize: &SummarizeJsonDef) -> Vec<String> {
    let content = lines.join("\n");
    let trimmed = content.trim();

    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return lines;
    }

    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return lines;
    };

    summarize_json_value(&mut value, summarize, 0);

    let result = serde_json::to_string_pretty(&value).unwrap_or(content);
    result.lines().map(|l| l.to_string()).collect()
}

fn summarize_json_value(value: &mut serde_json::Value, summarize: &SummarizeJsonDef, depth: usize) {
    if depth >= summarize.max_depth {
        return;
    }

    match value {
        serde_json::Value::Object(map) => {
            let keys_to_remove: Vec<String> =
                map.keys()
                    .filter(|k| {
                        let path = if depth == 0 { k.as_str() } else { "" };
                        summarize.exclude.iter().any(|ex| {
                            k.as_str() == ex.as_str() || (depth == 0 && path == ex.as_str())
                        })
                    })
                    .cloned()
                    .collect();
            for k in keys_to_remove {
                map.remove(&k);
            }

            for (k, v) in map.iter_mut() {
                let full_path = if depth == 0 {
                    k.clone()
                } else {
                    format!("{}.{}", depth, k)
                };
                if summarize
                    .always_include
                    .iter()
                    .any(|inc| inc == &full_path || inc == k)
                {
                    continue;
                }
                summarize_json_value(v, summarize, depth + 1);
            }
        }
        serde_json::Value::Array(arr) => {
            if arr.len() > summarize.max_array_items {
                let shown = arr.drain(summarize.max_array_items..).collect::<Vec<_>>();
                let count = shown.len();
                arr.push(serde_json::Value::String(format!(
                    "... {} more item(s) omitted ...",
                    count
                )));
            }
            for item in arr.iter_mut() {
                summarize_json_value(item, summarize, depth + 1);
            }
        }
        _ => {}
    }
}

fn apply_token_budget(text: &str, budget: usize) -> String {
    let tokens = crate::chunker::count_tokens(text);
    if tokens <= budget {
        return text.to_string();
    }

    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return text.to_string();
    }

    // Priority order: errors/warnings > head > tail > middle
    let mut priority_lines = Vec::new();
    let mut other_lines = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        let is_high_priority = t.starts_with("error")
            || t.starts_with("warning")
            || t.starts_with("FAIL")
            || t.starts_with("panic")
            || t.contains("error[")
            || t.contains("warning[")
            || i < lines.len() / 4
            || i >= lines.len() * 3 / 4;
        if is_high_priority {
            priority_lines.push((i, *line));
        } else {
            other_lines.push((i, *line));
        }
    }

    let mut result = Vec::new();
    let mut used = 0usize;

    for (_, line) in priority_lines {
        let line_tokens = crate::chunker::count_tokens(line);
        if used + line_tokens > budget {
            break;
        }
        result.push(line.to_string());
        used += line_tokens;
    }

    // Fill remaining budget with other lines (prefer head/tail)
    for (_, line) in other_lines {
        let line_tokens = crate::chunker::count_tokens(line);
        if used + line_tokens > budget {
            break;
        }
        result.push(line.to_string());
        used += line_tokens;
    }

    if result.len() < lines.len() {
        result.push(format!(
            "[... {} lines omitted to fit token budget {} ...]",
            lines.len() - result.len(),
            budget
        ));
    }

    result.join("\n")
}

/// Truncate `s` to at most `max_bytes`, backing off to the nearest char
/// boundary so we never slice through a multi-byte UTF-8 sequence (which would
/// panic). Returns a borrowed slice — no allocation.
fn truncate_at_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Keep only the first `max` lines per category; overflow collapses into one
/// count marker placed right after the category's last kept line. The first
/// matching cap owns a line, so overlapping patterns don't double-count.
fn apply_category_caps(lines: Vec<String>, caps: &[CategoryCap]) -> Vec<String> {
    let compiled: Vec<(Regex, usize)> = caps
        .iter()
        .filter_map(|c| cached_regex(&c.pattern).map(|re| (re, c.max)))
        .collect();
    if compiled.is_empty() {
        return lines;
    }
    let mut kept = vec![0usize; compiled.len()];
    let mut dropped = vec![0usize; compiled.len()];
    let mut marker_pos = vec![0usize; compiled.len()];
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    for line in lines {
        if let Some(ci) = compiled.iter().position(|(re, _)| re.is_match(&line)) {
            if kept[ci] < compiled[ci].1 {
                kept[ci] += 1;
                out.push(line);
                marker_pos[ci] = out.len();
            } else {
                dropped[ci] += 1;
            }
            continue;
        }
        out.push(line);
    }
    // Insert markers from the highest position down so earlier insertions
    // don't shift later ones.
    let mut markers: Vec<(usize, String)> = dropped
        .iter()
        .enumerate()
        .filter(|(_, d)| **d > 0)
        .map(|(ci, d)| {
            (
                marker_pos[ci],
                format!("[... {d} more similar line(s) omitted ...]"),
            )
        })
        .collect();
    markers.sort_by_key(|b| std::cmp::Reverse(b.0));
    for (pos, text) in markers {
        out.insert(pos.min(out.len()), text);
    }
    out
}

fn apply_sizing(lines: Vec<String>, f: &FilterDef) -> (Vec<String>, usize) {
    let original_len = lines.len();
    let n = lines.len();

    // Positional keep mask for the configured window.
    let mut keep = vec![true; n];
    let head_tail_combo = matches!((f.head_lines, f.tail_lines), (Some(h), Some(t)) if n > h + t);
    match (f.head_lines, f.tail_lines) {
        // head+tail together form a first+last window: header/context at the
        // top, verdict/summary at the bottom. The omitted middle gets an
        // explicit inline marker so the agent never sees a silent seam, and
        // is excluded from the returned count so notify_on_truncate does not
        // append a second, misplaced notice.
        (Some(head), Some(tail)) if n > head + tail => {
            for k in keep.iter_mut().take(n - tail).skip(head) {
                *k = false;
            }
        }
        // Both set but the output already fits the window — keep everything.
        (Some(_), Some(_)) => {}
        (Some(head), None) => {
            for k in keep.iter_mut().skip(head) {
                *k = false;
            }
        }
        (None, Some(tail)) => {
            for k in keep.iter_mut().take(n.saturating_sub(tail)) {
                *k = false;
            }
        }
        (None, None) => {
            if let Some(max) = f.max_lines {
                for k in keep.iter_mut().skip(max) {
                    *k = false;
                }
            }
        }
    }

    // Priority rescue: lines matching any priority regex survive the cut.
    if !f.priority_lines.is_empty() {
        let priority: Vec<Regex> = f
            .priority_lines
            .iter()
            .filter_map(|p| cached_regex(p))
            .collect();
        if !priority.is_empty() {
            for (i, line) in lines.iter().enumerate() {
                if !keep[i] && priority.iter().any(|re| re.is_match(line)) {
                    keep[i] = true;
                }
            }
        }
    }

    let dropped = keep.iter().filter(|k| !**k).count();
    if dropped == 0 {
        return (lines, 0);
    }

    let mut result: Vec<String> = Vec::with_capacity(n - dropped + 1);
    let mut marker_inserted = false;
    for (i, line) in lines.into_iter().enumerate() {
        if keep[i] {
            result.push(line);
        } else if head_tail_combo && !marker_inserted {
            result.push(format!("[... {dropped} lines omitted ...]"));
            marker_inserted = true;
        }
    }
    let omitted = if head_tail_combo {
        0
    } else {
        original_len - result.len()
    };
    (result, omitted)
}

/// Generate the TOML prompt to send to an AI CLI for filter creation.
pub fn build_filter_prompt(command: &str, sample_output: &str) -> String {
    format!(
        r#"Generate a tokenix TOML filter for the command `{command}`.

TOML filter schema (all fields optional except match_command):
```
[filters.<slug>]
description = "human-readable purpose"
match_command = "^regex_to_match_full_command_line"
strip_ansi = true          # remove ANSI color codes
strip_lines_matching = ["^pattern1", "^pattern2"]  # drop noisy lines
keep_lines_matching = ["error", "warning"]          # keep only signal lines
match_output = [           # short-circuit: if output matches pattern, return message
  {{ pattern = "already installed", message = "ok (already installed)" }},
  # optional `unless`: skip the short-circuit if output also matches it (avoids masking errors)
  {{ pattern = "Build complete!", message = "ok (build complete)", unless = "warning:|error:" }},
]
max_lines = 50             # truncate to N lines
head_lines = 30            # keep first N lines
tail_lines = 10            # keep last N lines
truncate_lines_at = 120    # truncate individual lines at N chars
on_empty = "command: ok"   # message when filter produces empty output

# ADVANCED (extended filtering capabilities):
replace_patterns = [       # regex replacements: [[pattern, replacement], ...]
  ["\\d+\\.\\d+s", "<duration>"],
  ["/home/[^/]+/", "~/"],
]
extract_sections = [       # extract content between markers
  {{ start_pattern = "---- FAILURES ----", end_pattern = "^\\s*$", include_markers = true, max_matches = 3 }},
]
semantic_filter = {{       # embedding-based relevance filtering (uses daemon/embed)
  query = "test failure error panic",
  threshold = 0.3,
  always_keep = ["^error\\[", "^FAIL"],
  model = "nomic-v1.5"
}}
deduplicate_blocks = {{    # structural block deduplication
  min_block_lines = 3,
  similarity = 0.8,
  block_delimiter = "^\\s*$"
}}
summarize_json = {{        # intelligent JSON summarization
  max_array_items = 10,
  max_depth = 3,
  always_include = ["packages", "workspace_members"],
  exclude = ["manifest", "dependencies"]
}}
token_budget = 2000        # hard token limit with smart truncation
```

Rules:
- Use strip_lines_matching to drop boilerplate (progress, verbose info)
- Use keep_lines_matching only if output has a clear signal/noise separation
- Use match_output for commands that succeed silently or with a predictable summary line
- Set on_empty when the command normally succeeds silently
- Use replace_patterns to normalize paths, timestamps, IDs, etc.
- Use extract_sections to pull out failure blocks, error sections, etc.
- Use semantic_filter for query-aware relevance (requires embed model)
- Use deduplicate_blocks for repetitive output (test runs, build steps)
- Use summarize_json for large JSON (cargo metadata, API responses)
- Use token_budget as a hard cap with priority-based truncation
- match_command must be a valid Rust regex matching `{command}` or its typical invocations
- Return ONLY valid TOML, no markdown code fences, no explanations

Sample output from `{command} --help` (or similar):
---
{sample_output}
---

TOML filter:"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_local_filters() {
        let temp_dir = std::env::current_dir()
            .unwrap()
            .join(".tokenix")
            .join("filters");
        std::fs::create_dir_all(&temp_dir).unwrap();
        let toml_path = temp_dir.join("test_local_cmd.toml");
        std::fs::write(
            &toml_path,
            r#"
[filters.test_local_cmd]
description = "test local"
match_command = "^test_local_cmd$"
on_empty = "empty filter output"
"#,
        )
        .unwrap();

        // Trust gate: repo-local filters are skipped until `tokenix trust`.
        let root = crate::store::find_project_root(&std::env::current_dir().unwrap());
        let _ = set_local_filters_trust(&root, false);
        assert!(
            load_local_filters().is_empty(),
            "untrusted repo-local filters must not load"
        );

        set_local_filters_trust(&root, true).unwrap();
        let local_filters = load_local_filters();
        assert!(!local_filters.is_empty());
        let found = find_filter("test_local_cmd", &local_filters);
        assert!(found.is_some());
        let filter = found.unwrap();
        assert_eq!(filter.on_empty.as_deref(), Some("empty filter output"));

        // Editing a trusted file revokes trust (hash mismatch).
        std::fs::write(
            &toml_path,
            "[filters.test_local_cmd]\nmatch_command = \"^x$\"\n",
        )
        .unwrap();
        assert!(
            load_local_filters().is_empty(),
            "edited filter file must fail the trust check until re-trusted"
        );

        // Clean up
        let _ = set_local_filters_trust(&root, false);
        let _ = std::fs::remove_file(&toml_path);
        let _ = std::fs::remove_dir_all(
            std::env::current_dir()
                .unwrap()
                .join(".tokenix")
                .join("filters"),
        );
    }

    #[test]
    fn test_tokenize_command() {
        assert_eq!(tokenize_command("cargo test"), vec!["cargo", "test"]);
        assert_eq!(
            tokenize_command("echo \"hello world\""),
            vec!["echo", "hello world"]
        );
        assert_eq!(
            tokenize_command("env CI=true cargo test"),
            vec!["env", "CI=true", "cargo", "test"]
        );
    }

    #[test]
    fn test_unwrap_shell_runner() {
        assert_eq!(
            unwrap_shell_runner("bash -c 'cargo test'"),
            Some("cargo test".to_string())
        );
        assert_eq!(
            unwrap_shell_runner("powershell -Command \"cargo test\""),
            Some("cargo test".to_string())
        );
        assert_eq!(
            unwrap_shell_runner("cmd.exe /c \"cargo test\""),
            Some("cargo test".to_string())
        );
        assert_eq!(unwrap_shell_runner("cargo test"), None);
    }

    #[test]
    fn test_get_effective_command() {
        assert_eq!(
            get_effective_command("cd /app && CI=true cargo test"),
            "cargo test"
        );
        assert_eq!(
            get_effective_command("bash -c 'cd /app && CI=true env cargo test'"),
            "cargo test"
        );
        assert_eq!(
            get_effective_command("env CI=true cargo test"),
            "cargo test"
        );
        // timeout wrapper
        assert_eq!(
            get_effective_command("timeout 180 pnpm run test"),
            "pnpm run test"
        );
        assert_eq!(
            get_effective_command("timeout -k 10 180 pnpm run test"),
            "pnpm run test"
        );
        assert_eq!(
            get_effective_command("timeout --foreground 60s cargo test --quiet"),
            "cargo test --quiet"
        );
        // time wrapper
        assert_eq!(
            get_effective_command("time pnpm run build"),
            "pnpm run build"
        );
        // nice wrapper
        assert_eq!(get_effective_command("nice -n 10 make all"), "make all");
        // stacked: timeout + nice
        assert_eq!(
            get_effective_command("timeout 30 nice -n 5 pnpm run test"),
            "pnpm run test"
        );
        // pnpm / bun workspaces filters
        assert_eq!(
            get_effective_command("pnpm --filter @mika/desktop test"),
            "pnpm test"
        );
        assert_eq!(
            get_effective_command("bun --cwd /app run build"),
            "bun run build"
        );
    }

    #[test]
    fn test_derive_command_candidates() {
        let cmd = "bash -c 'cd /app && cargo test'";
        let candidates = derive_command_candidates(cmd);
        assert!(candidates.contains(&"bash -c 'cd /app && cargo test'".to_string()));
        assert!(candidates.contains(&"cd /app && cargo test".to_string()));
        assert!(candidates.contains(&"cargo test".to_string()));
    }

    #[test]
    fn scanned_anchor_never_overshoots_parsed_pattern() {
        // The lazy loader may only ever *under*-constrain: a scanned anchor must
        // be a prefix of the anchor the fully parsed pattern would produce, or
        // the file must be marked unprefilterable.
        for (name, anchors) in bundled_anchor_index() {
            let Some(anchors) = anchors else { continue };
            let file = BundledFilters::get(name).expect("embedded file");
            let content = std::str::from_utf8(file.data.as_ref()).expect("utf8");
            let parsed = parse_filter_file_named(content);
            assert_eq!(
                parsed.len(),
                anchors.len(),
                "{name}: scan found {} anchors for {} filters",
                anchors.len(),
                parsed.len()
            );
            for (_, def) in &parsed {
                let real = prefilter_for(&def.match_command);
                assert!(
                    real.as_ref().is_some_and(|r| anchors.contains(r)),
                    "{name}: scan lost the prefilter for {:?}",
                    def.match_command
                );
            }
        }
    }

    /// Command corpus used to homologate the prefilter: it must never reject a
    /// candidate the compiled regex would have matched.
    fn prefilter_probe_commands() -> Vec<String> {
        let mut cmds: Vec<String> = [
            "cargo test --quiet",
            "cargo build --release",
            "npm install",
            "npx eslint src/ --fix",
            "pnpm run build",
            "yarn dlx prettier --write .",
            "uv run pytest -x",
            "python -m ruff check .",
            "git status",
            "git -C /repo log --oneline",
            "docker compose up -d",
            "docker build -t app .",
            "kubectl -n prod get pods",
            "terraform plan",
            "Get-Content big.log",
            "Get-CimInstance Win32_Process",
            "sudo apt-get install curl",
            "sudo apk add curl",
            "bash -c 'cd /app && actionlint'",
            "npx cypress run --headless",
            "timeout 300 bats tests/",
            "gradlew build",
            "./mvnw -q verify",
            "cd web && npx astro build",
            "zzz_nonexistent --foo",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        // Every bundled filter contributes its own literal as a probe, so a
        // pattern shape nobody wrote a scenario for is still covered.
        for f in load_bundled_filters() {
            match prefilter_for(&f.match_command) {
                Some(Prefilter::StartsWith(lit)) => cmds.push(format!("{lit} --flag arg")),
                Some(Prefilter::Contains(lit, _)) => cmds.push(format!("npx {lit} --flag arg")),
                None => {}
            }
        }
        // Real invocations recorded in the corpus: these are the exact strings
        // the golden suite asserts `match_command` matches, so they are the
        // strongest available evidence that the prefilter admits real matches.
        for asset in BundledFilters::iter() {
            let Some(file) = BundledFilters::get(&asset) else {
                continue;
            };
            let Ok(content) = std::str::from_utf8(file.data.as_ref()) else {
                continue;
            };
            let Ok(parsed) = toml::from_str::<FilterTestFile>(content) else {
                continue;
            };
            for cases in parsed.tests.values() {
                cmds.extend(cases.iter().filter_map(|c| c.command.clone()));
            }
        }
        cmds
    }

    #[test]
    fn prefilter_never_rejects_a_real_match() {
        // The whole optimization rests on this: the prefilter is a NECESSARY
        // condition, so `regex matches` must imply `prefilter allows`.
        let commands = prefilter_probe_commands();
        let mut checked = 0usize;
        for f in load_bundled_filters()
            .iter()
            .chain(load_user_filters().iter())
        {
            let Some(pf) = prefilter_for(&f.match_command) else {
                continue;
            };
            let Some(re) = cached_regex(&f.match_command) else {
                continue;
            };
            for cmd in &commands {
                if re.is_match(cmd) {
                    assert!(
                        pf.allows(cmd),
                        "prefilter {pf:?} rejected {cmd:?} which {:?} matches",
                        f.match_command
                    );
                }
                checked += 1;
            }
        }
        assert!(checked > 100_000, "corpus too small: {checked} pairs");
    }

    #[test]
    fn head_slice_never_drops_a_filter_declared_after_a_fake_tests_marker() {
        // The head slice cuts at the first line starting with `[[tests`. If
        // that line lives inside a multi-line value, the cut lands before a
        // later `[filters.x]` table. What makes this safe is not a guard but
        // TOML itself: the cut always leaves the string unterminated, so the
        // head fails to parse and the full-file fallback runs. This test pins
        // that reasoning down — if the fast path ever starts accepting a
        // truncated head, it fails here instead of silently losing a filter.
        let content = "\
[filters.first]
match_command = \"^first\\\\b\"
on_empty = \"\"\"
sample text
[[tests.not-really]] this line is inside a string
\"\"\"

[filters.second]
match_command = \"^second\\\\b\"

[[tests.first]]
input = \"\"
expected = \"\"
";
        let parsed = parse_filter_file_named(content);
        let names: Vec<&str> = parsed.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            parsed.len(),
            2,
            "head slice dropped a filter; got {names:?}"
        );
    }

    #[test]
    fn user_filter_index_invalidates_on_same_length_edit() {
        // Regression: the index stamp was seconds + length. Fixing a pattern
        // without changing its byte length (`^alpha` -> `^bravo`) inside the
        // same second left the stamp identical, so the hook kept prefiltering
        // by the OLD pattern and the edited filter silently stopped matching.
        let dir = std::env::temp_dir().join(format!("tokenix-idx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join("probe.toml");
        let toml_for = |cmd: &str| {
            format!("[filters.probe]\nmatch_command = \"^{cmd}\\\\b\"\nmax_lines = 10\n")
        };

        std::fs::write(&file, toml_for("alpha")).expect("write");
        let first = user_filter_index(&dir);
        let stamp_a = user_filter_stats(&dir)[0].1;

        // Same byte length, a realistic edit interval later — the case a
        // seconds-resolution stamp cannot see. The sleep clears the platform's
        // file-time tick (Windows updates file times on the ~15 ms system
        // clock tick, so sub-tick writes share a stamp on any resolution).
        std::thread::sleep(std::time::Duration::from_millis(25));
        std::fs::write(&file, toml_for("bravo")).expect("rewrite");
        let stamp_b = user_filter_stats(&dir)[0].1;
        let second = user_filter_index(&dir);
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(
            first[0].prefilters,
            Some(vec![Prefilter::StartsWith("alpha".into())])
        );
        // The whole cache rests on this: the stamp MUST distinguish two
        // consecutive same-length writes, or a filter edit is invisible to
        // every later hook. A failure here means the filesystem's timestamp
        // resolution is too coarse to validate the index by metadata.
        assert_ne!(
            stamp_a, stamp_b,
            "mtime stamp cannot distinguish two same-length writes"
        );
        assert_eq!(
            second[0].prefilters,
            Some(vec![Prefilter::StartsWith("bravo".into())]),
            "index served a stale prefilter after a same-length edit"
        );
    }

    #[test]
    fn piped_select_string_still_resolves_its_filter() {
        // `^Select-String\b|\|\s*Select-String\b` has a top-level alternation,
        // so only the FIRST branch is anchored. The leading-literal prefilter
        // used to demand that the candidate start with "Select-String", which
        // rejects the piped form the second branch exists for.
        //
        // In practice `split_on_operators` saved it: the segment candidate
        // `Select-String -Pattern ERROR` matches branch 1 on its own. So this
        // was a violated prefilter contract ("may only skip work, never change
        // results"), one candidate-derivation change away from becoming a real
        // miss — not a user-visible bug. This test pins the end-to-end shape;
        // `top_level_alternation_detection` pins the contract itself.
        let filters = load_bundled_filters();
        let cmd = "Get-Content log.txt | Select-String -Pattern ERROR";
        assert!(
            find_filter(cmd, &filters).is_some(),
            "piped Select-String must resolve a filter"
        );
    }

    #[test]
    fn top_level_alternation_detection() {
        assert!(has_top_level_alternation(r"^a\b|\|\s*a\b"));
        assert!(has_top_level_alternation("foo|bar"));
        // Alternation contained in a group is fine — the group is mandatory,
        // so the surrounding literal (if any) still applies.
        assert!(!has_top_level_alternation(r"^git\s+(add|rm)\b"));
        // `\|` is a literal pipe, `[|]` is a class member.
        assert!(!has_top_level_alternation(r"^grep\s+\|"));
        assert!(!has_top_level_alternation(r"^grep[|]x"));
    }

    #[test]
    fn uniform_success_is_a_claim_about_the_run_not_one_line() {
        // The reason `match_output` cannot express this: it fires on ANY
        // matching line, so a run where item 3 failed still reports success.
        let cfg = UniformSuccess {
            pattern: "^PASS - ".to_string(),
            message: "conftest: {n} policies passed".to_string(),
            ignore_lines: vec!["^Summary".to_string()],
        };

        let all_pass = "PASS - a.yaml - main\nPASS - b.yaml - main\nPASS - c.yaml - main\n";
        assert_eq!(
            uniform_summary(all_pass, &cfg).as_deref(),
            Some("conftest: 3 policies passed"),
            "a uniform run must collapse, with the real item count"
        );

        // One dissenting line is enough to disqualify the whole run.
        let mixed = "PASS - a.yaml - main\nFAIL - b.yaml - missing label\nPASS - c.yaml - main\n";
        assert_eq!(
            uniform_summary(mixed, &cfg),
            None,
            "a failed item must never be summarized as success"
        );

        // Unrecognized output is not a clean run either — this is what the 40
        // absence-based sentinels used to get wrong.
        assert_eq!(uniform_summary("some other tool output\n", &cfg), None);
        // Nothing to summarize is not success.
        assert_eq!(uniform_summary("\n  \n", &cfg), None);
        // Ignored lines don't count as items, but don't disqualify either.
        assert_eq!(
            uniform_summary("Summary: 1 checks\nPASS - a.yaml - main\n", &cfg).as_deref(),
            Some("conftest: 1 policies passed")
        );
    }

    #[test]
    fn prefilter_shapes() {
        assert_eq!(
            prefilter_for(r"^cargo\s+test"),
            Some(Prefilter::StartsWith("cargo".into()))
        );
        assert_eq!(
            prefilter_for(r"\bactionlint\b"),
            Some(Prefilter::Contains("actionlint".into(), false))
        );
        assert_eq!(
            prefilter_for(r"^(npx\s+)?eslint\b"),
            Some(Prefilter::Contains("eslint".into(), false))
        );
        assert_eq!(
            prefilter_for(r"(?i)^(get-ciminstance|gcim)\b"),
            None,
            "mandatory alternation cannot be reduced to one literal"
        );
        assert_eq!(
            prefilter_for(r"^(?:sudo\s+)?apk\s+(add|del)\b"),
            Some(Prefilter::Contains("apk".into(), false))
        );
        // `.` and `+` are metacharacters: the run must stop before them.
        assert_eq!(
            prefilter_for("^go.mod"),
            Some(Prefilter::StartsWith("go".into()))
        );
        // A quantified last character is optional and must be dropped.
        assert_eq!(
            prefilter_for(r"\bmake?\b"),
            Some(Prefilter::Contains("mak".into(), false))
        );
        // Too little literal left to be worth a scan.
        assert_eq!(prefilter_for(r"\bgo?\b"), None);
    }

    #[test]
    fn lazy_load_matches_full_load_for_commands() {
        let all = load_all_filters();
        let commands = [
            "cargo test --quiet",
            "npm install",
            "git status",
            "docker compose up -d",
            "cd /app && pnpm run build",
            "bash -c 'uv run pytest -q'",
            "kubectl -n prod get pods",
            "Get-Content big.log",
            "zzz_nonexistent --foo",
            "terraform plan",
        ];
        for cmd in commands.iter().map(|s| s.to_string()).chain(
            // Reuse the wide probe corpus so the lazy loader is homologated
            // against every bundled filter's own literal, not just scenarios.
            prefilter_probe_commands(),
        ) {
            let lazy = load_filters_for_command(&cmd);
            let full = find_filter(&cmd, &all).map(|f| f.match_command.clone());
            let subset = find_filter(&cmd, &lazy).map(|f| f.match_command.clone());
            assert_eq!(full, subset, "lazy load diverged for {cmd:?}");
        }
    }

    #[test]
    fn timeout_wrapper_included_in_candidates() {
        // Reported: `timeout 180 pnpm run test` must produce `pnpm run test`
        // as a candidate so filters keyed on `pnpm run test` match.
        let candidates = derive_command_candidates("timeout 180 pnpm run test");
        assert!(
            candidates.contains(&"pnpm run test".to_string()),
            "candidates: {candidates:?}"
        );
    }

    #[test]
    fn truncate_at_char_boundary_handles_multibyte() {
        // ASCII: exact byte cut
        assert_eq!(truncate_at_char_boundary("hello world", 5), "hello");
        // Shorter than limit: unchanged
        assert_eq!(truncate_at_char_boundary("hi", 10), "hi");
        // Multibyte: 'é' is 2 bytes — cutting at byte 4 lands mid-char, must back off
        let s = "café latte"; // 'é' occupies bytes 3..5
        let out = truncate_at_char_boundary(s, 4);
        assert!(s.starts_with(out));
        assert_eq!(out, "caf"); // backed off to char boundary, no panic
    }

    #[test]
    fn apply_filter_truncate_lines_at_no_panic_on_utf8() {
        let f = FilterDef {
            description: None,
            match_command: ".*".to_string(),
            skip_when_matches: None,
            notify_on_truncate: false,
            strip_ansi: false,
            strip_lines_matching: vec![],
            keep_lines_matching: vec![],
            max_lines: None,
            head_lines: None,
            tail_lines: None,
            on_empty: None,
            passthrough_when_emptied: false,
            match_output: vec![],
            truncate_lines_at: Some(4),
            uniform_success: None,
            filter_stderr: false,
            replace_patterns: vec![],
            extract_sections: vec![],
            semantic_filter: None,
            deduplicate_blocks: None,
            summarize_json: None,
            token_budget: None,
            on_failure: None,
            priority_lines: vec![],
            category_caps: vec![],
        };
        // Would panic with naive &l[..4] because 'é'/'ç' straddle the boundary.
        let out = apply_filter("café\nação\n", &f);
        assert_eq!(out, "caf\naç");
    }

    #[test]
    fn semantic_filter_issues_flags_unknown_model_and_bad_threshold() {
        let mut f = FilterDef {
            description: None,
            match_command: ".*".to_string(),
            skip_when_matches: None,
            notify_on_truncate: false,
            strip_ansi: false,
            strip_lines_matching: vec![],
            keep_lines_matching: vec![],
            max_lines: None,
            head_lines: None,
            tail_lines: None,
            on_empty: None,
            passthrough_when_emptied: false,
            match_output: vec![],
            truncate_lines_at: None,
            uniform_success: None,
            filter_stderr: false,
            replace_patterns: vec![],
            extract_sections: vec![],
            semantic_filter: Some(SemanticFilterDef {
                query: "errors".to_string(),
                threshold: 1.5,
                always_keep: vec![],
                model: Some("does-not-exist".to_string()),
            }),
            deduplicate_blocks: None,
            summarize_json: None,
            token_budget: None,
            on_failure: None,
            priority_lines: vec![],
            category_caps: vec![],
        };
        let issues = semantic_filter_issues(&f);
        assert_eq!(
            issues.len(),
            2,
            "expected model + threshold issues: {issues:?}"
        );

        let sem = f.semantic_filter.as_mut().unwrap();
        sem.model = Some("nomic-v1.5".to_string());
        sem.threshold = 0.3;
        assert!(semantic_filter_issues(&f).is_empty());
    }

    #[test]
    fn split_on_operators_handles_compound_commands() {
        // Spaced and unspaced operators segment identically.
        assert_eq!(
            split_on_operators("cd foo && gitleaks detect"),
            vec!["cd foo", "gitleaks detect"]
        );
        assert_eq!(
            split_on_operators("cd foo;gitleaks"),
            vec!["cd foo", "gitleaks"]
        );
        assert_eq!(split_on_operators("a || b"), vec!["a", "b"]);
        assert_eq!(
            split_on_operators("producer | gitleaks detect"),
            vec!["producer", "gitleaks detect"]
        );
        // Quoted operators are not split points.
        assert_eq!(
            split_on_operators(r#"echo "a;b" && x"#),
            vec![r#"echo "a;b""#, "x"]
        );
    }

    #[test]
    fn derive_candidates_segments_compound_commands() {
        let candidates = derive_command_candidates("cd foo;gitleaks detect --source .");
        assert!(
            candidates.iter().any(|c| c == "gitleaks detect --source ."),
            "expected a gitleaks segment candidate, got: {candidates:?}"
        );
    }

    #[test]
    fn find_filter_matches_command_after_cd_and_pipe() {
        let f = FilterDef {
            description: None,
            match_command: "^gitleaks\\b".to_string(),
            skip_when_matches: None,
            notify_on_truncate: false,
            strip_ansi: false,
            strip_lines_matching: vec![],
            keep_lines_matching: vec![],
            max_lines: None,
            head_lines: None,
            tail_lines: None,
            on_empty: None,
            passthrough_when_emptied: false,
            match_output: vec![],
            truncate_lines_at: None,
            uniform_success: None,
            filter_stderr: false,
            replace_patterns: vec![],
            extract_sections: vec![],
            semantic_filter: None,
            deduplicate_blocks: None,
            summarize_json: None,
            token_budget: None,
            on_failure: None,
            priority_lines: vec![],
            category_caps: vec![],
        };
        let filters = [f];
        // Unspaced semicolon, cd prefix, and a pipe all resolve to the filter.
        assert!(find_filter("cd repo;gitleaks detect", &filters).is_some());
        assert!(find_filter("npm i && gitleaks detect", &filters).is_some());
        assert!(find_filter("cat x | gitleaks detect", &filters).is_some());
        // A bare argument named gitleaks must NOT match (anchored base command).
        assert!(find_filter("echo gitleaks", &filters).is_none());

        // Test pipeline segment prioritization: B takes priority over A in "A | B"
        let f_cat = FilterDef {
            description: None,
            match_command: "^cat\\b".to_string(),
            skip_when_matches: None,
            notify_on_truncate: false,
            strip_ansi: false,
            strip_lines_matching: vec![],
            keep_lines_matching: vec![],
            max_lines: None,
            head_lines: None,
            tail_lines: None,
            on_empty: None,
            passthrough_when_emptied: false,
            match_output: vec![],
            truncate_lines_at: None,
            uniform_success: None,
            filter_stderr: false,
            replace_patterns: vec![],
            extract_sections: vec![],
            semantic_filter: None,
            deduplicate_blocks: None,
            summarize_json: None,
            token_budget: None,
            on_failure: None,
            priority_lines: vec![],
            category_caps: vec![],
        };
        let f_gitleaks = FilterDef {
            description: None,
            match_command: "^gitleaks\\b".to_string(),
            skip_when_matches: None,
            notify_on_truncate: false,
            strip_ansi: false,
            strip_lines_matching: vec![],
            keep_lines_matching: vec![],
            max_lines: None,
            head_lines: None,
            tail_lines: None,
            on_empty: None,
            passthrough_when_emptied: false,
            match_output: vec![],
            truncate_lines_at: None,
            uniform_success: None,
            filter_stderr: false,
            replace_patterns: vec![],
            extract_sections: vec![],
            semantic_filter: None,
            deduplicate_blocks: None,
            summarize_json: None,
            token_budget: None,
            on_failure: None,
            priority_lines: vec![],
            category_caps: vec![],
        };
        let filters2 = [f_cat, f_gitleaks];
        let matched = find_filter("cat x | gitleaks detect", &filters2).unwrap();
        assert_eq!(matched.match_command, "^gitleaks\\b");
    }

    #[test]
    fn skip_when_matches_bypasses_already_bounded_reads() {
        let f = FilterDef {
            description: None,
            match_command: "^(Get-Content|gc)\\b".to_string(),
            skip_when_matches: Some("(?i)-Tail\\b|-TotalCount\\b|-Head\\b|-Raw\\b".to_string()),
            notify_on_truncate: false,
            strip_ansi: false,
            strip_lines_matching: vec![],
            keep_lines_matching: vec![],
            max_lines: None,
            head_lines: None,
            tail_lines: None,
            on_empty: None,
            passthrough_when_emptied: false,
            match_output: vec![],
            truncate_lines_at: None,
            uniform_success: None,
            filter_stderr: false,
            replace_patterns: vec![],
            extract_sections: vec![],
            semantic_filter: None,
            deduplicate_blocks: None,
            summarize_json: None,
            token_budget: None,
            on_failure: None,
            priority_lines: vec![],
            category_caps: vec![],
        };
        let filters = [f];
        // Unbounded full dump: filter applies.
        assert!(find_filter("Get-Content large.log", &filters).is_some());
        // Agent already sliced its own read: filter steps aside.
        assert!(find_filter("Get-Content large.log -Tail 20", &filters).is_none());
        assert!(find_filter("Get-Content large.log -TotalCount 50", &filters).is_none());
    }

    #[test]
    fn strip_subcommand_global_opts_normalizes_tool_globals() {
        let eff = |c: &str| get_effective_command(c);
        // git global options before the subcommand are peeled away.
        assert_eq!(eff("git -C /repo add ."), "git add .");
        assert_eq!(eff("git -c user.name=x commit -m hi"), "git commit -m hi");
        assert_eq!(eff("git --git-dir=/r/.git -C /r status"), "git status");
        assert_eq!(eff("git --no-pager log --oneline"), "git log --oneline");
        // kubectl / docker / cargo share the same global-option bug class.
        assert_eq!(eff("kubectl -n prod get pods"), "kubectl get pods");
        assert_eq!(eff("docker -H tcp://h ps -a"), "docker ps -a");
        assert_eq!(eff("cargo +nightly test"), "cargo test");
        // terraform / tofu `-chdir=DIR` global flag precedes the subcommand.
        assert_eq!(eff("terraform -chdir=./infra apply"), "terraform apply");
        assert_eq!(eff("terraform -chdir ./infra apply"), "terraform apply");
        assert_eq!(
            eff("terraform -chdir=/a/b plan -out=p"),
            "terraform plan -out=p"
        );
        assert_eq!(eff("tofu -chdir=. init"), "tofu init");
        assert_eq!(eff("kubectl -n=prod get pods"), "kubectl get pods");
        // Subcommand-less or unknown tools are untouched.
        assert_eq!(eff("git status"), "git status");
        assert_eq!(eff("ls -la"), "ls -la");
        // Trailing valued options do not panic.
        assert_eq!(eff("git -C"), "git");
        assert_eq!(eff("git --git-dir"), "git");
    }

    #[test]
    fn strip_package_runner_exposes_inner_tool() {
        let eff = |c: &str| get_effective_command(c);
        assert_eq!(eff("uv run pytest tests/"), "pytest tests/");
        assert_eq!(eff("python -m ruff check ."), "ruff check .");
        assert_eq!(eff("python3 -m pytest"), "pytest");
        assert_eq!(eff("bunx biome check src"), "biome check src");
        assert_eq!(eff("npx tsc --noEmit"), "tsc --noEmit");
        assert_eq!(eff("pnpm exec eslint ."), "eslint .");
        assert_eq!(eff("pnpm dlx prettier -w ."), "prettier -w .");
        // Bare `pnpm build` is a script, not a runner — left untouched.
        assert_eq!(eff("pnpm build"), "pnpm build");
        // Composes with global-opt stripping: `uv run` then nothing to strip.
        assert_eq!(eff("npx kubectl -n ns get pods"), "kubectl get pods");
    }

    #[test]
    fn find_filter_matches_git_with_global_options() {
        let f = FilterDef {
            description: None,
            match_command: "^git\\s+add\\b".to_string(),
            skip_when_matches: None,
            notify_on_truncate: false,
            strip_ansi: false,
            strip_lines_matching: vec![],
            keep_lines_matching: vec![],
            max_lines: None,
            head_lines: None,
            tail_lines: None,
            on_empty: None,
            passthrough_when_emptied: false,
            match_output: vec![],
            truncate_lines_at: None,
            uniform_success: None,
            filter_stderr: false,
            replace_patterns: vec![],
            extract_sections: vec![],
            semantic_filter: None,
            deduplicate_blocks: None,
            summarize_json: None,
            token_budget: None,
            on_failure: None,
            priority_lines: vec![],
            category_caps: vec![],
        };
        let filters = [f];
        assert!(find_filter("git -C /repo add .", &filters).is_some());
        assert!(find_filter("cd x && git -c k=v add -A", &filters).is_some());
    }

    #[test]
    fn bundled_filters_route_new_targets() {
        let filters = load_bundled_filters();
        let desc = |cmd: &str| {
            find_filter(cmd, &filters)
                .and_then(|f| f.description.clone())
                .unwrap_or_default()
                .to_lowercase()
        };
        // real-world command forms (global flags / cd prefix) must reach the new filters
        assert!(
            desc("git -C /repo add .").contains("line-ending"),
            "git add"
        );
        assert!(
            desc("cd repo && git commit -m x").contains("file-mode"),
            "git commit"
        );
        assert!(
            desc("pnpm --filter @x/y remove hono").contains("remove"),
            "pnpm remove"
        );
        assert!(
            desc("Get-CimInstance Win32_Service").contains("ciminstance"),
            "get-ciminstance"
        );
    }

    #[test]
    fn apply_filter_match_output_unless_guards_errors() {
        let f = FilterDef {
            description: None,
            match_command: ".*".to_string(),
            skip_when_matches: None,
            notify_on_truncate: false,
            strip_ansi: false,
            strip_lines_matching: vec![],
            keep_lines_matching: vec![],
            max_lines: None,
            head_lines: None,
            tail_lines: None,
            on_empty: None,
            passthrough_when_emptied: false,
            match_output: vec![MatchOutput {
                pattern: "total size is".to_string(),
                message: "ok (synced)".to_string(),
                unless: Some("error|failed".to_string()),
            }],
            truncate_lines_at: None,
            uniform_success: None,
            filter_stderr: false,
            replace_patterns: vec![],
            extract_sections: vec![],
            semantic_filter: None,
            deduplicate_blocks: None,
            summarize_json: None,
            token_budget: None,
            on_failure: None,
            priority_lines: vec![],
            category_caps: vec![],
        };
        // Pattern present, no error → short-circuit to message
        assert_eq!(apply_filter("total size is 100\n", &f), "ok (synced)");
        // Pattern present AND error present → unless guard blocks short-circuit
        let out = apply_filter("rsync error\ntotal size is 100\n", &f);
        assert!(out.contains("error"), "error must not be masked: {out:?}");
    }

    /// Minimal `FilterDef` with everything off — scenario tests flip only the
    /// one field under test instead of repeating the full struct literal.
    fn base_filter() -> FilterDef {
        FilterDef {
            description: None,
            match_command: ".*".to_string(),
            skip_when_matches: None,
            notify_on_truncate: false,
            strip_ansi: false,
            strip_lines_matching: vec![],
            keep_lines_matching: vec![],
            max_lines: None,
            head_lines: None,
            tail_lines: None,
            on_empty: None,
            passthrough_when_emptied: false,
            match_output: vec![],
            truncate_lines_at: None,
            uniform_success: None,
            filter_stderr: false,
            replace_patterns: vec![],
            extract_sections: vec![],
            semantic_filter: None,
            deduplicate_blocks: None,
            summarize_json: None,
            token_budget: None,
            on_failure: None,
            priority_lines: vec![],
            category_caps: vec![],
        }
    }

    #[test]
    fn apply_filter_strip_ansi_removes_color_codes() {
        let mut f = base_filter();
        f.strip_ansi = true;
        assert_eq!(
            apply_filter("\x1b[31merror\x1b[0m here\n", &f),
            "error here"
        );
    }

    #[test]
    fn apply_filter_strip_lines_matching_drops_noise() {
        let mut f = base_filter();
        f.strip_lines_matching = vec!["^DEBUG".to_string(), "^\\s*$".to_string()];
        let out = apply_filter("DEBUG init\nreal line\n\nDEBUG done\nkeep me\n", &f);
        assert_eq!(out, "real line\nkeep me");
    }

    #[test]
    fn apply_filter_keep_lines_matching_keeps_signal() {
        let mut f = base_filter();
        f.keep_lines_matching = vec!["warn|error".to_string()];
        let out = apply_filter("info: starting\nwarn: low disk\nok\nerror: boom\n", &f);
        assert_eq!(out, "warn: low disk\nerror: boom");
    }

    #[test]
    fn apply_filter_sizing_head_tail_max() {
        let input = "l1\nl2\nl3\nl4\nl5\n";
        let mut head = base_filter();
        head.head_lines = Some(2);
        assert_eq!(apply_filter(input, &head), "l1\nl2");

        let mut tail = base_filter();
        tail.tail_lines = Some(2);
        assert_eq!(apply_filter(input, &tail), "l4\nl5");

        let mut max = base_filter();
        max.max_lines = Some(3);
        assert_eq!(apply_filter(input, &max), "l1\nl2\nl3");

        // head+tail combine into a first+last window with an explicit middle
        // marker (rtk parity): header/context on top, verdict at the bottom.
        let long_input = (1..=40)
            .map(|i| format!("line number {i} with some payload text"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut both = base_filter();
        both.head_lines = Some(2);
        both.tail_lines = Some(2);
        let out = apply_filter(&long_input, &both);
        assert_eq!(
            out,
            "line number 1 with some payload text\n\
             line number 2 with some payload text\n\
             [... 36 lines omitted ...]\n\
             line number 39 with some payload text\n\
             line number 40 with some payload text"
        );

        // The middle marker replaces the notify_on_truncate end notice — no
        // double marker for the same cut.
        let mut both_notify = base_filter();
        both_notify.head_lines = Some(2);
        both_notify.tail_lines = Some(2);
        both_notify.notify_on_truncate = true;
        assert_eq!(apply_filter(&long_input, &both_notify), out);
    }

    #[test]
    fn apply_filter_never_worse_than_raw() {
        // A success message longer than the raw output reverts to the raw:
        // filtering must never cost more tokens than not filtering.
        let mut f = base_filter();
        f.match_output = vec![MatchOutput {
            pattern: "ok".to_string(),
            message: "everything completed successfully with no findings".to_string(),
            unless: None,
        }];
        assert_eq!(apply_filter("ok\n", &f), "ok");

        // Same guard for truncation notices on tiny outputs.
        let mut noisy = base_filter();
        noisy.max_lines = Some(1);
        noisy.notify_on_truncate = true;
        assert_eq!(apply_filter("a\nb\n", &noisy), "a\nb");

        // Genuinely empty raw is exempt so on_empty sentinels still fire.
        let mut empty = base_filter();
        empty.on_empty = Some("cmd: ok".to_string());
        assert_eq!(apply_filter("", &empty), "cmd: ok");
    }

    #[test]
    fn apply_filter_with_exit_failure_policies() {
        // on_failure = "passthrough": nonzero exit emits raw untouched.
        let mut f = base_filter();
        f.keep_lines_matching = vec!["^KEEP".to_string()];
        f.on_failure = Some("passthrough".to_string());
        let raw = "some context line\nanother detail line\nKEEP this one\n";
        assert_eq!(
            apply_filter_with_exit(raw, &f, Some(false)),
            "some context line\nanother detail line\nKEEP this one"
        );
        // Success (or unknown exit) still filters normally.
        assert_eq!(apply_filter_with_exit(raw, &f, Some(true)), "KEEP this one");
        assert_eq!(apply_filter_with_exit(raw, &f, None), "KEEP this one");

        // on_failure = "tail:N": last N raw lines.
        let mut t = base_filter();
        t.keep_lines_matching = vec!["^KEEP".to_string()];
        t.on_failure = Some("tail:2".to_string());
        assert_eq!(
            apply_filter_with_exit(raw, &t, Some(false)),
            "another detail line\nKEEP this one"
        );
    }

    #[test]
    fn apply_filter_with_exit_suppresses_success_sentinels() {
        // A success match_output must not fire on a known-failed command,
        // even when the output contains its success marker.
        let mut f = base_filter();
        f.match_output = vec![MatchOutput {
            pattern: "Build complete!".to_string(),
            message: "ok".to_string(),
            unless: None,
        }];
        let raw = "Build complete! (but the linker crashed afterwards)\n";
        assert_eq!(apply_filter_with_exit(raw, &f, Some(false)), raw.trim_end());

        // on_empty must not report success for a failed command: benign-looking
        // emptied output falls back to a bounded raw view instead.
        let mut e = base_filter();
        e.keep_lines_matching = vec!["^NEVER".to_string()];
        e.on_empty = Some("cmd: ok".to_string());
        let benign = "all tasks completed with warnings suppressed\n";
        assert_eq!(
            apply_filter_with_exit(benign, &e, Some(false)),
            benign.trim_end()
        );
        // And a failed command that printed nothing yields nothing — never the
        // success sentinel.
        assert_eq!(apply_filter_with_exit("", &e, Some(false)), "");
    }

    #[test]
    fn apply_filter_priority_lines_survive_sizing() {
        let mut f = base_filter();
        f.max_lines = Some(2);
        f.priority_lines = vec!["^error".to_string(), "^Summary:".to_string()];
        let input = (1..=6)
            .map(|i| format!("noise line number {i} with plenty of padding"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\nerror: the thing that matters\nSummary: 1 failed\n";
        let out = apply_filter(&input, &f);
        assert_eq!(
            out,
            "noise line number 1 with plenty of padding\n\
             noise line number 2 with plenty of padding\n\
             error: the thing that matters\n\
             Summary: 1 failed"
        );
    }

    #[test]
    fn apply_filter_category_caps_bound_each_class() {
        let mut f = base_filter();
        f.category_caps = vec![CategoryCap {
            pattern: "^warning:".to_string(),
            max: 2,
        }];
        let input = "warning: first warning about a thing\n\
                     warning: second warning about a thing\n\
                     warning: third warning about a thing\n\
                     warning: fourth warning about a thing\n\
                     error: the actual problem to look at\n";
        let out = apply_filter(input, &f);
        assert_eq!(
            out,
            "warning: first warning about a thing\n\
             warning: second warning about a thing\n\
             [... 2 more similar line(s) omitted ...]\n\
             error: the actual problem to look at"
        );
    }

    #[test]
    fn filter_def_rejects_unknown_fields() {
        let toml = r#"
[filters.typo]
match_command = "^typo\\b"
keep_lines_matchin = ["oops"]
"#;
        assert!(
            toml::from_str::<FilterFile>(toml).is_err(),
            "typo'd field must fail loudly, not silently no-op"
        );
    }

    #[test]
    fn notify_on_truncate_is_opt_in_per_filter() {
        // Inputs are long enough that notice + kept lines stay cheaper than
        // raw, so the never_worse guard does not revert them.
        let input = (1..=9)
            .map(|i| format!("line {i} with enough payload to keep the notice cheaper"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut silent = base_filter();
        silent.max_lines = Some(2);
        assert!(!apply_filter(&input, &silent).contains("omitted"));

        let mut noisy = base_filter();
        noisy.max_lines = Some(2);
        noisy.notify_on_truncate = true;
        assert!(apply_filter(&input, &noisy).ends_with("[... 7 lines omitted (line limit) ...]"));

        let mut noisy_chars = base_filter();
        noisy_chars.truncate_lines_at = Some(24);
        noisy_chars.notify_on_truncate = true;
        let long_lines = format!(
            "café {0}\nação {0}\n",
            "is a rather long line with plenty of payload so the notice stays cheaper than raw"
        );
        assert!(apply_filter(&long_lines, &noisy_chars)
            .ends_with("[... 2 line(s) truncated to 24 chars ...]"));
    }

    #[test]
    fn apply_filter_replace_patterns_rewrites_lines() {
        let mut f = base_filter();
        f.replace_patterns = vec![
            ["\\d+\\.\\d+s".to_string(), "<dur>".to_string()],
            ["/home/[^/]+/".to_string(), "~/".to_string()],
        ];
        let out = apply_filter("built in 1.23s at /home/bob/app\n", &f);
        assert_eq!(out, "built in <dur> at ~/app");
    }

    #[test]
    fn apply_filter_extract_sections_between_markers() {
        let mut f = base_filter();
        f.extract_sections = vec![ExtractSection {
            start_pattern: "^START".to_string(),
            end_pattern: "^END".to_string(),
            include_markers: false,
            max_matches: None,
        }];
        let out = apply_filter("noise\nSTART\na\nb\nEND\ntrailing\n", &f);
        assert_eq!(out, "a\nb");

        // include_markers keeps the boundary lines.
        f.extract_sections[0].include_markers = true;
        let out2 = apply_filter("noise\nSTART\na\nEND\n", &f);
        assert_eq!(out2, "START\na\nEND");
    }

    #[test]
    fn apply_filter_extract_sections_no_match_returns_original() {
        let mut f = base_filter();
        f.extract_sections = vec![ExtractSection {
            start_pattern: "^NEVER".to_string(),
            end_pattern: "^NOPE".to_string(),
            include_markers: false,
            max_matches: None,
        }];
        // No marker present → falls back to the unmodified content.
        assert_eq!(apply_filter("just\ntwo lines\n", &f), "just\ntwo lines");
    }

    #[test]
    fn apply_filter_deduplicate_blocks_collapses_repeats() {
        let mut f = base_filter();
        f.deduplicate_blocks = Some(DeduplicateBlocksDef {
            min_block_lines: 3,
            similarity: 0.8,
            block_delimiter: None,
        });
        let block = "connection refused to upstream host\nretrying with backoff enabled\ngiving up after three attempts";
        let input = format!("{block}\n\n{block}\n\n{block}\n");
        let out = apply_filter(&input, &f);
        assert!(
            out.starts_with("connection refused to upstream host"),
            "first block kept: {out:?}"
        );
        assert!(
            out.contains("2 similar block(s) omitted"),
            "duplicates collapsed: {out:?}"
        );
    }

    #[test]
    fn apply_filter_token_budget_truncates_with_marker() {
        let mut f = base_filter();
        f.token_budget = Some(10);
        let mut input = String::from("error: critical failure\n");
        for i in 0..60 {
            input.push_str(&format!("filler line number {i} with words\n"));
        }
        let out = apply_filter(&input, &f);
        assert!(
            out.contains("omitted to fit token budget"),
            "expected truncation marker: {out:?}"
        );
        assert!(
            out.len() < input.len(),
            "budgeted output must be smaller than input"
        );
    }

    #[test]
    fn apply_filter_on_empty_for_genuinely_empty_output() {
        let mut f = base_filter();
        f.on_empty = Some("cmd: ok".to_string());
        // Truly empty / whitespace-only command output → success sentinel.
        assert_eq!(apply_filter("", &f), "cmd: ok");
        assert_eq!(apply_filter("   \n\t\n", &f), "cmd: ok");
    }

    #[test]
    fn apply_filter_benign_emptied_output_keeps_on_empty() {
        // keep rules strip everything, output is benign (no failure signal):
        // the success `on_empty` must still fire — guard must not over-trigger.
        let mut f = base_filter();
        f.keep_lines_matching = vec!["^KEEP".to_string()];
        f.on_empty = Some("cmd: ok".to_string());
        assert_eq!(
            apply_filter("benign output line\nnothing notable here\n", &f),
            "cmd: ok"
        );
    }

    #[test]
    fn apply_filter_failure_signal_overrides_on_empty() {
        // Same filter, but the output is a failure whose format the keep rule
        // doesn't recognize → must passthrough the error, never "cmd: ok".
        let mut f = base_filter();
        f.keep_lines_matching = vec!["^KEEP".to_string()];
        f.on_empty = Some("cmd: ok".to_string());
        let out = apply_filter("ERROR: boom\nprocess exited with exit code 1\n", &f);
        assert_ne!(out, "cmd: ok", "failure must not be masked");
        assert!(out.contains("ERROR: boom"), "real error surfaced: {out:?}");
    }

    #[test]
    fn apply_filter_passthrough_when_emptied_opt_in() {
        // Opt-in passthrough surfaces real output even without a failure signal.
        let mut f = base_filter();
        f.keep_lines_matching = vec!["^KEEP".to_string()];
        f.on_empty = Some("no changes".to_string());
        f.passthrough_when_emptied = true;
        let out = apply_filter("abc123 some commit subject\n", &f);
        assert_ne!(out, "no changes");
        assert!(out.contains("abc123"), "real output shown: {out:?}");
    }

    #[test]
    fn apply_filter_match_output_short_circuits_without_unless() {
        let mut f = base_filter();
        f.match_output = vec![MatchOutput {
            pattern: "BUILD SUCCESSFUL".to_string(),
            message: "ok".to_string(),
            unless: None,
        }];
        // Short-circuits before any line filtering / sizing.
        assert_eq!(
            apply_filter("noise\nBUILD SUCCESSFUL in 2s\nmore noise\n", &f),
            "ok"
        );
    }

    #[test]
    fn literal_anchor_extracts_only_plain_prefixes() {
        assert_eq!(literal_anchor("^cargo\\s+test\\b"), Some("cargo"));
        assert_eq!(literal_anchor("^gt\\s+submit\\b"), Some("gt"));
        assert_eq!(literal_anchor("^dotnet\\s+build"), Some("dotnet"));
        // Groups, alternations, escapes and unanchored patterns must never
        // yield a prefix — they go through full regex evaluation.
        assert_eq!(literal_anchor("^(npx\\s+)?alex\\b"), None);
        assert_eq!(literal_anchor("^(Get-Content|gc)\\b"), None);
        assert_eq!(literal_anchor("\\b(?:phpunit|pest)\\b"), None);
        assert_eq!(literal_anchor("^\\.?/gradlew"), None);
        // Single-char anchors are too weak to be worth the skip.
        assert_eq!(literal_anchor("^x\\b"), None);
    }

    #[test]
    #[ignore = "manual benchmark: cargo test --release hook_lookup_bench -- --ignored --nocapture"]
    fn hook_lookup_bench() {
        let filters = load_bundled_filters();
        let commands = [
            "git status",
            "cargo test --quiet 2>&1 | Select-Object -Last 5",
            "cd /app && npm install --no-audit",
            "Get-Content src/main.rs",
            "some-unknown-tool --flag value",
        ];
        // Warm the compile cache the way a long-lived process would…
        for cmd in &commands {
            let _ = find_filter(cmd, &filters);
        }
        let iterations = 200;
        let start = std::time::Instant::now();
        for _ in 0..iterations {
            for cmd in &commands {
                let _ = find_filter(cmd, &filters);
            }
        }
        let per_call = start.elapsed() / (iterations * commands.len() as u32);
        println!(
            "find_filter warm per-call: {per_call:?} over {} filters",
            filters.len()
        );

        // …and measure the cold path (fresh process cost) as a single pass.
        let cold = std::time::Instant::now();
        let _ = find_filter("terraform plan -out tf.plan", &filters);
        println!("find_filter cold first-call: {:?}", cold.elapsed());
    }

    #[test]
    fn find_filter_picks_longest_matching_pattern() {
        let mut broad = base_filter();
        broad.match_command = "^git\\b".to_string();
        let mut specific = base_filter();
        specific.match_command = "^git\\s+status\\b".to_string();
        let filters = [broad, specific];
        let hit = find_filter("git status -s", &filters).unwrap();
        assert_eq!(
            hit.match_command, "^git\\s+status\\b",
            "most specific (longest) pattern wins"
        );
    }

    // --- Golden self-test: run every bundled filter's embedded [[tests.<name>]]
    // cases through the real apply_filter pipeline. Homologation guard so the
    // ~150 declared input→expected pairs can never silently drift.
    #[derive(Debug, Deserialize)]
    struct GoldenCase {
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        command: Option<String>,
        input: String,
        expected: String,
    }

    #[derive(Debug, Deserialize)]
    struct FilterTestFile {
        #[serde(default)]
        filters: HashMap<String, FilterDef>,
        #[serde(default)]
        tests: HashMap<String, Vec<GoldenCase>>,
    }

    #[test]
    fn verbose_real_output_compresses_at_least_70pct() {
        // The corpus golden inputs are tiny fixtures; the headline 55% figure is
        // diluted by them. On the real use case — verbose, noisy command output —
        // the bundled filters must deliver heavy compression. Each pair is routed
        // through the real `find_filter` + `apply_filter` path, exactly like the
        // hook. Uses many command VARIANTS to also exercise filter resolution.
        use crate::chunker::count_tokens;
        let bundled = load_bundled_filters();

        // Realistic, verbose success output for common noisy commands. A clean
        // run is mostly progress/compile noise that collapses to a sentinel.
        let compiling = (0..30)
            .map(|i| format!("   Compiling crate_{i} v0.{i}.0"))
            .collect::<Vec<_>>()
            .join("\n");
        let cargo_build_out = format!(
            "    Updating crates.io index\n{compiling}\n    Finished dev [unoptimized + debuginfo] target(s) in 18.4s\n"
        );
        let cargo_test_out = format!(
            "{compiling}\n     Running unittests src/lib.rs\nrunning 42 tests\n{dots}\ntest result: ok. 42 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.31s\n",
            dots = (0..42)
                .map(|i| format!("test module::case_{i} ... ok"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let npm_out = {
            let mut s = String::new();
            for i in 0..25 {
                s.push_str(&format!(
                    "npm WARN deprecated pkg_{i}@1.0.0: use pkg_{i}@2\n"
                ));
            }
            s.push_str("added 642 packages, and audited 643 packages in 12s\n");
            s.push_str("found 0 vulnerabilities\n");
            s
        };
        let pip_out = {
            let mut s = String::new();
            for i in 0..20 {
                s.push_str(&format!(
                    "Requirement already satisfied: dep_{i} in ./venv\n"
                ));
            }
            s.push_str("Successfully installed app-1.0.0\n");
            s
        };
        let pytest_out = format!(
            "============================= test session starts ==============================\nplatform linux -- Python 3.12.0, pytest-8.0.0\ncollected 88 items\n\n{}\n\n============================== 88 passed in 4.21s ==============================\n",
            (0..6).map(|_| "tests/test_x.py ................................").collect::<Vec<_>>().join("\n")
        );
        let docker_out = {
            let mut s = String::new();
            for i in 1..25 {
                s.push_str(&format!(
                    "#{i} [stage {i}/24] RUN build step {i}\n#{i} DONE 0.{i}s\n"
                ));
            }
            s.push_str("#25 exporting to image\n#25 naming to docker.io/library/app:latest DONE\n");
            s
        };
        let eslint_out = "No problems found in 240 files.\n".to_string();
        let git_pull_out = "remote: Enumerating objects: 1200, done.\nremote: Counting objects: 100% (1200/1200), done.\nremote: Compressing objects: 100% (600/600), done.\nremote: Total 1200 (delta 800), reused 1100 (delta 700)\nUnpacking objects: 100% (1200/1200), 2.40 MiB | 4.80 MiB/s, done.\nFrom github.com:org/repo\n   abc1234..def5678  main       -> origin/main\nUpdating abc1234..def5678\nFast-forward\n".to_string();

        let cases: Vec<(&str, &str)> = vec![
            ("cargo build --release", &cargo_build_out),
            ("timeout 600 cargo test --all", &cargo_test_out),
            ("env CI=1 npm install --no-fund", &npm_out),
            ("pip install -r requirements.txt", &pip_out),
            ("python -m pytest -q", &pytest_out),
            ("docker build -t app:latest .", &docker_out),
            ("npx eslint src/", &eslint_out),
            ("git pull --rebase origin main", &git_pull_out),
        ];

        let mut in_total = 0usize;
        let mut out_total = 0usize;
        let mut report: Vec<String> = Vec::new();
        for (cmd, sample) in &cases {
            let f = find_filter(cmd, &bundled)
                .unwrap_or_else(|| panic!("no bundled filter resolved for {cmd:?}"));
            let got = apply_filter(sample, f);
            let it = count_tokens(sample);
            let ot = count_tokens(&got);
            let pct = (it.saturating_sub(ot) as f64 / it as f64) * 100.0;
            report.push(format!("{cmd}: {it}->{ot} ({pct:.0}%)"));
            in_total += it;
            out_total += ot;
        }
        let agg = (in_total.saturating_sub(out_total) as f64 / in_total as f64) * 100.0;
        eprintln!(
            "verbose-economy: {in_total}->{out_total} tokens, {agg:.1}% saved\n  {}",
            report.join("\n  ")
        );
        assert!(
            agg >= 70.0,
            "verbose real output must compress >=70%, got {agg:.1}%\n{}",
            report.join("\n")
        );
    }

    #[test]
    fn match_command_resolves_many_invocation_variants() {
        // Homologation: a filter must survive the many shapes a command arrives
        // in — wrappers (timeout/env/nice), launchers (npx/uv run/python -m),
        // shell `-c`, tool global options, cd-prefixes, pipes and `&&` chains.
        let bundled = load_bundled_filters();
        let variants: &[(&str, &[&str])] = &[
            (
                "cargo build",
                &[
                    "cargo build",
                    "cargo build --release",
                    "timeout 300 cargo build",
                    "env RUSTFLAGS=-W cargo build -j 8",
                    "cargo +nightly build",
                    "cd crates/app && cargo build",
                    "bash -c 'cargo build --workspace'",
                    "nice -n 10 cargo build",
                ],
            ),
            (
                "pytest",
                &[
                    "pytest",
                    "pytest -q tests/",
                    "python -m pytest",
                    "python3 -m pytest tests/unit",
                    "uv run pytest -x",
                    "cd backend && pytest",
                ],
            ),
            (
                "eslint",
                &[
                    "eslint .",
                    "npx eslint src/ --fix",
                    "cd web && npx eslint .",
                ],
            ),
            (
                "kubectl get",
                &[
                    "kubectl get pods",
                    "kubectl -n prod get pods -o wide",
                    "kubectl --context staging get deploy",
                ],
            ),
            (
                "git status",
                &[
                    "git status",
                    "git status -s",
                    "git -C /repo status",
                    "git -c color.ui=always status",
                    "cd repo && git status",
                ],
            ),
            (
                "docker build",
                &[
                    "docker build -t x .",
                    "docker buildx build --platform linux/amd64 .",
                    "timeout 900 docker build .",
                ],
            ),
            (
                "npm install",
                &[
                    "npm install",
                    "npm i",
                    "npm install --save-dev typescript",
                    "cat .npmrc && npm install",
                ],
            ),
            // Newly added filters — verify their variants resolve too.
            (
                "npm ci",
                &["npm ci", "timeout 300 npm ci", "cd web && npm ci"],
            ),
            (
                "cargo bench",
                &[
                    "cargo bench",
                    "cargo bench --bench parse",
                    "cargo +nightly bench",
                ],
            ),
            ("cargo update", &["cargo update", "cargo update -p serde"]),
            (
                "pip list",
                &[
                    "pip list",
                    "pip freeze",
                    "python -m pip list",
                    "pip3 freeze",
                ],
            ),
            (
                "git cherry-pick",
                &[
                    "git cherry-pick abc123",
                    "git -C /repo cherry-pick --continue",
                ],
            ),
            (
                "dotnet run",
                &[
                    "dotnet run",
                    "dotnet run --project app",
                    "cd svc && dotnet run",
                ],
            ),
            (
                "prisma",
                &[
                    "prisma generate",
                    "npx prisma migrate dev",
                    "pnpm prisma db push",
                    "yarn prisma studio",
                    "bunx prisma generate",
                ],
            ),
            (
                "wrangler",
                &[
                    "wrangler deploy",
                    "npx wrangler deploy",
                    "pnpm wrangler pages deploy dist",
                    "yarn wrangler tail",
                ],
            ),
        ];
        let mut misses: Vec<String> = Vec::new();
        for (label, cmds) in variants {
            for cmd in *cmds {
                if find_filter(cmd, &bundled).is_none() {
                    misses.push(format!("[{label}] no filter for variant: {cmd:?}"));
                }
            }
        }
        assert!(
            misses.is_empty(),
            "{} command variant(s) failed to resolve a filter:\n{}",
            misses.len(),
            misses.join("\n")
        );
    }

    #[test]
    fn bundled_filters_pass_embedded_golden_tests() {
        let mut total = 0usize;
        let mut files_with_tests = 0usize;
        let mut failures: Vec<String> = Vec::new();

        for asset in BundledFilters::iter() {
            let file = BundledFilters::get(&asset).expect("bundled asset readable");
            let content = std::str::from_utf8(file.data.as_ref()).expect("filter is utf8");
            let parsed: FilterTestFile = match toml::from_str(content) {
                Ok(p) => p,
                Err(e) => {
                    failures.push(format!("{asset}: TOML parse error: {e}"));
                    continue;
                }
            };
            if !parsed.tests.is_empty() {
                files_with_tests += 1;
            }
            for (fname, cases) in &parsed.tests {
                let Some(fdef) = parsed.filters.get(fname) else {
                    failures.push(format!(
                        "{asset}: [[tests.{fname}]] references undefined [filters.{fname}]"
                    ));
                    continue;
                };
                for (i, case) in cases.iter().enumerate() {
                    total += 1;
                    if let Some(command) = case.command.as_deref() {
                        let re = Regex::new(&fdef.match_command).unwrap_or_else(|e| {
                            panic!("{asset} [{fname}] invalid match_command: {e}")
                        });
                        assert!(
                            re.is_match(command),
                            "{asset} [{fname}] expected match_command to match {:?}",
                            command
                        );
                    }
                    let got = apply_filter(&case.input, fdef);
                    if got.trim_end() != case.expected.trim_end() {
                        let label = case.name.clone().unwrap_or_else(|| format!("#{i}"));
                        failures.push(format!(
                            "{asset} [{fname} / {label}]\n  expected: {:?}\n  got:      {:?}",
                            case.expected, got
                        ));
                    }
                }
            }
        }

        eprintln!(
            "golden: ran {total} embedded cases across {files_with_tests} bundled filter files"
        );
        assert!(
            failures.is_empty(),
            "{} bundled golden filter case(s) failed:\n\n{}",
            failures.len(),
            failures.join("\n\n")
        );
    }

    #[test]
    fn bundled_filters_require_minimum_tests() {
        let mut failures = Vec::new();
        for asset in BundledFilters::iter() {
            let file = BundledFilters::get(&asset).expect("bundled asset readable");
            let content = std::str::from_utf8(file.data.as_ref()).expect("filter is utf8");
            let parsed: FilterTestFile = match toml::from_str(content) {
                Ok(p) => p,
                Err(e) => {
                    failures.push(format!("{asset}: TOML parse error: {e}"));
                    continue;
                }
            };
            for fname in parsed.filters.keys() {
                match parsed.tests.get(fname) {
                    Some(cases) => {
                        if cases.len() < 2 {
                            failures.push(format!(
                                "{asset}: [filters.{fname}] has only {} test case(s), expected at least 2",
                                cases.len()
                            ));
                        }
                    }
                    None => {
                        failures.push(format!(
                            "{asset}: [filters.{fname}] has NO test cases defined"
                        ));
                    }
                }
            }
        }
        assert!(
            failures.is_empty(),
            "The following bundled filter(s) do not meet the minimum test requirement (>=2 golden cases):\n\n{}",
            failures.join("\n")
        );
    }

    #[test]
    #[ignore = "maintainer report; run with --ignored --nocapture"]
    fn report_sentinel_evidence_candidates() {
        // For each filter whose sentinel answers non-empty output, print the
        // fixture that proves it. The success marker for a `match_output`
        // (positive evidence) rule is almost always already in that text.
        let unknown = "unrecognized tool output line one\n\
                       some other shape the filter never saw\n\
                       third line of genuine output\n";
        for asset in BundledFilters::iter() {
            let file = BundledFilters::get(&asset).expect("asset");
            let content = std::str::from_utf8(file.data.as_ref()).expect("utf8");
            let Ok(parsed) = toml::from_str::<FilterTestFile>(content) else {
                continue;
            };
            for (fname, fdef) in &parsed.filters {
                let Some(sentinel) = fdef.on_empty.as_deref() else {
                    continue;
                };
                if apply_filter(unknown, fdef).trim() != sentinel.trim() {
                    continue;
                }
                let Some(cases) = parsed.tests.get(fname) else {
                    continue;
                };
                for c in cases {
                    if !c.input.trim().is_empty() && c.expected.trim() == sentinel.trim() {
                        println!(
                            "@@@\t{fname}\t{asset}\t{}",
                            c.input.replace('\n', " ⏎ ").trim()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn on_empty_sentinel_never_answers_unrecognized_output() {
        // Per-filter homologation of the "filters must only filter, never
        // invent responses" rule.
        //
        // An `on_empty` sentinel is legitimate in exactly two shapes:
        //  a) the tool's clean run really is empty (a silent linter) — the
        //     sentinel then answers *empty* output, and the filter must set
        //     `passthrough_when_emptied` so that NON-empty output it does not
        //     recognize is shown instead of being answered with a false "all
        //     clear";
        //  b) the filter deliberately collapses recognized verbose output into
        //     a summary — proven by a golden case whose non-empty input yields
        //     the sentinel.
        //
        // Anything else is a filter telling the agent a command succeeded when
        // it has no evidence of that.
        let unknown = "unrecognized tool output line one\n\
                       some other shape the filter never saw\n\
                       third line of genuine output\n";
        let mut offenders = Vec::new();
        for asset in BundledFilters::iter() {
            let file = BundledFilters::get(&asset).expect("asset");
            let content = std::str::from_utf8(file.data.as_ref()).expect("utf8");
            let Ok(parsed) = toml::from_str::<FilterTestFile>(content) else {
                continue;
            };
            for (fname, fdef) in &parsed.filters {
                let Some(sentinel) = fdef.on_empty.as_deref() else {
                    continue;
                };
                if apply_filter(unknown, fdef).trim() != sentinel.trim() {
                    continue; // does not fabricate — nothing to prove
                }
                let summarizes = parsed.tests.get(fname).is_some_and(|cases| {
                    cases
                        .iter()
                        .any(|c| !c.input.trim().is_empty() && c.expected.trim() == sentinel.trim())
                });
                if !summarizes {
                    offenders.push(format!(
                        "{asset} [{fname}]: answers unrecognized output with {sentinel:?}; \
                         add `passthrough_when_emptied = true` (or a golden case proving it \
                         legitimately summarizes non-empty output)"
                    ));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "{} filter(s) fabricate a success sentinel over real output:\n{}",
            offenders.len(),
            offenders.join("\n")
        );
    }

    /// Per-filter homologation report (one TSV row per bundled filter):
    /// golden verdict, token economy, failure-path coverage, masking,
    /// inflation, sentinel shape and prefilter class.
    ///
    /// Ignored by default because it is a report, not an assertion — the
    /// invariants it surfaces are locked by the tests above. Run it with:
    /// `cargo test --release --bin tokenix homologate -- --ignored --nocapture`
    #[test]
    #[ignore = "maintainer report; run with --ignored --nocapture"]
    fn homologate_each_filter() {
        use crate::chunker::count_tokens;
        println!("FILTER\tASSET\tCASES\tGOLDEN\tRAW\tOUT\tSAVED%\tFAILCASE\tMASKS\tINFLATES\tON_EMPTY\tPASSTHRU\tPREFILTER\tFABRICATES\tSUMMARY");
        for asset in BundledFilters::iter() {
            let file = BundledFilters::get(&asset).expect("asset");
            let content = std::str::from_utf8(file.data.as_ref()).expect("utf8");
            let parsed: FilterTestFile = match toml::from_str(content) {
                Ok(p) => p,
                Err(e) => {
                    println!("?\t{asset}\tPARSE_ERROR\t{e}");
                    continue;
                }
            };
            for (fname, fdef) in &parsed.filters {
                let empty: Vec<GoldenCase> = Vec::new();
                let cases = parsed.tests.get(fname).unwrap_or(&empty);
                let (mut raw, mut out) = (0usize, 0usize);
                let (mut golden_ok, mut fail_case, mut masks, mut inflates) =
                    (true, false, false, false);
                for case in cases {
                    let got = apply_filter(&case.input, fdef);
                    if got.trim_end() != case.expected.trim_end() {
                        golden_ok = false;
                    }
                    raw += count_tokens(&case.input);
                    out += count_tokens(&got);
                    // `never_worse` only guards non-empty raw output: an
                    // `on_empty` sentinel replacing genuinely empty output is
                    // by design, not inflation.
                    let raw_trim = case.input.trim_end();
                    if !raw_trim.is_empty() && got.len() > raw_trim.len() {
                        inflates = true;
                    }
                    // A failure-path case: the raw input carries a failure
                    // signal. The filtered view must still carry one, or the
                    // agent is told a failed command succeeded.
                    if output_has_failure_signal(&case.input) {
                        fail_case = true;
                        if !output_has_failure_signal(&got) {
                            masks = true;
                        }
                    }
                }
                let pct = if raw > 0 {
                    100.0 * (raw.saturating_sub(out)) as f64 / raw as f64
                } else {
                    0.0
                };
                // The failure mode `passthrough_when_emptied` exists to stop:
                // real, non-empty output the filter does not recognize being
                // replaced by a fabricated success sentinel. Probe it with a
                // benign payload carrying no failure signal.
                let unknown = "unrecognized tool output line one\n\
                               some other shape the filter never saw\n\
                               third line of genuine output\n";
                let probe = apply_filter(unknown, fdef);
                let fabricates = fdef
                    .on_empty
                    .as_deref()
                    .is_some_and(|sentinel| probe.trim() == sentinel.trim());

                // Does any golden case reach the sentinel from NON-EMPTY input?
                // If not, the sentinel only ever answers a genuinely empty run
                // (a clean linter), and `passthrough_when_emptied` can be added
                // without changing a single documented behavior.
                let sentinel_from_nonempty = fdef.on_empty.as_deref().is_some_and(|sentinel| {
                    cases
                        .iter()
                        .any(|c| !c.input.trim().is_empty() && c.expected.trim() == sentinel.trim())
                });

                let pf = match prefilter_for(&fdef.match_command) {
                    Some(Prefilter::StartsWith(_)) => "anchor",
                    Some(Prefilter::Contains(_, _)) => "contains",
                    None => "none",
                };
                println!(
                    "{fname}\t{asset}\t{}\t{}\t{raw}\t{out}\t{pct:.0}\t{}\t{}\t{}\t{}\t{}\t{pf}\t{}\t{}",
                    cases.len(),
                    if golden_ok { "ok" } else { "FAIL" },
                    if fail_case { "yes" } else { "NO" },
                    if masks { "MASKS" } else { "-" },
                    if inflates { "INFLATES" } else { "-" },
                    if fdef.on_empty.is_some() { "on_empty" } else { "-" },
                    if fdef.passthrough_when_emptied { "pass" } else { "-" },
                    if fabricates { "FABRICATES" } else { "-" },
                    if sentinel_from_nonempty { "SUMMARY" } else { "-" },
                );
            }
        }
    }

    /// Iterate every bundled filter's embedded golden cases, applying the real
    /// pipeline. Yields `(asset, filter_name, input, filtered_output)`.
    fn for_each_golden_case<F: FnMut(&str, &str, &str, &str)>(mut visit: F) {
        for asset in BundledFilters::iter() {
            let file = BundledFilters::get(&asset).expect("bundled asset readable");
            let content = std::str::from_utf8(file.data.as_ref()).expect("utf8");
            let parsed: FilterTestFile = match toml::from_str(content) {
                Ok(p) => p,
                Err(_) => continue,
            };
            for (fname, cases) in &parsed.tests {
                let Some(fdef) = parsed.filters.get(fname) else {
                    continue;
                };
                for case in cases {
                    let got = apply_filter(&case.input, fdef);
                    visit(&asset, fname, &case.input, &got);
                }
            }
        }
    }

    #[test]
    #[ignore]
    fn diag_per_filter_compression() {
        let mut rows: Vec<(String, usize, usize, f64)> = Vec::new();
        for_each_golden_case_grouped(|fname, it, ot| {
            let pct = if it > 0 {
                (it.saturating_sub(ot) as f64 / it as f64) * 100.0
            } else {
                0.0
            };
            rows.push((fname.to_string(), it, ot, pct));
        });
        // Lowest %-saved first; ties broken by biggest input (most waste left).
        rows.sort_by(|a, b| a.3.partial_cmp(&b.3).unwrap().then(b.1.cmp(&a.1)));
        let mut out = String::from("FILTER,IN_TOK,OUT_TOK,PCT_SAVED\n");
        for (n, it, ot, pct) in &rows {
            out.push_str(&format!("{n},{it},{ot},{pct:.0}\n"));
        }
        let path = std::env::temp_dir().join("tokenix_diag_compression.csv");
        std::fs::write(&path, out).unwrap();
        eprintln!("wrote {}", path.display());
    }

    /// Like `for_each_golden_case` but aggregates per filter: `(name, in_tok, out_tok)`.
    fn for_each_golden_case_grouped<F: FnMut(&str, usize, usize)>(mut visit: F) {
        use crate::chunker::count_tokens;
        for asset in BundledFilters::iter() {
            let file = BundledFilters::get(&asset).unwrap();
            let content = std::str::from_utf8(file.data.as_ref()).unwrap();
            let parsed: FilterTestFile = match toml::from_str(content) {
                Ok(p) => p,
                Err(_) => continue,
            };
            for (fname, cases) in &parsed.tests {
                let Some(fdef) = parsed.filters.get(fname) else {
                    continue;
                };
                let mut it = 0usize;
                let mut ot = 0usize;
                for case in cases {
                    it += count_tokens(&case.input);
                    ot += count_tokens(&apply_filter(&case.input, fdef));
                }
                visit(fname, it, ot);
            }
        }
    }

    #[test]
    fn filters_never_inflate_output_tokens() {
        // Economy invariant: a filter must never produce MORE tokens than it was
        // given (a tiny slack covers short inputs replaced by a sentinel message
        // like "cmd: ok"). A filter that inflates output is a net token loss.
        use crate::chunker::count_tokens;
        const SLACK_TOKENS: usize = 8;
        let mut offenders: Vec<String> = Vec::new();
        for_each_golden_case(|asset, fname, input, got| {
            let in_tok = count_tokens(input);
            let out_tok = count_tokens(got);
            if out_tok > in_tok + SLACK_TOKENS {
                offenders.push(format!(
                    "{asset} [{fname}]: {in_tok} -> {out_tok} tokens (inflated)"
                ));
            }
        });
        assert!(
            offenders.is_empty(),
            "{} filter case(s) inflated output beyond slack:\n{}",
            offenders.len(),
            offenders.join("\n")
        );
    }

    #[test]
    fn filters_deliver_aggregate_token_savings() {
        // The whole point of the tool: across the bundled corpus' realistic
        // sample inputs, filtering must cut a large share of tokens. Guards
        // against a regression that quietly neuters compression (e.g. a broken
        // strip/keep stage) while individual golden equality still passes.
        use crate::chunker::count_tokens;
        let mut in_total = 0usize;
        let mut out_total = 0usize;
        let mut cases = 0usize;
        for_each_golden_case(|_, _, input, got| {
            in_total += count_tokens(input);
            out_total += count_tokens(got);
            cases += 1;
        });
        assert!(cases > 100, "expected the full corpus, saw {cases} cases");
        let saved = in_total.saturating_sub(out_total);
        let pct = (saved as f64 / in_total as f64) * 100.0;
        eprintln!("economy: {cases} cases, {in_total} -> {out_total} tokens, {pct:.1}% saved");
        assert!(out_total < in_total, "corpus must shrink, not grow");
        assert!(
            pct >= 40.0,
            "expected >=40% aggregate token savings across the corpus, got {pct:.1}%"
        );
    }

    #[test]
    fn output_has_failure_signal_strict() {
        // Positives: real failure output.
        assert!(output_has_failure_signal(
            "fatal: something went wrong\nERROR: build failed with exit code 1"
        ));
        assert!(output_has_failure_signal(
            "panic: runtime error: index out of range"
        ));
        assert!(output_has_failure_signal(
            "FAILED tests/test_foo.py::test_bar - AssertionError"
        ));
        assert!(output_has_failure_signal(
            "Traceback (most recent call last):\n  File \"x.py\""
        ));
        assert!(output_has_failure_signal("error: aborting due to 1 error"));

        // New Positives:
        assert!(output_has_failure_signal(
            "[ERROR] database connection failed"
        ));
        assert!(output_has_failure_signal(
            "2026-06-22T12:00:00Z [error] database down"
        ));
        assert!(output_has_failure_signal("npm ERR! code ELIFECYCLE"));
        assert!(output_has_failure_signal("yarn ERR: error Command failed"));
        assert!(output_has_failure_signal(
            "--- FAIL: TestExploreCodebase (0.05s)"
        ));
        assert!(output_has_failure_signal(
            "Segmentation fault (core dumped)"
        ));
        assert!(output_has_failure_signal("Process received signal SIGSEGV"));
        assert!(output_has_failure_signal(
            "java.lang.NullPointerException: object is null"
        ));
        assert!(output_has_failure_signal("exited with status: 1"));
        assert!(output_has_failure_signal("exit status 127"));
        assert!(output_has_failure_signal("Command failed with exit code 2"));
        assert!(output_has_failure_signal(
            "time=\"xxx\" level=error msg=\"db lost\""
        ));

        // Negatives: benign success summaries that merely mention error/fail.
        assert!(!output_has_failure_signal("test result: ok. 0 failed"));
        assert!(!output_has_failure_signal("0 errors, 0 warnings"));
        assert!(!output_has_failure_signal("no errors found"));
        assert!(!output_has_failure_signal("Compiling: 0 failures"));
        assert!(!output_has_failure_signal("exit status 0"));
        assert!(!output_has_failure_signal("exited with status: 0"));
        assert!(!output_has_failure_signal("warnings: 12"));
    }

    #[test]
    fn bundled_filters_never_mask_generic_failure() {
        // Homologation guard: a generic command failure must never be reduced
        // to a filter's success `on_empty` message. Feeds each bundled filter
        // an unambiguous failure payload and asserts a failure marker survives.
        let payload = "fatal: the operation failed\nERROR: process exited with exit code 1\nFAILED";
        let survives = Regex::new(r"(?i)error|fail|fatal").unwrap();
        let mut masked: Vec<String> = Vec::new();
        for asset in BundledFilters::iter() {
            let file = BundledFilters::get(&asset).unwrap();
            let content = std::str::from_utf8(file.data.as_ref()).unwrap();
            let parsed: FilterFile = match toml::from_str(content) {
                Ok(p) => p,
                Err(_) => continue,
            };
            for (name, fdef) in &parsed.filters {
                let got = apply_filter(payload, fdef);
                if !survives.is_match(&got) {
                    masked.push(format!("{asset} [{name}] -> {:?}", got));
                }
            }
        }
        assert!(
            masked.is_empty(),
            "{} bundled filter(s) masked a generic failure as success:\n{}",
            masked.len(),
            masked.join("\n")
        );
    }

    #[test]
    fn test_gradlew_match() {
        let filters = load_bundled_filters();
        let f = find_filter("./gradlew", &filters);
        assert!(f.is_some(), "gradlew filter must be found for './gradlew'");
        // gradlew is an all-noise-success tool: an emptied benign output must
        // collapse to the on_empty sentinel (the engine's failure-signal guard
        // still passes real errors through), not resurrect raw noise.
        assert!(
            f.unwrap().on_empty.is_some(),
            "gradlew filter must report success via on_empty when filtering empties it"
        );
    }

    #[test]
    fn find_filter_parameter_scenarios() {
        let filters = [
            FilterDef {
                description: None,
                match_command: "^cargo\\s+test\\b".to_string(),
                skip_when_matches: None,
                notify_on_truncate: false,
                strip_ansi: false,
                strip_lines_matching: vec![],
                keep_lines_matching: vec![],
                max_lines: None,
                head_lines: None,
                tail_lines: None,
                on_empty: Some("cargo-test".to_string()),
                passthrough_when_emptied: false,
                match_output: vec![],
                truncate_lines_at: None,
                uniform_success: None,
                filter_stderr: false,
                replace_patterns: vec![],
                extract_sections: vec![],
                semantic_filter: None,
                deduplicate_blocks: None,
                summarize_json: None,
                token_budget: None,
                on_failure: None,
                priority_lines: vec![],
                category_caps: vec![],
            },
            FilterDef {
                description: None,
                match_command: "^docker\\s+build\\b".to_string(),
                skip_when_matches: None,
                notify_on_truncate: false,
                strip_ansi: false,
                strip_lines_matching: vec![],
                keep_lines_matching: vec![],
                max_lines: None,
                head_lines: None,
                tail_lines: None,
                on_empty: Some("docker-build".to_string()),
                passthrough_when_emptied: false,
                match_output: vec![],
                truncate_lines_at: None,
                uniform_success: None,
                filter_stderr: false,
                replace_patterns: vec![],
                extract_sections: vec![],
                semantic_filter: None,
                deduplicate_blocks: None,
                summarize_json: None,
                token_budget: None,
                on_failure: None,
                priority_lines: vec![],
                category_caps: vec![],
            },
            FilterDef {
                description: None,
                match_command: "^git\\s+diff\\b".to_string(),
                skip_when_matches: None,
                notify_on_truncate: false,
                strip_ansi: false,
                strip_lines_matching: vec![],
                keep_lines_matching: vec![],
                max_lines: None,
                head_lines: None,
                tail_lines: None,
                on_empty: Some("git-diff".to_string()),
                passthrough_when_emptied: false,
                match_output: vec![],
                truncate_lines_at: None,
                uniform_success: None,
                filter_stderr: false,
                replace_patterns: vec![],
                extract_sections: vec![],
                semantic_filter: None,
                deduplicate_blocks: None,
                summarize_json: None,
                token_budget: None,
                on_failure: None,
                priority_lines: vec![],
                category_caps: vec![],
            },
            FilterDef {
                description: None,
                match_command: "^git\\s+log\\b".to_string(),
                skip_when_matches: None,
                notify_on_truncate: false,
                strip_ansi: false,
                strip_lines_matching: vec![],
                keep_lines_matching: vec![],
                max_lines: None,
                head_lines: None,
                tail_lines: None,
                on_empty: Some("git-log".to_string()),
                passthrough_when_emptied: false,
                match_output: vec![],
                truncate_lines_at: None,
                uniform_success: None,
                filter_stderr: false,
                replace_patterns: vec![],
                extract_sections: vec![],
                semantic_filter: None,
                deduplicate_blocks: None,
                summarize_json: None,
                token_budget: None,
                on_failure: None,
                priority_lines: vec![],
                category_caps: vec![],
            },
            FilterDef {
                description: None,
                match_command: "^kubectl\\s+get\\b".to_string(),
                skip_when_matches: None,
                notify_on_truncate: false,
                strip_ansi: false,
                strip_lines_matching: vec![],
                keep_lines_matching: vec![],
                max_lines: None,
                head_lines: None,
                tail_lines: None,
                on_empty: Some("kubectl-get".to_string()),
                passthrough_when_emptied: false,
                match_output: vec![],
                truncate_lines_at: None,
                uniform_success: None,
                filter_stderr: false,
                replace_patterns: vec![],
                extract_sections: vec![],
                semantic_filter: None,
                deduplicate_blocks: None,
                summarize_json: Some(SummarizeJsonDef {
                    max_array_items: 10,
                    max_depth: 3,
                    always_include: vec![],
                    exclude: vec![],
                }),
                token_budget: None,
                on_failure: None,
                priority_lines: vec![],
                category_caps: vec![],
            },
            FilterDef {
                description: None,
                match_command: "^pytest\\b".to_string(),
                skip_when_matches: None,
                notify_on_truncate: false,
                strip_ansi: false,
                strip_lines_matching: vec![],
                keep_lines_matching: vec![],
                max_lines: None,
                head_lines: None,
                tail_lines: None,
                on_empty: Some("pytest".to_string()),
                passthrough_when_emptied: false,
                match_output: vec![],
                truncate_lines_at: None,
                uniform_success: None,
                filter_stderr: false,
                replace_patterns: vec![],
                extract_sections: vec![],
                semantic_filter: None,
                deduplicate_blocks: None,
                summarize_json: None,
                token_budget: None,
                on_failure: None,
                priority_lines: vec![],
                category_caps: vec![],
            },
            FilterDef {
                description: None,
                match_command: "^git\\b".to_string(),
                skip_when_matches: None,
                notify_on_truncate: false,
                strip_ansi: false,
                strip_lines_matching: vec![],
                keep_lines_matching: vec![],
                max_lines: None,
                head_lines: None,
                tail_lines: None,
                on_empty: Some("git-broad".to_string()),
                passthrough_when_emptied: false,
                match_output: vec![],
                truncate_lines_at: None,
                uniform_success: None,
                filter_stderr: false,
                replace_patterns: vec![],
                extract_sections: vec![],
                semantic_filter: None,
                deduplicate_blocks: None,
                summarize_json: None,
                token_budget: None,
                on_failure: None,
                priority_lines: vec![],
                category_caps: vec![],
            },
            FilterDef {
                description: None,
                match_command: "^git\\s+branch\\b".to_string(),
                skip_when_matches: None,
                notify_on_truncate: false,
                strip_ansi: false,
                strip_lines_matching: vec![],
                keep_lines_matching: vec![],
                max_lines: None,
                head_lines: None,
                tail_lines: None,
                on_empty: Some("git-branch".to_string()),
                passthrough_when_emptied: false,
                match_output: vec![],
                truncate_lines_at: None,
                uniform_success: None,
                filter_stderr: false,
                replace_patterns: vec![],
                extract_sections: vec![],
                semantic_filter: None,
                deduplicate_blocks: None,
                summarize_json: None,
                token_budget: None,
                on_failure: None,
                priority_lines: vec![],
                category_caps: vec![],
            },
            FilterDef {
                description: None,
                match_command: "^eslint\\b".to_string(),
                skip_when_matches: None,
                notify_on_truncate: false,
                strip_ansi: false,
                strip_lines_matching: vec![],
                keep_lines_matching: vec![],
                max_lines: None,
                head_lines: None,
                tail_lines: None,
                on_empty: Some("eslint".to_string()),
                passthrough_when_emptied: false,
                match_output: vec![],
                truncate_lines_at: None,
                uniform_success: None,
                filter_stderr: false,
                replace_patterns: vec![],
                extract_sections: vec![],
                semantic_filter: None,
                deduplicate_blocks: None,
                summarize_json: None,
                token_budget: None,
                on_failure: None,
                priority_lines: vec![],
                category_caps: vec![],
            },
        ];

        let run =
            |cmd: &str| find_filter(cmd, &filters).map(|f| f.on_empty.as_ref().unwrap().as_str());

        // 120+ data-driven test cases validating parameters, wrappers, shell features, and bypass flags
        let static_cases = vec![
            // 1. Tool-specific basic matches
            ("cargo test", Some("cargo-test")),
            ("docker build", Some("docker-build")),
            ("git diff", Some("git-diff")),
            ("git log", Some("git-log")),
            ("kubectl get", Some("kubectl-get")),
            ("pytest", Some("pytest")),
            ("eslint", Some("eslint")),
            ("git branch", Some("git-branch")),
            // 2. Standard parameters & flags
            (
                "cargo test --workspace --all-features --jobs 4",
                Some("cargo-test"),
            ),
            ("cargo test -p my-package --lib", Some("cargo-test")),
            (
                "docker build -t image:latest -f Dockerfile .",
                Some("docker-build"),
            ),
            (
                "docker build --build-arg KEY=VAL --no-cache .",
                Some("docker-build"),
            ),
            (
                "git diff HEAD~1 HEAD --stat --compact-summary",
                Some("git-diff"),
            ),
            ("git diff main..feature --name-status", Some("git-diff")),
            (
                "git log -n 50 --oneline --graph --decorate",
                Some("git-log"),
            ),
            (
                "git log --author=\"Bob\" --since=\"1 week ago\"",
                Some("git-log"),
            ),
            (
                "kubectl get pods -n kube-system -o wide",
                Some("kubectl-get"),
            ),
            (
                "kubectl get services,deployments -l app=nginx",
                Some("kubectl-get"),
            ),
            (
                "pytest tests/test_auth.py -k \"login_successful\"",
                Some("pytest"),
            ),
            (
                "pytest -v --tb=short --cov=src --cov-report=html",
                Some("pytest"),
            ),
            ("eslint src/ --ext .ts,.tsx --fix", Some("eslint")),
            (
                "eslint --cache --resolve-plugins-relative-to .",
                Some("eslint"),
            ),
            // 3. Env vars & wrappers
            ("CI=true cargo test", Some("cargo-test")),
            ("NODE_ENV=test PORT=3000 pytest", Some("pytest")),
            ("cross-env CI=true pnpm exec eslint", Some("eslint")),
            ("time cargo test", Some("cargo-test")),
            ("nice cargo test", Some("cargo-test")),
            ("nice -n 10 cargo test", Some("cargo-test")),
            ("timeout 30s pytest", Some("pytest")),
            ("timeout --foreground 60s pytest", Some("pytest")),
            ("timeout -k 5 10 nice -n 19 pytest", Some("pytest")),
            (
                "CI=true timeout 30 nice -n 5 cargo test --quiet",
                Some("cargo-test"),
            ),
            // 4. Directory prefixes (cd / pushd)
            ("cd app && cargo test", Some("cargo-test")),
            ("cd app; cargo test", Some("cargo-test")),
            ("cd app || exit 1; cargo test", Some("cargo-test")),
            ("pushd app && pytest", Some("pytest")),
            ("cd /d C:\\Project && docker build .", Some("docker-build")),
            ("cd src && ENV=1 timeout 10 pytest -v", Some("pytest")),
            // 5. Global tool options with subcommand parameters
            ("git -C /src diff", Some("git-diff")),
            ("git --git-dir=/src/.git diff", Some("git-diff")),
            ("git -c core.autocrlf=input diff", Some("git-diff")),
            ("git --no-pager diff", Some("git-diff")),
            (
                "git -C /src -c k=v --no-pager diff --stat",
                Some("git-diff"),
            ),
            ("docker -H tcp://1.2.3.4:2376 build .", Some("docker-build")),
            ("docker --context default build .", Some("docker-build")),
            (
                "kubectl --kubeconfig=~/.kube/config get pods",
                Some("kubectl-get"),
            ),
            (
                "kubectl -n default --context=dev get pods",
                Some("kubectl-get"),
            ),
            // 6. Package runners
            ("npx eslint", Some("eslint")),
            ("npx --no-install eslint .", Some("eslint")),
            ("pnpm exec eslint", Some("eslint")),
            ("pnpm dlx eslint", Some("eslint")),
            ("bunx eslint", Some("eslint")),
            ("bun x eslint", Some("eslint")),
            ("yarn dlx eslint", Some("eslint")),
            ("uv run pytest", Some("pytest")),
            ("uvx pytest", Some("pytest")),
            ("python -m pytest", Some("pytest")),
            ("python3 -m pytest", Some("pytest")),
            ("python -m ruff check", None), // no ruff filter registered
            // 7. Shell runners
            ("bash -c \"cargo test\"", Some("cargo-test")),
            ("sh -c \"pytest\"", Some("pytest")),
            ("cmd.exe /c \"cargo test\"", Some("cargo-test")),
            ("powershell -Command \"cargo test\"", Some("cargo-test")),
            ("pwsh -Command \"pytest\"", Some("pytest")),
            ("& 'cargo test'", Some("cargo-test")),
            // 8. Help flag bypass variants
            ("cargo test --help", None),
            ("cargo test -h", None),
            ("cargo test help", None),
            ("cargo test /h", None),
            ("cargo test /?", None),
            ("cargo test --help-all", None),
            ("cargo test -help", None),
            ("git diff --help", None),
            ("git diff -h", None),
            ("git help diff", None),
            ("kubectl get --help", None),
            ("pytest -h", None),
            ("eslint --help", None),
            // 9. Version flag bypass variants
            ("node --version", None),
            ("node -v", None),
            ("python -V", None),
            ("python3 --version", None),
            ("git --version", None),
            ("git -v", None),
            ("docker --version", None),
            ("docker -v", None),
            ("kubectl version", None),
            // 10. Debug / Verbose bypass variants
            ("cargo test --debug", None),
            ("cargo test --verbose", None),
            ("cargo test --trace", None),
            ("cargo test -vv", None),
            ("cargo test -vvv", None),
            ("cargo test --log-level=debug", None),
            ("cargo test --log-level=trace", None),
            ("pytest --verbose", None),
            ("pytest -vv", None),
            ("git diff --verbose", None),
            ("docker build --debug", None),
            ("kubectl get pods --log-level=debug", None),
            // 11. YAML format bypass variants
            ("kubectl get pods --yaml", None),
            ("kubectl get pods -o yaml", None),
            ("kubectl get pods -o=yaml", None),
            ("kubectl get pods --format yaml", None),
            ("kubectl get pods --format=yaml", None),
            ("docker inspect --format yaml", None),
            // 12. JSON format bypass vs match
            ("cargo test --json", None),                // no JSON support
            ("cargo test -o json", None),               // no JSON support
            ("cargo test -o=json", None),               // no JSON support
            ("cargo test --message-format=json", None), // no JSON support
            ("pytest --json", None),                    // no JSON support
            ("kubectl get pods -o json", Some("kubectl-get")), // supported!
            ("kubectl get pods -o=json", Some("kubectl-get")), // supported!
            ("kubectl get pods --format=json", Some("kubectl-get")), // supported!
            // 13. Collision & specificity
            ("git branch", Some("git-branch")),
            ("git branch -a", Some("git-branch")),
            ("git checkout -b branch", Some("git-broad")), // git-checkout falls back to git-broad
            ("git status", Some("git-broad")),             // git-status falls back to git-broad
            ("git status -s", Some("git-broad")),
            // 14. Complex combined expressions
            (
                "NODE_ENV=production PORT=8080 timeout 30s npx eslint --fix --ext .ts .",
                Some("eslint"),
            ),
            (
                "cd /app && time nice -n 5 pnpm exec eslint --cache",
                Some("eslint"),
            ),
            ("git -C /repo -c k=v diff --stat --verbose", None), // verbose bypasses
            ("kubectl --kubeconfig=config get pods -o yaml", None), // yaml bypasses
            (
                "cd /app && npm install --no-audit | git log --oneline --help",
                None,
            ), // help bypasses
            // 15. More combined and edge cases
            ("git", Some("git-broad")),
            ("git -v diff", None), // version flag bypasses
            ("nice nice nice cargo test", Some("cargo-test")), // nested wrappers
            ("cd app && pushd src && time cargo test", Some("cargo-test")),
            ("npx npx eslint", Some("eslint")),
            ("npx pnpm exec eslint", Some("eslint")),
            ("bunx bunx eslint", Some("eslint")),
            ("timeout 10 timeout 10 pytest", Some("pytest")),
            ("pytest -v --tb=short --json", None),
            ("kubectl get pods -o json -n kube-system --help", None), // help takes priority over JSON support
            ("kubectl get pods -o yaml -n default", None),            // YAML bypass
            ("kubectl get pods -o=yaml --log-level=debug", None),     // debug and YAML bypass
            ("git log --oneline --json", None),                       // no JSON support
            ("eslint --fix --format json", None),                     // no JSON support
        ];

        let mut test_cases: Vec<(String, Option<&str>)> = static_cases
            .into_iter()
            .map(|(cmd, expected)| (cmd.to_string(), expected))
            .collect();

        // 16. Dynamic generated matrix of combinatorial match and bypass variations (4000+ cases)
        let prefixes = [
            "",
            "CI=true",
            "timeout 30s",
            "cd src &&",
            "CI=true nice -n 10 timeout 5",
        ];

        let runners = ["", "npx", "pnpm exec", "uv run"];

        let tools = [
            ("cargo test", vec!["", " --workspace"], "cargo-test"),
            ("git diff", vec!["", " --stat --cached"], "git-diff"),
            ("docker build", vec!["", " -t tag ."], "docker-build"),
            ("pytest", vec!["", " tests/test_auth.py"], "pytest"),
            ("eslint", vec!["", " --fix"], "eslint"),
            ("kubectl get", vec!["", " pods"], "kubectl-get"),
        ];

        let shell_wrappers = ["", "bash -c", "powershell -Command"];

        for prefix in &prefixes {
            for runner in &runners {
                for (tool, args_list, expected) in &tools {
                    for arg in args_list {
                        let mut cmd_parts = Vec::new();
                        if !prefix.is_empty() {
                            cmd_parts.push(*prefix);
                        }
                        if !runner.is_empty() {
                            cmd_parts.push(*runner);
                        }
                        cmd_parts.push(*tool);
                        if !arg.is_empty() {
                            cmd_parts.push(arg.trim());
                        }
                        let base_cmd = cmd_parts.join(" ");

                        // Base match
                        test_cases.push((base_cmd.clone(), Some(*expected)));

                        // Wrapped match
                        for wrapper in &shell_wrappers {
                            if !wrapper.is_empty() {
                                test_cases.push((
                                    format!("{} \"{}\"", wrapper, base_cmd),
                                    Some(*expected),
                                ));
                            }
                        }

                        // Bypass variations
                        let bypass_flags = [
                            "--help",
                            "-h",
                            "--verbose",
                            "--debug",
                            "-vv",
                            "--yaml",
                            "-o yaml",
                        ];

                        for flag in &bypass_flags {
                            let bypass_cmd = format!("{} {}", base_cmd, flag);
                            test_cases.push((bypass_cmd.clone(), None));

                            for wrapper in &shell_wrappers {
                                if !wrapper.is_empty() {
                                    test_cases
                                        .push((format!("{} \"{}\"", wrapper, bypass_cmd), None));
                                }
                            }
                        }

                        // JSON bypass test
                        let json_cmd = format!("{} --json", base_cmd);
                        let json_expected = if *expected == "kubectl-get" {
                            Some("kubectl-get")
                        } else {
                            None
                        };
                        test_cases.push((json_cmd.clone(), json_expected));

                        for wrapper in &shell_wrappers {
                            if !wrapper.is_empty() {
                                test_cases
                                    .push((format!("{} \"{}\"", wrapper, json_cmd), json_expected));
                            }
                        }
                    }
                }
            }
        }

        for (cmd, expected) in &test_cases {
            assert_eq!(
                run(cmd),
                *expected,
                "command resolution failed for: {:?}",
                cmd
            );
        }
    }
}
