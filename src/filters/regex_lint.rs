//! Every regex a filter carries must compile. `cached_regex` skips an invalid
//! pattern with a stderr warning, so a bad rule silently disables itself and
//! the warning lands in front of the agent on every matching run.

use super::FilterDef;

/// One entry per pattern that fails to compile, as `field: pattern (error)`.
/// Used by `tokenix doctor` for user/local filters and by the bundled-corpus test.
pub fn regex_issues(f: &FilterDef) -> Vec<String> {
    let mut p: Vec<(&'static str, &str)> = vec![("match_command", &f.match_command)];
    p.extend(
        f.skip_when_matches
            .iter()
            .map(|s| ("skip_when_matches", s.as_str())),
    );
    p.extend(
        f.strip_lines_matching
            .iter()
            .map(|s| ("strip_lines_matching", s.as_str())),
    );
    p.extend(
        f.keep_lines_matching
            .iter()
            .map(|s| ("keep_lines_matching", s.as_str())),
    );
    for m in &f.match_output {
        p.push(("match_output.pattern", &m.pattern));
        p.extend(m.unless.iter().map(|s| ("match_output.unless", s.as_str())));
    }
    if let Some(u) = &f.uniform_success {
        p.push(("uniform_success.pattern", &u.pattern));
        p.extend(
            u.ignore_lines
                .iter()
                .map(|s| ("uniform_success.ignore_lines", s.as_str())),
        );
    }
    p.extend(
        f.replace_patterns
            .iter()
            .map(|[s, _]| ("replace_patterns", s.as_str())),
    );
    for s in &f.extract_sections {
        p.push(("extract_sections.start_pattern", &s.start_pattern));
        p.push(("extract_sections.end_pattern", &s.end_pattern));
    }
    if let Some(s) = &f.semantic_filter {
        p.extend(
            s.always_keep
                .iter()
                .map(|s| ("semantic_filter.always_keep", s.as_str())),
        );
    }
    if let Some(d) = f
        .deduplicate_blocks
        .as_ref()
        .and_then(|d| d.block_delimiter.as_ref())
    {
        p.push(("deduplicate_blocks.block_delimiter", d));
    }
    p.extend(
        f.priority_lines
            .iter()
            .map(|s| ("priority_lines", s.as_str())),
    );
    p.extend(
        f.category_caps
            .iter()
            .map(|c| ("category_caps.pattern", c.pattern.as_str())),
    );
    for b in &f.block_caps {
        p.push(("block_caps.start", &b.start));
        p.extend(b.end.iter().map(|s| ("block_caps.end", s.as_str())));
    }
    p.into_iter()
        .filter_map(|(field, pat)| {
            let err = regex::Regex::new(pat).err()?;
            let reason = err.to_string();
            let reason = reason.lines().last().unwrap_or_default().trim().to_string();
            Some(format!("{field}: {pat:?} does not compile ({reason})"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(toml_src: &str) -> FilterDef {
        toml::from_str(toml_src).expect("valid filter toml")
    }

    #[test]
    fn flags_lookaround_that_the_regex_crate_rejects() {
        let f = filter(
            "match_command = \"^git log\"\nreplace_patterns = [[\"[a-f0-9]{7}(?=\\\\s)\", \"x\"]]\n",
        );
        let issues = regex_issues(&f);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(issues[0].starts_with("replace_patterns:"), "{issues:?}");
    }

    #[test]
    fn a_valid_filter_has_no_issues() {
        let f = filter("match_command = \"^cargo\\\\s+build\"\nstrip_lines_matching = [\"^\\\\s*Compiling\"]\n");
        assert!(regex_issues(&f).is_empty());
    }

    #[test]
    fn every_bundled_filter_regex_compiles() {
        let broken: Vec<String> = crate::filters::load_bundled_filters_named()
            .iter()
            .flat_map(|(name, f)| {
                regex_issues(f)
                    .into_iter()
                    .map(move |i| format!("{name}: {i}"))
            })
            .collect();
        assert!(
            broken.is_empty(),
            "invalid regexes ship silently disabled:\n{}",
            broken.join("\n")
        );
    }
}
