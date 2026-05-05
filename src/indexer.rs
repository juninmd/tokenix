use anyhow::Result;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

use crate::chunker::{chunk_file, file_hash, should_index};
use crate::embed::get_embedding;
use crate::store::{
    count_stats, delete_chunks_for_file, get_file_info, init_schema, insert_chunk,
    insert_embedding, open_db, upsert_file, IndexStats,
};

const OLLAMA_URL: &str = "http://localhost:11434";

#[allow(dead_code)]
pub struct IndexResult {
    pub total: usize,
    pub indexed: usize,
    pub skipped: usize,
    pub errors: usize,
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

    let files: Vec<(PathBuf, String)> = WalkDir::new(repo_root)
        .into_iter()
        .filter_entry(|e| {
            // For directories: only skip explicitly ignored dirs
            if e.file_type().is_dir() {
                let name = e.file_name().to_string_lossy();
                return !crate::chunker::IGNORED_DIRS.contains(&name.as_ref());
            }
            true
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
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
    let mut indexed = 0;
    let mut skipped = 0;
    let mut errors = 0;

    for (i, (abs_path, rel)) in files.iter().enumerate() {
        progress_cb(&format!("[{}/{}] {}", i + 1, total, rel));

        let raw = match std::fs::read(abs_path) {
            Ok(b) => b,
            Err(e) => {
                errors += 1;
                progress_cb(&format!("ERR {}: {}", rel, e));
                continue;
            }
        };

        let stat = match std::fs::metadata(abs_path) {
            Ok(m) => m,
            Err(e) => {
                errors += 1;
                progress_cb(&format!("ERR {}: {}", rel, e));
                continue;
            }
        };

        let mtime = stat
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);

        let chash = file_hash(&raw);

        if !force {
            if let Ok(Some((_file_id, stored_mtime, stored_hash))) = get_file_info(&conn, rel) {
                if (stored_mtime - mtime).abs() < 0.01 && stored_hash == chash {
                    skipped += 1;
                    continue;
                }
            }
        }

        let content = String::from_utf8_lossy(&raw).into_owned();
        let file_id = match upsert_file(&conn, rel, mtime, &chash) {
            Ok(id) => id,
            Err(e) => {
                errors += 1;
                progress_cb(&format!("ERR {}: {}", rel, e));
                continue;
            }
        };

        let _ = delete_chunks_for_file(&conn, file_id);
        let chunks = chunk_file(rel, &content);

        for chunk in &chunks {
            let chunk_id = match insert_chunk(
                &conn,
                file_id,
                rel,
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

            let embed_text = format!("file:{}\n{}", rel, chunk.content);
            match get_embedding(&embed_text, model, OLLAMA_URL) {
                Ok(emb) => {
                    let _ = insert_embedding(&conn, chunk_id, &emb);
                }
                Err(e) => {
                    progress_cb(&format!("EMBED ERR {}: {}", rel, e));
                }
            }
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
