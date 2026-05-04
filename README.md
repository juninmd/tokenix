# Tokenix

![tokenix-logo](tokenix-logo.png)

> Local semantic index CLI that slashes LLM token usage by up to 90% — built in Rust, runs 100% offline.

When AI coding assistants need to understand a codebase, they read entire files — burning tokens on code irrelevant to the task. **tokenix** fixes this by indexing your repository locally (embeddings via [Ollama](https://ollama.com) + SQLite) and intercepting those file reads, returning only the relevant symbols and chunks instead.

```
Without tokenix:  Read(auth/middleware.ts) → 800 lines → ~2,400 tokens
With tokenix:     Read(auth/middleware.ts) → 12 symbols outline → ~180 tokens  ✦ 92% saved
```

---

## Supported Tools

| Tool | Integration | Hook mechanism |
|---|---|---|
| **Claude Code** | ✅ Native | `PreToolUse` hook in `~/.claude/settings.json` |
| **GitHub Copilot** | ✅ Native | `.github/hooks/hooks.json` (agent mode) + `.github/copilot-instructions.md` |
| **OpenAI Codex CLI** | ✅ Instructions | `~/.codex/instructions.md` + shell helpers (`tx-read`, `tx-query`) |

## Supported Platforms

| Platform | Binary | Path |
|---|---|---|
| **Linux** | `tokenix` (ELF) | `~/.cargo/bin/tokenix` |
| **macOS** | `tokenix` (Mach-O) | `~/.cargo/bin/tokenix` |
| **Windows** | `tokenix.exe` | `%USERPROFILE%\.cargo\bin\tokenix.exe` |

---

## Features

- **Semantic search** — finds relevant code by meaning, not just keyword matching
- **AST-aware chunking** — splits by function/class/struct boundaries (Rust, Python, TS/JS, Go)
- **Multi-tool hooks** — works with Claude Code, GitHub Copilot, and Codex CLI out of the box
- **Graceful fallback** — if the index is missing or stale (>1h), passes through to the original tool without breaking the session
- **Token budget** — returns chunks up to a configurable token limit (default 3,000)
- **Savings analytics** — `tokenix gain` shows exactly how many tokens were saved per session
- **Single binary** — no Node, no Python, no Docker; one `.exe` / ELF binary via `cargo install`
- **100% local** — embeddings via Ollama (`nomic-embed-text`), nothing leaves your machine

---

## How It Works

```
┌──────────────────────────────────────────────────────────┐
│  AI Coding Tool  (Claude Code / Copilot / Codex CLI)     │
│   Read(file)  ─┐                                         │
│   Grep(query) ─┤  PreToolUse hook / instructions         │
└────────────────┼─────────────────────────────────────────┘
                 ▼
       ┌──────────────────┐      ┌──────────────────────────┐
       │  tokenix (Rust)  │─────▶│  SQLite + embeddings     │
       │                  │      │  .tokenix/index.db       │
       │  • query         │      └──────────────────────────┘
       │  • index                             ▲
       │  • hook                              │ 768d float32
       └──────────────────────────────────────────────────────┘
                                              │
                                   ┌──────────────────────┐
                                   │  Ollama (local)      │
                                   │  nomic-embed-text    │
                                   └──────────────────────┘
```

1. **`tokenix index .`** — walks the repo, chunks by AST symbols, generates embeddings via Ollama, stores in `.tokenix/index.db`
2. **AI tool reads a file** → the hook fires `tokenix hook` before the tool executes
3. **`tokenix hook`** — if file is large (>200 lines) returns a symbol outline; if query is semantic (3+ words) returns top relevant chunks
4. **AI tool receives compact context** instead of the raw file — same quality, fraction of the tokens

---

## Requirements

- **Rust** ≥ 1.75 — for building from source
- **Ollama** — running locally (`ollama serve`)
- **nomic-embed-text** model — `ollama pull nomic-embed-text`
- **AI coding tool** — Claude Code, GitHub Copilot, or Codex CLI

---

## Installation

### Option 1: Cargo (recommended)

```bash
cargo install tokenix
```

### Option 2: Pre-built binary

Download the latest binary for your platform from [GitHub Releases](https://github.com/juninmd/tokenix/releases):

| Platform | File |
|---|---|
| Linux x86_64 | `tokenix-linux-x86_64` |
| Linux arm64 | `tokenix-linux-aarch64` |
| macOS x86_64 | `tokenix-macos-x86_64` |
| macOS arm64 | `tokenix-macos-aarch64` |
| Windows x86_64 | `tokenix-windows-x86_64.exe` |

```bash
# Linux / macOS
chmod +x tokenix-linux-x86_64
sudo mv tokenix-linux-x86_64 /usr/local/bin/tokenix

# Windows (PowerShell) — move to a directory in PATH
Move-Item tokenix-windows-x86_64.exe "$env:USERPROFILE\.cargo\bin\tokenix.exe"
```

### Option 3: Build from source

```bash
git clone https://github.com/juninmd/tokenix
cd tokenix
cargo build --release
# Binary at: target/release/tokenix (or tokenix.exe on Windows)
```

### Setup Ollama model

```bash
ollama pull nomic-embed-text
```

---

## Setup by Tool

### Claude Code

```bash
tokenix install-hook --tool claude-code
```

Writes to `~/.claude/settings.json` (global) or `.claude/settings.json` (use `--local`):

```json
{
  "hooks": {
    "PreToolUse": [{
      "matcher": "Read|Grep",
      "hooks": [{"type": "command", "command": "/path/to/tokenix hook"}]
    }]
  }
}
```

Works identically on **Windows** (`C:\Users\<user>\.claude\settings.json`), **macOS**, and **Linux**.

---

### GitHub Copilot

```bash
cd my-project
tokenix install-hook --tool copilot
git add .github/
git commit -m "chore: add tokenix context hooks"
```

Creates two files in your repository:

**`.github/copilot-instructions.md`** — injected into every Copilot chat request:
```markdown
## tokenix — Semantic Context Tool
Before reading large files, use:
  tokenix read <file>                    # symbol outline
  tokenix read <file> --symbol <name>    # specific function
  tokenix query "natural language query" # semantic search
```

**`.github/hooks/hooks.json`** — `preToolUse` hook for Copilot agent mode:
```json
{
  "version": 1,
  "hooks": {
    "preToolUse": [{
      "type": "command",
      "bash": "/path/to/tokenix hook",
      "powershell": "/path/to/tokenix hook",
      "timeoutSec": 10
    }]
  }
}
```

> Note: committing `.github/` applies the hook to all contributors on the repo.

---

### OpenAI Codex CLI

```bash
tokenix install-hook --tool codex
```

Creates:
- **`~/.codex/instructions.md`** — injected into every Codex session with tokenix usage instructions
- **`~/.codex/tokenix-init.sh`** — shell helpers for bash/zsh
- **`~/.codex/tokenix-init.ps1`** — shell helpers for PowerShell

Activate shell helpers:

```bash
# bash / zsh
echo 'source ~/.codex/tokenix-init.sh' >> ~/.bashrc

# PowerShell
echo '. ~/.codex/tokenix-init.ps1' >> $PROFILE
```

Then use in your shell:
```bash
tx-read src/auth/middleware.ts           # smart file reader
tx-query "how does JWT validation work"  # semantic search
```

---

### All tools at once

```bash
tokenix install-hook --tool all
```

---

## Usage

### Index a repository

```bash
cd my-project
tokenix index .
```

```
tokenix indexing /home/user/my-project
  [1/42] src/main.rs
  [2/42] src/auth/middleware.rs
  ...
Done in 38.2s
  Files: 42 indexed, 0 skipped, 0 errors
  Index: 318 chunks, 87,412 tokens stored
```

### Semantic search

```bash
tokenix query "how does JWT validation work"
tokenix query "database connection pooling" --budget 2000
```

### Smart file reader

```bash
tokenix read src/auth/middleware.rs               # symbol outline (>200 lines)
tokenix read src/auth/middleware.rs --symbol validate_token   # specific function
tokenix read src/auth/middleware.rs --lines 45-80             # line range
```

### Token savings analytics

```bash
tokenix gain
tokenix gain --history   # per-call breakdown
```

```
tokenix gain -- /home/user/my-project

  Total hook calls   47
  Intercepted        31
  Passed through     16
  Tokens saved       84,210
  Tokens used        9,340
  Reduction          90.0%
  Cost saved (est.)  $0.2526

By tool:
  Read: 28 calls, 76,450 tokens saved
  Grep: 3 calls, 7,760 tokens saved
```

### Remove hooks

```bash
tokenix remove-hook --tool all         # remove everything
tokenix remove-hook --tool claude-code # specific tool only
```

---

## Commands Reference

| Command | Description |
|---|---|
| `tokenix index [PATH]` | Index the repository at PATH (default: `.`) |
| `tokenix query TEXT` | Semantic search over indexed chunks |
| `tokenix read FILE` | Smart file reader with symbol outline |
| `tokenix gain` | Show token savings analytics |
| `tokenix stats` | Show index statistics |
| `tokenix install-hook` | Install hooks (default: `--tool all`) |
| `tokenix remove-hook` | Remove hooks (default: `--tool all`) |
| `tokenix hook` | Hook handler called by AI tools (not for direct use) |

### `tokenix install-hook` / `tokenix remove-hook` flags

| Flag | Values | Description |
|---|---|---|
| `--tool` | `claude-code`, `copilot`, `codex`, `all` | Target tool (default: `all`) |
| `--local` | — | For claude-code: use `.claude/settings.json` instead of global |

### `tokenix index` flags

| Flag | Default | Description |
|---|---|---|
| `--model`, `-m` | `nomic-embed-text` | Ollama embedding model |
| `--force`, `-f` | false | Reindex all files (ignore mtime/hash cache) |

### `tokenix query` flags

| Flag | Default | Description |
|---|---|---|
| `--budget`, `-b` | 3000 | Max tokens to return |
| `--k` | 20 | Candidate chunks before budget filter |
| `--file`, `-f` | — | Filter results to a specific file |
| `--model`, `-m` | `nomic-embed-text` | Embedding model |

---

## Hook Behavior

```
Index missing or stale (>1h)?  →  pass through (original tool runs)

Tool = Read:
  file < 200 lines?              →  pass through
  offset/limit specified?        →  pass through (targeted read)
  file ≥ 200 lines?              →  return symbol outline  [exit 2]

Tool = Grep:
  pattern < 3 words?             →  pass through (regex/symbol search)
  pattern ≥ 3 words?             →  return semantic results  [exit 2]
```

**Exit codes (Claude Code / Copilot agent mode):**
- `0` — pass through, AI tool runs the original command
- `2` — intercept, hook's stdout replaces the tool result

---

## Supported Languages (AST chunking)

| Language | Extensions | Symbol types |
|---|---|---|
| Rust | `.rs` | `fn`, `struct`, `enum`, `impl`, `trait`, `mod` |
| Python | `.py` | `def`, `async def`, `class` |
| TypeScript | `.ts`, `.tsx` | `function`, `class`, `interface`, `type`, arrow functions |
| JavaScript | `.js`, `.jsx`, `.mjs` | `function`, `class`, arrow functions |
| Go | `.go` | `func`, `type` |
| Generic | all others | 400-token line blocks |

---

## Architecture

```
src/
├── main.rs       CLI entry (clap). Command dispatch + install-hook for all tools.
├── chunker.rs    AST-aware chunking + outline generation. Token approximation.
├── embed.rs      Ollama HTTP client (get_embedding, check_ollama).
├── store.rs      SQLite: schema, CRUD, cosine similarity, hook log NDJSON.
├── indexer.rs    File walker (respects .gitignore) + incremental index pipeline.
├── query.rs      Cosine similarity ranking + token-budget filtering + formatting.
├── hook.rs       PreToolUse handler. Reads from stdin (Claude Code) or env vars (Copilot).
└── gain.rs       Analytics from .tokenix/hook.log.
```

**Storage:** `.tokenix/index.db` — SQLite with raw BLOB embeddings (768-dim float32, 3072 bytes/chunk). Cosine similarity computed in Rust — no external vector DB.

---

## Comparison

| Feature | tokenix | Serena MCP | Aider repomap | Claude Context (Zilliz) |
|---|---|---|---|---|
| Language | Rust | Python | Python | Node |
| Claude Code hook | ✅ | ❌ MCP only | ❌ | ❌ |
| GitHub Copilot hook | ✅ | ❌ | ❌ | ❌ |
| Codex CLI support | ✅ | ❌ | ❌ | ❌ |
| Local embeddings | ✅ | ❌ | ❌ | ❌ |
| Single binary | ✅ | ❌ | ❌ | ❌ |
| Token analytics | ✅ | ❌ | ❌ | ❌ |
| Offline | ✅ | ❌ | ❌ | ❌ |

---

## License

MIT
