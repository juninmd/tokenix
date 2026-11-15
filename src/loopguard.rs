//! Failure-loop note for `tokenix run`.
//!
//! An agent stuck on a failing command re-runs it and pays for the same failure
//! text every time. TokenEconomizer (arXiv 2610.22732) answers that with a
//! "loop guard": once a turn keeps hitting the same failure, the agent gets a
//! short redirect that mirrors its own actions back at it. Their guard asks a
//! judge model; this is the rule-based share that needs no model at all, and it
//! keeps their two constraints — the note carries nothing but what the agent
//! already did, and it never tells the agent to stop.
//!
//! The rule: the *same command*, in the *same project*, failing with the *same
//! output* `threshold` times inside the TTL. A different failure is progress and
//! restarts the count; a success clears it.
//!
//! It only ever **adds** one line to stderr. The failure text itself is printed
//! in full every time, so "never mask a failure" holds, and nothing is collapsed
//! on the strength of the signature — which is why the signature may ignore
//! timings, where `recall`'s dedup (which *replaces* output) must stay exact.
//!
//! State is a small JSON file of digests under `store::global_dir()`: no command
//! line and no output is persisted, so there is nothing for `redact` to mask.

use std::path::PathBuf;
use std::sync::OnceLock;

use regex::Regex;

/// Identical failures before the note is printed, and the cadence after it:
/// with 3 the note appears on the 3rd, 6th, 9th… repeat, not on every run.
const DEFAULT_THRESHOLD: u32 = 3;
/// A streak older than this is a previous sitting, not the current loop.
const DEFAULT_TTL_SECS: f64 = 1800.0;
/// Newest streaks kept; the file is bounded whatever the agent does.
const STATE_CAP: usize = 64;

fn enabled() -> bool {
    !std::env::var("TOKENIX_LOOP_GUARD").is_ok_and(|v| v == "0")
}

fn threshold() -> u32 {
    std::env::var("TOKENIX_LOOP_THRESHOLD")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        // 1 would annotate every failure, which is a tax, not a loop signal.
        .filter(|n| *n >= 2)
        .unwrap_or(DEFAULT_THRESHOLD)
}

fn ttl_secs() -> f64 {
    std::env::var("TOKENIX_LOOP_TTL")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|n| *n > 0.0)
        .unwrap_or(DEFAULT_TTL_SECS)
}

fn state_path() -> Option<PathBuf> {
    Some(crate::store::global_dir()?.join("loop_guard.json"))
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
struct Streak {
    /// Digest of the project root: the state file is machine-wide.
    project: String,
    /// Digest of the full command line.
    command: String,
    /// Digest of the normalized failure output plus the exit code.
    signature: String,
    count: u32,
    first_ts: f64,
    last_ts: f64,
}

fn load() -> Vec<Streak> {
    let Some(path) = state_path() else {
        return Vec::new();
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save(streaks: &[Streak]) {
    let Some(path) = state_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(raw) = serde_json::to_string(streaks) {
        let _ = std::fs::write(&path, raw);
        crate::store::restrict_to_owner(&path);
    }
}

/// Run-to-run noise that does not change what failed: durations, clock times,
/// dates and addresses. Counts are deliberately **kept** — "3 failed" becoming
/// "2 failed" is progress, and must read as a different failure.
fn volatile() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(concat!(
            r"0x[0-9a-fA-F]+",
            r"|\d{4}-\d{2}-\d{2}",
            r"|\d{1,2}:\d{2}(?::\d{2})?(?:[.,]\d+)?",
            r"|\d+(?:\.\d+)?(?:ns|µs|us|ms|s|m|h)\b",
            r"|\d+(?:\.\d+)?\s(?:secs?|seconds?|mins?|minutes?)\b",
        ))
        .expect("loop guard volatile pattern compiles")
    })
}

/// What a failing run "looked like", for telling a repeat from a new failure.
pub fn failure_signature(stdout: &str, stderr: &str, exit_code: i32) -> String {
    let re = volatile();
    let mut normalized = String::with_capacity(stdout.len() + stderr.len() + 16);
    for stream in [stdout, stderr] {
        normalized.push_str(&re.replace_all(stream, "~"));
        normalized.push('\u{1f}');
    }
    normalized.push_str(&exit_code.to_string());
    crate::recall::digest(&normalized)
}

