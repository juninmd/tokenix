use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: &'a [String],
}

#[derive(Deserialize)]
struct EmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

const EMBED_TIMEOUT_SECS: u64 = 120;
pub const MAX_BATCH_SIZE: usize = 64;

/// Embed a batch of texts in a single Ollama request.
/// Automatically splits batches larger than MAX_BATCH_SIZE.
pub fn get_embeddings_batch(
    texts: &[String],
    model: &str,
    base_url: &str,
) -> Result<Vec<Vec<f32>>> {
    if texts.is_empty() {
        return Ok(vec![]);
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(EMBED_TIMEOUT_SECS))
        .build()?;

    let mut all: Vec<Vec<f32>> = Vec::with_capacity(texts.len());
    for batch in texts.chunks(MAX_BATCH_SIZE) {
        let resp = client
            .post(format!("{}/api/embed", base_url))
            .json(&EmbedRequest {
                model,
                input: batch,
            })
            .send()?;
        if !resp.status().is_success() {
            return Err(anyhow!("Ollama embed error: {}", resp.status()));
        }
        let data: EmbedResponse = resp.json()?;
        if data.embeddings.len() != batch.len() {
            return Err(anyhow!(
                "Expected {} embeddings, got {}",
                batch.len(),
                data.embeddings.len()
            ));
        }
        all.extend(data.embeddings);
    }
    Ok(all)
}

/// Single embedding (thin wrapper around batch for backward compat).
pub fn get_embedding(text: &str, model: &str, base_url: &str) -> Result<Vec<f32>> {
    get_embeddings_batch(&[text.to_string()], model, base_url)?
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
