use anyhow::Result;
use ignore::WalkBuilder;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use sha2::Digest;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::chunker::{
    chunk_file, count_tokens, file_hash, index_config, redact_secrets, should_index, Chunk,
    IGNORED_DIRS,
};
use crate::embed::embed_documents;
use crate::store::{
    cached_embeddings, count_stats, delete_chunks_for_file, init_schema, insert_chunk,
    insert_embedding, load_all_file_info, open_db, upsert_embedding_cache, upsert_file,
    write_project_name, IndexStats, NewChunk,
};

/// Upper bound on file size to index (1.5 MB). Above this, files are almost
/// always machine-generated (lock files, bundles, data dumps) and only bloat
/// the index. Skipped during the directory walk.
const MAX_INDEX_FILE_BYTES: u64 = 1_500_000;

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
    pub no_embed: bool,
}

struct EmbedJob {
    file_idx: usize,
    chunk_idx: usize,
    cache_key: String,
    text: String,
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

fn chunk_embedding_key(text: &str) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(text.as_bytes());
    // Namespace the cache by model: vectors from different models are not
    // interchangeable, so re-indexing under a new model must not reuse old ones.
    format!(
        "{}:{}",
        crate::embed::active_model_id(),
        hex::encode(hasher.finalize())
    )
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
            let _ = fields.next();
        }

        if x == 'D' || y == 'D' {
            deleted.insert(rel);
            continue;
        }

        let abs = repo_root.join(&rel);
        if abs.is_file() && should_index(&abs) && seen.insert(rel.clone()) {
            files.push((abs, rel));
        }
    }

    Some(FilePlan {
        files,
        deleted,
        git_incremental: true,
    })
}

