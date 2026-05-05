# AGENTS.md - tokenix

This file guides AI coding agents working on the tokenix codebase.

## Project Overview

tokenix is a Rust CLI for token-efficient codebase exploration. It builds a local SQLite index with embeddings generated through Ollama, then exposes:

- `tokenix query` for semantic code search
- `tokenix read` for compact file outlines, symbol reads, and line-range reads
- `tokenix hook` for experimental AI-tool hook integration
- `tokenix gain` for estimated token-savings analytics

Use tokenix before reading large files directly.

## Required Agent Workflow

Before opening a large or unfamiliar file:

```bash
tokenix query "what you need to understand"
tokenix read <file>
```

Then narrow context with:

```bash
tokenix read <file> --symbol <name>
tokenix read <file> --lines N-M
```

Only read a full file directly when tokenix shows it is small, or when targeted symbol/line reads are not enough.

## Codebase Structure

```text
src/
|-- main.rs       CLI entry (clap), command dispatch, install/remove helpers
|-- chunker.rs    Symbol-aware heuristic chunking and outline generation
|-- embed.rs      Ollama HTTP client: get_embedding() and check_ollama()
|-- store.rs      SQLite schema, CRUD, cosine similarity, hook log NDJSON I/O
|-- indexer.rs    File walker plus incremental index pipeline
|-- query.rs      Ranking, token-budget selection, and result formatting
|-- hook.rs       Hook handler for Claude-style and Copilot-style input JSON
`-- gain.rs       Analytics from .tokenix/hook.log
```

## Tool Integration Model

### Claude Code

- Config: `PreToolUse` entry in `~/.claude/settings.json` or `.claude/settings.json`
- Input: stdin JSON like `{"tool_name":"Read","tool_input":{"file_path":"src/main.rs"}}`
- Current behavior: `tokenix hook` prints a JSON deny decision with compact context in the reason, then exits `0`

### GitHub Copilot

- Instructions: `.github/copilot-instructions.md`
- Hook config: `.github/hooks/hooks.json`
- Input: current Copilot hook schema uses stdin JSON with `toolName` and `toolArgs`
- Current behavior: tokenix normalizes `view`/`read` to `Read` and can block large reads with compact context

### OpenAI Codex CLI

- No native hook protocol is assumed.
- Prefer project `AGENTS.md` and direct calls to `tokenix read` / `tokenix query`.
- `tokenix install-hook --tool codex` writes compatibility helpers under `~/.codex/`.

## Key Design Decisions

### Cosine similarity in Rust

Embeddings are stored as raw float32 little-endian blobs in SQLite. Query-time similarity is computed in Rust. This avoids requiring sqlite-vec or a separate vector database.

### Token counting approximation

`count_tokens()` in `chunker.rs` uses `(len + 3) / 4`. It is a fast approximation for budget decisions and analytics.

### Hook fallback

`run_hook()` must fail open. It should silently allow the original tool when:

- `.tokenix/index.db` does not exist
- the index is older than `MAX_INDEX_AGE_SECS` (3600 seconds)
- the tool input cannot be parsed
- an internal lookup/search fails

Breaking the assistant session is worse than missing a token-saving opportunity.

### File filtering in indexer

Keep the `should_index` / `filter_entry` separation intact. Directory traversal should only check ignored directories; file-extension filtering belongs to files. Calling `should_index()` on directories can stop traversal through directories such as `src/`.

### Cross-platform paths

`tokenix_bin_path()` normalizes executable paths to forward slashes for shell/JSON config strings. Preserve this behavior for Windows compatibility.

## Development Commands

```bash
cargo build
cargo build --release
cargo install --path .
cargo test
cargo fmt
```

## Manual Integration Tests

```bash
tokenix index .

# Claude-style input
echo '{"tool_name":"Read","tool_input":{"file_path":"src/main.rs"}}' | tokenix hook

# Copilot-style input
echo '{"toolName":"view","toolArgs":"{\"path\":\"src/main.rs\"}"}' | tokenix hook

tokenix query "how does embedding generation work"
tokenix gain --history
```

## Adding a New Language

1. Add extensions to `INDEXED_EXTS` in `chunker.rs`.
2. Add a variant to the `Lang` enum and update `detect_lang()`.
3. Implement `chunk_<lang>(lines, path) -> Vec<Chunk>`.
4. Add a match arm in `chunk_file()`.

Follow the existing pattern: detect definition starts, track brace or indentation depth, flush when the block closes, and fall back to `chunk_by_lines()` when no symbols are found.

## Adding a New Tool Integration

1. Add a variant to the `Tool` enum in `main.rs`.
2. Implement `install_<tool>()` and `remove_<tool>()`.
3. Add match arms in `cmd_install_hook()` and `cmd_remove_hook()`.
4. Update `hook.rs` only when the tool has a real hook protocol.
5. Document the integration in `README.md`.

## Storage Schema

```sql
files(id, path TEXT UNIQUE, mtime REAL, content_hash TEXT)
chunks(id, file_id, path, start_line, end_line, symbol, kind, content, token_count)
embeddings(chunk_id PRIMARY KEY, embedding BLOB)
meta(key PRIMARY KEY, value)
```

Hook log: `.tokenix/hook.log` is NDJSON, one `HookEvent` per line.

## What Not to Change Casually

- Do not collapse `should_index` and `filter_entry`.
- Do not make hooks exit with errors for recoverable problems.
- Do not change `.tokenix/hook.log` away from NDJSON without updating `gain.rs`.
- Do not reintroduce backslashes in generated config command paths.
