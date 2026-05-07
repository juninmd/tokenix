# CLAUDE.md — tokenix

## Project

**tokenix** — Rust CLI that intercepts LLM file reads and replaces them with compact semantic context from a local embedding index. Reduces token usage 60–90%.

Stack: Rust · SQLite · fastembed (ONNX, in-process) · Claude Code `PreToolUse` hook · background daemon (TCP)

## Build & Run

```bash
cargo build                    # debug
cargo build --release          # release
cargo install --path .         # install to PATH (~/.cargo/bin)
tokenix --help
```

## Key Files

| File | Purpose |
|---|---|
| `src/main.rs` | CLI commands (clap). All `cmd_*` functions live here. |
| `src/chunker.rs` | AST chunking per language + `generate_outline()`. Token counting. |
| `src/embed.rs` | fastembed ONNX — `embed_documents()`, `embed_query()`. Model cached in `OnceCell`. |
| `src/store.rs` | SQLite schema, CRUD, cosine similarity search, hook log I/O |
| `src/indexer.rs` | File walk + incremental index pipeline. Embeds in batches of 512; wraps per-file inserts in transactions. |
| `src/query.rs` | Search + result formatting |
| `src/hook.rs` | `run_hook()` — called by Claude Code's PreToolUse hook. Tries daemon first for Grep. |
| `src/daemon.rs` | Background TCP server (port 47392). Holds model + embedding cache (LRU, max 3 projects, content cap 1000). Bounded to 4 handler threads. |
| `src/filters.rs` | Configurable output filter definitions (regex-based line keep/strip rules). |
| `src/cmd_filter.rs` | `cmd_filter` command — lists per-command compression stats from hook log. |
| `src/gain.rs` | Analytics from `.tokenix/hook.log` |

## Critical Rules

**Never lose content.** The chunker must store 100% of every indexed file. For the Read hook, generic files (.md, .txt, .yaml, .json, …) use `clean_generic_text()` — full content with markdown formatting stripped, emojis removed, whitespace trimmed. Truncated previews are forbidden. Only code files (Rust, Python, TS, Go, JS) use the symbol-based outline which preserves structure via named chunks stored in full in SQLite.

**Never break hook fallback.** `hook.rs::run_hook()` must always `exit(0)` on any error — including missing index, stale index, parse failures, and embed errors. Exiting with non-zero breaks Claude Code sessions.

**Daemon is optional.** If `tokenix serve` is not running, `handle_grep()` auto-starts it and retries once (800ms wait). If autostart fails, it falls back to direct in-process embed. Hook must never fail because the daemon is unavailable.

**Hook exit codes:** `0` = pass through (original tool runs) · `2` = block tool (hook **stderr** is sent to Claude as context). Never exit `1`. Note: Claude Code PreToolUse does NOT replace tool results with hook stdout — exit 2 blocks the tool and stderr becomes Claude's context.

**Directory filtering in indexer:** `filter_entry` for directories uses ONLY `IGNORED_DIRS`. Do NOT call `should_index()` on directories — it returns false for dirs without extensions (like `src/`) and breaks traversal.

**Token count is approximate.** `count_tokens()` = `(len + 3) / 4`. Intentional — no tiktoken dep.

## Intercept Logic

```
Read tool:
  file < 200 lines OR offset/limit set → exit 0 (pass)
  file ≥ 200 lines, no offset/limit   → return outline, exit 2 (intercept)

Grep tool:
  pattern < 3 words → exit 0 (pass — likely a regex/symbol search)
  pattern ≥ 3 words → return semantic results, exit 2 (intercept)

Index missing or >1h old → always exit 0 regardless of tool
```

## SQLite Schema

```sql
files(id, path TEXT UNIQUE, mtime REAL, content_hash TEXT)
chunks(id, file_id, path, start_line, end_line, symbol, kind, content, token_count)
embeddings(chunk_id PK, embedding BLOB)   -- float32 LE, 768 dims
meta(key PK, value)                       -- 'indexed_at' = unix timestamp
```

Hook log: `.tokenix/hook.log` — NDJSON, one `HookEvent` per line.

## Testing the Hook

```bash
tokenix index .

# Should intercept (exit 2) — large file
echo '{"tool_name":"Read","tool_input":{"file_path":"src/main.rs"}}' | tokenix hook; echo $?

# Should pass through (exit 0) — small file
echo '{"tool_name":"Read","tool_input":{"file_path":"Cargo.toml"}}' | tokenix hook; echo $?

# Should intercept (exit 2) — semantic query (auto-starts daemon on first call)
echo '{"tool_name":"Grep","tool_input":{"pattern":"how does embedding work"}}' | tokenix hook; echo $?

tokenix gain --history
```

## Daemon

```bash
tokenix serve            # start daemon (blocks; use & or detached process)
tokenix serve --port 9999
tokenix stop             # stop daemon (reads ~/.tokenix/daemon.pid)

# Health check
echo '{"type":"health"}' | nc 127.0.0.1 47392
# → {"ok":true,"cached_projects":1,"chunks":197}
```

The daemon holds the ONNX model and all embeddings in RAM. Warm Grep calls via daemon take ~80ms vs ~430ms for cold in-process embed. The daemon auto-starts on first Grep hook call — manual `tokenix serve` is not required.

**Resource limits (prevents PC freeze under parallel hooks):**
- Max **4 concurrent handler threads** (rayon `ThreadPool`) — unbounded spawning was the primary Windows freeze trigger.
- **Spawn lock** (`daemon.pid.spawning`) + PID liveness check — prevents N parallel hooks from each spawning a separate 130 MB daemon process.
- **Content cache capped at 1000 entries** per project — evicted (cleared) when exceeded; hot entries repopulate on demand from SQLite.

## Common Tasks

**Add a language:** `chunker.rs` — add to `INDEXED_EXTS`, `Lang` enum, `detect_lang()`, implement `chunk_<lang>()` following `chunk_rust()` pattern.

**Change intercept threshold:** `hook.rs` constants — `MAX_INDEX_AGE_SECS`, `MIN_LINES_FOR_OUTLINE`, `MIN_QUERY_WORDS`.

**Extend hook to other tools:** add tool name to matcher in `run_hook()`, implement `handle_<tool>()`, update `install-hook` matcher regex in `main.rs`.

**Change token budget:** `query.rs` — `DEFAULT_BUDGET` constant, or pass `--budget` flag at runtime.

## Release

Releases are automated via GitHub Actions (`.github/workflows/release.yml`). Pushing to `main` auto-creates a version tag and GitHub Release with pre-built binaries for Linux, macOS, and Windows.

To trigger manually: push a commit to `main` — the workflow reads version from `Cargo.toml`.
