use anyhow::{Context, Result};
use ignore::WalkBuilder;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::chunker::{
    chunk_file, count_tokens, file_hash, index_config, redact_secrets, should_index, Chunk,
    IGNORED_DIRS,
};
use crate::store::{
    count_stats, delete_chunks_for_file, init_schema, insert_chunk, load_all_file_info, open_db,
    upsert_file, write_project_name, IndexStats, NewChunk,
};

/// Upper bound on file size to index (1.5 MB). Above this, files are almost
/// always machine-generated (lock files, bundles, data dumps) and only bloat
/// the index. Skipped during the directory walk.
pub(crate) const MAX_INDEX_FILE_BYTES: u64 = 1_500_000;

#[allow(dead_code)]
pub struct IndexResult {
    pub total: usize,
    pub indexed: usize,
    pub skipped: usize,
    pub errors: usize,
}

struct ChunkedFile {
    rel: String,
    mtime: f64,
    hash: String,
    chunks: Vec<Chunk>,
    skipped: bool,
    error: Option<String>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct IndexOptions {
    pub force: bool,
    /// Inline pre-query refresh: must not apply deletions (see phase 6).
    pub inline: bool,
}

struct FilePlan {
    files: Vec<(PathBuf, String)>,
    deleted: HashSet<String>,
    git_incremental: bool,
}

/// Drop the process to below-normal CPU priority so a long index run does not
/// starve interactive use. Deliberately NOT `PROCESS_MODE_BACKGROUND_BEGIN`:
/// that also demotes I/O and memory priority and can slow indexing ~10x.
pub fn lower_process_priority() {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Threading::{
            GetCurrentProcess, SetPriorityClass, BELOW_NORMAL_PRIORITY_CLASS,
        };
        SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS);
    }
    #[cfg(unix)]
    unsafe {
        // Errors (e.g. already niced) are harmless; scheduling stays as-is.
        let _ = libc::nice(10);
    }
}

