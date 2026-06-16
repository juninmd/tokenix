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
    /// When the filter reduces *non-empty* command output to nothing — usually an
    /// unexpected output shape the keep/extract rules don't recognize (e.g.
    /// `git log --oneline` against a filter tuned for the full log format) — emit a
    /// bounded, ANSI-stripped view of the real output instead of `on_empty`.
    /// Opt-in so summary filters (cargo test all-pass → "ok") keep their behavior;
    /// only format-specific filters that would otherwise report a *false* "nothing
    /// here" (git-log, git-diff) set it.
    #[serde(default)]
    pub passthrough_when_emptied: bool,
    #[serde(default)]
    pub match_output: Vec<MatchOutput>,
    pub truncate_lines_at: Option<usize>,
    #[serde(default)]
    #[allow(dead_code)]
    pub filter_stderr: bool,

    /// Regex replacement rules: each entry is [pattern, replacement].
    /// Applied after line filtering, before sizing. Enables custom transformations
    /// like shortening paths, normalizing timestamps, etc.
    #[serde(default)]
    pub replace_patterns: Vec<[String; 2]>,

    /// Extract only content between start/end markers (inclusive).
    /// Useful for pulling out specific sections like test failures, error blocks, etc.
    #[serde(default)]
    pub extract_sections: Vec<ExtractSection>,

    /// Semantic filter: keep only lines semantically relevant to a query.
    /// Uses embeddings to score relevance. Requires daemon or in-process embed.
    #[serde(default)]
    pub semantic_filter: Option<SemanticFilterDef>,

    /// Deduplicate similar blocks (not just exact lines).
    /// Groups consecutive blocks by structural similarity.
    #[serde(default)]
    pub deduplicate_blocks: Option<DeduplicateBlocksDef>,

    /// Intelligent JSON summarization beyond simple compaction.
    /// Extracts key fields, summarizes arrays, preserves structure.
    #[serde(default)]
    pub summarize_json: Option<SummarizeJsonDef>,

    /// Hard token budget: truncate intelligently to stay under token limit.
    /// Prioritizes head/tail/errors/semantic relevance.
    pub token_budget: Option<usize>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ExtractSection {
    pub start_pattern: String,
    pub end_pattern: String,
    #[serde(default)]
    pub include_markers: bool,
    #[serde(default)]
    pub max_matches: Option<usize>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SemanticFilterDef {
    /// Query to score relevance against (e.g., "error", "test failure", "build output")
    pub query: String,
    /// Minimum cosine similarity to keep (0.0-1.0)
    #[serde(default = "default_semantic_threshold")]
    pub threshold: f32,
    /// Always keep lines matching these patterns regardless of score
    #[serde(default)]
    pub always_keep: Vec<String>,
    /// Model to use (defaults to index model)
    pub model: Option<String>,
}

fn default_semantic_threshold() -> f32 {
    0.3
}

#[derive(Debug, Deserialize, Clone)]
pub struct DeduplicateBlocksDef {
    /// Minimum lines per block to consider for deduplication
    #[serde(default = "default_min_block_lines")]
    pub min_block_lines: usize,
    /// Similarity threshold for block comparison (0.0-1.0)
    #[serde(default = "default_block_similarity")]
    pub similarity: f32,
    /// Regex to identify block boundaries (default: blank line)
    #[serde(default)]
    pub block_delimiter: Option<String>,
}

fn default_min_block_lines() -> usize {
    3
}

fn default_block_similarity() -> f32 {
    0.8
}

#[derive(Debug, Deserialize, Clone)]
pub struct SummarizeJsonDef {
    /// Max array elements to show before summarizing
    #[serde(default = "default_max_array_items")]
    pub max_array_items: usize,
    /// Max object depth to traverse
    #[serde(default = "default_max_depth")]
    pub max_depth: usize,
    /// Fields to always include (dot notation for nested)
    #[serde(default)]
    pub always_include: Vec<String>,
    /// Fields to exclude
    #[serde(default)]
    pub exclude: Vec<String>,
}

fn default_max_array_items() -> usize {
    10
}

fn default_max_depth() -> usize {
    3
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

/// First embedded `[[tests.<name>]].input` for each bundled filter, keyed by
/// filter name. The TUI uses these as representative sample output to preview how
/// a filter transforms input (input → apply_filter → output).
pub fn sample_inputs() -> HashMap<String, String> {
    #[derive(Deserialize)]
    struct SampleCase {
        input: String,
    }
    #[derive(Deserialize)]
    struct TestsOnly {
        #[serde(default)]
        tests: HashMap<String, Vec<SampleCase>>,
    }
    let mut map = HashMap::new();
    for name in BundledFilters::iter() {
        let Some(file) = BundledFilters::get(&name) else {
            continue;
        };
        let Ok(content) = std::str::from_utf8(file.data.as_ref()) else {
            continue;
        };
        if let Ok(parsed) = toml::from_str::<TestsOnly>(content) {
            for (fname, cases) in parsed.tests {
                if let Some(first) = cases.into_iter().next() {
                    map.entry(fname).or_insert(first.input);
                }
            }
        }
    }
    map
}

/// Bundled-filter inventory for `tokenix doctor`: total embedded golden test
/// cases across every bundled filter file.
pub fn bundled_test_case_count() -> usize {
    #[derive(Deserialize)]
    struct TestsOnly {
        #[serde(default)]
        tests: HashMap<String, Vec<toml::Value>>,
    }
    let mut count = 0;
    for name in BundledFilters::iter() {
        let Some(file) = BundledFilters::get(&name) else {
            continue;
        };
        let Ok(content) = std::str::from_utf8(file.data.as_ref()) else {
            continue;
        };
        if let Ok(parsed) = toml::from_str::<TestsOnly>(content) {
            count += parsed.tests.values().map(|v| v.len()).sum::<usize>();
        }
    }
    count
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

/// Config problems in a filter's semantic_filter section (unknown model,
/// out-of-range threshold). Used by `tokenix doctor`; empty = healthy.
pub fn semantic_filter_issues(f: &FilterDef) -> Vec<String> {
    let mut issues = Vec::new();
    if let Some(sem) = &f.semantic_filter {
        if let Some(model) = &sem.model {
            if !crate::embed::is_known_model(model) {
                issues.push(format!(
                    "semantic_filter.model '{}' unknown — falls back to '{}'",
                    model,
                    crate::embed::DEFAULT_MODEL_ID
                ));
            }
        }
        if !(0.0..=1.0).contains(&sem.threshold) {
            issues.push(format!(
                "semantic_filter.threshold {} outside 0.0-1.0",
                sem.threshold
            ));
        }
    }
    issues
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

/// Strip known Unix command-timing / resource-limit wrappers that prefix the
/// real command without altering its behaviour for filter-matching purposes:
///
/// - `timeout [OPTS] DURATION CMD`  (GNU coreutils)
/// - `time CMD`
/// - `nice [-n N] CMD`
/// - `ionice [-c C] [-n N] [-t] CMD`
///
/// Each wrapper is peeled in a loop so stacked prefixes like
/// `timeout 30 nice -n 10 pnpm run test` resolve to `pnpm run test`.
fn strip_leading_wrappers(argv: &[String]) -> Vec<String> {
    let mut index = 0;

    loop {
        if index >= argv.len() {
            break;
        }
        let name_raw = std::path::Path::new(&argv[index])
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or(&argv[index])
            .to_lowercase();
        let name = name_raw.strip_suffix(".exe").unwrap_or(&name_raw);

        match name {
            // `time CMD` — single token prefix
            "time" => {
                index += 1;
            }
            // `nice [-n N | --adjustment[=]N] CMD`
            "nice" => {
                index += 1;
                if index < argv.len() {
                    let a = &argv[index];
                    if a == "-n" || a == "--adjustment" {
                        index += 2;
                    } else if a.starts_with("--adjustment=") {
                        index += 1;
                    }
                }
            }
            // `ionice [-c C] [-n N] [-t] CMD`
            "ionice" => {
                index += 1;
                while index < argv.len() {
                    let a = &argv[index];
                    if (a == "-c" || a == "-n") && index + 1 < argv.len() {
                        index += 2;
                    } else if a == "-t" {
                        index += 1;
                    } else {
                        break;
                    }
                }
            }
            // `timeout [OPTS] DURATION CMD`
            // Options: --foreground, --preserve-status, --verbose, -k DUR, -s SIG
            "timeout" => {
                index += 1;
                let mut found_duration = false;
                while index < argv.len() {
                    let a = &argv[index];
                    if matches!(
                        a.as_str(),
                        "--foreground" | "--preserve-status" | "--verbose"
                    ) {
                        index += 1;
                        continue;
                    }
                    if (a == "-k" || a == "--kill-after" || a == "-s" || a == "--signal")
                        && index + 1 < argv.len()
                    {
                        index += 2;
                        continue;
                    }
                    if a.starts_with("--kill-after=") || a.starts_with("--signal=") {
                        index += 1;
                        continue;
                    }
                    if a.starts_with('-') {
                        index += 1;
                        continue;
                    }
                    // First non-option argument is the DURATION — skip it.
                    index += 1;
                    found_duration = true;
                    break;
                }
                if !found_duration {
                    // Malformed `timeout` invocation — stop peeling.
                    break;
                }
            }
            _ => break,
        }
    }

    argv[index..].to_vec()
}

/// Drop tool-global options that sit *between* a subcommand tool and its
/// subcommand, so a filter anchored on `^git\s+add` still matches
/// `git -C /repo -c user.name=x add .`. Without this, idiomatic invocations
/// like `git -C dir`, `kubectl -n ns`, `docker -H host` or `cargo +nightly`
/// bypass every subcommand filter and ship raw output.
///
/// Returns the tool name followed by the first non-option token onward, e.g.
/// `["git", "-C", "dir", "add", "."]` -> `["git", "add", "."]`. Tools not in
/// the recognized set are returned unchanged.
fn strip_subcommand_global_opts(argv: &[String]) -> Vec<String> {
    if argv.is_empty() {
        return Vec::new();
    }
    let tool = std::path::Path::new(&argv[0])
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(&argv[0])
        .to_lowercase();
    let tool = tool.strip_suffix(".exe").unwrap_or(&tool);

    // (flags taking a following value, boolean flags)
    let (valued, boolean): (&[&str], &[&str]) = match tool {
        "git" => (
            &[
                "-C",
                "-c",
                "--git-dir",
                "--work-tree",
                "--exec-path",
                "--namespace",
                "--super-prefix",
            ],
            &[
                "-p",
                "-P",
                "--paginate",
                "--no-pager",
                "--bare",
                "--no-replace-objects",
                "--literal-pathspecs",
                "--glob-pathspecs",
                "--noglob-pathspecs",
                "--icase-pathspecs",
                "--no-optional-locks",
            ],
        ),
        "kubectl" => (
            &[
                "-n",
                "--namespace",
                "--context",
                "--kubeconfig",
                "-s",
                "--server",
                "--token",
                "--as",
                "--as-group",
                "--cluster",
                "--user",
                "--cache-dir",
                "--request-timeout",
                "--client-certificate",
                "--client-key",
                "--certificate-authority",
                "--tls-server-name",
            ],
            &["--insecure-skip-tls-verify"],
        ),
        "docker" => (
            &[
                "-H",
                "--host",
                "--context",
                "--config",
                "-l",
                "--log-level",
                "--tlscacert",
                "--tlscert",
                "--tlskey",
            ],
            &["-D", "--debug", "--tls", "--tlsverify"],
        ),
        "cargo" => (&[], &[]),
        _ => return argv.to_vec(),
    };

    let mut i = 1;
    while i < argv.len() {
        let a = &argv[i];
        // `cargo +nightly test`
        if tool == "cargo" && a.starts_with('+') {
            i += 1;
            continue;
        }
        if a.starts_with("--") {
            if a.contains('=') {
                i += 1; // --opt=value
                continue;
            }
            if valued.contains(&a.as_str()) {
                i += 2; // --opt value
                continue;
            }
            if boolean.contains(&a.as_str()) {
                i += 1;
                continue;
            }
            break;
        } else if a.len() >= 2 && a.starts_with('-') {
            if valued.contains(&a.as_str()) {
                i += 2; // -C dir
                continue;
            }
            if boolean.contains(&a.as_str()) {
                i += 1;
                continue;
            }
            break;
        } else {
            break; // subcommand reached
        }
    }

    if i == 1 {
        return argv.to_vec();
    }
    let mut out = Vec::with_capacity(1 + argv.len() - i);
    out.push(argv[0].clone());
    out.extend_from_slice(&argv[i..]);
    out
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
        let stripped_wrappers = strip_leading_wrappers(&stripped_env);
        let stripped_cd = strip_cd_and_operators(&stripped_wrappers);
        let stripped_opts = strip_subcommand_global_opts(stripped_cd);

        if stripped_opts.len() == tokens.len() {
            break;
        }

        current = stripped_opts.join(" ");
    }

    current
}

/// Split a shell command into segments on the operators `&&`, `||`, `;` and the
/// pipe `|`, quote- and escape-aware. Operators are recognized regardless of
/// surrounding whitespace, so `a;b` and `a ; b` segment identically.
/// Quoted operators (e.g. `echo "a;b"`) are left intact.
pub fn split_on_operators(cmd: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaping = false;

    let chars: Vec<char> = cmd.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];

        if escaping {
            current.push(c);
            escaping = false;
            i += 1;
            continue;
        }
        if c == '\\' {
            current.push(c);
            escaping = true;
            i += 1;
            continue;
        }
        if let Some(q) = quote {
            current.push(c);
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if c == '\'' || c == '"' {
            quote = Some(c);
            current.push(c);
            i += 1;
            continue;
        }

        let next = chars.get(i + 1).copied();
        // Two-char operators `&&` / `||` first, so the trailing `|` of `||`
        // is not mistaken for a pipe split.
        if (c == '&' && next == Some('&')) || (c == '|' && next == Some('|')) {
            push_segment(&mut segments, &mut current);
            i += 2;
            continue;
        }
        if c == ';' || c == '|' {
            push_segment(&mut segments, &mut current);
            i += 1;
            continue;
        }

        current.push(c);
        i += 1;
    }
    push_segment(&mut segments, &mut current);
    segments
}

fn push_segment(segments: &mut Vec<String>, current: &mut String) {
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        segments.push(trimmed.to_string());
    }
    current.clear();
}

fn push_unique(candidates: &mut Vec<String>, candidate: &str) {
    let trimmed = candidate.trim();
    if !trimmed.is_empty() && !candidates.iter().any(|c| c == trimmed) {
        candidates.push(trimmed.to_string());
    }
}

pub fn derive_command_candidates(cmd: &str) -> Vec<String> {
    let mut candidates = Vec::new();

    push_unique(&mut candidates, cmd);

    let shell_body = unwrap_shell_runner(cmd);
    if let Some(body) = &shell_body {
        push_unique(&mut candidates, body);
    }

    push_unique(&mut candidates, &get_effective_command(cmd));

    // Operator-aware segmentation: split compound commands and add
    // each segment plus its effective form, so a filter anchored on its base
    // command matches regardless of position or spacing — e.g. `cd x;gitleaks`,
    // `npm i && gitleaks`, or `producer | gitleaks`.
    let mut bases = vec![cmd.to_string()];
    if let Some(body) = shell_body {
        bases.push(body);
    }
    for base in &bases {
        for segment in split_on_operators(base) {
            let effective = get_effective_command(&segment);
            push_unique(&mut candidates, &segment);
            push_unique(&mut candidates, &effective);
        }
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

    let mut lines: Vec<String> = s.lines().map(|l| l.to_string()).collect();

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

    // NEW: extract_sections - extract content between markers
    if !f.extract_sections.is_empty() {
        lines = apply_extract_sections(lines, &f.extract_sections);
    }

    // NEW: replace_patterns - regex replacements
    if !f.replace_patterns.is_empty() {
        lines = apply_replace_patterns(lines, &f.replace_patterns);
    }

    // NEW: deduplicate_blocks - structural deduplication
    if let Some(dedup) = &f.deduplicate_blocks {
        lines = apply_deduplicate_blocks(lines, dedup);
    }

    // NEW: semantic_filter - embedding-based relevance filtering
    if let Some(semantic) = &f.semantic_filter {
        lines = apply_semantic_filter(lines, semantic);
    }

    // NEW: summarize_json - intelligent JSON summarization
    if let Some(summarize) = &f.summarize_json {
        lines = apply_summarize_json(lines, summarize);
    }

    let lines = apply_sizing(lines, f);

    // NEW: token_budget - hard token limit with smart truncation
    let mut result = if let Some(max_len) = f.truncate_lines_at {
        lines
            .iter()
            .map(|l| truncate_at_char_boundary(l, max_len))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        lines.join("\n")
    };

    if let Some(budget) = f.token_budget {
        result = apply_token_budget(&result, budget);
    }

    if result.trim().is_empty() {
        if f.passthrough_when_emptied && !output.trim().is_empty() {
            // The command DID produce output, but every line was filtered away.
            // Reporting `on_empty` ("no commits"/"no changes") here would be a
            // false negative, so fall back to a bounded view of the real output.
            let cap = f.max_lines.unwrap_or(40);
            let fallback: Vec<String> = s
                .lines()
                .take(cap)
                .map(|l| match f.truncate_lines_at {
                    Some(n) => truncate_at_char_boundary(l, n).to_string(),
                    None => l.to_string(),
                })
                .collect();
            let fb_text = fallback.join("\n");
            return match f.token_budget {
                Some(budget) => apply_token_budget(&fb_text, budget),
                None => fb_text,
            };
        }
        if let Some(msg) = &f.on_empty {
            return msg.clone();
        }
    }
    result
}

fn apply_extract_sections(lines: Vec<String>, sections: &[ExtractSection]) -> Vec<String> {
    let mut result = Vec::new();
    let content = lines.join("\n");

    for section in sections {
        let start_re = match Regex::new(&section.start_pattern) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let end_re = match Regex::new(&section.end_pattern) {
            Ok(r) => r,
            Err(_) => continue,
        };

        let mut matches = 0;
        let max_matches = section.max_matches.unwrap_or(usize::MAX);

        let mut in_section = false;
        let mut section_lines = Vec::new();

        for line in content.lines() {
            let start_match = start_re.is_match(line);
            let end_match = end_re.is_match(line);

            if start_match && !in_section {
                in_section = true;
                if section.include_markers {
                    section_lines.push(line.to_string());
                }
                continue;
            }

            if in_section {
                if section.include_markers || !end_match {
                    section_lines.push(line.to_string());
                }
                if end_match {
                    result.append(&mut section_lines);
                    matches += 1;
                    in_section = false;
                    if matches >= max_matches {
                        break;
                    }
                }
            }
        }

        // Handle unclosed section
        if in_section && section.include_markers {
            result.extend(section_lines);
        }
    }

    if result.is_empty() {
        lines
    } else {
        result
    }
}

fn apply_replace_patterns(lines: Vec<String>, patterns: &[[String; 2]]) -> Vec<String> {
    lines
        .into_iter()
        .map(|mut line| {
            for [pattern, replacement] in patterns {
                if let Ok(re) = Regex::new(pattern) {
                    line = re.replace_all(&line, replacement.as_str()).to_string();
                }
            }
            line
        })
        .collect()
}

fn apply_deduplicate_blocks(lines: Vec<String>, dedup: &DeduplicateBlocksDef) -> Vec<String> {
    let delimiter = dedup.block_delimiter.as_deref().unwrap_or(r"^\s*$");
    let delim_re = match Regex::new(delimiter) {
        Ok(r) => r,
        Err(_) => return lines,
    };

    let mut blocks: Vec<Vec<String>> = Vec::new();
    let mut current_block = Vec::new();

    for line in &lines {
        if delim_re.is_match(line) && !current_block.is_empty() {
            if current_block.len() >= dedup.min_block_lines {
                blocks.push(current_block);
            }
            current_block = Vec::new();
        } else {
            current_block.push(line.clone());
        }
    }
    if !current_block.is_empty() && current_block.len() >= dedup.min_block_lines {
        blocks.push(current_block);
    }

    if blocks.len() < 2 {
        return lines;
    }

    let mut result = Vec::new();
    let mut i = 0;
    while i < blocks.len() {
        let block = &blocks[i];
        result.extend(block.iter().cloned());

        // Check next blocks for similarity
        let mut j = i + 1;
        let mut similar_count = 0;
        while j < blocks.len() {
            if blocks_similar(block, &blocks[j], dedup.similarity) {
                similar_count += 1;
                j += 1;
            } else {
                break;
            }
        }

        if similar_count > 0 {
            result.push(format!(
                "[... {} similar block(s) omitted ...]",
                similar_count
            ));
            i = j;
        } else {
            i += 1;
        }
    }

    result
}

fn blocks_similar(a: &[String], b: &[String], threshold: f32) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let matches = a.iter().zip(b.iter()).filter(|(x, y)| x == y).count();
    (matches as f32 / a.len() as f32) >= threshold
}

fn apply_semantic_filter(lines: Vec<String>, semantic: &SemanticFilterDef) -> Vec<String> {
    // Try to use real embeddings via daemon or in-process
    if let Ok(filtered) = apply_semantic_filter_with_embeddings(&lines, semantic) {
        return filtered;
    }

    // Fallback: keyword-based heuristic
    apply_semantic_filter_keyword_fallback(lines, semantic)
}

fn apply_semantic_filter_with_embeddings(
    lines: &[String],
    semantic: &SemanticFilterDef,
) -> Result<Vec<String>, anyhow::Error> {
    use crate::embed::{embed_query, set_active_model};

    // Set model if specified
    if let Some(model) = &semantic.model {
        if !crate::embed::is_known_model(model) {
            eprintln!(
                "[tokenix] warning: semantic_filter.model '{}' is unknown; falling back to '{}'",
                model,
                crate::embed::DEFAULT_MODEL_ID
            );
        }
        set_active_model(model);
    }

    // Embed the query
    let query_vec = embed_query(&semantic.query)?;

    // Embed each line (or small groups) and compute similarity
    let always_keep_patterns: Vec<Regex> = semantic
        .always_keep
        .iter()
        .filter_map(|p| Regex::new(p).ok())
        .collect();

    let mut results = Vec::new();

    for line in lines {
        // Always keep lines matching always_keep patterns
        if always_keep_patterns.iter().any(|re| re.is_match(line)) {
            results.push(line.clone());
            continue;
        }

        // Skip very short lines
        if line.trim().len() < 5 {
            continue;
        }

        // Embed the line
        let line_vec = embed_query(line)?;

        // Compute cosine similarity
        let similarity = cosine_similarity(&query_vec, &line_vec);

        if similarity >= semantic.threshold {
            results.push(line.clone());
        }
    }

    Ok(results)
}

fn apply_semantic_filter_keyword_fallback(
    lines: Vec<String>,
    semantic: &SemanticFilterDef,
) -> Vec<String> {
    let query_terms: Vec<&str> = semantic.query.split_whitespace().collect();
    let always_keep_patterns: Vec<Regex> = semantic
        .always_keep
        .iter()
        .filter_map(|p| Regex::new(p).ok())
        .collect();

    lines
        .into_iter()
        .filter(|line| {
            if always_keep_patterns.iter().any(|re| re.is_match(line)) {
                return true;
            }
            // Simple keyword overlap as proxy for semantic relevance
            let line_lower = line.to_lowercase();
            query_terms
                .iter()
                .any(|term| line_lower.contains(&term.to_lowercase()))
        })
        .collect()
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}

fn apply_summarize_json(lines: Vec<String>, summarize: &SummarizeJsonDef) -> Vec<String> {
    let content = lines.join("\n");
    let trimmed = content.trim();

    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return lines;
    }

    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return lines;
    };

    summarize_json_value(&mut value, summarize, 0);

    let result = serde_json::to_string_pretty(&value).unwrap_or(content);
    result.lines().map(|l| l.to_string()).collect()
}

