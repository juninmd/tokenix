use anyhow::{anyhow, Result};
use chrono::Utc;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreferenceScope {
    Global,
    Project,
}

pub fn global_preferences_path() -> Result<PathBuf> {
    Ok(tokenix_home()?.join("memory").join("preferences.md"))
}

pub fn project_preferences_path(repo_root: &Path) -> Result<PathBuf> {
    let name = repo_root
        .file_name()
        .map(|n| sanitize_repo_name(&n.to_string_lossy()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "repo".to_string());
    // Human-readable name + short project id keeps files browsable while staying
    // unique across same-named folders. Grouped under preferences/.
    Ok(tokenix_home()?.join("preferences").join(format!(
        "{}-{}.md",
        name,
        crate::store::project_id(repo_root)
    )))
}

/// Keep filesystem-safe, readable characters; collapse the rest to `_`.
fn sanitize_repo_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

pub fn add_preference(repo_root: &Path, scope: PreferenceScope, text: &str) -> Result<PathBuf> {
    let clean = normalize_preference_text(text)?;
    reject_sensitive_preference(&clean)?;

    let path = match scope {
        PreferenceScope::Global => global_preferences_path()?,
        PreferenceScope::Project => project_preferences_path(repo_root)?,
    };
    let header = match scope {
        PreferenceScope::Global => "# tokenix Preference Memory\n\n## Global Preferences\n\n",
        PreferenceScope::Project => "# tokenix Preference Memory\n\n## Project Preferences\n\n",
    };
    append_preference_to_file(
        &path,
        header,
        &clean,
        &Utc::now().format("%Y-%m-%d").to_string(),
    )?;
    Ok(path)
}

pub fn list_preferences(
    repo_root: &Path,
    include_global: bool,
    include_project: bool,
) -> Result<String> {
    let mut out = String::new();

    if include_global {
        out.push_str("## Global Preferences\n");
        append_scope_lines(&mut out, &global_preferences_path()?)?;
    }

    if include_project {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str("## Project Preferences\n");
        append_scope_lines(&mut out, &project_preferences_path(repo_root)?)?;
    }

    if out.trim().is_empty() {
        return Ok("No preferences saved.".to_string());
    }
    Ok(out.trim_end().to_string())
}

pub fn remove_preference(
    repo_root: &Path,
    scope: PreferenceScope,
    query: &str,
) -> Result<(PathBuf, usize)> {
    let path = scope_path(repo_root, scope)?;
    let removed = rewrite_preference_file(&path, |line| {
        if preference_matches(line, query) {
            None
        } else {
            Some(line.to_string())
        }
    })?;
    Ok((path, removed))
}

pub fn edit_preference(
    repo_root: &Path,
    scope: PreferenceScope,
    query: &str,
    replacement: &str,
) -> Result<(PathBuf, usize)> {
    let clean = normalize_preference_text(replacement)?;
    reject_sensitive_preference(&clean)?;

    let date = Utc::now().format("%Y-%m-%d").to_string();
    let path = scope_path(repo_root, scope)?;
    let mut changed = 0usize;
    rewrite_preference_file(&path, |line| {
        if preference_matches(line, query) {
            changed += 1;
            Some(format!("- [{}] {}", date, clean))
        } else {
            Some(line.to_string())
        }
    })?;
    Ok((path, changed))
}

pub fn preferences_for_context(repo_root: &Path, task: &str, max_items: usize) -> Result<String> {
    // Read BOTH scopes, project first. The old loop broke out as soon as the
    // global file alone filled `max_items`, so a user with 8+ global preferences
    // never had their repo-specific rules read at all — the most relevant scope
    // was the one systematically dropped.
    let mut lines = Vec::new();
    for path in [
        project_preferences_path(repo_root)?,
        global_preferences_path()?,
    ] {
        let content = fs::read_to_string(path).unwrap_or_default();
        lines.extend(extract_preference_lines(&content));
    }
    // Rank by embedding similarity to the task; fall back to keyword scoring when
    // the model is unavailable (e.g. not downloaded yet) so memory still works.
    if !rank_preferences_semantic(&mut lines, task) {
        rank_preference_lines(&mut lines, task);
    }
    lines.truncate(max_items);
    Ok(lines.join("\n"))
}

fn scope_path(repo_root: &Path, scope: PreferenceScope) -> Result<PathBuf> {
    match scope {
        PreferenceScope::Global => global_preferences_path(),
        PreferenceScope::Project => project_preferences_path(repo_root),
    }
}

fn tokenix_home() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("TOKENIX_HOME") {
        return Ok(PathBuf::from(path));
    }
    let home = dirs::home_dir().ok_or_else(|| anyhow!("Could not resolve home directory"))?;
    Ok(home.join(".tokenix"))
}

