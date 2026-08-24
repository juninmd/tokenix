use anyhow::Result;
use regex::Regex;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

use crate::store::{self, GraphNode, GraphRelation};

const KEYWORDS: &[&str] = &[
    "if", "for", "while", "loop", "match", "return", "fn", "function", "class", "struct", "enum",
    "trait", "impl", "let", "const", "static", "async", "await", "where", "switch", "catch",
    "println", "format", "vec", "Some", "None", "Ok", "Err", "new", "default", "clone", "unwrap",
    "expect", "insert", "get", "push", "len", "is_empty",
];

#[derive(Debug, Clone)]
struct ChunkSymbol {
    chunk_id: i64,
    file_id: i64,
    path: String,
    name: String,
    kind: String,
    start_line: usize,
    end_line: usize,
    content: String,
}

#[derive(Debug, Clone)]
struct SymbolTarget {
    chunk_id: i64,
    path: String,
}

pub fn rebuild_symbol_graph(conn: &Connection) -> Result<()> {
    let chunks = load_symbol_chunks(conn)?;
    store::clear_symbol_graph(conn)?;

    for chunk in &chunks {
        store::insert_graph_node(
            conn,
            chunk.chunk_id,
            chunk.file_id,
            &chunk.path,
            &chunk.name,
            &chunk.kind,
            chunk.start_line,
            chunk.end_line,
        )?;
    }

    let mut by_name: HashMap<String, Vec<SymbolTarget>> = HashMap::new();
    for chunk in &chunks {
        by_name
            .entry(normalize_name(&chunk.name))
            .or_default()
            .push(SymbolTarget {
                chunk_id: chunk.chunk_id,
                path: chunk.path.clone(),
            });
    }

    let mut inserted = HashSet::new();
    for chunk in &chunks {
        insert_reference_edges(conn, chunk, &by_name, &mut inserted, None)?;
    }

    let node_ids: Vec<i64> = chunks.iter().map(|c| c.chunk_id).collect();
    let edges: Vec<(i64, i64)> = inserted.into_iter().collect();
    let ranks = pagerank(&node_ids, &edges);
    store::set_node_ranks(conn, &ranks)?;

    Ok(())
}

/// Extract a chunk's references and insert resolved edges. When
/// `only_targets_in` is set, edges are inserted only toward those paths —
/// used by the incremental path to restore inbound edges without duplicating
/// untouched ones.
fn insert_reference_edges(
    conn: &Connection,
    chunk: &ChunkSymbol,
    by_name: &HashMap<String, Vec<SymbolTarget>>,
    inserted: &mut HashSet<(i64, i64)>,
    only_targets_in: Option<&HashSet<&str>>,
) -> Result<()> {
    let aliases = extract_import_aliases(&chunk.content);
    for reference in extract_references(&chunk.content, &chunk.path) {
        let resolved_reference = aliases
            .get(&reference)
            .or_else(|| aliases.get(&short_reference_name(&reference)))
            .map(String::as_str)
            .unwrap_or(&reference);
        let targets = resolve_reference_targets(by_name, resolved_reference);
        for target in targets {
            if target.chunk_id == chunk.chunk_id {
                continue;
            }
            if let Some(filter) = only_targets_in {
                if !filter.contains(target.path.as_str()) {
                    continue;
                }
            }
            if inserted.insert((chunk.chunk_id, target.chunk_id)) {
                store::insert_graph_edge(
                    conn,
                    chunk.chunk_id,
                    target.chunk_id,
                    &reference,
                    "references",
                )?;
            }
        }
    }
    Ok(())
}

/// Incremental symbol-graph refresh after `changed_paths` were re-chunked.
/// `delete_chunks_for_file` already dropped those files' nodes and every edge
/// touching them, so only two repairs are needed: (1) nodes + outgoing edges
/// for the changed files, (2) inbound edges from unchanged callers — found via
/// FTS candidate search instead of a whole-repo reference re-extract.
/// `tokenix rebuild-graph` stays available as the full-rebuild escape hatch.
pub fn update_symbol_graph_incremental(conn: &Connection, changed_paths: &[String]) -> Result<()> {
    if changed_paths.is_empty() {
        return Ok(());
    }
    let chunks = load_symbol_chunks_for_paths(conn, changed_paths)?;
    for chunk in &chunks {
        store::insert_graph_node(
            conn,
            chunk.chunk_id,
            chunk.file_id,
            &chunk.path,
            &chunk.name,
            &chunk.kind,
            chunk.start_line,
            chunk.end_line,
        )?;
    }

    // Resolution map over the whole graph, table-backed (no chunk re-extract).
    let mut by_name: HashMap<String, Vec<SymbolTarget>> = HashMap::new();
    for (chunk_id, name, path) in store::all_graph_node_names(conn)? {
        by_name
            .entry(normalize_name(&name))
            .or_default()
            .push(SymbolTarget { chunk_id, path });
    }

    let changed_set: HashSet<&str> = changed_paths.iter().map(String::as_str).collect();
    let mut inserted = HashSet::new();

    // (1) Outgoing edges from the changed files' chunks.
    for chunk in &chunks {
        insert_reference_edges(conn, chunk, &by_name, &mut inserted, None)?;
    }

    // (2) Inbound edges: FTS narrows unchanged chunks that mention the changed
    // files' symbol names; only their edges INTO changed files are re-inserted.
    // The FTS cap bounds work per symbol name, but silently dropping the tail
    // means a hot symbol's callers past the cap keep stale edges forever (the
    // incremental path never revisits them). Report when that happens so the
    // gap is visible instead of looking like a complete repair.
    const FTS_CANDIDATE_CAP: usize = 400;
    let mut candidate_ids: HashSet<i64> = HashSet::new();
    let mut capped: Vec<&str> = Vec::new();
    for chunk in &chunks {
        let hits =
            store::search_fts(conn, &chunk.name, FTS_CANDIDATE_CAP, None).unwrap_or_default();
        if hits.len() >= FTS_CANDIDATE_CAP {
            capped.push(chunk.name.as_str());
        }
        for (id, _) in hits {
            candidate_ids.insert(id);
        }
    }
    if !capped.is_empty() {
        capped.sort_unstable();
        capped.dedup();
        eprintln!(
            "tokenix: inbound-edge repair hit the {FTS_CANDIDATE_CAP}-candidate cap for {}; \
             run `tokenix index --force` to rebuild the graph fully",
            capped.join(", ")
        );
    }
    let changed_ids: HashSet<i64> = chunks.iter().map(|c| c.chunk_id).collect();
    for cand_id in candidate_ids {
        if changed_ids.contains(&cand_id) {
            continue;
        }
        let Some(cand) = load_symbol_chunk_by_id(conn, cand_id)? else {
            continue;
        };
        if changed_set.contains(cand.path.as_str()) {
            continue;
        }
        insert_reference_edges(conn, &cand, &by_name, &mut inserted, Some(&changed_set))?;
    }

    // Ranks shift globally with the new edge set: recompute over the full graph.
    let node_ids: Vec<i64> = by_name
        .values()
        .flat_map(|targets| targets.iter().map(|t| t.chunk_id))
        .collect();
    let edges = store::all_graph_edge_pairs(conn)?;
    let ranks = pagerank(&node_ids, &edges);
    store::set_node_ranks(conn, &ranks)?;
    Ok(())
}