fn summarize_json_value(value: &mut serde_json::Value, summarize: &SummarizeJsonDef, depth: usize) {
    if depth >= summarize.max_depth {
        return;
    }

    match value {
        serde_json::Value::Object(map) => {
            let keys_to_remove: Vec<String> =
                map.keys()
                    .filter(|k| {
                        let path = if depth == 0 { k.as_str() } else { "" };
                        summarize.exclude.iter().any(|ex| {
                            k.as_str() == ex.as_str() || (depth == 0 && path == ex.as_str())
                        })
                    })
                    .cloned()
                    .collect();
            for k in keys_to_remove {
                map.remove(&k);
            }

            for (k, v) in map.iter_mut() {
                let full_path = if depth == 0 {
                    k.clone()
                } else {
                    format!("{}.{}", depth, k)
                };
                if summarize
                    .always_include
                    .iter()
                    .any(|inc| inc == &full_path || inc == k)
                {
                    continue;
                }
                summarize_json_value(v, summarize, depth + 1);
            }
        }
        serde_json::Value::Array(arr) => {
            if arr.len() > summarize.max_array_items {
                let shown = arr.drain(summarize.max_array_items..).collect::<Vec<_>>();
                let count = shown.len();
                arr.push(serde_json::Value::String(format!(
                    "... {} more item(s) omitted ...",
                    count
                )));
            }
            for item in arr.iter_mut() {
                summarize_json_value(item, summarize, depth + 1);
            }
        }
        _ => {}
    }
}

