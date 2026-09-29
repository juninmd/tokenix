# AGENTS.md — tokenix

Rust CLI that gives AI coding agents compact repository context: a local
SQLite index (chunks + tree-sitter symbol graph; no embeddings, no model, no text search), deterministic output filters,
and `PreToolUse` hooks for Claude Code / Copilot / Codex / Antigravity (OpenCode
is MCP-only: no hook wiring is installed for it).
User-facing docs live in `README.md`; this file is the engineering contract.

## Build & test

```bash
cargo build --release
cargo test --bin tokenix          # 449 unit + golden tests
cargo test --tests                # + 39 end-to-end tests (one Windows-only) against the real binary
cargo fmt --check                 # CI runs fmt FIRST — run it before pushing
cargo clippy --all-targets --locked -- -D warnings
./scripts/verify.sh [--models]    # every CI gate locally, in CI order
```

`scripts/verify.sh` runs fmt → clippy → fuzz-target type-check → tests → release
build → the three homologation scripts, under a throwaway `TOKENIX_HOME`.

End-to-end tests live in `tests/`: `workflow_e2e.rs` (index → stats → symbol
graph → read/outline → Read/Grep hook → pack), `safety_e2e.rs` (exit code kept,
trust gate incl. revocation on edit, memory refusing credentials, inline
freshness), `hook_e2e.rs` (hook JSON contract per agent) and `mcp_proxy_e2e.rs`.
`tests/common::Sandbox` gives every test its own git repo and `TOKENIX_HOME`;
new e2e tests must use it (or set `TOKENIX_HOME`), never the developer's home.
There is no model and no network in the test suite.
Assert behavior a user relies on (exact symbol location, exit code, budget
held), not "did not panic" — that is what `homologation.sh` already covers. Every machine-wide path
derives from `store::global_dir()`, which honours an **absolute** `TOKENIX_HOME`
(relative values resolve per-cwd and are ignored); never build
`~/.tokenix` by hand, or checks and tests start writing to the user's real home.

MSRV is `1.90` (`rust-version` in Cargo.toml). The floor is not cosmetic:
`Command` only quotes arguments safely for Windows `.cmd`/`.bat` shims from
1.77.2 on (CVE-2024-24576), and `cmd_filter` spawns exactly those shims with
repo-controlled argv. It must match the highest `rust-version` in the locked
tree (tree-sitter 0.27 needs 1.90); it drifted to a false 1.88 because CI
builds on stable only. Check with `cargo +1.90.0 check --locked --all-targets`,
and update with `CARGO_RESOLVER_INCOMPATIBLE_RUST_VERSIONS=fallback cargo update`
so resolution never silently raises it.

## Key files

| File | Role |
|---|---|
| `chunker.rs` | Symbol-aware chunking, `count_tokens`, outline generation, `enforce_token_cap` |
| `indexer.rs` | Walks + indexes files. `filter_entry` (dirs) vs `should_index` (files) are separate on purpose |
| `store.rs` | SQLite access, `index_staleness`, graph tables, hook log, `global_dir()`, `find_project_root` |
| `graph.rs` | Symbol graph, PageRank, Tarjan SCC cycles, import graph, repo hotspots |
| `freshness.rs` | Inline pre-command refresh of dirty files (inline path), fails open |
| `modules.rs` | Louvain community detection over `graph_edges` — `tokenix modules` |
| `blast.rs` | Diff → changed symbols → reverse call graph (`tokenix blast`) |
| `snapshot.rs` | `export-index` / `import-index` — gzipped `VACUUM INTO` copy for teams; import caps decompressed size (2 GiB), holds the index lock and runs `integrity_check` before the swap |
| `hook.rs` | `PreToolUse` handler — the interception decision tree |
| `compress.rs` | Generic output compression, base64 redaction, token ceiling, EOL preservation; `run_hook_post` also redacts known secrets on every PostToolUse tool result via `secrets_scan::redact_known_secrets`, then emits `hookSpecificOutput.updatedToolOutput` for Claude Code/Codex |
| `filters.rs` | `FilterDef` schema, filter resolution, `apply_filter_with_exit` |
| `filters/trust.rs` | Trust gate for repo-controlled inputs: `repo_input_hashes`, `repo_inputs_trusted`, `set_repo_inputs_trust` |
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
| `conversation_audit.rs` | `conversation-audit` + `redact_credentials()` — the single credential masker every persisted view goes through |
| `recordings.rs` | `filter record` sessions — captures command output for filter authoring; redacted and self-gitignored, because captures land in the working tree and `filter generate` uploads them to an AI CLI |
| `memory.rs` | Cross-session notes read back with `memory list` / the MCP memory tools (no command injects them anymore); refuses text that trips a keyword *or* a bundled secret rule (`secrets_scan::redact_known_secrets`) |
| `benchmark.rs` | Token-reduction benchmark (`tokenix benchmark`): Read outlines, symbol workflows, command filters |
| `doctor.rs` | Install diagnosis — filter inventory and config issues, recording state |
| `artifacts.rs` | Reads build artifacts referenced by agent output |
| `docshot.rs` | `#[cfg(test)]` only — renders README screenshots as SVG, never ships in the binary |
| `tui.rs` | Ratatui shell — the only human interface |
| `self_update.rs` | Self-update from GitHub releases: SHA-256 validation, atomic binary replacement, background auto-update and update caching |

