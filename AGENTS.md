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
| `src/pack.rs` | `tokenix pack` — budgeted repo map + focused context, changed-file packs, token maps, and safety report. `order_by_priority` decides what survives the budget: reason first (`changed` > `semantic` > filler), then per-file PageRank centrality (`store::get_file_graph_ranks`), then path. Iterating the `BTreeSet` directly used to cut files **alphabetically**, so a changed file could lose its slot to filler that merely sorted earlier; filler selection is likewise rank-first instead of largest-file-first |
| `src/graph.rs` | Symbol graph with PageRank, cycle detection (Tarjan's SCC, homonym-filtered, `path:line`-annotated), tree-sitter references, incremental repair (`update_symbol_graph_incremental` — FTS-narrowed inbound-edge restore; `rebuild-graph` = full escape hatch), file-level import graph (`rebuild_import_graph`, per-language import extraction + path resolution), HTML + Mermaid export. Repo-wide overview (`tokenix graph`): `repo_hotspots` (degree + transitive-dependent blast radius, trivial-symbol filtered), `format_repo_report` (god nodes / bottlenecks / blast-radius leaders), `format_edges_dot` (Graphviz of the top subgraph) |
| `src/artifacts.rs` | Context artifacts — index non-code files (schemas, API specs, docs) via `.tokenix/artifacts.json` |
| `src/hook.rs` | `run_hook()` — called by PreToolUse hook. Tries daemon first for Grep. Thresholds (Read 200 lines / Grep 3 words) overridable via `[hook]` in `.tokenix.toml` (`read_min_lines`, `grep_min_words`). Read intercept `is_code` set is kept in sync with `chunker::detect_lang` (Rust/Py/TS/JS/Go/**C·C++·VB·SQL**); a large file in a supported-but-unlisted language used to pass through full. `try_grep_cap`/`grep_cap_input` + `input_rewrite_output` bound a lexical `output_mode="content"` Grep that carries no `head_limit` (`TOKENIX_GREP_HEAD_LIMIT`, default 100) — called both on the non-intercept path and inside the stale-index gate, since capping needs no index |
| `src/daemon.rs` | Background TCP server (port 47392). Holds model + int8-quantized embedding cache (LRU, max 3 projects, content cap 1000). Bounded to 4 handler threads. Protocol: `search`/`health`/`status`; CLI `tokenix daemon status\|stop\|restart` |
| `src/compress.rs` | Legacy `PostToolUse` compatibility compression + `tokenix run` command-output compression: ANSI strip, emoji removal, blank-line collapse, repeat grouping (identical lines **and** repeated 2–12 line blocks via `group_repeated_blocks`), a hard `enforce_token_budget` ceiling applied to every return path including the `compact_json` early return and the TOML-filter result (`TOKENIX_MAX_OUTPUT_TOKENS`, default 8000, `0` disables), JSON compaction, base64/data-URI blob redaction (`redact_base64_blobs` = `strip_base64_blobs` for single-line/data-URI runs ≥512 chars + `strip_wrapped_base64` for line-wrapped blocks — ≥5 pure-base64 lines ≥60 wide, e.g. PEM certs/keys/MIME; PEM `-----BEGIN/END-----` markers survive; `[<kind> omitted: N chars]` typed by decoded magic — `png image base64`/`jpeg image base64`/`pdf base64`/…, keeps any `data:<mime>;base64,` prefix; a run only redacts if `looks_like_base64` — mixed-case or a `+/=-_` symbol — so single-case pure-hex/all-digit runs like `.sha256` manifests or numeric columns are NOT eaten), cargo/git-log heuristics. `tokenix run` only applies command-specific filters to stderr when `filter_stderr=true`; otherwise stderr uses safe generic compression so errors are not turned into success sentinels. `run_hook_post` also processes **non-shell** tool results (e.g. MCP image-generation output) for base64-only redaction in dialects that can replace the result (Copilot); Claude/Codex post stays a no-op |
| `src/filters.rs` | `FilterDef` (TOML schema), active filter listing, `load_user_filters()`, `load_bundled_filters()` (rust-embed), `apply_filter()`. `find_filter()` matches via `derive_command_candidates()`, which unwraps shell runners, strips `cd`/env prefixes, and `split_on_operators()` splits compound commands quote-aware on `&&`/`\|\|`/`;`/`\|` so anchored `match_command` patterns match a base command in any segment/position. **Hot path (hook / `tokenix run`) uses `load_filters_for_command()`, not `load_all_filters()`** — see "Filter resolution cost" below |
| `src/cmd_filter.rs` | `tokenix filter list/active/generate` + `filter record start/stop/status` subcommands. `generate` prefers `recordings::read_samples` over a re-run, invokes a detected AI CLI, and saves to `~/.tokenix/filters/`; reused by the TUI Studio tab as a foreground drop-out |
| `src/tui.rs` | Interactive ratatui shell — the **only** human interface. Entered by a bare `tokenix` or by any human report command on a TTY (see "One interface" below); `run_entry(Entry)` seeds the opening tab and its scope, `should_open()` is the single TTY/`--no-tui` gate, and `new_shell()` builds the state (split out so the shell is unit-testable). `switch_tab` calls `disarm()` so an armed destructive confirmation (redact / delete filter / install) can never survive a tab change; Studio generate/delete call `reload_filters()` so the Filters tab never lists a filter that no longer exists. Tab bar (`←`/`→`): **Stats** dashboard (wordmark + version + hook status + index summary, with selectable Index / Install hooks / Install binary actions — Index runs in the foreground with live progress, the two install actions confirm before writing; Install binary self-execs `tokenix install-binary`), **Filters** (3-pane groups · filters · live `apply_filter` input→output preview with a `chunker::count_tokens` gauge line showing `X → Y tokens · % saved` between the panes), **Studio** (surfaces the record→preview→generate filter loop: `r`/`s` arm/stop a `recordings::start`/`stop` session, left column is a unified candidate list from `cmd_filter::suggest_filters` — recordings unioned with the tokens-wasted ranking, badged `⚠` unfiltered sink (biggest waste first) / `✓` already filtered / `●` recorded-only — plus saved `~/.tokenix/filters/*.toml`, right pane previews a `recordings::read_samples` head with a live `apply_filter` before→after `chunker::count_tokens` delta when an active filter matches the base command; `g` sets `request_generate` to run `cmd_filter::cmd_filter_generate` as a foreground drop-out — same pattern as Index — then resumes the TUI; `x` deletes a saved filter with confirm; `Tab` switches pane), **Gain** (native colored render of `gain::compute_gain`: tokens-saved headline with ≈USD at the ★ reference model's input rate, savings-by-source split — semantic index vs command filters — and numbered by command / by project tables with share %, toggles `c`/`a`), **Usage** (self-exec captured `tokenix usage` via dynamic argv: `s` cycles daily/model/blocks/project/session, `a` toggles all-projects, `r` refresh), **Doctor**/**Tokenmap** (self-exec captured output), **Graph** (self-exec captured `tokenix graph` repo overview — god nodes / bottlenecks / blast radius; `r` refresh), **Secrets** (background-threaded `secrets_scan::scan_findings` with spinner; dedup by distinct value + count; `v` reveal, `c` copy raw value to system clipboard via `clip`/`pbcopy`/`wl-copy`/`xclip`/`xsel`, `x` write `[REDACTED]`), **Egress** (background-threaded `egress_scan::scan_findings` with the same 3-pane pattern as Secrets: groups · destinations · occurrence detail; `s` cycles host/rule/agent/file grouping; `r` rescans; host reputation colors: green safe, red dangerous, yellow unknown). Both Secrets and Egress open scoped to the current repo (cwd) and `g` toggles a global all-repos view; scoping filters the raw scan by each finding's attributed `repo` (`is_local` matches exact `cwd` paths plus Claude `~slug:`/Gemini `~dir:` fallback markers against the project root). **Loading is standardized:** every data-loading tab (Gain, Usage, Doctor, Tokenmap, Graph, Secrets, Egress) loads on a background thread behind one shared panel (`draw_loading` + `spinner_frame`, single `SPINNER_FRAMES`) so the shell never blocks and the braille spinner animates; the event loop polls at 120ms whenever any `*_rx` is in flight. Only Index still runs as a foreground drop-out (it needs the child's own live progress bar) |
| `src/ui.rs` | Shared terminal-UI vocabulary for human-facing CLI output (`box_header`, `bar`, `section`/`kv`, `format_num`, `table` via `tabled`); LLM/JSON output deliberately does not route through it |
| `src/gain.rs` | `compute_gain()`/`compute_global_gain()`, `GainStats` (incl. `index_saved`/`filter_saved` source split: empty `command` = semantic-index intercept, non-empty = command filter; pre-phase Bash/PowerShell rewrite markers are excluded from `filter_calls`), `MODELS` pricing table (Anthropic/OpenAI/Google, with `input`/`output`/`cache_read`/`cache_write` per-1M rates; `price_for` name/prefix match + `usage_cost` per-record helper reused by `tokenix usage`). Grep semantic intercepts are logged as neutral usage, not claimed savings, because native grep output is not measured before interception |
| `src/transcripts.rs` | Shared enumeration of local agent transcript files (`roots` per agent: Claude/Codex/Copilot/OpenAI, `transcript_files` walker). Single source of truth reused by `conversation-audit` and `usage` |
| `src/usage.rs` | `tokenix usage` — absolute token spend + ≈USD cost parsed from transcript `message.usage` blocks (input/output/cache read+write), deduped by `(message.id, requestId)`. Aggregates by `daily\|weekly\|monthly\|session\|model\|project`; rolling 5-hour `blocks` with burn rate + projection; month-end forecast; `--cost-mode auto\|calculate\|display`; `--statusline`; `--all-projects` scope; `--json` |
| `src/mcp.rs` | MCP server. `--profile full` exposes all tools; `--profile slim` exposes context/search/call meta-tools for progressive discovery |
| `src/mcp_proxy.rs` | `tokenix mcp-proxy` — stdio JSON-RPC passthrough that compresses `tools/call` text results (the only reachable path for MCP output); see the section below for the contract |
| `src/mcp_audit.rs` | `tokenix prompt-audit` / `session-audit` — per-agent MCP config discovery (Claude, Codex, Copilot, OpenCode, Antigravity) + minimal synchronous MCP stdio client (`initialize`/`tools/list`) + token scoring/report + `context_weight()` (instruction files always-on, skills listing always-on / body on-invoke) |
| `src/secrets_scan.rs` | `tokenix scan-secrets` — gitleaks-style credential scan of Claude/Gemini/Copilot/Antigravity conversation transcripts under `~`; rules loaded from TOML (`assets/secret-rules/` bundled via `rust-embed`, extended by `<repo>/` then `~/.tokenix/secret-rules/*.toml`, later `id` wins), backtracking-free regex + entropy-gated generic rule. Each finding is attributed to its repo + git branch via the transcript line's `cwd`/`gitBranch` (Claude), falling back to the project dir slug. Report supports `--filter` (substring), `--group <value\|rule\|agent\|file\|repo>`, `--reveal` (raw values, default redacted), `--json`; exit 1 on hits. `scan_findings()` returns structured `ScanFinding`s (raw + redacted) for the TUI; `redact_in_files()` rewrites `[REDACTED]` over a value in text files (SQLite DBs skipped) |
| `src/egress_scan.rs` | `tokenix egress-audit` — scans Claude/Gemini/Copilot/Antigravity conversation transcripts for external DNS/IP destinations; bundled TOML rules live under `assets/egress-rules/`, local safe hosts are loaded from `~/.tokenix/safe-hosts.toml`, and local blocklist hosts from `~/.tokenix/dangerous-hosts.toml` (`dangerous`, `blocklist`, or `hosts` arrays); report supports `--filter`, `--group <host\|rule\|agent\|file>`, `--safe`, and `--json`. `scan_findings()` returns structured `EgressFinding`s for the TUI |
| `src/discover.rs` | `tokenix discover` — scans agent transcripts (Claude `tool_use`/`tool_result`, Codex/OpenAI `function_call`/`function_call_output`, argv-array commands) and REPLAYS the current filter set over historical command outputs: measured recoverable savings (filter exists, hook wasn't active) + uncovered commands ranked by waste. Memoizes command→filter matches |
| `assets/filters/` | 528 TOML output filters embedded via `rust-embed`, each homologated with ≥2 golden `[[tests]]` cases (realistic success + failure-path inputs; the failure case must prove errors are never masked). 1146 cases run through the real `apply_filter` pipeline in `bundled_filters_pass_embedded_golden_tests`; `verbose_real_output_compresses_at_least_70pct` proves ≥70% reduction on realistic verbose output and `match_command_resolves_many_invocation_variants` homologates wrapper/shell/global-opt command variants. User filters in `~/.tokenix/filters/` take priority |

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
  pattern < 3 words → not semantic; symbol lookup if identifier-like, else:
      output_mode="content" without head_limit → PreToolUse updatedInput injects
      head_limit (TOKENIX_GREP_HEAD_LIMIT, default 100, 0 disables); logged with
      saved_tokens=0 because the unbounded output never ran
      otherwise → exit 0 (pass)
  pattern ≥ 3 words → return semantic results, exit 2 (intercept); gain records this as neutral usage, not saved tokens

Bash / PowerShell tools:
  command matches a bundled/user filter → rewrite to `tokenix run` (PowerShell
  uses `& 'exe' run --shell pwsh '<cmd>'`, re-executed under pwsh with UTF-8)
  otherwise → exit 0 (pass)

Index stale → Grep still gets its head_limit cap, then exit 0 for every tool.
  Staleness is NOT age-based: `store::index_staleness` flags a missing DB, a
  missing `indexed_at`, an explicitly requested different embedding model, or a
  changed git fingerprint (auto-updated when HEAD moved but the code is
  identical).
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

`compress_output()` order: `redact_base64_blobs` → `compact_json` (early return,
**also capped**) → `strip_ansi` → `remove_emojis` → `collapse_blank_lines` →
`group_repeated_blocks` → `group_repeated_lines` → `generic_aggressive_compress`
→ `enforce_token_budget`. The last two are the newer backstops:
- `group_repeated_blocks` collapses a 2–12 line stanza repeated 3+ times
  (`[block of N lines repeated Kx]`); `group_repeated_lines` only ever saw runs
  of *identical adjacent lines*, so poll/watch loops slipped through (one
  monitoring session measured ~617k tokens this way). Skipped above 100k lines.
- `enforce_token_budget` clips anything over `TOKENIX_MAX_OUTPUT_TOKENS`
  (default 8000, `0` disables) to a 75% head + 25% tail window at char
  boundaries. It is the only cap that covers single-giant-line output and the
  `compact_json` early return, both of which bypass every line-count cap.
  `preserved_identifiers` scans the dropped range for things a caller cannot
  re-derive — git SHAs (hex, 7–40, mixed letters+digits so "decade"/"facade"
  and plain numbers are excluded), UUIDs, http(s) URLs, `E0308`-style codes —
  and appends up to 12 of them (≤240 chars) as `[ids kept from the omitted
  range: …]`.

## MCP proxy (`src/mcp_proxy.rs`)

`tokenix mcp-proxy [--name X] -- <server cmd>` wraps a stdio MCP server. This is
the **only** path that reaches MCP tool output: PreToolUse fires on the agent's
own tools, and Claude Code ships no PostToolUse
([[project_claude_no_posttooluse_by_design]] in memory) — so an image-generation
or browser-snapshot result was previously unreachable, which is where
`conversation-audit`'s largest bucket (image-blob, millions of tokens) lives.

Contract, deliberately narrow:
- requests are forwarded **verbatim** — the proxy never rewrites what the agent asks for;
- only `result.content[]` blocks with `type == "text"` are compressed, via
  `compress_output` (base64 redaction → JSON compaction → repeat collapse → cap);
- `image` blocks are never touched (the host renders them);
- `tools/list` schemas are never touched (a shortened description changes what
  the model believes the tool does — that is mcp-compressor's trade, not ours);
- per-block `never worse`: the original text stays when compression is not smaller;
- results under `MIN_RESULT_TOKENS` (200) pass through untouched;
- anything unparseable is forwarded as-is; child stderr is inherited.

Savings log as `tool="MCP"`, `phase="proxy"`, `command="mcp:<name>"`, so `gain`
counts them under command filters. `TOKENIX_MCP_PROXY=0` disables the rewriting.

## Recall (`src/recall.rs`)

Content-addressed stash + dedup, ported from what competitors do well (squeez's
reversible compression + cross-call dedup; read-once / semantic-cache-MCP's
re-read suppression). FNV-1a digest, no new dependency.

| Store | Path | Cap |
|---|---|---|
| blobs (raw + compressed bodies) | `~/.tokenix/blobs/<digest>.txt` | 60 files, oldest pruned |
| command output index | `~/.tokenix/recent_outputs.json` | 24 entries |
| full-read index | `~/.tokenix/recent_reads.json` | 24 entries |

- **`tokenix retrieve <key>`** prints a stashed body. Keys are validated as
  ASCII-alphanumeric, so a key echoed by the model can never traverse out of the
  blob directory.
- **Cross-call dedup** (`find_identical` in `run_command_and_compress`): only on
  **exit 0** — a repeated failure still needs its error text — only above
  `TOKENIX_DEDUP_MIN_TOKENS` (200), and only when the marker is shorter than the
  output. Every hit is verified against the stored blob, so a hash collision or
  a pruned blob degrades to "no match" rather than a wrong collapse.
  `TOKENIX_DEDUP=0` disables.
- **Re-read suppression** (`find_recent_read` in `handle_read`): remembers only
  reads that delivered the **full content**. A read answered with an outline is
  deliberately *not* remembered — otherwise the agent could never get past the
  outline. TTL `TOKENIX_READ_DEDUP_TTL` (900s) bounds the context-compaction
  risk; `TOKENIX_READ_DEDUP_MIN_TOKENS` (1500) keeps small files verbatim;
  `TOKENIX_READ_DEDUP=0` disables. Recovery is exact via the stash key.

`apply_filter()` pipeline: `match_output` short-circuit → `strip_ansi` → `strip_lines_matching` → `keep_lines_matching` → `head/tail/max_lines` → `truncate_lines_at` → `on_empty`. Opt-in `passthrough_when_emptied`: when the pipeline reduces *non-empty* output to nothing (an unrecognized output shape, not a genuinely empty command), emit a bounded view of the real output instead of `on_empty` — set on `git-log`/`git-diff` so `--oneline`/`--stat` don't report a false "no commits"/"no changes". The same bounded fallback fires **automatically** (no opt-in) whenever the original output matches `output_has_failure_signal()` (a strict, case/anchor-tuned `error`/`fatal`/`panic`/`FAILED`/`exit code N` probe) — so a failed build/test/deploy whose error text isn't matched by the tool's `keep_lines_matching` is never masked as the success `on_empty`. Guarded by `bundled_filters_never_mask_generic_failure`.

**Filter design rule: never use `on_empty` — use `passthrough_when_emptied = true` instead.** `on_empty` fabricates a static string when real output is filtered to nothing; `passthrough_when_emptied` returns the original unfiltered output. Filters must only filter, never invent responses. `match_output` is the only valid short-circuit (it fires only when a confirmed pattern exists in the real output). Tests must not assert on fabricated strings.

```toml
[filters.my-cmd]
match_command  = "^my-cmd\\b"
passthrough_when_emptied = true
strip_ansi     = true
strip_lines_matching  = ["^\\s+Downloading"]
match_output   = [{ pattern = "Success", message = "ok" }]
max_lines      = 30
```

## One interface: direct commands open the shell

There is exactly one human-facing rendering, the ratatui shell. A human report
command run on a terminal **redirects to its own tab** instead of printing a
second, divergent view of the same data:

| Command | Tab | Seeded state |
|---|---|---|
| `tokenix` (bare) | Stats | — |
| `stats` | Stats | — |
| `doctor` | Doctor | — |
| `tokenmap` | Tokenmap | — |
| `graph` | Graph | — |
| `discover` | Discover | — |
| `prompt-audit` | Audit | — |
| `gain` | Gain | `--global`, `--cost-estimate` |
| `usage [group]` | Usage | group (daily/model/blocks/project/session), `--all-projects` |
| `scan-secrets` | Secrets | opens all-repos (the CLI scan is not repo-scoped) |
| `egress-audit` | Egress | opens all-repos |
| `filter`, `filter active` | Filters | — |
| `filter list` | Studio | — |

`main.rs::tui_target()` is the single decision table; `tui_entry_for()` adds the
gate `tui::should_open()`. **The table must stay pure and testable** — the
`tests::{human_report_commands_target_their_tab, machine_and_unrepresentable_invocations_keep_printing, agent_facing_commands_never_open_the_shell, scoping_flags_are_carried_into_the_tab}` cases lock it down.

Plain output is kept whenever:
- stdout is not a terminal (pipe, CI, capture);
- `--no-tui` (global flag) or `TOKENIX_NO_TUI=1`;
- the invocation asked for something a tab cannot represent: `--json`,
  `--statusline`, `--format html|dot`, `--output <file>`, `--since/--until`,
  `--filter`, `--reveal`, `gain --history/--economics`, `--top`/`--since-days`
  other than the default (`graph`, `discover`), `prompt-audit
  --recommend/--profile-impact`, `usage weekly|monthly`, `filter list <index>`,
  or a `--path` pointing outside the current repo (the shell always reports on
  the repo it was opened in).

**Agent-facing commands are never redirected** — `hook`, `hook-post`,
`hook-antigravity`, `run`, `mcp`, `query`, `context`, `read`, `symbols`,
`pack`, `index`, `serve`, `retrieve`, `install-hook`, `prompt-audit`,
`discover`, `benchmark`. A terminal UI in those paths would break the product.

The shell's own self-exec captures (`capture()`, `run_index_foreground()`) set
`TOKENIX_NO_TUI=1` on the child so a report tab can never spawn another shell.

## Per-filter homologation

Aggregate green is not per-filter green. `homologate_each_filter` (ignored by
default — `cargo test --release --bin tokenix homologate -- --ignored
--nocapture`) emits one TSV row per bundled filter: golden verdict, raw→filtered
tokens and %, whether a golden case carries a failure signal, whether that case
is masked, byte inflation, sentinel shape, prefilter class, and whether the
filter fabricates its `on_empty` sentinel over unrecognized output.

Status of the 528 bundled filters at the last full pass:

| Property | Result |
|---|---|
| Golden cases byte-exact | 528/528 (1,146 cases) |
| Never inflates non-empty output (`never_worse`) | 528/528 |
| Never masks a failure signal | 528/528 |
| Median per-filter token economy | 32% (mean 33%, max 88%) |
| Fabricates a sentinel over unrecognized output | **36** (was 141) |
| Golden cases include a failure-path input | 204/528 |

Two findings from that pass are worth carrying forward:

1. **The 89 filters fixed.** A filter whose sentinel only ever answers a
   genuinely empty run (a silent linter) was answering *unrecognized non-empty*
   output with the same "all clear". Adding `passthrough_when_emptied = true`
   is behavior-preserving for every golden case (proven: the whole suite stayed
   green) and replaces the lie with a bounded view of the real output.
   `on_empty_sentinel_never_answers_unrecognized_output` locks this in.
2. **The summary filters (52 → 40).** These legitimately collapse *recognized*
   verbose output into a summary (proven by a golden case with non-empty
   input), so passthrough alone would destroy their compression. The fix is
   **positive evidence**, borrowed from rtk's `basedpyright` filter: pair a
   `match_output` on the tool's own success marker with
   `passthrough_when_emptied = true`. `match_output` short-circuits first, so
   the clean run still collapses to one line; unrecognized output now falls
   through to the real text instead of a fabricated "all clear".

   ```toml
   match_output = [
     { pattern = "didn't find any unused dependencies",
       message = "cargo-machete: no unused dependencies",
       unless = "(?i)\\b(error|fatal|failed)\\b" },
   ]
   passthrough_when_emptied = true
   ```

   **For per-item tools that rule does not apply** — a `match_output` on
   `PASS` fires on a run where item 3 failed. That is what `uniform_success`
   exists for: it collapses only when EVERY significant line matches the pass
   shape and at least one did, so a single dissenting line disqualifies the
   whole run, and `{n}` reports the real item count.

   ```toml
   passthrough_when_emptied = true
   uniform_success = { pattern = "^PASS - ", message = "conftest: {n} policies passed" }
   ```

   Converting a filter this way *promotes* it out of the summary group: with
   the summary job handled by evidence, its `on_empty` only answers a genuinely
   empty run, so `passthrough_when_emptied` becomes behavior-preserving.
   Applied to conftest, kubeconform, prowler and lychee (40 → 36). Verified
   end-to-end through the real binary, not just fixtures: uniform run →
   `conftest: 3 policies passed`; one `FAIL` line → real output kept;
   unrecognized output → real output kept.

   Applied to the 12 whose marker is a whole-run summary (cargo-machete,
   cargo-outdated, cargo-udeps, attw, dart-analyze, ggshield, istioctl, nuclei,
   osv-scanner, atlas, flux, sequelize) — golden stayed green and mean economy
   did not move. **The remaining 40 are deliberately untouched**: their marker
   is per-item (`PASS`, `ok 1`, `[200]`, `is valid`), so a `match_output` on it
   would short-circuit a run where *other* items failed. They need a
   whole-run assertion, not a line marker. `report_sentinel_evidence_candidates`
   (ignored test) prints the fixture that already contains each one's evidence.

**Coverage note:** 324 filters have no failure-path golden case of their own.
That is a corpus documentation gap, not a live hole —
`bundled_filters_never_mask_generic_failure` feeds *every* bundled filter a
synthetic failure payload and asserts a failure marker survives, so the
guarantee holds for all 528 regardless.

## Filter resolution cost (hook hot path)

The `PreToolUse` hook is a fresh process on **every** Bash/PowerShell call, so
filter resolution is latency, not throughput. Three mechanisms keep it cheap;
all three are "skip work only" — none may change which filter is selected.

1. **Prefilter (`prefilter_for`)** — a cheap NECESSARY condition derived from
   the raw pattern text: `^cargo\s+test` → candidate must *start with* `cargo`;
   `\bactionlint\b` / `^(npx\s+)?eslint\b` → candidate must *contain* the first
   mandatory literal (case-folded for a leading `(?i)`). Unrecognized shapes
   (mandatory alternation, nested groups) return `None` = always evaluate.
   `literal_run` stops at `.` and `+` because those are metacharacters, and a
   **top-level `|` alternation returns `None`** — only the first branch carries
   the leading literal, so no single literal is mandatory (3 bundled patterns;
   `select-string` is the one that was actually unsound). Guarded by
   `prefilter_never_rejects_a_real_match`, which asserts
   `regex matches ⇒ prefilter allows` over every bundled/user pattern against a
   corpus of each filter's own literal **plus every `command` recorded in the
   golden cases** — that corpus is what surfaced the alternation hole.
2. **Lazy loading (`load_filters_for_command`)** — the bundled corpus is
   indexed once per process by scanning each embedded file's `match_command`
   off the raw text (`scan_bundled_anchors`, no TOML parse); only files whose
   prefilter allows a candidate are parsed. `parse_filter_file_named` also cuts
   the content at the first `[[tests]]` block (~60% of the bytes) and falls
   back to a full parse if that head does not parse. Homologated by
   `lazy_load_matches_full_load_for_commands` and
   `scanned_anchor_never_overshoots_parsed_pattern`.
3. **User filter index** — `~/.tokenix/filters/.prefilter-index.json` caches
   per-file prefilters, validated against the directory listing (name + mtime +
   size); any drift rebuilds the whole index. Without it, a user with hundreds
   of generated filters pays ~15 ms of file I/O per hook call.

Regexes are compiled at most once per process (`cached_regex`), including
inside `apply_filter` — compiling per call made the TUI preview and
`tokenix discover`'s replay loop recompile the same patterns thousands of
times.

Measured on Windows (528 bundled + 231 user filters), `tokenix hook` for a
Bash command: **~59 ms → ~28 ms** end-to-end; `apply_filter` 1.28 ms → 0.07 ms.

## Prompt Audit (MCP/tool/context weight)

`tokenix prompt-audit` estimates the variable cost of the effective system prompt
per agent. The base system prompt is internal and **cannot be read or intercepted
via hooks** — this measures the next-largest levers instead: MCP tool-definition
JSON plus the context an agent loads before doing anything. All logic lives in
`src/mcp_audit.rs`.

Context weight (`context_weight()` → `ContextWeight`), added because a measured
history showed a single skill body costing ~198k tokens while the audit reported
only MCP schemas:

| Source | Agents | Counted as |
|---|---|---|
| `CLAUDE.md`, `AGENTS.md`, `.github/copilot-instructions.md`, `~/.claude/CLAUDE.md`, `~/.codex/AGENTS.md` | per-agent path list | always-on (full file) |
| `<repo>/.claude/skills/*/SKILL.md`, `~/.claude/skills/*/SKILL.md`, `~/.claude/plugins/**/skills/*/SKILL.md` (depth ≤ 4) | Claude Code only | always-on = frontmatter `name`+`description`; body reported separately as on-invoke |

Only always-on tokens enter the per-agent total and `combined_tokens`; bodies
≥ `HEAVY_SKILL_TOKENS` (5k) are listed as "loaded on invoke". `aggregate()` takes
the `ContextWeight` as a parameter (not the cwd) so tests aggregate against a
known-empty context instead of the developer's real home directory.

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

**Add a bundled filter:** create `assets/filters/<slug>.toml` with **≥2 embedded `[[tests.<name>]]` golden cases** (input/expected — enforced by `bundled_filters_require_minimum_tests`). `on_empty` and `passthrough_when_emptied` **compose — they do not conflict** (an earlier note here claimed otherwise; the code disagrees and 94 bundled filters ship with both, golden-green). The fallback in `apply_filter_with_exit` is gated on `!output.trim().is_empty()`, so passthrough only takes over when filtering emptied *non-empty* output; a genuinely empty run still gets the sentinel. Setting both is the recommended shape for a silent-on-success tool. Any filter that can empty a failure payload must keep failure markers (`(?i)error|fail|fatal`) or set `passthrough_when_emptied` — else `bundled_filters_never_mask_generic_failure` fails. Rebuild — rust-embed includes it automatically. Homologate with `cargo test --bin tokenix filters::tests::` (golden + 70% economy + never-mask + no-inflate). Currently 528 filters · 1146 golden cases. Engine invariants: `never_worse` guarantees the filtered result never costs more bytes than the raw output (longer sentinel/notice → raw wins); `head_lines`+`tail_lines` together form a first+last window with an inline `[... N lines omitted ...]` middle marker; `apply_filter_with_exit` suppresses success sentinels on nonzero exit and honors per-filter `on_failure = "passthrough"|"tail:N"`; `priority_lines` survive every sizing cut; `category_caps` bound repetitive classes with a count marker; `FilterDef` is `deny_unknown_fields` (typo'd keys fail loudly — `tokenix filter verify` runs user/project golden tests from the installed binary). Repo-local `.tokenix/filters` are trust-gated (`tokenix trust`, SHA-256 in `~/.tokenix/trusted_filters.json`) and skipped until approved. Failed commands with clipped output tee the raw to `~/.tokenix/tee/` with a `[full output: path]` hint (TOKENIX_TEE=0 disables). `TOKENIX_DISABLED=1` prefix bypasses the hook per command (logged as action="bypassed").

**`filter record` token-economy preview:** `recordings::economy()` reconstructs each captured command's raw output (stripping the `$ cmd`/`--- stderr ---`/truncation scaffold), resolves the bundled filter via the real `find_filter`+`apply_filter` path, and reports `raw→filtered` tokens. `record stop`/`status` render it as a per-command compression bar + total via `print_economy_table` in `cmd_filter.rs`.

**Change intercept threshold:** `hook.rs` constants — `MIN_LINES_FOR_OUTLINE`, `MIN_QUERY_WORDS` (both overridable per project via `[hook]` in `.tokenix.toml`). Index staleness lives in `store::index_staleness` and is fingerprint-based, not age-based.

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
tokenix read <file> --mode signatures      # signatures only (no bodies)
tokenix read <file> --mode diff            # outline + uncommitted hunks
tokenix read <file> --mode density:40      # keep ~40% highest-entropy lines
```

Only read a full file directly when tokenix shows it is small.

Inspect the symbol graph and spend with:

```bash
tokenix graph                 # repo-wide god nodes / bottlenecks / blast radius
tokenix graph --format dot    # Graphviz of the top subgraph
tokenix usage                 # absolute token spend + ≈USD cost (daily)
tokenix usage blocks          # rolling 5-hour billing blocks + burn rate
```

## Release

Releases are automated via GitHub Actions (`.github/workflows/release.yml`). Pushing to `main` auto-creates a version tag and GitHub Release with pre-built binaries for Linux, macOS, and Windows.

To trigger manually: push a commit to `main` — the workflow reads version from `Cargo.toml`.
