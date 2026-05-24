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
    Ok(tokenix_home()?.join(format!(
        "{}.preferences.md",
        crate::store::project_id(repo_root)
    )))
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

pub fn preferences_for_context(repo_root: &Path, max_items: usize) -> Result<String> {
    let mut lines = Vec::new();
    for path in [
        global_preferences_path()?,
        project_preferences_path(repo_root)?,
    ] {
        let content = fs::read_to_string(path).unwrap_or_default();
        lines.extend(extract_preference_lines(&content));
        if lines.len() >= max_items {
            break;
        }
    }
    lines.truncate(max_items);
    Ok(lines.join("\n"))
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
    fn rejects_sensitive_preferences() {
        let err = reject_sensitive_preference("use api_key abc for tests").unwrap_err();
        assert!(err.to_string().contains("sensitive"));
    }
}
