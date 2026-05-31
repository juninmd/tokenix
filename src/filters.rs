use std::collections::HashMap;
use std::path::PathBuf;

use regex::Regex;
use rust_embed::Embed;
use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct MatchOutput {
    pub pattern: String,
    pub message: String,
    /// Guard: when set, the short-circuit to `message` is skipped if the output
    /// also matches this regex. Prevents masking errors/warnings that appear
    /// alongside a success marker (e.g. "total size is" present, but so is "error").
    #[serde(default)]
    pub unless: Option<String>,
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
// Rebuild trigger for new filters
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

pub fn load_local_filters_named() -> Vec<(String, FilterDef)> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = crate::store::find_project_root(&cwd);
    let dir = root.join(".tokenix").join("filters");
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

pub fn load_local_filters() -> Vec<FilterDef> {
    load_local_filters_named()
        .into_iter()
        .map(|(_, f)| f)
        .collect()
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
    let mut result: Vec<ActiveFilter> = load_local_filters_named()
        .into_iter()
        .map(|(name, filter)| ActiveFilter {
            name,
            source: "local",
            filter,
        })
        .collect();
    result.extend(
        load_user_filters_named()
            .into_iter()
            .map(|(name, filter)| ActiveFilter {
                name,
                source: "user",
                filter,
            }),
    );
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

/// Returns local filters (highest priority), then user filters, then bundled filters as fallback.
pub fn load_all_filters() -> Vec<FilterDef> {
    let mut all = load_local_filters();
    all.extend(load_user_filters());
    all.extend(load_bundled_filters());
    all
}

pub fn find_filter<'a>(cmd: &str, filters: &'a [FilterDef]) -> Option<&'a FilterDef> {
    let candidates = derive_command_candidates(cmd);
    for f in filters {
        if let Ok(re) = Regex::new(&f.match_command) {
            for candidate in &candidates {
                if re.is_match(candidate) {
                    return Some(f);
                }
            }
        }
    }
    None
}

pub fn tokenize_command(command: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaping = false;

    for c in command.trim().chars() {
        if escaping {
            current.push(c);
            escaping = false;
            continue;
        }

        if c == '\\' {
            escaping = true;
            continue;
        }

        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else {
                current.push(c);
            }
            continue;
        }

        if c == '\'' || c == '"' {
            quote = Some(c);
            continue;
        }

        if c.is_whitespace() {
            if !current.is_empty() {
                tokens.push(current);
                current = String::new();
            }
            continue;
        }

        current.push(c);
    }

    if escaping {
        current.push('\\');
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}

pub fn unwrap_shell_runner(cmd: &str) -> Option<String> {
    let argv = tokenize_command(cmd);
    if argv.is_empty() {
        return None;
    }

    let first = &argv[0];
    let first_path = std::path::Path::new(first);
    let launcher_name = first_path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(first)
        .to_lowercase();
    let launcher_name_no_ext = launcher_name.strip_suffix(".exe").unwrap_or(&launcher_name);

    let is_shell = matches!(
        launcher_name_no_ext,
        "bash"
            | "sh"
            | "zsh"
            | "fish"
            | "dash"
            | "ksh"
            | "mksh"
            | "ash"
            | "csh"
            | "tcsh"
            | "cmd"
            | "powershell"
            | "pwsh"
    );

    if !is_shell {
        return None;
    }

    for i in 1..(argv.len().saturating_sub(1)) {
        let arg = &argv[i];
        let is_command_flag = if launcher_name_no_ext == "cmd" {
            arg.eq_ignore_ascii_case("/c") || arg.eq_ignore_ascii_case("-c")
        } else if launcher_name_no_ext == "powershell" || launcher_name_no_ext == "pwsh" {
            arg.eq_ignore_ascii_case("-c")
                || arg.eq_ignore_ascii_case("-command")
                || arg.eq_ignore_ascii_case("--command")
        } else {
            arg.starts_with('-') && arg.contains('c')
        };

        if is_command_flag {
            return Some(argv[i + 1].trim().to_string());
        }
    }

    None
}

