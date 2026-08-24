//! Shareable index snapshots.
//!
//! Indexing is the one expensive thing tokenix does, and today every developer
//! on a team pays it in full on a repo where the answer is identical for all of
//! them. A snapshot is the index itself — compacted, stripped of the local
//! embedding cache, gzipped — small enough to commit next to the code, so the
//! second person onward bootstraps in seconds and only indexes the diff.
//!
//! Two rules keep this honest:
//! * A snapshot never overwrites a *newer* local index without `--force`.
//! * Import stamps nothing: the snapshot's own `git_fingerprint` travels with
//!   it, so `index_staleness` immediately reports the gap between the snapshot's
//!   commit and the local checkout, and `tokenix index` closes it incrementally.

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use rusqlite::Connection;

use crate::store;

/// Where a team snapshot lives by default: inside the repo, next to the code it
/// describes, so it can be committed and reviewed like any other artifact.
pub const DEFAULT_SNAPSHOT_REL: &str = ".tokenix/index.db.gz";

pub struct ExportReport {
    pub output: PathBuf,
    pub bytes: u64,
    pub files: i64,
    pub chunks: i64,
    pub embeddings: i64,
    pub model: String,
    pub head: Option<String>,
}

#[derive(Debug)]
pub struct ImportReport {
    pub source: PathBuf,
    pub target: PathBuf,
    pub backup: Option<PathBuf>,
    pub files: i64,
    pub chunks: i64,
    pub embeddings: i64,
    pub model: String,
    pub created_by: Option<String>,
    pub replaced_newer: bool,
}

fn sql_literal(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "''"))
}

fn counts(conn: &Connection) -> (i64, i64, i64) {
    let one = |sql: &str| conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(0);
    (
        one("SELECT COUNT(*) FROM files"),
        one("SELECT COUNT(*) FROM chunks"),
        one("SELECT COUNT(*) FROM embeddings"),
    )
}

fn indexed_at(conn: &Connection) -> f64 {
    store::meta_value(conn, "indexed_at")
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0)
}

/// Write a compressed, self-contained copy of this repo's index.
pub fn export(repo_root: &Path, output: Option<&Path>) -> Result<ExportReport> {
    let source = store::db_path(repo_root);
    if !source.exists() {
        anyhow::bail!("no index for this repo yet. Run `tokenix index` first.");
    }
    let output = output
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| repo_root.join(DEFAULT_SNAPSHOT_REL));
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }

    // VACUUM INTO gives a compact, WAL-free copy without blocking on the live DB
    // being mid-write, and it refuses to overwrite, so clear any stale temp.
    let staging = output.with_extension(format!("staging-{}", std::process::id()));
    let _ = std::fs::remove_file(&staging);

    let (files, chunks, embeddings, model, head) = {
        let conn = Connection::open(&source)
            .with_context(|| format!("cannot open index at {}", source.display()))?;
        conn.execute_batch(&format!("VACUUM INTO {}", sql_literal(&staging)))
            .context("failed to snapshot the index (VACUUM INTO)")?;

        let staged = Connection::open(&staging)?;
        // The embedding cache is a local accelerator keyed by content hash; it
        // roughly doubles the file and the recipient rebuilds it as they index.
        staged.execute_batch("DELETE FROM embedding_cache;")?;
        store::set_meta(&staged, "snapshot_version", crate::VERSION)?;
        store::set_meta(
            &staged,
            "snapshot_created_at",
            &chrono::Utc::now().to_rfc3339(),
        )?;
        // VACUUM again so the deleted cache pages actually leave the file.
        staged.execute_batch("VACUUM;")?;

        let (files, chunks, embeddings) = counts(&staged);
        let model = store::meta_value(&staged, "embed_model")
            .unwrap_or_else(|| crate::embed::DEFAULT_MODEL_ID.to_string());
        let head = store::meta_value(&staged, "git_fingerprint");
        (files, chunks, embeddings, model, head)
    };

    let mut reader = BufReader::new(File::open(&staging)?);
    let out_file = File::create(&output)
        .with_context(|| format!("cannot write snapshot to {}", output.display()))?;
    let mut encoder = GzEncoder::new(BufWriter::new(out_file), Compression::default());
    std::io::copy(&mut reader, &mut encoder)?;
    encoder.finish()?.into_inner()?.sync_all()?;
    let _ = std::fs::remove_file(&staging);

    let bytes = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0);
    Ok(ExportReport {
        output,
        bytes,
        files,
        chunks,
        embeddings,
        model,
        head,
    })
}

