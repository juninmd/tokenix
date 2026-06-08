<div align="center">
  <img src="tokenix-logo.png" alt="tokenix logo" width="450" />

  <h1>tokenix</h1>

  <p><strong>Local semantic context for AI coding agents, with fewer wasted tokens.</strong></p>

  <p>
    <a href="https://github.com/juninmd/tokenix/releases"><img src="https://img.shields.io/github/v/release/juninmd/tokenix?style=flat-square&color=orange&label=release" alt="Latest Release" /></a>
    <a href="https://crates.io/crates/tokenix"><img src="https://img.shields.io/crates/v/tokenix?style=flat-square&color=orange" alt="crates.io" /></a>
    <a href="https://crates.io/crates/tokenix"><img src="https://img.shields.io/crates/d/tokenix?style=flat-square&color=orange&label=downloads" alt="crates.io downloads" /></a>
    <a href="https://github.com/juninmd/tokenix/stargazers"><img src="https://img.shields.io/github/stars/juninmd/tokenix?style=flat-square&color=yellow" alt="GitHub stars" /></a>
    <a href="https://github.com/juninmd/tokenix/actions/workflows/rust.yml"><img src="https://img.shields.io/github/actions/workflow/status/juninmd/tokenix/rust.yml?branch=main&style=flat-square&label=CI" alt="CI" /></a>
    <a href="https://github.com/juninmd/tokenix/actions/workflows/supply-chain.yml"><img src="https://img.shields.io/github/actions/workflow/status/juninmd/tokenix/supply-chain.yml?branch=main&style=flat-square&label=supply%20chain" alt="Supply Chain" /></a>
    <a href="https://scorecard.dev/viewer/?uri=github.com/juninmd/tokenix"><img src="https://img.shields.io/ossf-scorecard/github.com/juninmd/tokenix?style=flat-square&label=scorecard" alt="OpenSSF Scorecard" /></a>
    <a href="https://github.com/juninmd/tokenix/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue?style=flat-square" alt="License" /></a>
    <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/built%20with-Rust-orange?style=flat-square&logo=rust" alt="Built with Rust" /></a>
    <img src="https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20Windows-lightgrey?style=flat-square" alt="Platforms" />
  </p>

  <p>
    <a href="#-quick-install">Install</a> ·
    <a href="#-how-it-works">How it Works</a> ·
    <a href="#-usage">Usage</a> ·
    <a href="#-setup-by-tool">Setup</a> ·
    <a href="#-commands-reference">Commands</a> ·
    <a href="CONTRIBUTING.md">Contributing</a>
  </p>
</div>

---

> **tokenix** is a local-first Rust CLI that helps AI coding agents understand a repository without dumping huge files into the prompt. It indexes your code, finds relevant chunks by meaning, returns compact file outlines, and can hook into AI tools to replace noisy reads and command output with smaller, more useful context. Works with Claude Code, GitHub Copilot, OpenAI Codex CLI, Gemini, and any MCP client. **No Ollama or external server required.**

```
Without tokenix:  Read(src/auth/middleware.rs) → 800 lines → ~2,400 tokens  (illustrative)
With tokenix:     tokenix read src/auth/middleware.rs → symbol outline → ~180 tokens
```

Savings depend on codebase size, AI behavior, and file sizes. Run `tokenix gain` to see your real numbers.

---

## What Is tokenix?

AI coding agents often waste context on the wrong shape of information: entire files, long grep output, repeated build logs, and directory listings that are much larger than the useful signal inside them. tokenix is a context layer between the agent and your repository.

It does four jobs:

| Job | What tokenix does | Why it matters |
|---|---|---|
| **Index the repository** | Walks source files, splits them into symbol-aware chunks, and stores local embeddings in SQLite | The agent can search by intent instead of opening files blindly |
| **Read files compactly** | Returns outlines, symbols, or line ranges instead of full files when possible | Large files stop consuming thousands of unnecessary tokens |
| **Intercept assistant tools** | Hooks into supported tools before large reads and rewrites noisy command output | Optimization happens automatically during normal AI sessions |
| **Measure savings** | Logs hook decisions and estimates token/cost reduction with `tokenix gain` | You can see whether it is actually helping on your codebase |

