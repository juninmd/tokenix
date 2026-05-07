use crate::store::read_hook_log;
use std::path::Path;

pub struct ModelPrice {
    pub name: &'static str,
    pub input_per_1m: f64,
    pub reference: bool,
}

pub const MODELS: &[ModelPrice] = &[
    // Anthropic (source: docs.anthropic.com/pricing, May 2026)
    ModelPrice { name: "claude-haiku-4-5",         input_per_1m: 1.00, reference: false },
    ModelPrice { name: "claude-sonnet-4.6",        input_per_1m: 3.00, reference: true  },
    ModelPrice { name: "claude-opus-4.7",          input_per_1m: 5.00, reference: false },
    // OpenAI (source: openai.com/api/pricing, May 2026)
    ModelPrice { name: "gpt-4.1-nano",             input_per_1m: 0.10, reference: false },
    ModelPrice { name: "gpt-4.1-mini",             input_per_1m: 0.40, reference: false },
    ModelPrice { name: "gpt-4.1",                  input_per_1m: 2.00, reference: false },
    ModelPrice { name: "gpt-5.4-nano",             input_per_1m: 0.20,  reference: false },
    ModelPrice { name: "gpt-5.4-mini",             input_per_1m: 0.75,  reference: false },
    ModelPrice { name: "gpt-5.4",                  input_per_1m: 2.50,  reference: false },
    ModelPrice { name: "gpt-5.4-pro",              input_per_1m: 30.00, reference: false },
    ModelPrice { name: "gpt-5.5",                  input_per_1m: 5.00,  reference: false },
    ModelPrice { name: "gpt-5.5-pro",              input_per_1m: 30.00, reference: false },
    ModelPrice { name: "o3",                       input_per_1m: 2.00,  reference: false },
    ModelPrice { name: "o4-mini",                  input_per_1m: 1.10,  reference: false },
    ModelPrice { name: "o3-pro",                   input_per_1m: 20.00, reference: false },
    // Google (source: ai.google.dev/gemini-api/docs/pricing, May 2026)
    ModelPrice { name: "gemini-2.5-flash-lite",    input_per_1m: 0.10, reference: false },
    ModelPrice { name: "gemini-2.5-flash",         input_per_1m: 0.30, reference: false },
    ModelPrice { name: "gemini-2.5-pro",           input_per_1m: 1.25, reference: false },
    ModelPrice { name: "gemini-3.1-flash-preview", input_per_1m: 0.25, reference: false },
    ModelPrice { name: "gemini-3.1-pro-preview",   input_per_1m: 2.00, reference: false },
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
    let intercepted_events: Vec<_> = events.iter().filter(|e| e.action == "intercepted").collect();
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
    let mut by_tool: Vec<(String, usize, i64)> =
        by_tool_map.into_iter().map(|(k, (c, s))| (k, c, s)).collect();
    by_tool.sort_by(|a, b| b.2.cmp(&a.2));

    let mut by_phase_map: std::collections::HashMap<String, (usize, i64)> =
        std::collections::HashMap::new();
    for e in &intercepted_events {
        let entry = by_phase_map.entry(e.phase.clone()).or_default();
        entry.0 += 1;
        entry.1 += e.saved_tokens;
    }
    let mut by_phase: Vec<(String, usize, i64)> =
        by_phase_map.into_iter().map(|(k, (c, s))| (k, c, s)).collect();
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