/// Classic PageRank over the reference graph. An edge `caller -> callee` lets a
/// caller's importance flow to the symbols it references, so widely-referenced
/// symbols accumulate higher rank. Used only to break ties when surfacing
/// symbols — not as a hard filter. Damping 0.85, fixed iteration count.
fn pagerank(node_ids: &[i64], edges: &[(i64, i64)]) -> Vec<(i64, f32)> {
    let n = node_ids.len();
    if n == 0 {
        return Vec::new();
    }
    const DAMPING: f32 = 0.85;
    const ITERATIONS: usize = 20;

    let index: HashMap<i64, usize> = node_ids
        .iter()
        .enumerate()
        .map(|(i, id)| (*id, i))
        .collect();
    let mut out_degree = vec![0u32; n];
    let mut in_edges: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (caller, callee) in edges {
        let (Some(&u), Some(&v)) = (index.get(caller), index.get(callee)) else {
            continue;
        };
        out_degree[u] += 1;
        in_edges[v].push(u);
    }

    let base = (1.0 - DAMPING) / n as f32;
    let mut rank = vec![1.0f32 / n as f32; n];
    for _ in 0..ITERATIONS {
        // Dangling nodes (no out-edges) redistribute their mass uniformly.
        let dangling: f32 = (0..n)
            .filter(|&i| out_degree[i] == 0)
            .map(|i| rank[i])
            .sum();
        let mut next = vec![base + DAMPING * dangling / n as f32; n];
        for v in 0..n {
            for &u in &in_edges[v] {
                next[v] += DAMPING * rank[u] / out_degree[u] as f32;
            }
        }
        rank = next;
    }

    node_ids.iter().copied().zip(rank).collect()
}

/// Detect circular dependencies using Tarjan's SCC algorithm.
/// Returns cycles (SCCs with size > 1) as lists of symbol names.
///
/// Runs on a dedicated 64 MB-stack thread: `strongconnect` recurses once per
/// edge along a chain, and a deep call graph overflowed the default 8 MB main
/// stack — an abort with no recoverable error, since there is no unwinding.
pub fn detect_cycles(edges: &[store::GraphEdgeRow]) -> Vec<Vec<String>> {
    const STACK_BYTES: usize = 64 * 1024 * 1024;
    let owned: Vec<store::GraphEdgeRow> = edges.to_vec();
    std::thread::Builder::new()
        .stack_size(STACK_BYTES)
        .spawn(move || detect_cycles_inner(&owned))
        .and_then(|h| h.join().map_err(|_| std::io::Error::other("panic")))
        .unwrap_or_default()
}

fn detect_cycles_inner(edges: &[store::GraphEdgeRow]) -> Vec<Vec<String>> {
    let mut adj: HashMap<i64, Vec<i64>> = HashMap::new();
    let mut node_names: HashMap<i64, String> = HashMap::new();
    let mut node_labels: HashMap<i64, String> = HashMap::new();

    for (caller_id, caller_name, caller_loc, callee_id, callee_name, callee_loc) in edges {
        adj.entry(*caller_id).or_default().push(*callee_id);
        node_names
            .entry(*caller_id)
            .or_insert_with(|| caller_name.clone());
        node_names
            .entry(*callee_id)
            .or_insert_with(|| callee_name.clone());
        node_labels
            .entry(*caller_id)
            .or_insert_with(|| format!("{caller_name} ({caller_loc})"));
        node_labels
            .entry(*callee_id)
            .or_insert_with(|| format!("{callee_name} ({callee_loc})"));
    }

    let mut index_counter: usize = 0;
    let mut stack: Vec<i64> = Vec::new();
    let mut on_stack: HashSet<i64> = HashSet::new();
    let mut indices: HashMap<i64, usize> = HashMap::new();
    let mut lowlinks: HashMap<i64, usize> = HashMap::new();
    let mut sccs: Vec<Vec<i64>> = Vec::new();

    let all_nodes: Vec<i64> = node_names.keys().cloned().collect();

    #[allow(clippy::too_many_arguments)]
    fn strongconnect(
        v: i64,
        index_counter: &mut usize,
        stack: &mut Vec<i64>,
        on_stack: &mut HashSet<i64>,
        indices: &mut HashMap<i64, usize>,
        lowlinks: &mut HashMap<i64, usize>,
        adj: &HashMap<i64, Vec<i64>>,
        sccs: &mut Vec<Vec<i64>>,
    ) {
        indices.insert(v, *index_counter);
        lowlinks.insert(v, *index_counter);
        *index_counter += 1;
        stack.push(v);
        on_stack.insert(v);

        if let Some(neighbors) = adj.get(&v) {
            for &w in neighbors {
                if !indices.contains_key(&w) {
                    strongconnect(
                        w,
                        index_counter,
                        stack,
                        on_stack,
                        indices,
                        lowlinks,
                        adj,
                        sccs,
                    );
                    let v_low = *lowlinks.get(&v).unwrap_or(&usize::MAX);
                    let w_low = *lowlinks.get(&w).unwrap_or(&usize::MAX);
                    lowlinks.insert(v, v_low.min(w_low));
                } else if on_stack.contains(&w) {
                    let v_low = *lowlinks.get(&v).unwrap_or(&usize::MAX);
                    let w_idx = *indices.get(&w).unwrap_or(&usize::MAX);
                    lowlinks.insert(v, v_low.min(w_idx));
                }
            }
        }

        if lowlinks.get(&v) == indices.get(&v) {
            let mut scc = Vec::new();
            loop {
                let w = stack.pop().unwrap();
                on_stack.remove(&w);
                scc.push(w);
                if w == v {
                    break;
                }
            }
            if scc.len() > 1 {
                sccs.push(scc);
            }
        }
    }

    for node in all_nodes {
        if !indices.contains_key(&node) {
            strongconnect(
                node,
                &mut index_counter,
                &mut stack,
                &mut on_stack,
                &mut indices,
                &mut lowlinks,
                &adj,
                &mut sccs,
            );
        }
    }

    sccs.into_iter()
        // Drop homonym artifacts: an SCC whose nodes all share one normalized
        // name is fabricated by name-based reference resolution linking same-named
        // symbols across files, not a real dependency cycle.
        .filter(|scc| {
            let mut names = scc
                .iter()
                .map(|id| node_names.get(id).map(|n| normalize_name(n)));
            let first = names.next().flatten();
            !names.all(|n| n == first)
        })
        .map(|scc| {
            scc.into_iter()
                .map(|id| {
                    node_labels
                        .get(&id)
                        .cloned()
                        .unwrap_or_else(|| format!("node_{id}"))
                })
                .collect()
        })
        .collect()
}

/// Format detected cycles for CLI output.
pub fn format_cycles(cycles: &[Vec<String>]) -> String {
    if cycles.is_empty() {
        return "No circular dependencies found.".to_string();
    }
    let mut out = format!("## Circular Dependencies ({} cycles)\n", cycles.len());
    for (i, cycle) in cycles.iter().enumerate() {
        out.push_str(&format!("{}. {}", i + 1, cycle.join(" → ")));
        if let Some(first) = cycle.first() {
            out.push_str(&format!(" → {}", first));
        }
        out.push('\n');
    }
    out
}

fn resolve_reference_targets<'a>(
    by_name: &'a HashMap<String, Vec<SymbolTarget>>,
    reference: &str,
) -> Vec<&'a SymbolTarget> {
    let key = normalize_name(reference);
    if let Some(targets) = by_name.get(&key) {
        return targets.iter().collect();
    }

    let short_key = short_reference_name(reference);
    if is_keyword(&short_key) {
        return Vec::new();
    }

    let Some(targets) = by_name.get(&short_key) else {
        return Vec::new();
    };

    let qualifiers = reference_qualifiers(reference);
    if qualifiers.is_empty() {
        // Linking a bare `render()` to all 20 same-named definitions in the repo
        // invents caller/callee relations and fabricates cycles between files
        // that only share a name. Below the threshold the guess is usually
        // right and useful; above it there is no evidence at all, so record
        // nothing rather than something false.
        if targets.len() > MAX_AMBIGUOUS_TARGETS {
            return Vec::new();
        }
        return targets.iter().collect();
    }

    // Qualifiers are positive evidence: when none of the candidates match them,
    // falling back to "all candidates" contradicts the very hint we were given.
    targets
        .iter()
        .filter(|target| {
            qualifiers
                .iter()
                .any(|qualifier| path_matches_qualifier(&target.path, qualifier))
        })
        .collect()
}

/// How many same-named definitions an unqualified reference may resolve to
/// before it is treated as ambiguous and dropped.
const MAX_AMBIGUOUS_TARGETS: usize = 8;

