<div align="center">
  <img src="tokenix-logo.png" alt="tokenix logo" width="450" />

  <h1>tokenix</h1>

  <p><strong>Local semantic context for AI coding agents, with fewer wasted tokens.</strong></p>

  <p>
    <a href="https://github.com/juninmd/tokenix/releases"><img src="https://img.shields.io/github/v/release/juninmd/tokenix?style=flat-square&color=orange&label=release" alt="Latest Release" /></a>
    <a href="https://crates.io/crates/tokenix"><img src="https://img.shields.io/crates/v/tokenix?style=flat-square&color=orange" alt="crates.io" /></a>
    <a href="https://github.com/juninmd/tokenix/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue?style=flat-square" alt="License" /></a>
    <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/built%20with-Rust-orange?style=flat-square&logo=rust" alt="Built with Rust" /></a>
    <img src="https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20Windows-lightgrey?style=flat-square" alt="Platforms" />
    <img src="https://img.shields.io/badge/savings-up%20to%2090%25%20tokens-brightgreen?style=flat-square" alt="Token Savings" />
    <img src="https://img.shields.io/badge/no%20Ollama-required-blue?style=flat-square" alt="No Ollama required" />
  </p>

  <p>
    <a href="#-quick-install">Install</a> ·
    <a href="#-how-it-works">How it Works</a> ·
    <a href="#-benchmark">Benchmark</a> ·
    <a href="#-usage">Usage</a> ·
    <a href="#-setup-by-tool">Setup</a> ·
    <a href="CONTRIBUTING.md">Contributing</a>
  </p>
</div>

---

> **tokenix** is a local-first Rust CLI that helps AI coding agents understand a repository without dumping huge files into the prompt. It indexes your code, finds relevant chunks by meaning, returns compact file outlines, and can hook into AI tools to replace noisy reads and command output with smaller, more useful context. Works with Claude Code, GitHub Copilot, and OpenAI Codex CLI. **No Ollama or external server required.**

```
Without tokenix:  Read(src/auth/middleware.rs) → 800 lines → ~2,400 tokens  ❌
With tokenix:     tokenix read src/auth/middleware.rs → symbol outline → ~180 tokens  ✅
```

Actual savings depend on codebase size, AI behavior, and file sizes. Run `tokenix gain --history` to see your real numbers.

---

## What Is tokenix?

AI coding agents often waste context on the wrong shape of information: entire files, long grep output, repeated build logs, and directory listings that are much larger than the useful signal inside them. tokenix is a context layer between the agent and your repository.

It does four jobs:

| Job | What tokenix does | Why it matters |
|---|---|---|
| **Index the repository** | Walks source files, splits them into symbol-aware chunks, and stores local embeddings in SQLite | The agent can search by intent instead of opening files blindly |
| **Read files compactly** | Returns outlines, symbols, or line ranges instead of full files when possible | Large files stop consuming thousands of unnecessary tokens |
| **Intercept assistant tools** | Hooks into supported tools before large reads and after noisy command output | Optimization happens automatically during normal AI sessions |
| **Measure savings** | Logs hook decisions and estimates token/cost reduction with `tokenix gain` and `tokenix benchmark` | You can prove whether it is actually helping on your codebase |

tokenix is not a cloud service, not a vector database server, and not a replacement for your AI assistant. It is a local repository index plus a set of CLI and hook integrations that make the assistant's context smaller and more targeted.

---

## ⚡ Quick Install

### Pre-built binary (recommended)

