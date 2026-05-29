# AGENTS.md — tokenix

## Project

**tokenix** — Rust CLI for token-efficient codebase exploration. Builds a local SQLite index with in-process embeddings (fastembed/ONNX), then exposes semantic search, compact file reads, and AI-tool hook integration.

Stack: Rust · SQLite · fastembed (ONNX, in-process) · Claude Code `PreToolUse` hook · background daemon (TCP)

Reduces token usage 60–90% by intercepting large reads and replacing them with focused context.

## Build & Install

```bash
cargo build
cargo build --release
cargo install --path .   # installs to ~/.cargo/bin
tokenix --help
```

## Key Files

| File | Purpose |
|---|---|
| `src/main.rs` | CLI entry (clap), command dispatch, `install-hook`/`remove-hook` helpers |
| `src/chunker.rs` | Symbol-aware heuristic chunking, `generate_outline()`, token counting |
| `src/embed.rs` | fastembed ONNX — `embed_documents()`, `embed_query()`. Model cached in `OnceCell` |
| `src/store.rs` | SQLite schema, CRUD, cosine similarity search, hook log I/O |
| `src/indexer.rs` | File walk + incremental index pipeline. Embeds in batches of 512 |
| `src/query.rs` | Hybrid semantic/lexical ranking + result formatting |
| `src/hook.rs` | `run_hook()` — called by PreToolUse hook. Tries daemon first for Grep |
| `src/daemon.rs` | Background TCP server (port 47392). Holds model + embedding cache (LRU, max 3 projects, content cap 1000). Bounded to 4 handler threads |
| `src/compress.rs` | `PostToolUse` compression: ANSI strip, emoji removal, blank-line collapse, repeat grouping, JSON compaction, cargo/git-log heuristics |
| `src/filters.rs` | `FilterDef` (TOML schema), active filter listing, `load_user_filters()`, `load_bundled_filters()` (rust-embed), `apply_filter()` |
| `src/cmd_filter.rs` | `tokenix filter list/active/generate` subcommands |
| `src/gain.rs` | `compute_gain()`, `GainStats`, `MODELS` pricing table (Anthropic/OpenAI/Google) |
| `assets/filters/` | 59 RTK-compatible TOML filters embedded via `rust-embed`. User filters in `~/.tokenix/filters/` take priority |

## SQLite Schema

```sql
files(id, path TEXT UNIQUE, mtime REAL, content_hash TEXT)
chunks(id, file_id, path, start_line, end_line, symbol, kind, content, token_count)
chunks_fts(rowid, content, symbol, path)   -- FTS5 virtual table for keyword search
embeddings(chunk_id PK, embedding BLOB)    -- float32 LE, 768 dims
meta(key PK, value)                        -- 'indexed_at', git fingerprint
```

`meta` stores `indexed_at` and a Git fingerprint (worktree root + branch + HEAD). Hooks and `--if-stale` treat a different fingerprint as stale so branch switches don't reuse stale context.

Hook log: `.tokenix/hook.log` — NDJSON, one `HookEvent` per line.

## Intercept Logic

```
Read tool:
  file < 200 lines OR offset/limit set → exit 0 (pass through)
  file ≥ 200 lines, no offset/limit   → return outline, exit 2 (intercept)

Grep tool:
  pattern < 3 words → exit 0 (pass — likely a regex/symbol search)
  pattern ≥ 3 words → return semantic results, exit 2 (intercept)

Index missing or >1h old → always exit 0 regardless of tool
```

## Critical Rules

**Never lose content.** The chunker must store 100% of every indexed file. Generic files (.md, .txt, .yaml, .json) use `clean_generic_text()` — full content with formatting stripped. Truncated previews are forbidden. Only code files use symbol-based outlines stored in full in SQLite.

**Never break hook fallback.** `run_hook()` must always `exit(0)` on any error — missing index, stale index, parse failures, embed errors. Breaking Claude Code sessions is worse than missing a token-saving opportunity.

