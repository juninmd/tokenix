use crate::store::{list_all_project_logs, read_hook_log, read_hook_log_from_path, HookEvent};
use std::path::Path;

pub struct ModelPrice {
    pub name: &'static str,
    pub input_per_1m: f64,
    /// Output (completion) token rate. Used by `tokenix usage` for absolute spend.
    pub output_per_1m: f64,
    /// Cached-input read rate (typically a fraction of input).
    pub cache_read_per_1m: f64,
    /// Cache-write (creation) rate (typically a small premium over input).
    pub cache_write_per_1m: f64,
    pub reference: bool,
}

pub const PRICING_COLLECTED_AT: &str = "2026-06-11";

pub const MODELS: &[ModelPrice] = &[
    // Anthropic (source: platform.claude.com/docs/about-claude/pricing, collected 2026-06-11).
    // Anthropic convention: cache read = 0.1x input, cache write (5m) = 1.25x input.
    ModelPrice {
        name: "claude-haiku-4-5",
        input_per_1m: 1.00,
        output_per_1m: 5.00,
        cache_read_per_1m: 0.10,
        cache_write_per_1m: 1.25,
        reference: false,
    },
    ModelPrice {
        name: "claude-sonnet-4-6",
        input_per_1m: 3.00,
        output_per_1m: 15.00,
        cache_read_per_1m: 0.30,
        cache_write_per_1m: 3.75,
        reference: true,
    },
    ModelPrice {
        name: "claude-opus-4-8",
        input_per_1m: 5.00,
        output_per_1m: 25.00,
        cache_read_per_1m: 0.50,
        cache_write_per_1m: 6.25,
        reference: false,
    },
    ModelPrice {
        name: "claude-fable-5",
        input_per_1m: 10.00,
        output_per_1m: 50.00,
        cache_read_per_1m: 1.00,
        cache_write_per_1m: 12.50,
        reference: false,
    },
    // OpenAI (source: developers.openai.com/api/docs/pricing, collected 2026-06-11).
    // OpenAI has no separate cache-write; cached input is a discounted read (~0.25x).
    ModelPrice {
        name: "gpt-5.4-mini",
        input_per_1m: 0.75,
        output_per_1m: 3.00,
        cache_read_per_1m: 0.19,
        cache_write_per_1m: 0.75,
        reference: false,
    },
    ModelPrice {
        name: "gpt-5.4",
        input_per_1m: 2.50,
        output_per_1m: 10.00,
        cache_read_per_1m: 0.63,
        cache_write_per_1m: 2.50,
        reference: false,
    },
    ModelPrice {
        name: "gpt-5.5",
        input_per_1m: 5.00,
        output_per_1m: 20.00,
        cache_read_per_1m: 1.25,
        cache_write_per_1m: 5.00,
        reference: false,
    },
    // Google (source: ai.google.dev/gemini-api/docs/pricing, collected 2026-06-11).
    ModelPrice {
        name: "gemini-3.1-flash-lite",
        input_per_1m: 0.25,
        output_per_1m: 1.00,
        cache_read_per_1m: 0.06,
        cache_write_per_1m: 0.25,
        reference: false,
    },
    ModelPrice {
        name: "gemini-3.5-flash",
        input_per_1m: 1.50,
        output_per_1m: 6.00,
        cache_read_per_1m: 0.38,
        cache_write_per_1m: 1.50,
        reference: false,
    },
    ModelPrice {
        name: "gemini-3.1-pro-preview",
        input_per_1m: 2.00,
        output_per_1m: 8.00,
        cache_read_per_1m: 0.50,
        cache_write_per_1m: 2.00,
        reference: false,
    },
];

/// Look up pricing for a model id, matching by exact name then by prefix
/// (transcripts may carry suffixed ids like `claude-opus-4-8-20260101`).
pub fn price_for(model: &str) -> Option<&'static ModelPrice> {
    MODELS
        .iter()
        .find(|m| m.name == model)
        .or_else(|| MODELS.iter().find(|m| model.starts_with(m.name)))
        .or_else(|| {
            // Fall back to family match (e.g. "claude-opus" -> opus entry).
            MODELS.iter().find(|m| {
                let fam: String = m.name.split('-').take(2).collect::<Vec<_>>().join("-");
                !fam.is_empty() && model.starts_with(&fam)
            })
        })
}

