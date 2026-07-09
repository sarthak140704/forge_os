//! OpenAI adapter — https://api.openai.com/v1/chat/completions.
//!
//! Same request/response shape as Groq and OpenRouter (they all speak the
//! OpenAI chat-completions dialect). Kept separate so users can point at
//! `api.openai.com` with a native `sk-...` key without touching config.

use crate::{CompletionRequest, CompletionResponse, LlmError, LlmProvider, ProviderHealth};
use async_trait::async_trait;
use serde::Deserialize;
use std::time::Instant;

pub struct OpenAiProvider {
    api_key: String,
    base: String,
    organization: Option<String>,
    name: String,
    client: reqwest::Client,
}

impl OpenAiProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            base: "https://api.openai.com/v1".to_string(),
            organization: None,
            name: "openai".to_string(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .expect("reqwest client"),
        }
    }

    pub fn with_base(mut self, base: impl Into<String>) -> Self { self.base = base.into(); self }
    pub fn with_organization(mut self, org: impl Into<String>) -> Self { self.organization = Some(org.into()); self }
    /// Override the reported provider name. Used for OpenAI-compatible
    /// backends (LM Studio, vLLM) so telemetry attributes calls to the
    /// real backend rather than the generic "openai".
    pub fn with_name(mut self, name: impl Into<String>) -> Self { self.name = name.into(); self }
}

#[derive(Deserialize)]
struct OaResponse {
    choices: Vec<OaChoice>,
    #[serde(default)] usage: Option<OaUsage>,
    #[serde(default)] model: Option<String>,
    #[serde(default)] error: Option<OaError>,
}
#[derive(Deserialize)] struct OaChoice { message: OaMessage }
#[derive(Deserialize)] struct OaMessage { content: Option<String> }
#[derive(Deserialize)] struct OaUsage {
    #[serde(default)] prompt_tokens: usize,
    #[serde(default)] completion_tokens: usize,
}
#[derive(Deserialize)] struct OaError { message: String }

#[async_trait]
impl LlmProvider for OpenAiProvider {
    fn name(&self) -> &str { &self.name }

    async fn health(&self) -> ProviderHealth {
        let url = format!("{}/models", self.base);
        let mut req = self.client.get(url).bearer_auth(&self.api_key)
            .timeout(std::time::Duration::from_secs(5));
        if let Some(org) = &self.organization { req = req.header("OpenAI-Organization", org); }
        match req.send().await {
            Ok(r) if r.status().is_success() => ProviderHealth::Up,
            Ok(_) => ProviderHealth::Degraded,
            Err(_) => ProviderHealth::Down,
        }
    }

    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let start = Instant::now();
        let mut body = serde_json::json!({
            "model": req.model,
            "messages": req.messages.iter().map(|m| serde_json::json!({
                "role": m.role, "content": m.content,
            })).collect::<Vec<_>>(),
        });
        if let Some(t) = req.temperature { body["temperature"] = serde_json::json!(t); }
        if let Some(m) = req.max_tokens  { body["max_tokens"]  = serde_json::json!(m); }
        if req.json_mode { body["response_format"] = serde_json::json!({ "type": "json_object" }); }

        let url = format!("{}/chat/completions", self.base);
        let mut http = self.client.post(url).bearer_auth(&self.api_key);
        if let Some(org) = &self.organization { http = http.header("OpenAI-Organization", org); }
        let resp = http.json(&body).send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(LlmError::Provider(format!("HTTP {}: {}", status, text)));
        }
        let parsed: OaResponse = serde_json::from_str(&text)
            .map_err(|e| LlmError::InvalidResponse(format!("{e}: {text}")))?;
        if let Some(err) = parsed.error { return Err(LlmError::Provider(err.message)); }
        let choice = parsed.choices.into_iter().next()
            .ok_or_else(|| LlmError::InvalidResponse("no choices".to_string()))?;
        let content = choice.message.content
            .ok_or_else(|| LlmError::InvalidResponse("empty message content".to_string()))?;
        let usage = parsed.usage.unwrap_or(OaUsage { prompt_tokens: 0, completion_tokens: 0 });
        Ok(CompletionResponse {
            content,
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            provider: self.name.clone(),
            model: parsed.model.unwrap_or(req.model),
            latency_ms: start.elapsed().as_millis() as u64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_name_is_openai() {
        let p = OpenAiProvider::new("k".into());
        assert_eq!(p.name(), "openai");
    }

    #[test]
    fn with_name_overrides_for_compatible_backends() {
        let p = OpenAiProvider::new("not-needed".into())
            .with_base("http://localhost:1234/v1")
            .with_name("lmstudio");
        assert_eq!(p.name(), "lmstudio");
        assert_eq!(p.base, "http://localhost:1234/v1");
    }
}