fn short_reference_name(reference: &str) -> String {
    reference
        .rsplit("::")
        .next()
        .or_else(|| reference.rsplit('.').next())
        .map(normalize_name)
        .unwrap_or_else(|| normalize_name(reference))
}

fn reference_qualifiers(reference: &str) -> Vec<String> {
    let separators: &[char] = if reference.contains("::") {
        &[':']
    } else {
        &['.']
    };
    let mut parts: Vec<String> = reference
        .split(separators)
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(normalize_name)
        .collect();
    if parts.len() <= 1 {
        return Vec::new();
    }
    parts.pop();
    parts
        .into_iter()
        .filter(|part| !matches!(part.as_str(), "crate" | "self" | "super"))
        .collect()
}

fn path_matches_qualifier(path: &str, qualifier: &str) -> bool {
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    normalized
        .split('/')
        .any(|segment| segment.strip_suffix(".rs").unwrap_or(segment) == qualifier)
}

pub fn format_nodes(nodes: &[GraphNode], title: &str) -> String {
    if nodes.is_empty() {
        return format!("No symbols found for: {title}");
    }
    let mut out = format!("## {title}\n");
    for node in nodes {
        out.push_str(&format!(
            "- {}:{}-{} [{}] {}\n",
            node.path, node.start_line, node.end_line, node.kind, node.name
        ));
    }
    out
}

pub fn format_relations(relations: &[GraphRelation], title: &str) -> String {
    if relations.is_empty() {
        return format!("No graph relationships found for: {title}");
    }
    let mut out = format!("## {title}\n");
    for rel in relations {
        out.push_str(&format!(
            "- {}:{} [{}] {} -> {}:{} [{}] {} via `{}` ({})\n",
            rel.from.path,
            rel.from.start_line,
            rel.from.kind,
            rel.from.name,
            rel.to.path,
            rel.to.start_line,
            rel.to.kind,
            rel.to.name,
            rel.reference,
            rel.edge_kind
        ));
    }
    out
}

/// A repo-wide graph hotspot: a symbol ranked by its connectivity and the
/// number of symbols transitively affected if it changes (blast radius).
#[derive(Debug, Clone, serde::Serialize)]
pub struct Hotspot {
    pub name: String,
    pub path: String,
    pub in_degree: usize,
    pub out_degree: usize,
    /// Transitive dependents — how many symbols are affected by a change here.
    pub blast: usize,
}

/// Rank the most-connected symbols across the whole symbol graph. Blast radius
/// (transitive dependents) is computed only for the strongest degree candidates
/// to keep this bounded on large graphs.
pub fn repo_hotspots(edges: &[store::GraphEdgeRow], top: usize) -> Vec<Hotspot> {
    let mut label: HashMap<i64, (String, String)> = HashMap::new();
    let mut indeg: HashMap<i64, usize> = HashMap::new();
    let mut outdeg: HashMap<i64, usize> = HashMap::new();
    // Reverse adjacency: callee -> callers, for blast-radius traversal.
    let mut dependents: HashMap<i64, Vec<i64>> = HashMap::new();

    for (cid, cname, cpath, eid, ename, epath) in edges {
        label
            .entry(*cid)
            .or_insert_with(|| (cname.clone(), cpath.clone()));
        label
            .entry(*eid)
            .or_insert_with(|| (ename.clone(), epath.clone()));
        *outdeg.entry(*cid).or_default() += 1;
        *indeg.entry(*eid).or_default() += 1;
        dependents.entry(*eid).or_default().push(*cid);
    }

    // Rank by degree first; only the strongest candidates get a blast walk.
    let mut by_degree: Vec<i64> = label.keys().copied().collect();
    by_degree.sort_by(|a, b| {
        let da = indeg.get(a).unwrap_or(&0) + outdeg.get(a).unwrap_or(&0);
        let db = indeg.get(b).unwrap_or(&0) + outdeg.get(b).unwrap_or(&0);
        // Break ties on (name, path). The candidates come out of a HashMap, so
        // without this the order among equal-degree symbols is whatever the
        // hash seed produced — and since `candidate_cap` then truncates the
        // list, two runs over an unchanged index reported different hotspots.
        db.cmp(&da).then_with(|| label[a].cmp(&label[b]))
    });

    let candidate_cap = (top * 3).max(top);
    by_degree
        .into_iter()
        .filter(|id| !is_trivial_symbol(&label[id].0))
        .take(candidate_cap)
        .map(|id| {
            let (name, path) = label[&id].clone();
            Hotspot {
                name,
                path,
                in_degree: *indeg.get(&id).unwrap_or(&0),
                out_degree: *outdeg.get(&id).unwrap_or(&0),
                blast: transitive_dependents(id, &dependents),
            }
        })
        .collect()
}

/// Drop graph-extraction noise (single-letter bindings, `_`, language keywords)
/// so the hotspot report surfaces meaningful symbols.
fn is_trivial_symbol(name: &str) -> bool {
    name.len() <= 2 || name.chars().all(|c| c == '_') || KEYWORDS.contains(&name)
}

/// BFS count of unique nodes reachable from `start` over the reverse-edge map.
fn transitive_dependents(start: i64, dependents: &HashMap<i64, Vec<i64>>) -> usize {
    let mut seen = HashSet::new();
    let mut frontier = vec![start];
    while let Some(node) = frontier.pop() {
        if let Some(callers) = dependents.get(&node) {
            for &c in callers {
                if seen.insert(c) {
                    frontier.push(c);
                }
            }
        }
    }
    seen.len()
}

/// Render a compact repo-wide graph report: god nodes, bottlenecks, and
/// blast-radius leaders. Inspired by knowledge-graph overviews but built from
/// tokenix's own symbol graph.
pub fn format_repo_report(edges: &[store::GraphEdgeRow], top: usize) -> String {
    if edges.is_empty() {
        return "No symbol-graph edges found. Run `tokenix index` first.".to_string();
    }
    let node_count = {
        let mut s = HashSet::new();
        for (cid, _, _, eid, _, _) in edges {
            s.insert(*cid);
            s.insert(*eid);
        }
        s.len()
    };
    let spots = repo_hotspots(edges, top);

    let mut out = format!(
        "# Repo graph — {} symbols, {} edges\n",
        node_count,
        edges.len()
    );

    out.push_str("\n## God nodes (most connected)\n");
    let mut god = spots.clone();
    god.sort_by_key(|h| std::cmp::Reverse(h.in_degree + h.out_degree));
    for h in god.iter().take(top) {
        out.push_str(&format!(
            "- {} (↑{} ↓{})  {}\n",
            h.name, h.in_degree, h.out_degree, h.path
        ));
    }

    out.push_str("\n## Bottlenecks (high fan-in, low fan-out)\n");
    let mut neck = spots.clone();
    neck.sort_by(|a, b| {
        let sa = a.in_degree as i64 - a.out_degree as i64;
        let sb = b.in_degree as i64 - b.out_degree as i64;
        sb.cmp(&sa)
    });
    for h in neck.iter().filter(|h| h.in_degree > h.out_degree).take(top) {
        out.push_str(&format!(
            "- {} (↑{} ↓{})  {}\n",
            h.name, h.in_degree, h.out_degree, h.path
        ));
    }

    out.push_str("\n## Blast-radius leaders (most transitive dependents)\n");
    let mut blast = spots;
    blast.sort_by_key(|h| std::cmp::Reverse(h.blast));
    for h in blast.iter().take(top) {
        out.push_str(&format!(
            "- {} → {} dependents  {}\n",
            h.name, h.blast, h.path
        ));
    }

    // Orientation: which parts this repo actually has, derived from the graph
    // rather than from the folder names.
    out.push_str(&crate::modules::format_modules(
        &crate::modules::detect_modules(edges, MODULES_IN_REPORT),
    ));

    out
}

/// How many communities the repo report names. The report is an orientation
/// device, not an inventory — past a handful the reader stops reading.
const MODULES_IN_REPORT: usize = 8;