/// Absolute USD cost of a single usage record, per token category.
pub fn usage_cost(
    price: &ModelPrice,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
) -> f64 {
    (input as f64 * price.input_per_1m
        + output as f64 * price.output_per_1m
        + cache_read as f64 * price.cache_read_per_1m
        + cache_write as f64 * price.cache_write_per_1m)
        / 1_000_000.0
}

pub struct CostRow {
    pub model: &'static str,
    pub reference: bool,
    pub without_usd: f64,
    pub with_usd: f64,
    pub saved_usd: f64,
}

pub struct Economics {
    pub spend_usd: f64,
    pub saved_usd: f64,
    pub pct_of_spend: f64,
    pub total_tokens: u64,
    pub saved_tokens: i64,
}

/// Price measured savings against the user's OWN spend mix from transcripts,
/// not list-price hypotheticals. A weighted input-equivalent cost-per-token is
/// derived from the actual usage (output ≈ 5× input, cache write ≈ 1.25×,
/// cache read ≈ 0.1× — Anthropic's price ratios), then multiplied by the
/// measured saved tokens (which land on the input/cache side as tool
/// results). Returns None when no transcript usage is available.
///
/// The ratios are Anthropic's. `mix.cost_usd` itself comes from each record's
/// own model price, so the total spend is right for any provider, but the
/// weighting that splits it into a per-input-token rate is approximate when the
/// transcripts are dominated by a non-Anthropic model. The output is an
/// estimate and is presented as one.
pub fn economics(tokens_saved: i64) -> Option<Economics> {
    let mix = crate::usage::spend_mix()?;
    let units = mix.input as f64
        + 5.0 * mix.output as f64
        + 1.25 * mix.cache_write as f64
        + 0.1 * mix.cache_read as f64;
    if units <= 0.0 || mix.cost_usd <= 0.0 {
        return None;
    }
    let cpt = mix.cost_usd / units;
    let saved_usd = tokens_saved.max(0) as f64 * cpt;
    Some(Economics {
        spend_usd: mix.cost_usd,
        saved_usd,
        // Cap at 100%: this is "savings measured against what was actually
        // spent", and a long filtering history against a short billing window
        // could otherwise print a nonsensical 300%.
        pct_of_spend: (saved_usd / mix.cost_usd * 100.0).min(100.0),
        total_tokens: mix.input + mix.output + mix.cache_read + mix.cache_write,
        saved_tokens: tokens_saved.max(0),
    })
}

#[allow(dead_code)]
pub struct GainStats {
    pub total_calls: usize,
    pub intercepted: usize,
    pub passed: usize,
    pub tokens_saved: i64,
    pub tokens_used: i64,
    pub tokens_original: i64,
    pub pct_saved: f64,
    pub cost_rows: Vec<CostRow>,
    pub by_tool: Vec<(String, usize, i64)>,
    pub by_phase: Vec<(String, usize, i64)>,
    /// Top Bash commands compressed by the filter system, sorted by tokens saved.
    pub by_command: Vec<(String, usize, i64)>,
    /// Total semantic-search queries answered by the index (Read outlines + Grep).
    pub indexed_queries: usize,
    /// Commands that skipped filtering via the TOKENIX_DISABLED escape hatch.
    pub bypassed: usize,
}

/// Aggregate stats across all projects, with per-project breakdown.
pub struct GlobalGainStats {
    pub aggregate: GainStats,
    /// (label, tokens_saved, total_calls, intercepted) per project, desc by saved.
    pub projects: Vec<(String, i64, usize, usize)>,
}

/// Collapse a full Bash command line to a readable base label for the BY COMMAND
/// rollup. Keeps the leading executable + subcommand words and stops at the first
/// flag, redirect, shell operator, or argument carrying special characters — so
/// `pnpm run test --coverage 2>&1 | tail` → `pnpm run test`, and the many
/// `cargo test … | …` variants collapse into one `cargo test` bucket instead of
/// cluttering the footer with full argument strings.
pub(crate) fn command_label(cmd: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for tok in cmd.split_whitespace() {
        let stop = tok.starts_with('-') || tok.chars().any(|c| "|&;<>=\"'/\\`$()".contains(c));
        if stop {
            break;
        }
        parts.push(tok);
        if parts.len() >= 4 {
            break;
        }
    }
    if parts.is_empty() {
        // e.g. `VAR=x cmd` — fall back to the first token so the bucket isn't empty.
        cmd.split_whitespace().next().unwrap_or(cmd).to_string()
    } else {
        parts.join(" ")
    }
}

