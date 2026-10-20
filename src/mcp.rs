//! MCP server: the symbol-graph queries an agent cannot get from grep.
//!
//! Every tool answers with locations and relations (`path:line`), never with the
//! text of the code, and none of them searches file contents: the agent reads
//! files with its own Read and searches them with its own grep. One tool set,
//! no profiles.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use crate::graph::{self, Format};

fn find_repo_root(path: &Path) -> PathBuf {
    let abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    crate::store::find_project_root(&abs)
}

/// Estimated prompt cost of the tool schemas this server advertises.
pub fn tool_schema_tokens() -> usize {
    tool_list()
        .iter()
        .map(|tool| crate::chunker::count_tokens(&tool.to_string()))
        .sum()
}

/// Every tool this server advertises, in `tools/list` order.
fn tool_list() -> Vec<Value> {
    let format = |allowed: &[&str]| {
        json!({
            "type": "string",
            "enum": allowed,
            "description": "text (default), or a diagram/data format to paste into docs and PRs"
        })
    };
    let diagram = ["text", "mermaid", "dot", "json"];
    vec![
        json!({
            "name": "tokenix_symbols",
            "description": "Locate symbols by name or path. Returns path:line, never code: Read the file for contents and use your own grep for text.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Symbol name, partial name, or path fragment"},
                    "limit": {"type": "integer", "description": "Maximum results (default 20)", "default": 20}
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": "tokenix_graph",
            "description": "Relations around a symbol: its callers, its callees, the impact graph in both directions, or the forward call flow from an entry point.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": {"type": "string", "description": "Target symbol name"},
                    "direction": {
                        "type": "string",
                        "enum": ["callers", "callees", "impact", "flow"],
                        "description": "callers: who calls it. callees: what it calls. impact: both, to `depth` hops (default). flow: forward from an entry point."
                    },
                    "depth": {"type": "integer", "description": "Hops for impact (default 2) and flow (default 3)"},
                    "limit": {"type": "integer", "description": "Maximum relationships (default 20 for callers/callees, 50 otherwise)"},
                    "format": format(&diagram)
                },
                "required": ["symbol"]
            }
        }),
        json!({
            "name": "tokenix_blast",
            "description": "Symbols that depend on what the working tree changed since a git ref: the review order for a diff.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "since": {"type": "string", "description": "Git ref to diff against (default HEAD)", "default": "HEAD"},
                    "depth": {"type": "integer", "description": "Caller hops to follow (default 2)", "default": 2},
                    "limit": {"type": "integer", "description": "Maximum rows (default 50)", "default": 50},
                    "format": format(&["text", "mermaid", "json"])
                }
            }
        }),
        json!({
            "name": "tokenix_modules",
            "description": "Functional modules found in the symbol graph: groups of symbols that mostly call each other, and how the groups depend on one another.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "top": {"type": "integer", "description": "How many modules (default 12)", "default": 12},
                    "format": format(&diagram)
                }
            }
        }),
        json!({
            "name": "tokenix_deps",
            "description": "File-level imports of a file, or with reverse=true the files that import it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": {"type": "string", "description": "Repo-relative file path"},
                    "reverse": {"type": "boolean", "description": "List importers instead of imports", "default": false},
                    "format": format(&diagram)
                },
                "required": ["file"]
            }
        }),
        json!({
            "name": "tokenix_tokenmap",
            "description": "Top-level directories and files holding the most tokens, heaviest first.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "top": {"type": "integer", "description": "Rows per list (default 15)", "default": 15}
                }
            }
        }),
    ]
}

