use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Wall-clock cap for `tokenix_run`, overridable per environment.
fn run_timeout_secs() -> u64 {
    std::env::var("TOKENIX_MCP_RUN_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(300)
}

/// Run a child with a wall-clock cap. The MCP server is single-threaded, so an
/// interactive or hung command would otherwise block every tokenix tool for the
/// rest of the session with no way to recover.
fn run_with_timeout(
    mut cmd: std::process::Command,
    timeout: Duration,
) -> Result<std::process::Output> {
    let child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(timeout) {
        Ok(res) => res.map_err(Into::into),
        Err(_) => {
            kill_pid(pid);
            Err(anyhow!(
                "command exceeded the {}s timeout and was terminated",
                timeout.as_secs()
            ))
        }
    }
}

/// Kill by pid — the `Child` has been moved into the waiting thread, so
/// `Child::kill` is not reachable from here.
fn kill_pid(pid: u32) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
}

/// Resolve a tool-supplied path against the repo root and reject anything that
/// escapes it: absolute paths, `..` traversal, or a symlink pointing outside.
/// A non-existent (but in-bounds) path is returned so the caller can report
/// "File not found" instead of a confinement error.
fn confine_to_repo(repo_root: &Path, file: &str) -> Result<PathBuf> {
    use std::path::Component;
    let rel = Path::new(file);
    if rel.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::Prefix(_) | Component::RootDir
        )
    }) {
        return Err(anyhow!(
            "path must be relative to the repository root: {}",
            file
        ));
    }
    let candidate = repo_root.join(rel);
    let root = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_path_buf());
    match candidate.canonicalize() {
        Ok(c) if c.starts_with(&root) => Ok(c),
        Ok(_) => Err(anyhow!("path escapes the repository root: {}", file)),
        Err(_) => Ok(candidate),
    }
}

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
    json!([
                                {
                                    "name": "tokenix_read",
                                    "description": "Read one repo file from disk: the whole file, one `symbol`, or a `lines` range. Files over 1000 lines return an outline that omits comments, tests and usages: use `lines` or your native Read before editing or renaming.",
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
                                            },
                                            "outline": {
                                                "type": "boolean",
                                                "description": "Return the outline even for a short file"
                                            }
                                        },
                                        "required": ["file"]
                                    }
                                },
                                {
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
                                    "name": "tokenix_flow",
                                    "description": "Trace the call flow forward from an entry-point symbol",
                                    "inputSchema": {
                                        "type": "object",
                                        "properties": {
                                            "symbol": {"type": "string", "description": "Entry-point symbol name"},
                                            "depth": {"type": "integer", "description": "Call hops to follow (default 3)", "default": 3},
                                            "limit": {"type": "integer", "description": "Maximum relationships (default 50)", "default": 50}
                                        },
                                        "required": ["symbol"]
                                    }
                                },
                                {
                                    "name": "tokenix_blast",
                                    "description": "Symbols that depend on what the working tree changed since a git ref",
                                    "inputSchema": {
                                        "type": "object",
                                        "properties": {
                                            "since": {"type": "string", "description": "Git ref to diff against (default HEAD)", "default": "HEAD"},
                                            "depth": {"type": "integer", "description": "Caller hops to follow (default 2)", "default": 2},
                                            "limit": {"type": "integer", "description": "Maximum rows (default 50)", "default": 50}
                                        }
                                    }
                                },
                                {
                                    "name": "tokenix_modules",
                                    "description": "Functional modules found in the symbol graph: groups of symbols that mostly call each other",
                                    "inputSchema": {
                                        "type": "object",
                                        "properties": {
                                            "top": {"type": "integer", "description": "How many modules (default 12)", "default": 12}
                                        }
                                    }
                                },
                                {
                                    "name": "tokenix_deps",
                                    "description": "File-level imports of a file, or with reverse=true the files that import it",
                                    "inputSchema": {
                                        "type": "object",
                                        "properties": {
                                            "file": {"type": "string", "description": "Repo-relative file path"},
                                            "reverse": {"type": "boolean", "description": "List importers instead of imports", "default": false}
                                        },
                                        "required": ["file"]
                                    }
                                },
                                {
                                    "name": "tokenix_tokenmap",
                                    "description": "Top-level directories and files holding the most tokens, heaviest first",
                                    "inputSchema": {
                                        "type": "object",
                                        "properties": {
                                            "top": {"type": "integer", "description": "Rows per list (default 15)", "default": 15}
                                        }
                                    }
                                },
                                {
                                    "name": "tokenix_memory_add",
                                    "description": "Save a durable user preference for future sessions. Scope defaults to project.",
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
                                    "description": "Show measured token savings statistics from tokenix hooks",
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
                            ]).as_array().cloned().unwrap_or_default()
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

