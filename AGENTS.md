# AGENTS.md — tokenix

This file provides guidance for AI coding agents (Claude Code, Copilot, Cursor, etc.) working on the tokenix codebase.

## Project Overview

tokenix is a Rust CLI that intercepts AI coding assistant file reads and replaces them with compact semantic context, reducing token usage by 60–90%. It uses Ollama for local embeddings and SQLite for storage.

## Codebase Structure

```
src/
├── main.rs       CLI entry (clap). Contains all command implementations (cmd_index, cmd_query, etc.)
├── chunker.rs    AST-based code chunking. Core algorithm for splitting code into semantically meaningful pieces.
├── embed.rs      Ollama HTTP client. Thin wrapper — get_embedding() and check_ollama().
├── store.rs      SQLite layer. All DB interactions: files, chunks, embeddings (BLOB), hook log.
├── indexer.rs    File walker + indexing pipeline. Calls chunker → embed → store in sequence.
├── query.rs      Cosine similarity search. Fetches all embeddings, scores, ranks, applies token budget.
├── hook.rs       PreToolUse hook logic. Reads JSON from stdin, decides intercept or pass-through.
└── gain.rs       Analytics. Reads .tokenix/hook.log and computes savings statistics.
```

## Key Design Decisions

### Cosine similarity in Rust (no sqlite-vec)
Embeddings are stored as raw `BLOB` (float32 LE bytes) in SQLite. At query time, all embeddings are loaded into memory and cosine similarity is computed in Rust. This avoids the `sqlite-vec` extension dependency (which requires native compilation) at the cost of O(n) query time. Acceptable for repos up to ~100k chunks; add HNSW index if needed beyond that.

### Token counting approximation
`count_tokens()` in `chunker.rs` uses `(len + 3) / 4` — a fast approximation (~4 chars/token for English/code). This is intentional: shipping tiktoken in Rust adds significant compile time and binary size for marginal accuracy improvement in budget decisions.

### Hook exit codes
- Exit `0` → pass through (Claude Code runs the original tool)
- Exit `2` → block (Claude Code uses hook's stdout as the tool result)
Never exit with `1` — that signals an error and may confuse Claude Code.

### Fallback behavior is non-negotiable
The hook MUST silently pass through (`exit 0`) when:
- `.tokenix/index.db` doesn't exist
- Index is older than `MAX_INDEX_AGE_SECS` (3600s)
- Any internal error occurs

Breaking the hook means breaking the entire Claude Code session. Fail safe always.

### File filtering
`should_index()` in `chunker.rs` has two jobs: (1) filter out ignored dirs like `node_modules`, `target`; (2) allow only files with extensions in `INDEXED_EXTS`. In `indexer.rs`, the `filter_entry` callback for directories must NOT use `should_index()` directly — it would block traversal into dirs without extensions (like `src/`). Use only the `IGNORED_DIRS` check for directories.

## Development Commands

```bash
cargo build                   # debug build
cargo build --release         # release build
cargo install --path .        # install to ~/.cargo/bin
cargo test                    # run tests
cargo clippy                  # linter

# Re-index after code changes
tokenix index . --force

# Test hook manually (pipe JSON to stdin)
echo '{"tool_name":"Read","tool_input":{"file_path":"src/main.rs"}}' | tokenix hook
echo "Exit: $?"               # should be 0 (pass) or 2 (intercepted)
```

## Adding a New Language

1. Add extensions to `INDEXED_EXTS` in `chunker.rs`
2. Add a new variant to the `Lang` enum
3. Add the extension mapping in `detect_lang()`
4. Implement a `chunk_<lang>()` function following the same pattern as `chunk_rust()`
5. Add it to the `match lang` in `chunk_file()`

The chunking pattern: walk lines, detect definition starts (fn/class/etc.), track brace depth, flush when depth returns to 0. Fall back to `chunk_by_lines()` if no symbols found.

## Adding a New Hook Tool

Currently only `Read` and `Grep` are intercepted. To add `Write` or another tool:

1. In `hook.rs`, add the tool name to the matcher check at the top of `run_hook()`
2. Implement a `handle_<tool>()` function returning `(bool, String)` — `(true, output)` to intercept
3. Add the match arm in `run_hook()`
4. Update `settings.json` matcher regex in `cmd_install_hook()` in `main.rs`

## Storage Schema

```sql
files(id, path, mtime, content_hash)           -- one row per indexed file
chunks(id, file_id, path, start_line, end_line, symbol, kind, content, token_count)
embeddings(chunk_id, embedding BLOB)           -- float32 LE, 768 dims = 3072 bytes
meta(key, value)                               -- stores 'indexed_at' timestamp
```

Hook events are logged as NDJSON to `.tokenix/hook.log` — one JSON object per line.

## Testing the Hook Integration

```bash
# Index current repo
tokenix index .

# Test Read interception (file must be >200 lines)
echo '{"tool_name":"Read","tool_input":{"file_path":"src/main.rs"}}' | tokenix hook
# Expected: exit 2, stdout = symbol outline

# Test Read pass-through (small file)
echo '{"tool_name":"Read","tool_input":{"file_path":"Cargo.toml"}}' | tokenix hook
# Expected: exit 0, no stdout

# Test Grep interception (semantic query)
echo '{"tool_name":"Grep","tool_input":{"pattern":"how does embedding generation work"}}' | tokenix hook
# Expected: exit 2, stdout = relevant chunks

# Test Grep pass-through (regex pattern)
echo '{"tool_name":"Grep","tool_input":{"pattern":"fn main"}}' | tokenix hook
# Expected: exit 0

# View savings
tokenix gain --history
```

## What Not to Change

- The `should_index` / `filter_entry` separation — changing this broke indexing once already (only `Cargo.toml` was indexed)
- Hook exit codes — must be 0 or 2, never 1
- The NDJSON format of `.tokenix/hook.log` — `gain.rs` parses it with `serde_json::from_str` per line
