# AGENTS.md — tokenix

Rust CLI that gives AI coding agents compact repository context: local ONNX
embeddings + SQLite index, tree-sitter symbol graph, deterministic output filters,
and `PreToolUse` hooks for Claude Code / Copilot / Codex / OpenCode / Antigravity.
User-facing docs live in `README.md`; this file is the engineering contract.

## Build & test

```bash
cargo build --release
cargo test --bin tokenix          # 379 unit + golden tests
cargo fmt --check                 # CI runs fmt FIRST — run it before pushing
cargo clippy --bin tokenix --all-features
```

## Key files

| File | Role |
|---|---|
| `chunker.rs` | Symbol-aware chunking, `count_tokens`, outline generation, `enforce_token_cap` |
| `indexer.rs` | Walks + indexes files. `filter_entry` (dirs) vs `should_index` (files) are separate on purpose |
| `embed.rs` | ONNX via fastembed; `MODELS` registry, custom HF models, int8 quantization |
| `store.rs` | SQLite access, `index_staleness`, graph tables, hook log |
| `query.rs` | Semantic + lexical retrieval, RRF fusion, budgeting |
| `graph.rs` | Symbol graph, PageRank, Tarjan SCC cycles, import graph, repo hotspots |
| `hook.rs` | `PreToolUse` handler — the interception decision tree |
| `compress.rs` | Generic output compression, base64 redaction, token ceiling, EOL preservation |
| `filters.rs` | `FilterDef` schema, filter resolution, `apply_filter_with_exit` |
| `cmd_filter.rs` | `filter list/active/generate/verify` + recording |
| `pack.rs` | `tokenix pack` — budgeted repo map, `order_by_priority` (reason → PageRank → path) |
| `recall.rs` | Stash/retrieve of clipped output, re-read suppression |
| `gain.rs` | Savings analytics, `MODELS` pricing table |
| `usage.rs` | Spend + cost from agent transcripts (`message.usage`, incl. cache fields) |
| `mcp.rs` / `mcp_proxy.rs` | MCP server (`--profile slim\|full`) / stdio passthrough that compresses results |
| `mcp_audit.rs` | `prompt-audit` / `session-audit`, `context_weight()` |
| `secrets_scan.rs` / `egress_scan.rs` | Transcript forensics — credentials and outbound destinations |
| `discover.rs` | Replays current filters over historical agent output |
| `transcripts.rs` | Per-agent history roots and parsers |
| `tui.rs` | Ratatui shell — the only human interface |
| `daemon.rs` | Background embedding server, port 47392 |

## SQLite schema

```sql
files(id, path UNIQUE, mtime, content_hash)
chunks(id, file_id, path, start_line, end_line, symbol, kind, content, token_count)
chunks_fts(rowid, content, symbol, path)     -- FTS5
embeddings(chunk_id PK, embedding BLOB, scale REAL)
  -- scale NOT NULL → int8-quantized (1 byte/dim); NULL → legacy float32 LE.
  -- Scale cancels out of the cosine, so q8 search needs only raw bytes.
embedding_cache(content_hash PK, embedding BLOB, updated_at)  -- stays float32 so
  -- model switches and quantization changes never force a re-embed
graph_nodes(chunk_id PK, file_id, path, name, kind, start_line, end_line, rank)
graph_edges(id, caller_chunk_id, callee_chunk_id, reference, edge_kind)
graph_imports(id, source_path, target, resolved_path, kind, line)  -- NULL = external
meta(key PK, value)                          -- 'indexed_at', git fingerprint
```

`meta` holds `indexed_at` plus a Git fingerprint (worktree root + branch + HEAD);
a different fingerprint counts as stale so branch switches never reuse context.

**Query paths open old DBs without migrating** — SELECTs must degrade when `scale`
is missing (`embeddings_have_scale()` probes by selecting `NULL`).

Hook log: `~/.tokenix/<project-id>.log`, NDJSON, one `HookEvent` per line, rotates
at 5 MB (one generation). Fallback is repo-local `.tokenix/hook.log`.

## Intercept logic

```
Read:  < 200 lines OR offset/limit set → exit 0 (pass)
       ≥ 200 lines, no offset/limit    → outline, exit 2 (intercept)

Grep:  < 3 words → not semantic; symbol lookup if identifier-like, else
         output_mode="content" without head_limit → updatedInput injects
         head_limit (TOKENIX_GREP_HEAD_LIMIT, default 100, 0 disables);
         logged saved_tokens=0 because the unbounded output never ran
       ≥ 3 words → semantic results, exit 2; gain records neutral usage

Bash / PowerShell: matches a filter → rewrite to `tokenix run`
       (PowerShell: `& 'exe' run --shell pwsh '<cmd>'`, re-executed under pwsh)
       otherwise → exit 0

Index stale → Grep still gets its head_limit cap, then exit 0 for every tool.
```

