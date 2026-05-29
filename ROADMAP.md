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

**Last updated**: 2026-05-29  
**Owner**: Tokenix optimization initiative  
**Status**: In planning
