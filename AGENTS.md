# AGENTS.md — tokenix

This file provides guidance for AI coding agents (Claude Code, Copilot, Cursor, etc.) working on the tokenix codebase.

## Project Overview

tokenix is a Rust CLI that intercepts AI coding assistant file reads and replaces them with compact semantic context, reducing token usage by 60–90%. It supports **Claude Code**, **GitHub Copilot**, and **OpenAI Codex CLI** on **Windows**, **macOS**, and **Linux**.

## Codebase Structure

```
src/
├── main.rs       CLI entry (clap). All cmd_* functions + install-hook/remove-hook for all tools.
├── chunker.rs    AST-based code chunking per language + generate_outline(). Token approximation.
├── embed.rs      Ollama HTTP client — get_embedding() and check_ollama().
├── store.rs      SQLite layer — schema, CRUD, cosine similarity search, hook log NDJSON I/O.
├── indexer.rs    File walker (WalkDir + .gitignore via pathspec) + incremental index pipeline.
├── query.rs      Cosine similarity ranking + token-budget selection + result formatting.
├── hook.rs       PreToolUse handler — stdin JSON (Claude Code) or env vars (Copilot agent mode).
└── gain.rs       Analytics from .tokenix/hook.log.
```

## Tool Integration Model

### Claude Code
- Hook: `PreToolUse` entry in `~/.claude/settings.json` (global) or `.claude/settings.json` (local)
- Protocol: Claude Code pipes JSON to hook stdin; hook exits `0` (pass) or `2` (intercept)
- Input format: `{"tool_name": "Read", "tool_input": {"file_path": "..."}}`

### GitHub Copilot
- Hook: `.github/hooks/hooks.json` — `preToolUse` with `bash` and `powershell` command fields
- Instructions: `.github/copilot-instructions.md` — injected into every Copilot chat (VS Code)
- Protocol: Copilot may set `COPILOT_TOOL_NAME` / `COPILOT_TOOL_INPUT` env vars (agent mode)
- Both files must be committed to the repo; they apply to all contributors

### OpenAI Codex CLI
- Instructions: `~/.codex/instructions.md` — injected into every Codex session
- Shell helpers: `~/.codex/tokenix-init.sh` (bash/zsh) and `tokenix-init.ps1` (PowerShell)
- No native hook protocol — relies on LLM following instructions to call `tokenix read` / `tokenix query`

## Key Design Decisions

### Cosine similarity in Rust (no sqlite-vec)
Embeddings are stored as raw `BLOB` (float32 LE bytes) in SQLite. At query time, all embeddings are loaded into memory and cosine similarity is computed in Rust. No sqlite-vec extension needed — avoids native compilation issues across platforms.

### Token counting approximation
`count_tokens()` in `chunker.rs` uses `(len + 3) / 4`. Fast, no tiktoken dep. Accurate enough for budget decisions.