tokenix is not a cloud service, not a vector database server, and not a replacement for your AI assistant. It is a local repository index plus a set of CLI and hook integrations that make the assistant's context smaller and more targeted.

---

## ⚡ Quick Install

### Pre-built binary (recommended)

Every release ships a binary per platform. The version-less
`releases/latest/download/<asset>` URL **always resolves to the newest release**,
so you never pin a version.

| Platform | Asset |
|---|---|
| Linux x86_64 | `tokenix-linux-x86_64` |
| Linux arm64 | `tokenix-linux-aarch64` |
| macOS x86_64 | `tokenix-macos-x86_64` |
| macOS arm64 (Apple Silicon) | `tokenix-macos-aarch64` |
| Windows x86_64 | `tokenix-windows-x86_64.exe` |
| Windows x86_64 (GPU / DirectML) | `tokenix-windows-x86_64-directml.exe` |

Each release also publishes `sha256sums.txt` and SLSA build-provenance
attestations — see [SECURITY.md](SECURITY.md) for how to verify a download.

**macOS / Linux** — swap the asset for your platform from the table:

```bash
curl -fsSL https://github.com/juninmd/tokenix/releases/latest/download/tokenix-linux-x86_64 -o tokenix
chmod +x tokenix && sudo mv tokenix /usr/local/bin/tokenix
tokenix doctor   # verify
```

**Windows (PowerShell):**

```powershell
irm https://github.com/juninmd/tokenix/releases/latest/download/tokenix-windows-x86_64.exe -OutFile tokenix.exe
```

> **🤖 For AI agents / LLMs:** prefer this prebuilt binary over `cargo install` (no Rust
> toolchain, no compile step). Always fetch the version-less URL
> `https://github.com/juninmd/tokenix/releases/latest/download/tokenix-<platform>` — it
> redirects to the latest release, so **never hard-code a version**. Detect the platform,
> download the matching asset from the table, mark it executable, then run `tokenix doctor`
> to confirm. The embedding model downloads automatically on first use.

### From crates.io

```bash
cargo install tokenix --locked
```

### From source

```bash
git clone https://github.com/juninmd/tokenix
cd tokenix
cargo install --path . --locked
```

> **Use `--locked`.** It builds against the committed `Cargo.lock`; without it `cargo install` re-resolves dependencies and can pull an incompatible `ureq` into the `ort-sys` build script.