fn stats_from_events(events: Vec<HookEvent>) -> GainStats {
    let intercepted_events: Vec<_> = events
        .iter()
        .filter(|e| e.action == "intercepted")
        .collect();
    let passed_events: Vec<_> = events.iter().filter(|e| e.action == "pass").collect();

    let bypassed = events.iter().filter(|e| e.action == "bypassed").count();
    let tokens_saved: i64 = intercepted_events.iter().map(|e| e.saved_tokens).sum();
    let tokens_used: i64 = intercepted_events.iter().map(|e| e.actual_tokens).sum();
    let tokens_original: i64 = intercepted_events.iter().map(|e| e.original_estimate).sum();

    let pct_saved = if tokens_original > 0 {
        (tokens_saved as f64 / tokens_original as f64) * 100.0
    } else {
        0.0
    };

    let cost_rows = MODELS
        .iter()
        .map(|m| {
            let rate = m.input_per_1m / 1_000_000.0;
            let without_usd = tokens_original as f64 * rate;
            let with_usd = tokens_used as f64 * rate;
            CostRow {
                model: m.name,
                reference: m.reference,
                without_usd,
                with_usd,
                saved_usd: without_usd - with_usd,
            }
        })
        .collect();

    let mut by_tool_map: std::collections::HashMap<String, (usize, i64)> =
        std::collections::HashMap::new();
    for e in &intercepted_events {
        let entry = by_tool_map.entry(e.tool.clone()).or_default();
        entry.0 += 1;
        entry.1 += e.saved_tokens;
    }
    let mut by_tool: Vec<(String, usize, i64)> = by_tool_map
        .into_iter()
        .map(|(k, (c, s))| (k, c, s))
        .collect();
    by_tool.sort_by_key(|row| std::cmp::Reverse(row.2));

    let mut by_phase_map: std::collections::HashMap<String, (usize, i64)> =
        std::collections::HashMap::new();
    for e in &intercepted_events {
        let entry = by_phase_map.entry(e.phase.clone()).or_default();
        entry.0 += 1;
        entry.1 += e.saved_tokens;
    }
    let mut by_phase: Vec<(String, usize, i64)> = by_phase_map
        .into_iter()
        .map(|(k, (c, s))| (k, c, s))
        .collect();
    by_phase.sort_by(|a, b| a.0.cmp(&b.0));

    // Bash commands compressed by the filter system (command field non-empty).
    let mut by_cmd_map: std::collections::HashMap<String, (usize, i64)> =
        std::collections::HashMap::new();
    for e in &intercepted_events {
        if !e.command.is_empty() {
            let entry = by_cmd_map.entry(command_label(&e.command)).or_default();
            entry.0 += 1;
            entry.1 += e.saved_tokens;
        }
    }
    let mut by_command: Vec<(String, usize, i64)> = by_cmd_map
        .into_iter()
        .map(|(k, (c, s))| (k, c, s))
        .collect();
    by_command.sort_by_key(|row| std::cmp::Reverse(row.2));

    // Count semantic-index intercepts (Read/Grep — events without a command).
    let indexed_queries = intercepted_events
        .iter()
        .filter(|e| e.command.is_empty())
        .count();

    GainStats {
        total_calls: events.len(),
        intercepted: intercepted_events.len(),
        passed: passed_events.len(),
        tokens_saved,
        tokens_used,
        tokens_original,
        pct_saved,
        cost_rows,
        by_tool,
        by_phase,
        by_command,
        indexed_queries,
        bypassed,
    }
}

pub fn compute_gain(repo_root: &Path) -> GainStats {
    stats_from_events(read_hook_log(repo_root))
}