/// Render the most-connected subgraph as Graphviz DOT. Only edges whose both
/// endpoints are among the top hotspots are emitted, keeping the diagram legible.
pub fn format_edges_dot(edges: &[store::GraphEdgeRow], top: usize) -> String {
    let spots = repo_hotspots(edges, top);
    let keep: HashSet<String> = spots.iter().take(top).map(|h| h.name.clone()).collect();
    let mut out = String::from("digraph tokenix {\n  rankdir=LR;\n  node [shape=box];\n");
    let mut seen = HashSet::new();
    for (_, cname, _, _, ename, _) in edges {
        if keep.contains(cname) && keep.contains(ename) {
            let line = format!("  {:?} -> {:?};\n", cname, ename);
            if seen.insert(line.clone()) {
                out.push_str(&line);
            }
        }
    }
    out.push_str("}\n");
    out
}

/// Format graph relations as a Mermaid flowchart diagram.
pub fn format_relations_mermaid(relations: &[GraphRelation], title: &str) -> String {
    if relations.is_empty() {
        return format!("No graph relationships found for: {title}");
    }
    let mut out = String::from("```mermaid\ngraph LR\n");
    let mut seen_nodes = std::collections::HashSet::new();

    for rel in relations {
        let from_id = format!("N{}", rel.from.chunk_id);
        let to_id = format!("N{}", rel.to.chunk_id);

        if seen_nodes.insert(from_id.clone()) {
            out.push_str(&format!(
                "    {}[\"{}:{} [{}] {}\"]\n",
                from_id, rel.from.path, rel.from.start_line, rel.from.kind, rel.from.name
            ));
        }
        if seen_nodes.insert(to_id.clone()) {
            out.push_str(&format!(
                "    {}[\"{}:{} [{}] {}\"]\n",
                to_id, rel.to.path, rel.to.start_line, rel.to.kind, rel.to.name
            ));
        }
        out.push_str(&format!(
            "    {} -->|\"{}\"| {}\n",
            from_id, rel.reference, to_id
        ));
    }

    out.push_str(&format!("    %% {title}\n"));
    out.push_str("```\n");
    out
}

fn load_symbol_chunks_for_paths(conn: &Connection, paths: &[String]) -> Result<Vec<ChunkSymbol>> {
    let placeholders = paths.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT id, file_id, path, symbol, kind, start_line, end_line, content
         FROM chunks
         WHERE symbol IS NOT NULL AND symbol != '' AND symbol != 'anonymous'
           AND path IN ({placeholders})"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        rusqlite::params_from_iter(paths.iter().map(String::as_str)),
        chunk_symbol_from_row,
    )?;
    Ok(rows.filter_map(|row| row.ok()).collect())
}

fn load_symbol_chunk_by_id(conn: &Connection, chunk_id: i64) -> Result<Option<ChunkSymbol>> {
    let mut stmt = conn.prepare(
        "SELECT id, file_id, path, symbol, kind, start_line, end_line, content
         FROM chunks
         WHERE id = ?1 AND symbol IS NOT NULL AND symbol != '' AND symbol != 'anonymous'",
    )?;
    let mut rows = stmt.query_map([chunk_id], chunk_symbol_from_row)?;
    Ok(rows.next().and_then(|r| r.ok()))
}

fn chunk_symbol_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChunkSymbol> {
    Ok(ChunkSymbol {
        chunk_id: row.get(0)?,
        file_id: row.get(1)?,
        path: row.get(2)?,
        name: row.get(3)?,
        kind: row.get(4)?,
        start_line: row.get::<_, i64>(5)? as usize,
        end_line: row.get::<_, i64>(6)? as usize,
        content: row.get(7)?,
    })
}

fn load_symbol_chunks(conn: &Connection) -> Result<Vec<ChunkSymbol>> {
    let mut stmt = conn.prepare(
        "SELECT id, file_id, path, symbol, kind, start_line, end_line, content
         FROM chunks
         WHERE symbol IS NOT NULL AND symbol != '' AND symbol != 'anonymous'",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(ChunkSymbol {
            chunk_id: row.get(0)?,
            file_id: row.get(1)?,
            path: row.get(2)?,
            name: row.get(3)?,
            kind: row.get(4)?,
            start_line: row.get::<_, i64>(5)? as usize,
            end_line: row.get::<_, i64>(6)? as usize,
            content: row.get(7)?,
        })
    })?;
    Ok(rows.filter_map(|row| row.ok()).collect())
}

fn is_comment_or_string(kind: &str) -> bool {
    let k = kind.to_ascii_lowercase();
    k.contains("comment") || k.contains("string") || k == "char" || k == "character"
}

fn is_definition_node(node: tree_sitter::Node, parent: tree_sitter::Node) -> bool {
    let parent_kind = parent.kind();
    for field in &["name", "pattern", "declarator", "target", "left"] {
        if let Some(child) = parent.child_by_field_name(field) {
            if child.id() == node.id() {
                return true;
            }
        }
    }
    matches!(
        parent_kind,
        "parameter" | "formal_parameter" | "parameter_declaration"
    )
}

fn is_reference_node(kind: &str) -> bool {
    matches!(
        kind,
        "identifier"
            | "type_identifier"
            | "field_identifier"
            | "property_identifier"
            | "shorthand_property_identifier"
            | "namespace_identifier"
            | "scoped_identifier"
            | "scoped_type_identifier"
    )
}

fn extract_references_tree_sitter(content: &str, path: &str) -> Option<Vec<String>> {
    let p = std::path::Path::new(path);
    let lang = crate::chunker::detect_lang(p);
    let ts_lang = match lang {
        crate::chunker::Lang::Rust => Some(tree_sitter_rust::LANGUAGE.into()),
        crate::chunker::Lang::Python => Some(tree_sitter_python::LANGUAGE.into()),
        crate::chunker::Lang::TypeScript | crate::chunker::Lang::JavaScript => {
            Some(tree_sitter_javascript::LANGUAGE.into())
        }
        crate::chunker::Lang::Go => Some(tree_sitter_go::LANGUAGE.into()),
        crate::chunker::Lang::Cpp => Some(tree_sitter_cpp::LANGUAGE.into()),
        // No tree-sitter grammar bundled for VB6/SQL — heuristic chunking only.
        crate::chunker::Lang::Vb | crate::chunker::Lang::Sql | crate::chunker::Lang::Generic => {
            None
        }
    }?;

    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&ts_lang).ok()?;
    let tree = parser.parse(content, None)?;
    let mut refs = HashSet::new();

    fn traverse(node: tree_sitter::Node, content: &str, refs: &mut HashSet<String>) {
        let kind = node.kind();
        if is_comment_or_string(kind) {
            return;
        }

        let is_ref = is_reference_node(kind);
        if is_ref {
            let is_def = if let Some(parent) = node.parent() {
                is_definition_node(node, parent)
            } else {
                false
            };

            if !is_def {
                if let Some(text) = content.get(node.start_byte()..node.end_byte()) {
                    let cleaned = text.trim();
                    if !cleaned.is_empty() && !is_keyword(cleaned) {
                        refs.insert(cleaned.to_string());
                    }
                }
            }

            if matches!(kind, "scoped_identifier" | "scoped_type_identifier") {
                return;
            }
        }

        for i in 0..node.child_count() as u32 {
            if let Some(child) = node.child(i) {
                traverse(child, content, refs);
            }
        }
    }

    traverse(tree.root_node(), content, &mut refs);
    let mut result: Vec<String> = refs.into_iter().collect();
    result.sort();
    Some(result)
}

fn extract_references_regex(content: &str) -> Vec<String> {
    let call_re = Regex::new(r"\b([A-Za-z_][A-Za-z0-9_]*)\s*\(").unwrap();
    let path_call_re =
        Regex::new(r"\b([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)+)\s*\(").unwrap();
    let method_call_re = Regex::new(r"\.\s*([A-Za-z_][A-Za-z0-9_]*)\s*\(").unwrap();

    let mut refs = HashSet::new();
    for cap in path_call_re.captures_iter(content) {
        if let Some(name) = cap.get(1).map(|m| m.as_str()) {
            refs.insert(name.to_string());
        }
    }
    for cap in method_call_re.captures_iter(content) {
        if let Some(name) = cap.get(1).map(|m| m.as_str()) {
            if !is_keyword(name) {
                refs.insert(name.to_string());
            }
        }
    }
    for cap in call_re.captures_iter(content) {
        if let Some(name) = cap.get(1).map(|m| m.as_str()) {
            if !is_keyword(name) {
                refs.insert(name.to_string());
            }
        }
    }

    let mut refs: Vec<String> = refs.into_iter().collect();
    refs.sort();
    refs
}