fn is_env_assignment(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    if !bytes[0].is_ascii_alphabetic() && bytes[0] != b'_' {
        return false;
    }
    let mut i = 1;
    while i < bytes.len() {
        if bytes[i] == b'=' {
            return i > 0;
        }
        if !bytes[i].is_ascii_alphanumeric() && bytes[i] != b'_' {
            return false;
        }
        i += 1;
    }
    false
}

fn strip_leading_env_assignments(argv: &[String]) -> Vec<String> {
    let mut index = 0;
    while index < argv.len() && is_env_assignment(&argv[index]) {
        index += 1;
    }

    if index < argv.len() {
        let cmd_path = std::path::Path::new(&argv[index]);
        let cmd_name = cmd_path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or(&argv[index]);
        if cmd_name == "env" {
            index += 1;
            while index < argv.len() {
                let arg = &argv[index];
                if arg == "--" {
                    index += 1;
                    break;
                }
                if is_env_assignment(arg) {
                    index += 1;
                    continue;
                }
                if arg == "-i" || arg == "-0" || arg == "--ignore-environment" || arg == "--debug" {
                    index += 1;
                    continue;
                }
                if arg == "-u"
                    || arg == "--unset"
                    || arg == "-C"
                    || arg == "--chdir"
                    || arg == "-S"
                    || arg == "--split-string"
                {
                    index += 2;
                    continue;
                }
                if arg.starts_with("--unset=")
                    || arg.starts_with("--chdir=")
                    || arg.starts_with("--split-string=")
                {
                    index += 1;
                    continue;
                }
                break;
            }
        }
    }

    argv[index..].to_vec()
}

fn strip_cd_and_operators(mut argv: &[String]) -> &[String] {
    for _ in 0..8 {
        if argv.is_empty() {
            break;
        }
        let first = &argv[0];
        if first == "cd" || first == "pushd" {
            if argv.len() >= 2 && (argv[1] == "&&" || argv[1] == ";") {
                argv = &argv[2..];
                continue;
            }
            if argv.len() >= 3 && (argv[2] == "&&" || argv[2] == ";") {
                argv = &argv[3..];
                continue;
            }
        }
        break;
    }
    argv
}

pub fn get_effective_command(cmd: &str) -> String {
    let mut current = cmd.trim().to_string();

    for _ in 0..16 {
        let unwrapped = unwrap_shell_runner(&current);
        if let Some(inner) = unwrapped {
            current = inner;
            continue;
        }

        let tokens = tokenize_command(&current);
        if tokens.is_empty() {
            break;
        }

        let stripped_env = strip_leading_env_assignments(&tokens);
        let stripped_cd = strip_cd_and_operators(&stripped_env);

        if stripped_cd.len() == tokens.len() {
            break;
        }

        current = stripped_cd.join(" ");
    }

    current
}

pub fn derive_command_candidates(cmd: &str) -> Vec<String> {
    let mut candidates = Vec::new();

    let original = cmd.trim().to_string();
    if !original.is_empty() {
        candidates.push(original);
    }

    if let Some(shell_body) = unwrap_shell_runner(cmd) {
        if !shell_body.is_empty() && shell_body != cmd {
            candidates.push(shell_body.clone());
        }
    }

    let effective = get_effective_command(cmd);
    if !effective.is_empty() && !candidates.contains(&effective) {
        candidates.push(effective);
    }

    candidates
}

