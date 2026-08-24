# Token Economy Research — August 2026

Market and literature review positioning `tokenix` (baseline: v0.62.0) against the
GitHub ecosystem and the 2026 research on token economy for coding agents.

**Thesis:** the whole market crowded into *retrieval*. The literature says the
biggest share of an agent's tokens is *observation* — command output, logs, stack
traces. That is where tokenix already lives and where almost nobody else is.

> Status: pass 1 (single-researcher sweep). A five-lane deep sweep is appended in
> [Deep sweep](#deep-sweep-pass-2) below.

---

## 1. Landscape

Consolidated from third-party comparisons and project pages. Star counts and
claimed savings were **not** reproduced locally — treat as order of magnitude.

| Project | Stars | Approach | Layer | Claimed saving |
|---|---:|---|---|---|
| CodeGraph | 47.4k | AST graph in SQLite | Retrieval | 58–70% fewer tool calls |
| GitNexus | 42.0k | Graph (LadybugDB) | Retrieval | 74% tokens, 88% fewer calls |
| Repomix | 26.2k | Packing + tree-sitter | Retrieval | ~70% via compression |
| Serena | 25.2k | LSP / symbol navigation | Retrieval | not measured |
| Context Hub | 13.6k | Documentation packing | Retrieval | not measured |
| claude-context | 11.8k | Embedding + BM25 | Retrieval | not measured |
| code2prompt | 7.4k | Packing (Rust) | Retrieval | not measured · stalled since 2025-12 |
| CodeGraphContext | 3.7k | Pluggable graphs | Retrieval | not measured |
| grepai | 1.7k | Embedding + call graph (Go) | Retrieval | 97% input · **27.5% cost** |
| Octocode MCP | 0.9k | Multi-repo LSP | Retrieval | not measured |
| mcp-compressor (Atlassian) | — | Two-phase disclosure | MCP surface | not quantified |
| token-optimizer-mcp | — | Cache + compression | Mixed | 95%+ (unverified) |
| token-saver | — | Per-filetype compression | Observation | not quantified |
| **tokenix** | — | Index + 528 filters + hooks | Retrieval **and** observation | 67.4% over 7,807 real hook calls |

Of the 14, only grepai publishes the gap between tokens cut and money saved —
and that gap is **3.5×**.

---

## 2. Findings

### F1 · Positioning — the observation layer is the largest and it is empty

CoACT ([arXiv 2607.02911](https://arxiv.org/html/2607.02911)) measured where an
agent's tokens actually go: observations are **45.7%** of total consumption on
SWE-bench Verified and **67.8%** on Terminal-Bench. None of the ten most-starred
tools in the landscape table operate on that layer.

The whole market optimizes what the agent *reads from the repo*. Almost nobody
optimizes what the agent *gets back from running a command*. tokenix has 528
deterministic filters with 1,146 golden cases operating exactly there.

**→** Stop positioning tokenix as "local semantic search". Position it as the
observation compressor for coding agents, index included.

### F2 · Risk — the 67.4% figure is cache-blind and overstates dollar savings

- grepai measured **97%** reduction in fresh `input_tokens` and only **27.5%**
  cost saving, because `cache_read_input_tokens` dominated both arms.
- The two-tier cost model
  ([arXiv 2607.15516](https://arxiv.org/html/2607.15516v1)) formalizes it: with
  cache hit rate ρ, compression at ratio r only pays if
  `ρ < ρ_cross(r) = (α − 1/r)/(α − β)` where `α = c_write/p_in`, `β = c_read/p_in`.
  On Anthropic May-2026 pricing: r=2 → 0.652, r=3 → 0.797, r=6 → 0.942, r≥10 →
  always worth it. On τ-bench retail, query-aware compression cost **+40.1% more**
  than the vanilla baseline.

`tokenix gain` counts removed tokens and prices them at the full input rate. In a
real session a large share would have been billed at `cache_read` — an order of
magnitude cheaper. The report is correct in tokens and optimistic in dollars.

Structural good news: tokenix compresses at **write time** — the observation is
reduced before entering history — and never rewrites an already-cached prefix.
That is the pattern the paper flags as safe. The problem is accounting, not
architecture, and the safety property is worth stating publicly.

**→** Cache-aware gain: report "tokens removed" and "≈USD" separately, using the
effective blended rate. `usage.rs` already parses `cache_read`/`cache_write`.

### F3 · Gap — byte-exact golden tests prove determinism, not usefulness

CoACT defines **next-action preservation (NAP)** as the acceptance criterion: a
compression is valid if the agent picks the same next action from the compressed
observation as from the raw one. With that gate: 36% token saving and pass@1
*rising* 57.0 → 60.5. Without it, LLMLingua-2 saves tokens but drops pass@1 to
50% and increases step count by **111%** — the agent spends the savings
recovering.

The 1,146 golden cases guarantee a filter always produces the same output, and
the `never_mask_failure` rules guarantee an error never becomes a success. Nothing
measures whether the agent still *acts the same*. That is the difference between
"the filter is stable" and "the filter is safe".

Infrastructure already exists: `recordings` stores real output and
`filter generate` already invokes a detected AI CLI.

**→** Per-filter action-preservation homologation. Publish "% saved / % action
preserving" — a claim no competitor makes.

### F4 · Gap — no trajectory layer

In long-horizon tool tasks ([arXiv 2606.10209](https://arxiv.org/abs/2606.10209)):
full history = 71.0% completion at 1.48M tokens; prune to last 5 interactions =
79.0% at 535k; prune **and** summarize = **91.6% at 553k** — 37% of the tokens and
60% less wall time. Trajectory compression stacked on observation compression took
a run from $45.65 to $25.88.

The tokenix hook sees one call at a time. What accumulates over a session is
partially covered by re-read suppression and `stash`/`retrieve`.

The safe extension is **delta reads**: if a file or command was already delivered
this session and changed, return only the diff plus a pointer. Action-preserving
by construction (changed content appears in full) and avoids fuzzy dedup, which
was already correctly refused — hiding altered output is the failure mode the
literature punishes hardest. Note `ROADMAP.md` already defers a "diff-aware Read
intercept" — this is the same item with evidence behind it.

**→** Per-session observation ledger + delta mode. Exact hashes only, never
approximate similarity.

### F5 · Opportunity — the MCP surface is the cheapest saving available

- Anthropic: on-demand tool search took an MCP library from **77,000 → 8,700**
  tokens (85%); the code-execution pattern took a benchmark from **150,000 →
  2,000** (98.7%).
- TSCG ([arXiv 2605.04107](https://arxiv.org/pdf/2605.04107)): deterministic,
  lossless schema compilation cuts **40–60%** on its own.
- GitHub reported up to 62% reduction from pruning unused MCP alone.

tokenix has `mcp --profile slim`, `prompt-audit`, and an `mcp-proxy` that
compresses `tools/call` results. The missing middle is compressing the *schemas*
of the third-party servers the proxy fronts, with two-phase disclosure (minimal
surface → full schema on demand → invoke).

This is the only lane with a direct named competitor — Atlassian Labs'
`mcp-compressor`. But it only compresses schemas. A local Rust proxy that
compresses schema *and* result does not exist.

**→** Deterministic schema compilation + two-phase disclosure in `mcp-proxy`.

> Caveat pending verification: system prompt and tool schemas normally sit in the
> cached prefix, so a 60% schema cut may be worth far less in dollars than in
> tokens. Same correction as F2. Being checked in pass 2.

### F6 · Opportunity — nobody in this market publishes a public benchmark number

All 14 tools' claimed savings come from their own harness. Mature public targets
exist: CORE-Bench ([arXiv 2606.11864](https://arxiv.org/abs/2606.11864) — 180k
queries, 106k broader-context relevance labels, three levels), Terminal-Bench 2.1,
SWE-bench Verified.

`tokenix benchmark` measures against a vanilla baseline and deliberately refuses
competitor arms — an honest stance that avoids unfair comparison, but leaves the
project without external proof. Terminal-Bench is the natural target: it is where
observations are 67.8% of spend, i.e. tokenix's best case, scored by a third party.

**→** One public run reporting tokens **and** resolve rate. One auditable number
beats ten self-measured ones.

### F7 · Constraint — do not trade deterministic rules for a learned compressor

- LLMLingua-2 on agent observations: pass@1 57.0 → 50.0, steps +111%.
- Learned implicit compression (ICAE,
  [arXiv 2605.11051](https://arxiv.org/html/2605.11051)) on SWE-bench Verified:
  **19 → 7** issues resolved (p=0.013), with hallucinated paths and URLs during
  reconstruction — errors that compound every step. Works on single-shot tasks,
  breaks in agentic flow.

It is tempting to reuse the ONNX runtime already in `embed.rs` to run a token
classifier. The evidence says that without an action-preservation gate it destroys
more value than it saves. tokenix's deterministic rules are not a technical
limitation to overcome — they are why it does not have this failure mode.

**→** Any learned component goes only in the uncovered long tail (which
`discover` already ranks) and only behind the F3 gate.

---

## 3. Roadmap

Tiers are sequential — each depends on the previous one measuring correctly.

### Tier 1 — fix what is already claimed (weeks, low risk)

1. **Cache-aware gain accounting** (F2) — separate "tokens removed" from
   "≈USD saved", priced with the real full-input / `cache_read` / `cache_write`
   mix read from transcripts.
2. **Action-preservation homologation** (F3) — per filter, compare the next action
   proposed from raw vs filtered recorded output. A filter that changes the
   agent's action fails even at 95% savings.
3. **Schema compression in `mcp-proxy`** (F5) — deterministic lossless compilation
   of fronted servers' schemas plus two-phase disclosure. Process `tools/list`,
   not only `tools/call`.

### Tier 2 — open the trajectory layer and prove it externally (months, medium risk)

4. **Session ledger + delta reads** (F4) — exact hash only; changed content always
   shown in full.
5. **Public benchmark run** (F6) — Terminal-Bench 2.1 primary, published
   methodology, tokens and resolve rate side by side. Cheaper alternative:
   CORE-Bench for the retrieval arm only.
6. **Long-tail coverage loop** (F3, F7) — `discover` ranks uncovered waste, Studio
   records and generates, the Tier-1 gate accepts or rejects.

### Tier 3 — bets

7. **Compaction bridge** (F4) — expose what tokenix knows about the session to the
   agent's own compaction mechanism instead of letting it summarize blind.
8. **Signed filter registry** (F1) — the SHA-256 trust gate already exists locally;
   a shared registry turns 528 filters into a network effect, which a commodity
   code graph cannot have.

---

## 4. What the research advises against

- **Building a code graph to compete with CodeGraph / GitNexus.** Two projects
  above 40k stars, both local, both permissively licensed. Commodity, and
  `graph.rs` with PageRank already covers the internal use that matters.
- **Adopting TOON as a headline.** ~40% fewer tokens than JSON on tabular data,
  but the 2026 benchmark shows JSON with the best accuracy plus an instruction
  "prompt tax" that eats the gain in short contexts. At most useful for internal
  tabular output.
- **Similarity-based dedup.** Already refused, and the literature confirms: hiding
  altered output is the most expensive failure mode, because the agent acts on a
  reality that no longer exists.
- **Publishing a savings percentage without the cache math.** After F2, every
  savings number should carry its compression ratio and assumed cache hit rate.

---

## 5. Sources

- [CoACT: Action-Preserving Observation Compression for Coding Agents](https://arxiv.org/html/2607.02911) — arXiv 2607.02911
- [On Problems of Implicit Context Compression for Software Engineering Agents](https://arxiv.org/html/2605.11051) — arXiv 2605.11051
- [Cache-Aware Prompt Compression: A Two-Tier Cost Model for LLM API Caching](https://arxiv.org/html/2607.15516v1) — arXiv 2607.15516
- [Less Context, Better Agents: Efficient Context Engineering for Long-Horizon Tool-Using LLM Agents](https://arxiv.org/abs/2606.10209) — arXiv 2606.10209
- [CORE-Bench: A Comprehensive Benchmark for Code Retrieval in the Era of Agentic Coding](https://arxiv.org/abs/2606.11864) — arXiv 2606.11864
- [TSCG: Deterministic Tool-Schema Compilation for Agentic LLM Deployments](https://arxiv.org/pdf/2605.04107) — arXiv 2605.04107
- [Token-Oriented Object Notation vs JSON](https://arxiv.org/abs/2603.03306) — arXiv 2603.03306
- [Prompt Compression for Large Language Models: A Survey](https://arxiv.org/html/2410.12388v2) — arXiv 2410.12388
- [Code Isn't Memory: A Structural Codebase Index Inside a Coding Agent](https://arxiv.org/abs/2606.22417) — arXiv 2606.22417
- [Code Intelligence Tools for AI Agents Compared](https://rywalker.com/research/code-intelligence-tools) — Ry Walker Research
- [Codebase Memory: The 6 Best Tools for AI Coding Agents (2026)](https://www.sentra.app/articles/best-codebase-context-memory-tools) — Sentra
- [Benchmark: grepai vs grep on Claude Code](https://yoanbernabeu.github.io/grepai/blog/benchmark-grepai-vs-grep-claude-code/)
- [mcp-compressor](https://github.com/atlassian-labs/mcp-compressor/) — Atlassian Labs
- [Production Results: MCP Server for GitHub Validates Anthropic's Code-First Pattern](https://github.com/orgs/modelcontextprotocol/discussions/629)
- [GitHub Slashes Agent Workflow Token Spend up to 62%](https://www.infoq.com/news/2026/05/github-agentic-token-savings/) — InfoQ

---

## Deep sweep (pass 2)

Six parallel research lanes: client-side caching and the proxy question,
observation/trajectory compression, retrieval benchmarks, MCP/tool-surface
compression, exhaustive GitHub sweep, and the wider frontier. **Pass 2 overturns
most of pass 1.** Corrections come first.

### Corrections to pass 1

| Pass-1 claim | Verdict | Why |
|---|---|---|
| **F1** — the observation layer is empty | **FALSE** | rtk 75.2k★, headroom 65.4k★, lean-ctx 3.5k★, snip 397★ (same architecture, same `gain`/`discover` command names). tokenix is at 13★. The layer is one of the most crowded in agent tooling. |
| **F2** — `gain` overstates dollars | **TRUE, and worse than stated** | Not an accounting nuance. Token reduction has **r = 0.15** correlation with billed cost change (n=2,848 paired runs). See below. |
| **F5** — compress MCP schemas in the proxy | **DEAD** | Not implementable at the proxy layer, Claude Code already defers by default (120 tokens), and a facade would destroy per-tool permissions. |
| **F6** — Terminal-Bench as the benchmark target | **WRONG TARGET** | Expensive, high variance at n=89, signal diluted. CORE-Bench L2 is IR-only, $0 API. |
| **F3, F4, F7** | **Confirmed and strengthened** | The literature named F3's metric (NAP) in July 2026 with a concrete protocol. |

**Also invalidated: a stored project memory.** `project-claude-no-posttooluse-by-design`
claimed Claude Code PostToolUse cannot replace tool results. It can:
`hookSpecificOutput.updatedToolOutput` **replaces the tool's result**, for all
tools ([docs](https://code.claude.com/docs/en/hooks)). Verified directly. Memory
corrected 2026-08-08.

### The decisive finding

**Token Reduction Is Not Cost Reduction** ([arXiv 2607.12161](https://arxiv.org/html/2607.12161))
— 2,908 provider-billed Claude Code runs, 103 tasks, 7 repos, 3 models,
pre-registered paired design. It evaluates **hook-based compressors on Claude
Code**, i.e. tokenix's exact deployment shape, and names RTK, RTK-ML, Headroom,
ml_lexical and Caveman.

- An arm removing **38.4%** of raw tool-output tokens cost **+6.8% more**
  (95% CI [+2.8, +11.3]).
- Per-task correlation, reduction vs cost change: **Pearson r = 0.154**
  (CI crosses zero), **Spearman ρ = 0.013**.
- Bill composition: cache creation **44.3%**, cache reads **35.4%**, output
  **10.4%**, uncached input **1.3%**. ~80% of the bill is cache traffic; the
  share addressable by visible-token compression is **~5% of input cost**.
- Four break mechanisms: cache pricing multipliers, **closed-loop recovery**
  (the agent re-searches and re-transmits the prefix), **trajectory
  elongation**, and **downstream consumer corruption**.
- Proposed metric: **success-adjusted billed cost** (L7 on an L1–L8 evidence
  ladder). tokenix currently reports L1 — the rung with r = 0.15.

**Independently corroborated.** JetBrains ran rtk on SkillsBench: 425 billed
trials, 86 tasks, Claude Code 2.1.201 headless, Sonnet 5, Harbor sandboxes,
Wilcoxon signed-rank, pre-registered endpoints
([blog](https://blog.jetbrains.com/ai/2026/07/rtk-claude-code-token-savings/)).
Low effort: **+7.6% cost** (p=0.004), **+13.8% turns** (p=0.03), **+14.3% cache
reads** (p=0.008). High effort: +0.1% (p=0.99). Quality indistinguishable. Their
sentence: *"a tool's self-reported savings are a claim about its counterfactual,
not about your bill"* — rtk's own analytics showed **96 million tokens saved**
while cost rose.

tokenix's 67.4% is the same class of claim.

### Confirmed bug: `count_tokens` counts bytes

`src/chunker.rs:92` — comment says "~4 chars per token", code is
`text.len().div_ceil(4)`, and `len()` is **bytes**. Non-ASCII content (accented
logs, box-drawing, emoji, CJK) inflates by up to 3–4×. Three stacked errors:
bytes vs chars; 4 vs Anthropic's own ~3.5 guidance; and blind to the Opus 4.7+
tokenizer, which emits **1.32× more tokens than the old Claude tokenizer and
1.73× more than GPT-5.x on TypeScript** at unchanged prices.

Fix: [`bpe`](https://crates.io/crates/bpe) (github/rust-gems, MIT) — ~3.5× faster
than tiktoken, **linear** where tiktoken is quadratic (tokenix's inputs *are* the
adversarial cases), `AppendableEncoder` for streaming counts,
`IntervalEncoding` for exact-budget truncation.

### Two live hazards in the current design

1. **Edit-anchor corruption.** 73% token reduction dropped successful patch
   application **27/40 → 15/40** on SWE-bench-derived Go tasks, by rewriting the
   verbatim text an edit tool must match exactly (2607.12161). Corroborated:
   dedent alone costs **−8pp** absolute ([2606.01326](https://arxiv.org/abs/2606.01326)).
2. **Shell-pipeline corruption.** Filtered output entering a pipeline that
   expects raw text produces silently wrong answers. `tokenix run` has this
   exposure and no stdout-consumer guard.

### F3 upgraded: the metric now has a published protocol

CoACT ([arXiv 2607.02911](https://arxiv.org/abs/2607.02911)) formalizes
**Next-Action Preservation** and — critically — **uses no LLM judge**:

```
refs  = [sample(policy, prefix + raw_obs,      T=0.6) for _ in range(8)]
cand  =  sample(policy, prefix + filtered_obs, T=0.0)
score = mean(top_3([action_sim(cand, r) for r in refs]))
PASS if score >= 0.60
```

`action_sim` parses the action into `(op_type, fields)`, returns 0.0 on op_type
mismatch, else normalized field agreement — **deterministic, writable in Rust**.
Validated at **80.0% agreement** with human same/different labels on 100 pairs.
Reference actions cache per `(prefix, raw_obs)`, so a filter change costs one
greedy call per affected test: the suite is incremental, not 10k calls per PR.

Five formulations exist (CoACT NAP, AGORA counterfactual action-change, LCoW
action-matching, ACON contrastive trajectories, Self-GC no-impact rate). **None
uses an LLM as the equivalence judge** — LLM-judge step attribution measures
~14% accuracy. Use a model to *produce* the action; use code to *compare* them.

Report two axes with Wilson CIs — **(% tokens removed, % action preserved)**.
Self-GC's table is the proof: baselines pruned 1.5× more at 20pp lower no-impact.

### Determinism is the correct architecture, and it is now defensible

| Claim | Evidence |
|---|---|
| Deterministic masking matches LLM summarization at half the cost | Complexity Trap ([2508.21433](https://arxiv.org/abs/2508.21433), JetBrains): mask obs >10 turns → Qwen3-Coder-480B **54.8% / $0.61** vs summary 53.8% / $0.64 vs raw 53.4% / $1.29 |
| Deterministic beats generative in the aggressive regime | Control Under Compression ([2608.01056](https://arxiv.org/html/2608.01056), 15,525 runs): at 35% retained, section-based **47.0%** vs LLM rewriting **19.9%** |
| The deterministic floor contributes more than the learned scorer | AGORA ([2605.26596](https://arxiv.org/abs/2605.26596)): removing the floor costs −0.088 reward; removing the 125M scorer costs −0.031 |
| Rules have zero variance; summarizers do not | Parallel Context Compaction ([2605.23296](https://arxiv.org/abs/2605.23296)): compaction output **CV 19.8–84.5%**, and prompt instructions are *ignored* |
| Extractive output cannot poison context | ICAE-for-SWE ([2605.11051](https://arxiv.org/html/2605.11051)): hallucinated URLs/paths compounding across steps; **19 → 7** issues resolved (p=0.013) |

Perplexity-ranked token pruning is unusable on tool output — five independent
measurements (CoACT, AGORA, Control Under Compression, Cross-Lingual Arbitrage,
AgentDiet). LLMLingua-2 on agent observations: pass@1 57.0 → 50.0, tokens
**+172%**, steps **+111%**.

What determinism **cannot** do, per the same literature: task-conditioned
relevance in heterogeneous output (BM25 hits 0.22 recall where a 2B model hits
0.86); telling which of N similar items matters; generalizing across
environments without adaptation — TACO ([2604.19572](https://arxiv.org/abs/2604.19572))
found **200 hand-curated rules plateaued** while self-evolving rules kept
improving. That is a direct warning about a hand-maintained 528-filter set.

### The always-on context nobody measures

Measured on this machine:

| Source | Cost | State |
|---|---|---|
| `AGENTS.md` (tokenix repo) | ≈ **13,500 tok** | always loaded |
| 101 installed skills (frontmatter) | ≈ **13,000 tok** | always listed |
| `~/.claude/CLAUDE.md` | ≈ 790 tok | always loaded |
| MCP tools (deferred by default) | **120 tok** | — |

Anthropic's own reference for a project CLAUDE.md is **1,800 tokens**, with
explicit guidance of "under 200 lines". tokenix's `AGENTS.md` is **7.5× that**
and **112× the MCP line** that F5 wanted to optimize. `prompt-audit`'s
`context_weight()` already measures this.

### Where tokenix is genuinely unique (survived the sweep)

1. **528 TOML filters with 1,146 golden cases** — the largest per-command output
   filter corpus in existence (snip 132, rtk ~100 compiled, token-saver 36,
   squeez 19). Only token-saver has comparable test discipline, at 1/15 the size.
   No other project publishes filter-level correctness testing at all.
2. **In-process ONNX embeddings**, no daemon, no API, no Ollama. claude-context,
   octocode and codeseek all default to remote APIs.
3. **Retrospective transcript forensics** (secrets + egress). The entire egress
   field is inline proxies you had to install beforehand; nobody analyzes
   recorded sessions after the fact.

Not unique despite framing: `gain` and `discover` (rtk coined both, snip copied
the names), multi-agent interception (5 vs field median ~14), Rust, "60–90%",
stash/retrieve, cross-call dedup.

### Emphasis inversion

SWE-Pruner ([2601.16746](https://arxiv.org/abs/2601.16746)) measures **reads =
76.1% of coding-agent context tokens**. Filters on `cargo test` address the
minority. tokenix's semantic index — not the headline today — is the more
valuable asset. Corroboration: graph beats embeddings for localization by large
margins (LocAgent 75.91 Acc@1 vs best embedding 52.55 vs BM25 38.69), and 2026
consensus is **lexical as anchor, graph second, embeddings optional**
([LARGER 2605.16352](https://arxiv.org/abs/2605.16352): +13.9 pp Acc@5).

Also: `nomic-embed-text-v1.5` is a *general text* model. On CORE-Bench L2 even
Qwen3-Embedding-8B collapses to **20.3 NDCG@10**; CodeRankEmbed (137M, fits
local-first) leads small models at 52.55 Acc@1 on LocBench. `embed.rs` already
supports custom ONNX models — this is a registry change, not architecture.

### On caching: no proxy

- Claude Code and Codex CLI already place `cache_control` correctly; Claude Code
  closed breakpoint configuration as *not planned*
  ([#58103](https://github.com/anthropics/claude-code/issues/58103)). The broken
  harnesses are Cline, Roo, Continue, Aider.
- A proxy silently breaks subscription auth
  ([#33330](https://github.com/anthropics/claude-code/issues/33330),
  [#23022](https://github.com/anthropics/claude-code/issues/23022)), gateways are
  documented stripping `cache_control`
  ([Portkey #1579](https://github.com/Portkey-AI/gateway/issues/1579)), beta
  headers drift weekly, and LiteLLM shipped credential-stealing malware on PyPI.
  It would put every credential the agent touches inside tokenix.
- `prompt-cache-skills`, the one project built to fix agent caching,
  deliberately avoids a proxy and patches harnesses instead.
- **The non-proxy route**: Claude Code emits `claude_code.token.usage` over
  OpenTelemetry with `type = input | output | cache_read | cache_creation`, per
  model and session. A local OTLP receiver gives live cache hit rate and billed
  cost with zero interception.

Architectural point worth stating publicly: tokenix compresses at **write time**,
before the observation enters history, and never rewrites a cached prefix. That
is the safe side of the line that inverted AgentDiet's cost (−40.3% tokens,
+10.6% cost from rewriting history). But it must be **measured**, not asserted —
2607.12161 measured hook-based compressors and still found +6.8%.

---

## Decision — what to implement

Ordered. Each tier is a precondition for the next.

### Tier 0 — make the numbers true (nothing else is meaningful first)

1. **Real tokenizer.** Fix bytes→chars, then move to the `bpe` crate with a
   per-model calibration table (old-Claude / new-Claude / GPT). Every budget
   decision, gauge and `gain` figure depends on it.
2. **Billed-cost accounting in `gain`.** Demote "tokens removed" from headline to
   diagnostic. Report success-adjusted billed cost from `cache_read` /
   `cache_write` / `output` / uncached input, which `usage.rs` already parses.
   Report **Δturns alongside Δtokens** — +13.8% turns is the documented
   mechanism by which savings invert.
3. **Holdout mode.** `TOKENIX_HOLDOUT=0.1` to leave a random slice uncompressed
   as a control group, with paired CIs. headroom ships this; it is now the bar,
   and without it no causal claim is possible.
4. **OTLP receiver in the daemon** — live cache hit rate and cost split, no
   proxy. Enables (2) with provider-billed ground truth and doubles as a
   cache-breaking-action warning (instruction-file edit mid-session, MCP add or
   remove, model switch).

### Tier 1 — close the two hazards, then ship the claim nobody else can

5. **Edit-anchor guard.** Never filter content destined for exact-match editing.
6. **Shell-consumer guard** for `tokenix run` — detect pipeline consumption,
   pass through raw.
7. **NAP harness** (`filter test --action-preservation`). Two tiers: free
   deterministic CI checks (entity retention — paths, identifiers, hashes, line
   numbers, exit codes; verbatim-anchor preservation; negative-evidence tokens),
   then the CoACT protocol nightly against a local 3–8B model. Plus a ~50-case
   red-team set where a single token is load-bearing. Publish
   **(% saved, % action-preserving)** with Wilson CIs.

### Tier 2 — newly unlocked surface, and the reframe

8. **PostToolUse `updatedToolOutput`** for Claude Code — size-adaptive
   compression (the same `git status` varies 5 → 5,491 tokens, **1,098×**) and
   secret redaction *before* the model sees the output, not after in the
   transcript.
9. **`prompt-audit --recommend`** on instruction files and skills. The largest
   always-on cost measured, nobody else measures it, and tokenix's own
   `AGENTS.md` is the worst offender on this machine.
10. **Swap the embedding model** to a code-specialized one and measure. Cheap,
    and the current default is the weakest link in the retrieval pipeline.

### Tier 3 — external proof

11. **CORE-Bench L2** — IR-only, ~$0 API, 32 GB disk, published weak baselines,
    harness exists. Report NDCG@10 and Recall@100 plus **tokens-to-recall@10**
    and query latency, which nobody reports. Keep the retrieval claim and the
    economics claim in separate rooms.

### Dropped

- **MCP schema compression** — not implementable at the proxy layer; the
  `AGENTS.md` contract line stays as written.
- **Learned/perplexity compressor** — five independent measurements say it is
  harmful on tool output.
- **Code knowledge graph as a product** — commodity, two projects above 40k
  stars.
- **TOON as a headline** — accuracy loss plus prompt tax; at most for internal
  uniform tabular results.
- **Similarity-based dedup** — hiding altered output is the most expensive
  failure mode.
- **Any LLM proxy** — catastrophic tail risk for a benefit the evidence says is
  small on the harnesses tokenix targets.

### The positioning change

Stop leading with "60–90% token savings". That claim is now owned and publicly
discredited by a competitor 5,800× larger. Lead instead with the two things that
survived the sweep: **measurement nobody else does honestly** (billed cost,
holdout control, action preservation) and **forensics nobody else does at all**
(retrospective transcript secrets and egress). Being the first tool in this
category to survive a neutral harness is available, and currently unclaimed.

---

## Deep sweep (pass 3) — log induction, edit anchors, secrets, NAP corpus

### The benchmark decision is settled: LogDx-CI

[arXiv 2605.28876](https://arxiv.org/abs/2605.28876) — 35 real GitHub Actions
failures, logs 27 to 200,000+ lines, 8 failure categories, 7 ecosystems. Code
Apache-2.0, data CC-BY-4.0, API `logdx_ci.evaluate(reducer=...)`, and the default
`static-signal-recall` scorer is **deterministic and free — no API key, no LLM**.

| Method | Score | Tokens/case |
|---|---:|---:|
| hybrid-grep + **rtk** + tail | 0.670 | 19,844 |
| grep | 0.639 | 88,355 |
| **tail-200** | **0.614** | **6,108** |
| **raw (no reduction)** | **0.353** | 275,248 |
| **rtk-log** (aggressive filter) | **0.249** | 810 |

`rtk-log` scores **below doing nothing**. A dumb `tail -200` beats raw by 74% at
2.2% of the tokens. This is "never mask a failure" measured by a third party, on
the 75k-star competitor.

**Structural invariant implied:** a filter must never emit less context than an
unconditional tail window. `never_worse` guards bytes; this guards evidence. Do
not change 528 filters before LogDx-CI is wired up as the baseline.

Second corpus: **LogChunks** ([Zenodo 3632351](https://zenodo.org/record/3632351))
— 797 Travis CI logs, 80 repos, 29 languages, with the failure-explaining chunk
manually annotated plus developer search keywords (free seed material for
`priority_lines`). Third axis, unclaimed by anyone:
**CiDiff** ([2504.18182](https://arxiv.org/abs/2504.18182)) — diff the failing run
against the last green run: **~60% median reduction** in lines to inspect,
preferred **70% vs 5%**, over 17,906 CI regressions.

Calibration: the honest ceiling is **LogSieve's 40–42%** at cosine fidelity 0.93,
and TACO's self-evolving rules deliver **1–4% accuracy**, not big compression.

Other confirmed invariants: **exit code is the only high-precision failure
oracle** (F1 0.99, SWE-Factory) vs text heuristics at 99.2% precision but **76.2%
of faults missed** (Chromium CI) — text may never override a non-zero exit; force
`LC_ALL=C` on children. **ANSI stripping is not a token win** (tools auto-disable
when piped); the volume is progress bars (500–2,000 tok/run) and `\r` rewriting,
best fixed upstream (`--no-progress` alone = ~45% on PHPUnit).

### CORVUS validates delta reads

[arXiv 2607.22711](https://arxiv.org/abs/2607.22711) — decouples file-read actions
from observations via a synchronized registry. **9–50% fewer input tokens, 15–32%
shorter prompts, up to 37% FEWER reasoning cycles** at comparable pass rates.

That third number is the point: every content compressor pays a
trajectory-elongation tax (LLMLingua-2 +111% steps, ICAE +40%, rtk +13.8% turns).
Removing *duplicates* does not, because it removes no information. Caveats: CORVUS
reports input tokens, not billed cost; and its mechanism mutates history, which a
hook cannot do — the implementable form is write-side.

### Two anchor bugs confirmed in the tree

1. **CRLF laundering.** `join("\n")` appears 36× in `compress.rs` and 19× in
   `filters.rs`, and `\r` appears in **neither file**. The pipeline splits with
   `.lines()` (which consumes `\r\n`) and rejoins with `"\n"`. On Windows — this
   repo's primary platform — any CRLF file passed through a filter comes back
   LF-only, so every line ending differs by one byte from disk. No repair ladder
   surveyed handles CRLF. This is exactly the 27/40 → 15/40 mechanism.
2. **Outline signature lines.** `generate_outline` emits signatures via
   `extract_full_signature` — re-derived, not sliced verbatim. A near-exact code
   line in a code context is the most quotable-looking, least quotable thing
   tokenix can emit.

**Guard principle:** tokenix may delete content, and may replace content with a
pointer to its exact bytes. It may never hand back a *modified copy* of content
the agent might quote. Anchor-bearing-ness is decidable deterministically at hook
time from tool identity + path + repo state.

Supporting evidence: Sweep measured **77% of `str_replace` failures were "the code
isn't in the file"**; aider **removed** similarity fuzz in 2023 because silent
wrong edits beat loud apply failures; Anthropic's `str_replace` spec has **zero**
fuzz tolerance.

### Secrets: a confirmed arithmetic bug, now fixed

Shannon entropy over the observed distribution is bounded by `log2(n)`, so a
`min_entropy` above `log2(min_match_len)` can never be satisfied. Two bundled
rules were dead for short passwords: `basic-auth-url` (`{5,63}` @ 3.5 needed 12
chars) and `db-connection-uri` (`{4,}` @ 2.8 needed 7). Both now sit at 2.8 with a
matching 7-char floor. Regression test added. Residual limit: at 8 chars the 2.8
floor still needs ~7 distinct characters, so `s3cr3t99` stays missed — closing
that needs length-normalized entropy, not a lower constant.

Context: gitleaks-class scanners measure **46% precision / 88% recall**
([ESEM 2023](https://arxiv.org/abs/2307.00714)), FP root causes named verbatim as
*"generic regular expressions and ineffective entropy calculation"*.

Next, in order: **structural/checksum validation** (AWS AKIA, Stripe, `gh*_`, JWT
`exp`) — zero network, large FP reduction; **placeholder as an explicit third
class** (−33% high-severity alerts at 93% recall,
[2605.31520](https://arxiv.org/abs/2605.31520)); **format-preserving redaction** —
SlotGuard ([2607.17147](https://arxiv.org/abs/2607.17147)) measured generic
`[REDACTED]` dropping downstream task success to **2.5%** while same-shape
synthetic substitution held near baseline at 14.4 µs/turn. tokenix writes
`[REDACTED]` today.

Premise validated: **73.5% of agent credential leaks come from debug logging**,
because *"agent frameworks feed stdout into the LLM context window"*
([2604.03070](https://arxiv.org/abs/2604.03070)). And tokenix can compute a feature
no code scanner can — was the value in a *tool result* (near-certainly real) vs an
*assistant message* (possibly hallucinated)?

Unmeasured and publishable: **how often do agent CLI session files contain
plaintext secrets?** For egress, the highest-value detector is **sensitive-file
read → outbound call within N turns** — the only thing that catches exfiltration
to a vendor's own API. Do **not** build beaconing detection (event density is
orders of magnitude too low) and do **not** verify credentials over the network.

### NAP: the goldens are the wrong corpus

Measured in-repo: the 1,146 golden blocks have a **median `input` of 150 chars
(~40 tokens)**, mean 192, max 1,301 — far too small for compression to be a
meaningful decision. The goldens stay the right *byte-exactness* regression suite;
they are the wrong *behavioral* corpus.

**Cheapest path is Phase 0: mine local transcripts.** 990 Claude/Codex session
files / 873 MB on this machine already contain prefix + call + verbatim output +
*the actual next action a frontier model took* — a stronger label than any
small-model rollout, at zero inference cost. Gate on observations ≥2,000 chars
where the filter actually changes the output. Backfill uncovered filters from
AlienKevin/SWE-ZERO-12M (Apache-2.0, bash-only, same scaffold as CoACT),
nebius/SWE-agent-trajectories (CC-BY-4.0, prefer *failed* trajectories),
SWE-bench/SWE-smith-trajectories (MIT).

CoACT's protocol verified exactly: K=8 references at T=1.0, greedy under
compressed, mean of top M=3 structural similarity, θ=0.6, **80.0% agreement with
human labels**. Add AGORA's **soft labels** (fraction of K=8 paired rollouts whose
canonicalized next action differs) — same generations, absorbs decoder noise a
binary label conflates with real loss. Always run a **raw-vs-raw replicate arm**:
raw self-agreement is not 1.0, and without that floor the number is
uninterpretable.

**The statistics decide the reporting format.** Exact one-sided Clopper-Pearson
with zero failures is `LCB = α^(1/n)`: n=20 → **0.861**, n=30 → 0.905, n=59 →
**0.9505**. So a filter with a perfect 20/20 record supports "≥0.861", not
"≥0.95". McNemar needs ≥5 discordant pairs for one-sided p<0.05; at n=20 you
expect ~1.6, so an n=20 filter cannot produce a significant result at *any* effect
size. FWER across 528 tests pushes the requirement to **n=181** (Bonferroni) or
**n=219** (Benjamini-Yekutieli) — 96k–116k labeled examples. That settles the
frame: **estimation with hierarchical partial pooling, not 528 hypothesis tests.**

Report three states, never binary: **PASS / FAIL / INSUFFICIENT**.
"412 INSUFFICIENT, 109 PASS, 7 FAIL" is defensible; "521/528 pass at 95%" from
n=20 is not.

### Corrections to earlier passes

- The "Squeez, 11,477 examples / 618 curated test set" figure cited in pass 2 has
  **no found provenance** on arXiv or GitHub. Do not cite it.
- TACO's headline is **1–4% accuracy gain**, not a large compression number, and
  the "200 hand-curated rules plateaued" figure could not be extracted from the
  paper — treat as unverified positioning.

---

## Deep sweep (pass 4) — implementation audit + August literature, 2026-08-22

Status of the Tier 0–3 decision list in the actual tree (working tree has the
WIP uncommitted; `cargo test --bin tokenix` = **411 passed / 0 failed / 10
ignored**, 18.3 s).

### Implementation vs decision list

| Decision item | Status | Evidence in tree |
|---|---|---|
| T0.1 bytes→chars tokenizer | **DONE** | `chunker.rs:92` counts chars; test `count_tokens_counts_chars_not_bytes` |
| T0.1 accurate tokenizer | **DONE, deviated** | `embed.rs:179` uses HF `tokenizers` (not `bpe`); query display only, hot path keeps chars/4 |
| T0.2 billed-cost in `gain` | **DONE, half** | `MODELS` pricing incl. cache_read/write (`gain.rs:10-101`), `usage_cost`, `economics()` blended cpt from real usage mix, `gain --economics` (`main.rs:2745`). **Δturns NOT reported** — the documented inversion mechanism is still unmeasured |
| T0.3 holdout mode | **MISSING** | no `TOKENIX_HOLDOUT` anywhere |
| T0.4 OTLP receiver | **MISSING** | no otlp usage in daemon |
| T1.5 edit-anchor (CRLF) | **DONE** | `restore_eol`/`dominant_eol` (`compress.rs:1318-1344`, `filters.rs:1971`) — pass-3 hazard #1 closed |
| T1.5 edit-anchor (signature) | **OPEN** | `extract_full_signature` (`chunker.rs:912`) still normalizes whitespace + truncates with `…` — re-derived, quotable-looking, not a verbatim slice |
| T1.6 shell-consumer guard | **MISSING** | no `IsTerminal` on the `tokenix run` path (only `tui.rs:301`) |
| T1.7 NAP harness | **MISSING** | no action-preservation in `cmd_filter.rs` |
| T2.8 PostToolUse `updatedToolOutput` | **MISSING** | no reference in `hook.rs` |
| T2.9 `prompt-audit --recommend` | **DONE** | `mcp_audit.rs:1201` `recommendations_for_agent`, wired `main.rs:1431` |
| T2.10 code embedding swap | **HALF** | `jina-code` registered (`embed.rs:95`); default remains `nomic-v1.5` |
| T3.11 CORE-Bench | **MISSING** | `benchmark.rs` is still the internal harness |
| Pass-3 secrets entropy fix | **DONE** | both bundled rules at 2.8 with 7-char floors |

### New literature since pass 3

1. **Don't Break the Cache** ([2601.06007](https://arxiv.org/abs/2601.06007),
   Lumer et al., PwC) — 500+ agent sessions, DeepResearchBench, OpenAI/Anthropic/
   Google. Caching cuts cost **41–80%**; naive full-context caching can
   *increase* latency; dynamic content must append strictly at the **end**;
   tool-schema drift mid-session breaks the cache. Third independent confirmation
   that the only safe place to compress is **write time** — and that `prompt-audit`
   (schema-stability) sits on the same axis the paper names.
2. **TokenPilot** ([2606.17016](https://arxiv.org/abs/2606.17016), June 2026) —
   cache continuity vs text sparsity, prefix stabilization at ingestion:
   **61%/87%** cost cuts in isolated/continuous mode. Same conclusion: mutating
   the cached prefix is the expensive failure mode. tokenix's "compress at write
   time, never rewrite history" is exactly their mechanism.
3. **SkillReducer** ([2603.29919](https://arxiv.org/abs/2603.29919), March 2026) —
   optimizing agent *skills* for token efficiency. The always-on context lane
   pass 2 measured (13k tokens of skills on this machine) now has a named
   research thread — `prompt-audit --recommend` is the local expression.
4. **CAPC** ([2607.15516](https://arxiv.org/abs/2607.15516)) — already cited;
   new detail worth recording: Sonnet 4.6 cache has a **two-tier threshold near
   3,500 tokens**, below which ρ plateaus at ~0.83 — compression that pushes the
   cached prefix into the hot tier loses more than it saves.

### Verdict

The architecture is on the correct side of every line the 2026 literature draws:
write-time compression (before the observation enters history), never rewriting a
cached prefix (2601.06007 / 2606.17016 / 2607.12161 all converge here),
deterministic rules over learned compressors, exact-hash dedup only, CRLF
byte-exactness. **No architectural correction is needed.**

The gap is measurement, exactly as pass 2 predicted: `gain` remains an L1 claim
(tokens removed), holdout and NAP are absent, Δturns is absent. Cheapest closes,
in order: **Δturns in `gain`** (hours), **verbatim signature slice** in
`extract_full_signature` (hours, closes the last open anchor hazard), **holdout
mode** (a day, the bar for any causal claim). PostToolUse, OTLP and CORE-Bench
stay ranked behind those.

**Closed same day (2026-08-22):** the two hour-scale items. `generate_outline`
now slices the declaration verbatim from the file's own bytes
(`extract_signature_verbatim` + `line_starts` in `chunker.rs`), preserving
indentation, line breaks and CRLF — the re-derived, truncating `…` form is gone.
`gain` now prints session shape (`gain::session_stats`): sessions inferred from
time gaps, mean/max calls per session, and within-session re-requests of the
same tool+subject — the measurable share of the "+13.8% turns" inversion
mechanism — as a diagnostic that stays descriptive until holdout lands.

---

## Deep sweep (pass 5) — homologation, 2026-08-24

Full re-verification of the pass-4 implementation table against the tree at
`67e2f30` (merged to `main` as v0.64.0), plus a targeted sweep for literature
published after pass 4's 2026-08-22 cutoff.

### Verification method

Not a re-read of the table — each "DONE" row was checked against the actual
source, by symbol name, not by trusting the prior pass's note:

| Claim | Check | Result |
|---|---|---|
| T0.1 chars-not-bytes tokenizer | `chunker.rs:92` body + `count_tokens_counts_chars_not_bytes` test | **CONFIRMED** |
| T1.5 CRLF anchor fix | `dominant_eol`/`restore_eol` (`compress.rs:1318-1348`), wired in `filters.rs:1971-1972` | **CONFIRMED** |
| T1.5 verbatim signature slice | `extract_signature_verbatim` + `line_starts` (`chunker.rs:911-935`), called at `chunker.rs:1268` | **CONFIRMED** |
| Pass-3 entropy fix (`basic-auth-url`, `db-connection-uri`) | both rules at `min_entropy = 2.8` with `{7,...}` floors, `default.toml:296-312` | **CONFIRMED** |
| T0.2 `gain` session shape | `session_stats`/`SessionStats`/`re_requests` (`gain.rs:216-260`) | **CONFIRMED** |

`cargo fmt --check`, `cargo clippy --all-targets` and `cargo test` all clean —
438 + 14 + 2 = **454 passed, 10 ignored (model-tests, offline by design), 0
failed**.

### Correction to the pass-4 table

The pass-4 audit table's T0.2 row reads "Δturns NOT reported" — but that row
describes the state *before* the same-day close documented at the end of pass
4, which shipped `gain::session_stats`'s `re_requests` field as exactly that
diagnostic. The row was stale the moment it was written next to its own
same-day fix. Corrected here: **T0.2 is DONE**, with the caveat pass 4 already
stated correctly elsewhere — it is a descriptive proxy, not a causal Δturns
claim, because no holdout control group exists yet (that gap is T0.3, still
open).

### T1.6 closed — as an explicit opt-out, not auto-detection

Implemented `tokenix run --raw` / `TOKENIX_RAW=1`
(`compress::run_command_raw`, `Stdio::inherit`, sharing `build_shell_command`
with the compressed path so the two invocations cannot drift).

Investigating the "detect pipeline consumption" framing pass 4 carried forward
from the decision list found it is **not safely implementable as automatic
detection**: the agent harness that is the normal caller of `tokenix run`
reads its stdout through a pipe (that is how the harness captures output at
all), which is the identical `isatty()` signal a human piping into
`jq`/`grep`/a file would produce. There is no deterministic signal in this
process's own environment that tells the two apart — attempting one risks
silently *disabling compression in the primary use case* to guard a secondary
one, which is a worse failure than the hazard it closes. This is the same
category of correction pass 2 made to F5 (labeled DEAD): the failure mode is
real, the originally-imagined mechanism is not viable, and the paper's
prescription ("pass through raw" when detected) survives as an **explicit,
documented flag** instead of a guess. Verified functionally: byte-exact
stdout/stderr passthrough, exit code preserved (`exit 3` → process exit 3),
both the flag and the env var. Regression test:
`run_command_raw_preserves_exit_code`.

### New literature since pass 4

**What Does Context Compression Cost an Agent?** ([arXiv 2608.16370](https://arxiv.org/abs/2608.16370),
Liu, 2026-08-17 — 5 days after pass 4's cutoff) introduces **reacquisition
cost**: additional retrieval tool calls an agent issues to recover state a
compressor dropped, measured with a controlled bounded-horizon protocol
comparing a *dropping* operator against a *fact-preserving* one. At 5×
compression, a dropping operator produced a retrieval surge across all six
model/regime comparisons tested even where task completion stayed
statistically flat — GPT-5.5: completion 80%→85% (p=1.0, i.e. *unchanged*)
while retrieval calls rose 21.0→63.9 (p=.002). Semantically irrelevant content
replacement alone raised retrieval **57%** with no significant completion
change.

This does not change tokenix's architecture — it sharpens the existing
"closed-loop recovery" mechanism (2607.12161) with a name and a clean natural
experiment, and its own stated guidance is the position this repo already
holds: fact-preserving/semantic-filtering compression is safe, unconditional
dropping is not. It corroborates, in the same family as LogDx-CI's
`never_worse`-of-evidence finding and CORVUS's trajectory-elongation result,
why `filters.rs`'s `never_mask_failure`/`priority_lines`/`passthrough_when_emptied`
rules exist as hard invariants rather than tunables: a filter that drops
failure evidence to hit a compression ratio is exactly the *dropping* operator
this paper measures the cost of, even when its own golden tests still pass.
No code change indicated — recorded as corroboration for the next filter
audit pass.

### Remaining gaps — reaffirmed, still deferred

Unchanged from pass 4, each still absent from the tree by symbol-level check:
**T0.3 holdout mode** (no `TOKENIX_HOLDOUT`), **T0.4 OTLP receiver** (no OTLP
in `daemon.rs`), **T1.7 NAP harness** (no action-preservation in
`cmd_filter.rs`), **T2.8 `updatedToolOutput`** (no reference in `hook.rs`),
**T3.11 CORE-Bench** (`benchmark.rs` is still the internal-only harness). Each
is day-to-week scale, not an hours-scale close like T1.6; ranked in pass 4 and
that ranking still holds — holdout mode is the next cheapest and the
precondition for any causal savings claim.

### Verdict

No architectural correction from this pass. One real gap closed (T1.6, in the
only form that is actually safe), one stale table note corrected (T0.2), and
one new paper filed as corroboration rather than a new requirement. The
"homologate against the papers" bar for this pass is: every DONE claim in the
tree re-verified by symbol, not by re-reading the prior pass's prose — that
distinction is what this pass adds.
