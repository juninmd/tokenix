# Research — T2.8 PostToolUse `updatedToolOutput`

Issue: [#73](https://github.com/juninmd/tokenix/issues/73)
Supersedes: [#72](https://github.com/juninmd/tokenix/issues/72) (T0.3 holdout
mode) — not closed, still valid backlog, but deprioritized because it is pure
measurement infrastructure and does not itself save a token or block a
secret. This item does both.

## Why the pivot

User rejected T0.3 explicitly: "não queria seguir por esse, quero que traga
mais benefícios." Re-scanned the 2026 literature (fresh web search, this
pass) plus the repo's own still-open gap list. New/relevant since the
project's last homologation pass (2026-08-24):

- **Context Compaction Theory** (arXiv 2608.01326, 2026-08-02) — formalizes
  compaction as two games (selection / generation), proves the generation
  game equivalent to one-way communication complexity. Theoretically
  interesting, not actionable this pass (no concrete mechanism, no benchmark).
- Nothing else found post-dates the existing doc's pass-5 sweep with a
  bigger concrete win than what the doc already ranked and left unshipped.

So "more benefit" comes from re-ranking the **existing, already-researched
backlog** by direct impact rather than finding a new paper — T2.8 was
already identified (pass 2 of the doc) as a stale belief in the tree, and
confirmed unshipped through pass 5. Verified directly against current docs
this pass: [code.claude.com/docs/en/hooks](https://code.claude.com/docs/en/hooks)
states `hookSpecificOutput.updatedToolOutput` is honored for **all tools**
as of Claude Code v2.1.121+, not just MCP tools — the exact correction the
repo's research doc already logged in pass 2 ("Also invalidated: a stored
project memory... It can").

## Goal

Turn `run_hook_post()`'s Claude/Codex dialect from a hard no-op into a real
second compression pass, by emitting
`{"hookSpecificOutput":{"hookEventName":"PostToolUse","updatedToolOutput":"<text>"}}`
on stdout when compression changes the output — and, in the same pass, add
secret redaction to that path so credentials are stripped from **any** tool
result this hook sees (not only the Bash commands the PreToolUse rewrite
already covers), before the model receives them at all.

## Non-goals

- No change to the Copilot dialect (`PostDialect::CopilotJson`) — already
  works.
- No new filters, no change to filter selection/behavior.
- No holdout mode, OTLP receiver, NAP harness, or CORE-Bench in this pass —
  #72 and the rest of the doc's Tier 0–3 backlog stay ranked as-is, just not
  picked up now.

## Acceptance criteria

1. For `PostDialect::ClaudeNoop`, when `compressed != input.text`, print
   `{"hookSpecificOutput":{"hookEventName":"PostToolUse","updatedToolOutput":"<compressed>"}}`
   to stdout and exit 0, instead of exiting 0 silently.
2. Secret redaction runs on the PostToolUse text path for every tool the
   hook fires on (today: `is_shell` gates most of the work to Bash/
   ListDirectory before even reaching redaction) — extend so redaction
   applies regardless of `is_shell`, reusing the rules `secrets_scan`
   already compiles.
3. `HookEvent.saved_tokens`/`actual_tokens` for this dialect become real
   numbers (the caveat in the current code comment — "do NOT log savings
   the model never actually receives" — no longer applies once output is
   genuinely replaced).
4. Fail-open on older Claude Code versions that ignore `updatedToolOutput`
   for non-MCP tools (pre-2.1.121): worst case is the JSON is ignored and
   the original tool output stands — never a blocking error, never
   malformed stdout that could itself corrupt the transcript.
5. fmt/clippy/test clean; existing hook-post tests (Copilot dialect,
   base64 redaction) unaffected; new tests cover the emitted JSON shape and
   the redaction-before-context path for a non-Bash tool result.

## Codebase map

- `src/compress.rs:2075-2091` `enum PostDialect` / `struct PostHookInput` —
  the dialect split this item removes the `ClaudeNoop` dead branch from.
- `src/compress.rs:2169-2239` `run_hook_post()` — the function to change.
  Current short-circuit at `2213`: `if input.dialect == PostDialect::ClaudeNoop
  { std::process::exit(0); }` immediately after computing `compressed`, with
  a comment (`2205-2212`) asserting the belief this issue corrects.
- `src/compress.rs:2186-2189` — `is_shell` gate that currently skips *all*
  work (including redaction) for non-shell tools under the Claude dialect;
  needs splitting so redaction still runs even when full compression
  doesn't apply.
- `src/compress.rs:2191-2199` — existing compression dispatch
  (`compress_bash_output` / `compress_output` / `redact_base64_blobs`) to
  extend with secret redaction, not replace.
- `src/secrets_scan.rs:247` `fn scan_content(content, rules) -> Vec<RawMatch>`
  and `src/secrets_scan.rs:145` `fn load_rules() -> Vec<Rule>` — both
  private; reusing them from `compress.rs` needs a visibility bump
  (`pub(crate)`), not a new detection implementation. `redact()` at
  `secrets_scan.rs:212` is the per-match masking primitive.
- `src/main.rs:3625` hook installer — registers `PostToolUse` →
  `tokenix hook-post`; no change needed, the wiring already exists.
- `src/doctor.rs` — candidate place to surface a Claude Code version check
  (warn if `updatedToolOutput` for non-MCP tools isn't supported yet) if the
  plan stage decides that belongs here vs. just failing open silently.

## Prior art / literature already synthesized in-repo

`docs/research/2026-08-token-economy.md`:
- Tier 2 item 8: "PostToolUse `updatedToolOutput` for Claude Code —
  size-adaptive compression (the same `git status` varies 5 → 5,491 tokens,
  1,098×) and secret redaction *before* the model sees the output, not
  after in the transcript."
- Pass 2 correction: "Also invalidated: a stored project memory... It can:
  `hookSpecificOutput.updatedToolOutput` replaces the tool's result, for
  all tools ([docs](https://code.claude.com/docs/en/hooks)). Verified
  directly." (This session re-verified the same doc page fresh.)
- Pass 3 secrets section: "73.5% of agent credential leaks come from debug
  logging... agent frameworks feed stdout into the LLM context window"
  (arXiv 2604.03070) — directly the mechanism this closes for the
  PostToolUse-only surface (tool results the PreToolUse Bash rewrite never
  sees).
- Pass 3 also flags SlotGuard (arXiv 2607.17147): generic `[REDACTED]`
  dropped downstream task success to 2.5% vs. format-preserving redaction
  holding near baseline — tokenix's existing `redact()` in `secrets_scan.rs`
  should be checked against this before assuming today's masking format is
  safe to reuse verbatim in this new call site.

## Constraints

- Fail-open discipline (repo-wide invariant, see `hook.rs`/`store.rs`):
  malformed output, unexpected JSON shape, or an older Claude Code that
  doesn't honor the field must never block or corrupt the tool result.
- `never_mask_failure` / evidence-preservation invariants (pass 5
  corroboration, arXiv 2608.16370 "reacquisition cost") apply here exactly
  as they do to the PreToolUse path — this hook must not become a second
  place a failure signal can get silently dropped.
- No SecOps regression: redaction must not itself leak partial secrets
  (existing `secrets_scan.rs` tests already assert
  `redaction_never_exposes_full_secret` — new call site must inherit that
  guarantee, not reimplement masking ad hoc).

## Risks / open questions (queued for phase-plan)

1. Version gating: is there a reliable signal (env var, `CLAUDE_CODE_VERSION`,
   or similar) to detect pre-2.1.121 vs. current, or is "fail open, emit
   the JSON always, older versions just ignore it" sufficient? Needs a
   docs/behavior check — does an old Claude Code choke on an unrecognized
   `updatedToolOutput` field, or silently ignore it? (Docs say "now honored
   for all tools" implying it was already a recognized-but-scoped field
   before, which suggests silent ignore is safe — verify before committing.)
2. Scope of "every tool this hook sees": does `POST_HOOK_TOOLS`/`is_shell`
   need to expand, or does redaction need to run unconditionally for any
   tool with non-empty `text`, independent of that allowlist? Affects how
   much of `run_hook_post`'s early-exit logic (`2183-2189`) needs
   restructuring vs. just adding a redaction call before it.
3. Redaction masking format: reuse `secrets_scan::redact()` verbatim, or
   does SlotGuard's format-preserving finding (pass 3) mean this call site
   needs a different masking strategy than the existing `[REDACTED]`-style
   one already used elsewhere in the tree? If the existing format is being
   kept regardless, say so explicitly rather than silently importing it.
4. Does `HookEvent.reason`/`phase` need a new value to distinguish
   "PostToolUse redaction-only" (no size compression, secret found) from
   "PostToolUse full compression" for `gain` reporting, or is a single
   `phase: "post"` + existing fields enough?
