use std::collections::HashMap;
use std::path::PathBuf;

use regex::Regex;
use rust_embed::Embed;
use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct MatchOutput {
    pub pattern: String,
    pub message: String,
}

#[derive(Debug, Deserialize, Clone)]
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
    #[serde(default)]
    pub match_output: Vec<MatchOutput>,
    pub truncate_lines_at: Option<usize>,
    #[serde(default)]
    #[allow(dead_code)]
    pub filter_stderr: bool,
}

#[derive(Debug, Deserialize)]
struct FilterFile {
    #[serde(default)]
    filters: HashMap<String, FilterDef>,
}

pub struct ActiveFilter {
    pub name: String,
    pub source: &'static str,
    pub filter: FilterDef,
}

#[derive(Embed)]
#[folder = "assets/filters"]
#[include = "*.toml"]
struct BundledFilters;

pub fn filters_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tokenix")
        .join("filters")
}

fn parse_filter_file_named(content: &str) -> Vec<(String, FilterDef)> {
    toml::from_str::<FilterFile>(content)
        .map(|f| f.filters.into_iter().collect())
        .unwrap_or_default()
}

pub fn load_user_filters() -> Vec<FilterDef> {
    load_user_filters_named()
        .into_iter()
        .map(|(_, f)| f)
        .collect()
}

pub fn load_user_filters_named() -> Vec<(String, FilterDef)> {
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
                    result.extend(parse_filter_file_named(&content));
                }
            }
        }
    }
    result
}

pub fn load_bundled_filters() -> Vec<FilterDef> {
    load_bundled_filters_named()
        .into_iter()
        .map(|(_, f)| f)
        .collect()
}

pub fn load_bundled_filters_named() -> Vec<(String, FilterDef)> {
    BundledFilters::iter()
        .filter_map(|name| {
            let file = BundledFilters::get(&name)?;
            let content = std::str::from_utf8(file.data.as_ref()).ok()?;
            Some(parse_filter_file_named(content))
        })
        .flatten()
        .collect()
}

pub fn load_active_filters() -> Vec<ActiveFilter> {
    let mut result: Vec<ActiveFilter> = load_user_filters_named()
        .into_iter()
        .map(|(name, filter)| ActiveFilter {
            name,
            source: "user",
            filter,
        })
        .collect();
    result.extend(
        load_bundled_filters_named()
            .into_iter()
            .map(|(name, filter)| ActiveFilter {
                name,
                source: "bundled",
                filter,
            }),
    );
    result
}

/// Returns user filters (priority) merged with bundled filters as fallback.
pub fn load_all_filters() -> Vec<FilterDef> {
    let mut all = load_user_filters();
    all.extend(load_bundled_filters());
    all
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
    // match_output short-circuits before any other transformation
    for mo in &f.match_output {
        if let Ok(re) = Regex::new(&mo.pattern) {
            if re.is_match(output) {
                return mo.message.clone();
            }
        }
    }

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

    let result = if let Some(max_len) = f.truncate_lines_at {
        lines
            .iter()
            .map(|l| {
                if l.len() > max_len {
                    &l[..max_len]
                } else {
                    l
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        lines.join("\n")
    };

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
match_output = [           # short-circuit: if output matches pattern, return message
  {{ pattern = "already installed", message = "ok (already installed)" }},
]
max_lines = 50             # truncate to N lines
head_lines = 30            # keep first N lines
tail_lines = 10            # keep last N lines
truncate_lines_at = 120    # truncate individual lines at N chars
on_empty = "command: ok"   # message when filter produces empty output
```

Rules:
- Use strip_lines_matching to drop boilerplate (progress, verbose info)
- Use keep_lines_matching only if output has a clear signal/noise separation
- Use match_output for commands that succeed silently or with a predictable summary line
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
