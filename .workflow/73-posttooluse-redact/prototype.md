# Prototype — T2.8 PostToolUse `updatedToolOutput`

Three ways to turn `run_hook_post()`'s Claude/Codex dialect from a no-op
into a real compression + redaction path. Code sketches are throwaway
pseudocode, not meant to compile, never merged.

---

## Variant A — minimal patch

Delete the `ClaudeNoop` short-circuit, emit the JSON when output changed.
Redaction folded into the existing compression call, no new gate.

```rust
// compress.rs, run_hook_post() — replace the exit-0 short-circuit
let redacted = redact_known_secrets(&input.text);        // new: scan_content + redact()
let compressed = if input.tool_name == "Bash" {
    compress_bash_output(&input.command, &redacted)
} else if is_shell {
    compress_output(&redacted)
} else {
    redact_base64_blobs(&redacted)
};

if compressed != input.text {
    println!("{}", serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "updatedToolOutput": compressed
        }
    }));
}
// log_hook_event(...) unconditionally — real savings now
```

**Tradeoff:** smallest diff (~15 LOC), one call site, matches the file's
existing shape exactly. Redaction and compression are logged as one
undifferentiated `saved_tokens` number — `gain` can't tell "we blocked a
secret" from "we shrank a log", which is exactly the kind of security
signal the research doc's pass-3 secrets work says is worth surfacing on
its own.

---

## Variant B — redaction-first, two-stage logging

Redaction is its own unconditional first pass, independent of `is_shell`
or tool identity — it runs on every non-empty PostToolUse text before
anything else. Compression stays a second, tool-gated pass. Two
`HookEvent`s can fire for one call: one tagged `reason: "secret-redacted"`
if a match was found, one tagged normally if compression also changed size.

```rust
// compress.rs
let (redacted, secret_hit) = redact_known_secrets(&input.text);  // returns whether it fired
let compressed = if input.tool_name == "Bash" { compress_bash_output(&input.command, &redacted) }
                  else if is_shell { compress_output(&redacted) }
                  else { redacted.clone() };  // non-shell: redaction is the whole job

if secret_hit || compressed != input.text {
    println!("{}", serde_json::json!({ "hookSpecificOutput": {
        "hookEventName": "PostToolUse", "updatedToolOutput": compressed } }));
}
if secret_hit {
    log_hook_event(&repo_root, &HookEvent { reason: "secret-redacted".into(), .. });
}
if compressed != input.text {
    log_hook_event(&repo_root, &HookEvent { reason: "size-compressed".into(), .. });
}
```

**Tradeoff:** best observability — `gain`/`doctor` can report "N secrets
blocked from context this session" as a standalone security metric,
separate from token savings, which is a claim no competitor in the
research doc's landscape table makes. Costs: two log-event call sites
instead of one, and redaction now always runs (small CPU cost) even for
tool results `is_shell` used to skip entirely — acceptable since the whole
point is covering results the Bash-only path currently misses.

---

## Variant C — version-gated, fail-degrade

Same core change as A, but wrapped in a capability check: probe/cache
whether the installed Claude Code honors `updatedToolOutput` for non-MCP
tools (env var, version string, or a one-time `doctor`-style check
persisted to `.tokenix/`), and only *rely on* the model actually receiving
the redacted text when the capability is confirmed. On unknown/old
installs, still emit the JSON (harmless if ignored) but additionally write
a local alert (e.g., `.tokenix/secrets-alerts.log`) so a detected secret is
never silently lost just because an old harness might not apply the field.

```rust
let supports_update = claude_code_supports_updated_output();  // cached probe

let (redacted, secret_hit) = redact_known_secrets(&input.text);
let compressed = /* same as B */;

if secret_hit && !supports_update {
    append_secrets_alert(&repo_root, &input.tool_name, &input.command);  // fallback channel
}
if compressed != input.text {
    println!("{}", serde_json::json!({ "hookSpecificOutput": {
        "hookEventName": "PostToolUse", "updatedToolOutput": compressed } }));
}
```

**Tradeoff:** the only variant that treats "did the model actually receive
the redacted text" as a first-class question instead of an assumption —
matters because a secret that silently fails to redact on an old install
is worse than doing nothing (false sense of security). Costs: a version
probe with its own failure modes to design (what does the check look like
concretely?), and a second alerting channel to maintain that the other two
variants don't need.

---

## Recommendation (not a selection — user decides)

B is the strongest fit for "mais benefícios": it's still a same-scale diff
to A, but makes the security win *visible and reportable*, not just
present — which is exactly the axis this pivot was chosen for over T0.3.
A is the fallback if minimal diff matters more than the reporting split.
C is worth it only if there's real reason to distrust the Claude Code
version matrix in practice — otherwise it's solving a problem the docs say
is already closed (v2.1.121+, and this repo's users are on recent installs
per CI/release cadence already tracked in project memory).

## Decision

**Selected: B — redaction-first, two-stage.** User confirmed directly
("b") against the artifact comparison, no comments requesting another
round.

**Rejected:**
- **A (minimal patch)** — smaller diff, but folds redaction and compression
  into one undifferentiated `saved_tokens` signal; loses the standalone
  "secrets blocked" metric that made B worth the small extra surface.
- **C (version-gated, fail-degrade)** — solves a problem the docs already
  say is closed (v2.1.121+); the capability probe and alert-log fallback
  add real complexity with no confirmed need.

Carried forward to `phase-plan`: redaction runs unconditionally on every
non-empty PostToolUse text, independent of `is_shell`/tool identity;
compression stays a second, tool-gated pass; two `HookEvent`s may fire per
call (`reason: "secret-redacted"` / `"size-compressed"`) so `gain`/`doctor`
can report blocked-secret count as its own metric, separate from token
savings.