fn append_scope_lines(out: &mut String, path: &Path) -> Result<()> {
    let content = fs::read_to_string(path).unwrap_or_default();
    let lines = extract_preference_lines(&content);
    if lines.is_empty() {
        out.push_str("(empty)\n");
    } else {
        for line in lines {
            out.push_str(&line);
            out.push('\n');
        }
    }
    Ok(())
}

fn append_preference_to_file(path: &Path, header: &str, text: &str, date: &str) -> Result<()> {
    let mut content = fs::read_to_string(path).unwrap_or_else(|_| header.to_string());
    if !content.ends_with('\n') {
        content.push('\n');
    }

    let normalized = normalize_for_dedupe(text);
    if extract_preference_lines(&content)
        .iter()
        .any(|line| normalize_for_dedupe(line).contains(&normalized))
    {
        return Ok(());
    }

    content.push_str(&format!("- [{}] {}\n", date, text));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)?;
    Ok(())
}

fn rewrite_preference_file<F>(path: &Path, mut rewrite: F) -> Result<usize>
where
    F: FnMut(&str) -> Option<String>,
{
    let content = fs::read_to_string(path).unwrap_or_default();
    let mut changed = 0usize;
    let mut output = Vec::new();
    for line in content.lines() {
        if line.trim_start().starts_with("- ") {
            match rewrite(line.trim()) {
                Some(new_line) => {
                    if new_line != line {
                        changed += 1;
                    }
                    output.push(new_line);
                }
                None => changed += 1,
            }
        } else {
            output.push(line.to_string());
        }
    }
    if changed > 0 {
        fs::write(path, format!("{}\n", output.join("\n")))?;
    }
    Ok(changed)
}

fn extract_preference_lines(content: &str) -> Vec<String> {
    content
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("- "))
        .map(str::to_string)
        .collect()
}

fn normalize_preference_text(text: &str) -> Result<String> {
    let clean = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if clean.is_empty() {
        return Err(anyhow!("Preference text cannot be empty"));
    }
    Ok(clean)
}

fn normalize_for_dedupe(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn preference_matches(line: &str, query: &str) -> bool {
    let query = normalize_for_dedupe(query);
    !query.is_empty() && normalize_for_dedupe(line).contains(&query)
}

fn rank_preference_lines(lines: &mut [String], task: &str) {
    let terms: Vec<String> = normalize_for_dedupe(task)
        .split_whitespace()
        .filter(|term| term.len() >= 3)
        .map(str::to_string)
        .collect();
    if terms.is_empty() {
        return;
    }
    lines.sort_by(|a, b| {
        let score_a = preference_score(a, &terms);
        let score_b = preference_score(b, &terms);
        score_b.cmp(&score_a).then_with(|| a.cmp(b))
    });
}

fn preference_score(line: &str, terms: &[String]) -> usize {
    let normalized = normalize_for_dedupe(line);
    terms
        .iter()
        .filter(|term| normalized.contains(term.as_str()))
        .count()
}

/// Rank preference lines by embedding cosine similarity to the task. Returns
/// `false` (leaving order untouched) when embeddings are unavailable — e.g. the
/// model is not downloaded yet — so the caller can fall back to keyword ranking
/// and keep the graceful-fallback contract. The embedder is already warm on the
/// context/explore paths that call this, so the extra cost is negligible.
fn rank_preferences_semantic(lines: &mut [String], task: &str) -> bool {
    if lines.len() < 2 || task.trim().is_empty() {
        return false;
    }
    let task_vec = match crate::embed::embed_query(task) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let bodies: Vec<String> = lines.iter().map(|l| preference_body(l)).collect();
    let doc_vecs = match crate::embed::embed_documents(&bodies) {
        Ok(v) if v.len() == lines.len() => v,
        _ => return false,
    };
    let scores: Vec<f32> = doc_vecs.iter().map(|v| cosine(&task_vec, v)).collect();
    let mut order: Vec<usize> = (0..lines.len()).collect();
    order.sort_by(|&a, &b| {
        scores[b]
            .partial_cmp(&scores[a])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| lines[a].cmp(&lines[b]))
    });
    let reordered: Vec<String> = order.iter().map(|&i| lines[i].clone()).collect();
    lines.clone_from_slice(&reordered);
    true
}

