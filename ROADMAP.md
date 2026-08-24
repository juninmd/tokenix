# Tokenix Optimization Roadmap

**Goal**: Reduce token waste in Claude Code workflows through intelligent symbol indexing and semantic search.

## Next Up (P2 — deferred from the 2026-06 optimization pass)

Shipped in that pass: below-normal-priority indexing, embed progress bar, `--json`
output across retrieval commands, hook.log rotation, `daemon status|stop|restart`,
5 new filters (cargo-run, uv-add, pnpm-run, vite, node-test), int8-quantized
embeddings (4x smaller DB/RAM), file-level import graph + `tokenix deps`,
per-batch embed durability with crash resume, incremental symbol-graph repair,
configurable hook thresholds (`[hook]` in `.tokenix.toml`), `symbols --kind`.

Deferred:

1. **New tree-sitter languages** — Java, C#, Ruby, PHP, Kotlin, Swift (skipped by
   owner request 2026-06-10). Pattern per language ≈ 30-50 LOC: `Lang` enum +
   `detect_lang` + `is_<lang>_symbol()` + dispatch in `chunker.rs`, reference
   arm in `graph.rs`, fixture tests. Watch out for per-grammar identifier node
   kinds (`constant` Ruby, `name` PHP, `simple_identifier` Kotlin/Swift) in
   `find_first_identifier`.
2. **`tokenix watch`** — explicit opt-in file watcher (`notify` crate, 2-5 s
   debounce) running the git-incremental index at low priority. No auto-hooks.
3. **ANN gate (HNSW)** — only if int8 brute-force proves insufficient past
   ~50-100k chunks. Sidecar index, incremental rebuild. Do not build speculatively.
4. **Transitive `--depth` on callers/impact** — BFS over `graph_edges` with the
   existing SCC data as cycle guard.
5. **Diff-aware Read intercept** — for files modified since last index, return
   outline + changed-hunk context instead of full content. Measure with `gain`.
6. **`filter generate` template fallback** — skeleton TOML from captured output
   when no AI CLI is available. (`semantic_filter.model` validation in `doctor`
   shipped 2026-06-10 robustness pass, along with the debug-build stack-overflow
   fix, SQLite busy_timeout, and the daemon stale-cache reload fix.)

## Key Insights from Cluster Operations

### 1. **Verbose Output Patterns**
- **Problem**: Large kubectl output dumps (40+ lines per status check) consume 15-20% of session tokens
- **Solution**: Implement filtered output templates
  - Add `--output-filter` flag to CLI
  - Pre-define queries: `tokenix query "failing jobs"` → structured results only
  - Summary mode: Return counts + top-N matches instead of full enumeration

### 2. **CronJob & Deployment Monitoring**
- **Pattern**: Repeated status checks across namespaces cause 2x token overhead
- **Solution**: Add `watch` mode with delta reporting
  - Track last known state (in `.tokenix/cache`)
  - Report only changes: `NEW_FAILED: 3`, `RESOLVED: 2`
  - Reduce poll frequency with smart debouncing

### 3. **YAML Structural Errors**
- **Problem**: Duplicate key detection requires full file reads + validation (100+ tokens per file)
- **Solution**: Implement lightweight YAML linter
  - Fast path: Regex check for `spec:` duplicates before full parse
  - Index YAML keys at write time (`.tokenix/yaml-keys/`)
  - Report errors with line numbers in one pass

### 4. **Cross-Namespace Job Tracking**
- **Current**: List all jobs, filter by pattern → wasteful
- **Proposed**: Namespace-scoped indexing
  - `tokenix index --namespace evo-agent` → build local index
  - Fast queries: `tokenix query "failed crawl jobs" --namespace evo-agent`
  - Support multi-namespace aggregation without re-scanning

### 5. **ArgoCD Sync Status**
- **Problem**: Full application list returned even for single-app query
- **Solution**: Build ArgoCD application graph
  - Index app dependencies and sync chains
  - `tokenix query "sync blockers"` → return only blocking apps + why
  - Cache graph (TTL 5 min) to avoid repeated API hits

