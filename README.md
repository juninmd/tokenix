<div align="center">
  <img src=".github/prints/logo.jpg" alt="tokenix logo" width="560" style="max-width: 100%; border-radius: 14px; box-shadow: 0 8px 32px rgba(0, 0, 0, 0.2);" />

  <h1>tokenix</h1>

  <p><strong>Give your AI coding agent the exact slice of the repo it needs — not the whole file.</strong></p>

  <p><em>Local ONNX semantic search · Tree-sitter symbol graph · Surgical reads · Deterministic output filters · Native agent hooks</em></p>

  <p>
    <a href="https://github.com/juninmd/tokenix/releases"><img src="https://img.shields.io/github/v/release/juninmd/tokenix?style=flat-square&color=ff7700&label=release" alt="Latest Release" /></a>
    <a href="https://crates.io/crates/tokenix"><img src="https://img.shields.io/crates/v/tokenix?style=flat-square&color=e05d44&label=crates.io" alt="crates.io" /></a>
    <a href="https://crates.io/crates/tokenix"><img src="https://img.shields.io/crates/d/tokenix?style=flat-square&color=blue&label=downloads" alt="crates.io downloads" /></a>
    <a href="https://github.com/juninmd/tokenix/stargazers"><img src="https://img.shields.io/github/stars/juninmd/tokenix?style=flat-square&color=yellow" alt="GitHub stars" /></a>
    <a href="https://github.com/juninmd/tokenix/actions/workflows/rust.yml"><img src="https://img.shields.io/github/actions/workflow/status/juninmd/tokenix/rust.yml?branch=main&style=flat-square&label=CI" alt="CI" /></a>
    <a href="https://scorecard.dev/viewer/?uri=github.com/juninmd/tokenix"><img src="https://img.shields.io/ossf-scorecard/github.com/juninmd/tokenix?style=flat-square&label=scorecard" alt="OpenSSF Scorecard" /></a>
    <a href="https://github.com/juninmd/tokenix/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue?style=flat-square" alt="License" /></a>
    <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/built%20with-Rust%20(MSRV%201.90)-dea584?style=flat-square&logo=rust" alt="Built with Rust" /></a>
    <img src="https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20Windows-555555?style=flat-square" alt="Platforms" />
  </p>

  <p>
    <a href="#-quick-start"><b>⚡ Quick start</b></a> ·
    <a href="#-features-at-a-glance"><b>✨ Highlights</b></a> ·
    <a href="#-using-tokenix-day-to-day"><b>🧭 Guide</b></a> ·
    <a href="#-agent-guides"><b>🔌 Agent guides</b></a> ·
    <a href="#-dashboard"><b>🖥️ Dashboard</b></a> ·
    <a href="#-command-reference"><b>📖 Commands</b></a> ·
    <a href="#-troubleshooting"><b>❓ Troubleshooting</b></a>
  </p>
</div>

---

> ⚡ **tokenix** is a high-performance Rust CLI that sits directly between your AI coding agent and your repository. It indexes your codebase locally, and when an agent reaches for an unwieldy 1,500-line file or triggers a verbose 10,000-line test run, tokenix hands back only the symbol outline, targeted function, or pertinent errors.

```text
┌─────────────────────────────────────────────────────────────────────────────────────────────┐
│ Without tokenix:  Read(src/hook.rs)        → 1,518 lines  → 13,498 tokens (bloats context) │
│ With tokenix:     tokenix read src/hook.rs → outline      →  2,395 tokens (82.3% saved!)   │
└─────────────────────────────────────────────────────────────────────────────────────────────┘
```

## ✨ Features at a glance

| ⚡ Local Semantic Retrieval | 🌲 Tree-sitter Symbol Graph | ✂️ Deterministic Output Filters |
|:---|:---|:---|
| Fast on-device ONNX embeddings with int8 quantized cosine search and SQLite FTS5 hybrid RRF ranking. Zero external services, zero latency. | Deep AST symbol parsing with PageRank importance, bidirectional caller/callee tracing, and diff blast-radius impact analysis. | 528+ bundled filters stripping noisy build/test logs without masking real errors or exit codes. |

| 🛡️ Zero-Trust Security | 📉 Measured Token Savings | 🔌 Universal Agent Compatibility |
|:---|:---|:---|
| In-flight credential masking on hooks and failure tees. Repo-controlled filters are isolated behind SHA-256 trust gates. | 40% to 90% real payload reduction before prompt-cache writes. 100% fail-open contract (always exit 0 on errors). | Native hook support for Claude Code, OpenAI Codex, GitHub Copilot, Antigravity, OpenCode, and any MCP client. |

---

## ⚡ Quick start

> [!NOTE]
> Four simple steps, about three minutes. Step 2 is the only one that takes a moment initially, as the first index downloads the compact local embedding model (~130 MB, cached permanently).

### 1️⃣ Install the binary

```bash
# macOS / Linux: pick the asset for your platform (table below)
curl -fsSL https://github.com/juninmd/tokenix/releases/latest/download/tokenix-linux-x86_64 -o tokenix
chmod +x tokenix && sudo mv tokenix /usr/local/bin/tokenix
```

