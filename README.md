# Tokenix

![tokenix-logo](tokenix-logo.png)

> Local semantic index CLI for token-efficient codebase exploration. Built in Rust, with embeddings generated locally through [Ollama](https://ollama.com/).

Tokenix helps AI coding assistants avoid reading entire files when they only need targeted context. It indexes your repository into `.tokenix/index.db`, then provides compact commands for semantic search and smart file reads.

```text
Without tokenix:  Read(auth/middleware.ts) -> 800 lines -> ~2,400 tokens
With tokenix:     tokenix read auth/middleware.ts -> symbol outline -> ~180 tokens
```

Actual savings depend on the codebase, assistant behavior, prompt, and file sizes. Use `tokenix gain --history` to inspect estimated savings from hook events.

---

## Supported Tools

| Tool | Status | Integration |
|---|---|---|
| [Claude Code](https://docs.anthropic.com/en/docs/claude-code) | Hook + instructions | Installs a `PreToolUse` hook in `~/.claude/settings.json` or `.claude/settings.json` |
| [GitHub Copilot](https://docs.github.com/en/copilot) | Instructions + hook config | Writes `.github/copilot-instructions.md` and `.github/hooks/hooks.json` |
| [OpenAI Codex CLI](https://help.openai.com/en/articles/11096431-openai-codex-cli-getting-started) | Instructions/helpers only | Writes tokenix guidance and shell helpers under `~/.codex/` |

Tokenix uses hooks as a control layer. For large reads and semantic grep-like requests, the hook intercepts the tool call (exit code `2`) and returns compact tokenix context as the tool result. The most predictable path is still to call `tokenix read` and `tokenix query` directly, or rely on generated assistant instructions that ask the agent to do that.

## Supported Platforms

| Platform | Binary | Typical cargo install path |
|---|---|---|
| Linux | `tokenix` | `~/.cargo/bin/tokenix` |
| macOS | `tokenix` | `~/.cargo/bin/tokenix` |
| Windows | `tokenix.exe` | `%USERPROFILE%\.cargo\bin\tokenix.exe` |

---

## Features

- **Semantic search** - find relevant code by meaning, not just exact keywords.
- **Symbol-aware chunking** - parser-less heuristics for Rust, Python, TypeScript, JavaScript, and Go definitions.
- **Smart file reader** - outlines large files and supports symbol or line-range reads.
- **Hook-based guardrails** - blocks large reads when supported and gives the assistant compact context instead.
- **Graceful fallback** - hooks allow the original tool when the index is missing, stale, or input cannot be parsed.
- **Token budget** - query results fit within a configurable approximate token budget, default `3000`.
- **Savings analytics** - `tokenix gain` summarizes estimated token savings from `.tokenix/hook.log`.
- **Local-first storage** - SQLite plus local Ollama embeddings by default.

---

## How It Works

```text
AI assistant / shell
  |
  | tokenix read <file>
  | tokenix query "natural language query"
  v
tokenix CLI (Rust)
  |
  | chunks files, generates embeddings, searches by cosine similarity
  v
.tokenix/index.db (SQLite) <-> Ollama (nomic-embed-text by default)
```

1. `tokenix index .` walks the repo, chunks indexed files, generates embeddings via Ollama, and stores them in `.tokenix/index.db`.
2. `tokenix query "..."` embeds a natural-language query and returns the most relevant chunks within a token budget.
3. `tokenix read FILE` returns a symbol outline for large files, full content for small files, or targeted content with `--symbol` / `--lines`.
4. Optional generated instructions encourage Claude Code, Copilot, and Codex CLI to prefer these commands before reading large files.

---

## Benchmark (measured, not estimated)

Ran `tokenix read` on the tokenix source files + 4 realistic sample files (Python, TypeScript, Go, Rust).
Every number below comes from a live run — no mock data.

### What the AI actually receives

Instead of the full 722-line `src/main.rs`, the AI gets a structural map:

```
[src/main.rs] - 722 lines, 20 symbols

  L19-22  [struct]   Cli
  L25-34  [enum]     Tool
  L37-107 [enum]     Commands
  L109-121 [module]  find_repo_root(start: &PathBuf) -> PathBuf
  L125-129 [module]  tokenix_bin_path() -> Result<String>
  L131-156 [module]  main() -> Result<()>
  L158-188 [module]  cmd_index(path: &PathBuf, model: &str, force: bool) -> Result<()>
  L277-323 [module]  cmd_gain(path: &PathBuf, history: bool) -> Result<()>
  L327-339 [module]  cmd_install_hook(tool: Tool, local: bool) -> Result<()>
  L341-389 [module]  install_claude_code(local: bool) -> Result<()>
  L391-488 [module]  install_copilot() -> Result<()>
  ...
  L716-722 [module]  format_ts(ts: f64) -> String

Use --symbol <name> or --lines N-M to read specific parts.
```

**What is preserved:** all symbol names, signatures, kinds, and exact line ranges.  
**What is removed:** function bodies (on purpose — the AI uses `--symbol` to request them when needed).  
**Targeted reads always pass through:** if the AI asks `Read(src/main.rs, offset=391, limit=98)` the hook does not intercept it.

### Token reduction by file

| File | Lines | Without tokenix | With tokenix | Saved |
|---|---|---|---|---|
| src/main.rs | 723 | 5,412 tok | 412 tok | **92.4%** |
| src/chunker.rs | 578 | 4,292 tok | 245 tok | **94.3%** |
| src/hook.rs | 332 | 2,472 tok | 274 tok | **88.9%** |
| src/store.rs | 297 | 2,192 tok | 512 tok | **76.6%** |
| samples/api_handler.go | 322 | 2,266 tok | 649 tok | **71.4%** |
| samples/auth_middleware.py | 284 | 2,385 tok | 643 tok | **73.0%** |
| samples/database_client.ts | 358 | 2,805 tok | 332 tok | **88.2%** |
| samples/user_service.rs | 377 | 3,135 tok | 235 tok | **92.5%** |
| **TOTAL** | | **24,959 tok** | **3,302 tok** | **86.8%** |

Token count uses the same `(chars + 3) / 4` approximation tokenix applies at runtime.

### Cost comparison — Claude Sonnet 4.6 ($3.00 / M input tokens)

| Scenario | Tokens saved | Cost saved |
|---|---|---|
| 8 file reads (benchmark above) | 21,657 | $0.065 |
| 100 reads / session (typical) | ~270,000 | $0.81 |
| 200 reads / day | ~540,000 | $1.62 |
| 22 working days / month | ~11.9 M | **$35.64 / dev / month** |
| Team of 5 | — | **~$178 / month** |

Estimates assume an average of 2,700 tokens saved per intercepted read (file ~3,200 tok raw, outline ~500 tok).  
Small files (<200 lines) pass through with zero overhead.

### Reproduce it

```bash
# Linux / macOS
bash benchmark/bench.sh

# Windows
.\benchmark\bench.ps1
```

Sample source files are in `benchmark/samples/` — four realistic files (280–380 lines each) covering Python, TypeScript, Go, and Rust.

---

## Requirements

- [Rust](https://www.rust-lang.org/tools/install) `>= 1.75` for building from source.
- [Ollama](https://ollama.com/download) running locally, usually with `ollama serve`.
- The `nomic-embed-text` model:

```bash
ollama pull nomic-embed-text
```

---

## Installation

### Build from source

```bash
git clone https://github.com/juninmd/tokenix
cd tokenix
cargo build --release
```

The binary is created at `target/release/tokenix` on Linux/macOS or `target/release/tokenix.exe` on Windows.

Install this checkout into your cargo bin directory:

```bash
cargo install --path .
```

### crates.io

```bash
cargo install tokenix
```

This only works after the `tokenix` crate has been published to [crates.io](https://crates.io/).

### Pre-built binary

If GitHub Releases are available, download the matching asset from:

https://github.com/juninmd/tokenix/releases

Expected asset names may include:

| Platform | File |
|---|---|
| Linux x86_64 | `tokenix-linux-x86_64` |
| Linux arm64 | `tokenix-linux-aarch64` |
| macOS x86_64 | `tokenix-macos-x86_64` |
| macOS arm64 | `tokenix-macos-aarch64` |
| Windows x86_64 | `tokenix-windows-x86_64.exe` |

---

## Setup by Tool

### Claude Code

```bash
tokenix install-hook --tool claude-code
```

Writes to `~/.claude/settings.json` globally, or `.claude/settings.json` with `--local`:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Read|Grep",
        "hooks": [
          { "type": "command", "command": "/path/to/tokenix hook" }
        ]
      }
    ]
  }
}
```

Claude Code hooks receive JSON on stdin. Tokenix handles large `Read` calls and semantic `Grep` calls by printing compact context to stdout and exiting with code `2`, which causes Claude Code to use that output as the tool result instead of running the original tool.

### GitHub Copilot

```bash
cd my-project
tokenix install-hook --tool copilot
git add .github/
git commit -m "chore: add tokenix context instructions"
```

Creates:

- `.github/copilot-instructions.md` - tells Copilot to prefer tokenix commands.
- `.github/hooks/hooks.json` - configures a `preToolUse` hook.

The generated instructions emphasize this workflow:

```bash
tokenix query "what you need to understand"
tokenix read <file>
tokenix read <file> --symbol <name>
tokenix read <file> --lines N-M
```

Tokenix supports Copilot-style hook input with `toolName` and `toolArgs` for `view`/`read` calls.

### OpenAI Codex CLI

```bash
tokenix install-hook --tool codex
```

Creates:

- `~/.codex/instructions.md` - compatibility tokenix guidance for setups that load it.
- `~/.codex/tokenix-init.sh` - shell helpers for bash/zsh.
- `~/.codex/tokenix-init.ps1` - shell helpers for PowerShell.

OpenAI's current Codex guidance emphasizes project `AGENTS.md` and `~/.codex/config.toml`. Treat the generated `instructions.md` as a compatibility helper, not a guaranteed CLI standard.

Activate helpers:

```bash
# bash / zsh
echo 'source ~/.codex/tokenix-init.sh' >> ~/.bashrc

# PowerShell
echo '. ~/.codex/tokenix-init.ps1' >> $PROFILE
```

Then use:

```bash
tx-read src/auth/middleware.ts
tx-query "how does JWT validation work"
```

### All tools

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

Example output:

```text
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
tokenix read src/auth/middleware.rs
tokenix read src/auth/middleware.rs --symbol validate_token
tokenix read src/auth/middleware.rs --lines 45-80
```

### Token savings analytics

```bash
tokenix gain
tokenix gain --history
```

Example output:

```text
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
tokenix remove-hook --tool all
tokenix remove-hook --tool claude-code
```

---

## Commands Reference

| Command | Description |
|---|---|
| `tokenix index [PATH]` | Index the repository at PATH, default `.` |
| `tokenix query TEXT` | Semantic search over indexed chunks |
| `tokenix read FILE` | Smart file reader with symbol outline for large files |
| `tokenix gain` | Show estimated token savings analytics |
| `tokenix stats` | Show index statistics |
| `tokenix install-hook` | Install assistant instructions/hook files, default `--tool all` |
| `tokenix remove-hook` | Remove assistant instructions/hook files, default `--tool all` |
| `tokenix hook` | Hook handler called by AI tools |

### `tokenix install-hook` / `tokenix remove-hook` flags

| Flag | Values | Description |
|---|---|---|
| `--tool` | `claude-code`, `copilot`, `codex`, `all` | Target tool, default `all` |
| `--local` | - | For Claude Code: use `.claude/settings.json` instead of global settings |

### `tokenix index` flags

| Flag | Default | Description |
|---|---|---|
| `--model`, `-m` | `nomic-embed-text` | Ollama embedding model |
| `--force`, `-f` | false | Reindex all files, ignoring mtime/hash cache |

### `tokenix query` flags

| Flag | Default | Description |
|---|---|---|
| `--budget`, `-b` | 3000 | Max approximate tokens to return |
| `--k` | 20 | Candidate chunks before budget filtering |
| `--file`, `-f` | - | Filter results to a specific file |
| `--model`, `-m` | `nomic-embed-text` | Ollama embedding model |
| `--path`, `-p` | `.` | Repository/index path |

---

## Hook Behavior

Claude-style stdin JSON:

```json
{
  "tool_name": "Read",
  "tool_input": { "file_path": "src/main.rs" }
}
```

Copilot-style stdin JSON:

```json
{
  "toolName": "view",
  "toolArgs": "{\"path\":\"src/main.rs\"}"
}
```

Legacy environment variables are also supported:

- `COPILOT_TOOL_NAME` / `COPILOT_TOOL_INPUT`
- `TOOL_NAME` / `TOOL_INPUT`

Decision logic:

```text
Index missing or stale (>1h)?  -> exit 0 (pass through, original tool runs)

Tool = Read:
  file < 200 lines?              -> exit 0 (pass through)
  offset or limit specified?     -> exit 0 (pass through — targeted read)
  file >= 200 lines?             -> print symbol outline to stdout, exit 2

Tool = Grep:
  pattern < 3 words?             -> exit 0 (pass through — likely a regex search)
  pattern >= 3 words?            -> print semantic results to stdout, exit 2
```

Exit codes:

| Exit code | Meaning |
|---|---|
| `0` | Pass through — Claude Code runs the original tool |
| `2` | Intercept — Claude Code uses the hook's stdout as the tool result |

The hook never exits `1`. Any internal error falls through to `exit 0` so the AI session is never broken by a tokenix failure.

---

## Supported Languages

Tokenix indexes the extensions listed in `INDEXED_EXTS`. Some languages have symbol-aware chunking; other indexed extensions use generic 400-token line blocks.

| Language | Extensions | Symbol types |
|---|---|---|
| Rust | `.rs` | `fn`, `struct`, `enum`, `impl`, `trait`, `mod` |
| Python | `.py` | `def`, `async def`, `class` |
| TypeScript | `.ts`, `.tsx` | `function`, `class`, `interface`, `type`, arrow functions |
| JavaScript | `.js`, `.jsx`, `.mjs`, `.cjs` | `function`, `class`, arrow functions |
| Go | `.go` | `func`, `type` |
| Generic indexed files | `.java`, `.c`, `.cpp`, `.h`, `.hpp`, `.cs`, `.rb`, `.swift`, `.kt`, `.scala`, `.sh`, `.bash`, `.toml`, `.yaml`, `.yml`, `.json`, `.md`, `.txt` | 400-token line blocks |

---

## Architecture

```text
src/
|-- main.rs       CLI entry (clap), command dispatch, install/remove helpers
|-- chunker.rs    Symbol-aware heuristic chunking and outline generation
|-- embed.rs      Ollama HTTP client: get_embedding() and check_ollama()
|-- store.rs      SQLite schema, CRUD, cosine similarity, hook log NDJSON
|-- indexer.rs    File walker plus incremental index pipeline
|-- query.rs      Ranking, token-budget selection, and result formatting
|-- hook.rs       Hook handler for Claude-style and Copilot-style input JSON
`-- gain.rs       Analytics from .tokenix/hook.log
```

Storage lives at `.tokenix/index.db`. Embeddings are stored as raw float32 blobs in SQLite. Cosine similarity is computed in Rust, so tokenix does not require sqlite-vec or an external vector database.

---

## Comparison Notes

Tokenix is closest to tools that provide semantic code retrieval or compact repo context, but its current differentiators are narrow and concrete:

- Rust CLI with local SQLite storage.
- Local Ollama embeddings by default.
- Smart `read`, semantic `query`, and savings analytics commands.
- Assistant instructions and hook configs for Claude Code, Copilot, and Codex CLI.

Other tools may provide stronger MCP integration, LSP-backed symbol navigation, hybrid search, hosted vector databases, or IDE extensions. Check each project's current documentation before relying on feature comparisons.

---

## License

MIT
