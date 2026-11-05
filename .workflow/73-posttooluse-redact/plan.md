# Plan — T2.8 PostToolUse `updatedToolOutput` (variant B)

Issue: [#73](https://github.com/juninmd/tokenix/issues/73) · Selected variant: **B — redaction-first, two-stage**

## Approach

Redaction becomes an unconditional first pass over any non-empty PostToolUse
text, independent of tool identity or dialect. Compression stays a second,
`is_shell`-gated pass exactly as today. For the Claude/Codex dialect, replace
the current hard `std::process::exit(0)` no-op with emitting
`hookSpecificOutput.updatedToolOutput` whenever the text actually changed —
default ON, no new flag, reusing the existing `TOKENIX_DISABLED` escape
hatch that already governs the rest of compression. Two independent
`HookEvent`s may log for one call (`reason: "secret-redacted"` /
`"size-compressed"`), both `action: "intercepted"`, `phase: "post"` — no new
taxonomy, no schema change.

## Rejected alternatives

- **Variant A (minimal patch, one undifferentiated signal)** — rejected in
  `prototype.md`: folding redaction into the compression call loses the
  standalone "secrets blocked" metric, which is the whole reason this pivot
  beat T0.3 holdout mode.
- **Variant C (version-gated, fail-degrade)** — rejected: solves a problem
  the Claude Code docs say is already closed (`updatedToolOutput` honored
  for all tools since v2.1.121+, verified directly against
  code.claude.com/docs/en/hooks during research). No confirmed need for a
  capability probe or a second alert channel.
- **Opt-out flag (`TOKENIX_POST_REDACT=0`)** — rejected by questionnaire
  answer: this is a bugfix (dead code path + a live secret-leak surface),
  not a new opinionated feature; `TOKENIX_DISABLED` already covers "give me
  raw, skip everything" per command.
- **Standalone regex scan duplicated in `compress.rs`** — rejected by
  questionnaire answer: would duplicate `secrets_scan`'s rule compilation,
  a second place to update every time a rule changes.
- **New `HookEvent.action` values** — rejected by questionnaire answer:
  `reason` is already the free-text detail field on top of `action`; adding
  new `action` values would force every `hook.log` consumer to learn them.
- **Format-preserving redaction (SlotGuard, arXiv 2607.17147)** — flagged in
  `research.md` as worth checking, explicitly deferred: this pass reuses the
  existing `secrets_scan::redact()` masking format as-is (non-goal per
  research.md); revisiting the mask *shape* is separate from wiring the
  *pipe*, and changing it now would conflate two decisions.

## Steps

### 1. Expose secret-scanning as a reusable text-redaction function

**Intent:** give `compress.rs` a single call that scans arbitrary text
against the already-compiled rule set and returns both the redacted text and
whether anything fired, without duplicating rule logic.
**Files:** `src/secrets_scan.rs`
**Change:** add
```rust
pub(crate) fn redact_known_secrets(text: &str) -> (String, bool) {
    let rules = load_rules();
    let matches = scan_content(text, &rules);
    if matches.is_empty() {
        return (text.to_string(), false);
    }
    let mut out = text.to_string();
    for m in &matches {
        out = redact_occurrences(&out, &m.secret); // reuse existing boundary-safe replacer
    }
    (out, true)
}
```
Adjust `load_rules`/`scan_content`/`redact_occurrences` visibility to
`pub(crate)` only if not already crate-visible (currently private `fn`;
same-module callers are fine, cross-module needs the bump).
**verify:** `cargo build --lib 2>&1 | grep -i "error\[E0"` — expect no
visibility errors; `cargo clippy -p tokenix --lib` clean.

### 2. Add unit tests for `redact_known_secrets`

**Intent:** lock in behavior before wiring it into the hot path — no match
returns `(text, false)` unchanged; a match returns `(_, true)` with the
secret masked, never fully exposed (reuse the existing
`redaction_never_exposes_full_secret`-style assertion).
**Files:** `src/secrets_scan.rs` (`#[cfg(test)] mod tests`)
**New tests:**
- `redact_known_secrets_no_match_returns_unchanged`
- `redact_known_secrets_masks_without_full_exposure`
- `redact_known_secrets_multiple_matches_in_one_block`
**verify:** `cargo test --bin tokenix secrets_scan:: -- --nocapture` — new
tests pass.

### 3. Restructure `run_hook_post()`: redaction-first, compression second

**Intent:** implement variant B's core change — redaction runs before the
`is_shell` gate that currently makes non-shell Claude-dialect tools exit
immediately (`compress.rs:2186-2189`); compression stays gated exactly as
today.
**Files:** `src/compress.rs` (`run_hook_post`, ~lines 2169-2239)
**Change (shape):**
```rust
let (redacted, secret_hit) = crate::secrets_scan::redact_known_secrets(&input.text);

let is_shell = POST_HOOK_TOOLS.contains(&input.tool_name.as_str());
// Removed: the `if !is_shell && ClaudeNoop { exit(0) }` early-out —
// redaction must still run and still be reportable for non-shell tools.

let compressed = if input.tool_name == "Bash" {
    compress_bash_output(&input.command, &redacted)
} else if is_shell {
    compress_output(&redacted)
} else if input.dialect == PostDialect::ClaudeNoop {
    redacted.clone()          // non-shell, Claude dialect: redaction is the whole job
} else {
    redact_base64_blobs(&redacted)  // non-shell, Copilot dialect: unchanged behavior
};

let changed = compressed != input.text;
if secret_hit || changed {
    match input.dialect {
        PostDialect::ClaudeNoop => {
            println!("{}", serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PostToolUse",
                    "updatedToolOutput": compressed
                }
            }));
        }
        PostDialect::CopilotJson => {
            // existing Copilot modifiedResult branch, unchanged
        }
    }
}
```
Remove the stale comment block at `2205-2212` asserting Claude PostToolUse
"cannot replace or shorten a tool result" — it is what this issue corrects.
**verify:** `cargo build --bin tokenix 2>&1 | tail -30` — clean build.

### 4. Log the two-signal `HookEvent`s

**Intent:** make savings and redactions independently visible to `gain`.
**Files:** `src/compress.rs` (same function, after the emit block)
**Change:**
```rust
let repo_root = find_repo_root();
if secret_hit {
    let _ = log_hook_event(&repo_root, &HookEvent {
        ts: now_ts(), tool: input.tool_name.clone(), action: "intercepted".into(),
        phase: "post".into(), reason: "secret-redacted".into(),
        saved_tokens: 0, actual_tokens: count_tokens(&compressed) as i64,
        original_estimate: count_tokens(&input.text) as i64,
        input_preview: clean.chars().take(200).collect(), command: input.command.clone(),
    });
}
if changed {
    let original_tokens = count_tokens(&input.text) as i64;
    let actual_tokens = count_tokens(&compressed) as i64;
    let _ = log_hook_event(&repo_root, &HookEvent {
        ts: now_ts(), tool: input.tool_name, action: "intercepted".into(),
        phase: "post".into(), reason: "size-compressed".into(),
        saved_tokens: (original_tokens - actual_tokens).max(0), actual_tokens,
        original_estimate: original_tokens,
        input_preview: clean.chars().take(200).collect(), command: input.command,
    });
}
```
Note: a call where `secret_hit && changed` both true logs two rows —
intentional per variant B, gives `gain` an accurate count of "calls where a
secret was blocked" independent of "calls where output shrank."
**verify:** `cargo clippy --all-targets 2>&1 | tail -30` clean.

### 5. Existing Copilot-dialect behavior must be provably unchanged

**Intent:** the redaction-first restructuring must not alter Copilot's
already-working `modifiedResult` path.
**Files:** `tests/` (or `src/compress.rs` test module, wherever existing
Copilot hook-post tests live — locate via `grep -rn "CopilotJson" src/
tests/`)
**verify:** `cargo test --bin tokenix compress:: -- --nocapture` — every
pre-existing hook-post test (Copilot dialect, base64 redaction) still
passes unmodified.

### 6. New integration-style test: secret in a non-Bash tool result

**Intent:** prove the end-to-end path variant B exists for — a secret
inside a tool result that never touches the Bash-command PreToolUse
rewrite (e.g. a synthetic `Read`-shaped `tool_response`) gets redacted and
the JSON is emitted in the exact shape Claude Code expects.
**Files:** `src/compress.rs` test module
**New test:** `run_hook_post_redacts_secret_in_non_shell_tool_result` —
feeds a JSON payload shaped like a Claude Code PostToolUse call for a
non-Bash tool containing a recognizable secret pattern (e.g. an AWS-style
key) through `run_hook_post()`-equivalent internals, asserts:
1. stdout contains `hookSpecificOutput.updatedToolOutput`
2. the secret string does not appear verbatim in that output
3. a `HookEvent` with `reason: "secret-redacted"` was logged
**verify:** `cargo test --bin tokenix run_hook_post_redacts_secret -- --nocapture`

### 7. Full suite + lint gate

**Intent:** confirm no regression across the 454-test baseline.
**Files:** none (verification only)
**verify:** `cargo fmt --check && cargo clippy --all-targets && cargo test --bin tokenix`
— expect fmt clean, clippy clean, all previous tests + the ~5 new ones
passing, 0 failed.

### 8. Docs

**Intent:** repo convention (`feedback_doc_sync` memory) — every
new/changed feature updates README.md and AGENTS.md in the same change.
**Files:** `README.md`, `AGENTS.md`
**Change:** note that `hook-post` now actively compresses and redacts
Claude Code PostToolUse output (previously a documented no-op, if
documented as such anywhere) — check both files for any existing
"PostToolUse is a no-op for Claude Code" language and correct it.
**verify:** `grep -rn "no-op\|cannot replace or shorten" README.md AGENTS.md`
— confirm no stale claim remains.

## Rollback

Single-PR, no data migration, no `.tokenix/` schema change (`HookEvent`
still has the same fields, just new string values in existing `String`
fields). Revert is a plain `git revert` of the PR commit — no follow-up
cleanup needed since nothing persists a new file format.

## Confirmation-required steps

None beyond the normal PR review gate — no destructive operation, no infra
change, no new dependency (reuses `serde_json`, already a dependency). Per
global CLAUDE.md: no push to `main`/release branch without explicit
confirmation, which the `phase-implement` stage's PR flow already handles.

## Backlog (out of scope this pass)

- Format-preserving redaction masking (SlotGuard-informed) — `research.md`
  flagged, deliberately deferred, separate decision from this plumbing.
- Claude Code version capability probe (rejected variant C) — revisit only
  if a real pre-2.1.121 compatibility report surfaces.
- T0.3 holdout mode (issue #72) — still open, unrelated to this change,
  picked up in a future pass.