pub fn run_mcp_server() -> Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();
    let mut line = String::new();

    while reader.read_line(&mut line)? > 0 {
        let request_str = line.trim();
        if request_str.is_empty() {
            line.clear();
            continue;
        }

        let request: Value = match serde_json::from_str(request_str) {
            Ok(val) => val,
            Err(_) => {
                // JSON-RPC 2.0 §5: error response MUST include id; use null when
                // the request could not be parsed and no id is available.
                let response = json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": {
                        "code": -32700,
                        "message": "Parse error"
                    }
                });
                let _ = writeln!(writer, "{}", response);
                let _ = writer.flush();
                line.clear();
                continue;
            }
        };

        if let Some(method) = request.get("method").and_then(|m| m.as_str()) {
            let id = request.get("id").cloned();
            let params = request.get("params").cloned().unwrap_or(Value::Null);

            let response = match method {
                "initialize" => {
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "protocolVersion": "2024-11-05",
                            "capabilities": {
                                "tools": {}
                            },
                            "serverInfo": {
                                "name": "tokenix",
                                "version": env!("CARGO_PKG_VERSION")
                            }
                        }
                    })
                }
                // Silently discard all notification messages (no response required per JSON-RPC)
                n if n.starts_with("notifications/") => {
                    line.clear();
                    continue;
                }
                "tools/list" => {
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "tools": tool_list()
                        }
                    })
                }
                "tools/call" => {
                    let tool_name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    let tool_args = params.get("arguments").cloned().unwrap_or(Value::Null);

                    match call_tool_guarded(tool_name, tool_args) {
                        Ok(text) => {
                            json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": {
                                    "content": [
                                        {
                                            "type": "text",
                                            "text": text
                                        }
                                    ]
                                }
                            })
                        }
                        Err(e) => {
                            json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": {
                                    "content": [
                                        {
                                            "type": "text",
                                            "text": format!("Error: {}", e)
                                        }
                                    ],
                                    "isError": true
                                }
                            })
                        }
                    }
                }
                _ => {
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {
                            "code": -32601,
                            "message": format!("Method '{}' not found", method)
                        }
                    })
                }
            };

            let _ = writeln!(writer, "{}", response);
            let _ = writer.flush();
        }

        line.clear();
    }

    Ok(())
}

/// `handle_tool_call` behind a panic guard. A panic anywhere below this point —
/// a slice out of range in a chunker grammar arm, an arithmetic overflow, a
/// regex assertion — used to unwind out of the stdio loop and kill the server,
/// taking every subsequent tool call in the session with it. One bad request is
/// now one `isError` response.
fn call_tool_guarded(name: &str, args: Value) -> Result<String> {
    let name_owned = name.to_string();
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        handle_tool_call(&name_owned, args)
    })) {
        Ok(result) => result,
        Err(_) => Err(anyhow!(
            "internal error: tool '{name}' panicked (the server stayed up; \
             please report this input)"
        )),
    }
}