/// Strip the "- [date] " markup so only the meaningful preference text is embedded.
fn preference_body(line: &str) -> String {
    let trimmed = line.trim_start_matches("- ").trim();
    if let Some(rest) = trimmed.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            return rest[end + 1..].trim().to_string();
        }
    }
    trimmed.to_string()
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

fn reject_sensitive_preference(text: &str) -> Result<()> {
    let lower = text.to_ascii_lowercase();
    let sensitive = [
        "api_key",
        "apikey",
        "access_token",
        "auth_token",
        "bearer ",
        "client_secret",
        "password",
        "private_key",
        "secret",
        "-----begin",
    ];
    if sensitive.iter().any(|needle| lower.contains(needle)) {
        return Err(anyhow!(
            "Preference looks sensitive; refusing to store secrets in memory"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_file(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("tokenix-{name}-{nonce}.md"))
    }

    #[test]
    fn append_preference_creates_markdown_and_dedupes() {
        let path = temp_file("memory");
        append_preference_to_file(&path, "# H\n\n", "Prefer Biome over ESLint", "2026-05-24")
            .unwrap();
        append_preference_to_file(&path, "# H\n\n", "Prefer Biome over ESLint", "2026-05-24")
            .unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("# H"));
        assert_eq!(extract_preference_lines(&content).len(), 1);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn project_preferences_path_is_readable_and_grouped() {
        let repo = Path::new("D:/Solutions/pessoal/tokenix");
        let path = project_preferences_path(repo).unwrap();
        assert_eq!(path.parent().unwrap().file_name().unwrap(), "preferences");
        let fname = path.file_name().unwrap().to_string_lossy();
        assert!(fname.starts_with("tokenix-"), "got {fname}");
        assert!(fname.ends_with(".md"));
    }

    #[test]
    fn sanitize_repo_name_collapses_unsafe_chars() {
        assert_eq!(sanitize_repo_name("my repo!@#"), "my_repo___");
        assert_eq!(sanitize_repo_name("ok-name_1.2"), "ok-name_1.2");
    }

    #[test]
    fn rejects_sensitive_preferences() {
        let err = reject_sensitive_preference("use api_key abc for tests").unwrap_err();
        assert!(err.to_string().contains("sensitive"));
    }

    #[test]
    fn preference_body_strips_markup() {
        assert_eq!(
            preference_body("- [2026-05-24] Prefer Biome over ESLint"),
            "Prefer Biome over ESLint"
        );
        assert_eq!(preference_body("- plain text"), "plain text");
    }

    #[test]
    fn cosine_identical_is_one_orthogonal_is_zero() {
        let v = [1.0f32, 2.0, 3.0];
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-6);
        assert_eq!(cosine(&[1.0, 0.0], &[0.0, 1.0]), 0.0);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }

    #[test]
    fn ranks_preferences_by_task_terms() {
        let mut lines = vec![
            "- [2026-05-24] Prefer cargo check for Rust validation".to_string(),
            "- [2026-05-24] Prefer Biome over ESLint".to_string(),
        ];
        rank_preference_lines(&mut lines, "migrate eslint to biome");
        assert!(lines[0].contains("Biome"));
    }

    #[test]
    fn rewrite_removes_matching_preference() {
        let path = temp_file("memory-remove");
        fs::write(
            &path,
            "# H\n\n- [2026-05-24] Prefer Biome over ESLint\n- [2026-05-24] Use cargo check\n",
        )
        .unwrap();

        let removed = rewrite_preference_file(&path, |line| {
            if preference_matches(line, "Biome") {
                None
            } else {
                Some(line.to_string())
            }
        })
        .unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(removed, 1);
        assert!(!content.contains("Biome"));
        assert!(content.contains("cargo check"));

        let _ = fs::remove_file(path);
    }
}
