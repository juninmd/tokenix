//! `tokenix facts` — environment facts an agent keeps rediscovering.
//!
//! Sessions in one repository fail at the same things over and over: `pytest`
//! is not on PATH but `python -m pytest` works, `tsc` is missing but `npx tsc`
//! is there. Every rediscovery costs a failed call, its error text, and a retry.
//! TokenEconomizer (arXiv 2610.22732) calls the cure "micro-skills" and learns
//! them from finished sessions; this is its model-free family (their
//! `envfacts`: *form repair* and *absent tool*), read from the same on-disk
//! transcripts `discover` replays.
//!
//! Two deliberate differences from the original:
//!
//! - **Report only.** TokenEconomizer injects learned facts at session start.
//!   A fact here is derived from command output, which is attacker-reachable
//!   text; tokenix prints the lines and a person pastes them into `AGENTS.md` /
//!   `CLAUDE.md`. Nothing is written into an agent's context by this command.
//! - **Nothing free-form is rendered.** A fact interpolates only a tool name
//!   and a runner form, both validated against a narrow character set and a
//!   fixed list of runner shapes, so transcript text cannot ride into the
//!   instructions file through it.
//!
//! The evidence rule is theirs: a fact needs the failure in several sessions
//! (`--min-sessions`, default 3), and it retires when a later session shows the
//! bare tool working.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::Result;
use colored::Colorize;
use regex::Regex;
use serde::Serialize;
use serde_json::Value;

use crate::conversation_audit::Agent;
use crate::ui::box_header;

pub struct Options {
    pub agent: Agent,
    pub path: PathBuf,
    /// Sessions a failure must recur in before it is reported as a fact.
    pub min_sessions: usize,
    /// Only scan transcripts modified in the last N days (0 = no limit).
    pub since_days: u64,
    /// Read every project's transcripts instead of this repository's.
    pub all_projects: bool,
    pub json: bool,
    pub markdown: bool,
}

/// Same bound `discover` uses: a bounded scan beats a surprise ten-minute run.
const MAX_FILE_BYTES: u64 = 25 * 1024 * 1024;

/// One shell call as the transcript recorded it, in session order.
struct Call {
    command: String,
    output: String,
}

#[derive(Serialize, Debug, PartialEq)]
struct Fact {
    tool: String,
    /// Sessions in which the bare tool failed with "command not found".
    failed_sessions: usize,
    /// The runner form with the best record, if any form ever worked.
    working_form: Option<String>,
    /// Sessions in which `working_form` ran without an environment error.
    worked_sessions: usize,
    claim: String,
}

#[derive(Serialize)]
struct Report {
    scanned_sessions: usize,
    skipped_files: usize,
    min_sessions: usize,
    facts: Vec<Fact>,
    /// Failures seen in fewer than `min_sessions` sessions: evidence, not facts.
    forming: usize,
}

pub fn run(opts: Options) -> Result<()> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let root = std::fs::canonicalize(&opts.path).unwrap_or_else(|_| opts.path.clone());
    let root = crate::store::find_project_root(&root);

    let cutoff = (opts.since_days > 0)
        .then(|| {
            opts.since_days
                .checked_mul(86_400)
                .map(std::time::Duration::from_secs)
                .and_then(|dur| std::time::SystemTime::now().checked_sub(dur))
        })
        .flatten();

    // (mtime, calls): sessions are folded oldest first, so "a later session
    // showed the bare tool working" has a meaning.
    let mut sessions: Vec<(std::time::SystemTime, Vec<Call>)> = Vec::new();
    let mut skipped_files = 0usize;
    for (agent_key, dir) in crate::transcripts::roots(&home) {
        if !crate::discover::agent_selected(opts.agent, agent_key) || !dir.exists() {
            continue;
        }
        for path in crate::transcripts::transcript_files(&dir, agent_key) {
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            if cutoff.is_some_and(|cut| modified < cut) || meta.len() > MAX_FILE_BYTES {
                skipped_files += 1;
                continue;
            }
            let Ok(raw) = std::fs::read_to_string(&path) else {
                continue;
            };
            let values: Vec<Value> = if path.extension().and_then(|x| x.to_str()) == Some("json") {
                serde_json::from_str::<Value>(&raw).into_iter().collect()
            } else {
                raw.lines()
                    .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                    .collect()
            };
            if !opts.all_projects && !session_in_project(&values, &root) {
                continue;
            }
            let calls = session_calls(&values);
            if !calls.is_empty() {
                sessions.push((modified, calls));
            }
        }
    }
    sessions.sort_by_key(|(modified, _)| *modified);

    let ordered: Vec<Vec<Call>> = sessions.into_iter().map(|(_, calls)| calls).collect();
    let (facts, forming) = distill(&ordered, opts.min_sessions.max(1), &root);
    let report = Report {
        scanned_sessions: ordered.len(),
        skipped_files,
        min_sessions: opts.min_sessions.max(1),
        facts,
        forming,
    };

    if opts.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else if opts.markdown {
        for fact in &report.facts {
            println!("- {}", fact.claim);
        }
    } else {
        print_human(&report, &root);
    }
    Ok(())
}