fn apply_token_budget(text: &str, budget: usize) -> String {
    let tokens = crate::chunker::count_tokens(text);
    if tokens <= budget {
        return text.to_string();
    }

    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return text.to_string();
    }

    // Priority order: errors/warnings > head > tail > middle
    let mut priority_lines = Vec::new();
    let mut other_lines = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        let is_high_priority = t.starts_with("error")
            || t.starts_with("warning")
            || t.starts_with("FAIL")
            || t.starts_with("panic")
            || t.contains("error[")
            || t.contains("warning[")
            || i < lines.len() / 4
            || i >= lines.len() * 3 / 4;
        if is_high_priority {
            priority_lines.push((i, *line));
        } else {
            other_lines.push((i, *line));
        }
    }

    let mut result = Vec::new();
    let mut used = 0usize;

    for (_, line) in priority_lines {
        let line_tokens = crate::chunker::count_tokens(line);
        if used + line_tokens > budget {
            break;
        }
        result.push(line.to_string());
        used += line_tokens;
    }

    // Fill remaining budget with other lines (prefer head/tail)
    for (_, line) in other_lines {
        let line_tokens = crate::chunker::count_tokens(line);
        if used + line_tokens > budget {
            break;
        }
        result.push(line.to_string());
        used += line_tokens;
    }

    if result.len() < lines.len() {
        result.push(format!(
            "[... {} lines omitted to fit token budget {} ...]",
            lines.len() - result.len(),
            budget
        ));
    }

    result.join("\n")
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

