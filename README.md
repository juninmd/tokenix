<div align="center">
  <img src=".github/prints/logo.jpg" alt="tokenix logo" style="max-height: 450px;" />

  <p><strong>Local semantic search, symbol graphs, secrets scanning, output filters, and CLI hooks for AI coding agents.</strong></p>

  <p>
    <a href="https://github.com/juninmd/tokenix/releases"><img src="https://img.shields.io/github/v/release/juninmd/tokenix?style=flat-square&color=orange&label=release" alt="Latest Release" /></a>
    <a href="https://crates.io/crates/tokenix"><img src="https://img.shields.io/crates/v/tokenix?style=flat-square&color=orange" alt="crates.io" /></a>
    <a href="https://crates.io/crates/tokenix"><img src="https://img.shields.io/crates/d/tokenix?style=flat-square&color=orange&label=downloads" alt="crates.io downloads" /></a>
    <a href="https://github.com/juninmd/tokenix/stargazers"><img src="https://img.shields.io/github/stars/juninmd/tokenix?style=flat-square&color=yellow" alt="GitHub stars" /></a>
    <a href="https://github.com/juninmd/tokenix/actions/workflows/rust.yml"><img src="https://img.shields.io/github/actions/workflow/status/juninmd/tokenix/rust.yml?branch=main&style=flat-square&label=CI" alt="CI" /></a>
    <a href="https://scorecard.dev/viewer/?uri=github.com/juninmd/tokenix"><img src="https://img.shields.io/ossf-scorecard/github.com/juninmd/tokenix?style=flat-square&label=scorecard" alt="OpenSSF Scorecard" /></a>
    <a href="https://github.com/juninmd/tokenix/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue?style=flat-square" alt="License" /></a>
    <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/built%20with-Rust-orange?style=flat-square&logo=rust" alt="Built with Rust" /></a>
    <img src="https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20Windows-lightgrey?style=flat-square" alt="Platforms" />
  </p>

  <p>
    <a href="#-install">Install</a> ·
    <a href="#-what-we-measure-and-what-we-dont">What we measure</a> ·
    <a href="#-dashboard">Dashboard</a> ·
    <a href="#-setup-by-tool">Setup</a> ·
    <a href="#-commands">Commands</a> ·
    <a href="CONTRIBUTING.md">Contributing</a>
  </p>
</div>

---

> **tokenix** is a local-first Rust CLI that helps AI coding agents understand a repository without dumping huge files into the prompt. It indexes your code, finds relevant chunks by meaning, returns compact file outlines, and hooks into AI tools to replace noisy reads and command output with smaller, more useful context. Works with Claude Code, GitHub Copilot, OpenAI Codex CLI, OpenCode, Antigravity, and any MCP client. **No Ollama, no Python, no external services.**

```
Without tokenix:  Read(src/hook.rs)        → 1,518 lines → 13,498 tokens
With tokenix:     tokenix read src/hook.rs → symbol outline →  2,395 tokens
```

---

## 📏 What we measure — and what we don't

Most tools in this category advertise a savings percentage. We want to be precise
about what ours means, because the distinction turns out to matter a great deal.

**Everything below is a count of tokens removed from a payload before it reaches
the model.** It is measured, reproducible, and it is *not* a claim about your
bill.

| What | Baseline → tokenix | Tokens removed | Reproduce |
|---|---|---|---|
| Real sessions (7,807 hook calls) | 475,360 → 169,175 | **67.4%** | `tokenix gain` |
| Read interception, 31 real files | 346,892 → 58,154 | **83.2%** | `tokenix benchmark` |
| Task context vs reading the full file | 86,291 → 8,630 | **90.0%** | `tokenix benchmark` |
| Outline + targeted symbol workflow | 55,020 → 17,384 | **68.4%** | `tokenix benchmark` |
| Command filters, verbose output | 1,891 → 369 | **80.5%** | `cargo test verbose_real_output -- --nocapture` |
| Command filters, full golden corpus (1,146 cases) | 47,237 → 27,836 | **41.1%** | `cargo test filters_deliver_aggregate_token_savings -- --nocapture` |