Staleness is **not** age-based: missing DB, missing `indexed_at`, an explicitly
requested different embedding model, or a changed git fingerprint.

Installer matcher: `^(Read|Grep|Bash|PowerShell|grep_search|run_in_terminal)$`.
Claude Code's exact-name `PowerShell` tool takes the pwsh path; the lowercase
`powershell` from Copilot/Antigravity stays on the bash path.

`get_effective_command` normalizes before matching so filters anchored on the bare
tool still hit: strips shell wrappers, `cd`/env prefixes, package runners
(`uv run`, `python -m`, `npx`, `bunx`, `pnpm exec/dlx`, `yarn dlx`, `bun x`,
`deno run/task`) and tool-global options (`git -C`, `kubectl -n`, `docker -H`,
`cargo +tc`). Verified against real histories where `uv run pytest` and
`bunx biome` were bypassing their filters. `split_on_operators` splits compound
commands quote-aware on `&&`/`||`/`;`/`|`.

Per-project tuning in `.tokenix.toml`:

```toml
[hook]
read_min_lines = 120   # default 200
grep_min_words = 3     # default 3
```

## Critical rules

**Never lose content.** The chunker stores 100% of every indexed file. Generic
files (.md, .txt, .yaml, .json) use `clean_generic_text()` — full content,
formatting stripped. Truncated previews are forbidden.

**Never break hook fallback.** `run_hook()` must `exit(0)` on any error — missing
index, stale index, parse failure, embed error. Breaking a session is worse than
missing a saving.