fn mtime_of(path: &Path) -> f64 {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Modification time of a path as the index stores it (0.0 when unreadable).
pub fn file_mtime(path: &Path) -> f64 {
    mtime_of(path)
}

/// `((absolute, repo-relative) changed files, repo-relative deleted files)`.
pub type DirtyPaths = (Vec<(PathBuf, String)>, HashSet<String>);

/// Files git reports as changed/untracked in the working tree, plus the paths
/// it reports as deleted. `None` when this is not a git repo or git failed.
pub fn git_dirty_paths(repo_root: &Path) -> Option<DirtyPaths> {
    let plan = git_changed_files(repo_root)?;
    Some((plan.files, plan.deleted))
}

/// Whether indexing `abs` writes a `files` row at all. Empty, binary and
/// sub-`MIN_CHUNK_TOKENS` files never get one, so a caller waiting for that
/// row would wait forever.
pub fn stores_content(abs: &Path, rel: &str) -> bool {
    std::fs::read(abs)
        .ok()
        .and_then(|raw| decode_text(&raw))
        .is_some_and(|text| !chunk_file(rel, &text).is_empty())
}

fn within_size_cap(abs: &Path, max_bytes: u64) -> bool {
    std::fs::metadata(abs).is_ok_and(|m| m.len() <= max_bytes)
}

fn rel_path(repo_root: &Path, abs: &Path) -> String {
    abs.strip_prefix(repo_root)
        .unwrap_or(abs)
        .to_string_lossy()
        .replace('\\', "/")
}

fn walk_indexable_files(repo_root: &Path) -> Vec<(PathBuf, String)> {
    // Allow projects to raise/lower the cap via `[index] max_file_bytes`.
    let max_bytes = index_config()
        .max_file_bytes
        .unwrap_or(MAX_INDEX_FILE_BYTES);
    WalkBuilder::new(repo_root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        // Skip oversized files (generated lock files, bundled/minified data, etc.).
        // Hand-written source is virtually never this large; this avoids wasting
        // embedding budget on machine-generated noise.
        .max_filesize(Some(max_bytes))
        .filter_entry(|e| {
            if e.file_type().is_some_and(|t| t.is_dir()) {
                let name = e.file_name().to_string_lossy();
                return !IGNORED_DIRS.contains(&name.as_ref());
            }
            true
        })
        .build()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .filter(|e| should_index(e.path()))
        .map(|e| {
            let abs = e.into_path();
            let rel = rel_path(repo_root, &abs);
            (abs, rel)
        })
        .collect()
}

fn git_changed_files(repo_root: &Path) -> Option<FilePlan> {
    let output = Command::new("git")
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let max_bytes = index_config()
        .max_file_bytes
        .unwrap_or(MAX_INDEX_FILE_BYTES);
    let mut files = Vec::new();
    let mut deleted = HashSet::new();
    let mut seen = HashSet::new();
    let mut fields = output.stdout.split(|b| *b == 0).filter(|f| !f.is_empty());

    while let Some(field) = fields.next() {
        if field.len() < 4 {
            continue;
        }
        let x = field[0] as char;
        let y = field[1] as char;
        let rel = String::from_utf8_lossy(&field[3..]).replace('\\', "/");

        if x == 'R' || x == 'C' {
            // The next NUL field is the origin: a rename removes it, a copy keeps it.
            let origin = fields.next();
            if let (Some(o), 'R') = (origin, x) {
                deleted.insert(String::from_utf8_lossy(o).replace('\\', "/"));
            }
        }

        if x == 'D' || y == 'D' {
            deleted.insert(rel);
            continue;
        }

        let abs = repo_root.join(&rel);
        if abs.is_file()
            && should_index(&abs)
            && within_size_cap(&abs, max_bytes)
            && seen.insert(rel.clone())
        {
            files.push((abs, rel));
        }
    }

    Some(FilePlan {
        files,
        deleted,
        git_incremental: true,
    })
}

/// Tracked files that exist on disk but have no row in the index.
///
/// This is how a file comes back. A deletion drops the file's rows, and a later
/// `git checkout`/`git stash pop`/branch switch restores a file whose content
/// matches HEAD — so `git status` says nothing and a purely status-driven plan
/// would never look at it again. The index would stay silently short one file
/// until someone ran `--force`.
fn restored_files(
    repo_root: &Path,
    existing: &HashMap<String, (i64, f64, String)>,
    already: &[(PathBuf, String)],
) -> Vec<(PathBuf, String)> {
    let Some(out) = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(repo_root)
        .output()
        .ok()
        .filter(|o| o.status.success())
    else {
        return Vec::new();
    };
    let max_bytes = index_config()
        .max_file_bytes
        .unwrap_or(MAX_INDEX_FILE_BYTES);
    let planned: HashSet<&str> = already.iter().map(|(_, rel)| rel.as_str()).collect();

    let mut extra: Vec<(PathBuf, String)> = out
        .stdout
        .split(|b| *b == 0)
        .filter(|f| !f.is_empty())
        .map(|f| String::from_utf8_lossy(f).replace('\\', "/"))
        .filter(|rel| !existing.contains_key(rel.as_str()) && !planned.contains(rel.as_str()))
        .map(|rel| (repo_root.join(&rel), rel))
        .filter(|(abs, _)| abs.is_file() && should_index(abs) && within_size_cap(abs, max_bytes))
        .collect();
    extra.sort_by(|a, b| a.1.cmp(&b.1));
    extra
}

fn plan_files(
    repo_root: &Path,
    options: IndexOptions,
    existing: &HashMap<String, (i64, f64, String)>,
) -> FilePlan {
    if !options.force && !existing.is_empty() {
        if let Some(mut plan) = git_changed_files(repo_root) {
            // Only a full run reconciles these, and only it should pay for them:
            // an inline refresh runs on a query, where the extra
            // work is unbounded and the backlog would not shrink anyway.
            if !options.inline {
                plan.files
                    .extend(restored_files(repo_root, existing, &plan.files));
            }
            return plan;
        }
    }

    FilePlan {
        files: walk_indexable_files(repo_root),
        deleted: HashSet::new(),
        git_incremental: false,
    }
}

/// Decode file bytes for chunking. Handles UTF-16 BOMs (SSMS and other Windows
/// tools save `.sql` as UTF-16 LE), falls back to lossy UTF-8, and returns
/// `None` for binary content (a NUL byte in the first 8 KiB, like git's check).
fn decode_text(raw: &[u8]) -> Option<String> {
    if raw.starts_with(&[0xFF, 0xFE]) {
        let units: Vec<u16> = raw[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        return Some(String::from_utf16_lossy(&units));
    }
    if raw.starts_with(&[0xFE, 0xFF]) {
        let units: Vec<u16> = raw[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes(*c))
            .collect();
        return Some(String::from_utf16_lossy(&units));
    }
    if raw.iter().take(8192).any(|&b| b == 0) {
        return None;
    }
    Some(String::from_utf8_lossy(raw).into_owned())
}

fn chunk_only(
    abs_path: &Path,
    rel: &str,
    force: bool,
    redact: bool,
    existing: &HashMap<String, (i64, f64, String)>,
) -> ChunkedFile {
    let raw = match std::fs::read(abs_path) {
        Ok(b) => b,
        Err(e) => {
            return ChunkedFile {
                rel: rel.to_string(),
                mtime: 0.0,
                hash: String::new(),
                chunks: vec![],
                skipped: false,
                error: Some(e.to_string()),
            }
        }
    };

    let mtime = mtime_of(abs_path);
    let chash = file_hash(&raw);

    if !force {
        if let Some((_id, stored_mtime, stored_hash)) = existing.get(rel) {
            if (stored_mtime - mtime).abs() < 0.01 && stored_hash == &chash {
                return ChunkedFile {
                    rel: rel.to_string(),
                    mtime,
                    hash: chash,
                    chunks: vec![],
                    skipped: true,
                    error: None,
                };
            }
        }
    }

    let Some(content) = decode_text(&raw) else {
        // Binary file (e.g. an extension added via `[index] extensions` that
        // turns out not to be text): chunking the lossy-decoded bytes would
        // only pollute the index, so yield nothing.
        return ChunkedFile {
            rel: rel.to_string(),
            mtime,
            hash: chash,
            chunks: vec![],
            skipped: false,
            error: None,
        };
    };
    let mut chunks = chunk_file(rel, &content);
    if redact {
        for c in &mut chunks {
            let masked = redact_secrets(&c.content);
            if masked != c.content {
                c.token_count = count_tokens(&masked);
                c.content = masked;
            }
        }
    }
    ChunkedFile {
        rel: rel.to_string(),
        mtime,
        hash: chash,
        chunks,
        skipped: false,
        error: None,
    }
}

pub fn index_repo<F>(
    repo_root: &Path,
    force: bool,
    mut progress_cb: F,
) -> Result<(IndexResult, IndexStats)>
where
    F: FnMut(&str),
{
    index_repo_with_options(
        repo_root,
        IndexOptions {
            force,
            inline: false,
        },
        &mut progress_cb,
    )
}

pub fn index_repo_with_options<F>(
    repo_root: &Path,
    options: IndexOptions,
    progress_cb: &mut F,
) -> Result<(IndexResult, IndexStats)>
where
    F: FnMut(&str),
{
    let _lock = crate::store::acquire_index_lock(repo_root)?;

    let conn = open_db(repo_root, true)?.unwrap();
    init_schema(&conn, 768)?;
    if !options.inline {
        match crate::store::vacuum_if_flagged(&conn) {
            Ok(true) => progress_cb("reclaimed the space of the removed embedding tables (VACUUM)"),
            Ok(false) => {}
            Err(e) => progress_cb(&format!(
                "warning: VACUUM failed, will retry next index: {e}"
            )),
        }
    }
    let existing: Arc<HashMap<String, (i64, f64, String)>> = Arc::new(load_all_file_info(&conn)?);

    let file_plan = plan_files(repo_root, options, &existing);
    let files = file_plan.files;

    let total = files.len();
    if total == 0 && file_plan.deleted.is_empty() {
        if file_plan.git_incremental {
            progress_cb("git incremental: no changed files");
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let _ = crate::store::write_index_meta(&conn, repo_root, now);

        let stats = count_stats(&conn)?;
        return Ok((
            IndexResult {
                total: 0,
                indexed: 0,
                skipped: 0,
                errors: 0,
            },
            stats,
        ));
    }

    if file_plan.git_incremental {
        progress_cb(&format!(
            "git incremental: {} changed file(s), {} deleted file(s)",
            total,
            file_plan.deleted.len()
        ));
    } else {
        progress_cb(&format!("discovered {} file(s) — chunking", total));
    }

    // Phase 1: parallel file read + chunk
    let pb = ProgressBar::new(total as u64);
    pb.set_style(
        ProgressStyle::with_template("{bar:40.cyan/blue} {pos}/{len} {msg}")
            .unwrap()
            .progress_chars("=>-"),
    );

    let redact = index_config().redact_secrets;
    let chunked: Vec<ChunkedFile> = files
        .par_iter()
        .map(|(abs, rel)| {
            let r = chunk_only(abs, rel, options.force, redact, &existing);
            pb.inc(1);
            r
        })
        .collect();

    pb.finish_and_clear();

    // Phase 5: write to SQLite in a single transaction
    let mut indexed = 0usize;
    let mut skipped = 0usize;
    let mut errors = 0usize;
    let mut removed = false;

    // Surface transaction failures instead of swallowing them: a failed BEGIN
    // means every write below runs in autocommit, and a failed COMMIT means the
    // whole phase was rolled back while the run still reported success.
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("failed to open the index write transaction")?;
    for f in &chunked {
        if f.skipped {
            skipped += 1;
            continue;
        }
        if let Some(ref e) = f.error {
            errors += 1;
            progress_cb(&format!("ERR {}: {}", f.rel, e));
            continue;
        }
        if f.chunks.is_empty() {
            // The file exists but yields nothing (emptied, binary, sub-minimum):
            // its old rows would keep serving content that is no longer on disk.
            if let Some((file_id, _, _)) = existing.get(&f.rel) {
                match crate::store::delete_file(&conn, *file_id) {
                    Ok(()) => removed = true,
                    Err(e) => {
                        errors += 1;
                        progress_cb(&format!("ERR {}: could not drop stale rows: {e}", f.rel));
                    }
                }
            }
            continue;
        }

        let file_id = match upsert_file(&conn, &f.rel, f.mtime, &f.hash) {
            Ok(id) => id,
            Err(e) => {
                errors += 1;
                progress_cb(&format!("ERR {}: {}", f.rel, e));
                continue;
            }
        };

        // Skip the re-insert if the old rows could not be cleared: inserting on
        // top of them leaves duplicate chunks and orphaned graph edges in the
        // index (FK cascades are not enabled), which is worse than a stale file.
        if let Err(e) = delete_chunks_for_file(&conn, file_id) {
            errors += 1;
            progress_cb(&format!("ERR {}: could not clear old chunks: {e}", f.rel));
            continue;
        }

        for chunk in &f.chunks {
            if let Err(e) = insert_chunk(
                &conn,
                NewChunk {
                    file_id,
                    path: &f.rel,
                    start: chunk.start_line,
                    end: chunk.end_line,
                    symbol: &chunk.symbol,
                    kind: &chunk.kind,
                    content: &chunk.content,
                    token_count: chunk.token_count,
                },
            ) {
                errors += 1;
                progress_cb(&format!("ERR {}: could not insert chunk: {e}", f.rel));
            }
        }

        indexed += 1;
    }
    conn.execute_batch("COMMIT")
        .context("failed to commit the index write transaction")?;

    // Phase 6: clean up removed files from index.
    //
    // An inline refresh (`inline`) deliberately skips this. It runs on every
    // query, so it would see a file that is momentarily absent — mid-rebase,
    // mid-stash, a checkout in flight — and drop its rows; when the file comes
    // back unchanged, git reports nothing and the index would stay short one
    // file. Leaving a deleted file's rows in place until the next full index is
    // the cheaper mistake: stale hits, not missing ones.
    if options.inline {
        // nothing to do
    } else if file_plan.git_incremental {
        for rel_path in &file_plan.deleted {
            if let Some((file_id, _, _)) = existing.get(rel_path) {
                progress_cb(&format!("removing deleted file from index: {}", rel_path));
                let _ = crate::store::delete_file(&conn, *file_id);
                removed = true;
            }
        }
    } else {
        let walked_files: HashSet<&str> = files.iter().map(|(_, r)| r.as_str()).collect();
        for (rel_path, (file_id, _, _)) in existing.iter() {
            if !walked_files.contains(rel_path.as_str()) {
                progress_cb(&format!("removing deleted file from index: {}", rel_path));
                let _ = crate::store::delete_file(&conn, *file_id);
                removed = true;
            }
        }
    }

    if indexed > 0 || removed {
        let changed_paths: Vec<String> = chunked
            .iter()
            .filter(|f| !f.skipped && f.error.is_none() && !f.chunks.is_empty())
            .map(|f| f.rel.clone())
            .collect();
        // Incremental repair is worth it for small change sets on git runs;
        // big batches (or full walks) fall back to the simpler full rebuild.
        let small_change = changed_paths.len() * 5 < existing.len().max(1);
        if file_plan.git_incremental && small_change && !changed_paths.is_empty() && !removed {
            progress_cb(&format!(
                "updating symbol graph incrementally ({} changed file(s))...",
                changed_paths.len()
            ));
            crate::graph::update_symbol_graph_incremental(&conn, &changed_paths)?;
        } else {
            progress_cb("rebuilding symbol graph...");
            crate::graph::rebuild_symbol_graph(&conn)?;
        }
        let import_edges = crate::graph::rebuild_import_graph(&conn, repo_root)?;
        progress_cb(&format!("import graph: {import_edges} file-level edge(s)"));
    } else {
        progress_cb("no changes — skipping graph rebuild");
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    crate::store::write_index_meta(&conn, repo_root, now)?;

    let _ = write_project_name(repo_root);

    let stats = count_stats(&conn)?;
    Ok((
        IndexResult {
            total,
            indexed,
            skipped,
            errors,
        },
        stats,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_text_handles_utf8_utf16_and_binary() {
        // Plain UTF-8 passes through.
        assert_eq!(decode_text(b"SELECT 1;").as_deref(), Some("SELECT 1;"));

        // UTF-16 LE with BOM (SSMS-style .sql) decodes to the same text.
        let mut utf16le = vec![0xFF, 0xFE];
        for u in "CREATE VIEW saldo".encode_utf16() {
            utf16le.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(decode_text(&utf16le).as_deref(), Some("CREATE VIEW saldo"));

        // UTF-16 BE with BOM.
        let mut utf16be = vec![0xFE, 0xFF];
        for u in "Sub Main()".encode_utf16() {
            utf16be.extend_from_slice(&u.to_be_bytes());
        }
        assert_eq!(decode_text(&utf16be).as_deref(), Some("Sub Main()"));

        // Binary content (VB6 .frx-style, NUL bytes without a BOM) is skipped.
        assert_eq!(decode_text(&[0x01, 0x00, 0x42, 0x00, 0x00, 0x10]), None);
    }

    #[test]
    fn test_rel_path() {
        let root = Path::new("/workspace/project");
        let abs = Path::new("/workspace/project/src/main.rs");
        assert_eq!(rel_path(root, abs), "src/main.rs");

        let abs_windows = Path::new("/workspace/project/src\\main.rs");
        assert_eq!(rel_path(root, abs_windows), "src/main.rs");
    }

    #[test]
    fn test_mtime_of() {
        let temp_dir = std::env::temp_dir()
            .join("tokenix_test_indexer")
            .join(format!("mtime_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let file_path = temp_dir.join("test_mtime.txt");
        std::fs::write(&file_path, "test").unwrap();

        let mtime = mtime_of(&file_path);
        assert!(mtime > 0.0);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("tokenix_test_indexer")
            .join(format!("{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn stores_content_is_false_for_files_that_never_get_a_row() {
        let dir = scratch_dir("stores");
        std::fs::write(dir.join("empty.md"), "").unwrap();
        std::fs::write(dir.join("blob.rs"), [0x01, 0x00, 0x42, 0x00, 0x00, 0x10]).unwrap();
        let code = "pub fn invoice_total(items: &[u32]) -> u32 {\n    items.iter().sum()\n}\n";
        std::fs::write(dir.join("lib.rs"), code).unwrap();

        // The inline refresh counted these as new on every query, forever.
        assert!(!stores_content(&dir.join("empty.md"), "empty.md"));
        assert!(!stores_content(&dir.join("blob.rs"), "blob.rs"));
        assert!(stores_content(&dir.join("lib.rs"), "lib.rs"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn git_plan_skips_files_above_the_size_cap() {
        let dir = scratch_dir("git_cap");
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&dir)
                .output()
                .expect("git available")
        };
        assert!(git(&["init", "-q"]).status.success());
        std::fs::write(dir.join("small.rs"), "fn small() {}\n").unwrap();
        let line = "fn padding() {}\n";
        let big = line.repeat(MAX_INDEX_FILE_BYTES as usize / line.len() + 1);
        std::fs::write(dir.join("big.rs"), big).unwrap();

        // The full walk skips oversized files whole; the git-incremental plan
        // used to index them anyway.
        let plan = git_changed_files(&dir).expect("git repo");
        let rels: Vec<&str> = plan.files.iter().map(|(_, rel)| rel.as_str()).collect();
        assert!(rels.contains(&"small.rs"), "{rels:?}");
        assert!(!rels.contains(&"big.rs"), "{rels:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
