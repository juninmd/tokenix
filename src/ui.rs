//! Central terminal-UI vocabulary for tokenix's human-facing output.
//!
//! Every command that a person reads (help, doctor, gain, stats, filter) renders
//! through these helpers so the visual language stays consistent: one box style,
//! one progress-bar scheme, one set of semantic colors. The `colored` crate
//! auto-disables ANSI on non-TTY / `NO_COLOR`, so all of this degrades cleanly
//! when piped. LLM/JSON output (query, graph) deliberately does NOT come through
//! here — that is a token contract, not a display surface.

use colored::{ColoredString, Colorize};
use tabled::builder::Builder;
use tabled::settings::{object::Columns, Alignment, Modify, Style};

// ── semantic color tokens ───────────────────────────────────────────────────
// Thin wrappers so a color choice lives in exactly one place and can't drift.

/// Brand / commands / interactive hints.
pub fn accent(s: &str) -> ColoredString {
    s.bright_cyan()
}

/// Success, positive metrics.
pub fn ok(s: &str) -> ColoredString {
    s.green()
}

// ── blocks ──────────────────────────────────────────────────────────────────

const BOX_MIN_WIDTH: usize = 60;

/// Titled rounded box header (`╭─╮ │ ╰─╯`) — the signature header for `gain`,
/// `filter`, and friends. Width grows to fit the title; padding is computed on
/// `chars().count()` so multibyte titles (e.g. `·`) stay aligned.
pub fn box_header(title: &str) {
    let inner = format!(" {} ", title);
    let width = inner.chars().count().max(BOX_MIN_WIDTH);
    let pad = width - inner.chars().count();
    println!("\n{}", format!("╭{}╮", "─".repeat(width)).bright_black());
    println!(
        "{}{}{}{}",
        "│".bright_black(),
        inner.bold(),
        " ".repeat(pad),
        "│".bright_black()
    );
    println!("{}", format!("╰{}╯", "─".repeat(width)).bright_black());
}

/// Bold section label (used by doctor/stats/gain).
pub fn section(name: &str) {
    println!("{}", name.bold());
}

/// Aligned `key: value` row.
pub fn kv(key: &str, value: &str) {
    println!("  {:<22} {}", format!("{key}:").dimmed(), value);
}

/// Green-bulleted advisory.
pub fn tip(msg: &str) {
    println!("  {} {}", "•".green(), msg);
}

/// Yellow-flagged warning.
pub fn warn(msg: &str) {
    println!("  {} {}", "!".yellow(), msg.yellow());
}

/// The one and only progress/proportion bar: solid `█` over `░`, clamped to
/// `[0,1]`. Callers wrap in `[..]` or recolor as they like. Replaces the former
/// `reduction_bar` (`█/░`) and `mini_bar` (`▓/░`) so the scheme never forks again.
pub fn bar(frac: f64, width: usize) -> String {
    let frac = frac.clamp(0.0, 1.0);
    let filled = (frac * width as f64).round() as usize;
    let filled = filled.min(width);
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

/// Thousands-separated integer (`1,234,567`), negative-aware.
pub fn format_num(n: i64) -> String {
    if n < 0 {
        return format!("-{}", format_num(-n));
    }
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result.chars().rev().collect()
}

/// Rounded-border aligned table backed by `tabled`. `right` lists the column
/// indices to right-align (numeric columns); all others stay left. Cell strings
/// may carry ANSI color — the `ansi` feature measures display width correctly so
/// colored cells don't skew alignment. Returns the rendered block (no print).
pub fn table(headers: &[&str], rows: &[Vec<String>], right: &[usize]) -> String {
    let mut builder = Builder::default();
    builder.push_record(headers.iter().map(|h| h.to_string()));
    for row in rows {
        builder.push_record(row.iter().cloned());
    }
    let mut t = builder.build();
    t.with(Style::rounded());
    for &col in right {
        t.with(Modify::new(Columns::one(col)).with(Alignment::right()));
    }
    t.to_string()
}

/// Print a `table()` block indented two spaces to sit under section headers.
pub fn print_table(headers: &[&str], rows: &[Vec<String>], right: &[usize]) {
    for line in table(headers, rows, right).lines() {
        println!("  {line}");
    }
}