/// Does this transcript belong to the repository at `root`? Claude Code stamps
/// `cwd` on every line and Codex on its `session_meta` payload; a transcript
/// that names no working directory cannot be attributed and is left out.
fn session_in_project(values: &[Value], root: &Path) -> bool {
    values.iter().any(|v| {
        [v.get("cwd"), v.get("payload").and_then(|p| p.get("cwd"))]
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .any(|cwd| Path::new(cwd).starts_with(root))
    })
}

fn session_calls(values: &[Value]) -> Vec<Call> {
    let mut tool_map: HashMap<String, (String, Option<String>)> = HashMap::new();
    for value in values {
        crate::discover::collect_calls(value, &mut tool_map);
    }
    let mut calls = Vec::new();
    for value in values {
        crate::discover::visit_outputs(value, &mut |id: &str, text: String| {
            if let Some((_tool, Some(command))) = tool_map.get(id) {
                calls.push(Call {
                    command: command.clone(),
                    output: text,
                });
            }
        });
    }
    calls
}

// ---- reading one call -------------------------------------------------------

fn tool_name_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._+-]{0,39}$").expect("tool name regex"))
}

fn is_tool_name(s: &str) -> bool {
    tool_name_re().is_match(s)
}

/// Tools the output says the shell could not find: `bash: ruff: command not
/// found`, `/bin/sh: 1: ruff: not found`, `zsh: command not found: ruff`,
/// `'ruff' is not recognized as …` (cmd and PowerShell).
fn not_found_names(output: &str) -> BTreeSet<String> {
    static ZSH: OnceLock<Regex> = OnceLock::new();
    static POSIX: OnceLock<Regex> = OnceLock::new();
    static WINDOWS: OnceLock<Regex> = OnceLock::new();
    let zsh =
        ZSH.get_or_init(|| Regex::new(r"command not found:\s+(\S+)").expect("zsh not-found regex"));
    let posix = POSIX.get_or_init(|| {
        Regex::new(r#"([^\s:'"]+):\s+(?:command\s+)?not found\s*$"#).expect("posix not-found regex")
    });
    let windows =
        WINDOWS.get_or_init(|| Regex::new(r"'([^']+)' is not recognized").expect("windows regex"));

    let mut names = BTreeSet::new();
    // Cheap gate: almost no output mentions either phrase.
    if !output.contains("not found") && !output.contains("is not recognized") {
        return names;
    }
    for line in output.lines() {
        let hit = zsh
            .captures(line)
            .or_else(|| posix.captures(line))
            .or_else(|| windows.captures(line));
        if let Some(name) = hit.and_then(|c| c.get(1)) {
            let name = basename(name.as_str());
            if is_tool_name(name) {
                names.insert(name.to_string());
            }
        }
    }
    names
}

/// Output that shows the environment refused the command. A form only counts
/// as working when none of these appear: agents pipe through `| tail`, so the
/// text is a better witness than an exit code the transcript may not carry.
fn has_environment_error(output: &str) -> bool {
    [
        "not found",
        "is not recognized",
        "No such file or directory",
        "No module named",
        "ModuleNotFoundError",
        "Cannot find module",
        "Permission denied",
    ]
    .iter()
    .any(|needle| output.contains(needle))
}

fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// The words of one command segment, without leading `VAR=value` assignments.
fn words(segment: &str) -> Vec<&str> {
    segment
        .split_whitespace()
        .skip_while(|w| {
            w.split_once('=').is_some_and(|(name, _)| {
                !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            })
        })
        .collect()
}

/// Runner prefixes that execute the tool named right after them. A fixed list
/// on purpose: `grep pytest`, `pip install pytest` and `which pytest` also
/// "contain the tool and succeed", and none of them is a way to run it.
const RUNNERS: &[&[&str]] = &[
    &["python", "-m"],
    &["python3", "-m"],
    &["py", "-m"],
    &["uv", "run"],
    &["uvx"],
    &["poetry", "run"],
    &["pipenv", "run"],
    &["pdm", "run"],
    &["hatch", "run"],
    &["npx"],
    &["bunx"],
    &["bun", "x"],
    &["pnpm", "exec"],
    &["pnpm", "dlx"],
    &["yarn", "dlx"],
    &["yarn"],
    &["bundle", "exec"],
    &["dotnet"],
    &["mise", "exec", "--"],
];

/// If this segment runs `tool` through something other than its bare name,
/// return that form: `python -m pytest`, `npx tsc`, `./node_modules/.bin/tsc`.
fn runner_form(segment: &str, tool: &str, root: &Path) -> Option<String> {
    let words = words(segment);
    let head = *words.first()?;

    // An explicit path to the tool.
    if head != tool && basename(head) == tool {
        return path_form(head, root);
    }
    RUNNERS.iter().find_map(|runner| {
        let hit = words.len() > runner.len()
            && words[..runner.len()] == **runner
            && words[runner.len()] == tool;
        hit.then(|| format!("{} {tool}", runner.join(" ")))
    })
}

/// A path form is a repository fact only when it is relative to the
/// repository. An absolute path elsewhere names one machine (and often a home
/// directory), so it is dropped rather than printed into shared instructions.
fn path_form(head: &str, root: &Path) -> Option<String> {
    static SAFE: OnceLock<Regex> = OnceLock::new();
    let safe = SAFE.get_or_init(|| Regex::new(r"^[A-Za-z0-9._/+-]{1,120}$").expect("path regex"));
    let normalized = head.replace('\\', "/");
    let relative = if is_absolute_anywhere(&normalized) {
        let root = root.to_string_lossy().replace('\\', "/");
        let rel = normalized.strip_prefix(root.trim_end_matches('/'))?;
        // `/work/repo-two/bin/x` shares the prefix `/work/repo` without being
        // inside it: the remainder must start at a separator.
        rel.strip_prefix('/')?.to_string()
    } else {
        normalized
    };
    (!relative.is_empty() && safe.is_match(&relative)).then_some(relative)
}

/// Absolute on *either* platform, whatever this binary was built for. A
/// transcript records the path as the agent's shell spelled it, and Git Bash on
/// Windows writes `/home/alice/.local/bin/pytest` — which `Path::is_absolute`
/// calls relative there (no drive), so it used to be reported as a repo path.
fn is_absolute_anywhere(normalized: &str) -> bool {
    let bytes = normalized.as_bytes();
    normalized.starts_with('/')
        || normalized.starts_with('~')
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
}

// ---- folding sessions into facts --------------------------------------------

#[derive(Default)]
struct Evidence {
    /// Indexes of the sessions where the bare tool was not found.
    failed: BTreeSet<usize>,
    /// Runner form → sessions it worked in.
    forms: BTreeMap<String, BTreeSet<usize>>,
    /// Latest session where the bare tool ran without an environment error.
    bare_ok: Option<usize>,
}

/// `sessions` oldest first. Returns the confirmed facts and how many more
/// tools have failed in fewer than `min_sessions` sessions.
fn distill(sessions: &[Vec<Call>], min_sessions: usize, root: &Path) -> (Vec<Fact>, usize) {
    let mut evidence: BTreeMap<String, Evidence> = BTreeMap::new();

    // Pass 1: which tools ever failed bare. Only a tool the *command* tried to
    // run counts — "not found" lines about anything else are someone else's.
    for (idx, calls) in sessions.iter().enumerate() {
        for call in calls {
            let missing = not_found_names(&call.output);
            if missing.is_empty() {
                continue;
            }
            for segment in crate::filters::split_on_operators(&call.command) {
                let Some(head) = words(&segment).first().copied() else {
                    continue;
                };
                if missing.contains(head) {
                    evidence
                        .entry(head.to_string())
                        .or_default()
                        .failed
                        .insert(idx);
                }
            }
        }
    }

    // Pass 2: for those tools only, what worked.
    for (idx, calls) in sessions.iter().enumerate() {
        for call in calls {
            if has_environment_error(&call.output) {
                continue;
            }
            for segment in crate::filters::split_on_operators(&call.command) {
                let head = words(&segment).first().copied().unwrap_or("");
                for (tool, ev) in evidence.iter_mut() {
                    if head == tool {
                        ev.bare_ok = Some(idx);
                    } else if let Some(form) = runner_form(&segment, tool, root) {
                        ev.forms.entry(form).or_default().insert(idx);
                    }
                }
            }
        }
    }

    let mut facts = Vec::new();
    let mut forming = 0usize;
    for (tool, ev) in evidence {
        // Retired: the bare tool ran fine after the last time it was missing
        // (it was installed, or PATH was fixed). The fact no longer holds.
        let last_failure = ev.failed.iter().next_back().copied().unwrap_or(0);
        if ev.bare_ok.is_some_and(|ok| ok > last_failure) {
            continue;
        }
        if ev.failed.len() < min_sessions {
            forming += 1;
            continue;
        }
        // Best record first; the shorter form breaks a tie.
        let best = ev
            .forms
            .iter()
            .max_by(|a, b| {
                a.1.len()
                    .cmp(&b.1.len())
                    .then_with(|| b.0.len().cmp(&a.0.len()))
            })
            .map(|(form, worked)| (form.clone(), worked.len()));
        let claim = match &best {
            Some((form, _)) => format!("`{tool}` is not on PATH here; `{form}` works."),
            None => {
                format!("`{tool}` is not on PATH here, and no other way of running it has worked.")
            }
        };
        facts.push(Fact {
            tool,
            failed_sessions: ev.failed.len(),
            worked_sessions: best.as_ref().map_or(0, |(_, n)| *n),
            working_form: best.map(|(form, _)| form),
            claim,
        });
    }
    facts.sort_by(|a, b| {
        b.failed_sessions
            .cmp(&a.failed_sessions)
            .then_with(|| a.tool.cmp(&b.tool))
    });
    (facts, forming)
}

fn print_human(report: &Report, root: &Path) {
    box_header("tokenix facts");
    println!(
        "  {} · {} session(s) scanned",
        root.display(),
        report.scanned_sessions
    );
    if report.skipped_files > 0 {
        println!(
            "  {}",
            format!(
                "{} transcript(s) skipped (too old or over 25 MB)",
                report.skipped_files
            )
            .dimmed()
        );
    }
    println!();
    if report.facts.is_empty() {
        println!(
            "  No environment fact has recurred in {} session(s) yet.",
            report.min_sessions
        );
    } else {
        let width = report.facts.iter().map(|f| f.tool.len()).max().unwrap_or(0);
        for fact in &report.facts {
            let repair = match &fact.working_form {
                Some(form) => format!("`{form}` worked in {} session(s)", fact.worked_sessions),
                None => "no working form seen".to_string(),
            };
            println!(
                "  {:<width$}  command not found in {} session(s) · {repair}",
                fact.tool.bold(),
                fact.failed_sessions,
            );
        }
        println!();
        println!("  Lines for AGENTS.md / CLAUDE.md (`tokenix facts --md`):");
        for fact in &report.facts {
            println!("    - {}", fact.claim);
        }
    }
    if report.forming > 0 {
        println!();
        println!(
            "  {}",
            format!(
                "{} more seen in fewer than {} session(s) (--min-sessions 1 lists them)",
                report.forming, report.min_sessions
            )
            .dimmed()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(command: &str, output: &str) -> Call {
        Call {
            command: command.to_string(),
            output: output.to_string(),
        }
    }

    fn root() -> PathBuf {
        PathBuf::from("/work/repo")
    }

    const PYTEST_MISSING: &str = "bash: line 1: pytest: command not found\n";

    #[test]
    fn a_failure_in_three_sessions_with_a_repair_becomes_a_fact() {
        let session = || {
            vec![
                call("pytest tests/", PYTEST_MISSING),
                call("python -m pytest tests/", "3 passed in 0.4s\n"),
            ]
        };
        let (facts, forming) = distill(&[session(), session(), session()], 3, &root());
        assert_eq!(forming, 0);
        assert_eq!(
            facts,
            vec![Fact {
                tool: "pytest".into(),
                failed_sessions: 3,
                working_form: Some("python -m pytest".into()),
                worked_sessions: 3,
                claim: "`pytest` is not on PATH here; `python -m pytest` works.".into(),
            }]
        );
    }

    #[test]
    fn two_sessions_are_evidence_not_a_fact() {
        let session = || vec![call("pytest", PYTEST_MISSING)];
        let (facts, forming) = distill(&[session(), session()], 3, &root());
        assert!(facts.is_empty());
        assert_eq!(forming, 1);
    }

    #[test]
    fn a_tool_with_no_working_form_says_so() {
        let session = || {
            vec![call(
                "cd app && tsc --noEmit",
                "/bin/sh: 1: tsc: not found\n",
            )]
        };
        let (facts, _) = distill(&[session(), session(), session()], 3, &root());
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].working_form, None);
        assert_eq!(
            facts[0].claim,
            "`tsc` is not on PATH here, and no other way of running it has worked."
        );
    }

    #[test]
    fn a_later_session_where_the_bare_tool_works_retires_the_fact() {
        let broken = || vec![call("pytest", PYTEST_MISSING)];
        let fixed = vec![call("pytest", "5 passed in 1.0s\n")];
        let (facts, forming) = distill(&[broken(), broken(), broken(), fixed], 3, &root());
        assert!(facts.is_empty(), "pytest was installed since: {facts:?}");
        assert_eq!(forming, 0);

        // The other order is not a retirement: it worked once, then broke.
        let worked_first = vec![call("pytest", "5 passed in 1.0s\n")];
        let (facts, _) = distill(&[worked_first, broken(), broken(), broken()], 3, &root());
        assert_eq!(facts.len(), 1);
    }

    #[test]
    fn mentioning_a_tool_is_not_a_way_to_run_it() {
        let session = || {
            vec![
                call("pytest", PYTEST_MISSING),
                call("pip install pytest", "Successfully installed pytest-8.0\n"),
                call("grep -rn pytest setup.cfg", "setup.cfg:3:pytest\n"),
                call("which pytest || echo missing", "missing\n"),
            ]
        };
        let (facts, _) = distill(&[session(), session(), session()], 3, &root());
        assert_eq!(facts[0].working_form, None);
    }

    #[test]
    fn a_not_found_line_about_something_the_command_did_not_run_is_ignored() {
        // `make` ran; the missing `protoc` is a fact about make's recipe, and
        // "make is not on PATH" would be false.
        let session = || vec![call("make build", "/bin/sh: 1: protoc: not found\n")];
        let (facts, forming) = distill(&[session(), session(), session()], 3, &root());
        assert!(facts.is_empty());
        assert_eq!(forming, 0);
    }

    #[test]
    fn a_working_form_that_errored_does_not_count() {
        let session = || {
            vec![
                call("tsc", "bash: tsc: command not found\n"),
                call("npx tsc", "sh: 1: tsc: not found\n"),
            ]
        };
        let (facts, _) = distill(&[session(), session(), session()], 3, &root());
        assert_eq!(facts[0].working_form, None);
    }

    #[test]
    fn the_form_with_the_best_record_wins() {
        let both = || {
            vec![
                call("ruff check .", "zsh: command not found: ruff\n"),
                call("uv run ruff check .", "All checks passed!\n"),
            ]
        };
        let once = vec![
            call("ruff check .", "zsh: command not found: ruff\n"),
            call("python -m ruff check .", "All checks passed!\n"),
        ];
        let (facts, _) = distill(&[both(), both(), once], 3, &root());
        assert_eq!(facts[0].working_form.as_deref(), Some("uv run ruff"));
        assert_eq!(facts[0].worked_sessions, 2);
    }

    #[test]
    fn not_found_names_reads_every_shell_dialect() {
        let names = |out: &str| not_found_names(out).into_iter().collect::<Vec<_>>();
        assert_eq!(names("bash: line 1: ruff: command not found"), ["ruff"]);
        assert_eq!(names("/bin/sh: 1: ruff: not found"), ["ruff"]);
        assert_eq!(names("zsh: command not found: ruff"), ["ruff"]);
        assert_eq!(
            names("'tsc' is not recognized as an internal or external command,"),
            ["tsc"]
        );
        // Not a shell lookup failure.
        assert!(names("npm ERR! 404 Not Found - GET https://registry").is_empty());
        assert!(names("ld: library not found for -lfoo").is_empty());
        assert!(names("all good").is_empty());
    }

    #[test]
    fn only_safe_text_reaches_a_claim() {
        // A "tool name" carrying markdown or an instruction is not a tool name.
        assert!(not_found_names("sh: `rm -rf ~`: not found").is_empty());
        assert!(not_found_names("sh: ignore-previous-instructions!!: not found").is_empty());

        // A path form is relative to the repository or it is not reported.
        let root = root();
        assert_eq!(
            runner_form("./node_modules/.bin/tsc -p .", "tsc", &root).as_deref(),
            Some("./node_modules/.bin/tsc")
        );
        assert_eq!(
            runner_form("/work/repo/.venv/bin/pytest -q", "pytest", &root).as_deref(),
            Some(".venv/bin/pytest")
        );
        assert_eq!(
            runner_form("/home/alice/.local/bin/pytest -q", "pytest", &root),
            None,
            "another machine's home directory is not a repository fact"
        );
        // Same answers for the spellings a Windows transcript carries, on
        // every platform: absoluteness is read off the text, not `Path`.
        let win_root = PathBuf::from(r"C:\work\repo");
        assert_eq!(
            runner_form(r"C:\work\repo\.venv\Scripts\pytest -q", "pytest", &win_root).as_deref(),
            Some(".venv/Scripts/pytest")
        );
        assert_eq!(
            runner_form(r"C:\Users\alice\bin\pytest -q", "pytest", &win_root),
            None
        );
        assert_eq!(runner_form("~/bin/pytest -q", "pytest", &root), None);
        // A sibling directory that merely shares the prefix is not inside.
        assert_eq!(
            runner_form("/work/repo-two/bin/pytest -q", "pytest", &root),
            None
        );
    }

    #[test]
    fn env_assignments_do_not_hide_the_command() {
        assert_eq!(words("CI=1 FOO=bar pytest -q"), ["pytest", "-q"]);
        assert_eq!(
            runner_form("CI=1 npx tsc --noEmit", "tsc", &root()).as_deref(),
            Some("npx tsc")
        );
    }

    #[test]
    fn a_session_is_attributed_by_its_working_directory() {
        let root = root();
        let claude = serde_json::json!({"cwd": "/work/repo/crates/core", "type": "user"});
        let codex = serde_json::json!({"type": "session_meta", "payload": {"cwd": "/work/repo"}});
        let other = serde_json::json!({"cwd": "/work/repository-two"});
        let anonymous = serde_json::json!({"type": "user"});
        assert!(session_in_project(&[claude], &root));
        assert!(session_in_project(&[codex], &root));
        assert!(!session_in_project(&[other], &root));
        assert!(!session_in_project(&[anonymous], &root));
    }

    #[test]
    fn calls_are_read_from_a_claude_transcript_in_order() {
        let values = vec![
            serde_json::json!({"message": {"content": [
                {"type": "tool_use", "id": "a", "name": "Bash", "input": {"command": "pytest"}}
            ]}}),
            serde_json::json!({"message": {"content": [
                {"type": "tool_result", "tool_use_id": "a", "content": PYTEST_MISSING}
            ]}}),
            serde_json::json!({"message": {"content": [
                {"type": "tool_use", "id": "b", "name": "Read", "input": {"file_path": "x"}}
            ]}}),
            serde_json::json!({"message": {"content": [
                {"type": "tool_result", "tool_use_id": "b", "content": "file body"}
            ]}}),
        ];
        let calls = session_calls(&values);
        assert_eq!(calls.len(), 1, "only shell calls are evidence");
        assert_eq!(calls[0].command, "pytest");
        assert_eq!(calls[0].output, PYTEST_MISSING);
    }
}
