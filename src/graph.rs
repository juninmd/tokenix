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
        let aliases = extract_import_aliases(&chunk.content);
        for reference in extract_references(&chunk.content, &chunk.path) {
            let resolved_reference = aliases
                .get(&reference)
                .or_else(|| aliases.get(&short_reference_name(&reference)))
                .map(String::as_str)
                .unwrap_or(&reference);
            let targets = resolve_reference_targets(&by_name, resolved_reference);
            if targets.is_empty() {
                continue;
            }
            for target in targets {
                if target.chunk_id == chunk.chunk_id {
                    continue;
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
    }

    let node_ids: Vec<i64> = chunks.iter().map(|c| c.chunk_id).collect();
    let edges: Vec<(i64, i64)> = inserted.into_iter().collect();
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
pub fn detect_cycles(edges: &[store::GraphEdgeRow]) -> Vec<Vec<String>> {
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
        return targets.iter().collect();
    }

    let preferred: Vec<&SymbolTarget> = targets
        .iter()
        .filter(|target| {
            qualifiers
                .iter()
                .any(|qualifier| path_matches_qualifier(&target.path, qualifier))
        })
        .collect();
    if preferred.is_empty() {
        targets.iter().collect()
    } else {
        preferred
    }
}

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
        crate::chunker::Lang::Generic => None,
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

    let nodes_json = serde_json::to_string_pretty(&nodes).unwrap_or_else(|_| "[]".to_string());
    let edges_json = serde_json::to_string_pretty(&edges).unwrap_or_else(|_| "[]".to_string());

    format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <title>Tokenix Impact Graph - {}</title>
    <script type="text/javascript" src="https://unpkg.com/vis-network/standalone/umd/vis-network.min.js"></script>
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{init_schema, insert_chunk, upsert_file, NewChunk};
    use rusqlite::Connection;

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
}