fn apply_sizing(mut lines: Vec<String>, f: &FilterDef) -> Vec<String> {
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
        r#"Generate a tokenix TOML filter for the command `{command}`.

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

# ADVANCED (extended filtering capabilities):
replace_patterns = [       # regex replacements: [[pattern, replacement], ...]
  ["\\d+\\.\\d+s", "<duration>"],
  ["/home/[^/]+/", "~/"],
]
extract_sections = [       # extract content between markers
  {{ start_pattern = "---- FAILURES ----", end_pattern = "^\\s*$", include_markers = true, max_matches = 3 }},
]
semantic_filter = {{       # embedding-based relevance filtering (uses daemon/embed)
  query = "test failure error panic",
  threshold = 0.3,
  always_keep = ["^error\\[", "^FAIL"],
  model = "nomic-v1.5"
}}
deduplicate_blocks = {{    # structural block deduplication
  min_block_lines = 3,
  similarity = 0.8,
  block_delimiter = "^\\s*$"
}}
summarize_json = {{        # intelligent JSON summarization
  max_array_items = 10,
  max_depth = 3,
  always_include = ["packages", "workspace_members"],
  exclude = ["manifest", "dependencies"]
}}
token_budget = 2000        # hard token limit with smart truncation
```

Rules:
- Use strip_lines_matching to drop boilerplate (progress, verbose info)
- Use keep_lines_matching only if output has a clear signal/noise separation
- Use match_output for commands that succeed silently or with a predictable summary line
- Set on_empty when the command normally succeeds silently
- Use replace_patterns to normalize paths, timestamps, IDs, etc.
- Use extract_sections to pull out failure blocks, error sections, etc.
- Use semantic_filter for query-aware relevance (requires embed model)
- Use deduplicate_blocks for repetitive output (test runs, build steps)
- Use summarize_json for large JSON (cargo metadata, API responses)
- Use token_budget as a hard cap with priority-based truncation
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
        // timeout wrapper
        assert_eq!(
            get_effective_command("timeout 180 pnpm run test"),
            "pnpm run test"
        );
        assert_eq!(
            get_effective_command("timeout -k 10 180 pnpm run test"),
            "pnpm run test"
        );
        assert_eq!(
            get_effective_command("timeout --foreground 60s cargo test --quiet"),
            "cargo test --quiet"
        );
        // time wrapper
        assert_eq!(
            get_effective_command("time pnpm run build"),
            "pnpm run build"
        );
        // nice wrapper
        assert_eq!(get_effective_command("nice -n 10 make all"), "make all");
        // stacked: timeout + nice
        assert_eq!(
            get_effective_command("timeout 30 nice -n 5 pnpm run test"),
            "pnpm run test"
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
    fn timeout_wrapper_included_in_candidates() {
        // Reported: `timeout 180 pnpm run test` must produce `pnpm run test`
        // as a candidate so filters keyed on `pnpm run test` match.
        let candidates = derive_command_candidates("timeout 180 pnpm run test");
        assert!(
            candidates.contains(&"pnpm run test".to_string()),
            "candidates: {candidates:?}"
        );
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
            passthrough_when_emptied: false,
            match_output: vec![],
            truncate_lines_at: Some(4),
            filter_stderr: false,
            replace_patterns: vec![],
            extract_sections: vec![],
            semantic_filter: None,
            deduplicate_blocks: None,
            summarize_json: None,
            token_budget: None,
        };
        // Would panic with naive &l[..4] because 'é'/'ç' straddle the boundary.
        let out = apply_filter("café\nação\n", &f);
        assert_eq!(out, "caf\naç");
    }

    #[test]
    fn semantic_filter_issues_flags_unknown_model_and_bad_threshold() {
        let mut f = FilterDef {
            description: None,
            match_command: ".*".to_string(),
            strip_ansi: false,
            strip_lines_matching: vec![],
            keep_lines_matching: vec![],
            max_lines: None,
            head_lines: None,
            tail_lines: None,
            on_empty: None,
            passthrough_when_emptied: false,
            match_output: vec![],
            truncate_lines_at: None,
            filter_stderr: false,
            replace_patterns: vec![],
            extract_sections: vec![],
            semantic_filter: Some(SemanticFilterDef {
                query: "errors".to_string(),
                threshold: 1.5,
                always_keep: vec![],
                model: Some("does-not-exist".to_string()),
            }),
            deduplicate_blocks: None,
            summarize_json: None,
            token_budget: None,
        };
        let issues = semantic_filter_issues(&f);
        assert_eq!(
            issues.len(),
            2,
            "expected model + threshold issues: {issues:?}"
        );

        let sem = f.semantic_filter.as_mut().unwrap();
        sem.model = Some("nomic-v1.5".to_string());
        sem.threshold = 0.3;
        assert!(semantic_filter_issues(&f).is_empty());
    }

    #[test]
    fn split_on_operators_handles_compound_commands() {
        // Spaced and unspaced operators segment identically.
        assert_eq!(
            split_on_operators("cd foo && gitleaks detect"),
            vec!["cd foo", "gitleaks detect"]
        );
        assert_eq!(
            split_on_operators("cd foo;gitleaks"),
            vec!["cd foo", "gitleaks"]
        );
        assert_eq!(split_on_operators("a || b"), vec!["a", "b"]);
        assert_eq!(
            split_on_operators("producer | gitleaks detect"),
            vec!["producer", "gitleaks detect"]
        );
        // Quoted operators are not split points.
        assert_eq!(
            split_on_operators(r#"echo "a;b" && x"#),
            vec![r#"echo "a;b""#, "x"]
        );
    }

    #[test]
    fn derive_candidates_segments_compound_commands() {
        let candidates = derive_command_candidates("cd foo;gitleaks detect --source .");
        assert!(
            candidates.iter().any(|c| c == "gitleaks detect --source ."),
            "expected a gitleaks segment candidate, got: {candidates:?}"
        );
    }

    #[test]
    fn find_filter_matches_command_after_cd_and_pipe() {
        let f = FilterDef {
            description: None,
            match_command: "^gitleaks\\b".to_string(),
            strip_ansi: false,
            strip_lines_matching: vec![],
            keep_lines_matching: vec![],
            max_lines: None,
            head_lines: None,
            tail_lines: None,
            on_empty: None,
            passthrough_when_emptied: false,
            match_output: vec![],
            truncate_lines_at: None,
            filter_stderr: false,
            replace_patterns: vec![],
            extract_sections: vec![],
            semantic_filter: None,
            deduplicate_blocks: None,
            summarize_json: None,
            token_budget: None,
        };
        let filters = [f];
        // Unspaced semicolon, cd prefix, and a pipe all resolve to the filter.
        assert!(find_filter("cd repo;gitleaks detect", &filters).is_some());
        assert!(find_filter("npm i && gitleaks detect", &filters).is_some());
        assert!(find_filter("cat x | gitleaks detect", &filters).is_some());
        // A bare argument named gitleaks must NOT match (anchored base command).
        assert!(find_filter("echo gitleaks", &filters).is_none());
    }

    #[test]
    fn strip_subcommand_global_opts_normalizes_tool_globals() {
        let eff = |c: &str| get_effective_command(c);
        // git global options before the subcommand are peeled away.
        assert_eq!(eff("git -C /repo add ."), "git add .");
        assert_eq!(eff("git -c user.name=x commit -m hi"), "git commit -m hi");
        assert_eq!(eff("git --git-dir=/r/.git -C /r status"), "git status");
        assert_eq!(eff("git --no-pager log --oneline"), "git log --oneline");
        // kubectl / docker / cargo share the same global-option bug class.
        assert_eq!(eff("kubectl -n prod get pods"), "kubectl get pods");
        assert_eq!(eff("docker -H tcp://h ps -a"), "docker ps -a");
        assert_eq!(eff("cargo +nightly test"), "cargo test");
        // Subcommand-less or unknown tools are untouched.
        assert_eq!(eff("git status"), "git status");
        assert_eq!(eff("ls -la"), "ls -la");
    }

    #[test]
    fn find_filter_matches_git_with_global_options() {
        let f = FilterDef {
            description: None,
            match_command: "^git\\s+add\\b".to_string(),
            strip_ansi: false,
            strip_lines_matching: vec![],
            keep_lines_matching: vec![],
            max_lines: None,
            head_lines: None,
            tail_lines: None,
            on_empty: None,
            passthrough_when_emptied: false,
            match_output: vec![],
            truncate_lines_at: None,
            filter_stderr: false,
            replace_patterns: vec![],
            extract_sections: vec![],
            semantic_filter: None,
            deduplicate_blocks: None,
            summarize_json: None,
            token_budget: None,
        };
        let filters = [f];
        assert!(find_filter("git -C /repo add .", &filters).is_some());
        assert!(find_filter("cd x && git -c k=v add -A", &filters).is_some());
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
            passthrough_when_emptied: false,
            match_output: vec![MatchOutput {
                pattern: "total size is".to_string(),
                message: "ok (synced)".to_string(),
                unless: Some("error|failed".to_string()),
            }],
            truncate_lines_at: None,
            filter_stderr: false,
            replace_patterns: vec![],
            extract_sections: vec![],
            semantic_filter: None,
            deduplicate_blocks: None,
            summarize_json: None,
            token_budget: None,
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
        #[serde(default)]
        command: Option<String>,
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
                    if let Some(command) = case.command.as_deref() {
                        let re = Regex::new(&fdef.match_command).unwrap_or_else(|e| {
                            panic!("{asset} [{fname}] invalid match_command: {e}")
                        });
                        assert!(
                            re.is_match(command),
                            "{asset} [{fname}] expected match_command to match {:?}",
                            command
                        );
                    }
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