fn handle_tool_call(name: &str, args: Value) -> Result<String> {
    let repo_root = find_repo_root(Path::new("."));

    // An MCP session outlives many edits by the agent that is calling it, so
    // every tool reconciles the working tree before it answers.
    let _ = crate::freshness::refresh_before_query(&repo_root);

    match name {
        "tokenix_symbols" => {
            let query = args
                .get("query")
                .and_then(|q| q.as_str())
                .ok_or_else(|| anyhow!("Missing 'query' argument"))?;
            let limit = arg_usize(&args, "limit", 20);
            let conn = open_existing_index(&repo_root)?;
            let nodes = crate::store::search_graph_nodes(&conn, query, limit)?;
            Ok(graph::format_nodes(
                &nodes,
                &format!("Symbols matching `{query}`"),
            ))
        }
        "tokenix_graph" => {
            let symbol = args
                .get("symbol")
                .and_then(|q| q.as_str())
                .ok_or_else(|| anyhow!("Missing 'symbol' argument"))?;
            let direction = args
                .get("direction")
                .and_then(|d| d.as_str())
                .unwrap_or("impact");
            let format = arg_format(&args, DIAGRAM_FORMATS)?;
            let conn = open_existing_index(&repo_root)?;
            let (relations, title) = match direction {
                "callers" => (
                    crate::store::graph_callers(&conn, symbol, arg_usize(&args, "limit", 20))?,
                    format!("Callers of `{symbol}`"),
                ),
                "callees" => (
                    crate::store::graph_callees(&conn, symbol, arg_usize(&args, "limit", 20))?,
                    format!("Callees of `{symbol}`"),
                ),
                "impact" => (
                    crate::store::graph_impact(
                        &conn,
                        symbol,
                        arg_usize(&args, "depth", 2),
                        arg_usize(&args, "limit", 50),
                    )?,
                    format!("Impact graph for `{symbol}`"),
                ),
                "flow" => (
                    crate::store::graph_flow(
                        &conn,
                        symbol,
                        arg_usize(&args, "depth", 3),
                        arg_usize(&args, "limit", 50),
                    )?,
                    format!("Call flow from `{symbol}`"),
                ),
                other => {
                    return Err(anyhow!(
                        "Invalid direction '{other}'. Use: callers | callees | impact | flow"
                    ))
                }
            };
            graph::render_relations(&relations, &title, format)
        }
        "tokenix_blast" => {
            let since = args.get("since").and_then(|s| s.as_str()).unwrap_or("HEAD");
            let format = arg_format(&args, &[Format::Text, Format::Mermaid, Format::Json])?;
            let conn = open_existing_index(&repo_root)?;
            let report = crate::blast::analyze(
                &conn,
                &repo_root,
                since,
                arg_usize(&args, "depth", 2),
                arg_usize(&args, "limit", 50),
            )?;
            Ok(match format {
                Format::Json => serde_json::to_string_pretty(&report)?,
                Format::Mermaid => crate::blast::format_report_mermaid(&report),
                _ => crate::blast::format_report(&report),
            })
        }
        "tokenix_modules" => {
            let format = arg_format(&args, DIAGRAM_FORMATS)?;
            let conn = open_existing_index(&repo_root)?;
            let edges = crate::store::load_all_graph_edges(&conn)?;
            let map = crate::modules::detect_module_map(&edges, arg_usize(&args, "top", 12));
            if map.modules.is_empty() {
                return Ok(
                    "No modules found: the graph is too small or the index is empty.".into(),
                );
            }
            Ok(match format {
                Format::Json => serde_json::to_string_pretty(&map)?,
                Format::Mermaid => crate::modules::format_modules_mermaid(&map),
                Format::Dot => crate::modules::format_modules_dot(&map),
                _ => format!(
                    "# Modules: {} group(s) over {} edges{}",
                    map.modules.len(),
                    edges.len(),
                    crate::modules::format_modules(&map.modules)
                ),
            })
        }
        "tokenix_deps" => {
            let file = args
                .get("file")
                .and_then(|f| f.as_str())
                .ok_or_else(|| anyhow!("Missing 'file' argument"))?;
            let reverse = args
                .get("reverse")
                .and_then(|r| r.as_bool())
                .unwrap_or(false);
            let format = arg_format(&args, DIAGRAM_FORMATS)?;
            let conn = open_existing_index(&repo_root)?;
            let edges = crate::store::file_imports(&conn, file, reverse)?;
            let title = if reverse {
                format!("Files importing `{file}`")
            } else {
                format!("Imports of `{file}`")
            };
            graph::render_imports(&edges, &title, reverse, format)
        }
        "tokenix_tokenmap" => {
            let conn = open_existing_index(&repo_root)?;
            let counts = crate::store::get_file_token_counts(&conn)?;
            Ok(format_tokenmap(&counts, arg_usize(&args, "top", 15)))
        }
        _ => Err(anyhow!("Unknown tool '{}'", name)),
    }
}

/// Formats an agent can ask a graph tool for. `html` is a file format, so it
/// stays on the CLI.
const DIAGRAM_FORMATS: &[Format] = &[Format::Text, Format::Mermaid, Format::Dot, Format::Json];

fn arg_format(args: &Value, allowed: &[Format]) -> Result<Format> {
    let Some(raw) = args.get("format").and_then(|f| f.as_str()) else {
        return Ok(Format::Text);
    };
    let format = Format::parse(raw)?;
    if !allowed.contains(&format) {
        return Err(anyhow!("format '{raw}' is not available for this tool"));
    }
    Ok(format)
}

