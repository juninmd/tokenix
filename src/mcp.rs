use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpProfile {
    Slim,
    Full,
}

fn find_repo_root(path: &Path) -> PathBuf {
    let abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    crate::store::find_project_root(&abs)
}

fn slim_tools() -> Vec<Value> {
    vec![
        json!({
            "name": "tokenix_context",
            "description": "Build focused repository context for a task. Prefer this first.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task": {"type": "string"},
                    "mode": {"type": "string", "enum": ["plan", "debug", "audit", "security", "review"], "default": "plan"},
                    "budget": {"type": "integer", "default": 3000},
                    "max_files": {"type": "integer", "default": 4}
                },
                "required": ["task"]
            }
        }),
        json!({
            "name": "tokenix_search_tools",
            "description": "Find tokenix tool names for query/read/graph/memory/run/gain capabilities.",
            "inputSchema": {
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "required": ["query"]
            }
        }),
        json!({
            "name": "tokenix_call",
            "description": "Call a tokenix tool by name with JSON arguments after tokenix_search_tools.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "arguments": {"type": "object"}
                },
                "required": ["name"]
            }
        }),
    ]
}

pub fn tool_schema_tokens(profile: McpProfile) -> usize {
    let schemas = match profile {
        McpProfile::Slim => slim_tools(),
        McpProfile::Full => full_tool_estimate(),
    };
    schemas
        .iter()
        .map(|tool| crate::chunker::count_tokens(&tool.to_string()))
        .sum()
}

fn full_tool_estimate() -> Vec<Value> {
    [
        ("tokenix_query", "Semantic search over the indexed codebase repository"),
        ("tokenix_context", "Build focused task context in one call"),
        ("tokenix_explore", "Graph-aware source and relationship context"),
        ("tokenix_read", "Smart outline, symbol, or line-range file reader"),
        ("tokenix_symbols", "Find indexed symbols by name or path"),
        ("tokenix_callers", "Find symbols that call a target symbol"),
        ("tokenix_callees", "Find symbols called by a target symbol"),
        ("tokenix_impact", "Show bidirectional impact graph around a symbol"),
        ("tokenix_memory_add", "Save durable preference memory"),
        ("tokenix_memory_list", "List preference memory"),
        ("tokenix_memory_remove", "Remove preference memory"),
        ("tokenix_memory_edit", "Edit preference memory"),
        ("tokenix_run", "Run shell command with compressed output"),
        ("tokenix_gain", "Show hook token savings"),
    ]
    .into_iter()
    .map(|(name, description)| {
        json!({
            "name": name,
            "description": description,
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "task": {"type": "string"},
                    "budget": {"type": "integer"},
                    "file": {"type": "string"},
                    "symbol": {"type": "string"},
                    "limit": {"type": "integer"},
                    "command": {"type": "string"},
                    "scope": {"type": "string"},
                    "history": {"type": "boolean"}
                }
            }
        })
    })
    .collect()
}