## Implementation Priorities

### Phase 1 (Quick wins - 2-3 days)
1. **Output filtering templates**
   - `--summary-only` flag for kubectl wrappers
   - `--top-n 5` to limit large result sets
   - Token savings: ~20%

2. **YAML linter fast-path**
   - Pre-check for common errors before full parse
   - Cache structural metadata
   - Token savings: ~15%

3. **Delta reporting**
   - Track state in `.tokenix/last-state.json`
   - Report only what changed
   - Token savings: ~25% on repeated checks

### Phase 2 (Medium-term - 1-2 weeks)
1. **Namespace-scoped indexes**
   - Parallel indexing for large clusters
   - Fast multi-namespace queries

2. **ArgoCD graph indexing**
   - Dependency tracking
   - Sync blocker detection

3. **Smart caching layer**
   - TTL-based invalidation
   - Cache key versioning

### Phase 3 (Long-term)
1. **Semantic compression**
   - Summarize similar log entries: `[5 similar error lines]`
   - Cluster warnings by root cause

2. **Context-aware query optimization**
   - Learn from query patterns
   - Suggest optimal search strategies

## Token Budget Targets

| Operation | Current | Target | Savings |
|-----------|---------|--------|---------|
| Full cluster status | 1200 tokens | 400 tokens | 67% |
| Job monitoring | 600 tokens | 150 tokens | 75% |
| App sync check | 800 tokens | 250 tokens | 69% |
| YAML validation | 300 tokens | 50 tokens | 83% |
| **Session avg** | **~2900** | **~850** | **~70%** |

## File Structure

```
tokenix/
├── src/
│   ├── cli.rs (add --summary-only, --top-n, --delta-only flags)
│   ├── index/
│   │   ├── yaml.rs (lightweight YAML key extractor)
│   │   ├── argocd.rs (app dependency graph)
│   │   └── namespace.rs (per-namespace indexing)
│   └── cache/
│       ├── state.rs (track last known state)
│       └── ttl.rs (invalidation logic)
├── .tokenix/
│   ├── cache/ (state snapshots, TTLs)
│   ├── indexes/ (namespace-scoped indexes)
│   └── yaml-keys/ (YAML structure cache)
└── ROADMAP.md (this file)
```

## Measurement & Validation

1. **Benchmark suite**: `tokenix bench --session-like`
   - Simulate 20 typical cluster queries
   - Measure tokens before/after optimizations

2. **Integration test**: Run actual Claude Code session
   - Track token usage per query type
   - Compare to baseline

