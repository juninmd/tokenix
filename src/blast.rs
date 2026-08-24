//! Diff-scoped blast radius: what a change set can break.
//!
//! `tokenix impact <symbol>` answers the question once you already know which
//! symbol to ask about. Reviewing a branch is the other way round: the diff is
//! the input and the interesting output is everything downstream of it. This
//! maps changed line ranges onto symbols and walks the reverse call graph from
//! there, so a review starts from "these 3 callers of the function you touched
//! are untested" instead of from a list of files.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::store::{self, GraphNode};

#[derive(Debug, Clone, serde::Serialize)]
pub struct ChangedSymbol {
    pub name: String,
    pub path: String,
    pub kind: String,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ImpactedSymbol {
    pub name: String,
    pub path: String,
    /// Call-graph hops from the nearest changed symbol.
    pub distance: usize,
    /// How many symbols call this one — a proxy for how visible a break is.
    pub in_degree: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct BlastReport {
    /// What the diff was taken against (`HEAD`, `origin/main`, ...).
    pub base: String,
    pub changed_files: Vec<String>,
    /// Changed files that carry no indexed symbol (new files, data, configs).
    pub files_without_symbols: Vec<String>,
    pub changed_symbols: Vec<ChangedSymbol>,
    pub impacted: Vec<ImpactedSymbol>,
    /// Files containing at least one impacted symbol, most hits first.
    pub impacted_files: Vec<(String, usize)>,
    pub depth: usize,
    pub truncated: bool,
}

/// Line ranges touched per file, in terms of the *new* file's numbering.
type FileRanges = BTreeMap<String, Vec<(usize, usize)>>;

fn git_diff(repo_root: &Path, base: &str) -> Result<String> {
    let out = Command::new("git")
        .args([
            "diff",
            "--unified=0",
            "--no-color",
            "--no-ext-diff",
            "--find-renames",
            base,
        ])
        .current_dir(repo_root)
        .output()
        .context("failed to run git diff — is git installed and is this a repository?")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("git diff {base} failed: {}", err.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Parse `git diff --unified=0` into per-file changed line ranges.
///
/// Only the new-side ranges matter: they are what the index has symbols for. A
/// pure deletion (`+c,0`) still marks line `c` so the symbol that lost code is
/// reported.
pub fn parse_diff_ranges(diff: &str) -> FileRanges {
    let mut ranges: FileRanges = BTreeMap::new();
    let mut current: Option<String> = None;
    // A `+++ ` line is only a header when the previous line was the matching
    // `--- ` one. Added content can legitimately start with `+++`, and reading
    // that as a header would file the next hunks under a nonexistent path.
    let mut prev_was_old_header = false;
    for line in diff.lines() {
        if line.starts_with("--- ") {
            prev_was_old_header = true;
            continue;
        }
        if let Some(rest) = line.strip_prefix("+++ ") {
            if !prev_was_old_header {
                continue;
            }
            prev_was_old_header = false;
            let path = rest.trim();
            current = if path == "/dev/null" {
                None
            } else {
                Some(path.trim_start_matches("b/").replace('\\', "/"))
            };
            continue;
        }
        prev_was_old_header = false;
        if !line.starts_with("@@") {
            continue;
        }
        let Some(file) = current.clone() else {
            continue;
        };
        // @@ -old,count +new,count @@ optional context
        let Some(plus) = line.split('+').nth(1) else {
            continue;
        };
        let spec = plus.split_whitespace().next().unwrap_or("");
        let (start, count) = match spec.split_once(',') {
            Some((s, c)) => (s.parse::<usize>().ok(), c.parse::<usize>().ok()),
            None => (spec.parse::<usize>().ok(), Some(1)),
        };
        let (Some(start), Some(count)) = (start, count) else {
            continue;
        };
        let end = if count == 0 { start } else { start + count - 1 };
        ranges.entry(file).or_default().push((start.max(1), end));
    }
    ranges
}

/// `<module>` and friends are synthetic file-level graph nodes. They are useful
/// for edge resolution but meaningless in a review list: nobody can open
/// "`<module>`" and check it.
fn is_synthetic(name: &str) -> bool {
    name.starts_with('<')
}

fn overlaps(node: &GraphNode, ranges: &[(usize, usize)]) -> bool {
    ranges
        .iter()
        .any(|(s, e)| node.start_line <= *e && node.end_line >= *s)
}

/// What one reverse walk yields: the reached nodes with their hop distance, a
/// `chunk_id -> (symbol name, path:line)` map, and each node's caller count.
type ReverseWalk = (
    Vec<(i64, usize)>,
    HashMap<i64, (String, String)>,
    HashMap<i64, usize>,
);

/// Walk the reverse call graph from the changed symbols.
fn reverse_bfs(edges: &[store::GraphEdgeRow], seeds: &HashSet<i64>, depth: usize) -> ReverseWalk {
    let mut callers: HashMap<i64, Vec<i64>> = HashMap::new();
    let mut label: HashMap<i64, (String, String)> = HashMap::new();
    let mut in_degree: HashMap<i64, usize> = HashMap::new();
    for (cid, cname, cloc, eid, ename, eloc) in edges {
        label
            .entry(*cid)
            .or_insert_with(|| (cname.clone(), cloc.clone()));
        label
            .entry(*eid)
            .or_insert_with(|| (ename.clone(), eloc.clone()));
        callers.entry(*eid).or_default().push(*cid);
        *in_degree.entry(*eid).or_default() += 1;
    }

    let mut seen: HashSet<i64> = seeds.clone();
    let mut out: Vec<(i64, usize)> = Vec::new();
    let mut frontier: Vec<i64> = seeds.iter().copied().collect();
    frontier.sort_unstable();
    for hop in 1..=depth {
        let mut next: Vec<i64> = Vec::new();
        for node in frontier {
            let Some(list) = callers.get(&node) else {
                continue;
            };
            let mut list = list.clone();
            list.sort_unstable();
            for caller in list {
                if seen.insert(caller) {
                    out.push((caller, hop));
                    next.push(caller);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    (out, label, in_degree)
}

pub fn analyze(
    conn: &Connection,
    repo_root: &Path,
    base: &str,
    depth: usize,
    limit: usize,
) -> Result<BlastReport> {
    let diff = git_diff(repo_root, base)?;
    let ranges = parse_diff_ranges(&diff);

    let mut changed_symbols = Vec::new();
    let mut files_without_symbols = Vec::new();
    let mut seeds: HashSet<i64> = HashSet::new();
    for (file, spans) in &ranges {
        let nodes = store::graph_nodes_for_path(conn, file)?;
        let mut hit = false;
        for node in nodes {
            if overlaps(&node, spans) {
                hit = true;
                // Seed the walk from every touched node, including file-level
                // ones — they carry real edges — but keep them out of the report.
                seeds.insert(node.chunk_id);
                if is_synthetic(&node.name) {
                    continue;
                }
                changed_symbols.push(ChangedSymbol {
                    name: node.name,
                    path: node.path,
                    kind: node.kind,
                    start_line: node.start_line,
                    end_line: node.end_line,
                });
            }
        }
        if !hit {
            files_without_symbols.push(file.clone());
        }
    }
    changed_symbols.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then_with(|| a.start_line.cmp(&b.start_line))
    });

    let edges = store::load_all_graph_edges(conn)?;
    let (reached, label, in_degree) = reverse_bfs(&edges, &seeds, depth.max(1));

    let changed_ids = seeds;
    let mut impacted: Vec<ImpactedSymbol> = reached
        .into_iter()
        .filter(|(id, _)| !changed_ids.contains(id))
        .filter(|(id, _)| !label.get(id).is_some_and(|(name, _)| is_synthetic(name)))
        .map(|(id, distance)| {
            let (name, loc) = label
                .get(&id)
                .cloned()
                .unwrap_or_else(|| (format!("chunk#{id}"), String::new()));
            ImpactedSymbol {
                name,
                path: loc,
                distance,
                in_degree: in_degree.get(&id).copied().unwrap_or(0),
            }
        })
        .collect();
    // Nearest first, then the most-called symbols: that is the reading order for
    // a review — closest to the change and most exposed if it breaks.
    impacted.sort_by(|a, b| {
        a.distance
            .cmp(&b.distance)
            .then_with(|| b.in_degree.cmp(&a.in_degree))
            .then_with(|| a.name.cmp(&b.name))
    });

    let mut per_file: BTreeMap<String, usize> = BTreeMap::new();
    for s in &impacted {
        let file = s.path.rsplit_once(':').map(|(p, _)| p).unwrap_or(&s.path);
        *per_file.entry(file.to_string()).or_default() += 1;
    }
    let mut impacted_files: Vec<(String, usize)> = per_file.into_iter().collect();
    impacted_files.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let truncated = impacted.len() > limit;
    impacted.truncate(limit);

    Ok(BlastReport {
        base: base.to_string(),
        changed_files: ranges.keys().cloned().collect(),
        files_without_symbols,
        changed_symbols,
        impacted,
        impacted_files,
        depth: depth.max(1),
        truncated,
    })
}

pub fn format_report(report: &BlastReport) -> String {
    if report.changed_files.is_empty() {
        return format!("No changes against {}.", report.base);
    }
    let mut out = format!(
        "# Blast radius vs {} — {} changed file(s), {} changed symbol(s)\n",
        report.base,
        report.changed_files.len(),
        report.changed_symbols.len()
    );

    out.push_str("\n## Changed symbols\n");
    if report.changed_symbols.is_empty() {
        out.push_str("- (none indexed — the diff touches files with no symbols)\n");
    }
    for s in &report.changed_symbols {
        out.push_str(&format!(
            "- {} ({})  {}:{}\n",
            s.name, s.kind, s.path, s.start_line
        ));
    }

    out.push_str(&format!(
        "\n## Impacted (up to {} hop(s) of callers)\n",
        report.depth
    ));
    if report.impacted.is_empty() {
        out.push_str("- nothing depends on the changed symbols\n");
    }
    for s in &report.impacted {
        out.push_str(&format!(
            "- [{}] {} (↑{})  {}\n",
            s.distance, s.name, s.in_degree, s.path
        ));
    }
    if report.truncated {
        out.push_str("- … more impacted symbols hidden (raise --limit)\n");
    }

    if !report.impacted_files.is_empty() {
        out.push_str("\n## Files to review\n");
        for (file, hits) in report.impacted_files.iter().take(15) {
            out.push_str(&format!("- {file} ({hits} impacted symbol(s))\n"));
        }
    }

    if !report.files_without_symbols.is_empty() {
        out.push_str("\n## Changed files with no indexed symbol\n");
        for f in report.files_without_symbols.iter().take(15) {
            out.push_str(&format!("- {f}\n"));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "diff --git a/src/store.rs b/src/store.rs\n\
index 111..222 100644\n\
--- a/src/store.rs\n\
+++ b/src/store.rs\n\
@@ -10,3 +10,5 @@ fn thing() {\n\
+added\n\
@@ -80,2 +82,0 @@ fn other() {\n\
-gone\n\
diff --git a/README.md b/README.md\n\
--- a/README.md\n\
+++ b/README.md\n\
@@ -1 +1 @@\n\
-old\n\
+new\n";

    #[test]
    fn parses_new_side_ranges() {
        let ranges = parse_diff_ranges(DIFF);
        assert_eq!(ranges["src/store.rs"], vec![(10, 14), (82, 82)]);
        assert_eq!(ranges["README.md"], vec![(1, 1)]);
    }

    #[test]
    fn added_content_that_looks_like_a_header_is_not_one() {
        // The body of this hunk adds a literal `+++ b/evil.rs` line.
        let diff = "--- a/src/real.rs\n\
+++ b/src/real.rs\n\
@@ -1,0 +2,1 @@\n\
+++ b/evil.rs\n\
@@ -9,0 +9,1 @@\n\
+ok\n";
        let ranges = parse_diff_ranges(diff);
        assert_eq!(ranges.len(), 1, "{ranges:?}");
        assert_eq!(ranges["src/real.rs"], vec![(2, 2), (9, 9)]);
    }

    #[test]
    fn deleted_files_are_skipped() {
        let diff = "--- a/gone.rs\n+++ /dev/null\n@@ -1,5 +0,0 @@\n-x\n";
        assert!(parse_diff_ranges(diff).is_empty());
    }

    fn node(id: i64, name: &str, start: usize, end: usize) -> GraphNode {
        GraphNode {
            chunk_id: id,
            path: "src/store.rs".to_string(),
            name: name.to_string(),
            kind: "function".to_string(),
            start_line: start,
            end_line: end,
        }
    }

    #[test]
    fn overlap_is_inclusive_on_both_ends() {
        let n = node(1, "f", 10, 20);
        assert!(overlaps(&n, &[(20, 25)]));
        assert!(overlaps(&n, &[(5, 10)]));
        assert!(overlaps(&n, &[(12, 13)]));
        assert!(!overlaps(&n, &[(21, 30)]));
        assert!(!overlaps(&n, &[(1, 9)]));
    }

    #[test]
    fn bfs_reports_hop_distance_and_stops_at_depth() {
        // c -> b -> a  (a is the changed symbol)
        let edges = vec![
            (
                2,
                "b".to_string(),
                "src/b.rs:1".to_string(),
                1,
                "a".to_string(),
                "src/a.rs:1".to_string(),
            ),
            (
                3,
                "c".to_string(),
                "src/c.rs:1".to_string(),
                2,
                "b".to_string(),
                "src/b.rs:1".to_string(),
            ),
        ];
        let seeds: HashSet<i64> = [1].into_iter().collect();

        let (one_hop, _, _) = reverse_bfs(&edges, &seeds, 1);
        assert_eq!(one_hop, vec![(2, 1)]);

        let (two_hops, _, in_degree) = reverse_bfs(&edges, &seeds, 2);
        assert_eq!(two_hops, vec![(2, 1), (3, 2)]);
        assert_eq!(in_degree.get(&1), Some(&1));
    }

    #[test]
    fn synthetic_file_nodes_are_not_reportable_symbols() {
        assert!(is_synthetic("<module>"));
        assert!(!is_synthetic("handle_checkout"));
    }

    #[test]
    fn cycles_do_not_loop_forever() {
        // a -> b -> a
        let edges = vec![
            (
                1,
                "a".to_string(),
                "src/a.rs:1".to_string(),
                2,
                "b".to_string(),
                "src/b.rs:1".to_string(),
            ),
            (
                2,
                "b".to_string(),
                "src/b.rs:1".to_string(),
                1,
                "a".to_string(),
                "src/a.rs:1".to_string(),
            ),
        ];
        let seeds: HashSet<i64> = [1].into_iter().collect();
        let (reached, _, _) = reverse_bfs(&edges, &seeds, 10);
        assert_eq!(reached, vec![(2, 1)]);
    }
}
