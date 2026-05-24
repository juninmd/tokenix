use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

fn find_repo_root(path: &Path) -> PathBuf {
    let abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    crate::store::find_project_root(&abs)
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
                let response = json!({
                    "jsonrpc": "2.0",
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
                "notifications/initialized" => {
                    line.clear();
                    continue;
                }
                "tools/list" => {
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "tools": [
                                {
                                    "name": "tokenix_query",
                                    "description": "Semantic search over the indexed codebase repository",
                                    "inputSchema": {
                                        "type": "object",
                                        "properties": {
                                            "query": {
                                                "type": "string",
                                                "description": "Natural language query, e.g. 'how does authentication work'"
                                            },
                                            "budget": {
                                                "type": "integer",
                                                "description": "Optional token budget limit for query context (default 3000)",
                                                "default": 3000
                                            },
                                            "file_filter": {
                                                "type": "string",
                                                "description": "Optional path or name filter to search a specific file"
                                            }
                                        },
                                        "required": ["query"]
                                    }
                                },
                                {
                                    "name": "tokenix_context",
                                    "description": "PRIMARY TOOL: build focused task context in one call by combining semantic search, preference-memory capture guidance, entry points, and compact file outlines",
                                    "inputSchema": {
                                        "type": "object",
                                        "properties": {
                                            "task": {
                                                "type": "string",
                                                "description": "Task, feature, bug, or architecture question to gather context for"
                                            },
                                            "budget": {
                                                "type": "integer",
                                                "description": "Optional token budget limit for returned context (default 3000)",
                                                "default": 3000
                                            },
                                            "max_files": {
                                                "type": "integer",
                                                "description": "Maximum number of file outlines to include (default 4)",
                                                "default": 4
                                            }
                                        },
                                        "required": ["task"]
                                    }
                                },
                                {
                                    "name": "tokenix_explore",
                                    "description": "Graph-aware exploration in one capped call: preference-memory capture guidance, entry points, relationship map, and source grouped by file. Use after tokenix_context when you need implementation details.",
                                    "inputSchema": {
                                        "type": "object",
                                        "properties": {
                                            "query": {
                                                "type": "string",
                                                "description": "Symbol names, file names, or focused task terms to explore"
                                            },
                                            "budget": {
                                                "type": "integer",
                                                "description": "Optional token budget limit for returned source context (default 4000)",
                                                "default": 4000
                                            },
                                            "max_symbols": {
                                                "type": "integer",
                                                "description": "Maximum seed symbols to expand (default 8)",
                                                "default": 8
                                            }
                                        },
                                        "required": ["query"]
                                    }
                                },
                                {
                                    "name": "tokenix_read",
                                    "description": "Smart outline, symbol, or line-range file reader (token-efficient)",
                                    "inputSchema": {
                                        "type": "object",
                                        "properties": {
                                            "file": {
                                                "type": "string",
                                                "description": "Relative path to the file to read"
                                            },
                                            "symbol": {
                                                "type": "string",
                                                "description": "Optional symbol/class/function name to extract"
                                            },
                                            "lines": {
                                                "type": "string",
                                                "description": "Optional line range, e.g. '1-50'"
                                            }
                                        },
                                        "required": ["file"]
                                    }
                                },
                                {
                                    "name": "tokenix_symbols",
                                    "description": "Find indexed symbols by name or path",
                                    "inputSchema": {
                                        "type": "object",
                                        "properties": {
                                            "query": {"type": "string", "description": "Symbol name, partial name, or path fragment"},
                                            "limit": {"type": "integer", "description": "Maximum results (default 20)", "default": 20}
                                        },
                                        "required": ["query"]
                                    }
                                },
                                {
                                    "name": "tokenix_callers",
                                    "description": "Find symbols that call or reference a target symbol",
                                    "inputSchema": {
                                        "type": "object",
                                        "properties": {
                                            "symbol": {"type": "string", "description": "Target symbol name"},
                                            "limit": {"type": "integer", "description": "Maximum relationships (default 20)", "default": 20}
                                        },
                                        "required": ["symbol"]
                                    }
                                },
                                {
                                    "name": "tokenix_callees",
                                    "description": "Find symbols called or referenced by a target symbol",
                                    "inputSchema": {
                                        "type": "object",
                                        "properties": {
                                            "symbol": {"type": "string", "description": "Target symbol name"},
                                            "limit": {"type": "integer", "description": "Maximum relationships (default 20)", "default": 20}
                                        },
                                        "required": ["symbol"]
                                    }
                                },
                                {
                                    "name": "tokenix_impact",
                                    "description": "Show a bidirectional impact graph around a symbol",
                                    "inputSchema": {
                                        "type": "object",
                                        "properties": {
                                            "symbol": {"type": "string", "description": "Target symbol name"},
                                            "depth": {"type": "integer", "description": "Graph traversal depth (default 2)", "default": 2},
                                            "limit": {"type": "integer", "description": "Maximum relationships (default 50)", "default": 50}
                                        },
                                        "required": ["symbol"]
                                    }
                                },
                                {
                                    "name": "tokenix_memory_add",
                                    "description": "Save a durable user preference for future tokenix context. Scope defaults to project.",
                                    "inputSchema": {
                                        "type": "object",
                                        "properties": {
                                            "text": {
                                                "type": "string",
                                                "description": "Preference to remember, e.g. 'Prefer Biome over ESLint for linting migrations'"
                                            },
                                            "scope": {
                                                "type": "string",
                                                "description": "Preference scope: project or global",
                                                "default": "project"
                                            }
                                        },
                                        "required": ["text"]
                                    }
                                },
                                {
                                    "name": "tokenix_memory_list",
                                    "description": "List saved tokenix preferences from global, project, or all scopes",
                                    "inputSchema": {
                                        "type": "object",
                                        "properties": {
                                            "scope": {
                                                "type": "string",
                                                "description": "Scope to list: all, project, or global",
                                                "default": "all"
                                            }
                                        }
                                    }
                                },
                                {
                                    "name": "tokenix_gain",
                                    "description": "Show estimated token savings statistics from tokenix hooks",
                                    "inputSchema": {
                                        "type": "object",
                                        "properties": {
                                            "history": {
                                                "type": "boolean",
                                                "description": "Optional flag to show per-call history",
                                                "default": false
                                            }
                                        }
                                    }
                                }
                            ]
                        }
                    })
                }
                "tools/call" => {
                    let tool_name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    let tool_args = params.get("arguments").cloned().unwrap_or(Value::Null);

                    match handle_tool_call(tool_name, tool_args) {
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

fn handle_tool_call(name: &str, args: Value) -> Result<String> {
    let path = Path::new(".");
    let repo_root = find_repo_root(path);

    match name {
        "tokenix_query" => {
            let query = args
                .get("query")
                .and_then(|q| q.as_str())
                .ok_or_else(|| anyhow!("Missing 'query' argument"))?;
            let budget = args.get("budget").and_then(|b| b.as_u64()).unwrap_or(3000) as usize;
            let file_filter = args.get("file_filter").and_then(|f| f.as_str());

            let results = crate::query::query_index(&repo_root, query, budget, 20, file_filter)?
                .ok_or_else(|| anyhow!("Index not found. Please index the workspace first."))?;

            Ok(crate::query::format_results(&results, query))
        }
        "tokenix_context" => {
            let task = args
                .get("task")
                .and_then(|q| q.as_str())
                .ok_or_else(|| anyhow!("Missing 'task' argument"))?;
            let budget = args.get("budget").and_then(|b| b.as_u64()).unwrap_or(3000) as usize;
            let max_files = args.get("max_files").and_then(|b| b.as_u64()).unwrap_or(4) as usize;

            crate::query::build_task_context(&repo_root, task, budget, max_files)
        }
        "tokenix_explore" => {
            let query = args
                .get("query")
                .and_then(|q| q.as_str())
                .ok_or_else(|| anyhow!("Missing 'query' argument"))?;
            let budget = args.get("budget").and_then(|b| b.as_u64()).unwrap_or(4000) as usize;
            let max_symbols = args
                .get("max_symbols")
                .and_then(|b| b.as_u64())
                .unwrap_or(8) as usize;

            crate::query::build_explore_context(&repo_root, query, budget, max_symbols)
        }
        "tokenix_read" => {
            let file = args
                .get("file")
                .and_then(|f| f.as_str())
                .ok_or_else(|| anyhow!("Missing 'file' argument"))?;
            let symbol = args.get("symbol").and_then(|s| s.as_str());
            let lines = args.get("lines").and_then(|l| l.as_str());

            let fp = {
                let p = Path::new(file);
                if p.exists() {
                    p.to_path_buf()
                } else {
                    repo_root.join(file)
                }
            };

            if !fp.exists() {
                return Err(anyhow!("File not found: {}", file));
            }

            let content = std::fs::read_to_string(&fp)?;
            let file_lines: Vec<&str> = content.lines().collect();

            if let Some(range) = lines {
                let parts: Vec<&str> = range.split('-').collect();
                if parts.len() == 2 {
                    if let (Ok(s), Ok(e)) = (parts[0].parse::<usize>(), parts[1].parse::<usize>()) {
                        let slice =
                            file_lines[s.saturating_sub(1)..e.min(file_lines.len())].join("\n");
                        return Ok(slice);
                    }
                }
                return Err(anyhow!("Invalid 'lines' range format. Use: N-M"));
            }

            let rel = fp
                .strip_prefix(&repo_root)
                .unwrap_or(&fp)
                .to_string_lossy()
                .replace('\\', "/");

            if let Some(sym) = symbol {
                let chunks = crate::chunker::chunk_file(&rel, &content);
                let found: Vec<_> = chunks
                    .iter()
                    .filter(|c| c.symbol.to_lowercase().contains(&sym.to_lowercase()))
                    .collect();
                if found.is_empty() {
                    return Err(anyhow!("Symbol '{}' not found in file", sym));
                }
                let mut out = String::new();
                for c in found {
                    out.push_str(&format!(
                        "# L{}-{} [{}] {}\n",
                        c.start_line, c.end_line, c.kind, c.symbol
                    ));
                    out.push_str(&c.content);
                    out.push_str("\n\n");
                }
                return Ok(out.trim_end().to_string());
            }

            if file_lines.len() >= 200 {
                let mut out = crate::chunker::generate_outline(&content, &rel);
                out.push_str("\n\nUse 'symbol' or 'lines' arguments to read specific parts.");
                Ok(out)
            } else {
                Ok(content)
            }
        }
        "tokenix_symbols" => {
            let query = args
                .get("query")
                .and_then(|q| q.as_str())
                .ok_or_else(|| anyhow!("Missing 'query' argument"))?;
            let limit = args.get("limit").and_then(|b| b.as_u64()).unwrap_or(20) as usize;
            let conn = open_existing_index(&repo_root)?;
            let nodes = crate::store::search_graph_nodes(&conn, query, limit)?;
            Ok(crate::graph::format_nodes(
                &nodes,
                &format!("Symbols matching `{query}`"),
            ))
        }
        "tokenix_callers" | "tokenix_callees" => {
            let symbol = args
                .get("symbol")
                .and_then(|q| q.as_str())
                .ok_or_else(|| anyhow!("Missing 'symbol' argument"))?;
            let limit = args.get("limit").and_then(|b| b.as_u64()).unwrap_or(20) as usize;
            let conn = open_existing_index(&repo_root)?;
            let callers = name == "tokenix_callers";
            let relations = if callers {
                crate::store::graph_callers(&conn, symbol, limit)?
            } else {
                crate::store::graph_callees(&conn, symbol, limit)?
            };
            let title = if callers {
                format!("Callers of `{symbol}`")
            } else {
                format!("Callees of `{symbol}`")
            };
            Ok(crate::graph::format_relations(&relations, &title))
        }
        "tokenix_impact" => {
            let symbol = args
                .get("symbol")
                .and_then(|q| q.as_str())
                .ok_or_else(|| anyhow!("Missing 'symbol' argument"))?;
            let depth = args.get("depth").and_then(|b| b.as_u64()).unwrap_or(2) as usize;
            let limit = args.get("limit").and_then(|b| b.as_u64()).unwrap_or(50) as usize;
            let conn = open_existing_index(&repo_root)?;
            let relations = crate::store::graph_impact(&conn, symbol, depth, limit)?;
            Ok(crate::graph::format_relations(
                &relations,
                &format!("Impact graph for `{symbol}`"),
            ))
        }
        "tokenix_memory_add" => {
            let text = args
                .get("text")
                .and_then(|q| q.as_str())
                .ok_or_else(|| anyhow!("Missing 'text' argument"))?;
            let scope = args
                .get("scope")
                .and_then(|q| q.as_str())
                .unwrap_or("project");
            let scope = match scope {
                "global" => crate::memory::PreferenceScope::Global,
                "project" => crate::memory::PreferenceScope::Project,
                other => return Err(anyhow!("Invalid scope '{}'. Use: global | project", other)),
            };
            let path = crate::memory::add_preference(&repo_root, scope, text)?;
            Ok(format!("saved {}", path.display()))
        }
        "tokenix_memory_list" => {
            let scope = args.get("scope").and_then(|q| q.as_str()).unwrap_or("all");
            let (include_global, include_project) = match scope {
                "all" => (true, true),
                "global" => (true, false),
                "project" => (false, true),
                other => {
                    return Err(anyhow!(
                        "Invalid scope '{}'. Use: all | global | project",
                        other
                    ))
                }
            };
            crate::memory::list_preferences(&repo_root, include_global, include_project)
        }
        "tokenix_gain" => {
            let history = args
                .get("history")
                .and_then(|h| h.as_bool())
                .unwrap_or(false);

            let stats = crate::gain::compute_gain(&repo_root);
            let project_name = repo_root
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("project");

            let mut out = String::new();
            out.push_str(&format!("Project: {}\n", project_name));
            out.push_str(&format!(
                "Total tool calls intercepted: {}\n",
                stats.total_calls
            ));
            out.push_str(&format!(
                "Blocked large files read: {}\n",
                stats.intercepted
            ));
            out.push_str(&format!(
                "Original tokens requested: {}\n",
                stats.tokens_original
            ));
            out.push_str(&format!("Sent tokens: {}\n", stats.tokens_used));
            out.push_str(&format!(
                "Wasted/Overhead tokens: {}\n",
                stats.tokens_original - stats.tokens_saved - stats.tokens_used
            ));
            out.push_str(&format!(
                "Estimated savings: {} tokens ({:.1}%)\n",
                stats.tokens_saved,
                if stats.tokens_original > 0 {
                    (stats.tokens_saved as f64 / stats.tokens_original as f64) * 100.0
                } else {
                    0.0
                }
            ));

            if history {
                let events = crate::store::read_hook_log(&repo_root);
                if !events.is_empty() {
                    out.push_str("\n--- History ---\n");
                    for call in events.iter().rev().take(50) {
                        let secs = call.ts as u64;
                        let h = (secs / 3600) % 24;
                        let m = (secs / 60) % 60;
                        let s = secs % 60;
                        out.push_str(&format!(
                            "[{:02}:{:02}:{:02}] tool={} action={} saved={} original={}\n",
                            h,
                            m,
                            s,
                            call.tool,
                            call.action,
                            call.saved_tokens,
                            call.original_estimate
                        ));
                    }
                }
            }
            Ok(out)
        }
        _ => Err(anyhow!("Unknown tool '{}'", name)),
    }
}

fn open_existing_index(repo_root: &Path) -> Result<rusqlite::Connection> {
    crate::store::open_db(repo_root, false)?
        .ok_or_else(|| anyhow!("Index not found. Please index the workspace first."))
}
