use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::Path;

#[derive(Debug, Deserialize, Default)]
pub struct ArtifactsConfig {
    pub artifacts: Vec<ArtifactEntry>,
}

#[derive(Debug, Deserialize)]
pub struct ArtifactEntry {
    pub name: String,
    pub path: String,
    #[serde(rename = "type", default)]
    pub artifact_type: String,
    pub description: Option<String>,
}

pub fn load_artifacts(repo_root: &Path) -> Result<ArtifactsConfig> {
    let path = repo_root.join(".tokenix").join("artifacts.json");
    // A missing artifacts file is a normal, common state (the feature is opt-in) —
    // degrade to an empty config so `artifacts list`/`show` report "none" instead
    // of erroring. A present-but-unreadable or malformed file still errors.
    if !path.exists() {
        return Ok(ArtifactsConfig::default());
    }
    let content = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read artifacts file at {}", path.display()))?;
    let config: ArtifactsConfig =
        serde_json::from_str(&content).context("Failed to parse artifacts.json")?;
    Ok(config)
}

pub fn read_artifact_content(repo_root: &Path, entry: &ArtifactEntry) -> Result<String> {
    // `artifacts.json` is repo data, so its `path` is untrusted input: an
    // absolute path or `..` chain would read anything the process can open.
    let rel = Path::new(&entry.path);
    if rel.components().any(|c| {
        matches!(
            c,
            std::path::Component::ParentDir
                | std::path::Component::Prefix(_)
                | std::path::Component::RootDir
        )
    }) {
        anyhow::bail!(
            "artifact '{}' has path {:?} — artifact paths must stay inside the repository",
            entry.name,
            entry.path
        );
    }
    let path = repo_root.join(rel);
    let content = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read artifact file at {}", path.display()))?;
    Ok(content)
}

pub fn list_artifacts(repo_root: &Path) -> Result<()> {
    let config = load_artifacts(repo_root)?;
    if config.artifacts.is_empty() {
        println!("No context artifacts defined in .tokenix/artifacts.json");
        return Ok(());
    }
    println!("Context artifacts ({}):", config.artifacts.len());
    for entry in &config.artifacts {
        let desc = entry.description.as_deref().unwrap_or("(no description)");
        println!("  {} [{type}] -> {path}  {desc}",
            entry.name,
            type = entry.artifact_type,
            path = entry.path,
            desc = desc,
        );
    }
    Ok(())
}

pub fn show_artifact(repo_root: &Path, name: &str) -> Result<()> {
    let config = load_artifacts(repo_root)?;
    let entry = config
        .artifacts
        .iter()
        .find(|a| a.name == name)
        .with_context(|| format!("Artifact '{name}' not found"))?;
    let content = read_artifact_content(repo_root, entry)?;
    println!("--- {} ---", entry.name);
    println!("{}", content);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_artifacts_missing_file_returns_empty() {
        // A repo with no .tokenix/artifacts.json must not error — `artifacts list`
        // should report "none", not "No such file or directory".
        let dir = std::env::temp_dir().join(format!("tokenix_artifacts_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let cfg = load_artifacts(&dir).expect("missing artifacts.json must degrade to empty");
        assert!(cfg.artifacts.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_artifacts_malformed_file_still_errors() {
        let dir =
            std::env::temp_dir().join(format!("tokenix_artifacts_bad_{}", std::process::id()));
        let _ = std::fs::create_dir_all(dir.join(".tokenix"));
        std::fs::write(dir.join(".tokenix").join("artifacts.json"), "{ not json").unwrap();
        assert!(load_artifacts(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