The interactive CLI startup triggers the background update check before the bare
TUI opens. It is limited to terminal, human commands: no network or cache work in
hooks, MCP, pack/read, piped output or CI. The check is cached for 24 hours;
the installed binary changes for the next invocation. A development binary under
`target/` may check and notify but must never silently overwrite the per-user
binary. Keep this behavior documented in `README.md`.
The GitHub token belongs only on the release API request, whose client forbids
redirects. Asset URLs must be HTTPS links to this repo's release path and use an
unauthenticated client; both the asset and `sha256sums.txt` are checked before a
binary replacement. Downloads are capped: `sha256sums.txt` at 64 KB, the binary at min(`asset.size`, 512 MB).

## SQLite schema

```sql
files(id, path UNIQUE, mtime, content_hash)
chunks(id, file_id, path, start_line, end_line, symbol, kind, content, token_count)
graph_nodes(chunk_id PK, file_id, path, name, kind, start_line, end_line, rank)
graph_edges(id, caller_chunk_id, callee_chunk_id, reference, edge_kind)
graph_imports(id, source_path, target, resolved_path, kind, line)  -- NULL = external
meta(key PK, value)                          -- 'indexed_at', git fingerprint
```

`meta` holds `indexed_at` plus a Git fingerprint (worktree root + branch + HEAD);
a different fingerprint counts as stale so branch switches never reuse context.
`snapshot_version` / `snapshot_created_at` are stamped by `tokenix export-index`.

Indexes built before embeddings were removed still carry `embeddings` and
`embedding_cache` tables (and `ne:`-prefixed hashes from `--no-embed` runs).
`init_schema` drops the tables (and VACUUMs) on the next `tokenix index`; the `ne:` hashes
never match a real hash, so those files are simply re-chunked once.

Only the inline refresh (`IndexOptions.inline`) skips applying deletions; an
every explicit `index` applies them. A git rename (`R`) records its origin
as deleted (a copy, `C`, does not), and a file that still exists but now yields
no chunks (emptied, binary, sub-minimum) has its row and chunks dropped rather
than left serving the old body.

The inline refresh (`freshness.rs`) counts a dirty file the index does not know
only if `indexer::stores_content` says indexing it would write a row. Empty,
binary and sub-`MIN_CHUNK_TOKENS` files never get one, and counting them made
every index command pay a refresh, forever. The git-incremental plan applies
the same `max_file_bytes` cap as the full walk (`within_size_cap`); it used to
index oversized dirty files the walk skips.

**Query paths open old DBs without migrating.** They never touch the legacy
vector tables, so an old index answers normally until it is re-indexed.