3. **Real-world validation**
   - Use in actual cluster ops (this session's patterns)
   - Measure feedback loop improvements

---

---

## Session-Validated Patterns (2026-05-29 cluster recovery)

Real token-burning sequences observed while bringing 38 ArgoCD apps to Synced+Healthy.
Each maps to a concrete proposed `tokenix k8s` subcommand that collapses N verbose calls
into one structured answer.

### P1. ArgoCD drift triage — the biggest sink
**Observed**: Diagnosing why an app was OutOfSync required a chain of ~6 calls per app:
`get application -o wide` → `jsonpath conditions` → `jsonpath operationState.message` →
`jsonpath resources[?(@.status=="OutOfSync")]` → per-resource `get -o jsonpath managedFields`
→ live-vs-git field compare. Repeated for 3 apps = ~18 calls, most returning huge JSON.

**Proposed**: `tokenix k8s drift <app>` → one line per OutOfSync resource with the *minimal*
diff and a root-cause classifier:
```
prometheus-stack/Prometheus  CSA-migration-blocked (manager: kubectl-client-side-apply)
monitoring-stack/ExternalSecret×3  operator-defaulted-fields (/spec/data, /spec/target/deletionPolicy)
cluster-config/PriorityClass app-low  missing-on-cluster (immutable value=100)
```
Classifier dictionary (seed from this session):
- `immutable-field` (PriorityClass value, resourceVersion:0 on update)
- `operator-defaulted-fields` (external-secrets conversionStrategy/decodingStrategy/metadataPolicy)
- `CSA-migration-blocked` (kubectl-client-side-apply manager + ServerSideApply=true)
- `annotation-too-long` (>262144 bytes last-applied on large CRDs)
- `managed-by-root-app` (local edits will be reverted by selfHeal — warn before editing)

Estimated saving: ~18 calls → 3 calls, ~85% fewer tokens on the dominant workflow.

### P2. "Local edit silently reverted" detector
**Observed**: Editing an ArgoCD-managed Application (monitoring-app.yaml) via `kubectl apply`
appeared to work ("configured") but root-app selfHeal reverted it; confirmed only after a
later `jsonpath spec.helm.values` showed the old content. Pure wasted round-trip.

**Proposed**: `tokenix k8s owner <resource>` returns `argocd:root-app (selfHeal=true)` and a
warning: "edits here revert unless pushed to git <repoURL>@<path>". Reads the
`argocd.argoproj.io/tracking-id` annotation; zero guessing.

### P3. YAML structural lint that matches the cluster's strictness
**Observed**: A duplicate `spec:` key in 4 cronjobs was tolerated by `kubectl apply`
(non-strict) but broke ArgoCD's `kustomize build` → app stuck Unknown for hours. Detecting it
cost a full `kubectl kustomize` run + per-file `sed`/`grep` hunting.

**Proposed**: `tokenix lint k8s <dir>` — strict-mode YAML/kustomize pre-flight that flags
duplicate keys, misplaced fields (e.g. `activeDeadlineSeconds` at CronJob.spec vs
jobTemplate.spec), and renders the same errors ArgoCD would, before commit. One call,
line-accurate.

### P4. Job/Pod failure rollup
**Observed**: `kubectl get jobs -A` + `awk` filtering + per-job `logs -l job-name=...` to find
the one error line, repeated for postgres-backup-verify, evo-agent, github-assistance. Logs
returned 20-30 lines to surface a single causal line ("Connection refused", "401 Unauthorized").

**Proposed**: `tokenix k8s failures [--since 24h]` → table of failed jobs with the *single*
extracted error line (regex: `error|failed|refused|unauthorized|timeout`), de-duplicated by
root cause. Turns ~10 calls of multi-line dumps into one rollup.

### P5. Orphan detector
**Observed**: Stray CronJobs in `default` ns (evo-agent-crawl, akitemquiz-cuts-daily) — not in
git, no ArgoCD tracking — spawned Pending pods hourly (missing PVC). Found only by manually
cross-checking tracking labels vs git.

**Proposed**: `tokenix k8s orphans` — lists live workloads with no `argocd.argoproj.io/tracking-id`
and no matching git manifest. Surfaces exactly this class of silent resource leak.

### Output-discipline lessons (apply to tokenix's own CLI)
- Default to **single-line-per-item** with a `--wide`/`--json` opt-in, never the reverse.
- Extract-then-show: for logs, return the matched causal line + ±1 context, not the tail.
- Always prefer `jsonpath`/projection over full `-o yaml`/`-o json` dumps (a single Prometheus
  CR or large CRD `-o yaml` is thousands of tokens; the needed field is ~10).

### Suggested first implementation slice
`tokenix k8s drift <app>` (P1) + `tokenix k8s failures` (P4) cover the two dominant sinks of
this session and reuse the same kube client + a small root-cause classifier table. Ship those
before the broader indexing work in Phases 1–3 above.

---

## Session Recovery Complete (2026-05-29)

**Cluster status**: ✅ **38 apps deployed, 37 Synced+Healthy, 1 OutOfSync but Healthy**
**Pod health**: ✅ 72 pods running, 0 CrashLoops, 0 failed
**Critical services**: ✅ evo-agent, fast-news, shorts-generator, queima-buchinho, vibe-code all operational
**Postgres backup**: ✅ Fixed (pg_isready retry loop added, tested successful)
**Monitoring**: ✅ Telegram alerts active for backup/scheduled job failures

### Fixes Applied This Session
1. **evo-agent Unknown** → Fixed 4 cronjobs with duplicate spec: key (kustomize build blocker)
2. **cluster-config OutOfSync** → Recreated missing PriorityClass app-low
3. **prometheus-stack CSA migration** → Added crds.enabled=false, removed >256KB annotations
4. **monitoring-stack ExternalSecret drift** → Added ignoreDifferences for operator defaults
5. **Orphaned cronjobs** → Deleted untracked evo-agent-crawl and akitemquiz-cuts-daily in default ns
6. **postgres-backup-verify Connection refused** → Added pg_isready 30-retry loop (60s timeout)
7. **Monitoring gaps** → Added DatabaseBackupFailed (critical) and ScheduledJobFailed (warning) alerts

### Artifacts Generated
- **tokenix ROADMAP.md**: Session-validated patterns (P1–P5) capturing token-burning workflows
- **app-charts commit d0a7bc4**: Cleanup 7 obsolete app manifests, 7 test files
- **Monitoring enhancements**: Telegram notifications for backup and scheduled job failures

### Next Phase: Tokenix K8s Subcommands
Priority order for implementation:
1. `tokenix k8s drift <app>` — Drift triage with classifier (immutable-field, CSA-migration-blocked, operator-defaulted-fields)
2. `tokenix k8s failures [--since 24h]` — Job/pod failure rollup with root-cause extraction
3. `tokenix k8s owner <resource>` — ArgoCD tracking + revert-on-edit warning
4. `tokenix lint k8s <dir>` — Strict YAML lint matching cluster validation (catches duplicate keys, field placement)
5. `tokenix k8s orphans` — Detect untracked live resources not in git

Estimated token savings from P1+P2: ~60% of typical cluster-ops workflow.

---

**Last updated**: 2026-05-29 (session recovery: 38 apps → fully operational)
**Owner**: Tokenix optimization initiative + app-charts cluster recovery
**Status**: Session complete; next phase is tokenix k8s subcommand implementation
## Competitive gap pass (2026-08)

Audit against [Graft](https://github.com/NanoNets/Graft),
[codebase-memory-mcp](https://github.com/DeusData/codebase-memory-mcp) and
[Agent-Reach](https://github.com/Panniantong/Agent-Reach).

Shipped in this pass:

1. **Freshness-on-query** (`freshness.rs`) — retrieval reconciles the working
   tree before answering, `--no-embed` path, fails open. Graft refreshes before
   every command; tokenix previously answered from the last index run.
2. **Modules** (`modules.rs`, `tokenix modules`) — Louvain communities over the
   symbol graph, the repo-orientation answer cbm gets from Louvain and Graft
   pays an LLM for. No API key, deterministic.
3. **Blast radius of a diff** (`blast.rs`, `tokenix blast`) — the `graft blast`
   equivalent, built on the existing graph.
4. **Team snapshots** (`snapshot.rs`, `export-index` / `import-index`) — the
   `.codebase-memory/graph.db.zst` idea: index once per repo, not once per dev.

Deliberately not done:

- **Tree-sitter breadth** (Java, C#, Ruby, PHP, Kotlin, Swift). Graft covers 21
  languages, cbm claims 158; tokenix covers 8. Still the weakest column, and
  still a per-language grammar dependency + binary-size decision the owner
  declined on 2026-06-10.
- **Route/IaC nodes** (HTTP/gRPC handler ↔ call site, Dockerfile/k8s manifests as
  graph nodes). Highest-value remaining gap and it overlaps the cluster items
  below; needs its own design pass.
- **Task-level correctness benchmark** (SWE-bench-style). Graft publishes
  correctness and billed cost; tokenix publishes tokens only. This is a harness
  project, not a code change, and the README already states the position.
- **Agent-Reach**: orthogonal (internet access). The one transferable idea is its
  capability layer — ordered backends with automatic fallback surfaced by
  `doctor`.