/// Restore a snapshot as this repo's index.
pub fn import(repo_root: &Path, input: Option<&Path>, force: bool) -> Result<ImportReport> {
    let source = input
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| repo_root.join(DEFAULT_SNAPSHOT_REL));
    if !source.exists() {
        anyhow::bail!("snapshot not found: {}", source.display());
    }

    let target = store::db_path(repo_root);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let staging = target.with_extension(format!("import-{}", std::process::id()));
    let _ = std::fs::remove_file(&staging);

    {
        let mut decoder = GzDecoder::new(BufReader::new(File::open(&source)?));
        let mut out = BufWriter::new(File::create(&staging)?);
        std::io::copy(&mut decoder, &mut out)
            .with_context(|| format!("{} is not a valid gzip snapshot", source.display()))?;
    }

    let (files, chunks, embeddings, model, created_by, snapshot_at) = {
        let staged = Connection::open(&staging)
            .with_context(|| format!("{} is not a readable index", source.display()))?;
        // Reject anything that is not actually a tokenix index before it can
        // replace a working one.
        let ok: i64 = staged
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table'
                   AND name IN ('files','chunks','embeddings','meta','graph_nodes')",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if ok < 5 {
            let _ = std::fs::remove_file(&staging);
            anyhow::bail!(
                "{} does not look like a tokenix index (missing core tables)",
                source.display()
            );
        }
        // Bring a snapshot from an older tokenix up to the current schema.
        store::init_schema(&staged, 768)?;
        let (files, chunks, embeddings) = counts(&staged);
        if chunks == 0 {
            let _ = std::fs::remove_file(&staging);
            anyhow::bail!(
                "{} holds no chunks — refusing to import an empty index",
                source.display()
            );
        }
        (
            files,
            chunks,
            embeddings,
            store::meta_value(&staged, "embed_model")
                .unwrap_or_else(|| crate::embed::DEFAULT_MODEL_ID.to_string()),
            store::meta_value(&staged, "snapshot_version"),
            indexed_at(&staged),
        )
    };

    // Never silently discard a local index that is newer than the snapshot.
    let mut replaced_newer = false;
    if target.exists() {
        let local_at = Connection::open(&target)
            .ok()
            .map(|c| indexed_at(&c))
            .unwrap_or(0.0);
        if local_at > snapshot_at {
            if !force {
                let _ = std::fs::remove_file(&staging);
                anyhow::bail!(
                    "the local index is newer than {} — pass --force to replace it anyway",
                    source.display()
                );
            }
            replaced_newer = true;
        }
    }

    let backup = if target.exists() {
        let backup = target.with_extension("pre-import.bak");
        let _ = std::fs::remove_file(&backup);
        std::fs::rename(&target, &backup).with_context(|| {
            format!(
                "cannot move the existing index aside to {}",
                backup.display()
            )
        })?;
        Some(backup)
    } else {
        None
    };
    // A WAL/SHM pair left from the replaced database would be applied on top of
    // the imported file and corrupt it.
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = target.clone().into_os_string();
        sidecar.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(sidecar));
    }
    std::fs::rename(&staging, &target).context("cannot move the imported index into place")?;

    Ok(ImportReport {
        source,
        target,
        backup,
        files,
        chunks,
        embeddings,
        model,
        created_by,
        replaced_newer,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_repo(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tokenix_snapshot_{tag}_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Build a minimal but real index database at `path`.
    fn seed_index(path: &Path, chunk_body: &str, indexed_at_value: f64) {
        let conn = Connection::open(path).unwrap();
        store::init_schema(&conn, 768).unwrap();
        conn.execute(
            "INSERT INTO files(id,path,mtime,content_hash) VALUES(1,'src/a.rs',1.0,'h')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO chunks(id,file_id,path,start_line,end_line,symbol,kind,content,token_count)
             VALUES(1,1,'src/a.rs',1,5,'alpha','function',?1,10)",
            [chunk_body],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO embedding_cache(content_hash,embedding,updated_at) VALUES('x',?1,1.0)",
            [vec![0u8; 4096]],
        )
        .unwrap();
        store::set_meta(&conn, "embed_model", "nomic-v1.5").unwrap();
        store::set_meta(&conn, "indexed_at", &indexed_at_value.to_string()).unwrap();
    }

    #[test]
    fn export_then_import_round_trips_the_index() {
        let dir = temp_repo("roundtrip");
        let db = dir.join("index.db");
        seed_index(&db, "fn alpha() {}", 100.0);

        // Export straight from the seeded database (not via db_path, which is
        // keyed to the real home directory).
        let out = dir.join("snap.db.gz");
        let staging = dir.join("staging.db");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(&format!("VACUUM INTO {}", sql_literal(&staging)))
                .unwrap();
            let staged = Connection::open(&staging).unwrap();
            staged
                .execute_batch("DELETE FROM embedding_cache;")
                .unwrap();
            store::set_meta(&staged, "snapshot_version", "test").unwrap();
        }
        let mut reader = BufReader::new(File::open(&staging).unwrap());
        let mut enc = GzEncoder::new(
            BufWriter::new(File::create(&out).unwrap()),
            Compression::default(),
        );
        std::io::copy(&mut reader, &mut enc).unwrap();
        enc.finish()
            .unwrap()
            .into_inner()
            .unwrap()
            .sync_all()
            .unwrap();

        // Decompress and verify the payload survived intact and cache-free.
        let restored = dir.join("restored.db");
        {
            let mut dec = GzDecoder::new(BufReader::new(File::open(&out).unwrap()));
            let mut w = BufWriter::new(File::create(&restored).unwrap());
            std::io::copy(&mut dec, &mut w).unwrap();
        }
        let conn = Connection::open(&restored).unwrap();
        let (files, chunks, _) = counts(&conn);
        assert_eq!((files, chunks), (1, 1));
        let cached: i64 = conn
            .query_row("SELECT COUNT(*) FROM embedding_cache", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cached, 0, "the local embedding cache must not ship");
        let body: String = conn
            .query_row("SELECT content FROM chunks WHERE id=1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(body, "fn alpha() {}");
        assert_eq!(
            store::meta_value(&conn, "embed_model").as_deref(),
            Some("nomic-v1.5")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_rejects_a_file_that_is_not_an_index() {
        let dir = temp_repo("garbage");
        let bogus = dir.join("bogus.db.gz");
        {
            let mut enc = GzEncoder::new(
                BufWriter::new(File::create(&bogus).unwrap()),
                Compression::default(),
            );
            std::io::Write::write_all(&mut enc, b"definitely not sqlite").unwrap();
            enc.finish().unwrap();
        }
        let err = import(&dir, Some(&bogus), false).unwrap_err().to_string();
        assert!(
            err.contains("does not look like a tokenix index")
                || err.contains("not a readable index"),
            "unexpected error: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_rejects_non_gzip_input() {
        let dir = temp_repo("plain");
        let plain = dir.join("plain.db.gz");
        std::fs::write(&plain, b"not gzip at all").unwrap();
        assert!(import(&dir, Some(&plain), false).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_snapshot_is_a_clear_error() {
        let dir = temp_repo("missing");
        let err = import(&dir, Some(&dir.join("nope.gz")), false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("snapshot not found"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sql_literal_escapes_quotes() {
        assert_eq!(
            sql_literal(Path::new("/tmp/it's.db")),
            "'/tmp/it''s.db'".to_string()
        );
    }
}