### Hook exit codes (Claude Code + Copilot agent mode)
- Exit `0` → pass through (AI tool runs original tool)
- Exit `2` → block (hook's stdout replaces tool result)
- Never exit `1` — that signals an error and may break the session

### Fallback is non-negotiable
`run_hook()` MUST silently `exit(0)` when:
- `.tokenix/index.db` doesn't exist
- Index is older than `MAX_INDEX_AGE_SECS` (3600s)
- Any internal error occurs

Breaking the hook breaks the AI coding session. Fail safe always.

### Hook input source detection (`hook.rs`)
```
1. Check env vars: COPILOT_TOOL_NAME / COPILOT_TOOL_INPUT  →  Copilot agent mode
2. Fall back to stdin JSON parsing                          →  Claude Code / Codex
3. If neither, exit 0 silently
```

### File filtering in indexer (critical — do not change)
`filter_entry` for directories checks ONLY `IGNORED_DIRS`. Do NOT call `should_index()` on directories — it returns false for dirs without file extensions (like `src/`) and breaks traversal. This was a real bug.

### Cross-platform paths
- `tokenix_bin_path()` in `main.rs` normalizes the exe path to forward slashes for config files — works in bash, PowerShell, and JSON strings on all platforms
- `dirs::home_dir()` resolves `~` correctly on all platforms (`C:\Users\<user>` on Windows, `/home/<user>` on Linux/macOS)
- `~/.claude/settings.json` path is the same on all platforms (Claude Code uses `dirs::home_dir()` internally)

## Development Commands

```bash
cargo build                   # debug build
cargo build --release         # optimized release build
cargo install --path .        # install to ~/.cargo/bin

# Test multi-tool install
tokenix install-hook --tool all
tokenix install-hook --tool claude-code --local
tokenix install-hook --tool copilot     # writes .github/ files
tokenix install-hook --tool codex       # writes ~/.codex/ files

# Test hook — Claude Code format (stdin JSON)
echo '{"tool_name":"Read","tool_input":{"file_path":"src/main.rs"}}' | tokenix hook
echo "Exit: $?"   # 2 = intercepted, 0 = pass through

# Test hook — Copilot format (env vars)
COPILOT_TOOL_NAME=Read COPILOT_TOOL_INPUT='{"file_path":"src/main.rs"}' tokenix hook
echo "Exit: $?"

# Check savings
tokenix gain --history
```

## Adding a New Language

1. Add extensions to `INDEXED_EXTS` in `chunker.rs`
2. Add variant to `Lang` enum, add extension mapping to `detect_lang()`
3. Implement `chunk_<lang>(lines, path)` → `Vec<Chunk>` following `chunk_rust()` pattern
4. Add match arm in `chunk_file()`

The chunking pattern: scan for definition starts → track brace/indent depth → flush when block closes → fall back to `chunk_by_lines()` if no symbols found.

## Adding a New Tool Integration

1. Add variant to `Tool` enum in `main.rs`
2. Implement `install_<tool>()` and `remove_<tool>()` functions
3. Add match arm in `cmd_install_hook()` and `cmd_remove_hook()`
4. Update `hook.rs` if the tool uses a different input format (env vars, CLI args, etc.)
5. Document in README.md under "Setup by Tool"

## Storage Schema

```sql
files(id, path TEXT UNIQUE, mtime REAL, content_hash TEXT)
chunks(id, file_id, path, start_line, end_line, symbol, kind, content, token_count)
embeddings(chunk_id PK, embedding BLOB)   -- float32 LE, 768 dims = 3072 bytes/chunk
meta(key PK, value)                       -- 'indexed_at' = unix timestamp as string
```

Hook log: `.tokenix/hook.log` — NDJSON, one `HookEvent` struct per line.

## Cross-Platform Notes

- **Windows paths in config files**: always use forward slashes (handled by `tokenix_bin_path()`)
- **Binary extension**: `tokenix` on Unix, `tokenix.exe` on Windows — `current_exe()` returns the correct name
- **Copilot hooks.json**: has separate `bash` and `powershell` command fields for cross-platform
- **Shell helpers**: separate `.sh` and `.ps1` files for different shells

## Testing the Full Integration

```bash
# 1. Index
tokenix index .

# 2. Claude Code hook — should intercept large file (exit 2)
echo '{"tool_name":"Read","tool_input":{"file_path":"src/main.rs"}}' | tokenix hook; echo "Exit: $?"

# 3. Claude Code hook — should pass small file (exit 0)
echo '{"tool_name":"Read","tool_input":{"file_path":"Cargo.toml"}}' | tokenix hook; echo "Exit: $?"

# 4. Copilot hook — env var format (exit 2)
COPILOT_TOOL_NAME=Read COPILOT_TOOL_INPUT='{"file_path":"src/main.rs"}' tokenix hook; echo "Exit: $?"

# 5. Semantic Grep interception (exit 2)
echo '{"tool_name":"Grep","tool_input":{"pattern":"how does embedding generation work"}}' | tokenix hook; echo "Exit: $?"

# 6. Short Grep — should pass (exit 0)
echo '{"tool_name":"Grep","tool_input":{"pattern":"fn main"}}' | tokenix hook; echo "Exit: $?"

# 7. Analytics
tokenix gain --history
```

## What Not to Change

- **`should_index` / `filter_entry` separation** — changing this broke indexing once (only Cargo.toml was indexed)
- **Hook exit codes** — must be 0 or 2, never 1
- **NDJSON format of `.tokenix/hook.log`** — `gain.rs` parses it line-by-line with `serde_json`
- **Forward slashes in config paths** — Windows paths with backslashes break bash commands inside JSON