### Why we don't convert those into a dollar figure

Published work measuring *provider-billed* cost for hook-based compressors on
Claude Code found that token reduction and cost reduction are close to
uncorrelated ([arXiv:2607.12161](https://arxiv.org/abs/2607.12161), 2,848 paired
runs): one arm removed 38.4% of tool-output tokens and billed **6.8% more**, with
a per-task correlation of r = 0.15. The reason is that prompt-cache traffic
dominates a real bill, so tokens removed from a fresh payload are not the tokens
you are mostly paying for. An independent 425-trial study of a competing tool
measured a **+7.6%** cost increase while that tool's own analytics reported 96
million tokens saved.

We have not yet measured tokenix against provider-billed cost. Until we have,
this README will report tokens and call them tokens. Cache-aware cost accounting
is the top item on the roadmap; see
[`docs/research/2026-08-token-economy.md`](docs/research/2026-08-token-economy.md)
for the full evidence review that led to this position.

### Quality is measured alongside the savings

Compression is worth nothing if the agent then answers wrong, so the same
benchmark checks retrieval:

| Check | Result |
|---|---|
| Expected file in the top 3 results (8 labeled queries) | **8/8** |
| Expected file ranked #1 | 6/8 |
| Budgeted context still contained the expected file | **8/8**, 0 budget violations |
| Golden filter cases reproducing byte-exact expected output | **1,146/1,146** |

### Reading the numbers honestly

- **The 41.1% corpus figure is deliberately pessimistic.** Half the golden corpus
  is failure-path cases a filter must pass through *unfiltered*, so errors are
  never masked as success. Filters are not supposed to compress those.
- **`tokenix benchmark`'s own command arm reports a low ~26%** because its sample
  commands emit trivial output (27–148 tokens each). Compression has nothing to
  work with there.
- **Semantic Grep is never counted as savings.** The native grep output is unknown
  before interception, so `tokenix gain` logs it as neutral usage.
- **Runs that saved nothing are in the denominator.** Every command routed through
  `tokenix run` is measured, including the ones that came back the same size.
  Earlier builds logged only the runs that shrank, which answered "how much do we
  save when we save" — a percentage that cannot go down. The 67.4% row above was
  measured under the old method and reads high for that reason; it will be
  re-measured before the next release.
- **Savings depend on your codebase, file sizes, and agent behavior.** Run
  `tokenix gain` to see measured numbers on your machine rather than ours.

---

## 🖥 Dashboard

Run bare `tokenix` to open a terminal dashboard — twelve tabs, zero flags.
`←`/`→` switch tabs, `↑`/`↓` move, `q` quits. Piped or non-TTY falls back to
`--help`.

**There is only one human interface.** Typing a report command on a terminal opens
the dashboard on that command's tab instead of printing a second rendering of the
same data — `tokenix doctor` lands on Doctor, `tokenix scan-secrets` on Secrets,
`tokenix filter list` on Studio. Everything scriptable is untouched: piping,
`--json`, `--statusline`, `--format`, `--output`, and any flag a tab cannot
represent keep the plain text output. `--no-tui` (or `TOKENIX_NO_TUI=1`) forces it
explicitly. Agent-facing commands — `hook`, `run`, `mcp`, `query`, `read`, `pack` —
never open a UI.

| Tab | What it shows |
|---|---|
| **Stats** | Version, per-agent hook status, index summary, and one-key actions: index repo · install hooks · install binary on PATH |
| **Gain** | Tokens removed with a reduction bar, split by source and by command. `c` adds a ≈USD table at list input rates · `a` all-projects · `r` refresh |
| **Usage** | Absolute token spend and ≈USD cost read from agent transcripts. `s` cycles daily · model · 5-hour blocks · project · session |
| **Filters** | All 528 bundled filters by tool, with a live input → output preview and a per-filter token gauge |
| **Studio** | Record → preview → generate filters. Ranks the biggest unfiltered token sinks first (`⚠`), marks filtered commands (`✓`) and recordings (`●`) |
| **Secrets** | Credentials found in agent transcripts, grouped by rule, attributed to repo + branch. `v` reveal · `c` copy · `x` redact |
| **Egress** | External DNS/IP destinations in transcripts, validated against local reputation lists |
| **Graph** | Repo-wide symbol-graph overview: god nodes, bottlenecks, blast-radius leaders |
| **Tokenmap** | The repository as a tree weighted by token count, heaviest paths first |
| **Discover** | Replays the current filter set over historical agent output: savings you could have had, plus uncovered commands |
| **Audit** | MCP/tool weight of the effective system prompt per agent, plus always-on instruction files and skills |
| **Doctor** | Build/GPU support, detected GPU + CUDA/cuDNN status, active embedding model, bundled-filter inventory |

<table>
<tr>
<td width="50%"><img src=".github/prints/stats.png" alt="Stats tab" /></td>
<td width="50%"><img src=".github/prints/gain.png" alt="Gain tab" /></td>
</tr>
<tr>
<td><img src=".github/prints/filters.png" alt="Filters tab" /></td>
<td><img src=".github/prints/secrets.png" alt="Secrets tab" /></td>
</tr>
</table>

---

## ⚡ Install

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

```bash
# macOS / Linux — swap the asset for your platform
curl -fsSL https://github.com/juninmd/tokenix/releases/latest/download/tokenix-linux-x86_64 -o tokenix
chmod +x tokenix && sudo mv tokenix /usr/local/bin/tokenix
tokenix doctor   # verify
```

```powershell
# Windows
irm https://github.com/juninmd/tokenix/releases/latest/download/tokenix-windows-x86_64.exe -OutFile tokenix.exe
```

> **🤖 For AI agents / LLMs:** prefer the prebuilt binary over `cargo install` (no
> Rust toolchain, no compile step). Always fetch the version-less URL — it
> redirects to the latest release, so **never hard-code a version**. Detect the
> platform, download the matching asset, mark it executable, then run
> `tokenix doctor`. The embedding model downloads automatically on first use.

### From crates.io or source

```bash
cargo install tokenix --locked
```

```bash
git clone https://github.com/juninmd/tokenix && cd tokenix
cargo install --path . --locked
```

> **Use `--locked`.** It builds against the committed `Cargo.lock`; without it
> `cargo install` re-resolves dependencies and can pull an incompatible `ureq`
> into the `ort-sys` build script.

> **Requirements:** a recent stable [Rust](https://www.rust-lang.org/tools/install)
> toolchain (edition 2021). The embedding model (`nomic-embed-text-v1.5`, ~130 MB)
> is downloaded automatically on first use and cached locally.

### Get started

```bash
tokenix index .          # index the repo
tokenix install-hook     # wire up your agents
tokenix                  # open the dashboard
```

---

## 🚀 How it works

tokenix is a context layer between the agent and your repository. It does four
jobs:

| Job | What tokenix does | Why it matters |
|---|---|---|
| **Index the repository** | Walks source files, splits them into symbol-aware chunks, stores local embeddings in SQLite | The agent searches by intent instead of opening files blindly |
| **Read files compactly** | Returns outlines, symbols, or line ranges instead of full files when possible | Large files stop consuming thousands of unnecessary tokens |
| **Intercept assistant tools** | Hooks in before large reads and rewrites noisy command output | Optimization happens automatically during normal sessions |
| **Measure** | Logs hook decisions and reports token reduction where the original is known | You can see whether it helps on your codebase |

tokenix is not a cloud service, not a vector database server, and not a
replacement for your AI assistant.

### Interception

The `PreToolUse` hook sees each tool call before it runs:

- **Large file reads** → replaced with a symbol outline, with the full content
  recoverable on demand.
- **Semantic-looking Grep queries** → answered from the local index instead of a
  literal scan.
- **Noisy commands** → rewritten to run through `tokenix run`, which applies the
  matching output filter.

Everything else passes through untouched.

### Output compression

Command output is compressed **at write time** — before it enters the
conversation — so an already-cached prefix is never rewritten. Filters are
deterministic TOML rules, never a model. Engine guarantees:

- **Failures are never masked.** A non-zero exit suppresses success sentinels, and
  a filter that could empty a failure payload must preserve failure markers.
- **Never worse.** A filtered result never costs more bytes than the raw output.
- **Line endings are preserved.** CRLF input comes back CRLF, so content the agent
  may quote into an exact-match edit still matches the bytes on disk.
- **Repo-local filters are trust-gated.** `.tokenix/filters` is skipped until
  `tokenix trust` pins its SHA-256, so a cloned repo cannot rewrite what the agent
  sees.

---

## 🔌 Supported AI tools

| Tool | Integration |
|---|---|
| **Claude Code** | Native `PreToolUse` hook |
| **GitHub Copilot** | `.github/copilot-instructions.md` + `.github/hooks/hooks.json` |
| **OpenAI Codex CLI** | Shell helpers (`tx-read`, `tx-query`); on Windows also `hooks.json` |
| **OpenCode** | MCP server registration in `opencode.json` |
| **Antigravity** | Native plugin with `toolCall` payload handling |
| **Any MCP client** | `tokenix mcp` (`--profile slim` advertises a reduced tool surface) |

---

## 🔧 Setup by tool

### Claude Code

```bash
tokenix install-hook --tool claude-code
```

Writes a `PreToolUse` hook to `~/.claude/settings.json` (or
`.claude/settings.local.json` with `--local`). Large reads, semantic greps, and
noisy Bash commands are intercepted automatically — no prompt changes needed.

### GitHub Copilot

```bash
cd my-project
tokenix install-hook --tool copilot
git add .github/ && git commit -m "chore: add tokenix context instructions"
```

Creates `.github/copilot-instructions.md` and `.github/hooks/hooks.json`.

### OpenAI Codex CLI

```bash
tokenix install-hook --tool codex
echo 'source ~/.codex/tokenix-init.sh' >> ~/.bashrc   # bash / zsh
echo '. ~/.codex/tokenix-init.ps1' >> $PROFILE        # PowerShell
```

Then use `tx-read` and `tx-query` as shell helpers. On Windows this also installs
`~/.codex/hooks.json` and a PowerShell wrapper that forwards `PreToolUse`
intercepts for Bash-like terminal tools (`Bash`, `run_in_terminal`) and normalizes
`grep_search` to the same semantic path as `Grep`.

### OpenCode

```bash
tokenix install-hook --tool opencode
```

Writes an `mcp.tokenix` entry to `opencode.json` in the repository root. This
integration is **MCP-only** — tokenix does not install OpenCode
`experimental.hook` entries and does not emulate Claude-style hooks there. The
config expects `tokenix` on `PATH`; run `tokenix install-binary` first if needed.

### Antigravity

```bash
tokenix install-hook --tool antigravity
tokenix install-hook --tool antigravity --local   # workspace-only
```

Global installation uses `agy plugin install` and stores the plugin under
`~/.gemini/config/plugins/tokenix/`. Workspace installation writes
`.agents/plugins/tokenix/` and validates it with `agy plugin validate`.

### All tools at once

```bash
tokenix install-hook --tool all
```

`--tool all` intentionally skips OpenCode — request it explicitly when you want a
repo-local `opencode.json` MCP registration.

---

## 📖 Commands

> Run bare `tokenix` (or `tokenix --help`) for the same catalog with examples.

### 🤖 AI agent commands

| Command | Description |
|---|---|
| `tokenix context TEXT` | One-call task context: entry points, relevant source, compact outlines, strict budget modes |
| `tokenix explore TEXT` | Graph-aware exploration: entry points, relationships, grouped source |
| `tokenix query TEXT` | Semantic search over indexed chunks |
| `tokenix grep PATTERN` | Exact regex/literal search over indexed content (no embedding) |
| `tokenix read FILE` | Smart reader — outline for large files, full for small (`--symbol`, `--lines`, `--mode full\|outline\|signatures\|diff\|density:X`) |
| `tokenix symbols QUERY` | Find indexed symbols by name or path (`--kind` filters by symbol type) |
| `tokenix callers SYMBOL` | Symbols that call/reference a symbol |
| `tokenix callees SYMBOL` | Symbols called/referenced by a symbol |
| `tokenix deps FILE` | File-level import dependencies (`--reverse`, `--transitive`, `--json`) |
| `tokenix impact SYMBOL` | Bidirectional impact graph (`--format html\|mermaid`) |
| `tokenix flow SYMBOL` | Forward call-flow trace (`--depth`, `--format text\|mermaid`) |
| `tokenix graph` | Repo-wide symbol-graph overview — god nodes, bottlenecks, blast-radius leaders, modules (`--format text\|dot\|json`, `--top N`) |
| `tokenix modules` | Functional modules found by community detection over the symbol graph (`--top`, `--json`) |
| `tokenix blast` | Blast radius of the current diff — changed symbols and everything that calls them (`--since REF`, `--depth`, `--json`) |
| `tokenix pack` | Budgeted repo pack for non-hook AI tools (`--mode/--profile`, `--changed`, `--token-map`) |
| `tokenix memory add\|list\|remove\|edit` | Save preferences (`--global` / `--project`) for future context |

### 🧑 Human commands

| Command | Description |
|---|---|
| `tokenix` (no args) | Open the [dashboard](#-dashboard); piped/non-TTY falls back to help |
| `tokenix index [PATH]` | Index the repo at PATH (default `.`) |
| `tokenix export-index` | Write the index to a shareable snapshot (`.tokenix/index.db.gz`) teammates can commit |
| `tokenix import-index` | Bootstrap this repo's index from a snapshot instead of indexing from zero (`--force`) |
| `tokenix install-hook` / `remove-hook` | Install or remove assistant hooks/instructions (default `--tool all`) |
| `tokenix install-binary` | Copy the running executable to a per-user bin dir and ensure it is on PATH |
| `tokenix doctor` | Diagnose embedding backend, GPU, model cache, daemon, filter inventory, and filter config |
| `tokenix serve` / `stop` | Start or stop the background embedding daemon |
| `tokenix daemon status\|stop\|restart` | Inspect (pid, port, uptime, model, cache RAM) or control the daemon |
| `tokenix gain` | Tokens removed, split by source — Read interception vs command filters; semantic Grep counts as neutral usage. Reports session shape (sessions, calls/session, within-session re-requests — the measurable share of the "+turns" inversion mechanism) and (`--cost-estimate`, `--economics`) |
| `tokenix discover` | Replay current filters over historical agent output — measured recoverable savings plus uncovered commands (`--agent`, `--top`, `--json`) |
| `tokenix retrieve KEY` | Print the exact original output a compressed run stashed; the key comes from a `[tokenix: ...]` marker |
| `tokenix trust` / `untrust` | Approve (SHA-256 pinned) or revoke this repo's `.tokenix/filters` (`--status`) |
| `tokenix usage` | Absolute token spend + ≈USD from agent transcripts (`daily\|weekly\|monthly\|session\|model\|project\|blocks`, `--all-projects`, `--statusline`, `--json`) |
| `tokenix stats` | Index statistics (files, chunks, tokens, age) |
| `tokenix tokenmap` | Directory tree weighted by token count, heaviest paths first (`--format html`) |
| `tokenix benchmark` | Reproducible token-reduction and retrieval-quality benchmark — vanilla vs tokenix (`--json`) |
| `tokenix filter list\|active\|generate\|record\|verify` | Browse, generate, record, and golden-test output filters |
| `tokenix prompt-audit` | Audit MCP/tool token weight across agents (`--agent`, `--recommend`, `--profile-impact`, `--json`) |
| `tokenix session-audit` | Health check: index, hook events, MCP/tool weight, cache hygiene |
| `tokenix conversation-audit` | Scan local conversation histories for token-waste patterns (`--generate` prints ready-to-run filter commands) |
| `tokenix scan-secrets` | Scan agent transcripts for exposed credentials, gitleaks-style, attributed to repo + branch (`--group`, `--reveal`, `--json`) |
| `tokenix egress-audit` | Scan agent transcripts for external DNS/IP destinations, validated against local reputation lists |
| `tokenix artifacts list\|show` | Context artifacts from `.tokenix/artifacts.json` |
| `tokenix cycles` | Detect circular dependencies (Tarjan's SCC) |
| `tokenix rebuild-graph` | Rebuild graph tables from existing chunks without re-embedding |

### ⚙ Internal (invoked by hooks, not by hand)

| Command | Description |
|---|---|
| `tokenix hook` | `PreToolUse` handler — intercepts large reads, semantic grep, noisy commands |
| `tokenix hook-post` | `PostToolUse` compatibility handler |
| `tokenix run "CMD"` | Run a command and compress its output (`--shell`, `--path/-p`, `--raw`) |
| `tokenix mcp` | MCP server exposing context, read/search, graph, and gain tools (`--profile slim\|full`) |

<details>
<summary>Selected flags</summary>

**Global** — `--only-cpu` (force CPU embedding), `--no-tui` (plain text instead of
the dashboard, same as `TOKENIX_NO_TUI=1`), `TOKENIX_BRANCH_AWARE=true` (suffix the
SQLite DB per git branch), `TOKENIX_DISABLED=1` (bypass the hook for one command),
`TOKENIX_MAX_OUTPUT_TOKENS` (global output ceiling, default 8000, `0` disables).

**`tokenix run`** — `--raw` (also `TOKENIX_RAW=1`) prints the command's
stdout/stderr byte-for-byte, skipping compression, dedup and the recall stash.
Use it when a script or pipeline consumes `tokenix run`'s output directly and
needs the exact original bytes — auto-detecting that case is not reliable (the
agent harness that normally calls `tokenix run` also reads its stdout through a
pipe, indistinguishable from a script doing the same), so this is an explicit
opt-out rather than automatic detection.

**Freshness** — every retrieval command (`query`, `context`, `explore`, `grep`,
`symbols`, `callers`, `callees`, `impact`, `flow`, `deps`, `graph`, `modules`,
`blast`) and the equivalent MCP tools reconcile the working tree before
answering: files changed since the last index are re-chunked into the text index
and the symbol graph, without embedding them (milliseconds, no model load). Those
files are marked so the next `tokenix index` embeds them. `TOKENIX_AUTO_REFRESH=0`
turns it off; `TOKENIX_AUTO_REFRESH_MAX` (default 25) is the file count past which
a change set is left for a real index run.

**Dedup** — repeated identical command output collapses to a one-line pointer.
`TOKENIX_DEDUP=0` disables it, `TOKENIX_DEDUP_MIN_TOKENS` sets the floor (default
200), `TOKENIX_DEDUP_TTL` how long an earlier run stays usable (default 3600 s).
Matches are scoped to the current project and verified byte-for-byte against the
stash, so a pointer is only emitted for output this checkout really produced.
Re-read suppression is separate: `TOKENIX_READ_DEDUP=0`,
`TOKENIX_READ_DEDUP_TTL` (900 s), `TOKENIX_READ_DEDUP_MIN_TOKENS` (1500).

**`tokenix index`** — `--force/-f`, `--cpu-profile <low|default|max>`, `--jobs N`,
`--embed-batch N` (default 16 CPU / 64 GPU), `--if-stale`, `--path/-p`,
`--model <id>`, `--no-low-priority` (indexing runs below-normal priority by
default).

**Embedding model** — default `nomic-v1.5`. Select with `tokenix index --model <id>`
or `TOKENIX_EMBED_MODEL=<id>`; `tokenix doctor` lists ids (`nomic-v1.5`,
`bge-small`, `bge-base`, `minilm-l6`, `e5-small`, `jina-code`). The model is
stamped into the index and read back at query time, so search always matches what
was indexed. It is sticky across re-indexes; an explicit switch re-embeds.
`nomic-v1.5` (768d) is the quality default, `bge-small` (384d) indexes faster,
`e5-small` is multilingual, `jina-code` is code-specialized (custom ONNX
downloaded on first use).

**`tokenix query`** — `--budget/-b` (1200), `--k` (20), `--file/-f`, `--link`
(cross-project, repeatable), `--json`, `--path/-p`

**`tokenix context`** — `--mode <plan|debug|audit|security|review>`, `--budget/-b`
(1200), `--max-files`, `--budget-breakdown`, `--json`, `--path/-p`

**`tokenix symbols`** — `--limit/-l` (20), `--kind/-k`, `--json`, `--path/-p`

**`tokenix grep`** — `--limit/-l` (20), `--ignore-case/-i`, `--file/-f`, `--path/-p`

**`tokenix deps`** — `--reverse`, `--transitive`, `--json`, `--path/-p`

**`tokenix impact`** — `--depth/-d` (2), `--limit/-l` (50),
`--format <text|html|mermaid|json>`, `--output/-o`, `--path/-p`

**`tokenix flow`** — `--depth/-d` (3), `--limit/-l` (50), `--format <text|mermaid>`

**`tokenix blast`** — `--since REF` (default `HEAD`, e.g. `origin/main`),
`--depth/-d` (2), `--limit/-l` (50), `--json`, `--path/-p`

**`tokenix modules`** — `--top` (12), `--json`, `--path/-p`

**`tokenix export-index` / `import-index`** — `--output/-o` and `--input/-i`
(default `.tokenix/index.db.gz`), `--force` on import to replace a newer local
index. The snapshot is a compacted copy of the index with the local embedding
cache stripped; import refuses anything that is not a tokenix index, keeps the
previous DB as `*.pre-import.bak`, and leaves the snapshot's git fingerprint in
place so `tokenix index` only has to catch up on the diff.

**`tokenix pack`** — `--mode/--profile <plan|debug|audit|security|review>`,
`--budget N` (8000), `--format <markdown|xml|json>`, `--changed`, `--since REF`,
`--token-map`, `--output/-o`

**`tokenix install-hook` / `remove-hook`** —
`--tool <claude-code|copilot|codex|mcp|opencode|antigravity|all>` (default `all`),
`--local`

**`tokenix scan-secrets`** —
`--agent <claude|gemini|copilot|antigravity|all>`, `--filter <substr>`,
`--group <none|value|rule|agent|file|repo>`, `--reveal`, `--json`. Scans each
agent's transcripts under `~`. Output is redacted by default; exit code is `1`
when findings exist. Rules are TOML `[[rules]]` (`id`, `pattern`, optional
`capture`/`min_entropy`): bundled defaults in `assets/secret-rules/`, extended by
`<repo>/.tokenix/secret-rules/*.toml` then `~/.tokenix/secret-rules/*.toml` (later
sources win on matching `id`).

**`tokenix egress-audit`** — `--agent`, `--filter`, `--group <host|rule|agent|file>`,
`--safe`, `--json`. Local reputation lists live in `~/.tokenix/safe-hosts.toml` and
`~/.tokenix/dangerous-hosts.toml`.

**`tokenix conversation-audit`** — `--agent`, `--min-chars N` (5000), `--limit N`
(30), `--json`, `--generate`

**`tokenix prompt-audit`** — `--agent`, `--json`, `--recommend`, `--profile-impact`

**`tokenix benchmark`** — `--budget N` (1200), `--json`, `--refresh-index`, `--cases FILE`

</details>

---

## 🔧 Output filters

528 bundled filters, 1,146 golden cases. A filter is a TOML file matching a
command and shaping its output:

```toml
match_command = '^cargo test'
strip_lines_matching = ['^\s*Compiling ', '^\s*Finished ']
priority_lines = ['(?i)^error', '^test result:']
tail_lines = 40
on_empty = "cargo test: all tests passed"
passthrough_when_emptied = true

[[tests.all_pass]]
input = """..."""
expected = """cargo test: all tests passed"""
```

Filters resolve in order: `<repo>/.tokenix/filters` (trust-gated) →
`~/.tokenix/filters` → bundled. Every bundled filter ships **≥2 embedded golden
cases**, enforced in CI along with never-mask-failure and no-inflation checks.
`tokenix filter generate` drafts one from recorded output via a detected AI CLI;
`tokenix filter verify` runs your own filters' golden cases through the real
pipeline.

Failed commands with clipped output tee the raw text to `~/.tokenix/tee/` with a
`[full output (credentials masked): path]` hint (`TOKENIX_TEE=0` disables).

A **successful** command whose output was clipped by more than 500 bytes gets a
recovery hint instead: `[tokenix: N bytes not shown — tokenix retrieve <key> …]`.
Compression is never a one-way door — the raw text is stashed either way.

---

## 🧠 Supported languages

Rust, Python, JavaScript, TypeScript, Go, Java, C, C++, C#, Ruby, PHP, Kotlin,
Swift, Scala, Bash, PowerShell, SQL, HTML, CSS, Markdown, JSON, YAML, TOML.

Symbol-aware chunking and the reference graph use tree-sitter where a grammar is
available; other files are chunked by structure. Add a custom extension →
language mapping in `.tokenix.toml`.

---

## 🏗 Architecture

```
┌─────────────┐   PreToolUse    ┌──────────────┐
│  AI agent   │ ──────────────▶ │ tokenix hook │
└─────────────┘                 └──────┬───────┘
                                       │
                    ┌──────────────────┼──────────────────┐
                    ▼                  ▼                  ▼
             ┌────────────┐     ┌────────────┐     ┌────────────┐
             │ SQLite     │     │  Symbol    │     │  Output    │
             │ index +    │     │  graph +   │     │  filters   │
             │ embeddings │     │  PageRank  │     │  (TOML)    │
             └────────────┘     └────────────┘     └────────────┘
```

- **Storage** — one SQLite DB per project under `~/.tokenix/`, int8-quantized
  embeddings, FTS5 for lexical search. `tokenix export-index` turns it into a
  committable snapshot so a team indexes once, not once per developer.
- **Freshness** — retrieval never answers from an index the working tree has
  moved past: dirty files are re-chunked into the text index and symbol graph
  first, and marked for embedding on the next real index run.
- **Embeddings** — in-process ONNX via `fastembed`. No daemon required; if
  `tokenix serve` is running it keeps the model in RAM and answers over a local
  socket, otherwise the hook embeds in-process. The socket is bound to
  `127.0.0.1` **and** authenticated with a capability token in
  `~/.tokenix/daemon.token` (mode `0600`, regenerated on every start), because on
  a shared host loopback alone would let any other local account read your
  indexed source.
- **GPU (opt-in)** — DirectML on Windows, CUDA 12.x + cuDNN 9.x on Linux/Windows.
  `tokenix doctor` reports what is detected.
- **Local data** — everything tokenix writes lives under `~/.tokenix/`. Command
  lines and command output are masked for credential shapes before they are
  persisted (hook log, failure tee), and files are created owner-only (`0600`).
  `tokenix retrieve` blobs are the deliberate exception: they must return the
  exact original bytes, so they rely on permissions alone.

---

## 🔒 Security

- **Everything is local.** No code, prompt, or transcript leaves your machine. The
  only network access is the one-time embedding-model download, pinned to a commit
  SHA on the hub so the weights you get are the weights we tested.
- **Repo-local filters are untrusted by default** and skipped until `tokenix trust`
  pins their SHA-256 — a cloned repository cannot silently rewrite what your agent
  sees.
- **Secrets are never indexed.** `.env`, `.pem`, and similar files are excluded,
  and `tokenix scan-secrets` redacts by default.
- **Credentials are masked before anything is persisted.** Command lines and
  failing-command output are the places tokens actually appear, so the hook log and
  the failure tee are redacted on write, and `~/.tokenix` is owner-only (`0600`).
- **The embedding daemon is authenticated**, not merely bound to loopback: on a
  shared host any local account can reach a `127.0.0.1` port, so `search` requires
  the capability token from `~/.tokenix/daemon.token`.
- **Supply chain** — releases publish `sha256sums.txt` and SLSA provenance;
  workflows are SHA-pinned and covered by `cargo-deny`, `zizmor`, and OpenSSF
  Scorecard. See [SECURITY.md](SECURITY.md).

---

## 🤝 Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). New filters need ≥2 golden cases;
`AGENTS.md` documents the engine invariants a filter must not break.

## 📄 License

MIT — see [LICENSE](LICENSE).
