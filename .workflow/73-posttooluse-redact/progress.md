# Progress — T2.8 PostToolUse `updatedToolOutput` (issue #73)

Branch: `feat/posttooluse-redact` (off `origin/main` @ `1b59531`)

## Step evidence

**1. `redact_known_secrets()` in `secrets_scan.rs`** — added `pub(crate) fn
redact_known_secrets(text) -> (String, bool)`, built on existing
`load_rules()`/`scan_content()`/`redact_occurrences()`.
`verify: cargo build --bin tokenix` → exit 0 (`Finished dev profile ... in
8m 20s`, one benign linker-message warning).

**2. Unit tests** — `redact_known_secrets_no_match_returns_unchanged`,
`redact_known_secrets_masks_without_full_exposure`,
`redact_known_secrets_multiple_matches_in_one_block`.
`verify: cargo test --bin tokenix secrets_scan::` → all pass.

**3. Restructured `run_hook_post()`** — redaction now runs first,
unconditionally, before the `is_shell` gate; removed the stale
`ClaudeNoop` immediate-exit and the comment asserting Claude Code
PostToolUse "cannot replace or shorten a tool result" (corrected per
`code.claude.com/docs/en/hooks`: `updatedToolOutput` honored for all tools
since v2.1.121+).
`verify: cargo build --bin tokenix` → exit 0.

**4. Two-signal `HookEvent` logging** — `reason: "secret-redacted"` /
`"size-compressed"`, both `action: "intercepted"`, `phase: "post"`, may
both fire for one call.
`verify: cargo clippy --all-targets` → clean, no warnings.

**5. Copilot dialect unchanged** — no existing unit tests referenced
`CopilotJson` directly (none existed before this pass); behavior verified
indirectly — the `modifiedResult` emission path is untouched code, only
its input (`compressed`) now derives from `redacted` instead of raw text.
`verify: cargo test --bin tokenix` → 441 passed, 0 failed, 10 ignored
(baseline was 438 pre-pass; +3 = the new unit tests, no regressions).

**6. New integration test** —
`claude_read_result_secret_is_redacted_via_updated_tool_output` in
`tests/hook_e2e.rs`: spawns the real `tokenix hook-post` binary with a
synthetic Claude Code `Read` PostToolUse payload containing an AWS-shaped
secret, asserts the emitted `hookSpecificOutput.updatedToolOutput` JSON
shape and that the secret does not survive.
`verify: cargo test --test hook_e2e` → 15 passed (was 14; +1), 0 failed.

**7. Full gate** —
`verify: cargo fmt --check && cargo clippy --all-targets && cargo test`
→ fmt clean, clippy clean, **458 total passed** (441 bin + 15 hook_e2e + 2
mcp_proxy_e2e), 0 failed, 10 ignored. Baseline was 454 (per pass-5
homologation: 438+14+2); +4 = the 3 new unit tests + 1 new integration
test, exactly as planned. No regressions.

**8. Docs** — `README.md`: `tokenix hook-post` row now states it redacts
secrets and compresses for Claude Code/Codex/Copilot CLI, not just a
"compatibility handler". `AGENTS.md`: `compress.rs` module-map row now
names `run_hook_post`'s redaction-then-`updatedToolOutput` behavior.
`verify: grep -rn "no-op\|cannot replace or shorten" README.md AGENTS.md`
→ no stale claim found (there wasn't one already, but the description was
generic enough to read as unchanged from before this fix — now accurate).

## Drift from plan

None. All 8 steps implemented as planned; no scope change.

## Iteration round 1 — full-project code review (`/code-review src high`)

User asked for a whole-codebase review, not just the PR diff. 3 findings,
all in the new code:

1. **CONFIRMED, fixed** — `secret_hit` always implies `changed` (redaction
   never leaves text byte-identical), so the two independent `if secret_hit`
   / `if changed` blocks both fired on any secret hit, double-logging the
   same call and double-counting it in `gain`'s `intercepted` count and
   token sums. Collapsed to one `log_hook_event` call, `reason` picked by
   priority (`secret-redacted` wins). Commit `1042c8f`.
2. **CONFIRMED, not fixed — accepted tradeoff** — `redact_known_secrets`
   now runs unconditionally on every PostToolUse call, reloading/
   recompiling ~60 rules each time (new cost; previously non-shell
   Claude-dialect calls exited before any of this ran). Measured: 5
   `hook-post` invocations ≈ 362ms wall (≈72ms/call), same order of
   magnitude as process-spawn overhead already inherent to this
   per-invocation CLI architecture — not a regression class worth a
   caching redesign in this pass, and caching wouldn't help anyway (each
   hook call is a fresh process; nothing persists across invocations to
   cache into). Left as a deliberate scope decision from variant B
   (redaction runs unconditionally, by design) rather than reopened.
3. **CONFIRMED, fixed** — `PostDialect::ClaudeNoop`'s doc comment still
   asserted PostToolUse "cannot replace or shorten a tool result" a few
   lines above the code that now does exactly that. Corrected the comment
   in place; kept the `ClaudeNoop` identifier (renaming touches every call
   site, out of scope for a comment fix).

`verify: cargo fmt --check && cargo clippy --all-targets && cargo test` →
458 passed (unchanged), 0 failed, fmt/clippy clean. Pushed as commit
`1042c8f` on the same PR (#74).

## Notes for the reviewer

- A first `cargo build --bin tokenix --test` run hit a `rustc` internal
  crash (`STATUS_STACK_BUFFER_OVERRUN`) — a local toolchain/compiler ICE
  unrelated to this change (reproduced once, not on retry; not our code).
  A clean retry compiled successfully; not raised as a project concern.
- The `secret_hit` and `changed` `HookEvent` rows are independent by
  design (variant B) but in practice always co-occur: any successful
  redaction changes `compressed` relative to `input.text`, so
  `secret_hit == true` implies `changed == true`. Both rows still log
  separately so `gain`/`doctor` can report "secrets blocked" as its own
  count without conflating it with size-only compressions.
