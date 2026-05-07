use std::collections::HashMap;
use std::path::PathBuf;

use regex::Regex;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct FilterDef {
    #[allow(dead_code)]
    pub description: Option<String>,
    pub match_command: String,
    #[serde(default)]
    pub strip_ansi: bool,
    #[serde(default)]
    pub strip_lines_matching: Vec<String>,
    #[serde(default)]
    pub keep_lines_matching: Vec<String>,
    pub max_lines: Option<usize>,
    pub head_lines: Option<usize>,
    pub tail_lines: Option<usize>,
    pub on_empty: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FilterFile {
    #[serde(default)]
    filters: HashMap<String, FilterDef>,
}

pub fn filters_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tokenix")
        .join("filters")
}

pub fn load_user_filters() -> Vec<FilterDef> {
    let dir = filters_dir();
    if !dir.exists() {
        return vec![];
    }
    let mut result = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("toml") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(f) = toml::from_str::<FilterFile>(&content) {
                        result.extend(f.filters.into_values());
                    }
                }
            }
        }
    }
    result
}

pub fn find_filter<'a>(cmd: &str, filters: &'a [FilterDef]) -> Option<&'a FilterDef> {
    for f in filters {
        if let Ok(re) = Regex::new(&f.match_command) {
            if re.is_match(cmd) {
                return Some(f);
            }
        }
    }
    None
}

pub fn apply_filter(output: &str, f: &FilterDef) -> String {
    let s = if f.strip_ansi {
        crate::compress::strip_ansi(output)
    } else {
        output.to_string()
    };

    let mut lines: Vec<&str> = s.lines().collect();

    if !f.strip_lines_matching.is_empty() {
        let patterns: Vec<Regex> = f
            .strip_lines_matching
            .iter()
            .filter_map(|p| Regex::new(p).ok())
            .collect();
        lines.retain(|l| !patterns.iter().any(|re| re.is_match(l)));
    }

    if !f.keep_lines_matching.is_empty() {
        let patterns: Vec<Regex> = f
            .keep_lines_matching
            .iter()
            .filter_map(|p| Regex::new(p).ok())
            .collect();
        lines.retain(|l| patterns.iter().any(|re| re.is_match(l)));
    }

    let lines = apply_sizing(lines, f);
    let result = lines.join("\n");

    if result.trim().is_empty() {
        if let Some(msg) = &f.on_empty {
            return msg.clone();
        }
    }
    result
}

fn apply_sizing<'a>(mut lines: Vec<&'a str>, f: &FilterDef) -> Vec<&'a str> {
    if let Some(head) = f.head_lines {
        lines.truncate(head);
    } else if let Some(tail) = f.tail_lines {
        let len = lines.len();
        if len > tail {
            lines = lines[len - tail..].to_vec();
        }
    } else if let Some(max) = f.max_lines {
        lines.truncate(max);
    }
    lines
}

/// Generate the TOML prompt to send to an AI CLI for filter creation.
pub fn build_filter_prompt(command: &str, sample_output: &str) -> String {
    format!(
        r#"Generate an RTK-format TOML filter for the command `{command}`.

TOML filter schema (all fields optional except match_command):
```
[filters.<slug>]
description = "human-readable purpose"
match_command = "^regex_to_match_full_command_line"
strip_ansi = true          # remove ANSI color codes
strip_lines_matching = ["^pattern1", "^pattern2"]  # drop noisy lines
keep_lines_matching = ["error", "warning"]          # keep only signal lines
max_lines = 50             # truncate to N lines
head_lines = 30            # keep first N lines
tail_lines = 10            # keep last N lines
on_empty = "command: ok"   # message when filter produces empty output
```

Rules:
- Use strip_lines_matching to drop boilerplate (progress, verbose info)
- Use keep_lines_matching only if output has a clear signal/noise separation
- Set on_empty when the command normally succeeds silently
- match_command must be a valid Rust regex matching `{command}` or its typical invocations
- Return ONLY valid TOML, no markdown code fences, no explanations

Sample output from `{command} --help` (or similar):
---
{sample_output}
---

TOML filter:"#
    )
}