pub fn apply_filter(output: &str, f: &FilterDef) -> String {
    // match_output short-circuits before any other transformation
    for mo in &f.match_output {
        if let Ok(re) = Regex::new(&mo.pattern) {
            if re.is_match(output) {
                // `unless` guard: do not short-circuit when the output also matches
                // this pattern, so errors/warnings are never masked as success.
                if let Some(unless) = &mo.unless {
                    if Regex::new(unless)
                        .map(|u| u.is_match(output))
                        .unwrap_or(false)
                    {
                        continue;
                    }
                }
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
            .map(|l| truncate_at_char_boundary(l, max_len))
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

/// Truncate `s` to at most `max_bytes`, backing off to the nearest char
/// boundary so we never slice through a multi-byte UTF-8 sequence (which would
/// panic). Returns a borrowed slice — no allocation.
fn truncate_at_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
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
  # optional `unless`: skip the short-circuit if output also matches it (avoids masking errors)
  {{ pattern = "Build complete!", message = "ok (build complete)", unless = "warning:|error:" }},
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_local_filters() {
        let temp_dir = std::env::current_dir()
            .unwrap()
            .join(".tokenix")
            .join("filters");
        std::fs::create_dir_all(&temp_dir).unwrap();
        let toml_path = temp_dir.join("test_local_cmd.toml");
        std::fs::write(
            &toml_path,
            r#"
[filters.test_local_cmd]
description = "test local"
match_command = "^test_local_cmd$"
on_empty = "empty filter output"
"#,
        )
        .unwrap();

        let local_filters = load_local_filters();
        assert!(!local_filters.is_empty());
        let found = find_filter("test_local_cmd", &local_filters);
        assert!(found.is_some());
        let filter = found.unwrap();
        assert_eq!(filter.on_empty.as_deref(), Some("empty filter output"));

        // Clean up
        let _ = std::fs::remove_file(&toml_path);
        let _ = std::fs::remove_dir_all(
            std::env::current_dir()
                .unwrap()
                .join(".tokenix")
                .join("filters"),
        );
    }

    #[test]
    fn test_tokenize_command() {
        assert_eq!(tokenize_command("cargo test"), vec!["cargo", "test"]);
        assert_eq!(
            tokenize_command("echo \"hello world\""),
            vec!["echo", "hello world"]
        );
        assert_eq!(
            tokenize_command("env CI=true cargo test"),
            vec!["env", "CI=true", "cargo", "test"]
        );
    }

    #[test]
    fn test_unwrap_shell_runner() {
        assert_eq!(
            unwrap_shell_runner("bash -c 'cargo test'"),
            Some("cargo test".to_string())
        );
        assert_eq!(
            unwrap_shell_runner("powershell -Command \"cargo test\""),
            Some("cargo test".to_string())
        );
        assert_eq!(
            unwrap_shell_runner("cmd.exe /c \"cargo test\""),
            Some("cargo test".to_string())
        );
        assert_eq!(unwrap_shell_runner("cargo test"), None);
    }

    #[test]
    fn test_get_effective_command() {
        assert_eq!(
            get_effective_command("cd /app && CI=true cargo test"),
            "cargo test"
        );
        assert_eq!(
            get_effective_command("bash -c 'cd /app && CI=true env cargo test'"),
            "cargo test"
        );
        assert_eq!(
            get_effective_command("env CI=true cargo test"),
            "cargo test"
        );
    }

    #[test]
    fn test_derive_command_candidates() {
        let cmd = "bash -c 'cd /app && cargo test'";
        let candidates = derive_command_candidates(cmd);
        assert!(candidates.contains(&"bash -c 'cd /app && cargo test'".to_string()));
        assert!(candidates.contains(&"cd /app && cargo test".to_string()));
        assert!(candidates.contains(&"cargo test".to_string()));
    }

    #[test]
    fn truncate_at_char_boundary_handles_multibyte() {
        // ASCII: exact byte cut
        assert_eq!(truncate_at_char_boundary("hello world", 5), "hello");
        // Shorter than limit: unchanged
        assert_eq!(truncate_at_char_boundary("hi", 10), "hi");
        // Multibyte: 'é' is 2 bytes — cutting at byte 4 lands mid-char, must back off
        let s = "café latte"; // 'é' occupies bytes 3..5
        let out = truncate_at_char_boundary(s, 4);
        assert!(s.starts_with(out));
        assert_eq!(out, "caf"); // backed off to char boundary, no panic
    }

    #[test]
    fn apply_filter_truncate_lines_at_no_panic_on_utf8() {
        let f = FilterDef {
            description: None,
            match_command: ".*".to_string(),
            strip_ansi: false,
            strip_lines_matching: vec![],
            keep_lines_matching: vec![],
            max_lines: None,
            head_lines: None,
            tail_lines: None,
            on_empty: None,
            match_output: vec![],
            truncate_lines_at: Some(4),
            filter_stderr: false,
        };
        // Would panic with naive &l[..4] because 'é'/'ç' straddle the boundary.
        let out = apply_filter("café\nação\n", &f);
        assert_eq!(out, "caf\naç");
    }

    #[test]
    fn apply_filter_match_output_unless_guards_errors() {
        let f = FilterDef {
            description: None,
            match_command: ".*".to_string(),
            strip_ansi: false,
            strip_lines_matching: vec![],
            keep_lines_matching: vec![],
            max_lines: None,
            head_lines: None,
            tail_lines: None,
            on_empty: None,
            match_output: vec![MatchOutput {
                pattern: "total size is".to_string(),
                message: "ok (synced)".to_string(),
                unless: Some("error|failed".to_string()),
            }],
            truncate_lines_at: None,
            filter_stderr: false,
        };
        // Pattern present, no error → short-circuit to message
        assert_eq!(apply_filter("total size is 100\n", &f), "ok (synced)");
        // Pattern present AND error present → unless guard blocks short-circuit
        let out = apply_filter("rsync error\ntotal size is 100\n", &f);
        assert!(out.contains("error"), "error must not be masked: {out:?}");
    }

    // --- Golden self-test: run every bundled filter's embedded [[tests.<name>]]
    // cases through the real apply_filter pipeline. Homologation guard so the
    // ~150 declared input→expected pairs can never silently drift.
    #[derive(Debug, Deserialize)]
    struct GoldenCase {
        #[serde(default)]
        name: Option<String>,
        input: String,
        expected: String,
    }

    #[derive(Debug, Deserialize)]
    struct FilterTestFile {
        #[serde(default)]
        filters: HashMap<String, FilterDef>,
        #[serde(default)]
        tests: HashMap<String, Vec<GoldenCase>>,
    }

    #[test]
    fn bundled_filters_pass_embedded_golden_tests() {
        let mut total = 0usize;
        let mut files_with_tests = 0usize;
        let mut failures: Vec<String> = Vec::new();

        for asset in BundledFilters::iter() {
            let file = BundledFilters::get(&asset).expect("bundled asset readable");
            let content = std::str::from_utf8(file.data.as_ref()).expect("filter is utf8");
            let parsed: FilterTestFile = match toml::from_str(content) {
                Ok(p) => p,
                Err(e) => {
                    failures.push(format!("{asset}: TOML parse error: {e}"));
                    continue;
                }
            };
            if !parsed.tests.is_empty() {
                files_with_tests += 1;
            }
            for (fname, cases) in &parsed.tests {
                let Some(fdef) = parsed.filters.get(fname) else {
                    failures.push(format!(
                        "{asset}: [[tests.{fname}]] references undefined [filters.{fname}]"
                    ));
                    continue;
                };
                for (i, case) in cases.iter().enumerate() {
                    total += 1;
                    let got = apply_filter(&case.input, fdef);
                    if got.trim_end() != case.expected.trim_end() {
                        let label = case.name.clone().unwrap_or_else(|| format!("#{i}"));
                        failures.push(format!(
                            "{asset} [{fname} / {label}]\n  expected: {:?}\n  got:      {:?}",
                            case.expected, got
                        ));
                    }
                }
            }
        }

        eprintln!(
            "golden: ran {total} embedded cases across {files_with_tests} bundled filter files"
        );
        assert!(
            failures.is_empty(),
            "{} bundled golden filter case(s) failed:\n\n{}",
            failures.len(),
            failures.join("\n\n")
        );
    }
}
