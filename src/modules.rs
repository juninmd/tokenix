//! Functional modules discovered from the symbol graph.
//!
//! `tokenix graph` already answers "which symbols matter" (god nodes,
//! bottlenecks, blast radius). It did not answer "what are the parts of this
//! system", which is the question an agent opening an unfamiliar repo actually
//! has. Directory layout is a poor proxy: a `utils/` folder is not a subsystem
//! and a subsystem is regularly spread over several folders.
//!
//! This module runs Louvain community detection over the call/reference graph,
//! so a module is a set of symbols that talk to each other far more than to the
//! rest of the repo — derived from the code, not from the folder names. It is
//! deterministic (no randomness, every tie broken on a stable key) so two runs
//! over the same index produce the same report, and it needs no LLM.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::store;

/// Communities below this size are graph noise (a helper and its two callers),
/// not a subsystem worth naming in a report.
const MIN_MODULE_SYMBOLS: usize = 4;
/// Louvain passes. Two levels is enough to grow file-sized communities into
/// subsystem-sized ones; more only merges everything into one blob.
const MAX_LEVELS: usize = 2;
/// Local-moving sweeps per level.
const MAX_SWEEPS: usize = 12;

#[derive(Debug, Clone, serde::Serialize)]
pub struct Module {
    /// Dominant location of the members — a directory, or a file when one file
    /// holds most of the module.
    pub label: String,
    pub symbols: usize,
    /// Edges with both endpoints inside the module.
    pub internal_edges: usize,
    /// Edges crossing the module boundary in either direction.
    pub external_edges: usize,
    /// `internal / (internal + external)` — 1.0 is a self-contained module.
    pub cohesion: f32,
    /// Highest-degree members, the symbols to read first.
    pub key_symbols: Vec<String>,
    /// Files the module spans, most members first.
    pub files: Vec<String>,
}

/// Strip the `:line` suffix `store::GraphEdgeRow` carries on each endpoint.
fn file_of(loc: &str) -> &str {
    loc.rsplit_once(':').map(|(p, _)| p).unwrap_or(loc)
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map(|(d, _)| d).unwrap_or(path)
}

/// Undirected weighted view of the directed symbol graph: two symbols that call
/// each other in both directions are simply strongly connected.
struct Weighted {
    adj: Vec<Vec<(usize, f64)>>,
    self_loops: Vec<f64>,
    /// Sum of incident weights per node (self-loops counted twice).
    degree: Vec<f64>,
    /// 2m — the total weight of the graph, doubled.
    total: f64,
}

impl Weighted {
    fn from_pairs(n: usize, pairs: &BTreeMap<(usize, usize), f64>) -> Self {
        let mut adj = vec![Vec::new(); n];
        let mut self_loops = vec![0.0; n];
        let mut degree = vec![0.0; n];
        let mut total = 0.0;
        for (&(a, b), &w) in pairs {
            if a == b {
                self_loops[a] += w;
                degree[a] += 2.0 * w;
                total += 2.0 * w;
            } else {
                adj[a].push((b, w));
                adj[b].push((a, w));
                degree[a] += w;
                degree[b] += w;
                total += 2.0 * w;
            }
        }
        Weighted {
            adj,
            self_loops,
            degree,
            total,
        }
    }
}