/// Fold one failure into `streaks` and say whether this repeat earns the note.
/// Returns `(count, seconds since the first identical failure)` when it does.
///
/// Pure over its inputs so the rule is testable without touching the disk.
fn fold_failure(
    streaks: &mut Vec<Streak>,
    project: &str,
    command: &str,
    signature: &str,
    now: f64,
    threshold: u32,
    ttl: f64,
) -> Option<(u32, f64)> {
    streaks.retain(|s| now - s.last_ts <= ttl);
    let pos = streaks
        .iter()
        .position(|s| s.project == project && s.command == command);
    let streak = match pos {
        Some(i) if streaks[i].signature == signature => {
            let mut s = streaks.remove(i);
            s.count = s.count.saturating_add(1);
            s.last_ts = now;
            s
        }
        other => {
            // A different failure from the same command: the agent changed
            // something and the result moved. That is not a loop.
            if let Some(i) = other {
                streaks.remove(i);
            }
            Streak {
                project: project.to_string(),
                command: command.to_string(),
                signature: signature.to_string(),
                count: 1,
                first_ts: now,
                last_ts: now,
            }
        }
    };
    let verdict = (streak.count >= threshold && streak.count % threshold == 0)
        .then(|| (streak.count, (now - streak.first_ts).max(0.0)));
    streaks.insert(0, streak);
    streaks.truncate(STATE_CAP);
    verdict
}

fn fold_success(streaks: &mut Vec<Streak>, project: &str, command: &str) -> bool {
    let before = streaks.len();
    streaks.retain(|s| !(s.project == project && s.command == command));
    streaks.len() != before
}

fn span(secs: f64) -> String {
    let secs = secs as u64;
    if secs < 90 {
        format!("{secs}s")
    } else {
        format!("{}m", secs / 60)
    }
}

/// The line itself. Only facts about the agent's own runs: no advice, no
/// instruction to stop, nothing the agent could mistake for command output.
fn note(count: u32, elapsed: f64) -> String {
    format!(
        "[tokenix: this command has failed {count} times in {} with the same output (timings aside) — nothing it reports changed between runs]",
        span(elapsed)
    )
}

fn has_failure_text(stdout: &str, stderr: &str) -> bool {
    !(stdout.trim().is_empty() && stderr.trim().is_empty())
}

/// Record a failed run; returns the note when this failure repeats enough.
pub fn on_failure(
    project: &str,
    command: &str,
    stdout: &str,
    stderr: &str,
    exit_code: i32,
    now: f64,
) -> Option<String> {
    // A silent non-zero exit is an answer (`grep -q`, `git diff --exit-code`,
    // `test -f`), not a failure the agent is stuck on: there is no failure
    // text to repeat, and calling a poll "failed N times" would be noise.
    if !enabled() || !has_failure_text(stdout, stderr) {
        return None;
    }
    let mut streaks = load();
    let verdict = fold_failure(
        &mut streaks,
        &crate::recall::digest(project),
        &crate::recall::digest(command),
        &failure_signature(stdout, stderr, exit_code),
        now,
        threshold(),
        ttl_secs(),
    );
    save(&streaks);
    verdict.map(|(count, elapsed)| note(count, elapsed))
}

