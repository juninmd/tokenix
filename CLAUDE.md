# CLAUDE.md — tokenix

## Project

**tokenix** — Rust CLI that intercepts LLM file reads and replaces them with compact semantic context from a local embedding index. Reduces token usage 60–90%.

Stack: Rust · SQLite · Ollama (`nomic-embed-text`) · Claude Code `PreToolUse` hook

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
| `src/embed.rs` | Ollama HTTP — `get_embedding()`, `check_ollama()` |
| `src/store.rs` | SQLite schema, CRUD, cosine similarity search, hook log I/O |
| `src/indexer.rs` | File walk + incremental index pipeline |
| `src/query.rs` | Search + result formatting |
| `src/hook.rs` | `run_hook()` — called by Claude Code's PreToolUse hook |
| `src/gain.rs` | Analytics from `.tokenix/hook.log` |

## Critical Rules

**Never break hook fallback.** `hook.rs::run_hook()` must always `exit(0)` on any error — including missing index, stale index, parse failures, and Ollama errors. Exiting with non-zero breaks Claude Code sessions.

**Hook exit codes:** `0` = pass through (original tool runs) · `2` = intercept (hook stdout replaces tool result). Never exit `1`.

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

# Should intercept (exit 2) — semantic query
echo '{"tool_name":"Grep","tool_input":{"pattern":"how does embedding work"}}' | tokenix hook; echo $?

tokenix gain --history
```

## Common Tasks

**Add a language:** `chunker.rs` — add to `INDEXED_EXTS`, `Lang` enum, `detect_lang()`, implement `chunk_<lang>()` following `chunk_rust()` pattern.

**Change intercept threshold:** `hook.rs` constants — `MAX_INDEX_AGE_SECS`, `MIN_LINES_FOR_OUTLINE`, `MIN_QUERY_WORDS`.

**Extend hook to other tools:** add tool name to matcher in `run_hook()`, implement `handle_<tool>()`, update `install-hook` matcher regex in `main.rs`.

**Change token budget:** `query.rs` — `DEFAULT_BUDGET` constant, or pass `--budget` flag at runtime.

## Release

Releases are automated via GitHub Actions (`.github/workflows/release.yml`). Pushing to `main` auto-creates a version tag and GitHub Release with pre-built binaries for Linux, macOS, and Windows.

To trigger manually: push a commit to `main` — the workflow reads version from `Cargo.toml`.
