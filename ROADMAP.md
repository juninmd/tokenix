# Tokenix Optimization Roadmap

**Goal**: Reduce token waste in Claude Code workflows through intelligent symbol indexing and semantic search.

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

**Last updated**: 2026-05-29  
**Owner**: Tokenix optimization initiative  
**Status**: In planning (session-validated patterns added from app-charts cluster recovery)