/// A success ends the streak: the next failure of this command starts at one.
pub fn on_success(project: &str, command: &str) {
    if !enabled() {
        return;
    }
    let mut streaks = load();
    // Most successful commands never failed; do not rewrite the file for them.
    if fold_success(
        &mut streaks,
        &crate::recall::digest(project),
        &crate::recall::digest(command),
    ) {
        save(&streaks);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TTL: f64 = 1800.0;

    fn fail(streaks: &mut Vec<Streak>, cmd: &str, sig: &str, now: f64) -> Option<(u32, f64)> {
        fold_failure(streaks, "proj", cmd, sig, now, 3, TTL)
    }

    #[test]
    fn note_fires_on_the_third_identical_failure_and_then_every_third() {
        let mut s = Vec::new();
        assert_eq!(fail(&mut s, "cargo test", "a", 0.0), None);
        assert_eq!(fail(&mut s, "cargo test", "a", 10.0), None);
        assert_eq!(fail(&mut s, "cargo test", "a", 25.0), Some((3, 25.0)));
        assert_eq!(fail(&mut s, "cargo test", "a", 30.0), None);
        assert_eq!(fail(&mut s, "cargo test", "a", 40.0), None);
        assert_eq!(fail(&mut s, "cargo test", "a", 50.0), Some((6, 50.0)));
    }

    #[test]
    fn a_different_failure_is_progress_and_restarts_the_count() {
        let mut s = Vec::new();
        fail(&mut s, "cargo test", "a", 0.0);
        fail(&mut s, "cargo test", "a", 1.0);
        assert_eq!(fail(&mut s, "cargo test", "b", 2.0), None);
        assert_eq!(s.len(), 1, "one streak per command, not one per failure");
        assert_eq!(fail(&mut s, "cargo test", "b", 3.0), None);
        assert_eq!(fail(&mut s, "cargo test", "b", 4.0), Some((3, 2.0)));
    }

    #[test]
    fn a_success_clears_the_streak() {
        let mut s = Vec::new();
        fail(&mut s, "cargo test", "a", 0.0);
        fail(&mut s, "cargo test", "a", 1.0);
        assert!(fold_success(&mut s, "proj", "cargo test"));
        assert!(!fold_success(&mut s, "proj", "cargo test"));
        assert_eq!(fail(&mut s, "cargo test", "a", 2.0), None);
    }

    #[test]
    fn streaks_are_scoped_to_command_and_project() {
        let mut s = Vec::new();
        fail(&mut s, "cargo test", "a", 0.0);
        fail(&mut s, "cargo test", "a", 1.0);
        // Same failure text from another command, and from another checkout.
        assert_eq!(fail(&mut s, "cargo build", "a", 2.0), None);
        assert_eq!(
            fold_failure(&mut s, "other", "cargo test", "a", 3.0, 3, TTL),
            None
        );
        assert_eq!(fail(&mut s, "cargo test", "a", 4.0), Some((3, 4.0)));
    }

    #[test]
    fn an_expired_streak_does_not_count() {
        let mut s = Vec::new();
        fail(&mut s, "cargo test", "a", 0.0);
        fail(&mut s, "cargo test", "a", 1.0);
        assert_eq!(fail(&mut s, "cargo test", "a", 1.0 + TTL + 1.0), None);
        assert_eq!(s[0].count, 1);
    }

    #[test]
    fn state_is_bounded() {
        let mut s = Vec::new();
        for i in 0..(STATE_CAP + 20) {
            fail(&mut s, &format!("cmd {i}"), "a", i as f64);
        }
        assert_eq!(s.len(), STATE_CAP);
    }

    #[test]
    fn signature_ignores_timings_but_not_counts() {
        let a = failure_signature("", "2 passed, 3 failed in 1.20s\n", 1);
        let b = failure_signature("", "2 passed, 3 failed in 0.87s\n", 1);
        assert_eq!(a, b, "a duration is not a different failure");

        let fewer = failure_signature("", "3 passed, 2 failed in 1.20s\n", 1);
        assert_ne!(a, fewer, "fewer failing tests is progress");

        let stamped = failure_signature("12:01:07 ERROR at 0x7ffe01\n", "", 1);
        let restamped = failure_signature("12:03:44 ERROR at 0x55aa90\n", "", 1);
        assert_eq!(stamped, restamped);

        assert_ne!(a, failure_signature("", "2 passed, 3 failed in 1.20s\n", 2));
        assert_ne!(
            failure_signature("x", "", 1),
            failure_signature("", "x", 1),
            "stdout and stderr are not interchangeable"
        );
    }

    #[test]
    fn a_silent_exit_code_is_an_answer_not_a_failure() {
        assert!(!has_failure_text("", " \n"));
        assert!(has_failure_text("", "error: boom\n"));
        assert!(has_failure_text("1 failed\n", ""));
    }

    #[test]
    fn note_states_only_what_happened() {
        let line = note(3, 125.0);
        assert!(line.starts_with("[tokenix: "));
        assert!(line.contains("failed 3 times in 2m"));
        assert!(!line.contains('\n'));
        // The TokenEconomizer lint, kept: a note must never read as an instruction
        // to stop or as completion/verification advice.
        let lower = line.to_lowercase();
        for word in ["stop", "skip", "do not", "don't", "abort", "verify", "done"] {
            assert!(!lower.contains(word), "note must not say {word:?}");
        }
    }
}