**Hook exit codes:** `0` = pass through (original tool runs) · `2` = block tool (hook stderr becomes Claude's context). Never exit `1`.

**Daemon is optional.** If `tokenix serve` is not running, `handle_grep()` auto-starts it and retries once (800ms wait). If autostart fails, falls back to direct in-process embed.

**Directory filtering in indexer:** `filter_entry` for directories uses ONLY `IGNORED_DIRS`. Do NOT call `should_index()` on directories — it returns false for dirs without extensions and breaks traversal. Keep `should_index` / `filter_entry` separation intact.

**Cross-platform paths:** `tokenix_bin_path()` normalizes to forward slashes for shell/JSON config strings. Preserve for Windows compatibility.

**Hook log format:** Do not change `.tokenix/hook.log` away from NDJSON without updating `gain.rs`.

**Token count is approximate.** `count_tokens()` = `(len + 3) / 4`. Intentional — no tiktoken dep.

## Daemon

```bash
tokenix serve            # start daemon (blocks; use & or detached)
tokenix serve --port 9999
tokenix stop             # stop daemon (reads ~/.tokenix/daemon.pid)

# Health check
echo '{"type":"health"}' | nc 127.0.0.1 47392
# → {"ok":true,"cached_projects":1,"chunks":197}
```

Warm Grep calls via daemon: ~80ms vs ~430ms cold in-process. Daemon auto-starts on first Grep hook call.

**Resource limits (prevents freeze under parallel hooks):**
- Max **4 concurrent handler threads** — unbounded spawning was the primary Windows freeze trigger
- **Spawn lock** (`daemon.pid.spawning`) + PID liveness check — prevents N parallel hooks from each spawning a separate 130 MB daemon process
- **Content cache capped at 1000 entries** per project

## Output Filters

`PostToolUse` compression flows through (in order):
1. User TOML filters (`~/.tokenix/filters/*.toml`) — highest priority
2. Bundled TOML filters (`assets/filters/*.toml`, rust-embed)
3. Built-in heuristics in `compress.rs` — cargo, git-log, generic head/tail

`apply_filter()` pipeline: `match_output` short-circuit → `strip_ansi` → `strip_lines_matching` → `keep_lines_matching` → `head/tail/max_lines` → `truncate_lines_at` → `on_empty`.

```toml
[filters.my-cmd]
match_command  = "^my-cmd\\b"
strip_ansi     = true
strip_lines_matching  = ["^\\s+Downloading"]
match_output   = [{ pattern = "Success", message = "ok" }]
max_lines      = 30
on_empty       = "my-cmd: ok"
```

## Common Tasks

**Add a language:** `chunker.rs` — add extension to `INDEXED_EXTS`, add `Lang` variant, map in `detect_lang()`, implement `chunk_<lang>()` following `chunk_rust()` pattern. Do NOT add to `INDEXED_EXTS` without a symbol-aware chunker.

**Add a bundled filter:** create `assets/filters/<slug>.toml`. Rebuild — rust-embed includes it automatically.

**Change intercept threshold:** `hook.rs` constants — `MAX_INDEX_AGE_SECS`, `MIN_LINES_FOR_OUTLINE`, `MIN_QUERY_WORDS`.

**Extend hook to a new tool:**
1. Add variant to `Tool` enum in `main.rs`
2. Implement `install_<tool>()` and `remove_<tool>()`
3. Add match arms in `cmd_install_hook()` and `cmd_remove_hook()`
4. Update `hook.rs` only if the tool has a real hook protocol
5. Document in `README.md`

**Change token budget:** `query.rs` — `DEFAULT_BUDGET` constant, or pass `--budget` flag.

**Update pricing table:** `gain.rs` — edit `MODELS` constant. Fields: `name`, `input_per_1m` (USD), `reference` (marks ★ model).

## Testing the Hook

```bash
tokenix index .

# Should intercept (exit 2) — large file
echo '{"tool_name":"Read","tool_input":{"file_path":"src/main.rs"}}' | tokenix hook; echo $?

# Should pass through (exit 0) — small file
echo '{"tool_name":"Read","tool_input":{"file_path":"Cargo.toml"}}' | tokenix hook; echo $?

# Should intercept (exit 2) — semantic query (auto-starts daemon)
echo '{"tool_name":"Grep","tool_input":{"pattern":"how does embedding work"}}' | tokenix hook; echo $?

# Copilot-style input
echo '{"toolName":"view","toolArgs":"{\"path\":\"src/main.rs\"}"}' | tokenix hook; echo $?

# PostToolUse compression — bundled filter short-circuit
echo '{"tool_name":"Bash","tool_input":{"command":"uv sync"},"tool_response":{"output":"Resolved 42 packages in 123ms\nAudited 42 packages in 0.05ms\n"}}' | tokenix hook-post; echo $?

tokenix gain --history
```

## Claude Code Integration Setup

After `cargo install --path .`, configure Claude Code globally:

### 1. Hooks (`~/.claude/settings.json`)

Add to the `hooks` key — merging with any existing entries:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Read|Grep|Glob",
        "hooks": [{ "type": "command", "command": "tokenix hook" }]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "Bash",
        "hooks": [{ "type": "command", "command": "tokenix hook-post" }]
      }
    ]
  }
}
```

`PreToolUse` intercepts large reads and semantic Grep queries (see intercept logic above).
`PostToolUse` compresses Bash output via bundled filters + heuristics.

### 2. Behavioral instruction (`~/.claude/CLAUDE.md`)

Add a section so Claude prefers tokenix over raw Grep/Glob/Read for codebase searches:

```markdown
## Tokenix Indexed Search (Token Economy)
- `tokenix` is indexed and available in PATH.
- For any codebase search, PREFER tokenix over Grep/Glob/Read:
  - Symbol by name: `tokenix symbols <name>`
  - Semantic query: `tokenix query "<question>"`
  - Callers of a function: `tokenix callers <symbol>`
  - Callees: `tokenix callees <symbol>`
  - Impact graph: `tokenix impact <symbol>`
  - Focused context for a task: `tokenix context "<task description>"`
  - Explore related: `tokenix explore <symbol>`
