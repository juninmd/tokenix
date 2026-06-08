use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::Path;

#[derive(Debug, Deserialize)]
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
    let content = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read artifacts file at {}", path.display()))?;
    let config: ArtifactsConfig =
        serde_json::from_str(&content).context("Failed to parse artifacts.json")?;
    Ok(config)
}

pub fn read_artifact_content(repo_root: &Path, entry: &ArtifactEntry) -> Result<String> {
    let path = repo_root.join(&entry.path);
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
