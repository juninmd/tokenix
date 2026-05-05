use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: &'a str,
}

#[derive(Deserialize)]
struct EmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

pub fn get_embedding(text: &str, model: &str, base_url: &str) -> Result<Vec<f32>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let resp = client
        .post(format!("{}/api/embed", base_url))
        .json(&EmbedRequest { model, input: text })
        .send()?;
    if !resp.status().is_success() {
        return Err(anyhow!("Ollama embed error: {}", resp.status()));
    }
    let data: EmbedResponse = resp.json()?;
    data.embeddings
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("Empty embeddings response"))
}

pub fn check_ollama(model: &str, base_url: &str) -> Result<()> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()?;
    let resp = client.get(format!("{}/api/tags", base_url)).send()?;
    if !resp.status().is_success() {
        return Err(anyhow!("Ollama not responding: {}", resp.status()));
    }
    let data: serde_json::Value = resp.json()?;
    let models = data["models"].as_array().cloned().unwrap_or_default();
    let model_base = model.split(':').next().unwrap_or(model);
    let found = models
        .iter()
        .any(|m| m["name"].as_str().unwrap_or("").contains(model_base));
    if !found {
        return Err(anyhow!(
            "Model '{}' not found in Ollama. Run: ollama pull {}",
            model,
            model
        ));
    }
    Ok(())
}