fn extract_references(content: &str, path: &str) -> Vec<String> {
    if let Some(refs) = extract_references_tree_sitter(content, path) {
        refs
    } else {
        extract_references_regex(content)
    }
}

fn extract_import_aliases(content: &str) -> HashMap<String, String> {
    let mut aliases = HashMap::new();
    extract_rust_use_aliases(content, &mut aliases);
    extract_ts_import_aliases(content, &mut aliases);
    aliases
}

fn extract_rust_use_aliases(content: &str, aliases: &mut HashMap<String, String>) {
    let group_re = Regex::new(r"\buse\s+([^;{]+)::\{([^}]+)\}\s*;").unwrap();
    for cap in group_re.captures_iter(content) {
        let Some(prefix) = cap.get(1).map(|m| m.as_str().trim()) else {
            continue;
        };
        let Some(items) = cap.get(2).map(|m| m.as_str()) else {
            continue;
        };
        for item in items
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
        {
            let parts: Vec<&str> = item.split_whitespace().collect();
            if parts.len() == 3 && parts[1] == "as" {
                aliases.insert(parts[2].to_string(), format!("{prefix}::{}", parts[0]));
            } else if parts.len() == 1 {
                aliases.insert(parts[0].to_string(), format!("{prefix}::{}", parts[0]));
            }
        }
    }

    let direct_re =
        Regex::new(r"\buse\s+([A-Za-z_][A-Za-z0-9_:]*(?:::[A-Za-z_][A-Za-z0-9_]*)+)(?:\s+as\s+([A-Za-z_][A-Za-z0-9_]*))?\s*;")
            .unwrap();
    for cap in direct_re.captures_iter(content) {
        let Some(path) = cap.get(1).map(|m| m.as_str()) else {
            continue;
        };
        let short = short_reference_name(path);
        let alias = cap.get(2).map(|m| m.as_str()).unwrap_or(&short);
        aliases.insert(alias.to_string(), path.to_string());
    }
}

