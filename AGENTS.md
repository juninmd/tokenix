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
| `src/main.rs` | CLI entry (clap), command dispatch, `install-hook`/`remove-hook` helpers (including Antigravity global install/uninstall through `agy plugin`, repo-local OpenCode native `opencode.json` MCP registration/removal, and local `.agents/plugins/tokenix`), `install-binary` (copies the running exe to `global_bin_dir()` — `%LOCALAPPDATA%\tokenix\bin` / `~/.local/bin` — and persists the Windows user PATH via PowerShell `[Environment]::SetEnvironmentVariable`, never `setx`). `banner()` = neon "tokenix" wordmark + tagline; `help_catalog()` = audience-grouped command list (AI agent vs human) + examples, wired via custom `HELP_TEMPLATE` (`before_help`/`after_help`); bare `tokenix` prints this help |
| `src/chunker.rs` | Symbol-aware heuristic chunking, `generate_outline()`, token counting. Tree-sitter for Rust/Python/TS/JS/Go/C++; `chunk_by_symbol_lines()` line-scanning chunkers for grammar-less languages — VB6/VBA (`Sub`/`Function`/`Property`/`Attribute VB_Name`) and SQL (`CREATE [OR REPLACE] <object>`) |
| `src/embed.rs` | fastembed ONNX — `embed_documents()`, `embed_query()`. Model **registry** (`MODELS`, `spec_for`) + thread-local active model (`set_active_model`/`active_model_id`) + per-id loaded-model cache. Per-model query/doc prefixes; query cache keyed by model |
| `src/store.rs` | SQLite schema, CRUD, cosine similarity search (int8-quantized vectors + legacy f32 fallback, `quantize_q8`/`backfill_quantized_embeddings`), import graph (`graph_imports`, `file_imports`), hook log I/O + 5 MB rotation, PID index lock, branch-aware DB paths |
| `src/indexer.rs` | File walk + incremental index pipeline. Runs at below-normal OS priority (`lower_process_priority()`, opt-out `--no-low-priority`/`TOKENIX_FOREGROUND`). `decode_text()` handles UTF-16 BOMs (SSMS-saved `.sql`) and skips binary files (NUL in first 8 KiB). Embeds in batches (default 16) with a progress bar; each batch commits to the embedding cache so a killed run resumes via cache hits |
| `src/query.rs` | Hybrid semantic/lexical ranking (FTS5 + BM25 + RRF), strict `context` modes, budget enforcement, cross-project search |
| `src/pack.rs` | `tokenix pack` — budgeted repo map + focused context, changed-file packs, token maps, and safety report |
| `src/graph.rs` | Symbol graph with PageRank, cycle detection (Tarjan's SCC, homonym-filtered, `path:line`-annotated), tree-sitter references, incremental repair (`update_symbol_graph_incremental` — FTS-narrowed inbound-edge restore; `rebuild-graph` = full escape hatch), file-level import graph (`rebuild_import_graph`, per-language import extraction + path resolution), HTML + Mermaid export |
| `src/artifacts.rs` | Context artifacts — index non-code files (schemas, API specs, docs) via `.tokenix/artifacts.json` |
| `src/hook.rs` | `run_hook()` — called by PreToolUse hook. Tries daemon first for Grep. Thresholds (Read 200 lines / Grep 3 words) overridable via `[hook]` in `.tokenix.toml` (`read_min_lines`, `grep_min_words`) |
| `src/daemon.rs` | Background TCP server (port 47392). Holds model + int8-quantized embedding cache (LRU, max 3 projects, content cap 1000). Bounded to 4 handler threads. Protocol: `search`/`health`/`status`; CLI `tokenix daemon status\|stop\|restart` |
| `src/compress.rs` | Legacy `PostToolUse` compatibility compression + `tokenix run` command-output compression: ANSI strip, emoji removal, blank-line collapse, repeat grouping, JSON compaction, cargo/git-log heuristics. `tokenix run` only applies command-specific filters to stderr when `filter_stderr=true`; otherwise stderr uses safe generic compression so errors are not turned into success sentinels |
| `src/filters.rs` | `FilterDef` (TOML schema), active filter listing, `load_user_filters()`, `load_bundled_filters()` (rust-embed), `apply_filter()`. `find_filter()` matches via `derive_command_candidates()`, which unwraps shell runners, strips `cd`/env prefixes, and `split_on_operators()` splits compound commands quote-aware on `&&`/`\|\|`/`;`/`\|` so anchored `match_command` patterns match a base command in any segment/position |
| `src/cmd_filter.rs` | `tokenix filter list/active/generate` + `filter record start/stop/status` subcommands. `generate` prefers `recordings::read_samples` over a re-run, invokes a detected AI CLI, and saves to `~/.tokenix/filters/`; reused by the TUI Studio tab as a foreground drop-out |
| `src/tui.rs` | Interactive ratatui shell shown by a bare `tokenix` / `tokenix filter` in a TTY (else falls back to help / `filter list`). Tab bar (`←`/`→`): **Stats** dashboard (wordmark + version + hook status + index summary, with selectable Index / Install hooks / Install binary actions — Index runs in the foreground with live progress, the two install actions confirm before writing; Install binary self-execs `tokenix install-binary`), **Filters** (3-pane groups · filters · live `apply_filter` input→output preview with a `chunker::count_tokens` gauge line showing `X → Y tokens · % saved` between the panes), **Studio** (surfaces the record→preview→generate filter loop: `r`/`s` arm/stop a `recordings::start`/`stop` session, left column lists `recordings::summary` captures + saved `~/.tokenix/filters/*.toml`, right pane previews a `recordings::read_samples` head with a live `apply_filter` before→after `chunker::count_tokens` delta when an active filter matches the base command; `g` sets `request_generate` to run `cmd_filter::cmd_filter_generate` as a foreground drop-out — same pattern as Index — then resumes the TUI; `x` deletes a saved filter with confirm; `Tab` switches pane), **Gain** (native colored render of `gain::compute_gain`: tokens-saved headline with ≈USD at the ★ reference model's input rate, savings-by-source split — semantic index vs command filters — and numbered by command / by project tables with share %, toggles `c`/`a`), **Doctor**/**Tokenmap** (self-exec captured output), **Secrets** (background-threaded `secrets_scan::scan_findings` with spinner; dedup by distinct value + count; `v` reveal, `c` copy raw value to system clipboard via `clip`/`pbcopy`/`wl-copy`/`xclip`/`xsel`, `x` write `[REDACTED]`), **Egress** (background-threaded `egress_scan::scan_findings` with the same 3-pane pattern as Secrets: groups · destinations · occurrence detail; `s` cycles host/rule/agent/file grouping; `r` rescans; host reputation colors: green safe, red dangerous, yellow unknown). Both Secrets and Egress open scoped to the current repo (cwd) and `g` toggles a global all-repos view; scoping filters the raw scan by each finding's attributed `repo` (`is_local` matches exact `cwd` paths plus Claude `~slug:`/Gemini `~dir:` fallback markers against the project root) |
| `src/ui.rs` | Shared terminal-UI vocabulary for human-facing CLI output (`box_header`, `bar`, `section`/`kv`, `format_num`, `table` via `tabled`); LLM/JSON output deliberately does not route through it |
| `src/gain.rs` | `compute_gain()`/`compute_global_gain()`, `GainStats` (incl. `index_saved`/`filter_saved` source split: empty `command` = semantic-index intercept, non-empty = command filter; pre-phase Bash/PowerShell rewrite markers are excluded from `filter_calls`), `MODELS` pricing table (Anthropic/OpenAI/Google). Grep semantic intercepts are logged as neutral usage, not claimed savings, because native grep output is not measured before interception |
| `src/mcp.rs` | MCP server. `--profile full` exposes all tools; `--profile slim` exposes context/search/call meta-tools for progressive discovery |
| `src/mcp_audit.rs` | `tokenix prompt-audit` / `session-audit` — per-agent MCP config discovery (Claude, Codex, Copilot, OpenCode, Antigravity) + minimal synchronous MCP stdio client (`initialize`/`tools/list`) + token scoring/report |
| `src/secrets_scan.rs` | `tokenix scan-secrets` — gitleaks-style credential scan of Claude/Gemini/Copilot/Antigravity conversation transcripts under `~`; rules loaded from TOML (`assets/secret-rules/` bundled via `rust-embed`, extended by `<repo>/` then `~/.tokenix/secret-rules/*.toml`, later `id` wins), backtracking-free regex + entropy-gated generic rule. Each finding is attributed to its repo + git branch via the transcript line's `cwd`/`gitBranch` (Claude), falling back to the project dir slug. Report supports `--filter` (substring), `--group <value\|rule\|agent\|file\|repo>`, `--reveal` (raw values, default redacted), `--json`; exit 1 on hits. `scan_findings()` returns structured `ScanFinding`s (raw + redacted) for the TUI; `redact_in_files()` rewrites `[REDACTED]` over a value in text files (SQLite DBs skipped) |
| `src/egress_scan.rs` | `tokenix egress-audit` — scans Claude/Gemini/Copilot/Antigravity conversation transcripts for external DNS/IP destinations; bundled TOML rules live under `assets/egress-rules/`, local safe hosts are loaded from `~/.tokenix/safe-hosts.toml`, and local blocklist hosts from `~/.tokenix/dangerous-hosts.toml` (`dangerous`, `blocklist`, or `hosts` arrays); report supports `--filter`, `--group <host\|rule\|agent\|file>`, `--safe`, and `--json`. `scan_findings()` returns structured `EgressFinding`s for the TUI |
| `assets/filters/` | 244 TOML output filters embedded via `rust-embed`, each homologated with ≥2 golden `[[tests]]` cases (realistic success + failure-path inputs; the failure case must prove errors are never masked). 526 cases run through the real `apply_filter` pipeline in `bundled_filters_pass_embedded_golden_tests`. User filters in `~/.tokenix/filters/` take priority |

## SQLite Schema

```sql
files(id, path TEXT UNIQUE, mtime REAL, content_hash TEXT)
chunks(id, file_id, path, start_line, end_line, symbol, kind, content, token_count)
chunks_fts(rowid, content, symbol, path)   -- FTS5 virtual table for keyword search
embeddings(chunk_id PK, embedding BLOB, scale REAL)
  -- scale NOT NULL → int8-quantized vector (1 byte/dim); scale NULL → legacy
  -- float32 LE blob. Search branches per row; the scale cancels out of the
  -- cosine, so q8 search needs only the raw bytes. Legacy rows are migrated
  -- (re-encode only) by backfill_quantized_embeddings() at index time + VACUUM.
embedding_cache(content_hash PK, embedding BLOB, updated_at)  -- stays float32 so
  -- model switches and quantization changes never force a re-embed
graph_nodes(chunk_id PK, file_id, path, name, kind, start_line, end_line, rank)
graph_edges(id, caller_chunk_id, callee_chunk_id, reference, edge_kind)
graph_imports(id, source_path, target, resolved_path, kind, line)
  -- file-level import edges; resolved_path NULL = external dependency
meta(key PK, value)                        -- 'indexed_at', git fingerprint
```

`meta` stores `indexed_at` and a Git fingerprint (worktree root + branch + HEAD). Hooks and `--if-stale` treat a different fingerprint as stale so branch switches don't reuse stale context.

Query paths open old DBs without running migrations — SELECTs must degrade when the `scale` column is missing (`embeddings_have_scale()` probe selects `NULL` instead).

Hook log: `~/.tokenix/<project-id>.log` — NDJSON, one `HookEvent` per line. Rotates at 5 MB to `<project-id>.log.1` (one generation kept); `read_hook_log()` reads both. Fallback when the home dir is unavailable is repo-local `.tokenix/hook.log`.

## Intercept Logic

```
Read tool:
  file < 200 lines OR offset/limit set → exit 0 (pass through)
  file ≥ 200 lines, no offset/limit   → return outline, exit 2 (intercept)

Grep tool:
  pattern < 3 words → exit 0 (pass — likely a regex/symbol search)
  pattern ≥ 3 words → return semantic results, exit 2 (intercept); gain records this as neutral usage, not saved tokens

Bash / PowerShell tools:
  command matches a bundled/user filter → rewrite to `tokenix run` (PowerShell
  uses `& 'exe' run --shell pwsh '<cmd>'`, re-executed under pwsh with UTF-8)
  otherwise → exit 0 (pass)

Index missing or >1h old → always exit 0 regardless of tool
```

Matcher (installer): `^(Read|Grep|Bash|PowerShell|grep_search|run_in_terminal)$`.
Claude Code's dedicated `PowerShell` tool (exact name) takes the pwsh path; the
generic lowercase `powershell` from Copilot/Antigravity stays on the bash path.

`get_effective_command` normalizes a command before matching so filters anchored
on the bare tool still hit: it strips shell wrappers, `cd`/env prefixes, package
runners (`uv run`, `python -m`, `npx`, `bunx`, `pnpm exec/dlx`, `yarn dlx`,
`bun x`, `deno run/task`), and tool-global options (`git -C`, `kubectl -n`,
`docker -H`, `cargo +tc`). Verified against Codex/Antigravity histories where
`uv run pytest`, `python -m ruff`, `bunx biome` were bypassing their filters.

Both thresholds are per-project tunable via `.tokenix.toml`:

```toml
[hook]
read_min_lines = 120   # default 200
grep_min_words = 3     # default 3
```

## Critical Rules

**Never lose content.** The chunker must store 100% of every indexed file. Generic files (.md, .txt, .yaml, .json) use `clean_generic_text()` — full content with formatting stripped. Truncated previews are forbidden. Only code files use symbol-based outlines stored in full in SQLite.

**Never break hook fallback.** `run_hook()` must always `exit(0)` on any error — missing index, stale index, parse failures, embed errors. Breaking Claude Code sessions is worse than missing a token-saving opportunity.

**Hook exit codes:** `0` = pass through (original tool runs) · `2` = block tool (hook stderr becomes Claude's context). Never exit `1`.

**Daemon is optional.** If `tokenix serve` is not running, `handle_grep()` auto-starts it and retries once (800ms wait). If autostart fails, falls back to direct in-process embed.

**Directory filtering in indexer:** `filter_entry` for directories uses ONLY `IGNORED_DIRS`. Do NOT call `should_index()` on directories — it returns false for dirs without extensions and breaks traversal. Keep `should_index` / `filter_entry` separation intact.

**Cross-platform paths:** `tokenix_bin_path()` normalizes to forward slashes for shell/JSON config strings. Preserve for Windows compatibility.

**Hook log format:** Do not change `~/.tokenix/<project-id>.log` away from NDJSON without updating `gain.rs`.

**Token count is approximate.** `count_tokens()` = `(len + 3) / 4`. Intentional — no tiktoken dep.

**Keep docs in sync.** Every new or changed user-facing feature MUST update both `README.md` (Features table, Commands Reference, Usage, Architecture) and `AGENTS.md` (Key Files + relevant section) in the same change.

## Daemon

```bash
tokenix serve            # start daemon (blocks; use & or detached)
tokenix serve --port 9999
tokenix stop             # stop daemon (reads ~/.tokenix/daemon.pid)
tokenix daemon status    # pid, port, uptime, model, cached projects + RAM
tokenix daemon restart   # stop (if running) + detached respawn

# Health check
echo '{"type":"health"}' | nc 127.0.0.1 47392
# → {"ok":true,"cached_projects":1,"chunks":197}
# Status over the same socket: {"type":"status"}
```

Warm Grep calls via daemon: ~80ms vs ~430ms cold in-process. Daemon auto-starts on first Grep hook call.

**Resource limits (prevents freeze under parallel hooks):**
- Max **4 concurrent handler threads** — unbounded spawning was the primary Windows freeze trigger
- **Spawn lock** (`daemon.pid.spawning`) + PID liveness check — prevents N parallel hooks from each spawning a separate 130 MB daemon process
- **Content cache capped at 1000 entries** per project

## Output Filters

Legacy `hook-post` compression flows through (in order):
1. User TOML filters (`~/.tokenix/filters/*.toml`) — highest priority
2. Bundled TOML filters (`assets/filters/*.toml`, rust-embed)
3. Built-in heuristics in `compress.rs` — cargo, git-log, generic head/tail

`apply_filter()` pipeline: `match_output` short-circuit → `strip_ansi` → `strip_lines_matching` → `keep_lines_matching` → `head/tail/max_lines` → `truncate_lines_at` → `on_empty`. Opt-in `passthrough_when_emptied`: when the pipeline reduces *non-empty* output to nothing (an unrecognized output shape, not a genuinely empty command), emit a bounded view of the real output instead of `on_empty` — set on `git-log`/`git-diff` so `--oneline`/`--stat` don't report a false "no commits"/"no changes".

```toml
[filters.my-cmd]
match_command  = "^my-cmd\\b"
strip_ansi     = true
strip_lines_matching  = ["^\\s+Downloading"]
match_output   = [{ pattern = "Success", message = "ok" }]
max_lines      = 30
on_empty       = "my-cmd: ok"
```

## Prompt Audit (MCP/tool weight)

`tokenix prompt-audit` estimates the variable cost of the effective system prompt
per agent. The base system prompt is internal and **cannot be read or intercepted
via hooks** — this measures the next-largest lever instead: MCP tool-definition
JSON. All logic lives in `src/mcp_audit.rs`.

Per-agent MCP config sources (one `ConfigSource` each, ausente = silently skipped):

| Agent | Path(s) | Format |
|---|---|---|
| Claude Code | `<repo>/.mcp.json` + `~/.claude.json` (`mcpServers` + `projects[<cwd>]`) | JSON |
| Codex | `~/.codex/config.toml` → `[mcp_servers.<name>]` | TOML (`toml` dep) |
| OpenCode | `<repo>/opencode.json` (`mcp`) | JSON |
| Antigravity | `~/.gemini/antigravity-cli/mcp_config.json` (`mcp_config_path()`) | JSON |
| Copilot | `.vscode/mcp.json` (`servers`) + VS Code user `mcp.json` | JSON, best-effort |

Pipeline: discover → dedupe stdio transports → `introspect_stdio()` (spawn, JSON-RPC
`initialize`/`tools/list`, 5s timeout via reader thread + `recv_timeout`, kill on
done) → tokenize schemas with `count_tokens` → add static `Agent::native_tokens()`
baseline → compare to thresholds (`TOKENIX_AUDIT_WARN_{TOKENS,SERVERS,TOOLS}`).
`TOKENIX_BRANCH_AWARE=true` suffixes SQLite DB with git branch name to isolate indexes per branch.
HTTP/SSE servers are not introspected (shown `unknown`). CLI-only — no hooks, no
settings.json changes.

`--recommend` adds conservative reduction advice. `--profile-impact` estimates
the tokenix full-vs-slim MCP schema delta. `tokenix session-audit` reuses the
same summary and combines it with index freshness plus hook-log evidence;
`--cache-hygiene` also reports stable-prefix/cache-risk hints.

`tokenix mcp --profile slim` is the token-saving MCP mode: it advertises only
`tokenix_context`, `tokenix_search_tools`, and `tokenix_call`. Keep `full` as
the default for compatibility with hosts that do not support progressive tool
discovery.

## Repository Pack

`tokenix pack` emits a budgeted repo map for non-hook AI tools. Modes/profiles:
`plan`, `debug`, `audit`, `security`, `review`. Formats: `markdown`, `xml`,
`json`. `--changed` and `--since <ref>` produce review-sized packs; `--token-map`
adds per-file token/reason metadata.
It uses indexed context, file token counts, and symbol outlines; it must skip
obvious secrets, credentials, `.env`, key files, `.git`, and build output by
default. Do not turn `pack` into a raw full-repo dump.

## Benchmark

`tokenix benchmark` measures tokenix against a plain **vanilla** baseline only —
no external tools. It prints read-only token reduction, targeted
outline+symbol workflows, semantic Hit@1/Hit@3, context homologation (vanilla
full file vs tokenix budgeted context), and command-output compression. Flags:
`--budget N`, `--refresh-index`, `--cases FILE`, `--json`.

**Fairness contract (do not regress).** Benchmark is tokenix-vs-vanilla only —
do not add competitor/market comparison arms. Vanilla and tokenix are scored on
identical input counted with the same `count_tokens`. Semantic Hit@1/Hit@3 are
reported as measured (misses included), never filtered to flatter tokenix.
Default scenarios span Rust/TS/Go/Python plus SQLite vector search and command
output (cargo, git, npm, docker compose). Verdict logic is unit-tested in
`benchmark.rs`.

**Windows caveat:** `npx`/`uvx` run via `cmd /C`; `child.kill()` kills the wrapper
but a `node` grandchild may linger briefly until stdin EOF. Kill-the-tree
(`taskkill /T`) is a possible hardening follow-up.

## Common Tasks

**Add a language:** `chunker.rs` — add extension to `INDEXED_EXTS`, add `Lang` variant, map in `detect_lang()`, implement `chunk_<lang>()` following `chunk_rust()` pattern (tree-sitter), or `chunk_by_symbol_lines()` with a `<lang>_symbol_of()` line matcher when no grammar is bundled (see VB6/SQL). Also add the new `Lang` arms in `graph.rs` (`extract_references_tree_sitter`, `extract_file_imports`). Do NOT add to `INDEXED_EXTS` without a symbol-aware chunker.

**Add a bundled filter:** create `assets/filters/<slug>.toml`. Rebuild — rust-embed includes it automatically.

**Change intercept threshold:** `hook.rs` constants — `MAX_INDEX_AGE_SECS`, `MIN_LINES_FOR_OUTLINE`, `MIN_QUERY_WORDS`.

**Extend hook to a new tool:**
1. Add variant to `Tool` enum in `main.rs`
2. Implement `install_<tool>()` and `remove_<tool>()`
3. Add match arms in `cmd_install_hook()` and `cmd_remove_hook()`
4. Update `hook.rs` only if the tool has a real hook protocol
5. Document in `README.md`

**Add an agent to `prompt-audit`:** `mcp_audit.rs` — add an `Agent` variant (with `label`/`key`/`native_tokens`), a `discover_<agent>()` config source, and an `AuditAgent` value + mapping in `main.rs`. Reuse `parse_json_map` for JSON `mcpServers`-style configs.

**Add a `scan-secrets` rule:** no Rust change needed — append a `[[rules]]` block (`id`, `pattern`, optional `capture`/`min_entropy`) to `assets/secret-rules/default.toml` (or a new bundled `*.toml`), or to `~/.tokenix/secret-rules/*.toml` / `<repo>/.tokenix/secret-rules/*.toml` at runtime. Patterns use the backtracking-free `regex` crate (no lookaround). `secrets_scan.rs::compile_rules` dedups by `id` (later source wins) and skips invalid regexes with a stderr warning.

**Change token budget:** `query.rs` — `DEFAULT_BUDGET` constant, or pass `--budget` flag.

**Embedding model (flexible):** `embed.rs` `MODELS` registry maps a friendly id → `EmbeddingModel` + query/doc prefixes. Default is `nomic-v1.5` (existing indexes keep working). Select with `tokenix index --model <id>` or `TOKENIX_EMBED_MODEL=<id>`. The chosen model is **stamped in the index `meta` (`embed_model`)**; query/hook/daemon read it back via `store::index_model_id` and `embed::set_active_model` so query vectors always match the indexed docs. The model is **sticky** (a plain re-index keeps it); an explicit switch forces a full re-embed. `index_staleness` only flags a model change when `TOKENIX_EMBED_MODEL` is explicitly set. The embedding cache key (`chunk_embedding_key`) and the persistent query cache are namespaced by model id. Add a built-in model: append a `ModelSpec` with `ModelSource::BuiltIn(EmbeddingModel::…)` (use the non-quantized variant if the Qdrant-Q ONNX fails ORT's `SkipLayerNormalization`). Add a **custom** model (one fastembed does not ship, e.g. code-specialized): `ModelSource::Custom { hf_repo, onnx_file, pooling }` — `build_custom_embedding` downloads the onnx + tokenizer files (`reqwest`) into `<model_cache>/custom/<id>/` and loads them via fastembed's `UserDefinedEmbeddingModel`. `jina-code` (jinaai/jina-embeddings-v2-base-code, mean pooling) is the first such model. `tokenix doctor` lists available + active + this-repo's model, and validates user/local filters' `semantic_filter` config (`filters::semantic_filter_issues`); an unknown `semantic_filter.model` also warns at apply time before falling back to the default.

**Update pricing table:** `gain.rs` — edit `MODELS` constant and bump `PRICING_COLLECTED_AT`. Fields: `name`, `input_per_1m` (USD), `reference` (marks ★ model — used for the Gain tab's ≈USD headline).

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

### 1. Hooks (`~/.claude/settings.json` or project `.claude/settings.local.json`)

Add to the `hooks` key — merging with any existing entries:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "^(Read|Grep|Bash)$",
        "hooks": [{ "type": "command", "command": "tokenix hook", "timeout": 10 }]
      }
    ]
  }
}
```

`PreToolUse` intercepts large reads and semantic Grep queries, and rewrites noisy Bash commands so they execute through tokenix before the model sees the output.

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
- Config: `PreToolUse` in `~/.claude/settings.json` or project `.claude/settings.local.json` (see setup above)
- Input: `{"tool_name":"Read","tool_input":{"file_path":"src/main.rs"}}`

### GitHub Copilot
- Config: `.github/copilot-instructions.md` + VS Code-compatible `.github/hooks/hooks.json`
- Input: `{"toolName":"view","toolArgs":"{\"path\":\"src/main.rs\"}"}`
- tokenix normalizes `view`/`read` → `Read`

### OpenAI Codex CLI
- Config: `~/.codex/hooks.json` for `PreToolUse` Bash rewrites + optional shell helpers under `~/.codex/`

### OpenCode
- Config: repo-local `opencode.json` native `mcp` block
- Shape: `{"mcp":{"tokenix":{"type":"local","command":["tokenix","mcp"]}}}`
- Note: tokenix does **not** install `experimental.hook`; OpenCode support is native MCP registration only

### Antigravity
- Global config: `~/.gemini/config/plugins/tokenix/`, installed and registered through `agy plugin install`
- Local config: `<repo>/.agents/plugins/tokenix/`, validated through `agy plugin validate`
- Input: `{"toolCall":{"name":"read_file","args":{"path":"src/main.rs"}}}`
- Output: native `decision: allow|deny`; command rewrites use `overwrite`. Do not install `PostToolUse` for compression because Antigravity cannot replace the original output there.

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