fn plan_files(
    repo_root: &Path,
    force: bool,
    existing: &HashMap<String, (i64, f64, String)>,
) -> FilePlan {
    if !force && !existing.is_empty() {
        if let Some(plan) = git_changed_files(repo_root) {
            return plan;
        }
    }

    FilePlan {
        files: walk_indexable_files(repo_root),
        deleted: HashSet::new(),
        git_incremental: false,
    }
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

    let content = String::from_utf8_lossy(&raw).into_owned();
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
            no_embed: false,
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
    // Resolve and pin the embedding model for this whole run so documents and the
    // stamped meta agree. Precedence: an explicit TOKENIX_EMBED_MODEL wins; else
    // keep the model the existing index already uses (sticky — a plain re-index
    // must not silently switch models); else the default.
    let prev_model = crate::store::index_model_id(repo_root);
    let explicit_model = std::env::var("TOKENIX_EMBED_MODEL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(|s| crate::embed::spec_for(&s).id.to_string());
    let model_id = explicit_model
        .clone()
        .or_else(|| prev_model.clone())
        .unwrap_or_else(|| crate::embed::DEFAULT_MODEL_ID.to_string());
    crate::embed::set_active_model(&model_id);

    // An explicit model switch makes the existing per-chunk vectors incompatible,
    // so force a full re-embed to keep the index single-model.
    let mut options = options;
    if let (Some(prev), Some(explicit)) = (&prev_model, &explicit_model) {
        if prev != explicit {
            options.force = true;
        }
    }

    let _lock = crate::store::acquire_index_lock(repo_root)?;

    let conn = open_db(repo_root, true)?.unwrap();
    init_schema(&conn, 768)?;
    // One-time int8 migration of legacy f32 rows: pure re-encode, no embedding.
    let migrated = crate::store::backfill_quantized_embeddings(&conn)?;
    if migrated > 0 {
        progress_cb(&format!(
            "quantized {migrated} stored embedding(s) to int8 (4x smaller, same recall)"
        ));
    }
    let existing: Arc<HashMap<String, (i64, f64, String)>> = Arc::new(load_all_file_info(&conn)?);

    let file_plan = plan_files(repo_root, options.force, &existing);
    let files = file_plan.files;

    let total = files.len();
    if total == 0 && file_plan.deleted.is_empty() {
        if file_plan.git_incremental {
            progress_cb("git incremental: no changed files");
        }
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

    // Phase 1: parallel file read + chunk (no embedding)
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

    // Phase 2: collect embeddings, reusing cache by chunk text hash.
    let mut file_embeddings: HashMap<usize, Vec<Option<Vec<f32>>>> = HashMap::new();
    let mut embed_jobs = Vec::new();
    let mut candidate_count = 0usize;

    if options.no_embed {
        progress_cb("skipping embeddings (--no-embed); updating chunks and graph only");
    } else {
        let mut candidate_jobs = Vec::new();
        let mut candidate_keys = Vec::new();

        for (fi, f) in chunked.iter().enumerate() {
            if f.skipped || f.error.is_some() || f.chunks.is_empty() {
                continue;
            }
            let embeddings = vec![None; f.chunks.len()];
            for (ci, chunk) in f.chunks.iter().enumerate() {
                let text = format!("{}\n{}", f.rel, chunk.content);
                let cache_key = chunk_embedding_key(&text);
                candidate_keys.push(cache_key.clone());
                candidate_jobs.push(EmbedJob {
                    file_idx: fi,
                    chunk_idx: ci,
                    cache_key,
                    text,
                });
            }
            file_embeddings.insert(fi, embeddings);
        }

        candidate_count = candidate_keys.len();
        let cached = cached_embeddings(&conn, &candidate_keys)?;
        for job in candidate_jobs {
            if let Some(embedding) = cached.get(&job.cache_key) {
                if let Some(file_embs) = file_embeddings.get_mut(&job.file_idx) {
                    file_embs[job.chunk_idx] = Some(embedding.clone());
                }
            } else {
                embed_jobs.push(job);
            }
        }
    }

    // Phase 3: embed in batches to cap peak memory. ONNX Runtime can allocate
    // large intermediate buffers on Windows, so keep the default conservative
    // and allow local tuning for larger machines.
    let embed_batch = std::env::var("TOKENIX_EMBED_BATCH")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(16);
    let embed_sleep = std::env::var("TOKENIX_EMBED_SLEEP_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let new_embeddings = if embed_jobs.is_empty() {
        vec![]
    } else {
        let cached_hits = candidate_count.saturating_sub(embed_jobs.len());
        // A leftover checkpoint means the previous run died mid-embed. Its
        // completed batches were committed to the embedding cache per batch,
        // so they surface as cache hits here — only the remainder re-embeds.
        if let Some((phase, batches_done)) = read_checkpoint(&conn) {
            if phase == "embed" {
                progress_cb(&format!(
                    "resuming interrupted run: {batches_done} batch(es) already embedded (now cache hits)"
                ));
            }
        }
        progress_cb(&format!(
            "embedding {} uncached chunks ({} cache hits) via fastembed (ONNX), batch size {}...",
            embed_jobs.len(),
            cached_hits,
            embed_batch
        ));
        let embed_pb = ProgressBar::new(embed_jobs.len() as u64);
        embed_pb.set_style(
            ProgressStyle::with_template("{bar:40.cyan/blue} {pos}/{len} chunks (eta {eta}) {msg}")
                .unwrap()
                .progress_chars("=>-"),
        );
        let mut all: Vec<Vec<f32>> = Vec::with_capacity(embed_jobs.len());
        let total_batches = embed_jobs.len().div_ceil(embed_batch);
        for (batch_idx, batch) in embed_jobs.chunks(embed_batch).enumerate() {
            embed_pb.set_message(format!("batch {}/{}", batch_idx + 1, total_batches));
            let texts: Vec<String> = batch.iter().map(|job| job.text.clone()).collect();
            let batch_embs = embed_documents(&texts).map_err(|e| {
                anyhow::anyhow!(
                    "embedding failed at batch {}/{} with {} chunk(s): {}",
                    batch_idx + 1,
                    total_batches,
                    batch.len(),
                    e
                )
            })?;
            // Per-batch durability: commit this batch to the embedding cache
            // now, so a crash/kill loses at most one batch of work. On rerun
            // these chunks resolve as cache hits and are never re-embedded.
            conn.execute_batch("BEGIN IMMEDIATE")?;
            for (job, embedding) in batch.iter().zip(batch_embs.iter()) {
                upsert_embedding_cache(&conn, &job.cache_key, embedding)?;
            }
            conn.execute_batch("COMMIT")?;
            all.extend(batch_embs);
            embed_pb.inc(batch.len() as u64);
            save_checkpoint(&conn, "embed", batch_idx + 1)?;
            if embed_sleep > 0 && batch_idx + 1 < total_batches {
                thread::sleep(Duration::from_millis(embed_sleep));
            }
        }
        embed_pb.finish_and_clear();
        all
    };

    // Phase 4: pair new embeddings back with files (cache was already updated
    // per batch in Phase 3 for crash durability).
    for (job, embedding) in embed_jobs.iter().zip(new_embeddings.iter()) {
        if let Some(file_embs) = file_embeddings.get_mut(&job.file_idx) {
            file_embs[job.chunk_idx] = Some(embedding.clone());
        }
    }

    // Phase 5: write to SQLite in a single transaction
    let mut indexed = 0usize;
    let mut skipped = 0usize;
    let mut errors = 0usize;

    let _ = conn.execute_batch("BEGIN IMMEDIATE");
    for (fi, f) in chunked.iter().enumerate() {
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

        let _ = delete_chunks_for_file(&conn, file_id);

        for (ci, chunk) in f.chunks.iter().enumerate() {
            let chunk_id = match insert_chunk(
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
                Ok(id) => id,
                Err(_) => continue,
            };
            if let Some(Some(embedding)) = file_embeddings.get(&fi).and_then(|embs| embs.get(ci)) {
                let _ = insert_embedding(&conn, chunk_id, embedding);
            }
        }

        indexed += 1;
    }
    let _ = conn.execute_batch("COMMIT");

    // Phase 6: clean up removed files from index
    let mut removed = false;
    if file_plan.git_incremental {
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

    // Clear checkpoint on success
    crate::store::set_meta(&conn, "index_checkpoint", "")?;

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

fn save_checkpoint(conn: &rusqlite::Connection, phase: &str, count: usize) -> Result<()> {
    crate::store::set_meta(conn, "index_checkpoint", &format!("{phase}:{count}"))
}

fn read_checkpoint(conn: &rusqlite::Connection) -> Option<(String, usize)> {
    crate::store::meta_value(conn, "index_checkpoint").and_then(|val| {
        val.split_once(':')
            .and_then(|(p, c)| c.parse().ok().map(|n| (p.to_string(), n)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rel_path() {
        let root = Path::new("/workspace/project");
        let abs = Path::new("/workspace/project/src/main.rs");
        assert_eq!(rel_path(root, abs), "src/main.rs");

        let abs_windows = Path::new("/workspace/project/src\\main.rs");
        assert_eq!(rel_path(root, abs_windows), "src/main.rs");
    }

    #[test]
    fn test_chunk_embedding_key() {
        let text = "hello world";
        let key = chunk_embedding_key(text);
        // "<model-id>:<64-hex>" — namespaced so different models never collide.
        let (model, hash) = key.split_once(':').expect("key has model prefix");
        assert_eq!(model, crate::embed::active_model_id());
        assert_eq!(hash.len(), 64); // SHA-256 hex is 64 chars

        let key2 = chunk_embedding_key(text);
        assert_eq!(key, key2); // deterministic

        // Switching the model changes the namespace, preventing cache reuse.
        crate::embed::set_active_model("bge-small");
        assert!(chunk_embedding_key(text).starts_with("bge-small:"));
        crate::embed::set_active_model(crate::embed::DEFAULT_MODEL_ID);
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
}
