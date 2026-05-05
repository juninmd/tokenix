use anyhow::Result;
use ignore::WalkBuilder;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::chunker::{chunk_file, file_hash, should_index, Chunk, IGNORED_DIRS};
use crate::embed::get_embeddings_batch;
use crate::store::{
    count_stats, delete_chunks_for_file, init_schema, insert_chunk, insert_embedding,
    load_all_file_info, open_db, upsert_file, write_project_name, IndexStats,
};

const OLLAMA_URL: &str = "http://localhost:11434";

#[allow(dead_code)]
pub struct IndexResult {
    pub total: usize,
    pub indexed: usize,
    pub skipped: usize,
    pub errors: usize,
}

struct ProcessedFile {
    rel: String,
    mtime: f64,
    hash: String,
    chunks: Vec<(Chunk, Vec<f32>)>,
    skipped: bool,
    error: Option<String>,
}

fn mtime_of(path: &Path) -> f64 {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn process_file(
    abs_path: &Path,
    rel: &str,
    model: &str,
    ollama_url: &str,
    force: bool,
    existing: &HashMap<String, (i64, f64, String)>,
) -> ProcessedFile {
    let raw = match std::fs::read(abs_path) {
        Ok(b) => b,
        Err(e) => {
            return ProcessedFile {
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
                return ProcessedFile {
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
    let chunks = chunk_file(rel, &content);

    if chunks.is_empty() {
        return ProcessedFile {
            rel: rel.to_string(),
            mtime,
            hash: chash,
            chunks: vec![],
            skipped: false,
            error: None,
        };
    }

    let embed_texts: Vec<String> = chunks
        .iter()
        .map(|c| format!("file:{}\n{}", rel, c.content))
        .collect();

    match get_embeddings_batch(&embed_texts, model, ollama_url) {
        Ok(embeddings) => {
            let paired = chunks.into_iter().zip(embeddings).collect();
            ProcessedFile {
                rel: rel.to_string(),
                mtime,
                hash: chash,
                chunks: paired,
                skipped: false,
                error: None,
            }
        }
        Err(e) => ProcessedFile {
            rel: rel.to_string(),
            mtime,
            hash: chash,
            chunks: vec![],
            skipped: false,
            error: Some(format!("embed: {}", e)),
        },
    }
}

pub fn index_repo<F>(
    repo_root: &Path,
    model: &str,
    force: bool,
    mut progress_cb: F,
) -> Result<(IndexResult, IndexStats)>
where
    F: FnMut(&str),
{
    let conn = open_db(repo_root, true)?.unwrap();
    init_schema(&conn, 768)?;

    let files: Vec<(PathBuf, String)> = WalkBuilder::new(repo_root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .filter_entry(|e| {
            if e.file_type().map_or(false, |t| t.is_dir()) {
                let name = e.file_name().to_string_lossy();
                return !IGNORED_DIRS.contains(&name.as_ref());
            }
            true
        })
        .build()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map_or(false, |t| t.is_file()))
        .filter(|e| should_index(e.path()))
        .map(|e| {
            let abs = e.into_path();
            let rel = abs
                .strip_prefix(repo_root)
                .unwrap_or(&abs)
                .to_string_lossy()
                .replace('\\', "/");
            (abs, rel)
        })
        .collect();

    let total = files.len();
    if total == 0 {
        let stats = count_stats(&conn)?;
        return Ok((IndexResult { total: 0, indexed: 0, skipped: 0, errors: 0 }, stats));
    }

    // Pre-load existing file metadata for skip detection (read-only, shared across threads)
    let existing: Arc<HashMap<String, (i64, f64, String)>> =
        Arc::new(load_all_file_info(&conn)?);

    progress_cb(&format!(
        "discovered {} file(s) — embedding with batch size {}",
        total,
        crate::embed::MAX_BATCH_SIZE
    ));

    let pb = ProgressBar::new(total as u64);
    pb.set_style(
        ProgressStyle::with_template("{bar:40.cyan/blue} {pos}/{len} {msg}")
            .unwrap()
            .progress_chars("=>-"),
    );

    // Parallel: read → chunk → batch-embed (one Ollama call per file)
    let results: Vec<ProcessedFile> = files
        .par_iter()
        .map(|(abs, rel)| {
            let r = process_file(abs, rel, model, OLLAMA_URL, force, &existing);
            pb.set_message(rel.to_string());
            pb.inc(1);
            r
        })
        .collect();

    pb.finish_and_clear();

    // Serial: write to SQLite
    let mut indexed = 0usize;
    let mut skipped = 0usize;
    let mut errors = 0usize;

    for result in results {
        if result.skipped {
            skipped += 1;
            continue;
        }
        if let Some(ref e) = result.error {
            errors += 1;
            progress_cb(&format!("ERR {}: {}", result.rel, e));
            continue;
        }

        let file_id = match upsert_file(&conn, &result.rel, result.mtime, &result.hash) {
            Ok(id) => id,
            Err(e) => {
                errors += 1;
                progress_cb(&format!("ERR {}: {}", result.rel, e));
                continue;
            }
        };

        let _ = delete_chunks_for_file(&conn, file_id);

        for (chunk, embedding) in &result.chunks {
            let chunk_id = match insert_chunk(
                &conn,
                file_id,
                &result.rel,
                chunk.start_line,
                chunk.end_line,
                &chunk.symbol,
                &chunk.kind,
                &chunk.content,
                chunk.token_count,
            ) {
                Ok(id) => id,
                Err(_) => continue,
            };
            let _ = insert_embedding(&conn, chunk_id, embedding);
        }

        indexed += 1;
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    conn.execute(
        "INSERT OR REPLACE INTO meta(key,value) VALUES('indexed_at',?1)",
        rusqlite::params![now.to_string()],
    )?;

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
