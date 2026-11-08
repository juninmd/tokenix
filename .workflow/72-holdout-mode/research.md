# Research — T0.3 holdout mode for `gain`

Issue: [#72](https://github.com/juninmd/tokenix/issues/72)

## Goal

Give `tokenix gain` a randomized control arm: `TOKENIX_HOLDOUT=<p>` makes a
fraction `p` of in-scope hook calls skip compression at random, tagged as
`holdout`, so token savings can be reported as a paired comparison
(compressed vs. uncompressed, same repo/session) instead of an uncontrolled
before/after number.

## Non-goals

- No OTLP receiver (T0.4), no NAP/action-preservation harness (T1.7), no
  `updatedToolOutput` PostToolUse path (T2.8), no CORE-Bench (T3.11). Each
  stays ranked behind this item in the research doc.
- No change to filter behavior, selection, or golden tests — holdout only
  gates *whether* compression runs on a given call, at random.
- No billed-cost accounting change — `gain::economics` already exists;
  holdout only needs to split its input population, not extend the pricing
  model in this pass.

## Acceptance criteria

1. `TOKENIX_HOLDOUT=0.1` (env var, float 0.0–1.0) makes ~10% of otherwise-
   compressible Bash/PowerShell hook calls pass through raw instead of being
   rewritten/filtered.
2. Every such call is logged as a distinct `HookEvent` state (new `action`
   value, e.g. `"holdout"`) — never silently merged into `"pass"` or
   `"bypassed"`, which already mean different things (unsupported tool /
   explicit `TOKENIX_DISABLED`).
3. Unset (default) → byte-identical behavior to today, zero overhead: the
   holdout check must short-circuit before any RNG call when the env var is
   absent.
4. `tokenix gain` gains a report mode that splits events by arm (`intercepted`
   vs `holdout`) and prints paired stats: mean/median tokens per call in each
   arm, sample sizes, and a naive interval (reuse `gain::session_stats`'
   existing shape rather than inventing new statistics machinery this pass —
   the research doc explicitly warns off ad hoc significance claims without
   the sample sizes literature ties to FWER, see docs/research pass 3).
5. `cargo fmt --check`, `cargo clippy --all-targets`, `cargo test` clean;
   existing 454 tests unaffected; new tests cover: env var parsing/bounds
   clamping, RNG gating at p=0 (never holds out) and p=1 (always holds out),
   and the new `HookEvent` action appears in `gain` output.

## Codebase map

- `src/hook.rs:856` `run_hook()` — hook entry point, reads `HookInput`,
  dispatches Bash/PowerShell/Read/Grep. This is where the holdout gate must
  sit, right after the existing early-outs (`TOKENIX_DISABLED` at
  `hook.rs:918`/`1085`, recording capture at `hook.rs:941`) and before any
  filter/compression rewrite is chosen.
- `src/hook.rs:915-935` existing per-command escape hatch pattern
  (`TOKENIX_DISABLED=1`) — same shape to copy for the holdout log event, but
  holdout is *not* a command-embedded flag; it is a session-wide env var
  read once via `std::env::var`, same idiom as `TOKENIX_GREP_HEAD_LIMIT`
  (`hook.rs:696`).
- `src/store.rs:1740` `struct HookEvent` — `action: String` field, currently
  takes `"pass" | "bypassed" | "intercepted"`. Add `"holdout"` as a fourth
  value; no schema/serde change needed since it is already a plain `String`.
- `src/store.rs:1810` `log_hook_event()` — existing sink, already handles
  redaction and rotation; holdout events flow through unchanged.
- `src/gain.rs:165` `economics()`, `src/gain.rs:221` `struct SessionStats`,
  `src/gain.rs:231` `session_stats()` — existing aggregation machinery to
  extend/reuse for the two-arm split, rather than building a parallel stats
  path.
- No RNG dependency currently used for this purpose in the hook path — need
  a source of randomness; check `Cargo.toml` for an existing `rand`
  dependency (fastembed/ort's transitive deps may already pull one in) before
  adding a new direct one, per repo's dependency-minimalism history (deny.toml,
  supply-chain hardening pass in memory `project_supply_chain_followups`).

## Prior art / literature (already synthesized in-repo)

`docs/research/2026-08-token-economy.md`, Tier 0 item 3: "Holdout mode.
`TOKENIX_HOLDOUT=0.1` to leave a random slice uncompressed as a control
group, with paired CIs. headroom ships this; it is now the bar, and without
it no causal claim is possible." Reaffirmed open, unchanged rank, through
pass 5 (2026-08-24, current HEAD `563fd1c`).

Why this matters (already established in the doc, not re-derived here):
- arXiv 2607.12161 (2,908 provider-billed Claude Code runs): token reduction
  vs. cost change has Pearson r = 0.154 (CI crosses zero) — an uncontrolled
  before/after token count is not evidence of cost savings.
- JetBrains SkillsBench corroboration (425 billed trials): "a tool's
  self-reported savings are a claim about its counterfactual, not about your
  bill" — rtk's own analytics showed 96M tokens saved while cost rose.
- Competitor `headroom` already ships a holdout mechanism; this closes a
  concrete competitive gap, not a theoretical one.

## Constraints

- Windows-primary repo (PowerShell + Bash hook paths both need the gate —
  `hook.rs` already branches on `is_bash`/`is_powershell`, holdout check
  should sit above that branch so it covers both).
- Fail-open discipline used everywhere else in `hook.rs`/`store.rs` (malformed
  env var → treat as unset/0, never panic, never block the hook).
- rust-version floor 1.88; no new heavyweight dependency without checking
  `deny.toml`/existing transitive RNG availability first (SecOps priority
  per user's global CLAUDE.md: minimal, audited dependency surface).
- Must not interact badly with the existing `TOKENIX_DISABLED=1` per-command
  bypass or `recordings::is_in_scope` capture path — holdout should not fire
  on commands already excluded for another reason (avoid double-counting a
  bypassed call as also "holdout").

## Risks / open questions (queued for phase-plan)

1. RNG source: is there already a `rand`-family crate in the dependency tree
   (transitively via fastembed/ort/rusqlite), or does this need a new direct
   dependency? Affects supply-chain review scope.
2. Determinism for tests: should holdout selection be seedable (env var like
   `TOKENIX_HOLDOUT_SEED`) for reproducible CI tests, or is thread_rng()
   with p=0/p=1 boundary tests sufficient? Non-boundary p values are
   inherently probabilistic to test — need a strategy (e.g. large N + loose
   bound, or inject a seedable RNG behind a trait for unit tests).
3. Where exactly does compression get "chosen" per command — is there one
   single funnel point in `run_hook` after all the early-outs, or do Bash
   and PowerShell paths each need their own gate copied in (like
   `TOKENIX_DISABLED` currently is, at two separate line ranges)? Needs a
   closer read of the ~200 lines after `hook.rs:973` before writing the plan.
4. `gain`'s report format: new subcommand/flag (`gain --holdout-report`) vs.
   folding into existing `gain --economics`? Determines CLI surface change.
5. Does a `HookEvent.action == "holdout"` event still need `saved_tokens` /
   `original_estimate` populated (i.e., compute what *would* have been
   saved, for the comparison) or leave them 0 like `"pass"`/`"bypassed"`
   today? This is the crux of what makes the arm comparison meaningful —
   likely needs `original_estimate` populated (raw output size) even though
   no compression ran, so `gain` can compute "tokens if compressed" as a
   counterfactual estimate for the holdout arm... but a *true* counterfactual
   requires running the filter to know its size without applying it, which
   is more invasive. Simpler v1: just compare raw-observed-tokens between
   arms directly (holdout arm's actual delivered tokens vs. intercepted arm's
   actual delivered tokens) — defer counterfactual-per-call to a later pass.
   Flag this tradeoff explicitly in the plan questionnaire.
