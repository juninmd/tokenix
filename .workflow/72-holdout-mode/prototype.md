# Prototype — T0.3 holdout mode

Three distinct mechanisms for `TOKENIX_HOLDOUT`. This is a hook-internal
Rust feature with no UI, so "clickable" variants are code sketches +
worked examples of the `gain` output each produces — throwaway, not meant
to compile as-is, never merged. Pick by mechanism, not by prose quality.

---

## Variant A — inline coin-flip, actual-only comparison

Single gate near the top of `run_hook`, before the Bash/PowerShell branch:

```rust
// hook.rs, right after the TOKENIX_DISABLED early-outs
if let Some(p) = holdout_fraction() {           // parses TOKENIX_HOLDOUT, clamps 0..1, None if unset
    if rand::random::<f64>() < p {
        log_hook_event(&repo_root, &HookEvent { action: "holdout".into(), .. });
        pass_through(antigravity);              // raw output, no compression, no filter
    }
}
```

`gain --holdout` buckets existing events by `action` and prints actual
delivered tokens per arm — no attempt to know what compression *would*
have done on a holdout call:

```
$ tokenix gain --holdout
arm          n      mean tok/call   median
intercepted  842    118             64
holdout      94     311             180
Δ (intercepted vs holdout, unpaired): -62.1% mean tokens/call
```

**Tradeoff:** smallest diff (~20 LOC in hook.rs + report branch in gain.rs),
zero new state files, matches existing `HookEvent`/`session_stats` shapes
exactly. Comparison is *unpaired* — arms differ in which commands landed
in each bucket by chance, not a per-call delta, so at low n the two arms
can just have different command mixes. Needs large n to trust.

---

## Variant B — counterfactual dual-run (shadow compression)

On a holdout draw, still run the filter pipeline to compute the size it
*would* have produced, log both numbers, deliver raw:

```rust
if is_holdout_draw(p) {
    let raw_tokens = count_tokens(&raw_output);
    let shadow = apply_filters(&raw_output, &command);   // same call the real path uses
    log_hook_event(&repo_root, &HookEvent {
        action: "holdout".into(),
        actual_tokens: raw_tokens,
        saved_tokens: raw_tokens - shadow.tokens,          // counterfactual, not applied
        original_estimate: raw_tokens,
        ..
    });
    pass_through(antigravity);   // still raw — the filter run above is throwaway
}
```

`gain --holdout` reports a *paired* per-call delta because every holdout
row already carries "what compression would have saved here":

```
$ tokenix gain --holdout
arm          n      mean would-save/call   paired 95% CI
holdout      94     58.3%                  [51.2%, 65.1%]
(intercepted arm's realized saving, same period: 61.0% — within CI, no evidence of divergence)
```

**Tradeoff:** the statistically strongest arm (real paired comparison, not
just distributional), but every holdout call now pays the filter's compute
cost twice-ish (run for real would've been once; here it's computed but
discarded) and, more importantly, doubles the code paths that must stay in
sync — a filter bug could show up only in the shadow path or only in the
real path if they ever drift, which is a foot-gun the research doc's
determinism section would flag.

---

## Variant C — sidecar ledger, zero schema change

Don't touch `HookEvent`/`store.rs` at all. Coin-flip lives in hook.rs same
as A, but instead of a new `action` value, it appends one line to a
separate `.tokenix/holdout.jsonl` (ts, tool, command hash) and *still*
lets the existing `"pass"`-shaped event get logged normally for that call
(so `hook.log` stays exactly the schema every existing consumer expects).
`gain --holdout` left-joins the two files by (ts, command hash) at report
time.

```rust
if is_holdout_draw(p) {
    append_holdout_ledger(&repo_root, ts, &command_hash);  // new tiny file, own rotation
    pass_through(antigravity);
}
```

```
$ tokenix gain --holdout
(joins .tokenix/hook.log × .tokenix/holdout.jsonl on ts+hash)
arm          n      mean tok/call
intercepted  842    118
holdout      94     311
```

**Tradeoff:** lowest blast radius on shared state (`HookEvent` struct and
its serde shape are untouched, so nothing that reads `hook.log` today needs
to learn a new `action` value) — but adds a second log file, a join step in
`gain`, and a rotation/GC policy to duplicate from `store.rs`'s existing one.
Same unpaired-comparison limitation as A, for more moving parts.

---

## Recommendation (not a selection — user decides)

A is the default "just ship the bar the research doc asks for" choice:
cheapest, matches existing patterns exactly (`HookEvent.action` already has
three ad hoc string values, a fourth is idiomatic), unpaired comparison is
an accepted limitation the acceptance criteria in research.md already
scoped down to ("defer counterfactual-per-call to a later pass"). B is the
correct answer if the *causal* claim needs to be airtight in this same
pass rather than deferred. C only pays off if `HookEvent` schema stability
is a hard constraint for some external consumer not yet identified.