Download the latest binary for your platform from [GitHub Releases](https://github.com/juninmd/tokenix/releases):

| Platform | File |
|---|---|
| Linux x86_64 | `tokenix-linux-x86_64` |
| Linux arm64 | `tokenix-linux-aarch64` |
| macOS x86_64 | `tokenix-macos-x86_64` |
| macOS arm64 (M1/M2/M3) | `tokenix-macos-aarch64` |
| Windows x86_64 | `tokenix-windows-x86_64.exe` |
| Windows x86_64 (GPU / DirectML) | `tokenix-windows-x86_64-directml.exe` |

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

> **Requirements:** [Rust](https://www.rust-lang.org/tools/install) `>= 1.75` — that's all. No Ollama, no Python, no external services.

The embedding model (`nomic-embed-text-v1.5-Q`, ~130 MB) is downloaded automatically on first use and cached locally.

---

## ✨ Features

| Feature | Description |
|---|---|
| **Semantic search** | Find relevant code by meaning, not just keywords |
| **One-call MCP context** | `tokenix_context` combines semantic search, entry points, and compact outlines so agents do not burn calls chaining search/read loops |
| **Graph-aware explore** | `tokenix explore` / `tokenix_explore` returns related symbols, relationship maps, and grouped source in one capped call |
| **Symbol graph** | `tokenix symbols`, `callers`, `callees`, and `impact` trace relationships between indexed symbols |
| **Interactive HTML graph** | `tokenix impact --format html` exports a dark-mode vis.js graph with node colours, directional arrows, and physics springs |
| **Preference memory** | `tokenix memory add/list` stores global and project preferences in editable Markdown; context/explore include saved preferences and capture guidance |
| **Dynamic language detection** | Map custom file extensions to any built-in parser via a project `.tokenix.toml` — no recompile needed |
| **Symbol-aware chunking** | AST Tree-sitter parsers for Rust, Python, TypeScript, JavaScript, Go, C++ |
| **Smart file reader** | Outlines large files; supports `--symbol` and `--lines` reads |
| **Hook-based interception** | `PreToolUse` intercepts large reads; `PostToolUse` compresses Bash/ListDirectory output |
| **RTK-grade Compression** | Absorbed RTK features: Fuzzy Grouping (groups `Removing...`, `Compiling...`, etc.), NDJSON/JSON compaction, and ANSI/Emoji stripping |
| **Local project filters** | Drop `.toml` files in `.tokenix/filters/` for project-scoped compression rules — highest priority over user and bundled filters |
| **Output filters** | 70+ RTK-compatible TOML filters embedded in the binary — auto-applied to Bash output for `uv`, `cargo`, `terraform`, `ansible`, and more |
| **Incremental branch indexing** | Branch/HEAD switches with identical code auto-update the git fingerprint without re-indexing |
| **GPU acceleration (opt-in)** | Build with `--features directml` (Windows) or `--features cuda` to run embeddings on GPU (~10× faster indexing); GPU is used by default with automatic CPU fallback, or force CPU with `--only-cpu` |
| **Environment diagnostics** | `tokenix doctor` reports the compiled backend, detected GPU, CUDA/cuDNN status, model cache, and daemon — with tailored recommendations |
| **In-memory daemon** | `tokenix serve` keeps model + index in RAM — warm Grep calls drop from ~430ms to ~80ms |
| **Graceful fallback** | Always exits `0` on errors — your AI session is never broken |
| **Token budget** | Results fit within a configurable token budget (default `1200`) |
| **Savings analytics** | `tokenix gain` — token summary, focused cost table for 7 reference models, by-tool breakdown |
| **Local-first, no dependencies** | fastembed ONNX in-process — no Ollama, no server, no internet after first run |

---

## 🔌 Supported AI Tools

| Tool | Integration |
|---|---|
| [Claude Code](https://docs.anthropic.com/en/docs/claude-code) | `PreToolUse` + `PostToolUse` hooks in `~/.claude/settings.json` |
| [GitHub Copilot](https://docs.github.com/en/copilot) | `.github/copilot-instructions.md` + `.github/hooks/hooks.json` |
| [OpenAI Codex CLI](https://help.openai.com/en/articles/11096431-openai-codex-cli-getting-started) | `~/.codex/hooks.json` + Windows wrapper + optional shell helpers |

---

## 🚀 How It Works

tokenix has two modes:

1. **Manual mode**: run `tokenix query` and `tokenix read` directly when you want compact context.
2. **Hook mode**: install hooks so supported AI tools call tokenix automatically before large reads and after noisy tool output.

### Real-world Compression (RTK Mode)

tokenix now includes advanced output filtering logic inspired by RTK (Rust Token Killer). It doesn't just truncate output; it understands the structure of common CLI tools.

- **Fuzzy Grouping:** Collapses 100s of "Compiling..." or "Removing..." lines into a single summary line.
- **Structural Compaction:** Compacts pretty-printed JSON and NDJSON into single-line formats automatically.
- **Signal Preservation:** Automatically keeps error messages and summaries even when the middle of a log is truncated.

---

## 📊 Benchmark

> Every number below comes from a live benchmark run on the tokenix source, using the actual index, chunking, and query code paths.

### Benchmark Results

We measure **tokenix** against pure **Vanilla** reads and **RTK** command filtering. `N/A` means the tool does not provide that category of function, not that the measurement failed.

| Metric | **tokenix** | **RTK** | **Vanilla** |
| :--- | :---: | :---: | :---: |
| **Large-file read reduction** | **84.8% saved** | N/A | 0% |
| **Targeted workflow reduction** | **67.2% saved** | N/A | 0% |
| **Context tokens, avg** | **435** | N/A | 5,050 |
| **Context homologation** | **4/4** | N/A | **4/4** |
| **Context latency, avg** | **11ms** | N/A | N/A |
| **Semantic quality** | **Hit@1 3/4, Hit@3 4/4** | N/A | N/A |
| **Command compression** | **63.0% saved** | 9.8% saved | 0% |
| **Command compression vs RTK** | **4/4 equal or lower tokens** | baseline | N/A |

### Capability Matrix

This table compares what each tool is designed to do. It is intentionally separate from the benchmark table so RTK is not judged as a semantic code search tool, and CodeGraph is not judged as a shell-output compressor.

| Capability | **tokenix** | **RTK** | **CodeGraph** | **Vanilla** |
| :--- | :---: | :---: | :---: | :---: |
| **Large read interception** | Yes | No | No | No |
| **Compact file outlines** | Yes | No | No | No |
| **Symbol-targeted reads** | Yes | No | Yes | No |
| **Semantic code search** | Yes | No | Yes | No |
| **Symbol graph / relationships** | Yes | No | Yes | No |
| **Shell output filtering** | Yes | Yes | No | No |
| **RTK-compatible filters** | Yes | Native | No | No |
| **Claude/Codex/Copilot hooks** | Yes | Yes | Partial | No |
| **Stale-index fail-open guard** | Yes | N/A | N/A | N/A |
| **Local embeddings / SQLite** | Yes | N/A | N/A | N/A |
| **Savings analytics** | Yes | Yes | No | No |
| **MCP support** | Yes | No | Yes | No |

*Results from `cargo run --release -- benchmark --refresh-index` on May 25, 2026.*

### Methodology

- **Large-file read reduction:** full file tokens vs. large-file outline tokens.
- **Command output compression:** measures the same synthetic command outputs through tokenix and `rtk pipe`; tokenix must be equal or lower tokens per command to avoid a hidden regression.
- **Semantic search quality:** Hit@1/Hit@3 accuracy on labeled repository queries.
- **Context homologation:** validates whether each context arm includes the expected file, not just whether it is small.
- **CodeGraph comparison:** real CodeGraph context tokens and latency are measured from the local CLI, not estimated from README claims.

### Reproduce it

```bash
cargo run --release -- benchmark --refresh-index
```

To include a local CodeGraph comparison:
```bash
cargo run --release -- benchmark --refresh-index --compare-codegraph /path/to/codegraph
```


---

## 🛠 Usage

### 1. Index your repository

```bash
cd my-project
tokenix index .
```

```
tokenix indexing /home/user/my-project
  discovered 42 file(s) — chunking
  embedding 318 chunks via fastembed (ONNX)...
Done in 42.3s  ·  42 files indexed  ·  318 chunks  ·  87,412 tokens stored
```

> **First run:** the model (~130 MB) is downloaded automatically. Subsequent runs use the local cache.

### 2. Semantic search

```bash
tokenix query "how does JWT validation work"
tokenix query "database connection pooling" --budget 2000
```

### 3. One-call task context

```bash
tokenix context "fix login refresh token bug"
tokenix context "how does the indexer batch embeddings" --budget 2000 --max-files 3
tokenix explore "run_hook hook_post compression" --budget 4000 --max-symbols 8
```

### 4. Smart file reader

```bash
tokenix read src/auth/middleware.rs                     # symbol outline
tokenix read src/auth/middleware.rs --symbol validate_token   # targeted
tokenix read src/auth/middleware.rs --lines 45-80       # line range
```

### 5. Symbol graph

```bash
tokenix symbols validate_token
tokenix callers validate_token
tokenix callees run_hook
tokenix impact update_user --depth 2
tokenix impact update_user --format html                          # dark-mode vis.js graph
tokenix impact update_user --format html --output update_user.html --depth 3
tokenix rebuild-graph   # recompute relationships without re-embedding
```

### 6. Token savings analytics

```bash
tokenix gain
tokenix gain --history   # includes last 20 hook events
```

```
╭────────────────────────────────────────────────────────────────╮
│ tokenix gain  ·  my-project                                   │
╰────────────────────────────────────────────────────────────────╯

  TOKEN SUMMARY                              HOOK CALLS
  Original (would-be)               332,068    Total                       349
  After optimization                214,646    Intercepted            148  (42%)
  Saved                             240,091    Passed through              201
  Reduction                  72.3%  [█████████████░░░░░]

  COST ESTIMATE  (input tokens · USD)
    Prices per 1M input tokens from public provider pricing pages. Collected: 2026-05-07.

      Model                          $/1M in       Without          With         Saved
      ───────────────────────────  ─────────  ────────────  ────────────  ────────────
      claude-haiku-4-5                 $1.00       $0.3321       $0.2146       $0.1174
      claude-sonnet-4.6 ★              $3.00       $0.9962       $0.6439       $0.3523
      claude-opus-4.7                  $5.00       $1.6603       $1.0732       $0.5871
      gpt-5.4-mini                     $0.75       $0.2491       $0.1610       $0.0881
      gpt-5.4                          $2.50       $0.8302       $0.5366       $0.2936
      gemini-3.1-flash-preview         $0.25       $0.0830       $0.0537       $0.0294
      gemini-3.1-pro-preview           $2.00       $0.6641       $0.4293       $0.2348
      ★ reference model · prices collected 2026-05-07

  BY TOOL
  Read    59 calls   228,974 ▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓░
  Grep    87 calls    11,094 ▓░░░░░░░░░░░░░░░░░░░
  Bash     2 calls        23 ░░░░░░░░░░░░░░░░░░░░
```

The cost table intentionally stays small: 7 reference models across Anthropic, OpenAI, and Google. Prices are shown with the collection date so benchmark reports stay auditable.

---

## 🔧 Setup by Tool

### Claude Code

```bash
tokenix install-hook --tool claude-code
```

Writes a `PreToolUse` hook to `~/.claude/settings.json` (or `.claude/settings.json` with `--local`). Large reads and semantic greps are intercepted automatically — no changes to your prompts needed.

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

Then use `tx-read` and `tx-query` as shell helpers.

On Windows, this also installs `~/.codex/hooks.json` and
`~/.codex/tokenix-codex-hook.ps1`. The wrapper keeps `PreToolUse` intercepts
active, but makes `PostToolUse` fail open so Codex does not report compressed
Bash output as a failed hook.

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
| `tokenix context TEXT` | One-call task context: entry points, relevant source, compact outlines |
| `tokenix explore TEXT` | Graph-aware exploration: entry points, relationships, grouped source |
| `tokenix memory add TEXT` | Save a project preference for future context |
| `tokenix memory add --global TEXT` | Save a global preference for future context |
| `tokenix memory list` | List global and project preferences |
| `tokenix memory remove QUERY` | Remove matching project preferences |
| `tokenix memory edit QUERY REPLACEMENT` | Replace matching project preferences |
| `tokenix read FILE` | Smart reader — outline for large files, full for small |
| `tokenix symbols QUERY` | Find indexed symbols by name or path |
| `tokenix callers SYMBOL` | Show symbols that call/reference a symbol |
| `tokenix callees SYMBOL` | Show symbols called/referenced by a symbol |
| `tokenix impact SYMBOL` | Show bidirectional impact graph around a symbol |
| `tokenix impact SYMBOL --format html` | Export interactive vis.js HTML graph (dark mode, physics, colour-coded by kind) |
| `tokenix impact SYMBOL --format html --output FILE.html` | Save HTML graph to a specific path |
| `tokenix rebuild-graph` | Rebuild graph tables from existing indexed chunks without re-embedding |
| `tokenix gain` | Token savings analytics with per-model cost table |
| `tokenix gain --history` | Same, plus last 20 hook events |
| `tokenix benchmark` | Reproducible savings and semantic-quality benchmark |
| `tokenix benchmark --compare-codegraph PATH` | Add a lightweight local CodeGraph comparison section |
| `tokenix stats` | Index statistics (files, chunks, tokens, age) |
| `tokenix serve [--port N]` | Start background embedding daemon (keeps model + index in RAM) |
| `tokenix stop` | Stop the background daemon |
| `tokenix doctor` | Diagnose embedding backend, GPU availability, model cache, and daemon |
| `tokenix filter list` | Show top Bash commands by tokens wasted (no filter yet) |
| `tokenix filter active` | Show active user and bundled output filters |
| `tokenix filter generate [CMD]` | AI-generate a TOML output filter for a command |
| `tokenix install-hook` | Install assistant hook/instructions (default `--tool all`) |
| `tokenix remove-hook` | Remove assistant hook/instructions (default `--tool all`) |
| `tokenix hook` | `PreToolUse` handler — intercepts large reads (called by AI tools) |
| `tokenix hook-post` | `PostToolUse` handler — compresses Bash/ListDirectory output (called by AI tools) |
| `tokenix mcp` | MCP server exposing context, read/search, graph, and gain tools |

<details>
<summary>Flag reference</summary>

**Global**

| Flag | Default | Description |
|---|---|---|
| `--only-cpu` | false | Force CPU embedding even on a GPU-enabled build (no-op on CPU-only builds) |

**`tokenix index`**

| Flag | Default | Description |
|---|---|---|
| `--force`, `-f` | false | Reindex all files, ignoring cache |
| `--cpu-profile` | `default` | Resource profile: `low` (1 worker, tiny batches, pause between batches), `default`, `max` (all cores, large batches) |
| `--jobs N` | env/default | Set max rayon worker threads for indexing |
| `--embed-batch N` | 16 (CPU) / 64 (GPU) | Embedding batch size; drives peak memory — lower it if RAM/VRAM is tight |
| `--if-stale` | false | Skip if index is fresh for the current Git worktree/branch/HEAD |

**`tokenix query`**

| Flag | Default | Description |
|---|---|---|
| `--budget`, `-b` | 1200 | Max approximate tokens to return |
| `--k` | 20 | Candidate chunks before budget filtering |
| `--file`, `-f` | — | Filter results to a specific file |
| `--path`, `-p` | `.` | Repository/index path |

**`tokenix benchmark`**

| Flag | Default | Description |
|---|---|---|
| `--refresh-index` | false | Refresh index metadata before measuring |
| `--budget` | 1200 | Semantic query token budget |
| `--compare-codegraph` | — | Path to a local CodeGraph checkout; prints measured CodeGraph context tokens/latency |
| `--path`, `-p` | `.` | Repository/index path |

**`tokenix install-hook` / `tokenix remove-hook`**

| Flag | Values | Description |
|---|---|---|
| `--tool` | `claude-code`, `copilot`, `codex`, `all` | Target tool (default `all`) |
| `--local` | — | Claude Code: use `.claude/settings.json` instead of global |

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
| Config / Docs | `.toml`, `.md`, `.txt`, `.sh`, `.bash` | 400-token line blocks |
| Data files (opt-in) | `.json`, `.yaml`, `.yml` | Indexed only when `data_files = true` in `.tokenix.toml` |
| **Custom** | any extension | Mapped to an existing parser via `.tokenix.toml` |

Languages without a symbol-aware chunker (Java, C#, Ruby, Swift, Kotlin, Scala, …) are not indexed — blind line-block chunking produces low-quality search results and is intentionally excluded.

### Custom language mapping

Create a `.tokenix.toml` (or `tokenix.toml`) in the project root:

```toml
[languages]
# map custom extensions to existing parsers
pyi   = "python"    # Python stub files
mts   = "typescript"  # TypeScript module files
lua   = "generic"   # use sliding-window chunks
```

Valid parser values: `rust`, `python`, `typescript`, `javascript`, `go`, `cpp`, `c`, `generic`.

---

## 🔧 Output Filters

tokenix compresses `Bash` and `ListDirectory` output via a `PostToolUse` hook before Claude sees it. Claude uses `hook-post` directly, where `exit 2` means "replace the tool output with compressed context". Codex uses a small wrapper that treats post-tool compression as success because Codex reports non-zero post hooks as failures. Filtering happens in three layers (highest priority first):

1. **Local project filters** — drop `.toml` files in `.tokenix/filters/` inside the repository. Scoped to the project, committed to version control, shared with the team.
2. **User filters** — drop `.toml` files in `~/.tokenix/filters/`. Take priority over bundled filters, apply to all projects.
3. **Bundled filters** — 59 RTK-compatible TOML filters shipped inside the binary, covering `uv sync`, `cargo build`, `gradle`, `terraform plan`, `make`, `npm`, `poetry`, `docker`, and more. Applied automatically — no setup needed.

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
| `keep_lines_matching` | Keep only lines matching these patterns (signal/noise) |
| `match_output` | Short-circuit: if output matches `pattern`, return `message` immediately |
| `max_lines` / `head_lines` / `tail_lines` | Truncate output |
| `truncate_lines_at` | Truncate individual lines at N characters |
| `on_empty` | Message to return when filtering produces empty output |

### AI-assisted filter generation

```bash
# See which commands waste the most tokens (no filter yet)
tokenix filter list

# Show all active user and bundled RTK-compatible filters
tokenix filter active

# Generate a TOML filter using a local AI CLI (claude, gh copilot, etc.)
tokenix filter generate "cargo test"

# Save to user filters directory
# → ~/.tokenix/filters/cargo-test.toml
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
├── query.rs       Hybrid semantic + sparse FTS5 ranking, token-budget selection, result formatting
├── graph.rs       Symbol relationship graph + export_relations_to_html() for vis.js HTML output
├── hook.rs        PreToolUse handler — Claude-style and Copilot-style JSON input
├── daemon.rs      Background TCP server — holds model + in-memory embedding cache
├── compress.rs    PostToolUse compression pipeline (Bash/ListDirectory output)
├── filters.rs     FilterDef, load_local/user/bundled_filters(), priority merge, apply_filter()
├── cmd_filter.rs  `tokenix filter` subcommands (list, active, generate)
└── gain.rs        Analytics from .tokenix/hook.log — per-model cost table

assets/
└── filters/       59 RTK-compatible TOML filters, embedded in the binary via rust-embed
```

### GPU Acceleration (opt-in)

A default build runs embeddings on CPU. Compile with a GPU feature to use the GPU — it then becomes the **default at runtime, with automatic CPU fallback** if the provider is unavailable:

```bash
# Windows — DirectML (works with any D3D12-capable GPU, no CUDA toolkit required)
cargo install --path . --features directml --locked

# Linux / Windows — CUDA (needs CUDA 12.x + cuDNN 9.x installed and on PATH;
# ort rc.9 does not support CUDA 13 yet)
cargo install --path . --features cuda --locked
```

> **Use `--locked`.** `cargo install` otherwise re-resolves dependencies and can pull an incompatible `ureq` into the `ort-sys` build script. `--locked` builds against the committed `Cargo.lock`.

On a GPU build, force CPU per-invocation with the global `--only-cpu` flag:

```bash
tokenix index .              # uses the GPU
tokenix --only-cpu index .   # forces CPU on a GPU build
```

Run `tokenix doctor` to see the compiled backend, detected GPU, CUDA/cuDNN status, and tailored recommendations.

> **GPU throughput (measured, RTX 4060 Ti / DirectML):** ~10× faster indexing than CPU (a 10k-chunk repo dropped from ~54 min to ~6 min). The CPU keeps RAM bounded by the embedding batch size — `--embed-batch` defaults to 16 on CPU (~2.8 GB peak) and 64 on GPU.

Storage lives at `~/.tokenix/<project-id>.db` (global, one DB per project). Embeddings are stored as raw `float32` blobs. Cosine similarity is computed in Rust — no external vector database needed.

### Daemon

The background daemon (`tokenix serve`) keeps the 130 MB ONNX model and all project embeddings in RAM. Hook calls route over TCP loopback instead of re-loading the model each subprocess invocation:

```
Without daemon:  hook process → load model (293 MB) → embed → search SQLite → exit  ~430ms
With daemon:     hook process → TCP → daemon (model already loaded) → search RAM →  ~80ms
```

The daemon **auto-starts** on the first Grep hook call — you don't need to run it manually. Multiple parallel hook calls share a single model instance, capping RAM at 293 MB regardless of concurrency.

### Embedding model

| Property | Value |
|---|---|
| Model | `nomic-embed-text-v1.5` (quantized int8) |
| Dimensions | 768 |
| File size | ~130 MB |
| Cache location | `%LOCALAPPDATA%\tokenix\models` (Windows) / `~/.cache/tokenix/models` (Linux/macOS) |
| Download | Automatic on first run |
| Runtime | fastembed (ONNX Runtime, in-process) |

---

## 🤝 Contributing

Contributions are welcome! See [CONTRIBUTING.md](CONTRIBUTING.md) for how to get started.

---

## 📄 License

[MIT](LICENSE)

<!-- GitHub Topics: rust cli llm token-optimization semantic-search embeddings fastembed onnx claude-code copilot ai-tools code-assistant developer-tools no-ollama -->