pub fn run_mcp_server(profile: McpProfile) -> Result<()> {
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
                    if profile == McpProfile::Slim {
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "tools": slim_tools()
                            }
                        })
                    } else {
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
                                                "mode": {
                                                    "type": "string",
                                                    "description": "Context mode: plan, debug, audit, security, or review",
                                                    "enum": ["plan", "debug", "audit", "security", "review"],
                                                    "default": "plan"
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
                                        "name": "tokenix_memory_remove",
                                        "description": "Remove saved preferences matching text from project or global memory",
                                        "inputSchema": {
                                            "type": "object",
                                            "properties": {
                                                "query": {"type": "string", "description": "Text to match in saved preferences"},
                                                "scope": {"type": "string", "description": "Scope to edit: project or global", "default": "project"}
                                            },
                                            "required": ["query"]
                                        }
                                    },
                                    {
                                        "name": "tokenix_memory_edit",
                                        "description": "Replace saved preferences matching text in project or global memory",
                                        "inputSchema": {
                                            "type": "object",
                                            "properties": {
                                                "query": {"type": "string", "description": "Text to match in saved preferences"},
                                                "replacement": {"type": "string", "description": "Replacement preference text"},
                                                "scope": {"type": "string", "description": "Scope to edit: project or global", "default": "project"}
                                            },
                                            "required": ["query", "replacement"]
                                        }
                                    },
                                    {
                                        "name": "tokenix_run",
                                        "description": "Execute a shell command with token-efficient output compression (removes noise, truncates long logs)",
                                        "inputSchema": {
                                            "type": "object",
                                            "properties": {
                                                "command": {
                                                    "type": "string",
                                                    "description": "Shell command to execute, e.g. 'npm install' or 'cargo test'"
                                                }
                                            },
                                            "required": ["command"]
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
        "tokenix_search_tools" => {
            let query = args
                .get("query")
                .and_then(|q| q.as_str())
                .ok_or_else(|| anyhow!("Missing 'query' argument"))?;
            Ok(search_tool_catalog(query))
        }
        "tokenix_call" => {
            let tool = args
                .get("name")
                .and_then(|q| q.as_str())
                .ok_or_else(|| anyhow!("Missing 'name' argument"))?;
            let arguments = args.get("arguments").cloned().unwrap_or(Value::Null);
            if matches!(tool, "tokenix_call" | "tokenix_search_tools") {
                return Err(anyhow!("Refusing recursive tokenix_call"));
            }
            handle_tool_call(tool, arguments)
        }
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
            let mode = parse_context_mode(
                args.get("mode")
                    .and_then(|m| m.as_str())
                    .unwrap_or("plan"),
            )?;

            crate::query::build_task_context_with_mode(&repo_root, task, mode, budget, max_files)
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
            let scope = parse_memory_scope(scope)?;
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
        "tokenix_memory_remove" => {
            let query = args
                .get("query")
                .and_then(|q| q.as_str())
                .ok_or_else(|| anyhow!("Missing 'query' argument"))?;
            let scope = parse_memory_scope(
                args.get("scope")
                    .and_then(|q| q.as_str())
                    .unwrap_or("project"),
            )?;
            let (path, count) = crate::memory::remove_preference(&repo_root, scope, query)?;
            Ok(format!("removed {} from {}", count, path.display()))
        }
        "tokenix_memory_edit" => {
            let query = args
                .get("query")
                .and_then(|q| q.as_str())
                .ok_or_else(|| anyhow!("Missing 'query' argument"))?;
            let replacement = args
                .get("replacement")
                .and_then(|q| q.as_str())
                .ok_or_else(|| anyhow!("Missing 'replacement' argument"))?;
            let scope = parse_memory_scope(
                args.get("scope")
                    .and_then(|q| q.as_str())
                    .unwrap_or("project"),
            )?;
            let (path, count) =
                crate::memory::edit_preference(&repo_root, scope, query, replacement)?;
            Ok(format!("edited {} in {}", count, path.display()))
        }
        "tokenix_run" => {
            let command = args
                .get("command")
                .and_then(|q| q.as_str())
                .ok_or_else(|| anyhow!("Missing 'command' argument"))?;

            if invokes_tokenix_binary(command) {
                return Err(anyhow!("Cannot run tokenix recursively via MCP"));
            }

            let mut cmd = if cfg!(windows) {
                let mut c = std::process::Command::new("cmd");
                c.args(["/C", command]);
                c
            } else {
                let mut c = std::process::Command::new("sh");
                c.args(["-c", command]);
                c
            };

            let output = cmd.output()?;
            let stdout_raw = String::from_utf8_lossy(&output.stdout);
            let stderr_raw = String::from_utf8_lossy(&output.stderr);

            // Capture raw output if a `tokenix filter record` session is active.
            crate::recordings::capture(&repo_root, command, &stdout_raw, &stderr_raw);

            let stdout_compressed = crate::compress::compress_bash_output(command, &stdout_raw);
            let stderr_compressed = crate::compress::compress_bash_output(command, &stderr_raw);

            // Log savings
            let original_tokens = (crate::chunker::count_tokens(&stdout_raw)
                + crate::chunker::count_tokens(&stderr_raw))
                as i64;
            let actual_tokens = (crate::chunker::count_tokens(&stdout_compressed)
                + crate::chunker::count_tokens(&stderr_compressed))
                as i64;
            let saved = (original_tokens - actual_tokens).max(0);

            if saved > 0 {
                let _ = crate::store::log_hook_event(
                    &repo_root,
                    &crate::store::HookEvent {
                        ts: crate::compress::now_ts(),
                        tool: "MCP".to_string(),
                        action: "intercepted".to_string(),
                        phase: "mcp_run".to_string(),
                        reason: "compressed command output".to_string(),
                        saved_tokens: saved,
                        actual_tokens,
                        original_estimate: original_tokens,
                        input_preview: command.chars().take(200).collect(),
                        command: command.to_string(),
                    },
                );
            }

            let mut combined = String::new();
            if !stdout_compressed.is_empty() {
                combined.push_str(&stdout_compressed);
            }
            if !stderr_compressed.is_empty() {
                if !combined.is_empty() && !combined.ends_with('\n') {
                    combined.push('\n');
                }
                combined.push_str(&stderr_compressed);
            }

            Ok(combined)
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

fn search_tool_catalog(query: &str) -> String {
    const TOOLS: &[(&str, &str)] = &[
        (
            "tokenix_query",
            "semantic code search with optional file filter",
        ),
        ("tokenix_context", "one-call focused context for a task"),
        (
            "tokenix_explore",
            "graph-aware source and relationship context",
        ),
        (
            "tokenix_read",
            "smart file outline, symbol, or line-range read",
        ),
        ("tokenix_symbols", "find indexed symbols by name or path"),
        ("tokenix_callers", "find symbols that reference a target"),
        ("tokenix_callees", "find symbols referenced by a target"),
        ("tokenix_impact", "bidirectional symbol impact graph"),
        ("tokenix_memory_add", "save durable preference memory"),
        ("tokenix_memory_list", "list saved preference memory"),
        ("tokenix_memory_remove", "remove saved preference memory"),
        ("tokenix_memory_edit", "edit saved preference memory"),
        ("tokenix_run", "run shell command with compressed output"),
        ("tokenix_gain", "show hook token savings"),
    ];
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|s| s.to_ascii_lowercase())
        .collect();
    let mut rows = Vec::new();
    for (name, desc) in TOOLS {
        let haystack = format!("{name} {desc}").to_ascii_lowercase();
        if terms.is_empty() || terms.iter().any(|term| haystack.contains(term)) {
            rows.push(format!("- {name}: {desc}"));
        }
    }
    if rows.is_empty() {
        "No tokenix tools matched. Try query, context, read, graph, memory, run, or gain."
            .to_string()
    } else {
        rows.join("\n")
    }
}

fn parse_memory_scope(scope: &str) -> Result<crate::memory::PreferenceScope> {
    match scope {
        "global" => Ok(crate::memory::PreferenceScope::Global),
        "project" => Ok(crate::memory::PreferenceScope::Project),
        other => Err(anyhow!("Invalid scope '{}'. Use: global | project", other)),
    }
}

fn parse_context_mode(mode: &str) -> Result<crate::query::ContextMode> {
    match mode {
        "plan" => Ok(crate::query::ContextMode::Plan),
        "debug" => Ok(crate::query::ContextMode::Debug),
        "audit" => Ok(crate::query::ContextMode::Audit),
        "security" => Ok(crate::query::ContextMode::Security),
        "review" => Ok(crate::query::ContextMode::Review),
        other => Err(anyhow!(
            "Invalid mode '{}'. Use: plan | debug | audit | security | review",
            other
        )),
    }
}

fn invokes_tokenix_binary(command: &str) -> bool {
    command
        .split(['&', '|', ';'])
        .filter_map(first_shell_word)
        .any(|word| is_tokenix_executable(&word))
}

fn first_shell_word(segment: &str) -> Option<String> {
    let segment = segment.trim_start();
    if segment.is_empty() {
        return None;
    }
    let mut chars = segment.chars();
    let first = chars.next()?;
    if first == '"' || first == '\'' {
        let word: String = chars.take_while(|c| *c != first).collect();
        return Some(word);
    }
    Some(
        std::iter::once(first)
            .chain(chars.take_while(|c| !c.is_whitespace()))
            .collect(),
    )
}

fn is_tokenix_executable(part: &str) -> bool {
    let normalized = part.replace('\\', "/").to_ascii_lowercase();
    let name = normalized.rsplit('/').next().unwrap_or(&normalized);
    matches!(name, "tokenix" | "tokenix.exe")
}

fn open_existing_index(repo_root: &Path) -> Result<rusqlite::Connection> {
    crate::store::open_db(repo_root, false)?
        .ok_or_else(|| anyhow!("Index not found. Please index the workspace first."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_memory_scope() {
        assert!(matches!(
            parse_memory_scope("global").unwrap(),
            crate::memory::PreferenceScope::Global
        ));
        assert!(matches!(
            parse_memory_scope("project").unwrap(),
            crate::memory::PreferenceScope::Project
        ));
        assert!(parse_memory_scope("invalid").is_err());
    }

    #[test]
    fn recursive_tokenix_command_detection_blocks_binary_invocations() {
        assert!(invokes_tokenix_binary("tokenix stats"));
        assert!(invokes_tokenix_binary("tokenix.exe hook"));
        assert!(invokes_tokenix_binary("\"C:\\Tools\\tokenix.exe\" stats"));
        assert!(invokes_tokenix_binary("./target/debug/tokenix mcp"));
        assert!(invokes_tokenix_binary("echo ok && tokenix gain"));

        assert!(!invokes_tokenix_binary("echo tokenix stats"));
        assert!(!invokes_tokenix_binary("mytokenix stats"));
        assert!(!invokes_tokenix_binary("echo ./target/debug/tokenix"));
    }
}