> **Requirements:** a recent stable [Rust](https://www.rust-lang.org/tools/install) toolchain (edition 2021). No Ollama, no Python, no external services.

The embedding model (`nomic-embed-text-v1.5`, ~130 MB) is downloaded automatically on first use and cached locally.

---

## ✨ Features

| Feature | Description |
|---|---|
| **Semantic search** | Find relevant code by meaning, not just keywords (`tokenix query`); cross-project with `--link` |
| **Context artifacts** | `tokenix artifacts` indexes non-code schemas, API docs, and specs via `.tokenix/artifacts.json` |
| **Hybrid ranking** | FTS5 BM25 + vector cosine + RRF fusion for ranked results |
| **Exact search** | Regex/literal search over indexed content, no embedding (`tokenix grep`) |
| **One-call task context** | `tokenix context` combines semantic search, entry points, and compact outlines with strict budget modes (`plan`, `debug`, `audit`, `security`, `review`) |
| **Graph-aware explore** | `tokenix explore` returns related symbols, relationship maps, and grouped source in one capped call |
| **Repository pack** | `tokenix pack` emits a budgeted, secret-safe repo map with changed-file packs, token maps, and safety reporting |
| **Symbol graph** | `tokenix symbols`, `callers`, `callees`, `impact`, `flow`, and `cycles` trace relationships, call-flow, and circular deps between indexed symbols |
| **Interactive HTML/Mermaid graphs** | `tokenix impact --format html\|mermaid` exports vis.js / Mermaid flowcharts; `tokenix flow --format mermaid` traces call flow |
| **Cycle detection** | `tokenix cycles` finds circular dependencies via Tarjan's strongly-connected components algorithm |
| **Token map** | `tokenix tokenmap` shows a directory tree with token counts per file/folder |
| **Preference memory** | `tokenix memory add/list` stores global and project preferences in editable Markdown; context/explore include saved preferences |
| **Dynamic language detection** | Map custom file extensions to any built-in parser via a project `.tokenix.toml` — no recompile needed |
| **Symbol-aware chunking** | AST Tree-sitter parsers for Rust, Python, TypeScript, JavaScript, Go, C/C++ |
| **Multi-agent safe index** | PID-based index lock prevents concurrent reindex; resumable checkpoints survive mid-index process kills |
| **Smart file reader** | Outlines large files; supports `--symbol` and `--lines` reads |
| **Hook-based interception** | `PreToolUse` intercepts large reads and rewrites noisy Bash commands before execution |
| **RTK-grade compression** | Fuzzy grouping, compact `git`/`cargo` filters, NDJSON/JSON compaction, and ANSI/Emoji stripping |
| **Local project filters** | Drop `.toml` files in `.tokenix/filters/` for project-scoped compression rules — highest priority over user and bundled filters |
| **Output filters** | 73 RTK-compatible TOML filters embedded in the binary — auto-applied to Bash output for `uv`, `cargo`, `terraform`, `ansible`, and more |
| **Filter generation** | `tokenix filter generate` writes a TOML filter for a command; `tokenix filter record` captures real output for richer generation |
| **GPU acceleration (opt-in)** | Build with `--features directml` (Windows) or `--features cuda` to run embeddings on GPU; GPU is used by default at runtime with automatic CPU fallback, or force CPU with `--only-cpu` |
| **Environment diagnostics** | `tokenix doctor` reports the compiled backend, detected GPU, CUDA/cuDNN status, model cache, and daemon |
| **Branch-aware indexing** | `TOKENIX_BRANCH_AWARE=true` isolates indexes per git branch |
| **In-memory daemon** | `tokenix serve` keeps model + index in RAM so repeated hook calls avoid reloading the model each invocation |
| **Graceful fallback** | Exits `0` on errors — your AI session is never broken |
| **Token budget** | Results fit within a configurable token budget (default `1200`) |
| **Savings analytics** | `tokenix gain` — token summary and by-tool histogram; `--cost-estimate` adds a per-model cost table (9 reference models across Anthropic / OpenAI / Google) |
| **Slim MCP profile** | `tokenix mcp --profile slim` exposes 3 meta-tools instead of the full tool surface for hosts that support progressive discovery |
| **MCP/prompt weight audit** | `tokenix prompt-audit --recommend --profile-impact` connects to configured MCP servers, tokenizes tool schemas, and shows full-vs-slim MCP savings |
| **Session audit** | `tokenix session-audit --cache-hygiene` combines index freshness, hook history, MCP/tool weight, and prompt-cache stability risks |
| **Local-first, no dependencies** | fastembed ONNX in-process — no Ollama, no server, no internet after first run |

---

## 🔌 Supported AI Tools

| Tool | Integration |
|---|---|
| [Claude Code](https://docs.anthropic.com/en/docs/claude-code) | `PreToolUse` hooks in `~/.claude/settings.json` or project `.claude/settings.local.json` |
| [GitHub Copilot](https://docs.github.com/en/copilot) | `.github/copilot-instructions.md` + VS Code-compatible `.github/hooks/hooks.json` |
| [OpenAI Codex CLI](https://help.openai.com/en/articles/11096431-openai-codex-cli-getting-started) | `~/.codex/hooks.json` for `PreToolUse` Bash rewrites + optional shell helpers |
| Gemini | `tokenix install-hook --tool gemini` |
| Any MCP client | `tokenix mcp` — Model Context Protocol server over stdin/stdout (`--tool mcp`) |

---

## 🚀 How It Works

tokenix has two modes:

1. **Manual mode**: run `tokenix query`, `tokenix read`, `tokenix context`, etc. directly when you want compact context.
2. **Hook mode**: install hooks so supported AI tools call tokenix automatically before large reads and before noisy Bash commands execute.

### Output compression (RTK mode)

tokenix includes output-filtering logic inspired by RTK (Rust Token Killer). It doesn't just truncate output; it understands the structure of common CLI tools.

- **Fuzzy grouping:** collapses hundreds of `Compiling…` or `Removing…` lines into a single summary line.
- **Structural compaction:** compacts pretty-printed JSON and NDJSON into single-line formats.
- **Signal preservation:** keeps error messages and summaries even when the middle of a log is truncated.

---

## 🛠 Usage

### 1. Index your repository

```bash
cd my-project
tokenix index .
```

> **First run:** the model (~130 MB) is downloaded automatically. Subsequent runs use the local cache.

### 2. Search

```bash
tokenix query "how does JWT validation work"      # semantic
tokenix query "database connection pooling" --budget 2000
tokenix grep "fn validate_token" --ignore-case    # exact regex/literal
```

### 3. One-call task context

```bash
tokenix context "fix login refresh token bug"
tokenix context "how does the indexer batch embeddings" --mode debug --budget 2000
tokenix context "review this auth change" --mode review --budget 1200
tokenix explore "run_hook hook_post compression" --budget 4000
```

### 4. Repository pack

```bash
tokenix pack --mode plan --budget 8000 --format markdown --token-map
tokenix pack --mode review --changed --budget 4000
tokenix pack --mode security --format json --output tokenix-security-pack.json
```

`pack` builds a stable repo map plus focused context for tools that cannot call
tokenix directly. It respects the index, skips obvious secrets and build output,
and supports `plan`, `debug`, `audit`, `security`, and `review` modes. Use
`--changed` or `--since <ref>` for compact review packs.

### 5. Smart file reader

```bash
tokenix read src/auth/middleware.rs                           # symbol outline
tokenix read src/auth/middleware.rs --symbol validate_token   # targeted
tokenix read src/auth/middleware.rs --lines 45-80             # line range
```

### 6. Symbol graph & maps

```bash
tokenix symbols validate_token
tokenix callers validate_token
tokenix callees run_hook
tokenix impact update_user --depth 2
tokenix impact update_user --format html --output update_user.html   # vis.js graph
tokenix tokenmap                                                     # token tree
tokenix rebuild-graph   # recompute relationships without re-embedding
```

### 7. Token savings analytics

```bash
tokenix gain                  # token summary + by-tool histogram
tokenix gain --history        # include per-call history
tokenix gain --cost-estimate  # add the per-model cost table
tokenix session-audit         # index + hook + MCP token-economy health
```

`tokenix gain --cost-estimate` prices the savings against 9 reference models
across Anthropic, OpenAI, and Google. Prices are shown with their collection
date (currently `2026-06-01`) so the numbers stay auditable.

### 8. Audit MCP / tool weight

```bash
tokenix prompt-audit                  # every agent that has MCP config
tokenix prompt-audit --agent claude   # one agent (claude|codex|copilot|antigravity)
tokenix prompt-audit --json           # machine-readable
tokenix prompt-audit --recommend      # include practical reduction advice
```

Discovers the MCP servers configured for each agent, connects to each one live
(`initialize` + `tools/list`), tokenizes the returned tool schemas, and warns
when too many servers/tools inflate the effective system prompt. The base system
prompt itself cannot be read by tools, so this is a **relative bloat estimate**:
the native-tool baseline is approximate and HTTP/SSE servers are shown as
`unknown`. Thresholds are overridable via `TOKENIX_AUDIT_WARN_TOKENS`,
`TOKENIX_AUDIT_WARN_SERVERS`, and `TOKENIX_AUDIT_WARN_TOOLS`.

For MCP hosts that support progressive discovery, run `tokenix mcp --profile slim`.
The slim profile advertises only `tokenix_context`, `tokenix_search_tools`, and
`tokenix_call`, reducing tool-schema tokens while preserving access to the full
tokenix capability set through the meta-tool path.

### 9. Competitive benchmark

```bash
tokenix benchmark --competitive
tokenix benchmark --competitive --json
```

`--competitive` adds a market-facing scorecard. It measures tokenix `context`
and `pack`, auto-detects optional CLI competitors such as Repomix and Aider when
they are installed, and prints a feature matrix against current market signals:

| Feature | tokenix | Market signal |
|---|---|---|
| Budgeted repo map | `pack` + strict-budget `context` | Aider repo-map; Repomix pack |
| Graph-aware context | `symbols`, `callers`, `callees`, `impact`, `flow`, `cycles` | Aider graph rank; Sourcegraph Cody Code Graph |
| Semantic index | Local fastembed + SQLite + FTS5 BM25 | Cursor/Continue embeddings; Augment Context Engine |
| Progressive MCP | `mcp --profile slim` + meta-tools | MCP progressive discovery |
| Output savings | PreToolUse command rewrite + filters | RTK-style shell output compression |
| Remote repo pack | Not yet | Repomix `--remote`; Cody remote context |
| Learned rerank model | Not yet; heuristic hybrid ranker | Continue rerank role; cloud IDE rerankers |

---

## 🔧 Setup by Tool

### Claude Code

```bash
tokenix install-hook --tool claude-code
```

Writes a `PreToolUse` hook to `~/.claude/settings.json` (or `.claude/settings.local.json` with `--local`). Large reads, semantic greps, and noisy Bash commands are intercepted automatically — no changes to your prompts needed. `hook-post` (`PostToolUse`) remains a compatibility handler, not a default Claude install, because it cannot replace the original tool output.

### GitHub Copilot

```bash
cd my-project
tokenix install-hook --tool copilot
git add .github/
git commit -m "chore: add tokenix context instructions"
```

Creates `.github/copilot-instructions.md` and `.github/hooks/hooks.json`.

### OpenAI Codex CLI

```bash
tokenix install-hook --tool codex
# bash / zsh
echo 'source ~/.codex/tokenix-init.sh' >> ~/.bashrc
# PowerShell
echo '. ~/.codex/tokenix-init.ps1' >> $PROFILE
```

Then use `tx-read` and `tx-query` as shell helpers. On Windows this also installs `~/.codex/hooks.json` and a PowerShell wrapper that forwards `PreToolUse` intercepts for Bash-like terminal tools (`Bash`, `run_in_terminal`) and normalizes `grep_search` to the same semantic path as `Grep`.

### All tools at once

```bash
tokenix install-hook --tool all
```

---

## 📖 Commands Reference

| Command | Description |
|---|---|
| `tokenix index [PATH]` | Index the repo at PATH (default `.`) |
| `tokenix query TEXT` | Semantic search over indexed chunks |
| `tokenix grep PATTERN` | Exact regex/literal search over indexed content (no embedding) |
| `tokenix context TEXT` | One-call task context: entry points, relevant source, compact outlines, strict budget modes |
| `tokenix explore TEXT` | Graph-aware exploration: entry points, relationships, grouped source |
| `tokenix pack` | Budgeted repo pack for non-hook AI tools (`--mode/--profile`, `--changed`, `--token-map`) |
| `tokenix memory add TEXT` | Save a preference (`--global` or `--project`) for future context |
| `tokenix memory list` | List global and project preferences |
| `tokenix memory remove TEXT` | Remove preferences matching text |
| `tokenix memory edit TEXT` | Replace preferences matching text |
| `tokenix read FILE` | Smart reader — outline for large files, full for small |
| `tokenix symbols QUERY` | Find indexed symbols by name or path |
| `tokenix callers SYMBOL` | Show symbols that call/reference a symbol |
| `tokenix callees SYMBOL` | Show symbols called/referenced by a symbol |
| `tokenix impact SYMBOL` | Bidirectional impact graph (`--format html\|mermaid` for vis.js graph or Mermaid flowchart) |
| `tokenix flow SYMBOL` | Forward call-flow trace from a symbol (`--depth`, `--format text\|mermaid`) |
| `tokenix cycles` | Detect circular dependencies in the symbol graph using Tarjan's SCC algorithm |
| `tokenix tokenmap` | Directory tree map with token counts (`--format html` supported) |
| `tokenix rebuild-graph` | Rebuild graph tables from existing chunks without re-embedding |
| `tokenix gain` | Token savings analytics (`--cost-estimate` adds a per-model cost table) |
| `tokenix prompt-audit` | Audit MCP/tool token weight across agents; warns on bloat (`--agent`, `--json`, `--recommend`, `--profile-impact`) |
| `tokenix session-audit` | Token-economy health check: index, hook events, MCP/tool weight, cache hygiene |
| `tokenix benchmark` | Reproducible token-savings and retrieval-quality benchmark (`--competitive`, `--json`) |
| `tokenix stats` | Index statistics (files, chunks, tokens, age) |
| `tokenix artifacts list` | List context artifacts defined in `.tokenix/artifacts.json` |
| `tokenix artifacts show NAME` | Show context artifact content |
| `tokenix serve` | Start the background embedding daemon (keeps model + index in RAM) |
| `tokenix stop` | Stop the background daemon |
| `tokenix doctor` | Diagnose embedding backend, GPU availability, model cache, and daemon |
| `tokenix filter list` | Show top Bash commands by tokens wasted (no filter yet) |
| `tokenix filter active` | Show active user and bundled output filters |
| `tokenix filter generate [CMD]` | AI-generate a TOML output filter for a command |
| `tokenix filter record [CMD]` | Record real command output for richer filter generation |
| `tokenix run -- CMD` | Run a command and compress its output through tokenix filters |
| `tokenix install-hook` | Install assistant hook/instructions (default `--tool all`) |
| `tokenix remove-hook` | Remove assistant hook/instructions (default `--tool all`) |
| `tokenix hook` | `PreToolUse` handler — intercepts large reads, semantic grep, and noisy Bash commands (called by AI tools) |
| `tokenix hook-post` | Legacy `PostToolUse` compatibility handler |
| `tokenix mcp` | MCP server exposing context, read/search, graph, and gain tools (`--profile slim\|full`) |

<details>
<summary>Selected flags</summary>

**Global**

| Flag | Description |
|---|---|---|
| `--only-cpu` | Force CPU embedding even on a GPU-enabled build (no-op on CPU-only builds) |
| `TOKENIX_BRANCH_AWARE=true` | Env var: suffix SQLite DB per git branch (isolate indexes per branch) |

**`tokenix index`** — `--force/-f`, `--cpu-profile <low\|default\|max>`, `--jobs N`, `--embed-batch N` (default 16 CPU / 64 GPU), `--if-stale`, `--path/-p`, `--model <id>`

**Embedding model** — default `nomic-v1.5`. Select another with `tokenix index --model <id>` or `TOKENIX_EMBED_MODEL=<id>`; run `tokenix doctor` to list available ids (`nomic-v1.5`, `bge-small`, `bge-base`, `minilm-l6`, `e5-small`). The model is stamped into the index and read back at query time, so search always matches what was indexed; it is sticky across re-indexes and an explicit switch re-embeds. `nomic-v1.5` (768d) is the quality default; `bge-small` (384d) indexes faster; `e5-small` is multilingual. Existing indexes keep working unchanged.

**`tokenix query`** — `--budget/-b` (1200), `--k` (20), `--file/-f`, `--link` (cross-project, repeatable), `--path/-p`

**`tokenix grep`** — `--limit/-l` (20), `--ignore-case/-i`, `--file/-f`, `--path/-p`

**`tokenix context`** — `--mode <plan\|debug\|audit\|security\|review>`, `--budget/-b` (1200), `--max-files`, `--budget-breakdown`, `--path/-p`

**`tokenix impact`** — `--depth/-d` (2), `--limit/-l` (50), `--format <text\|html\|mermaid>`, `--output/-o`, `--path/-p`

**`tokenix flow`** — `--depth/-d` (3), `--format <text\|mermaid>`, `--output/-o`, `--path/-p`

**`tokenix install-hook` / `remove-hook`** — `--tool <claude-code\|copilot\|codex\|mcp\|gemini\|all>` (default `all`), `--local` (claude-code only)

**`tokenix pack`** — `--mode/--profile <plan\|debug\|audit\|security\|review>`, `--budget N` (8000), `--format <markdown\|xml\|json>`, `--changed`, `--since REF`, `--token-map`, `--output/-o`

**`tokenix benchmark`** — `--budget N` (1200), `--competitive`, `--json`, `--refresh-index`, `--cases FILE`, `--compare-codegraph PATH`

**`tokenix prompt-audit`** — `--agent <claude\|codex\|copilot\|antigravity\|all>` (default `all`), `--json`, `--recommend`, `--profile-impact`

**`tokenix session-audit`** — `--json`, `--cache-hygiene`, `--path/-p`

**`tokenix mcp`** — `--profile <full\|slim>` (default `full`)

</details>

---

## 🧠 Supported Languages

| Language | Extensions | Symbol types |
|---|---|---|
| Rust | `.rs` | `fn`, `struct`, `enum`, `impl`, `trait`, `mod` |
| Python | `.py` | `def`, `async def`, `class` |
| TypeScript | `.ts`, `.tsx` | `function`, `class`, `interface`, `type`, arrow functions |
| JavaScript | `.js`, `.jsx`, `.mjs`, `.cjs` | `function`, `class`, arrow functions |
| Go | `.go` | `func`, `type` |
| C / C++ | `.c`, `.cpp`, `.h`, `.hpp`, `.cc`, `.cxx` | `function`, `class`, `struct`, `namespace` |
| Config / Docs | `.toml`, `.md`, `.txt`, `.sh`, `.bash` | line blocks |
| Data files (opt-in) | `.json`, `.yaml`, `.yml` | Indexed only when `data_files = true` in `.tokenix.toml` |
| **Custom** | any extension | Mapped to an existing parser via `.tokenix.toml` |

Languages without a symbol-aware chunker (Java, C#, Ruby, Swift, Kotlin, …) are not indexed by default — blind line-block chunking produces low-quality search results.

### Custom language mapping

Create a `.tokenix.toml` (or `tokenix.toml`) in the project root:

```toml
[languages]
pyi = "python"       # Python stub files
mts = "typescript"   # TypeScript module files
lua = "generic"      # use sliding-window chunks
```

Valid parser values: `rust`, `python`, `typescript`, `javascript`, `go`, `cpp`, `c`, `generic`.

---

## 🔧 Output Filters

tokenix reduces noisy shell output by rewriting matching `Bash` commands in `PreToolUse` so they run through `tokenix run` before the agent sees the result. Filtering happens in three layers (highest priority first):

1. **Local project filters** — `.toml` files in `.tokenix/filters/` inside the repo. Scoped to the project, committed to version control.
2. **User filters** — `.toml` files in `~/.tokenix/filters/`. Apply to all projects, override bundled filters.
3. **Bundled filters** — 73 RTK-compatible TOML filters shipped inside the binary, covering `uv sync`, `cargo build`, `git`, `gradle`, `terraform plan`, `make`, `npm`, `poetry`, `docker`, and more. Applied automatically — no setup needed.

### Filter format

```toml
[filters.uv-sync]
description = "Compact uv sync output"
match_command = "^uv\\s+(sync|pip\\s+install)\\b"
strip_ansi = true
strip_lines_matching = ["^\\s*$", "^\\s+Downloading ", "^\\s+Using cached "]
match_output = [
  { pattern = "Audited \\d+ package", message = "ok (up to date)" },
]
max_lines = 20
on_empty = "uv: ok"
```

| Field | Description |
|---|---|
| `match_command` | Rust regex matched against the full Bash command line |
| `strip_ansi` | Remove ANSI colour codes before filtering |
| `strip_lines_matching` | Drop lines matching any of these regex patterns |
| `keep_lines_matching` | Keep only lines matching these patterns |
| `match_output` | Short-circuit: if output matches `pattern`, return `message` immediately |
| `max_lines` / `head_lines` / `tail_lines` | Truncate output |
| `truncate_lines_at` | Truncate individual lines at N characters |
| `on_empty` | Message to return when filtering produces empty output |

### AI-assisted filter generation

```bash
tokenix filter list                  # commands wasting the most tokens (no filter yet)
tokenix filter active                # all active user + bundled filters
tokenix filter record "cargo test"   # capture real output for richer generation
tokenix filter generate "cargo test" # generate a TOML filter via a local AI CLI
```

---

## 🏗 Architecture

```
src/
├── main.rs        CLI entry (clap), command dispatch, install-hook helpers
├── chunker.rs     Symbol-aware AST chunking (Tree-sitter) + dynamic language config (.tokenix.toml)
├── embed.rs       fastembed ONNX: embed_documents(), embed_query() — optional GPU via ort features
├── store.rs       SQLite schema, CRUD, FTS5, hybrid search, incremental branch fingerprint check
├── indexer.rs     File walker + incremental index pipeline (parallel chunking + batch embedding)
├── query.rs       Hybrid semantic + sparse FTS5 ranking, strict context modes, token-budget selection
├── pack.rs        Budgeted repo pack generation for non-hook AI tools, changed packs, token maps
├── graph.rs       Symbol relationship graph, cycle detection (Tarjan's SCC), HTML/Mermaid export
├── artifacts.rs   Context artifacts — parse `.tokenix/artifacts.json`, read non-code content
├── hook.rs        PreToolUse handler — Claude-, Copilot-, and grep_search/run_in_terminal-style JSON input
├── daemon.rs      Background TCP server — holds model + in-memory embedding cache
├── compress.rs    Legacy PostToolUse compatibility pipeline for tool-output rewriting
├── filters.rs     FilterDef, load_local/user/bundled_filters(), priority merge, apply_filter()
├── cmd_filter.rs  `tokenix filter` subcommands (list, active, generate, record)
├── recordings.rs  Capture/replay real command output for filter generation
├── memory.rs      Global/project preference memory (editable Markdown)
├── gain.rs        Analytics from the hook log — per-model cost table
├── benchmark.rs   Reproducible savings + retrieval-quality benchmark
├── doctor.rs      Backend / GPU / model-cache / daemon diagnostics
├── mcp.rs         Model Context Protocol server (full and slim profiles)
└── mcp_audit.rs   Multi-agent MCP config discovery + live tools/list introspection (prompt/session audit)

assets/
└── filters/       72 RTK-compatible TOML filters, embedded in the binary via rust-embed
```

### GPU acceleration (opt-in)

A default build runs embeddings on CPU. Compile with a GPU feature to use the GPU — it then becomes the **default at runtime, with automatic CPU fallback** if the provider is unavailable:

```bash
# Windows — DirectML (works with any D3D12-capable GPU, no CUDA toolkit required)
cargo install --path . --features directml --locked

# Linux / Windows — CUDA (needs CUDA 12.x + cuDNN 9.x installed and on PATH;
# ort rc.9 does not support CUDA 13 yet)
cargo install --path . --features cuda --locked
```

On a GPU build, force CPU per-invocation with the global `--only-cpu` flag:

```bash
tokenix index .              # uses the GPU
tokenix --only-cpu index .   # forces CPU on a GPU build
```

`--embed-batch` drives peak memory (default 16 on CPU, 64 on GPU) — lower it if RAM/VRAM is tight. Run `tokenix doctor` to see the compiled backend, detected GPU, CUDA/cuDNN status, and tailored recommendations.

### Daemon

The background daemon (`tokenix serve`) keeps the ONNX model and project embeddings in RAM. Hook calls route over TCP loopback instead of re-loading the model on each subprocess invocation, and it auto-starts on the first Grep hook call — you don't need to run it manually.

### Embedding model

| Property | Value |
|---|---|
| Model | `nomic-embed-text-v1.5` (quantized) |
| Dimensions | 768 |
| File size | ~130 MB |
| Cache location | `%LOCALAPPDATA%\tokenix\models` (Windows) / `~/.cache/tokenix/models` (Linux/macOS) |
| Download | Automatic on first run |
| Runtime | fastembed (ONNX Runtime, in-process) |

Index storage lives at `~/.tokenix/<project-id>.db` (one DB per project). Embeddings are stored as raw `float32` blobs and cosine similarity is computed in Rust — no external vector database needed.

---

## 🔒 Security

tokenix's build and release pipeline is hardened against supply-chain attacks:
SHA-pinned GitHub Actions, least-privilege workflow permissions, `cargo-deny`
(advisories + license + crates.io-only sources), `zizmor` workflow analysis,
OpenSSF Scorecard, SLSA build-provenance attestations, and tokenless crates.io
publishing via OIDC. See [SECURITY.md](SECURITY.md) for the disclosure policy
and release-verification steps.

---

## 🤝 Contributing

Contributions are welcome! See [CONTRIBUTING.md](CONTRIBUTING.md) for how to get started.

---

## 📄 License

[MIT](LICENSE)

<!-- GitHub Topics: rust cli llm token-optimization semantic-search embeddings fastembed onnx claude-code copilot ai-tools code-assistant developer-tools no-ollama -->