/// Aggregate gain stats across every project that has a hook log in `~/.tokenix/`.
pub fn compute_global_gain() -> GlobalGainStats {
    let project_logs = list_all_project_logs();

    let mut all_events: Vec<HookEvent> = Vec::new();
    let mut projects: Vec<(String, i64, usize, usize)> = Vec::new();

    for entry in &project_logs {
        let events = read_hook_log_from_path(&entry.log_path);
        let saved: i64 = events
            .iter()
            .filter(|e| e.action == "intercepted")
            .map(|e| e.saved_tokens)
            .sum();
        let total = events.len();
        let intercepted = events.iter().filter(|e| e.action == "intercepted").count();
        if total > 0 {
            projects.push((entry.label.clone(), saved, total, intercepted));
        }
        all_events.extend(events);
    }

    // Sort projects by tokens saved descending.
    projects.sort_by_key(|(_, saved, _, _)| std::cmp::Reverse(*saved));

    GlobalGainStats {
        aggregate: stats_from_events(all_events),
        projects,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{log_hook_event, HookEvent};

    fn create_test_temp_dir(sub: &str) -> std::path::PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let p = std::env::temp_dir().join("tokenix_test_gain").join(format!(
            "{}_{}_{}",
            sub,
            std::process::id(),
            unique
        ));
        let _ = std::fs::remove_dir_all(&p);
        let _ = std::fs::create_dir_all(&p);
        p
    }

    #[test]
    fn test_compute_gain_empty() {
        let temp_dir = create_test_temp_dir("empty");
        let stats = compute_gain(&temp_dir);
        assert_eq!(stats.total_calls, 0);
        assert_eq!(stats.intercepted, 0);
        assert_eq!(stats.passed, 0);
        assert_eq!(stats.tokens_saved, 0);
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_compute_gain_with_events() {
        let temp_dir = create_test_temp_dir("events");

        let ev1 = HookEvent {
            ts: 1234567.0,
            tool: "Bash".to_string(),
            action: "intercepted".to_string(),
            phase: "post".to_string(),
            reason: "".to_string(),
            saved_tokens: 100,
            actual_tokens: 20,
            original_estimate: 120,
            input_preview: "".to_string(),
            command: "".to_string(),
        };
        let ev2 = HookEvent {
            ts: 1234568.0,
            tool: "Read".to_string(),
            action: "pass".to_string(),
            phase: "pre".to_string(),
            reason: "not intercepted".to_string(),
            saved_tokens: 0,
            actual_tokens: 0,
            original_estimate: 0,
            input_preview: "".to_string(),
            command: "".to_string(),
        };

        log_hook_event(&temp_dir, &ev1).unwrap();
        log_hook_event(&temp_dir, &ev2).unwrap();

        let stats = compute_gain(&temp_dir);
        assert_eq!(stats.total_calls, 2);
        assert_eq!(stats.intercepted, 1);
        assert_eq!(stats.passed, 1);
        assert_eq!(stats.tokens_saved, 100);
        assert_eq!(stats.tokens_used, 20);
        assert_eq!(stats.tokens_original, 120);
        assert!((stats.pct_saved - 83.333).abs() < 0.01);
        assert_eq!(stats.by_tool.len(), 1);
        assert_eq!(stats.by_tool[0], ("Bash".to_string(), 1, 100));
        assert_eq!(stats.by_phase.len(), 1);
        assert_eq!(stats.by_phase[0], ("post".to_string(), 1, 100));
        // ev1 carries no command → counted as an indexed query.
        assert_eq!(stats.indexed_queries, 1);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_source_split_index_vs_filters() {
        let temp_dir = create_test_temp_dir("source_split");

        let index_ev = HookEvent {
            ts: 1.0,
            tool: "Read".to_string(),
            action: "intercepted".to_string(),
            phase: "pre".to_string(),
            reason: "outline".to_string(),
            saved_tokens: 300,
            actual_tokens: 50,
            original_estimate: 350,
            input_preview: String::new(),
            command: String::new(),
        };
        let filter_ev = HookEvent {
            ts: 2.0,
            tool: "Bash".to_string(),
            action: "intercepted".to_string(),
            phase: "ToolOutputCompressed".to_string(),
            reason: "compressed command output".to_string(),
            saved_tokens: 200,
            actual_tokens: 30,
            original_estimate: 230,
            input_preview: String::new(),
            command: "cargo test".to_string(),
        };

        // Pre-phase rewrite marker for the same command: saved is 0 and the
        // event must not inflate the filter call count.
        let rewrite_marker = HookEvent {
            ts: 1.5,
            tool: "Bash".to_string(),
            action: "intercepted".to_string(),
            phase: "pre".to_string(),
            reason: "rewrote command to tokenix run".to_string(),
            saved_tokens: 0,
            actual_tokens: 0,
            original_estimate: 0,
            input_preview: String::new(),
            command: "cargo test".to_string(),
        };

        log_hook_event(&temp_dir, &index_ev).unwrap();
        log_hook_event(&temp_dir, &rewrite_marker).unwrap();
        log_hook_event(&temp_dir, &filter_ev).unwrap();

        let stats = compute_gain(&temp_dir);
        assert_eq!(stats.indexed_queries, 1);
        assert_eq!(stats.tokens_saved, 500);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    fn intercepted(tool: &str, command: &str, original: i64, actual: i64) -> HookEvent {
        HookEvent {
            ts: 0.0,
            tool: tool.to_string(),
            action: "intercepted".to_string(),
            phase: "post".to_string(),
            reason: String::new(),
            saved_tokens: (original - actual).max(0),
            actual_tokens: actual,
            original_estimate: original,
            input_preview: String::new(),
            command: command.to_string(),
        }
    }

    #[test]
    fn cost_rows_price_savings_per_model() {
        let temp_dir = create_test_temp_dir("cost");
        // 1M original tokens compressed to 200k used → 800k saved.
        log_hook_event(
            &temp_dir,
            &intercepted("Bash", "cargo build", 1_000_000, 200_000),
        )
        .unwrap();

        let stats = compute_gain(&temp_dir);
        assert_eq!(stats.cost_rows.len(), MODELS.len());
        // Exactly one reference model (sonnet) drives the headline figure.
        assert_eq!(stats.cost_rows.iter().filter(|r| r.reference).count(), 1);

        let sonnet = stats
            .cost_rows
            .iter()
            .find(|r| r.model == "claude-sonnet-4-6")
            .expect("sonnet present");
        // $3 / 1M input tokens.
        assert!((sonnet.without_usd - 3.0).abs() < 1e-9);
        assert!((sonnet.with_usd - 0.6).abs() < 1e-9);
        assert!((sonnet.saved_usd - 2.4).abs() < 1e-9);
        // saved is always without - with, for every model.
        for r in &stats.cost_rows {
            assert!((r.saved_usd - (r.without_usd - r.with_usd)).abs() < 1e-9);
            assert!(r.saved_usd >= 0.0, "{} saved negative", r.model);
        }

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn price_for_matches_exact_and_suffixed_ids() {
        assert_eq!(
            price_for("claude-sonnet-4-6").map(|m| m.name),
            Some("claude-sonnet-4-6")
        );
        // Suffixed transcript ids resolve by prefix to the base model.
        assert_eq!(
            price_for("claude-opus-4-8-20260101").map(|m| m.name),
            Some("claude-opus-4-8")
        );
        assert!(price_for("totally-unknown-model").is_none());
    }

    #[test]
    fn usage_cost_sums_all_token_categories() {
        let p = price_for("claude-sonnet-4-6").unwrap();
        // 1M each of input/output/cache_read/cache_write at 3/15/0.30/3.75 per 1M.
        let cost = usage_cost(p, 1_000_000, 1_000_000, 1_000_000, 1_000_000);
        assert!((cost - (3.0 + 15.0 + 0.30 + 3.75)).abs() < 1e-9);
        // Zero usage costs nothing.
        assert_eq!(usage_cost(p, 0, 0, 0, 0), 0.0);
    }

    #[test]
    fn by_command_rollup_aggregates_same_base() {
        let temp_dir = create_test_temp_dir("rollup");
        // Two cargo-test variants collapse into one bucket; git status separate.
        log_hook_event(
            &temp_dir,
            &intercepted("Bash", "cargo test --all", 500, 100),
        )
        .unwrap();
        log_hook_event(
            &temp_dir,
            &intercepted("Bash", "cargo test -p core", 300, 50),
        )
        .unwrap();
        log_hook_event(&temp_dir, &intercepted("Bash", "git status -s", 80, 20)).unwrap();

        let stats = compute_gain(&temp_dir);
        let cargo = stats
            .by_command
            .iter()
            .find(|(c, _, _)| c == "cargo test")
            .expect("cargo test bucket");
        assert_eq!(cargo.1, 2, "two cargo invocations rolled up");
        assert_eq!(cargo.2, 400 + 250, "saved tokens summed");
        // Sorted by tokens saved desc → cargo (650) before git status (60).
        assert_eq!(stats.by_command[0].0, "cargo test");
        assert_eq!(stats.tokens_saved, 400 + 250 + 60);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn pct_saved_zero_without_original_tokens() {
        let temp_dir = create_test_temp_dir("pctzero");
        // An intercept that recorded no original estimate must not divide by zero.
        log_hook_event(&temp_dir, &intercepted("Bash", "noisy", 0, 0)).unwrap();
        let stats = compute_gain(&temp_dir);
        assert_eq!(stats.pct_saved, 0.0);
        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
