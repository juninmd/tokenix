use crate::store::read_hook_log;
use std::path::Path;

const COST_PER_1K_INPUT: f64 = 0.003;

#[allow(dead_code)]
pub struct GainStats {
    pub total_calls: usize,
    pub intercepted: usize,
    pub passed: usize,
    pub tokens_saved: i64,
    pub tokens_used: i64,
    pub tokens_original: i64,
    pub pct_saved: f64,
    pub cost_saved_usd: f64,
    pub by_tool: Vec<(String, usize, i64)>,
}

pub fn compute_gain(repo_root: &Path) -> GainStats {
    let events = read_hook_log(repo_root);
    let intercepted_events: Vec<_> = events
        .iter()
        .filter(|e| e.action == "intercepted")
        .collect();
    let passed_events: Vec<_> = events.iter().filter(|e| e.action == "pass").collect();

    let tokens_saved: i64 = intercepted_events.iter().map(|e| e.saved_tokens).sum();
    let tokens_used: i64 = intercepted_events.iter().map(|e| e.actual_tokens).sum();
    let tokens_original: i64 = intercepted_events.iter().map(|e| e.original_estimate).sum();

    let pct_saved = if tokens_original > 0 {
        (tokens_saved as f64 / tokens_original as f64) * 100.0
    } else {
        0.0
    };
    let cost_saved_usd = (tokens_saved as f64 / 1000.0) * COST_PER_1K_INPUT;

    let mut by_tool_map: std::collections::HashMap<String, (usize, i64)> =
        std::collections::HashMap::new();
    for e in &intercepted_events {
        let entry = by_tool_map.entry(e.tool.clone()).or_default();
        entry.0 += 1;
        entry.1 += e.saved_tokens;
    }
    let by_tool: Vec<(String, usize, i64)> = by_tool_map
        .into_iter()
        .map(|(k, (c, s))| (k, c, s))
        .collect();

    GainStats {
        total_calls: events.len(),
        intercepted: intercepted_events.len(),
        passed: passed_events.len(),
        tokens_saved,
        tokens_used,
        tokens_original,
        pct_saved,
        cost_saved_usd,
        by_tool,
    }
}