/// One Louvain level: move nodes between communities while modularity improves.
/// Returns the community id of every node (not renumbered).
fn local_moving(g: &Weighted) -> Vec<usize> {
    let n = g.adj.len();
    let mut comm: Vec<usize> = (0..n).collect();
    let mut tot: Vec<f64> = g.degree.clone();
    if g.total <= 0.0 {
        return comm;
    }

    for _ in 0..MAX_SWEEPS {
        let mut moved = false;
        for i in 0..n {
            let ci = comm[i];
            // Weight from `i` to each neighbouring community. BTreeMap keeps the
            // scan order stable, so equal gains always resolve the same way.
            let mut links: BTreeMap<usize, f64> = BTreeMap::new();
            for &(j, w) in &g.adj[i] {
                *links.entry(comm[j]).or_insert(0.0) += w;
            }

            tot[ci] -= g.degree[i];
            let ki = g.degree[i];
            let mut best = ci;
            let mut best_gain = links.get(&ci).copied().unwrap_or(0.0) - tot[ci] * ki / g.total;
            for (&c, &w) in &links {
                if c == ci {
                    continue;
                }
                let gain = w - tot[c] * ki / g.total;
                if gain > best_gain + 1e-12 {
                    best_gain = gain;
                    best = c;
                }
            }
            tot[best] += ki;
            if best != ci {
                comm[i] = best;
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
    comm
}

/// Collapse each community into a single node, preserving edge weights, so the
/// next level can merge communities into larger ones.
fn aggregate(g: &Weighted, comm: &[usize]) -> (Weighted, Vec<usize>) {
    let mut renumber: BTreeMap<usize, usize> = BTreeMap::new();
    for &c in comm {
        let next = renumber.len();
        renumber.entry(c).or_insert(next);
    }
    let mapped: Vec<usize> = comm.iter().map(|c| renumber[c]).collect();
    let n = renumber.len();

    let mut pairs: BTreeMap<(usize, usize), f64> = BTreeMap::new();
    for (i, neighbours) in g.adj.iter().enumerate() {
        for &(j, w) in neighbours {
            if i > j {
                continue; // each undirected edge once
            }
            let (a, b) = (mapped[i], mapped[j]);
            let key = if a <= b { (a, b) } else { (b, a) };
            *pairs.entry(key).or_insert(0.0) += w;
        }
    }
    for (i, &loop_w) in g.self_loops.iter().enumerate() {
        if loop_w > 0.0 {
            let a = mapped[i];
            *pairs.entry((a, a)).or_insert(0.0) += loop_w;
        }
    }

    (Weighted::from_pairs(n, &pairs), mapped)
}

/// Name a module after where its members live.
///
/// Order matters: one dominant file is the clearest name, a shared *nested*
/// directory is the next best, and otherwise the top files are joined. Plain
/// `src/` is refused as a label — in a flat repo every module shares it, and a
/// report where five entries are all called `src/` names nothing.
fn label_for(files: &[(&str, usize)], dirs: &[(&str, usize)], members: usize) -> String {
    if let Some((file, count)) = files.first() {
        if count * 10 >= members * 6 {
            return (*file).to_string();
        }
    }
    if let Some((dir, count)) = dirs.first() {
        let nested = dir.contains('/');
        if nested && count * 10 >= members * 8 {
            return format!("{dir}/");
        }
    }
    let joined: Vec<&str> = files
        .iter()
        .take(3)
        .map(|(f, _)| f.rsplit_once('/').map(|(_, n)| n).unwrap_or(f))
        .collect();
    if joined.is_empty() {
        return "(unknown)".to_string();
    }
    let mut label = joined.join(" + ");
    if files.len() > 3 {
        label.push_str(&format!(" +{}", files.len() - 3));
    }
    label
}

/// Group the symbol graph into functional modules, largest first.
pub fn detect_modules(edges: &[store::GraphEdgeRow], max_modules: usize) -> Vec<Module> {
    if edges.is_empty() || max_modules == 0 {
        return Vec::new();
    }

    // Stable node numbering: sort the raw chunk ids so the whole pipeline is
    // reproducible regardless of row order coming out of SQLite.
    let mut ids: Vec<i64> = edges
        .iter()
        .flat_map(|(cid, _, _, eid, _, _)| [*cid, *eid])
        .collect();
    ids.sort_unstable();
    ids.dedup();
    let index: HashMap<i64, usize> = ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    let mut names: Vec<(String, String)> = vec![(String::new(), String::new()); ids.len()];

    let mut pairs: BTreeMap<(usize, usize), f64> = BTreeMap::new();
    let mut degree: Vec<usize> = vec![0; ids.len()];
    for (cid, cname, cloc, eid, ename, eloc) in edges {
        let a = index[cid];
        let b = index[eid];
        if names[a].0.is_empty() {
            names[a] = (cname.clone(), file_of(cloc).to_string());
        }
        if names[b].0.is_empty() {
            names[b] = (ename.clone(), file_of(eloc).to_string());
        }
        degree[a] += 1;
        degree[b] += 1;
        let key = if a <= b { (a, b) } else { (b, a) };
        *pairs.entry(key).or_insert(0.0) += 1.0;
    }

    let mut graph = Weighted::from_pairs(ids.len(), &pairs);
    let mut membership: Vec<usize> = (0..ids.len()).collect();
    for level in 0..MAX_LEVELS {
        let comm = local_moving(&graph);
        let distinct: HashSet<usize> = comm.iter().copied().collect();
        let (next, mapped) = aggregate(&graph, &comm);
        membership = membership.iter().map(|c| mapped[*c]).collect();
        // Nothing merged at this level, so another one cannot help either.
        if distinct.len() == graph.adj.len() || level + 1 == MAX_LEVELS {
            break;
        }
        graph = next;
    }

    // Gather members per community.
    let mut members: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (node, comm) in membership.iter().enumerate() {
        members.entry(*comm).or_default().push(node);
    }

    // Edge accounting is done on the original directed edges so the numbers
    // match what `tokenix graph` reports elsewhere.
    let mut internal: HashMap<usize, usize> = HashMap::new();
    let mut external: HashMap<usize, usize> = HashMap::new();
    for (cid, _, _, eid, _, _) in edges {
        let a = membership[index[cid]];
        let b = membership[index[eid]];
        if a == b {
            *internal.entry(a).or_default() += 1;
        } else {
            *external.entry(a).or_default() += 1;
            *external.entry(b).or_default() += 1;
        }
    }

    let mut modules: Vec<Module> = members
        .into_iter()
        .filter(|(_, nodes)| nodes.len() >= MIN_MODULE_SYMBOLS)
        .map(|(comm, nodes)| {
            let mut file_counts: BTreeMap<&str, usize> = BTreeMap::new();
            let mut dir_counts: BTreeMap<&str, usize> = BTreeMap::new();
            for &node in &nodes {
                let path = names[node].1.as_str();
                if path.is_empty() {
                    continue;
                }
                *file_counts.entry(path).or_default() += 1;
                *dir_counts.entry(dir_of(path)).or_default() += 1;
            }

            let mut files: Vec<(&str, usize)> = file_counts.into_iter().collect();
            files.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
            let mut dirs: Vec<(&str, usize)> = dir_counts.into_iter().collect();
            dirs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));

            let label = label_for(&files, &dirs, nodes.len());

            let mut key: Vec<usize> = nodes.clone();
            key.sort_by(|a, b| {
                degree[*b]
                    .cmp(&degree[*a])
                    .then_with(|| names[*a].0.cmp(&names[*b].0))
            });
            let mut seen_names: HashSet<&str> = HashSet::new();
            let key_symbols: Vec<String> = key
                .iter()
                .map(|n| names[*n].0.as_str())
                // `<module>` and friends are synthetic file-level nodes, not
                // symbols a reader can open. The same name also appears once per
                // file that declares it, and a list reading "Agent, Agent, Agent"
                // tells the reader nothing.
                .filter(|n| n.len() > 2 && !n.starts_with('<') && seen_names.insert(n))
                .map(|n| n.to_string())
                .take(6)
                .collect();

            let internal_edges = internal.get(&comm).copied().unwrap_or(0);
            let external_edges = external.get(&comm).copied().unwrap_or(0);
            let denom = (internal_edges + external_edges).max(1) as f32;
            Module {
                label,
                symbols: nodes.len(),
                internal_edges,
                external_edges,
                cohesion: internal_edges as f32 / denom,
                key_symbols,
                files: files
                    .iter()
                    .take(5)
                    .map(|(f, _)| (*f).to_string())
                    .collect(),
            }
        })
        .collect();

    modules.sort_by(|a, b| {
        b.symbols
            .cmp(&a.symbols)
            .then_with(|| a.label.cmp(&b.label))
    });
    modules.truncate(max_modules);
    modules
}

pub fn format_modules(modules: &[Module]) -> String {
    if modules.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n## Modules (symbols that mostly talk to each other)\n");
    for m in modules {
        out.push_str(&format!(
            "- {} — {} symbols, cohesion {:.0}% ({} internal / {} crossing)\n",
            m.label,
            m.symbols,
            m.cohesion * 100.0,
            m.internal_edges,
            m.external_edges
        ));
        if !m.key_symbols.is_empty() {
            out.push_str(&format!("    key: {}\n", m.key_symbols.join(", ")));
        }
        if m.files.len() > 1 {
            out.push_str(&format!("    files: {}\n", m.files.join(", ")));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(a: i64, an: &str, ap: &str, b: i64, bn: &str, bp: &str) -> store::GraphEdgeRow {
        (
            a,
            an.to_string(),
            format!("{ap}:1"),
            b,
            bn.to_string(),
            format!("{bp}:1"),
        )
    }

    /// Two cliques joined by a single edge must come back as two modules.
    fn two_cluster_graph() -> Vec<store::GraphEdgeRow> {
        let mut edges = Vec::new();
        let auth = [
            (1, "login"),
            (2, "verify_token"),
            (3, "hash_password"),
            (4, "session_new"),
            (5, "logout"),
        ];
        let billing = [
            (11, "charge"),
            (12, "refund"),
            (13, "invoice_pdf"),
            (14, "tax_for"),
            (15, "ledger_write"),
        ];
        for (i, (ida, na)) in auth.iter().enumerate() {
            for (idb, nb) in auth.iter().skip(i + 1) {
                edges.push(edge(*ida, na, "src/auth.rs", *idb, nb, "src/auth.rs"));
            }
        }
        for (i, (ida, na)) in billing.iter().enumerate() {
            for (idb, nb) in billing.iter().skip(i + 1) {
                edges.push(edge(*ida, na, "src/billing.rs", *idb, nb, "src/billing.rs"));
            }
        }
        // The single bridge between the two subsystems.
        edges.push(edge(
            1,
            "login",
            "src/auth.rs",
            11,
            "charge",
            "src/billing.rs",
        ));
        edges
    }

    #[test]
    fn separates_two_communities() {
        let modules = detect_modules(&two_cluster_graph(), 10);
        assert_eq!(
            modules.len(),
            2,
            "expected one module per clique: {modules:?}"
        );
        let labels: Vec<&str> = modules.iter().map(|m| m.label.as_str()).collect();
        assert!(labels.contains(&"src/auth.rs"), "labels: {labels:?}");
        assert!(labels.contains(&"src/billing.rs"), "labels: {labels:?}");
        for m in &modules {
            assert!(
                m.cohesion > 0.8,
                "a clique joined by one bridge is cohesive: {m:?}"
            );
            assert_eq!(m.symbols, 5);
        }
    }

    #[test]
    fn is_deterministic_across_row_order() {
        let mut edges = two_cluster_graph();
        let first = detect_modules(&edges, 10);
        edges.reverse();
        let second = detect_modules(&edges, 10);
        let names = |ms: &[Module]| -> Vec<(String, usize)> {
            ms.iter().map(|m| (m.label.clone(), m.symbols)).collect()
        };
        assert_eq!(names(&first), names(&second));
    }

    #[test]
    fn flat_repos_get_a_file_list_label_not_a_bare_src() {
        // Members spread evenly over top-level files of one flat folder: `src/`
        // would be true of every module in the repo, so it is not a name.
        let files = [
            ("src/a.rs", 3),
            ("src/b.rs", 2),
            ("src/c.rs", 2),
            ("src/d.rs", 1),
        ];
        let dirs = [("src", 8)];
        assert_eq!(label_for(&files, &dirs, 8), "a.rs + b.rs + c.rs +1");

        // One file carrying 60% of the module still wins.
        let dominant = [("src/store.rs", 6), ("src/a.rs", 4)];
        assert_eq!(label_for(&dominant, &dirs, 10), "src/store.rs");
    }

    #[test]
    fn empty_graph_yields_no_modules() {
        assert!(detect_modules(&[], 10).is_empty());
        assert_eq!(format_modules(&[]), "");
    }

    #[test]
    fn tiny_clusters_are_not_reported() {
        // A helper called by two symbols is not a subsystem.
        let edges = vec![
            edge(1, "helper", "src/u.rs", 2, "caller_a", "src/a.rs"),
            edge(1, "helper", "src/u.rs", 3, "caller_b", "src/b.rs"),
        ];
        assert!(detect_modules(&edges, 10).is_empty());
    }

    #[test]
    fn label_falls_back_to_the_shared_directory() {
        // Members spread over several files in one folder: the folder names it.
        let mut edges = Vec::new();
        let members = [
            (1, "a_one", "src/http/get.rs"),
            (2, "a_two", "src/http/post.rs"),
            (3, "a_three", "src/http/head.rs"),
            (4, "a_four", "src/http/route.rs"),
            (5, "a_five", "src/http/serve.rs"),
        ];
        for (i, (ida, na, pa)) in members.iter().enumerate() {
            for (idb, nb, pb) in members.iter().skip(i + 1) {
                edges.push(edge(*ida, na, pa, *idb, nb, pb));
            }
        }
        let modules = detect_modules(&edges, 10);
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].label, "src/http/");
    }
}
