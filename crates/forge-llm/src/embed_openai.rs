//! OpenAI embeddings adapter (Phase 6a).
//!
//! `POST {base}/embeddings` with `{model, input}` → `{data: [{embedding: [f32]}]}`.
//! Compatible with the `/v1/embeddings` endpoint at `api.openai.com` and any
//! drop-in replacement that mirrors the same schema.
//!
//! Default model `text-embedding-3-small` → dim **1536**. Users can override
//! the model (e.g. `text-embedding-3-large` → dim 3072) but MUST also override
//! the reported `dim()` because the mission service pre-filters rows by
//! embedding dimension for cosine ranking.

use crate::{EmbeddingProvider, LlmError};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct OpenAiEmbeddingProvider {
    name:    String,
    api_key: String,
    base:    String,
    model:   String,
    dim:     usize,
    http:    Client,
}

impl OpenAiEmbeddingProvider {
    /// Convenience constructor for `text-embedding-3-small` (dim 1536).
    pub fn small(api_key: impl Into<String>, base: Option<String>) -> Self {
        Self::new(
            api_key.into(),
            base.unwrap_or_else(|| "https://api.openai.com/v1".to_string()),
            "text-embedding-3-small".to_string(),
            1536,
        )
    }
    /// Convenience constructor for `text-embedding-3-large` (dim 3072).
    pub fn large(api_key: impl Into<String>, base: Option<String>) -> Self {
        Self::new(
            api_key.into(),
            base.unwrap_or_else(|| "https://api.openai.com/v1".to_string()),
            "text-embedding-3-large".to_string(),
            3072,
        )
    }
    /// General constructor. Set `dim` to the vector length the model
    /// returns — the mission service will filter rows on this value.
    pub fn new(api_key: String, base: String, model: String, dim: usize) -> Self {
        let name = format!("openai:{model}");
        Self { name, api_key, base, model, dim, http: Client::new() }
    }
}

#[derive(Serialize)]
struct EmbeddingReq<'a> {
    model: &'a str,
    input: &'a str,
}

#[derive(Deserialize)]
struct EmbeddingResp {
    data: Vec<EmbeddingItem>,
}
#[derive(Deserialize)]
struct EmbeddingItem {
    embedding: Vec<f32>,
}

#[async_trait]
impl EmbeddingProvider for OpenAiEmbeddingProvider {
    fn name(&self) -> &str { &self.name }
    fn dim(&self) -> usize { self.dim }
    async fn embed(&self, text: &str) -> Result<Vec<f32>, LlmError> {
        let url = format!("{}/embeddings", self.base.trim_end_matches('/'));
        let body = EmbeddingReq { model: &self.model, input: text };
        let resp = self.http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(LlmError::Provider(format!("openai embed http {status}: {body}")));
        }
        let parsed: EmbeddingResp = resp.json().await.map_err(|e|
            LlmError::InvalidResponse(format!("openai embed decode: {e}"))
        )?;
        let first = parsed.data.into_iter().next().ok_or_else(||
            LlmError::InvalidResponse("openai embed: empty data".into())
        )?;
        if first.embedding.len() != self.dim {
            return Err(LlmError::InvalidResponse(format!(
                "openai embed: expected dim {} got {}", self.dim, first.embedding.len()
            )));
        }
        Ok(first.embedding)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_embedding_response() {
        let raw = r#"{"data":[{"embedding":[0.1,-0.2,0.3]}]}"#;
        let p: EmbeddingResp = serde_json::from_str(raw).unwrap();
        assert_eq!(p.data[0].embedding, vec![0.1_f32, -0.2, 0.3]);
    }
}