Project root = nearest ancestor that **was indexed** (`<id>.db` or `<id>.name` in
`global_dir()`) or carries a marker (`.git`, `Cargo.toml`, `package.json`, …).
The index check comes first: `tokenix index <dir>` roots at `<dir>`, and a
`package.json` in a parent (a home directory, often) used to capture every
marker-less repo below it, so `stats`/`symbols`/the hook never found its index.

Hook log: `~/.tokenix/<project-id>.log`, NDJSON, one `HookEvent` per line, rotates
at 5 MB (one generation). Fallback is repo-local `.tokenix/hook.log`.

## Intercept logic

```
Read:  < 200 lines OR offset/limit set → exit 0 (pass)
       ≥ 200 lines, no offset/limit    → outline, exit 2 (intercept), only for
         code extensions (`is_code`) and when the outline saves ≥ 30%;
         otherwise the file passes through whole

Grep:  never answered by tokenix (an index answer hid textual matches of
       the identifier); passes through, and only
         output_mode="content" without head_limit → updatedInput injects
         head_limit (TOKENIX_GREP_HEAD_LIMIT, default 100, 0 disables);
         logged saved_tokens=0 because the unbounded output never ran

Bash / PowerShell: matches a filter → rewrite to `tokenix run`
       (PowerShell: `& 'exe' run --shell pwsh '<cmd>'`, re-executed under pwsh;
       Bash on Windows: re-executed under the Git Bash in `MSYSTEM`/`EXEPATH`/
       `CLAUDE_CODE_GIT_BASH_PATH`, `cmd /C` only outside one — cmd broke
       heredocs, `$(...)`, `2>/dev/null` and `echo` in a Haiku benchmark)
       otherwise → exit 0

Index stale → Grep still gets its head_limit cap, then exit 0 for every tool.
```