/// Read a repo file for `tokenix_read`: refuse anything above the index size
/// cap (an unbounded read is a memory sink) and mask known credential shapes,
/// since this path hands raw file bytes to the agent without a hook in between.
fn read_capped_redacted(path: &Path, cap: u64) -> Result<String> {
    let len = std::fs::metadata(path)?.len();
    if len > cap {
        return Err(anyhow!(
            "File is {len} bytes, above the {cap}-byte read cap (index max_file_bytes)"
        ));
    }
    let content = std::fs::read_to_string(path)?;
    Ok(crate::secrets_scan::redact_known_secrets(&content).0)
}

fn handle_tool_call(name: &str, args: Value) -> Result<String> {
    let path = Path::new(".");
    let repo_root = find_repo_root(path);

    // An MCP session outlives many edits by the agent that is calling it, so a
    // retrieval tool has to reconcile the working tree before it answers.
    // Non-retrieval tools (memory, gain, run) never touch the index.
    if matches!(
        name,
        "tokenix_read"
            | "tokenix_symbols"
            | "tokenix_callers"
            | "tokenix_callees"
            | "tokenix_impact"
            | "tokenix_flow"
            | "tokenix_blast"
            | "tokenix_modules"
            | "tokenix_deps"
            | "tokenix_tokenmap"
    ) {
        let _ = crate::freshness::refresh_before_query(&repo_root);
    }

    match name {
        "tokenix_read" => {
            let file = args
                .get("file")
                .and_then(|f| f.as_str())
                .ok_or_else(|| anyhow!("Missing 'file' argument"))?;
            let symbol = args.get("symbol").and_then(|s| s.as_str());
            let lines = args.get("lines").and_then(|l| l.as_str());

            // The schema documents `file` as a repo-relative path. Honour that:
            // an absolute path or a `..` escape reaching over the MCP wire would
            // be an unlogged read of anything the process can open (~/.claude.json,
            // .env, SSH keys).
            let fp = confine_to_repo(&repo_root, file)?;

            if !fp.exists() {
                return Err(anyhow!("File not found: {}", file));
            }

            let cap = crate::chunker::index_config()
                .max_file_bytes
                .unwrap_or(crate::indexer::MAX_INDEX_FILE_BYTES);
            let content = read_capped_redacted(&fp, cap)?;
            let file_lines: Vec<&str> = content.lines().collect();

            if let Some(range) = lines {
                let parts: Vec<&str> = range.split('-').collect();
                if parts.len() == 2 {
                    if let (Ok(s), Ok(e)) = (parts[0].parse::<usize>(), parts[1].parse::<usize>()) {
                        // Validate like the CLI does: a descending range slices
                        // with start > end, which panics and — with no
                        // `catch_unwind` anywhere — kills the whole MCP server.
                        let total = file_lines.len();
                        if s == 0 || e == 0 || s > total || e > total || s > e {
                            return Err(anyhow!(
                                "Invalid line range. File has {} lines (1-{})",
                                total,
                                total
                            ));
                        }
                        return Ok(file_lines[s - 1..e].join("\n"));
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

            let force_outline = args
                .get("outline")
                .and_then(|o| o.as_bool())
                .unwrap_or(false);
            if force_outline || file_lines.len() > READ_FULL_MAX_LINES {
                let mut out = crate::chunker::generate_outline(&content, &rel);
                out.push_str(
                    "\n\nOutline only: comments, tests and every use of a name are omitted. \
                     Use 'symbol' or 'lines' to read parts, or your native Read before editing.",
                );
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
        "tokenix_flow" => {
            let symbol = args
                .get("symbol")
                .and_then(|q| q.as_str())
                .ok_or_else(|| anyhow!("Missing 'symbol' argument"))?;
            let depth = arg_usize(&args, "depth", 3);
            let limit = arg_usize(&args, "limit", 50);
            let conn = open_existing_index(&repo_root)?;
            let relations = crate::store::graph_flow(&conn, symbol, depth, limit)?;
            Ok(crate::graph::format_relations(
                &relations,
                &format!("Call flow from `{symbol}`"),
            ))
        }
        "tokenix_blast" => {
            let since = args.get("since").and_then(|s| s.as_str()).unwrap_or("HEAD");
            let depth = arg_usize(&args, "depth", 2);
            let limit = arg_usize(&args, "limit", 50);
            let conn = open_existing_index(&repo_root)?;
            let report = crate::blast::analyze(&conn, &repo_root, since, depth, limit)?;
            Ok(crate::blast::format_report(&report))
        }
        "tokenix_modules" => {
            let top = arg_usize(&args, "top", 12);
            let conn = open_existing_index(&repo_root)?;
            let edges = crate::store::load_all_graph_edges(&conn)?;
            let found = crate::modules::detect_modules(&edges, top);
            if found.is_empty() {
                return Ok(
                    "No modules found: the graph is too small or the index is empty.".into(),
                );
            }
            Ok(format!(
                "# Modules: {} group(s) over {} edges{}",
                found.len(),
                edges.len(),
                crate::modules::format_modules(&found)
            ))
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
            let conn = open_existing_index(&repo_root)?;
            let edges = crate::store::file_imports(&conn, file, reverse)?;
            let title = if reverse {
                format!("Files importing `{file}`")
            } else {
                format!("Imports of `{file}`")
            };
            Ok(crate::graph::format_imports(&edges, &title, reverse))
        }
        "tokenix_tokenmap" => {
            let top = arg_usize(&args, "top", 15);
            let conn = open_existing_index(&repo_root)?;
            let counts = crate::store::get_file_token_counts(&conn)?;
            Ok(format_tokenmap(&counts, top))
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

            let cmd = if cfg!(windows) {
                let mut c = std::process::Command::new("cmd");
                c.args(["/C", command]);
                c
            } else {
                let mut c = std::process::Command::new("sh");
                c.args(["-c", command]);
                c
            };

            let output = run_with_timeout(cmd, Duration::from_secs(run_timeout_secs()))?;
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

fn parse_memory_scope(scope: &str) -> Result<crate::memory::PreferenceScope> {
    match scope {
        "global" => Ok(crate::memory::PreferenceScope::Global),
        "project" => Ok(crate::memory::PreferenceScope::Project),
        other => Err(anyhow!("Invalid scope '{}'. Use: global | project", other)),
    }
}

fn invokes_tokenix_binary(command: &str) -> bool {
    if command
        .split(['&', '|', ';'])
        .filter_map(first_shell_word)
        .any(|word| is_tokenix_executable(&word))
    {
        return true;
    }
    // A shell runner hides the real program from the first-word test:
    // `sh -c "tokenix gain"` and `cmd /C "tokenix gain"` contain no split
    // operator, so the guard saw only `sh`/`cmd` and let the recursion through.
    // Scan every word once a shell runner is in play.
    let mut words = command.split_whitespace();
    let Some(prog) = words.next() else {
        return false;
    };
    let prog = prog
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(prog)
        .trim_end_matches(".exe")
        .to_ascii_lowercase();
    if !matches!(
        prog.as_str(),
        "sh" | "bash" | "zsh" | "cmd" | "pwsh" | "powershell"
    ) {
        return false;
    }
    words.any(|w| is_tokenix_executable(w.trim_matches(|c| c == '"' || c == '\'')))
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

/// Files up to this many lines come back whole from `tokenix_read`.
const READ_FULL_MAX_LINES: usize = 1000;

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
    fn tool_list_has_the_structure_tools_and_counts_its_own_weight() {
        let names: Vec<String> = tool_list()
            .iter()
            .filter_map(|t| t["name"].as_str().map(str::to_string))
            .collect();
        for want in [
            "tokenix_symbols",
            "tokenix_flow",
            "tokenix_blast",
            "tokenix_modules",
            "tokenix_deps",
            "tokenix_tokenmap",
        ] {
            assert!(names.iter().any(|n| n == want), "{want} missing: {names:?}");
        }
        assert!(tool_schema_tokens() > 500);
    }

    #[test]
    fn read_capped_redacted_masks_keys_and_enforces_cap() {
        let dir = std::env::temp_dir().join(format!("tokenix-mcp-read-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.txt");
        std::fs::write(&f, "key = AKIAIOSFODNN7EXAMPLE\nok\n").unwrap();
        let out = read_capped_redacted(&f, 1024).unwrap();
        assert!(!out.contains("AKIAIOSFODNN7EXAMPLE"), "{out}");
        assert!(out.contains("ok"));
        assert!(read_capped_redacted(&f, 4).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

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