**Hook exit codes:** `0` = pass through · `2` = block tool (stderr becomes the
agent's context). Never exit `1`.

**Never mask a failure.** Non-zero exit suppresses success sentinels
(`match_output`, `on_empty`). A filter that can empty a failure payload must keep
failure markers (`(?i)error|fail|fatal`) or set `passthrough_when_emptied`.
Measured elsewhere: an over-aggressive log filter scored *below* passing the raw
output through, because it destroyed the root cause.

**Preserve line endings.** Every transform splits with `.lines()` (which consumes
`\r\n`) and rejoins with `"\n"`. `compress_output` and `apply_filter_with_exit`
detect the dominant terminator (`compress::dominant_eol`, CRLF wins when
`crlf * 2 > lf`) and restore it on the way out (`compress::restore_eol`,
idempotent). Without this a CRLF file on Windows comes back LF-only and every line
differs from disk by one byte — fatal when the agent quotes it into an exact-match
edit, which has zero fuzz tolerance.

**Compress at write time, never rewrite history.** Reducing an observation before
it enters the conversation leaves an already-cached prefix intact. Retroactively
rewriting earlier turns invalidates the prompt cache from that point, and cache
write costs ~12.5× a cache read.

**Directory filtering:** `filter_entry` for directories uses ONLY `IGNORED_DIRS`.
Do NOT call `should_index()` on directories — it returns false for dirs without
extensions and breaks traversal.

**Daemon is optional.** If `tokenix serve` is not running, `handle_grep()`
autostarts it and retries once (800 ms), then falls back to in-process embed.

**Cross-platform paths:** `tokenix_bin_path()` normalizes to forward slashes for
shell/JSON config strings.

**Hook log format:** do not move away from NDJSON without updating `gain.rs`.

**Token count is approximate.** `count_tokens()` = `chars / 4` rounded up —
**chars, not bytes**, so non-ASCII is not charged 2–4×. Deliberate approximation;
a real BPE tokenizer is roadmap, not a dependency yet.

**`gain` reports tokens, not money.** Tokens removed from a payload is a token
measurement. Published work found token reduction and provider-billed cost are
close to uncorrelated (r = 0.15) because cache traffic dominates a real bill. Do
not add a savings-in-dollars headline until cache-aware accounting lands. See
`docs/research/2026-08-token-economy.md`.

**Keep docs in sync.** Every new or changed user-facing feature MUST update both
`README.md` (Commands, and any affected section) and this file in the same change.

## Output filters

Resolution: `<repo>/.tokenix/filters` (trust-gated) → `~/.tokenix/filters` →
bundled. Currently **528 filters / 1,146 golden cases**.

**Hot path uses `load_filters_for_command()`, not `load_all_filters()`.** A
prefilter narrows candidates before any regex compiles; `find_filter` matches via
`derive_command_candidates()`. Measured on Windows with 528 bundled + 231 user
filters, the hook stays in single-digit milliseconds.

Engine invariants: `never_worse` (a filtered result never costs more bytes than
raw) · `head_lines`+`tail_lines` form a first+last window with an inline
`[... N lines omitted ...]` marker · `priority_lines` survive every sizing cut ·
`category_caps` bound repetitive classes with a count marker · `apply_filter_with_exit`
honors per-filter `on_failure = "passthrough"|"tail:N"` · `FilterDef` is
`deny_unknown_fields` so typo'd keys fail loudly.

`on_empty` and `passthrough_when_emptied` **compose** — 94 bundled filters ship
both, and that is the recommended shape for a silent-on-success tool. The
`apply_filter_with_exit` fallback is gated on `!output.trim().is_empty()`, so
passthrough only takes over when filtering emptied *non-empty* output.

Repo-local filters are trust-gated (`tokenix trust`, SHA-256 in
`~/.tokenix/trusted_filters.json`) and skipped until approved. Failed commands
with clipped output tee raw text to `~/.tokenix/tee/` with a `[full output: path]`
hint (`TOKENIX_TEE=0` disables). `TOKENIX_DISABLED=1` bypasses the hook for one
command (logged as `action="bypassed"`).

**Adding a filter:** create `assets/filters/<slug>.toml` with **≥2 embedded
`[[tests.<name>]]` golden cases** (enforced by
`bundled_filters_require_minimum_tests`). rust-embed picks it up automatically.
Homologate with `cargo test --bin tokenix filters::tests::` — golden, ≥70% economy,
never-mask-failure, and no-inflate all run there.

Full per-filter homologation (slower, opt-in):

```bash
cargo test --release --bin tokenix homologate -- --ignored --nocapture
```

## MCP proxy contract

`tokenix mcp-proxy [--name X] -- <server cmd>` wraps a stdio MCP server and
compresses **only** `tools/call` text results — the sole reachable path for MCP
output.

`tools/list` schemas are **never touched**. That is deliberate: the client, not
the proxy, serializes `inputSchema` into the prompt, so schema re-serialization is
not implementable here; Claude Code already defers MCP tool schemas by default;
and collapsing many tools behind one meta-tool would destroy per-tool permissions
and human-in-the-loop review. Schema compression is `mcp-compressor`'s trade, not
ours.

## Recall

Clipped output is stashed so nothing is unrecoverable: `tokenix retrieve <key>`
prints the exact original body a compressed run saved. Keys are validated and come
from a `[tokenix: ...]` marker in the compressed output. Re-read suppression uses
**exact hashes only** — fuzzy/similarity dedup is refused on purpose, because
hiding *altered* output makes the agent act on a reality that no longer exists.

## One interface

A bare `tokenix` or any human report command on a TTY opens the ratatui shell on
that command's tab. `should_open()` is the single TTY/`--no-tui` gate;
`run_entry(Entry)` seeds the tab and scope. Piping, `--json`, `--statusline`,
`--format`, `--output`, and any flag a tab cannot represent keep plain output.
Agent-facing commands (`hook`, `run`, `mcp`, `query`, `read`, `pack`) never open a
UI. Every data-loading tab loads on a background thread behind one shared spinner
(`draw_loading` / `spinner_frame`); only Index runs as a foreground drop-out
because it needs the child's own progress bar.

## Common tasks

**New tree-sitter language:** `Lang` enum + `detect_lang` + `is_<lang>_symbol()` +
dispatch in `chunker.rs`, reference arm in `graph.rs`, fixture tests. Watch
per-grammar identifier node kinds (`constant` Ruby, `name` PHP,
`simple_identifier` Kotlin/Swift) in `find_first_identifier`.

**New embedding model:** append a `ModelSpec` to `embed.rs::MODELS`. Built-in uses
`ModelSource::BuiltIn`; custom uses `ModelSource::Custom { hf_repo, onnx_file, pooling }`
(downloaded to `<model_cache>/custom/<id>/`). The active model is **stamped in the
index `meta`** and read back by query/hook/daemon, so vectors always match. It is
sticky across re-indexes; an explicit switch forces a full re-embed. Cache keys are
namespaced by model id.

**New secret rule:** `assets/secret-rules/*.toml`, `[[rules]]` with `id`,
`pattern`, optional `capture` / `min_entropy`. **`min_entropy` must be reachable at
the pattern's minimum match length** — Shannon entropy over the observed
distribution is bounded by `log2(n)`, so a floor above `log2(min_len)` silently
disables the rule. Ship a true-positive *and* a near-miss negative test.

## Testing the hook

```bash
tokenix index .
echo '{"tool_name":"Read","tool_input":{"file_path":"src/main.rs"}}' | tokenix hook; echo $?   # 2
echo '{"tool_name":"Read","tool_input":{"file_path":"Cargo.toml"}}' | tokenix hook; echo $?    # 0
echo '{"tool_name":"Grep","tool_input":{"pattern":"how does embedding work"}}' | tokenix hook  # 2
echo '{"toolName":"view","toolArgs":"{\"path\":\"src/main.rs\"}"}' | tokenix hook              # Copilot shape
tokenix gain --history
```

## Release

`feat:` / `fix:` / `perf:` on `main` auto-cuts a public GitHub release and bumps
`Cargo.toml`. Use `test:` / `ci:` / `chore:` to avoid one. CI runs `cargo fmt`
first, so an unformatted push fails before anything else. Never quote GitHub's
auto-skip phrase in a commit body — it skips every workflow.
