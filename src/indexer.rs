use anyhow::Result;
use ignore::WalkBuilder;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::chunker::{chunk_file, file_hash, should_index, Chunk, IGNORED_DIRS};
use crate::embed::embed_documents;
use crate::store::{
    count_stats, delete_chunks_for_file, init_schema, insert_chunk, insert_embedding,
    load_all_file_info, open_db, upsert_file, write_project_name, IndexStats,
};

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

fn mtime_of(path: &Path) -> f64 {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn chunk_only(
    abs_path: &Path,
    rel: &str,
    force: bool,
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
    ChunkedFile {
        rel: rel.to_string(),
        mtime,
        hash: chash,
        chunks: chunk_file(rel, &content),
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

    let existing: Arc<HashMap<String, (i64, f64, String)>> =
        Arc::new(load_all_file_info(&conn)?);

    progress_cb(&format!("discovered {} file(s) — chunking", total));

    // Phase 1: parallel file read + chunk (no embedding)
    let pb = ProgressBar::new(total as u64);
    pb.set_style(
        ProgressStyle::with_template("{bar:40.cyan/blue} {pos}/{len} {msg}")
            .unwrap()
            .progress_chars("=>-"),
    );

    let chunked: Vec<ChunkedFile> = files
        .par_iter()
        .map(|(abs, rel)| {
            let r = chunk_only(abs, rel, force, &existing);
            pb.set_message(rel.to_string());
            pb.inc(1);
            r
        })
        .collect();

    pb.finish_and_clear();

    // Phase 2: collect all texts that need embedding
    // Track which (file_idx, chunk_idx) each embed slot belongs to
    let mut embed_texts: Vec<String> = Vec::new();
    let mut embed_map: Vec<(usize, usize)> = Vec::new(); // (file_idx, chunk_idx)

    for (fi, f) in chunked.iter().enumerate() {
        if f.skipped || f.error.is_some() || f.chunks.is_empty() {
            continue;
        }
        for (ci, chunk) in f.chunks.iter().enumerate() {
            embed_texts.push(format!("{}\n{}", f.rel, chunk.content));
            embed_map.push((fi, ci));
        }
    }

    // Phase 3: batch embed all chunks in one call (model loads once)
    let embeddings = if embed_texts.is_empty() {
        vec![]
    } else {
        progress_cb(&format!("embedding {} chunks via fastembed (ONNX)...", embed_texts.len()));
        embed_documents(&embed_texts)?
    };

    // Phase 4: pair embeddings back with files
    // Build per-file embedding vecs
    let mut file_embeddings: HashMap<usize, Vec<Vec<f32>>> = HashMap::new();
    for (slot, (fi, _ci)) in embed_map.iter().enumerate() {
        file_embeddings
            .entry(*fi)
            .or_default()
            .push(embeddings[slot].clone());
    }

    // Phase 5: write to SQLite
    let mut indexed = 0usize;
    let mut skipped = 0usize;
    let mut errors = 0usize;

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

        let file_embs = match file_embeddings.get(&fi) {
            Some(e) => e,
            None => continue,
        };

        let file_id = match upsert_file(&conn, &f.rel, f.mtime, &f.hash) {
            Ok(id) => id,
            Err(e) => {
                errors += 1;
                progress_cb(&format!("ERR {}: {}", f.rel, e));
                continue;
            }
        };

        let _ = delete_chunks_for_file(&conn, file_id);

        for (chunk, embedding) in f.chunks.iter().zip(file_embs.iter()) {
            let chunk_id = match insert_chunk(
                &conn,
                file_id,
                &f.rel,
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