```powershell
# Windows
irm https://github.com/juninmd/tokenix/releases/latest/download/tokenix-windows-x86_64.exe -OutFile tokenix.exe
.\tokenix.exe install-binary      # copies itself to a per-user bin dir on PATH
```

### 2️⃣ Index your repository

```bash
cd your-project
tokenix index .
```

### 3️⃣ Connect your agent

```bash
tokenix install-hook --tool claude-code   # or codex · copilot · antigravity · opencode · all
```

Each agent is wired a little differently. The [agent guides](#-agent-guides) cover
what gets installed, what gets intercepted and how to undo it.

### 4️⃣ Verify that it works

```bash
tokenix doctor     # binary, model, GPU, daemon, filters
tokenix            # dashboard → Stats tab: which agents are wired
tokenix gain       # after a session: hook calls, intercepts, tokens removed
```

Then work with your agent as usual. You do not need to change your prompts or habits.

<details>
<summary><b>All install options</b> (platform assets, crates.io, source, verification)</summary>

Every release ships one binary per platform. The version-less
`releases/latest/download/<asset>` URL **always resolves to the newest release**.

| Platform | Asset |
|---|---|
| Linux x86_64 | `tokenix-linux-x86_64` |
| Linux arm64 | `tokenix-linux-aarch64` |
| macOS x86_64 | `tokenix-macos-x86_64` |
| macOS arm64 (Apple Silicon) | `tokenix-macos-aarch64` |
| Windows x86_64 | `tokenix-windows-x86_64.exe` |
| Windows x86_64 (GPU / DirectML) | `tokenix-windows-x86_64-directml.exe` |

Each release also publishes `sha256sums.txt` and SLSA build-provenance attestations.
[SECURITY.md](SECURITY.md) explains how to verify a download.

```bash
cargo install tokenix --locked                     # from crates.io
git clone https://github.com/juninmd/tokenix && cd tokenix
cargo install --path . --locked                    # from source
```

Use `--locked`. It builds against the committed `Cargo.lock`. Without it, `cargo
install` re-resolves dependencies and can pull an incompatible `ureq` into the
`ort-sys` build script. Building from source needs a recent stable
[Rust](https://www.rust-lang.org/tools/install) toolchain (MSRV 1.90).

> [!TIP]
> **For AI agents installing tokenix:** Use the prebuilt binary, not `cargo install`. It needs no toolchain and no compile step. Fetch the version-less URL and **never hard-code a version**. Detect the platform, download the matching asset, mark it executable, then run `tokenix doctor`.

</details>

---

## 🧭 Using tokenix day to day

### 1. Let the agent use it automatically

After `install-hook`, tokenix sees each tool call before it runs and decides:

| The agent tries to… | tokenix does | Result |
|---|---|---|
| Read a code file of **≥ 200 lines** with no range | Returns a symbol outline (only when it saves ≥ 30%) | The agent sees the file's structure and asks for the function it needs |
| Read a small file, or read with `offset`/`limit` | Nothing | The read happens as asked |
| Grep for an **identifier** (`apply_tax`) | Answers with its definition site from the symbol graph | `src/billing.rs:5 [function] apply_tax` |
| Grep a **phrase of 3+ words** (`where is the invoice taxed`) | Answers from semantic search | Relevant chunks, ranked |
| Grep with unbounded content output | Adds `head_limit` (100 by default) | Output is capped |
| Run a noisy command (`cargo test`, `terraform plan`, `git status` …) | Reruns it through `tokenix run` and the matching filter | Failures and summaries stay, noise goes, and the **exit code is the real one** |

Anything else passes through untouched. Nothing is lost for good: see
[getting the full output back](#5-when-you-need-the-raw-output).

### 2. Ask it questions yourself

The same commands the agent uses work in your terminal. Here they run against a
two-file demo project:

```console
$ tokenix symbols apply_tax
## Symbols matching `apply_tax`
- src/billing.rs:5-7 [function] apply_tax

$ tokenix callers apply_tax
## Callers of `apply_tax`
- src/billing.rs:1 [function] invoice_total -> src/billing.rs:5 [function] apply_tax via `apply_tax` (references)

$ tokenix read src/billing.rs --symbol apply_tax
# L5-7 [function] apply_tax
fn apply_tax(amount: u32) -> u32 {
    amount + amount / 10
}
```

| You want to… | Run |
|---|---|
| Find code by meaning | `tokenix query "where is the invoice taxed"` |
| Find an exact string or regex | `tokenix grep "amount / 10"` |
| Find a symbol by name | `tokenix symbols apply_tax` |
| See a big file's structure | `tokenix read src/billing.rs` |
| Read exactly one function | `tokenix read src/billing.rs --symbol apply_tax` |
| Find who calls something, and what it calls | `tokenix callers apply_tax` · `tokenix callees main` |
| See everything a change could break | `tokenix impact apply_tax` · `tokenix flow main` |
| Get context for a task in one call | `tokenix context "add a discount to invoices" --budget 2000` |
| Hand a budgeted bundle to a tool without hooks | `tokenix pack --budget 8000 > context.md` |

### 3. Review a change before you push

`blast` maps your diff to the symbols it touches and walks the call graph up from
them:

```console
$ tokenix blast            # vs HEAD; use --since origin/main for a branch
# Blast radius vs HEAD — 1 changed file(s), 1 changed symbol(s)

## Changed symbols
- apply_tax (function)  src/billing.rs:5

## Impacted (up to 2 hop(s) of callers)
- [1] invoice_total (↑1)  src/billing.rs:1
- [2] main (↑0)  src/main.rs:11
```

### 4. Keep the index current

- **Edits are picked up on their own.** Before answering, every retrieval command
  re-chunks the files you changed since the last index. That takes milliseconds
  and loads no model, so a function you just wrote is searchable right away.
- **Embeddings catch up later.** Run `tokenix index` now and then (or
  `tokenix index --if-stale` in a script) to embed what changed.
- **Branch switches are detected.** The index records the branch and `HEAD`. When
  they change, the hook passes calls through until you re-index, so it never
  answers from the wrong branch.
- **Share one index with the team.** `tokenix export-index` writes
  `.tokenix/index.db.gz` with known secrets masked. Teammates run
  `tokenix import-index` instead of indexing from zero.

### 5. When you need the raw output

- A clipped successful run ends with `[tokenix: N bytes not shown — tokenix retrieve <key> …]`.
  `tokenix retrieve <key>` prints the exact original bytes.
- A failed run tees its full output, with credentials masked, to `~/.tokenix/tee/`,
  and prints the path.
- `tokenix run --raw "cmd"` (or `TOKENIX_RAW=1`) passes output through byte for byte.
  Use it when a script parses the output.
- `TOKENIX_DISABLED=1 <cmd>` bypasses the hook for one command.

### 6. Tune it for your project

Put a `.tokenix.toml` in the repository root. Unknown keys are reported, not
silently ignored.

```toml
[hook]
read_min_lines = 120    # outline files from 120 lines instead of 200
grep_min_words = 3      # words before a Grep is treated as semantic

[index]
exclude = ["fixtures", "vendor"]   # extra directories to skip
extensions = ["proto"]             # extra extensions to index
data_files = false                 # index .json/.yaml/.yml too
redact_secrets = true              # mask secret-shaped strings in stored chunks
max_file_bytes = 1500000           # files above this are skipped whole
```

### 7. Add your own output filters

A filter is a TOML rule, never a model. Start from your real output:

```bash
tokenix discover                     # which commands in your history cost the most
tokenix filter record "npm test"     # capture real output (redacted, git-ignored)
tokenix filter generate npm          # draft a filter with an AI CLI you have installed
tokenix filter verify                # run its golden tests through the real pipeline
```

User filters go in `~/.tokenix/filters/`. Filters committed to a repo in
`.tokenix/filters/` apply **only after `tokenix trust`**, and editing one revokes
trust until you approve it again. [Output filters](#-output-filters) covers the
format.

---

## 🔌 Agent guides

| Agent | Install | Integration | Guide |
|---|---|---|---|
| **Claude Code** | `tokenix install-hook --tool claude-code` | `PreToolUse` hook in `settings.json` | [docs/agents/claude-code.md](docs/agents/claude-code.md) |
| **OpenAI Codex CLI** | `tokenix install-hook --tool codex` | `PreToolUse` hook for commands + `tx-read`/`tx-query` helpers | [docs/agents/codex.md](docs/agents/codex.md) |
| **GitHub Copilot** | `tokenix install-hook --tool copilot` | `PreToolUse` + `PostToolUse` hooks; `--local` commits them to `.github/` | [docs/agents/copilot.md](docs/agents/copilot.md) |
| **Antigravity** | `tokenix install-hook --tool antigravity` | Native plugin | [docs/agents/antigravity.md](docs/agents/antigravity.md) |
| **OpenCode** | `tokenix install-hook --tool opencode` | MCP server only | [docs/agents/opencode.md](docs/agents/opencode.md) |
| **Any MCP client** | `tokenix mcp` | MCP server over stdio | [docs/agents/mcp.md](docs/agents/mcp.md) |

`tokenix install-hook --tool all` wires every agent except OpenCode. OpenCode's
registration lives in the repository, so you have to ask for it explicitly.
`tokenix remove-hook --tool <agent>` undoes the install.

---

## 🖥 Dashboard

Run bare `tokenix` for a terminal dashboard with twelve tabs and no flags. `←`/`→`
switch tabs, `↑`/`↓` move, `q` quits.

On a terminal, a report command opens its own tab: `tokenix doctor` lands on
Doctor, `tokenix scan-secrets` on Secrets. Piping, `--json`, `--statusline`,
`--format`, `--output` and `--no-tui` (or `TOKENIX_NO_TUI=1`) keep plain text
output. Agent-facing commands (`hook`, `run`, `mcp`, `query`, `read`, `pack`) never
open a UI.

| Tab | What it shows |
|---|---|
| **Stats** | Version, per-agent hook status, index summary, one-key actions: index repo · install hooks · install binary on PATH |
| **Gain** | Tokens removed with a reduction bar, split by source and by command. `c` adds a ≈USD table at list input rates · `a` all projects · `r` refresh |
| **Usage** | Absolute token spend and ≈USD cost from agent transcripts. `s` cycles daily · model · 5-hour blocks · project · session |
| **Filters** | All 528 bundled filters by tool, with a live input → output preview and a per-filter token gauge |
| **Studio** | Record → preview → generate filters. Ranks the biggest unfiltered token sinks first (`⚠`), marks filtered commands (`✓`) and recordings (`●`) |
| **Secrets** | Credentials found in agent transcripts, grouped by rule, attributed to repo + branch. `v` reveal · `c` copy · `x` redact |
| **Egress** | External DNS/IP destinations in transcripts, checked against local reputation lists |
| **Graph** | Repo-wide symbol graph: god nodes, bottlenecks, blast-radius leaders |
| **Tokenmap** | The repository as a tree weighted by token count, heaviest paths first |
| **Discover** | Replays the current filters over past agent output: savings you could have had, plus commands no filter covers |
| **Audit** | MCP/tool weight of each agent's effective system prompt, plus always-on instruction files and skills |
| **Doctor** | Build/GPU support, detected GPU + CUDA/cuDNN, active embedding model, bundled-filter inventory |

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

## 📏 What we measure, and what we don't

**Every number below counts tokens removed from a payload before it reached the
model.** The measurements are reproducible. They are *not* a claim about your bill.

| What | Baseline → tokenix | Tokens removed | Reproduce |
|---|---|---|---|
| Real sessions (7,807 hook calls) | 475,360 → 169,175 | **67.4%** | `tokenix gain` |
| Read interception, 31 real files | 346,892 → 58,154 | **83.2%** | `tokenix benchmark` |
| Task context vs reading the full file | 86,291 → 8,630 | **90.0%** | `tokenix benchmark` |
| Outline + targeted symbol workflow | 55,020 → 17,384 | **68.4%** | `tokenix benchmark` |
| Command filters, verbose output | 1,891 → 369 | **80.5%** | `cargo test verbose_real_output -- --nocapture` |
| Command filters, full golden corpus (1,150 cases) | 46,804 → 27,728 | **40.8%** | `cargo test filters_deliver_aggregate_token_savings -- --nocapture` |

Retrieval quality is checked by the same benchmark:

| Check | Result |
|---|---|
| Expected file in the top 3 results (8 labeled queries) | **8/8** |
| Expected file ranked #1 | 6/8 |
| Budgeted context still contained the expected file | **8/8**, 0 budget violations |
| Golden filter cases reproducing byte-exact expected output | **1,150/1,150** |

<details>
<summary><b>Why there is no dollar figure, and how to read these numbers</b></summary>

Published work measured *provider-billed* cost for hook-based compressors on Claude
Code. It found token reduction and cost reduction close to uncorrelated
([arXiv:2607.12161](https://arxiv.org/abs/2607.12161), 2,848 paired runs). One arm
removed 38.4% of tool-output tokens and billed **6.8% more**, with a per-task
correlation of r = 0.15. Prompt-cache traffic dominates a real bill, so tokens
removed from a fresh payload are mostly not the tokens you pay for. An independent
425-trial study of a competing tool measured a **+7.6%** cost increase while that
tool's analytics reported 96 million tokens saved.

We have not yet measured tokenix against provider-billed cost. Until then this
README reports tokens and calls them tokens. Cache-aware cost accounting is the top
roadmap item. [`docs/research/2026-08-token-economy.md`](docs/research/2026-08-token-economy.md)
has the full evidence review.

- **The 40.8% corpus figure is pessimistic on purpose.** Half the golden corpus is
  failure-path cases a filter must pass through *unfiltered*, so errors are never
  masked as success.
- **`tokenix benchmark`'s command arm reports about 26%** because its sample
  commands print only 27–148 tokens each, which leaves little to compress.
- **Semantic Grep is never counted as savings.** The native grep output is unknown
  before interception, so `tokenix gain` logs it as neutral usage.
- **Runs that saved nothing are in the denominator.** The 67.4% row was measured
  before that rule existed, so it reads high. It will be re-measured before the
  next release.
- **Your mileage depends on your codebase and your agent.** `tokenix gain` shows
  what happened on your machine.

</details>

---

## 📖 Command reference

> [!TIP]
> Run `tokenix --help` for the full catalog with usage examples; `tokenix <command> --help` displays all flags and parameters for any specific command.

### 🤖 Commands agents use

| Command | Description |
|---|---|
| `tokenix context TEXT` | One-call task context: entry points, relevant source, compact outlines, strict budget modes |
| `tokenix explore TEXT` | Graph-aware exploration: entry points, relationships, grouped source |
| `tokenix query TEXT` | Semantic search over indexed chunks |
| `tokenix grep PATTERN` | Exact regex/literal search over indexed content (no embedding) |
| `tokenix read FILE` | Smart reader: outline for large files, full text for small ones (`--symbol`, `--lines`, `--mode full\|outline\|signatures\|diff\|density:X`) |
| `tokenix symbols QUERY` | Find indexed symbols by name or path (`--kind` filters by symbol type) |
| `tokenix callers SYMBOL` | Symbols that call or reference a symbol |
| `tokenix callees SYMBOL` | Symbols a symbol calls or references |
| `tokenix deps FILE` | File-level import dependencies (`--reverse`, `--transitive`, `--json`) |
| `tokenix impact SYMBOL` | Bidirectional impact graph (`--format html\|mermaid`) |
| `tokenix flow SYMBOL` | Forward call-flow trace (`--depth`, `--format text\|mermaid`) |
| `tokenix graph` | Repo-wide symbol-graph overview: god nodes, bottlenecks, blast-radius leaders, modules (`--format text\|dot\|json`, `--top N`) |
| `tokenix modules` | Functional modules found by community detection over the symbol graph (`--top`, `--json`) |
| `tokenix blast` | Blast radius of the current diff: changed symbols and everything that calls them (`--since REF`, `--depth`, `--json`) |
| `tokenix pack` | Budgeted repo pack for tools without hooks (`--mode/--profile`, `--changed`, `--token-map`) |
| `tokenix memory add\|list\|remove\|edit` | Save preferences (`--global` / `--project`) for future context. Text that looks like a credential is refused |
| `tokenix retrieve KEY` | Print the exact original output behind a `[tokenix: ...]` marker |

### 🧑 Commands you run

| Command | Description |
|---|---|
| `tokenix` (no args) | Open the [dashboard](#-dashboard); piped or non-TTY falls back to help |
| `tokenix index [PATH]` | Index the repo at PATH (default `.`) |
| `tokenix export-index` / `import-index` | Write or load a shareable, secret-masked snapshot (`.tokenix/index.db.gz`) |
| `tokenix install-hook` / `remove-hook` | Install or remove agent hooks and instructions (default `--tool all`) |
| `tokenix install-binary` | Copy the running executable to a per-user bin dir and put it on PATH |
| `tokenix update` | Check for, install, or toggle automatic updates from GitHub releases (`--check`, `--auto`, `--enable-auto`, `--disable-auto`) |
| `tokenix doctor` | Diagnose embedding backend, GPU, model cache, daemon, filter inventory and filter config |
| `tokenix serve` / `stop` | Start or stop the background embedding daemon |
| `tokenix daemon status\|stop\|restart` | Inspect (pid, port, uptime, model, cache RAM) or control the daemon |
| `tokenix gain` | Tokens removed, split by source (Read interception vs command filters), plus session shape. Dollar estimates only with `--cost-estimate` / `--economics` |
| `tokenix discover` | Replay current filters over past agent output: recoverable savings plus uncovered commands (`--agent`, `--top`, `--json`) |
| `tokenix trust` / `untrust` | Approve (SHA-256 pinned) or revoke this repo's executable inputs (`--status`) |
| `tokenix usage` | Absolute token spend and ≈USD from agent transcripts (`daily\|weekly\|monthly\|session\|model\|project\|blocks`, `--all-projects`, `--statusline`, `--json`) |
| `tokenix stats` | Index statistics (files, chunks, tokens, age, files waiting for embeddings) |
| `tokenix tokenmap` | Directory tree weighted by token count, heaviest paths first (`--format html`) |
| `tokenix benchmark` | Reproducible token-reduction and retrieval-quality benchmark, vanilla vs tokenix (`--json`) |
| `tokenix filter list\|active\|generate\|record\|verify` | Browse, generate, record and golden-test output filters |

| `tokenix prompt-audit` | Audit MCP/tool token weight across agents (`--agent`, `--recommend`, `--profile-impact`, `--json`) |
| `tokenix session-audit` | Health check: index, hook events, MCP/tool weight, cache hygiene |
| `tokenix conversation-audit` | Scan local conversation histories for token-waste patterns (`--generate` prints ready-to-run filter commands) |
| `tokenix scan-secrets` | Scan agent transcripts for exposed credentials, attributed to repo + branch (`--group`, `--reveal`, `--json`) |
| `tokenix egress-audit` | Scan agent transcripts for external DNS/IP destinations, checked against local reputation lists |
| `tokenix artifacts list\|show` | Context artifacts from `.tokenix/artifacts.json` |
| `tokenix cycles` | Detect circular dependencies (Tarjan's SCC) |
| `tokenix rebuild-graph` | Rebuild graph tables from existing chunks without re-embedding |
| `tokenix generate-ignores` | Write `.gitignore` entries for tokenix artifacts |

When you open `tokenix` in a terminal, it checks GitHub releases in the background
on a 24-hour cache and automatically installs a newer release by default.
The current command keeps running; the new version is used on the next launch.
Downloads use the release's SHA-256 checksums; a GitHub token, when set, is sent
only to the releases API, never to asset downloads.
Checks are skipped in CI, piped commands, hooks, and agent-facing commands. Set
`TOKENIX_AUTO_UPDATE=notify` to receive update hints without installing, or run
`tokenix update --disable-auto` to turn off automatic installation.

### ⚙ Internal (called by hooks, not by hand)

| Command | Description |
|---|---|
| `tokenix hook` | `PreToolUse` handler: intercepts large reads, greps and noisy commands |
| `tokenix hook-post` | `PostToolUse` handler: redacts secrets and compresses tool output (agents whose install wires it, see the [agent guides](#-agent-guides)) |
| `tokenix run "CMD"` | Run a command and compress its output (`--shell`, `--path/-p`, `--raw`) |
| `tokenix mcp` | MCP server exposing context, read/search, graph and gain tools (`--profile slim\|full`) |
| `tokenix mcp-proxy -- CMD` | Wrap another stdio MCP server and compress only its `tools/call` text results |

<details>
<summary><b>Flags and environment variables</b></summary>

**Global:** `--only-cpu` forces CPU embedding. `--no-tui` (or `TOKENIX_NO_TUI=1`)
prints plain text instead of opening the dashboard.

**Environment:**

| Variable | Effect |
|---|---|
| `TOKENIX_HOME` | Absolute path that replaces `~/.tokenix` (indexes, hook log, trust store, daemon token, recall stash). Relative values are ignored. Useful for CI and throwaway runs |
| `TOKENIX_DISABLED=1` | Bypass the hook for one command |
| `TOKENIX_RAW=1` | Same as `tokenix run --raw` |
| `TOKENIX_MAX_OUTPUT_TOKENS` | Global output ceiling (default 8000, `0` disables) |
| `TOKENIX_BRANCH_AWARE=true` | One SQLite DB per git branch |
| `TOKENIX_EMBED_MODEL` | Embedding model id (see below) |
| `TOKENIX_GREP_HEAD_LIMIT` | `head_limit` injected into unbounded Grep (default 100, `0` disables) |
| `TOKENIX_AUTO_REFRESH=0` | Turn off the inline refresh before retrieval commands |
| `TOKENIX_AUTO_REFRESH_MAX` | Dirty-file count above which the inline refresh is skipped (default 25) |
| `TOKENIX_DEDUP=0` · `TOKENIX_DEDUP_MIN_TOKENS` · `TOKENIX_DEDUP_TTL` | Cross-call dedup of identical output (defaults: on, 200 tokens, 3600 s; scoped to the project) |
| `TOKENIX_READ_DEDUP=0` · `TOKENIX_READ_DEDUP_TTL` · `TOKENIX_READ_DEDUP_MIN_TOKENS` | Re-read suppression (defaults: on, 900 s, 1500 tokens; scoped to the agent session) |
| `TOKENIX_TEE=0` | Do not tee failed-command output to `~/.tokenix/tee/` |
| `TOKENIX_AUTO_UPDATE=1\|0\|notify` | Control auto-update behavior (`1`/`install` auto-updates in background; `0` disables checks) |
| `TOKENIX_NO_UPDATE=1` | Disable all update checks and auto-updates |

**`tokenix index`:** `--force/-f`, `--no-embed`, `--cpu-profile <low|default|max>`,
`--jobs N`, `--embed-batch N` (default 16 CPU / 64 GPU), `--if-stale`, `--path/-p`,
`--model <id>`, `--no-low-priority` (indexing runs at below-normal priority by
default).

**Embedding model:** the default is `nomic-v1.5` (768d). Choose another with
`tokenix index --model <id>` or `TOKENIX_EMBED_MODEL=<id>`; `tokenix doctor` lists
the ids (`nomic-v1.5`, `bge-small`, `bge-base`, `minilm-l6`, `e5-small`,
`jina-code`). `bge-small` (384d) indexes faster, `e5-small` is multilingual and
`jina-code` is code-specialized. The model is stamped into the index and read back
at query time, so search always uses the model the index was built with. An
explicit switch re-embeds everything.

**`tokenix query`:** `--budget/-b` (1200), `--k` (20), `--file/-f`, `--link`
(cross-project, repeatable), `--json`, `--path/-p`

**`tokenix context`:** `--mode <plan|debug|audit|security|review>`, `--budget/-b`
(1200), `--max-files`, `--budget-breakdown`, `--json`, `--path/-p`

**`tokenix symbols`:** `--limit/-l` (20), `--kind/-k`, `--json`, `--path/-p`

**`tokenix grep`:** `--limit/-l` (20), `--ignore-case/-i`, `--file/-f`, `--path/-p`

**`tokenix deps`:** `--reverse`, `--transitive`, `--json`, `--path/-p`

**`tokenix impact`:** `--depth/-d` (2), `--limit/-l` (50),
`--format <text|html|mermaid|json>`, `--output/-o`, `--path/-p`

**`tokenix flow`:** `--depth/-d` (3), `--limit/-l` (50), `--format <text|mermaid>`

**`tokenix blast`:** `--since REF` (default `HEAD`, e.g. `origin/main`),
`--depth/-d` (2), `--limit/-l` (50), `--json`, `--path/-p`

**`tokenix modules`:** `--top` (12), `--json`, `--path/-p`

**`tokenix export-index` / `import-index`:** `--output/-o` and `--input/-i` (default
`.tokenix/index.db.gz`), `--force` on import to replace a newer local index. The
snapshot is a compacted copy of the index with the local embedding cache stripped.
Import refuses anything that is not a tokenix index, keeps the previous DB as
`*.pre-import.bak`, and keeps the snapshot's git fingerprint so `tokenix index`
only has to catch up on the diff.

**`tokenix pack`:** `--mode/--profile <plan|debug|audit|security|review>`,
`--budget N` (8000), `--format <markdown|xml|json>`, `--changed`, `--since REF`,
`--token-map`, `--output/-o`

**`tokenix install-hook` / `remove-hook`:**
`--tool <claude-code|copilot|codex|mcp|opencode|antigravity|all>` (default `all`),
`--local`

**`tokenix scan-secrets`:** `--agent <claude|gemini|copilot|antigravity|all>`,
`--filter <substr>`, `--group <none|value|rule|agent|file|repo>`, `--reveal`,
`--json`. Output is redacted by default, and the exit code is `1` when findings
exist. Rules are TOML `[[rules]]` (`id`, `pattern`, optional `capture` /
`min_entropy`). Bundled defaults live in `assets/secret-rules/`.
`~/.tokenix/secret-rules/*.toml` may add or override rules. A repo's
`<repo>/.tokenix/secret-rules/*.toml` loads only after `tokenix trust` and may only
*add*: an id that collides with a bundled rule is dropped with a warning.

**`tokenix egress-audit`:** `--agent`, `--filter`, `--group <host|rule|agent|file>`,
`--safe`, `--json`. Local reputation lists live in `~/.tokenix/safe-hosts.toml` and
`~/.tokenix/dangerous-hosts.toml`.

**`tokenix conversation-audit`:** `--agent`, `--min-chars N` (5000), `--limit N`
(30), `--json`, `--generate`

**`tokenix prompt-audit`:** `--agent`, `--json`, `--recommend`, `--profile-impact`

**`tokenix benchmark`:** `--budget N` (1200), `--json`, `--refresh-index`, `--cases FILE`

</details>

---

## 🔧 Output filters

528 bundled filters, 1,150 golden cases. A filter matches a command and shapes its
output:

```toml
[filters.cargo-test]
match_command = '^cargo test'
strip_lines_matching = ['^\s*Compiling ', '^\s*Finished ']
priority_lines = ['(?i)^error', '^test result:']
tail_lines = 40
on_empty = "cargo test: all tests passed"
passthrough_when_emptied = true

[[tests.cargo-test]]
name = "all pass"
input = """..."""
expected = """cargo test: all tests passed"""
```

Filters resolve in this order: `<repo>/.tokenix/filters` (only after `tokenix
trust`) → `~/.tokenix/filters` → bundled. Every bundled filter ships **at least two
embedded golden cases**, and CI also enforces the rules below.

- **Failures are never masked.** A non-zero exit suppresses success messages, and a
  filter that could empty a failure payload must keep the failure markers.
- **Never worse.** A filtered result never costs more bytes than the raw output.
- **Every regex compiles.** An invalid pattern would silently disable its rule;
  CI fails on one in a bundled filter, and `tokenix doctor` lists them in yours.
- **stderr is opt-in per filter** (`filter_stderr = true`). The cargo filters set it,
  because rustc writes every diagnostic there. `block_caps` then keeps each
  warning's message and location and drops its snippet; errors keep theirs.
- **Line endings are preserved.** CRLF comes back as CRLF, so text the agent quotes
  into an exact-match edit still matches the bytes on disk.
- **Compression happens at write time,** before output enters the conversation, so
  an already-cached prompt prefix is never rewritten.

---

## 🧠 Supported languages

| Support | Languages |
|---|---|
| **Symbol-aware chunking + call graph** (tree-sitter) | Rust, Python, JavaScript, TypeScript, Go, C, C++ |
| **Symbol-aware chunking** (line-based) | Visual Basic, SQL |
| **Indexed as text** (line chunks, searchable) | Shell (`.sh`, `.bash`), Markdown, TOML, plain text; JSON/YAML with `data_files = true` |

Anything else (Java, C#, Ruby, PHP, Kotlin, Swift, PowerShell, HTML, CSS …) is
**not indexed by default**. Opt in per project and those files are indexed as text:

```toml
[index]
extensions = ["java", "cs", "rb", "php", "kt", "swift", "ps1", "inc"]

[languages]
inc = "cpp"    # parse an extension with a supported grammar: rust, python, typescript,
               # javascript, go, cpp/c, vb, sql (anything else is chunked as text)
```

Files above `max_file_bytes` (1.5 MB) and binary files are skipped whole, never
truncated.

---

## 🏗 Architecture

```
┌─────────────┐  tool call   ┌──────────────┐   pass (exit 0) / intercept (exit 2)
│  AI agent   │ ───────────▶ │ tokenix hook │ ─────────────────────────────────▶
└─────────────┘              └──────┬───────┘
                                    │
                 ┌──────────────────┼──────────────────┐
                 ▼                  ▼                  ▼
          ┌────────────┐     ┌────────────┐     ┌────────────┐
          │ SQLite     │     │  Symbol    │     │  Output    │
          │ FTS5 +     │     │  graph +   │     │  filters   │
          │ int8 vecs  │     │  PageRank  │     │  (TOML)    │
          └────────────┘     └────────────┘     └────────────┘
```

- **Storage:** one SQLite DB per project under `~/.tokenix/`, with int8-quantized
  embeddings and FTS5 for lexical search. Semantic and lexical results are fused
  with reciprocal-rank fusion.
- **Embeddings:** in-process ONNX via `fastembed`. `tokenix serve` is optional. It
  keeps the model in RAM and answers over `127.0.0.1`, authenticated with a
  capability token in `~/.tokenix/daemon.token` (owner-only, regenerated on every
  start). Loopback alone would let any other local account read your indexed
  source.
- **GPU (opt-in):** DirectML on Windows (use the `-directml` asset), CUDA 12.x +
  cuDNN 9.x on Linux/Windows when built with `--features cuda`. `tokenix doctor`
  reports what it detects.
- **Fail-open contract:** the hook exits `0` (pass) on any error, a missing or
  stale index, or even a panic. It exits `2` only to intercept, and never exits `1`.

---

## 🔒 Security

- **Your code stays local.** Indexing, search and interception make no network
  calls. The network is used for:
  - the one-time model download from Hugging Face, pinned to a commit SHA;
  - `tokenix filter generate`, which sends your recorded (redacted) output to an AI
    CLI **you** have installed, and asks before it re-runs any command.
- **Repositories are untrusted by default.** A repo's `.tokenix/filters`,
  `.tokenix/secret-rules`, `.tokenix/egress-rules`, `.mcp.json`, `opencode.json` and
  `.vscode/mcp.json` are ignored until `tokenix trust` pins their SHA-256, and any
  edit revokes that trust. A cloned repo cannot change what your agent sees, blind
  the secret scanner or make `prompt-audit` spawn a server of its choosing.
  User-scoped configs (`~/.claude.json`, `~/.codex/`) are yours and are not gated.
- **Credentials are masked before anything is saved.** The hook log, the failure
  tee, recordings and index snapshots go through one redactor, memory refuses
  notes that look like a credential, and `~/.tokenix` is owner-only (`0600`). The recall stash is the deliberate exception:
  `tokenix retrieve` must return the exact original bytes, so it relies on file
  permissions alone.
- **Secrets are not indexed.** `.env`, `.pem`, keystores and similar files are
  excluded, and `tokenix scan-secrets` redacts its output by default.
- **Supply chain:** releases publish `sha256sums.txt` and SLSA provenance.
  Workflows are SHA-pinned and checked by `cargo-deny`, `zizmor` and OpenSSF
  Scorecard. See [SECURITY.md](SECURITY.md).

---

## ❓ Troubleshooting

| Symptom | Fix |
|---|---|
| `No index found` | Run `tokenix index .` in the repository root. The nearest indexed directory counts as the project root, even when a parent directory has a `package.json`. |
| The agent still reads whole files | Open `tokenix` → **Stats** tab and check that your agent shows as installed. Only code files of ≥ 200 lines are outlined, only when that saves ≥ 30%, and never when the agent passes `offset`/`limit`. After a branch switch, re-index: a stale index makes the hook pass everything through. |
| Search misses something I just wrote | It should not: edits are re-chunked before each query. If more than 25 files changed, run `tokenix index` (or raise `TOKENIX_AUTO_REFRESH_MAX`). |
| A repo filter is ignored | Run `tokenix trust --status`. Repo filters need `tokenix trust`, and an edit revokes it. |
| Output was cut and I need all of it | Run `tokenix retrieve <key>` from the `[tokenix: ...]` marker, or `tokenix run --raw`. |
| First index is slow or the machine stalls | Use `tokenix index --cpu-profile low` or a smaller `--embed-batch`. `tokenix index --no-embed` gives you graph and text search in seconds, and the embeddings can follow later. |
| Offline or air-gapped | Copy the model cache from a connected machine (`tokenix doctor` prints its path), or use `--no-embed` and lexical search only. |
| Remove everything | `tokenix remove-hook --tool all`, then delete `~/.tokenix` (or your `TOKENIX_HOME`). |

---

## 🤝 Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). `./scripts/verify.sh` runs every CI gate
locally, in CI order: fmt, clippy, unit, golden and end-to-end tests, release build
and the homologation scripts. Add `--models` for the ONNX-backed tests. It never
touches your real `~/.tokenix`. New filters need at least two golden cases, and
[`AGENTS.md`](AGENTS.md) lists the engine rules a change must not break.

## 📄 License

MIT. See [LICENSE](LICENSE).