fn extract_ts_import_aliases(content: &str, aliases: &mut HashMap<String, String>) {
    let named_re = Regex::new(r#"\bimport\s+\{([^}]+)\}\s+from\s+['"][^'"]+['"]"#).unwrap();
    for cap in named_re.captures_iter(content) {
        let Some(items) = cap.get(1).map(|m| m.as_str()) else {
            continue;
        };
        for item in items
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
        {
            let parts: Vec<&str> = item.split_whitespace().collect();
            if parts.len() == 3 && parts[1] == "as" {
                aliases.insert(parts[2].to_string(), parts[0].to_string());
            } else if parts.len() == 1 {
                aliases.insert(parts[0].to_string(), parts[0].to_string());
            }
        }
    }
}

fn normalize_name(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

fn is_keyword(name: &str) -> bool {
    KEYWORDS.iter().any(|kw| kw.eq_ignore_ascii_case(name))
}

/// Escape text destined for HTML *element* content. Symbol names and paths reach
/// this file from the repository, so a name containing `<` must not become markup.
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Serialize for embedding inside a `<script>` block. JSON escaping alone is not
/// enough: `serde_json` leaves `/` untouched, so a path or symbol containing
/// `</script>` would close the block and everything after it would be parsed as
/// HTML. Escaping the slash keeps the value identical to JavaScript while making
/// the sequence unrepresentable.
fn script_safe_json(value: &[serde_json::Value]) -> String {
    serde_json::to_string_pretty(value)
        .unwrap_or_else(|_| "[]".to_string())
        .replace("</", "<\\/")
}

pub fn export_relations_to_html(relations: &[GraphRelation], title: &str) -> String {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut seen_nodes = std::collections::HashSet::new();

    let get_color = |kind: &str| -> &str {
        match kind.to_lowercase().as_str() {
            "function" | "method" => "#38bdf8",
            "struct" | "class" | "interface" => "#34d399",
            "enum" => "#f472b6",
            "module" | "file" => "#a78bfa",
            _ => "#94a3b8",
        }
    };

    for rel in relations {
        if seen_nodes.insert(rel.from.chunk_id) {
            nodes.push(serde_json::json!({
                "id": rel.from.chunk_id,
                "label": format!("{}\n[{}]", rel.from.name, rel.from.kind),
                "title": format!("{}: L{}-{}", rel.from.path, rel.from.start_line, rel.from.end_line),
                "color": {
                    "background": get_color(&rel.from.kind),
                    "border": "#1e293b"
                }
            }));
        }
        if seen_nodes.insert(rel.to.chunk_id) {
            nodes.push(serde_json::json!({
                "id": rel.to.chunk_id,
                "label": format!("{}\n[{}]", rel.to.name, rel.to.kind),
                "title": format!("{}: L{}-{}", rel.to.path, rel.to.start_line, rel.to.end_line),
                "color": {
                    "background": get_color(&rel.to.kind),
                    "border": "#1e293b"
                }
            }));
        }
        edges.push(serde_json::json!({
            "from": rel.from.chunk_id,
            "to": rel.to.chunk_id,
            "label": rel.reference,
            "title": format!("Kind: {}", rel.edge_kind)
        }));
    }

    let nodes_json = script_safe_json(&nodes);
    let edges_json = script_safe_json(&edges);
    let title = html_escape(title);

    format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <title>Tokenix Impact Graph - {}</title>
    <!-- Version-pinned with an SRI digest: the unversioned URL resolved to
         whatever unpkg served at open time, so a compromised or breaking
         upstream release executed here with no way to notice. `integrity`
         makes the browser refuse a modified file. -->
    <script type="text/javascript"
            src="https://unpkg.com/vis-network@9.1.9/standalone/umd/vis-network.min.js"
            integrity="sha384-yxKDWWf0wwdUj/gPeuL11czrnKFQROnLgY8ll7En9NYoXibgg3C6NK/UDHNtUgWJ"
            crossorigin="anonymous"
            referrerpolicy="no-referrer"></script>
    <style type="text/css">
        body {{
            background-color: #0f172a;
            color: #f8fafc;
            font-family: 'Segoe UI', Tahoma, Geneva, Verdana, sans-serif;
            margin: 0;
            padding: 0;
            overflow: hidden;
        }}
        #network {{
            width: 100vw;
            height: 100vh;
        }}
        .header {{
            position: absolute;
            top: 20px;
            left: 20px;
            z-index: 10;
            background: rgba(15, 23, 42, 0.85);
            padding: 15px;
            border-radius: 8px;
            border: 1px solid #334155;
            backdrop-filter: blur(4px);
        }}
        h1 {{ margin: 0 0 5px 0; font-size: 20px; color: #38bdf8; }}
        p {{ margin: 0; font-size: 12px; color: #94a3b8; }}
    </style>
</head>
<body>
    <div class="header">
        <h1>{}</h1>
        <p>Tokenix Bidirectional Impact Relationship Graph</p>
    </div>
    <div id="network"></div>
    <script type="text/javascript">
        // The renderer is a remote script: offline, or on an integrity mismatch,
        // it simply never defines `vis` and the page would be blank with no
        // explanation. Say so instead.
        if (typeof vis === 'undefined') {{
            document.getElementById('network').textContent =
                'vis-network could not be loaded (offline, blocked, or integrity mismatch). ' +
                'The graph data is in the page source.';
            throw new Error('vis-network unavailable');
        }}
        var nodes = new vis.DataSet({});
        var edges = new vis.DataSet({});
        var container = document.getElementById('network');
        var data = {{ nodes: nodes, edges: edges }};
        var options = {{
            nodes: {{
                shape: 'dot',
                size: 20,
                font: {{ color: '#f8fafc', size: 12, face: 'monospace' }},
                borderWidth: 2,
                shadow: true
            }},
            edges: {{
                width: 2,
                color: {{ color: '#64748b', highlight: '#38bdf8' }},
                arrows: {{ to: {{ enabled: true, scaleFactor: 0.5 }} }},
                shadow: true,
                font: {{ color: '#94a3b8', size: 10, align: 'middle' }}
            }},
            physics: {{
                barnesHut: {{ gravitationalConstant: -2000, centralGravity: 0.3, springLength: 120 }},
                minVelocity: 0.75
            }}
        }};
        var network = new vis.Network(container, data, options);
    </script>
</body>
</html>
"#,
        title, title, nodes_json, edges_json
    )
}

// ---- File-level import graph --------------------------------------------------

/// Scan every indexed file for import statements and persist file→file edges.
/// Reads source from disk (imports live between symbol chunks, so DB chunks
/// can't see them). Unresolvable targets are stored with `resolved_path NULL`
/// — still queryable as "which files touch <external dep>".
pub fn rebuild_import_graph(conn: &Connection, repo_root: &std::path::Path) -> Result<usize> {
    let paths = store::all_file_paths(conn)?;
    let known: HashSet<&str> = paths.iter().map(String::as_str).collect();

    store::clear_import_graph(conn)?;
    let mut count = 0usize;
    conn.execute_batch("BEGIN IMMEDIATE")?;
    for path in &paths {
        let Ok(content) = std::fs::read_to_string(repo_root.join(path)) else {
            continue;
        };
        for (target, kind, line) in extract_file_imports(path, &content) {
            let resolved = resolve_import_target(path, &target, &kind, &known);
            store::insert_import(
                conn,
                &store::ImportEdge {
                    source_path: path.clone(),
                    target,
                    resolved_path: resolved,
                    kind,
                    line,
                },
            )?;
            count += 1;
        }
    }
    conn.execute_batch("COMMIT")?;
    Ok(count)
}

/// Extract (target, kind, 1-based line) import statements for the file's language.
fn extract_file_imports(path: &str, content: &str) -> Vec<(String, String, usize)> {
    let lang = crate::chunker::detect_lang(std::path::Path::new(path));
    use crate::chunker::Lang;
    let patterns: &[(&str, &str)] = match lang {
        Lang::Rust => &[
            (
                r"^\s*(?:pub(?:\([^)]*\))?\s+)?use\s+([A-Za-z_][A-Za-z0-9_:]*)",
                "use",
            ),
            (
                r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+([a-z_][a-z0-9_]*)\s*;",
                "mod",
            ),
        ],
        Lang::Python => &[
            (r"^\s*from\s+([.\w]+)\s+import\b", "import"),
            (r"^\s*import\s+([.\w]+)", "import"),
        ],
        Lang::TypeScript | Lang::JavaScript => &[
            (
                r#"^\s*(?:import|export)\s+(?:[^'"]*?\s+from\s+)?['"]([^'"]+)['"]"#,
                "import",
            ),
            (r#"\brequire\(\s*['"]([^'"]+)['"]\s*\)"#, "require"),
        ],
        Lang::Go => &[(r#"^\s*(?:import\s+)?(?:\w+\s+)?"([^"]+)""#, "import")],
        Lang::Cpp => &[
            (r#"^\s*#\s*include\s+"([^"]+)""#, "include"),
            (r"^\s*#\s*include\s+<([^>]+)>", "include"),
        ],
        // VB6/SQL have no file-level import statements worth graphing.
        Lang::Vb | Lang::Sql => return Vec::new(),
        Lang::Generic => {
            if path.ends_with(".sh") || path.ends_with(".bash") {
                &[(r"^\s*(?:source|\.)\s+(\S+)", "source")]
            } else {
                return Vec::new();
            }
        }
    };

    let compiled: Vec<(Regex, &str)> = patterns
        .iter()
        .filter_map(|(p, k)| Regex::new(p).ok().map(|re| (re, *k)))
        .collect();

    // Go `import "x"` lines outside import blocks would false-positive on any
    // string literal; restrict Go matching to import statements/blocks.
    let mut in_go_import_block = false;
    let mut out = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        if lang == Lang::Go {
            let trimmed = line.trim_start();
            if trimmed.starts_with("import (") {
                in_go_import_block = true;
                continue;
            }
            if in_go_import_block && trimmed.starts_with(')') {
                in_go_import_block = false;
                continue;
            }
            if !in_go_import_block && !trimmed.starts_with("import") {
                continue;
            }
        }
        for (re, kind) in &compiled {
            if let Some(cap) = re.captures(line) {
                if let Some(m) = cap.get(1) {
                    out.push((m.as_str().to_string(), kind.to_string(), idx + 1));
                    break;
                }
            }
        }
    }
    out
}

/// Best-effort heuristic mapping of an import target to an indexed repo file.
fn resolve_import_target(
    source_path: &str,
    target: &str,
    kind: &str,
    known: &HashSet<&str>,
) -> Option<String> {
    let dir = source_path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");

    let exists = |p: &str| -> Option<String> {
        let normalized = normalize_rel_path(p);
        known.contains(normalized.as_str()).then_some(normalized)
    };

    match kind {
        "use" => {
            // Rust: crate::a::b → src/a/b.rs | src/a.rs | src/a/mod.rs (walk
            // segments longest-first so `crate::store::open_db` hits store.rs).
            let segs: Vec<&str> = target
                .split("::")
                .filter(|s| !["crate", "self", "super"].contains(s))
                .collect();
            for n in (1..=segs.len()).rev() {
                let base = segs[..n].join("/");
                for cand in [
                    format!("src/{base}.rs"),
                    format!("src/{base}/mod.rs"),
                    format!("{base}.rs"),
                ] {
                    if let Some(hit) = exists(&cand) {
                        return Some(hit);
                    }
                }
            }
            None
        }
        "mod" => {
            // Rust: mod foo; → sibling foo.rs / foo/mod.rs.
            for cand in [
                format!("{dir}/{target}.rs"),
                format!("{dir}/{target}/mod.rs"),
            ] {
                if let Some(hit) = exists(cand.trim_start_matches('/')) {
                    return Some(hit);
                }
            }
            None
        }
        "import" | "require" if target.starts_with('.') => {
            // JS/TS relative or Python relative (leading dots).
            if target.contains('/') || !target.contains('.') || target.ends_with(".js") {
                // JS/TS style: ./x, ../x/y
                let joined = format!("{dir}/{target}");
                let exts = ["", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"];
                for ext in exts {
                    if let Some(hit) = exists(&format!("{joined}{ext}")) {
                        return Some(hit);
                    }
                }
                for idx in ["index.ts", "index.tsx", "index.js"] {
                    if let Some(hit) = exists(&format!("{joined}/{idx}")) {
                        return Some(hit);
                    }
                }
                None
            } else {
                // Python relative: .mod / ..pkg.mod
                let dots = target.chars().take_while(|&c| c == '.').count();
                let rest = &target[dots..];
                let mut base = dir.to_string();
                for _ in 1..dots {
                    base = base
                        .rsplit_once('/')
                        .map(|(d, _)| d.to_string())
                        .unwrap_or_default();
                }
                let modpath = rest.replace('.', "/");
                for cand in [
                    format!("{base}/{modpath}.py"),
                    format!("{base}/{modpath}/__init__.py"),
                ] {
                    if let Some(hit) = exists(cand.trim_start_matches('/')) {
                        return Some(hit);
                    }
                }
                None
            }
        }
        "import" => {
            // Python absolute: a.b.c → a/b/c.py | a/b/c/__init__.py.
            // (Go package paths and JS bare specifiers won't match and stay external.)
            let modpath = target.replace('.', "/");
            for cand in [
                format!("{modpath}.py"),
                format!("{modpath}/__init__.py"),
                format!("src/{modpath}.py"),
            ] {
                if let Some(hit) = exists(&cand) {
                    return Some(hit);
                }
            }
            None
        }
        "include" => {
            // C/C++: resolve relative to the including file, then repo root.
            for cand in [format!("{dir}/{target}"), target.to_string()] {
                if let Some(hit) = exists(cand.trim_start_matches('/')) {
                    return Some(hit);
                }
            }
            None
        }
        "source" => exists(&format!("{dir}/{target}")).or_else(|| exists(target)),
        _ => None,
    }
}

/// Collapse `a/b/../c` and `./` segments into a clean repo-relative path.
fn normalize_rel_path(p: &str) -> String {
    let normalized = p.replace('\\', "/");
    let mut out: Vec<&str> = Vec::new();
    for seg in normalized.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    out.join("/")
}

pub fn format_imports(edges: &[store::ImportEdge], title: &str, reverse: bool) -> String {
    if edges.is_empty() {
        return format!("No imports found for: {title}");
    }
    let mut out = format!("## {title}\n");
    for e in edges {
        if reverse {
            out.push_str(&format!(
                "- {}:{} [{}] imports `{}`\n",
                e.source_path,
                e.line,
                e.kind,
                e.resolved_path.as_deref().unwrap_or(&e.target),
            ));
        } else {
            match &e.resolved_path {
                Some(r) => out.push_str(&format!(
                    "- {}:{} [{}] -> {}\n",
                    e.source_path, e.line, e.kind, r
                )),
                None => out.push_str(&format!(
                    "- {}:{} [{}] -> {} (external)\n",
                    e.source_path, e.line, e.kind, e.target
                )),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{init_schema, insert_chunk, upsert_file, NewChunk};
    use rusqlite::Connection;

    #[test]
    fn exported_html_escapes_the_title() {
        // The impact target is user input and reaches both <title> and <h1>.
        let html = export_relations_to_html(&[], "<img src=x onerror=alert(1)>");
        assert!(
            !html.contains("<img src=x"),
            "raw markup reached the document"
        );
        assert!(html.contains("&lt;img src=x onerror=alert(1)&gt;"));
    }

    #[test]
    fn embedded_json_cannot_close_the_script_block() {
        // A path or symbol containing `</script>` would end the block and let the
        // remainder be parsed as HTML. serde_json does not escape `/`, so we do.
        let payload = vec![serde_json::json!({ "label": "a</script><b>" })];
        let json = script_safe_json(&payload);
        assert!(!json.contains("</script>"), "{json}");
        assert!(json.contains("<\\/script>"), "{json}");
    }

    #[test]
    fn html_escape_covers_the_five_significant_characters() {
        assert_eq!(
            html_escape(r#"&<>"'"#),
            "&amp;&lt;&gt;&quot;&#39;",
            "ampersand must be escaped first or the others double-encode"
        );
    }

    #[test]
    fn incremental_update_matches_full_rebuild() {
        use crate::store::delete_chunks_for_file;

        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn, 4).unwrap();

        let mk = |conn: &Connection, file_id: i64, path: &str, sym: &str, content: &str| {
            insert_chunk(
                conn,
                NewChunk {
                    file_id,
                    path,
                    start: 1,
                    end: 5,
                    symbol: sym,
                    kind: "function",
                    content,
                    token_count: 5,
                },
            )
            .unwrap()
        };

        let fa = upsert_file(&conn, "src/a.rs", 1.0, "ha").unwrap();
        let fb = upsert_file(&conn, "src/b.rs", 1.0, "hb").unwrap();
        let fc = upsert_file(&conn, "src/c.rs", 1.0, "hc").unwrap();
        mk(
            &conn,
            fa,
            "src/a.rs",
            "alpha",
            "fn alpha() { beta_helper(); }",
        );
        mk(
            &conn,
            fb,
            "src/b.rs",
            "beta_helper",
            "fn beta_helper() { gamma_util(); }",
        );
        mk(&conn, fc, "src/c.rs", "gamma_util", "fn gamma_util() {}");

        // Snapshot of the full rebuild's edges as name pairs.
        let edge_names = |conn: &Connection| -> std::collections::BTreeSet<(String, String)> {
            crate::store::load_all_graph_edges(conn)
                .unwrap()
                .into_iter()
                .map(|(_, from, _, _, to, _)| (from, to))
                .collect()
        };
        rebuild_symbol_graph(&conn).unwrap();
        let full = edge_names(&conn);
        assert!(full.contains(&("alpha".into(), "beta_helper".into())));
        assert!(full.contains(&("beta_helper".into(), "gamma_util".into())));

        // Simulate a re-index of src/b.rs: cascade delete + re-chunk.
        delete_chunks_for_file(&conn, fb).unwrap();
        let fb2 = upsert_file(&conn, "src/b.rs", 2.0, "hb2").unwrap();
        mk(
            &conn,
            fb2,
            "src/b.rs",
            "beta_helper",
            "fn beta_helper() { gamma_util(); }",
        );

        update_symbol_graph_incremental(&conn, &["src/b.rs".to_string()]).unwrap();
        let incremental = edge_names(&conn);
        assert_eq!(
            full, incremental,
            "incremental repair must reproduce the full rebuild's edge set"
        );
    }

    #[test]
    fn extracts_imports_per_language() {
        let rust = extract_file_imports(
            "src/main.rs",
            "use crate::store::open_db;\npub mod daemon;\nuse anyhow::Result;\n",
        );
        assert!(rust.contains(&("crate::store::open_db".into(), "use".into(), 1)));
        assert!(rust.contains(&("daemon".into(), "mod".into(), 2)));
        assert!(rust.contains(&("anyhow::Result".into(), "use".into(), 3)));

        let ts = extract_file_imports(
            "src/app.ts",
            "import { x } from './utils';\nimport React from 'react';\nconst y = require('../lib/db');\n",
        );
        assert!(ts.contains(&("./utils".into(), "import".into(), 1)));
        assert!(ts.contains(&("react".into(), "import".into(), 2)));
        assert!(ts.contains(&("../lib/db".into(), "require".into(), 3)));

        let py = extract_file_imports(
            "pkg/svc.py",
            "from .models import User\nimport os.path\nfrom pkg.db import conn\n",
        );
        assert!(py.contains(&(".models".into(), "import".into(), 1)));
        assert!(py.contains(&("os.path".into(), "import".into(), 2)));
        assert!(py.contains(&("pkg.db".into(), "import".into(), 3)));

        let c = extract_file_imports("src/a.c", "#include \"util.h\"\n#include <stdio.h>\n");
        assert!(c.contains(&("util.h".into(), "include".into(), 1)));
        assert!(c.contains(&("stdio.h".into(), "include".into(), 2)));
    }

    #[test]
    fn resolves_import_targets_against_known_files() {
        let known: HashSet<&str> = [
            "src/store.rs",
            "src/daemon.rs",
            "src/utils.ts",
            "lib/db.ts",
            "pkg/models.py",
            "pkg/db.py",
            "src/util.h",
        ]
        .into_iter()
        .collect();

        // Rust: longest-prefix walk lands on the module file.
        assert_eq!(
            resolve_import_target("src/main.rs", "crate::store::open_db", "use", &known),
            Some("src/store.rs".into())
        );
        assert_eq!(
            resolve_import_target("src/main.rs", "daemon", "mod", &known),
            Some("src/daemon.rs".into())
        );
        assert_eq!(
            resolve_import_target("src/main.rs", "anyhow::Result", "use", &known),
            None // external
        );
        // TS relative with extension probing + `..` normalization.
        assert_eq!(
            resolve_import_target("src/app.ts", "./utils", "import", &known),
            Some("src/utils.ts".into())
        );
        assert_eq!(
            resolve_import_target("src/app.ts", "../lib/db", "require", &known),
            Some("lib/db.ts".into())
        );
        // Python relative + absolute.
        assert_eq!(
            resolve_import_target("pkg/svc.py", ".models", "import", &known),
            Some("pkg/models.py".into())
        );
        assert_eq!(
            resolve_import_target("pkg/svc.py", "pkg.db", "import", &known),
            Some("pkg/db.py".into())
        );
        // C include relative to the including file.
        assert_eq!(
            resolve_import_target("src/a.c", "util.h", "include", &known),
            Some("src/util.h".into())
        );
        assert_eq!(
            resolve_import_target("src/a.c", "stdio.h", "include", &known),
            None
        );
    }

    #[test]
    fn extracts_function_and_method_references() {
        let refs = extract_references(
            "fn a() { foo(); user.save(); crate::bar::baz(); if ready() {} }",
            "test.rs",
        );
        assert!(refs.contains(&"foo".to_string()));
        assert!(refs.contains(&"save".to_string()));
        assert!(refs.contains(&"crate::bar::baz".to_string()));
        assert!(refs.contains(&"ready".to_string()));
        assert!(!refs.contains(&"if".to_string()));
    }

    #[test]
    fn tree_sitter_ignores_comments_and_strings() {
        let code = r#"
            // This is a comment calling ignored_comment_func()
            /* Another ignored_block_func() comment */
            fn my_func() {
                let some_str = "ignored_string_func()";
                actual_func();
            }
        "#;
        let refs = extract_references(code, "test.rs");
        assert!(!refs.contains(&"ignored_comment_func".to_string()));
        assert!(!refs.contains(&"ignored_block_func".to_string()));
        assert!(!refs.contains(&"ignored_string_func".to_string()));
        assert!(refs.contains(&"actual_func".to_string()));
    }

    #[test]
    fn pagerank_ranks_widely_referenced_node_highest() {
        // 1,2,3 all reference 4; 4 references nothing. Node 4 must rank highest.
        let nodes = vec![1, 2, 3, 4];
        let edges = vec![(1, 4), (2, 4), (3, 4)];
        let ranks: HashMap<i64, f32> = pagerank(&nodes, &edges).into_iter().collect();
        let central = ranks[&4];
        assert!(
            [1, 2, 3].iter().all(|id| central > ranks[id]),
            "central node should outrank its callers: {ranks:?}"
        );
    }

    #[test]
    fn pagerank_empty_graph_is_safe() {
        assert!(pagerank(&[], &[]).is_empty());
    }

    #[test]
    fn rebuild_symbol_graph_links_callers_and_callees() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn, 4).unwrap();
        let file_id = upsert_file(&conn, "src/main.rs", 1.0, "abc").unwrap();
        insert_chunk(
            &conn,
            NewChunk {
                file_id,
                path: "src/main.rs",
                start: 1,
                end: 3,
                symbol: "caller",
                kind: "function",
                content: "fn caller() { callee(); }",
                token_count: 6,
            },
        )
        .unwrap();
        insert_chunk(
            &conn,
            NewChunk {
                file_id,
                path: "src/main.rs",
                start: 5,
                end: 7,
                symbol: "callee",
                kind: "function",
                content: "fn callee() {}",
                token_count: 4,
            },
        )
        .unwrap();

        rebuild_symbol_graph(&conn).unwrap();

        let callers = store::graph_callers(&conn, "callee", 10).unwrap();
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0].from.name, "caller");
        assert_eq!(callers[0].to.name, "callee");

        let callees = store::graph_callees(&conn, "caller", 10).unwrap();
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0].to.name, "callee");
    }

    #[test]
    fn qualified_reference_prefers_matching_module_path() {
        let mut by_name = HashMap::new();
        by_name.insert(
            "insert_chunk".to_string(),
            vec![
                SymbolTarget {
                    chunk_id: 1,
                    path: "src/other.rs".to_string(),
                },
                SymbolTarget {
                    chunk_id: 2,
                    path: "src/store.rs".to_string(),
                },
            ],
        );

        let targets = resolve_reference_targets(&by_name, "store::insert_chunk");
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].chunk_id, 2);
    }

    #[test]
    fn crate_qualified_reference_uses_last_module_as_hint() {
        let mut by_name = HashMap::new();
        by_name.insert(
            "rebuild_symbol_graph".to_string(),
            vec![SymbolTarget {
                chunk_id: 7,
                path: "src/graph.rs".to_string(),
            }],
        );

        let targets = resolve_reference_targets(&by_name, "crate::graph::rebuild_symbol_graph");
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].chunk_id, 7);
    }

    #[test]
    fn rust_use_alias_expands_reference_target() {
        let aliases = extract_import_aliases(
            "use crate::store::{insert_chunk as put_chunk, upsert_file};\nfn x(){ put_chunk(); }",
        );
        assert_eq!(
            aliases.get("put_chunk").map(String::as_str),
            Some("crate::store::insert_chunk")
        );
        assert_eq!(
            aliases.get("upsert_file").map(String::as_str),
            Some("crate::store::upsert_file")
        );
    }

    #[test]
    fn ts_import_alias_expands_reference_target() {
        let aliases = extract_import_aliases(
            "import { createUser as makeUser, deleteUser } from './users';\nmakeUser();",
        );
        assert_eq!(
            aliases.get("makeUser").map(String::as_str),
            Some("createUser")
        );
        assert_eq!(
            aliases.get("deleteUser").map(String::as_str),
            Some("deleteUser")
        );
    }

    fn edge(
        from_id: i64,
        from: &str,
        from_loc: &str,
        to_id: i64,
        to: &str,
        to_loc: &str,
    ) -> store::GraphEdgeRow {
        (
            from_id,
            from.to_string(),
            from_loc.to_string(),
            to_id,
            to.to_string(),
            to_loc.to_string(),
        )
    }

    #[test]
    fn detect_cycles_reports_real_cycle_with_locations() {
        // a -> b -> a, distinct symbols: a real dependency cycle.
        let edges = vec![
            edge(1, "a", "src/a.rs:1", 2, "b", "src/b.rs:1"),
            edge(2, "b", "src/b.rs:1", 1, "a", "src/a.rs:1"),
        ];
        let cycles = detect_cycles(&edges);
        assert_eq!(cycles.len(), 1, "expected one cycle: {cycles:?}");
        let mut members = cycles[0].clone();
        members.sort();
        assert_eq!(members, vec!["a (src/a.rs:1)", "b (src/b.rs:1)"]);
    }

    #[test]
    fn detect_cycles_drops_homonym_artifact() {
        // Two same-named symbols in different files, linked by name-based
        // resolution. This is not a real cycle and must be filtered out.
        let edges = vec![
            edge(1, "now_ts", "src/a.rs:1", 2, "now_ts", "src/b.rs:1"),
            edge(2, "now_ts", "src/b.rs:1", 1, "now_ts", "src/a.rs:1"),
        ];
        assert!(
            detect_cycles(&edges).is_empty(),
            "homonym SCC should be dropped"
        );
    }

    #[test]
    fn repo_hotspots_ranks_by_degree_and_blast() {
        // hub is called by three callers; chain a->b->hub gives hub 2 transitive
        // dependents through b plus the direct callers.
        let edges = vec![
            edge(1, "caller_one", "src/a.rs:1", 4, "hub", "src/hub.rs:1"),
            edge(2, "caller_two", "src/b.rs:1", 4, "hub", "src/hub.rs:1"),
            edge(3, "caller_three", "src/c.rs:1", 4, "hub", "src/hub.rs:1"),
        ];
        let spots = repo_hotspots(&edges, 10);
        let hub = spots.iter().find(|h| h.name == "hub").expect("hub present");
        assert_eq!(hub.in_degree, 3);
        assert_eq!(hub.out_degree, 0);
        assert_eq!(hub.blast, 3, "three transitive dependents");

        // Trivial single-letter / keyword symbols are filtered out of ranking.
        let noisy = vec![edge(1, "e", "src/a.rs:1", 2, "fn", "src/b.rs:1")];
        assert!(repo_hotspots(&noisy, 10).is_empty());
    }

    #[test]
    fn format_edges_dot_emits_only_top_subgraph() {
        let edges = vec![edge(
            1,
            "alpha_fn",
            "src/a.rs:1",
            2,
            "beta_fn",
            "src/b.rs:1",
        )];
        let dot = format_edges_dot(&edges, 10);
        assert!(dot.starts_with("digraph tokenix {"));
        assert!(dot.contains("\"alpha_fn\" -> \"beta_fn\";"));
    }
}