fn arg_usize(args: &Value, key: &str, default: usize) -> usize {
    args.get(key)
        .and_then(|v| v.as_u64())
        .map_or(default, |v| (v as usize).max(1))
}

/// Heaviest top-level directories and files by indexed tokens.
fn format_tokenmap(counts: &[(String, i64)], top: usize) -> String {
    if counts.is_empty() {
        return "No files in the index.".to_string();
    }
    let total: i64 = counts.iter().map(|(_, t)| t).sum();
    let mut dirs: std::collections::BTreeMap<&str, i64> = std::collections::BTreeMap::new();
    for (path, tokens) in counts {
        let top_dir = path.split_once('/').map_or("(root files)", |(d, _)| d);
        *dirs.entry(top_dir).or_default() += tokens;
    }
    let mut dirs: Vec<_> = dirs.into_iter().collect();
    dirs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let mut files: Vec<_> = counts.iter().collect();
    files.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    let pct = |t: i64| t as f64 * 100.0 / total.max(1) as f64;
    let mut out = format!("# Token map: {} tokens in {} files\n", total, counts.len());
    out.push_str("\n## Directories\n");
    for (dir, tokens) in dirs.iter().take(top) {
        out.push_str(&format!("- {dir}: {tokens} ({:.1}%)\n", pct(*tokens)));
    }
    out.push_str("\n## Files\n");
    for (path, tokens) in files.iter().take(top) {
        out.push_str(&format!("- {path}: {tokens} ({:.1}%)\n", pct(*tokens)));
    }
    out
}

fn open_existing_index(repo_root: &Path) -> Result<rusqlite::Connection> {
    crate::store::open_db(repo_root, false)?
        .ok_or_else(|| anyhow!("Index not found. Please index the workspace first."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenmap_ranks_directories_and_files_by_tokens() {
        let counts = vec![
            ("src/a.rs".to_string(), 300),
            ("src/b.rs".to_string(), 100),
            ("docs/x.md".to_string(), 250),
            ("README.md".to_string(), 50),
        ];
        let out = format_tokenmap(&counts, 2);
        assert!(out.contains("700 tokens in 4 files"), "{out}");
        let src = out.find("- src: 400 (57.1%)").expect("src row");
        let docs = out.find("- docs: 250 (35.7%)").expect("docs row");
        assert!(src < docs, "heaviest directory first: {out}");
        assert!(!out.contains("README.md"), "top=2 keeps two files: {out}");
        assert!(out.contains("- src/a.rs: 300"), "{out}");
    }

    #[test]
    fn one_tool_set_of_structure_queries_and_nothing_that_reads_or_runs() {
        let names: Vec<String> = tool_list()
            .iter()
            .filter_map(|t| t["name"].as_str().map(str::to_string))
            .collect();
        assert_eq!(
            names,
            [
                "tokenix_symbols",
                "tokenix_graph",
                "tokenix_blast",
                "tokenix_modules",
                "tokenix_deps",
                "tokenix_tokenmap"
            ]
        );
        assert!(tool_schema_tokens() > 300);
    }

    #[test]
    fn arg_format_defaults_to_text_and_rejects_unknown_or_unavailable_formats() {
        let none = json!({});
        assert_eq!(arg_format(&none, DIAGRAM_FORMATS).unwrap(), Format::Text);
        let mermaid = json!({"format": "Mermaid"});
        assert_eq!(
            arg_format(&mermaid, DIAGRAM_FORMATS).unwrap(),
            Format::Mermaid
        );
        assert!(arg_format(&json!({"format": "html"}), DIAGRAM_FORMATS).is_err());
        assert!(arg_format(&json!({"format": "dot"}), &[Format::Text]).is_err());
        assert!(arg_format(&json!({"format": "svg"}), DIAGRAM_FORMATS).is_err());
    }
}
