//! Ollama embeddings adapter (Phase 6a).
//!
//! `POST {base}/api/embeddings` with `{model, prompt}` → `{embedding: [f32]}`.
//! Local, free, and works on any machine with `ollama serve` running.
//! Default model `nomic-embed-text` (dim 768).
//!
//! Users can pick other models (e.g. `mxbai-embed-large` at dim 1024). The
//! constructor takes an explicit `dim` because the runtime uses it to
//! pre-filter rows in `org_memory` before cosine ranking.

use crate::{EmbeddingProvider, LlmError};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct OllamaEmbeddingProvider {
    name:  String,
    base:  String,
    model: String,
    dim:   usize,
    http:  Client,
}

impl OllamaEmbeddingProvider {
    /// Convenience constructor for `nomic-embed-text` (dim 768).
    pub fn nomic(base: Option<String>) -> Self {
        Self::new(
            base.unwrap_or_else(|| "http://127.0.0.1:11434".to_string()),
            "nomic-embed-text".to_string(),
            768,
        )
    }
    pub fn new(base: String, model: String, dim: usize) -> Self {
        let name = format!("ollama:{model}");
        Self { name, base, model, dim, http: Client::new() }
    }
}

#[derive(Serialize)]
struct EmbeddingReq<'a> {
    model:  &'a str,
    prompt: &'a str,
}

#[derive(Deserialize)]
struct EmbeddingResp {
    embedding: Vec<f32>,
}

#[async_trait]
impl EmbeddingProvider for OllamaEmbeddingProvider {
    fn name(&self) -> &str { &self.name }
    fn dim(&self) -> usize { self.dim }
    async fn embed(&self, text: &str) -> Result<Vec<f32>, LlmError> {
        let url = format!("{}/api/embeddings", self.base.trim_end_matches('/'));
        let body = EmbeddingReq { model: &self.model, prompt: text };
        let resp = self.http.post(&url).json(&body).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(LlmError::Provider(format!("ollama embed http {status}: {body}")));
        }
        let parsed: EmbeddingResp = resp.json().await.map_err(|e|
            LlmError::InvalidResponse(format!("ollama embed decode: {e}"))
        )?;
        if parsed.embedding.len() != self.dim {
            return Err(LlmError::InvalidResponse(format!(
                "ollama embed: expected dim {} got {}", self.dim, parsed.embedding.len()
            )));
        }
        Ok(parsed.embedding)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_embedding_response() {
        let raw = r#"{"embedding":[0.5,0.25,-0.125]}"#;
        let p: EmbeddingResp = serde_json::from_str(raw).unwrap();
        assert_eq!(p.embedding, vec![0.5_f32, 0.25, -0.125]);
    }
}
