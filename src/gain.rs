use crate::store::read_hook_log;
use std::path::Path;

pub struct ModelPrice {
    pub name: &'static str,
    pub input_per_1m: f64,
    pub reference: bool,
}

pub const PRICING_COLLECTED_AT: &str = "2026-05-07";

pub const MODELS: &[ModelPrice] = &[
    // Anthropic (source: anthropic.com/pricing, collected 2026-05-07)
    ModelPrice {
        name: "claude-haiku-4-5",
        input_per_1m: 1.00,
        reference: false,
    },
    ModelPrice {
        name: "claude-sonnet-4.6",
        input_per_1m: 3.00,
        reference: true,
    },
    ModelPrice {
        name: "claude-opus-4.7",
        input_per_1m: 5.00,
        reference: false,
    },
    // OpenAI (source: openai.com/api/pricing, collected 2026-05-07)
    ModelPrice {
        name: "gpt-5.4-mini",
        input_per_1m: 0.75,
        reference: false,
    },
    ModelPrice {
        name: "gpt-5.4",
        input_per_1m: 2.50,
        reference: false,
    },
    // Google (source: ai.google.dev/pricing, collected 2026-05-07)
    ModelPrice {
        name: "gemini-3.1-flash-preview",
        input_per_1m: 0.25,
        reference: false,
    },
    ModelPrice {
        name: "gemini-3.1-pro-preview",
        input_per_1m: 2.00,
        reference: false,
    },
];

pub struct CostRow {
    pub model: &'static str,
    pub reference: bool,
    pub without_usd: f64,
    pub with_usd: f64,
    pub saved_usd: f64,
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{log_hook_event, HookEvent};

    fn create_test_temp_dir(sub: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join("tokenix_test_gain").join(format!(
            "{}_{}",
            sub,
            std::process::id()
        ));
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

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
