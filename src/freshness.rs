//! Freshness-on-query: bring the index up to date with the working tree before
//! a retrieval command answers.
//!
//! The index is built from committed *and* working-tree state, so an edit made
//! after the last `tokenix index` silently makes every retrieval answer stale —
//! `query` returns the old body of a function the agent just rewrote. The hook
//! already fails open when the whole index is stale, but a CLI/MCP call has no
//! such escape: it answers confidently from old rows.
//!
//! This module closes that window for the common case (a handful of dirty
//! files) by re-chunking just those files **without embedding them**: chunk
//! text lands in the FTS5 side of hybrid search and the symbol graph is
//! repaired, which costs milliseconds and no model load. Files touched this way
//! are stamped with a sentinel content hash (see `indexer::NO_EMBED_HASH_PREFIX`)
//! so the next real `tokenix index` re-embeds them instead of skipping them as
//! unchanged.
//!
//! Everything here fails open: any error, lock contention, or a change set too
//! large to be cheap leaves the existing index untouched and the command
//! answers exactly as it did before.

use std::path::Path;
use std::time::Instant;

use crate::indexer::{self, IndexOptions};
use crate::store;

/// Refuse to auto-refresh past this many dirty files: beyond it the run stops
/// being "a few milliseconds" and a query would stall behind a real index.
pub const DEFAULT_MAX_FILES: usize = 25;

pub struct RefreshReport {
    pub files: usize,
    pub deleted: usize,
    pub elapsed_ms: u128,
}

pub enum Refresh {
    /// `TOKENIX_AUTO_REFRESH=0`.
    Disabled,
    /// No index yet, or it could not be opened — nothing to refresh.
    NoIndex,
    /// Not a git repo (or git is unavailable): no cheap way to find dirty files.
    NotGit,
    /// The working tree matches what the index already holds.
    Clean,
    /// Too many dirty files for an inline refresh.
    TooBig {
        files: usize,
        cap: usize,
    },
    /// An index run is already in progress, or the refresh itself failed.
    Failed(String),
    Refreshed(RefreshReport),
}

fn env_flag_disabled(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| {
        let v = v.trim().to_ascii_lowercase();
        v == "0" || v == "false" || v == "off" || v == "no"
    })
}

fn max_files() -> usize {
    std::env::var("TOKENIX_AUTO_REFRESH_MAX")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_FILES)
}

/// Re-chunk working-tree changes into the existing index, if that is cheap.
///
/// Never returns an error: the caller's command must run either way.
pub fn refresh_before_query(repo_root: &Path) -> Refresh {
    if env_flag_disabled("TOKENIX_AUTO_REFRESH") {
        return Refresh::Disabled;
    }

    let dirty = {
        let conn = match store::open_db(repo_root, false) {
            Ok(Some(c)) => c,
            _ => return Refresh::NoIndex,
        };
        // Never refresh an index that has no `indexed_at`: it is half-written or
        // from a crashed run, and a partial refresh would stamp it as complete.
        if store::meta_value(&conn, "indexed_at").is_none() {
            return Refresh::NoIndex;
        }
        let Some((changed, deleted)) = indexer::git_dirty_paths(repo_root) else {
            return Refresh::NotGit;
        };
        let Ok(existing) = store::load_all_file_info(&conn) else {
            return Refresh::NoIndex;
        };

        let files = changed
            .iter()
            .filter(|(abs, rel)| match existing.get(rel.as_str()) {
                // Known file: an mtime match means the index already holds this
                // body. Content is not hashed here — the indexer does that and
                // skips a no-op re-chunk on its own.
                Some((_, stored_mtime, _)) => {
                    (stored_mtime - indexer::file_mtime(abs)).abs() >= 0.01
                }
                // Unknown file: created since the last index.
                None => true,
            })
            .count();
        // Deletions are counted for the report only. The refresh does not apply
        // them (see `indexer::index_repo_with_options`), so on their own they are
        // not a reason to run one.
        let deleted = deleted
            .iter()
            .filter(|rel| existing.contains_key(rel.as_str()))
            .count();
        (files, deleted)
    };

    let (files, deleted) = dirty;
    if files == 0 {
        return Refresh::Clean;
    }
    let cap = max_files();
    if files > cap {
        return Refresh::TooBig { files, cap };
    }

    let started = Instant::now();
    let mut silent = |_: &str| {};
    match indexer::index_repo_with_options(
        repo_root,
        IndexOptions {
            force: false,
            no_embed: true,
        },
        &mut silent,
    ) {
        Ok(_) => Refresh::Refreshed(RefreshReport {
            files,
            deleted,
            elapsed_ms: started.elapsed().as_millis(),
        }),
        Err(e) => Refresh::Failed(e.to_string()),
    }
}

/// One stderr line when the refresh did something the user should know about.
/// Silent on the boring outcomes so scripted/JSON callers keep a clean stream.
pub fn announce(outcome: &Refresh) {
    match outcome {
        Refresh::Refreshed(r) => {
            let mut what = format!("{} changed file(s)", r.files);
            if r.deleted > 0 {
                what.push_str(&format!(
                    ", {} deleted (still indexed until `tokenix index`)",
                    r.deleted
                ));
            }
            eprintln!(
                "[tokenix] index refreshed for {what} in {}ms (text + graph; run `tokenix index` to embed them)",
                r.elapsed_ms
            );
        }
        Refresh::TooBig { files, cap } => {
            eprintln!(
                "[tokenix] {files} changed file(s) exceed the inline refresh cap of {cap} — answering from the stored index. Run `tokenix index` for fresh results."
            );
        }
        Refresh::Failed(e) => {
            eprintln!("[tokenix] index refresh skipped: {e}");
        }
        Refresh::Disabled | Refresh::NoIndex | Refresh::NotGit | Refresh::Clean => {}
    }
}

/// Refresh and report in one call — what every retrieval command wants.
pub fn refresh_and_announce(repo_root: &Path) {
    let outcome = refresh_before_query(repo_root);
    announce(&outcome);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_by_env_returns_disabled() {
        let key = "TOKENIX_AUTO_REFRESH";
        let prev = std::env::var(key).ok();
        std::env::set_var(key, "0");
        let outcome = refresh_before_query(Path::new("."));
        assert!(matches!(outcome, Refresh::Disabled));
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    #[test]
    fn missing_index_is_a_no_op() {
        let dir = std::env::temp_dir().join(format!("tokenix_fresh_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        // No DB for this path, so the refresh must bail before touching git.
        assert!(matches!(
            refresh_before_query(&dir),
            Refresh::NoIndex | Refresh::NotGit
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cap_is_configurable_and_defaults() {
        let key = "TOKENIX_AUTO_REFRESH_MAX";
        let prev = std::env::var(key).ok();
        std::env::remove_var(key);
        assert_eq!(max_files(), DEFAULT_MAX_FILES);
        std::env::set_var(key, "3");
        assert_eq!(max_files(), 3);
        // Garbage falls back to the default instead of disabling the feature.
        std::env::set_var(key, "not-a-number");
        assert_eq!(max_files(), DEFAULT_MAX_FILES);
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    #[test]
    fn announce_is_silent_for_boring_outcomes() {
        // No assertion on stderr here; the point is that these arms must not
        // panic and must stay in the "silent" match arm.
        announce(&Refresh::Clean);
        announce(&Refresh::Disabled);
        announce(&Refresh::NoIndex);
        announce(&Refresh::NotGit);
    }
}