Staleness is **not** age-based: missing DB, missing `indexed_at`, or a changed
git fingerprint.

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
```

## Critical rules

**Never lose content.** The chunker stores 100% of every indexed file. Generic
files (.md, .txt, .yaml, .json) use `clean_generic_text()` — full content,
formatting stripped. Truncated previews are forbidden. Files are *skipped* whole,
never truncated, above `max_file_bytes` (1.5 MB default) or when binary (NUL
sniff). The `MIN_CHUNK_TOKENS` floor applies only to anonymous line blocks:
named symbols and module-level pieces (`fn tiny() {}`, `use std::fmt;`) are kept
at any size. Oversized chunks split on line boundaries by tokens, and only a
single over-long line is byte-split, each piece keeping the line it lives on.
Tree-sitter parses Rust, Python, JS/TS, Go and C/C++; VB and SQL use
line-based symbol chunking; everything else is generic line chunking.

**Never break hook fallback.** `run_hook()` must `exit(0)` on any error — missing
index, stale index, parse failure, index error. Breaking a session is worse than
missing a saving. This covers **panics too**: `main::guard_hook` wraps every hook
entry point in `catch_unwind`, because an unwind used to exit 101 (and skip
Antigravity's `decision:allow`), which is the one outcome the contract exists to
prevent. The MCP server has the same guard per `tools/call`
(`mcp::call_tool_guarded`) — one bad request must not take the session's server
down with it.

**No text search, no answer in place of the agent's grep.** `tokenix query`,
`context`, `explore`, `grep`, the FTS5 table and the Grep identifier lookup were
removed on purpose: an index answer stands in for the agent's own search and hides
what it would have found, and no lexical or embedding retrieval here was shown to
beat plain grep. What stays is what an agent cannot get from grep — the symbol
graph (`symbols`, `callers`, `callees`, `impact`, `flow`, `blast`, `modules`),
outlines for big files, `pack`, and output filters. `init_schema` drops the
`chunks_fts` table **and its triggers** from older indexes (a surviving trigger
would fail every chunk write); the inbound-edge repair in `graph.rs` finds
candidate callers with an `instr` scan, uncapped. Do not reintroduce a retrieval
command without an A/B measurement against the agent's native grep.

**Hook exit codes:** `0` = pass through · `2` = block tool (stderr becomes the
agent's context). Never exit `1`.

**Nothing persisted carries a raw credential.** `store::log_hook_event`
(`command`, `input_preview`) and the failure tee run through
`conversation_audit::redact_credentials` before writing; `~/.tokenix` files and
directories are created `0600`/`0700` (`store::restrict_to_owner`, no-op on
Windows). The one exception is `recall`'s blobs: `retrieve` promises the exact
original bytes and `find_identical` verifies against them, so those are protected
by permissions only. Add a masking rule in **one** place — `redact_credentials`
— and every writer inherits it.

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

**Cross-platform paths:** `tokenix_bin_path()` normalizes to forward slashes for
shell/JSON config strings.

**Hook log format:** do not move away from NDJSON without updating `gain.rs`.

**Token count is approximate.** `count_tokens()` = `chars / 4` rounded up —
**chars, not bytes**, so non-ASCII is not charged 2–4×. Deliberate approximation;
a real BPE tokenizer is roadmap, not a dependency yet.

**`gain` reports tokens, not money.** Tokens removed from a payload is a token
measurement. Published work found token reduction and provider-billed cost are
close to uncorrelated (r = 0.15) because cache traffic dominates a real bill. Do
not add a savings-in-dollars headline until cache-aware accounting lands. Dollar
figures live only behind explicit opt-ins (`gain --cost-estimate`,
`gain --economics`, the TUI's `c` table); the Gain tab's headline used to print
"≈ $X saved at Sonnet input rates" and no longer does. See
`docs/research/2026-08-token-economy.md`.

**`gain`'s denominator includes the runs that saved nothing.** Every command that
goes through `tokenix run` is logged — `action="intercepted"` when it shrank,
`action="pass"` when it did not — and `pct_saved` divides by both. Logging only
the wins made the headline "how much do we save when we save", a number that
cannot go down. Never reintroduce a `if saved > 0` guard around `log_hook_event`.

**`gain` reports session shape alongside tokens.** `gain::session_stats`
sessionizes the hook log by time gaps (no session id is stored) and reports
sessions, mean calls/session, and within-session re-requests of the same
tool+subject — the measurable share of the "+13.8% turns" mechanism by which
compression savings invert (arXiv 2607.12161). These are diagnostics: without a
holdout control group no causal Δturns claim is possible, so `SessionStats` must
stay descriptive and never feed a headline number.

**`tokenix run --raw` / `TOKENIX_RAW=1`** — byte-exact passthrough, no
compression/dedup/stash, via `compress::run_command_raw` (`Stdio::inherit`,
shares `build_shell_command` with the compressed path so the two invocations
can't drift). Closes the "shell-pipeline corruption" hazard from
2607.12161 (filtered output silently wrong when a script expects raw text) as
an **explicit opt-out**, not auto-detection: the agent harness's own capture of
`tokenix run`'s stdout is a pipe, identical in signal to a script piping it
into `jq`/`grep`/etc., so `isatty` cannot tell the two apart. Do not attempt to
infer "raw" from environment heuristics — same guard-safety rule as the
`never_mask_failure` filter invariant.

**Keep docs in sync.** Every new or changed user-facing feature MUST update both
`README.md` (Commands, and any affected section) and this file in the same change.
A change to what `install-hook` writes, or to how an agent's payload is parsed,
also updates that agent's guide in `docs/agents/`. Every command and payload
shown there was run against the real binary; keep it that way.

## Output filters

Resolution: `<repo>/.tokenix/filters` (trust-gated) → `~/.tokenix/filters` →
bundled. Currently **528 filters / 1,150 golden cases**.

**Hot path uses `load_filters_for_command()`, not `load_all_filters()`.** A
prefilter narrows candidates before any regex compiles; `find_filter` matches via
`derive_command_candidates()`. Measured on Windows with 528 bundled + 231 user
filters, the hook stays in single-digit milliseconds.

Engine invariants: `never_worse` (a filtered result never costs more bytes than
raw) · `head_lines`+`tail_lines` form a first+last window with an inline
`[... N lines omitted ...]` marker · `priority_lines` survive every sizing cut ·
`category_caps` bound repetitive classes with a count marker · `apply_filter_with_exit`
honors per-filter `on_failure = "passthrough"|"tail:N"` · `FilterDef` is
`deny_unknown_fields` so typo'd keys fail loudly · every regex must compile
(`filters::regex_issues`; `every_bundled_filter_regex_compiles` gates the corpus,
`doctor` reports user/local ones) — `cached_regex` skips a bad pattern with a
warning, so seven bundled rules (lookarounds, bare `{`/`)`) had silently never run.

stderr only reaches a filter with `filter_stderr = true`; otherwise it gets the
generic compressor. rustc writes every diagnostic to stderr, so the cargo
build/check/clippy/test filters set it and use `block_caps` (applied before
`strip_lines_matching`, since a blank line ends a block) to keep a warning's
message + location and drop its snippet — errors are never capped. A competitor
benchmark measured 0% reduction on `cargo clippy` before this.

`extract_sections` with `split_on_start = true` treats each start line as the next
record's header (`git log`), and `max_lines` caps each section with a count
marker. Without `split_on_start`, a start line inside an open section is content
(`npm ERR!` runs). `git-log` keeps 12-char hashes: replacing them with
`<commit_hash>` left the agent nothing to `git show`. `git-diff` keeps `@@` hunk
headers — the only line numbers an agent can edit by.

Unit tests (`cfg(test)`) default `global_dir()` to a per-process temp dir: the
developer's `~/.tokenix/filters` shadowed bundled filters and made results
machine-dependent.

`on_empty` and `passthrough_when_emptied` **compose** — 94 bundled filters ship
both, and that is the recommended shape for a silent-on-success tool. The
`apply_filter_with_exit` fallback is gated on `!output.trim().is_empty()`, so
passthrough only takes over when filtering emptied *non-empty* output.

Repo-controlled inputs are trust-gated (`tokenix trust`, SHA-256 in
`~/.tokenix/trusted_filters.json`) and skipped until approved. The gate covers
everything a clone can put in the tree that tokenix acts on:
`.tokenix/{filters,secret-rules,egress-rules}/*.toml`, `.mcp.json`,
`opencode.json`, `.vscode/mcp.json` — see `filters::repo_input_hashes`, which is
the single list the gate is defined by. `mcp_audit` introspects a server by
*spawning* it, so a repo-declared server reports
`unknown (declared by this repo — not spawned)` until trusted; user-scoped
configs are never gated. Repo rule files load only once trusted
(`secrets_scan::repo_rule_specs`, `egress_scan::repo_rule_specs`) — an added `.+`
rule would make the PostToolUse redactor blank every tool result — and are
additionally add-only (`secrets_scan::reject_overrides`): a repo id colliding
with a bundled one is dropped with a warning, so a clone cannot redefine
`aws-secret-access-key` into a regex that matches nothing. Failed commands
with clipped output tee raw text to `~/.tokenix/tee/` with a
`[full output (credentials masked): path]` hint (`TOKENIX_TEE=0` disables). The
hint says "masked" on purpose — the file is redacted, so the agent must not read a
`[REDACTED]` as the literal value. `TOKENIX_DISABLED=1` bypasses the hook for one
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

The key is advertised on the **first** clipped run, not only on a later dedup hit:
a successful command that dropped ≥ `RECOVERY_HINT_MIN_DROPPED_BYTES` (500) gets
`[tokenix: N bytes not shown — tokenix retrieve <key> ...]` appended. Before that,
the raw blob was stashed and the key thrown away, so the single output most worth
recovering — a big first result the caps just trimmed — had no way back except
re-running the command. Failures take the tee path instead.

Cross-call dedup is scoped to the **project**, the **command**, and to
`TOKENIX_DEDUP_TTL` (default 3600 s, `recall::reusable`). The index lives in
`~/.tokenix` and outlives both the session and the checkout, and the marker
claims the output "is already in this conversation" — a hit from another
repository, or from yesterday, cannot honour that. The command scope exists
because the compressed-output digest alone is not a safe match key: two
genuinely different commands can produce byte-identical compressed text, and
matching on content only made the marker name the *wrong* command while the
real command's own raw output was never stashed (`remember()` is skipped on a
dedup hit) — `tokenix retrieve` then handed back bytes from a command the
agent never ran. Entries written before either scoping field existed
deserialize with an empty `project`/`command_key` and therefore never match.
Re-read suppression keeps its own 900 s TTL
(`TOKENIX_READ_DEDUP_TTL`) and is scoped to the agent's `session_id` for the same
reason: `read_marker` claims the file "is already in this conversation", and
`recent_reads.json` is machine-wide, so a second session started minutes later
was being handed that claim — and denied the content — for bytes it never saw.
Agents that send no session id compare equal to each other, so their behavior is
unchanged.

## One interface

A bare `tokenix` or any human report command on a TTY opens the ratatui shell on
that command's tab. `should_open()` is the single TTY/`--no-tui` gate;
`run_entry(Entry)` seeds the tab and scope. Piping, `--json`, `--statusline`,
`--format`, `--output`, and any flag a tab cannot represent keep plain output.
Agent-facing commands (`hook`, `run`, `mcp`, `read`, `pack`) never open a
UI. Every data-loading tab loads on a background thread behind one shared spinner
(`draw_loading` / `spinner_frame`); only Index runs as a foreground drop-out
because it needs the child's own progress bar.

## Common tasks

**New tree-sitter language:** `Lang` enum + `detect_lang` + `is_<lang>_symbol()` +
dispatch in `chunker.rs`, reference arm in `graph.rs`, fixture tests. Watch
per-grammar identifier node kinds in `find_first_identifier` — grammars differ
(`constant` in Ruby, `name` in PHP, `simple_identifier` in Kotlin/Swift; none of
those are wired yet).

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
echo '{"tool_name":"Grep","tool_input":{"pattern":"how does indexing work"}}' | tokenix hook  # 0 (not intercepted)
echo '{"toolName":"view","toolArgs":"{\"path\":\"src/main.rs\"}"}' | tokenix hook              # Copilot shape
tokenix gain --history
```

## Project config

`.tokenix.toml` (or `tokenix.toml`) at the project root, `[hook]`, `[index]`,
and `[update]` sections. All are `deny_unknown_fields`, and a parse error is reported on stderr
instead of silently falling back to defaults — the previous `.ok()` swallow left
users convinced a misspelled `read_min_lines` was active. A bad config still
degrades to defaults rather than failing the hook.

## CI topology

| Workflow | Trigger | Gate? |
|---|---|---|
| `rust.yml` | push/PR | fmt + clippy (Linux), `cargo test` on **Linux + Windows** |
| `homologation.yml` | push/PR | release build, CLI smoke, hook/Copilot scripts, aarch64 sanity |
| `supply-chain.yml` | push/PR + weekly | cargo-deny, zizmor, install-path egress audit |
| `security.yml` | push to main | gitleaks (reusable workflow, SHA-pinned) |
| `release.yml` | push to main | CI gate on **Linux + Windows**, then bump → build → attest → release → crates.io |
| `scorecard.yml` / `release-drafter.yml` / `cflite_pr.yml` | — | posture, notes, fuzzing |

The release CI gate runs the Windows leg too. It did not, and `release.yml` fires
independently of `rust.yml`, so a Windows-only failure still shipped binaries —
on the platform where the PowerShell rewriting, cmd quoting and `C:\` handling are
the only code paths that execute.

## Release

`feat:` / `fix:` / `perf:` on `main` auto-cuts a public GitHub release and bumps
`Cargo.toml`. Use `test:` / `ci:` / `chore:` to avoid one. The type prefix is read
from the commit **subject** only (`%s`); the body (`%b`) is scanned solely for
`BREAKING CHANGE:`, so quoting "fix: …" inside a body no longer cuts a release.
CI runs `cargo fmt` first, so an unformatted push fails before anything else. Never
quote GitHub's auto-skip phrase in a commit body — it skips every workflow.
