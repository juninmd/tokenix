//! `block_caps`: bound the detail of one class of multi-line block while leaving
//! every other line alone. rustc prints each warning as a header, a location and
//! a source snippet; the header and location carry the signal, the snippet is
//! most of the bytes. Errors use the same shape but keep their snippets, which is
//! why this is scoped by a start pattern instead of stripping snippet lines.

use regex::Regex;
use serde::Deserialize;

use super::cached_regex;

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct BlockCap {
    /// A line matching this opens a block (the line itself is always kept).
    pub start: String,
    /// A line matching this closes the block and is not part of it. Default: blank line.
    #[serde(default)]
    pub end: Option<String>,
    /// Lines of each block to keep, the start line included.
    pub max_lines: usize,
}

const DEFAULT_END: &str = r"^\s*$";

/// Runs before blank lines are stripped, since a blank line is the default terminator.
pub fn apply_block_caps(lines: Vec<String>, caps: &[BlockCap]) -> Vec<String> {
    let compiled: Vec<(Regex, Regex, usize)> = caps
        .iter()
        .filter_map(|c| {
            let start = cached_regex(&c.start)?;
            let end = cached_regex(c.end.as_deref().unwrap_or(DEFAULT_END))?;
            Some((start, end, c.max_lines.max(1)))
        })
        .collect();
    if compiled.is_empty() {
        return lines;
    }

    let mut out = Vec::with_capacity(lines.len());
    let (mut dropped, mut capped_blocks) = (0usize, 0usize);
    // (cap index, lines of the current block seen so far)
    let mut open: Option<(usize, usize)> = None;
    for line in lines {
        if let Some(idx) = compiled.iter().position(|(s, _, _)| s.is_match(&line)) {
            open = Some((idx, 1));
            out.push(line);
            continue;
        }
        if let Some((idx, seen)) = open {
            let (_, end, max) = &compiled[idx];
            if end.is_match(&line) {
                open = None;
            } else {
                open = Some((idx, seen + 1));
                if seen < *max {
                    out.push(line);
                } else {
                    dropped += 1;
                    if seen == *max {
                        capped_blocks += 1;
                    }
                }
                continue;
            }
        }
        out.push(line);
    }
    if dropped > 0 {
        out.push(format!(
            "[... {dropped} detail lines omitted from {capped_blocks} blocks ...]"
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(str::to_string).collect()
    }

    fn warn_cap() -> Vec<BlockCap> {
        vec![BlockCap {
            start: r"^warning(\[[\w-]+\])?: ".into(),
            end: Some(r"^(\s*$|[A-Za-z])".into()),
            max_lines: 2,
        }]
    }

    const RUSTC: &str = "warning: unused variable: `a`\n --> src/lib.rs:2:38\n  |\n2 | let a = x;\n  |     ^ help: prefix it\n\nerror[E0425]: cannot find value `b`\n --> src/lib.rs:9:5\n  |\n9 |     b\n  |     ^ not found in this scope\n\nwarning: unused variable: `c`\n --> src/lib.rs:4:38\n  |\n4 | let c = x;\n";

    #[test]
    fn warnings_keep_header_and_location_errors_keep_everything() {
        let out = apply_block_caps(lines(RUSTC), &warn_cap()).join("\n");
        assert!(
            out.contains("warning: unused variable: `a`\n --> src/lib.rs:2:38\n\nerror"),
            "{out}"
        );
        assert!(
            out.contains("9 |     b\n  |     ^ not found in this scope"),
            "error snippet must survive: {out}"
        );
        assert!(
            out.contains(
                "`c`\n --> src/lib.rs:4:38\n[... 5 detail lines omitted from 2 blocks ...]"
            ),
            "{out}"
        );
        assert!(!out.contains("let a = x"), "{out}");
    }

    #[test]
    fn output_without_a_matching_block_is_untouched() {
        let raw = lines("   Compiling demo v0.1.0\n    Finished dev\n");
        assert_eq!(apply_block_caps(raw.clone(), &warn_cap()), raw);
    }

    #[test]
    fn a_block_at_the_cap_adds_no_marker() {
        let raw = lines("warning: x\n --> a.rs:1:1\n\n");
        assert_eq!(apply_block_caps(raw.clone(), &warn_cap()), raw);
    }
}