- Fall back to Grep/Glob/Read only when tokenix returns no results or for exact literal matches.
```

### 3. Index the project

```bash
cd <project>
tokenix index .
tokenix stats      # verify file/chunk count
```

The daemon auto-starts on first Grep hook call. Run `tokenix serve` manually only to pre-warm it.

## Tool Integration Model

### Claude Code
- Config: `PreToolUse`/`PostToolUse` in `~/.claude/settings.json` (see setup above)
- Input: `{"tool_name":"Read","tool_input":{"file_path":"src/main.rs"}}`

### GitHub Copilot
- Config: `.github/copilot-instructions.md` + `.github/hooks/hooks.json`
- Input: `{"toolName":"view","toolArgs":"{\"path\":\"src/main.rs\"}"}`
- tokenix normalizes `view`/`read` → `Read`

### OpenAI Codex CLI
- No native hook protocol. Prefer project `AGENTS.md` and direct `tokenix read`/`tokenix query` calls.
- `tokenix install-hook --tool codex` writes helpers under `~/.codex/`

## Agent Workflow (when working on this repo)

Before opening a large or unfamiliar file:

```bash
tokenix query "what you need to understand"
tokenix read <file>
```

Narrow context with:

```bash
tokenix read <file> --symbol <name>
tokenix read <file> --lines N-M
```

Only read a full file directly when tokenix shows it is small.

## Release

Releases are automated via GitHub Actions (`.github/workflows/release.yml`). Pushing to `main` auto-creates a version tag and GitHub Release with pre-built binaries for Linux, macOS, and Windows.

To trigger manually: push a commit to `main` — the workflow reads version from `Cargo.toml`.
