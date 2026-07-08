//! OpenRouter adapter (https://openrouter.ai) — OpenAI-compatible chat completions.

use crate::{ChatMessage, CompletionRequest, CompletionResponse, LlmError, LlmProvider, ProviderHealth};
use async_trait::async_trait;
use serde::Deserialize;
use std::time::Instant;

pub struct OpenRouterProvider {
    api_key: String,
    base: String,
    client: reqwest::Client,
}

impl OpenRouterProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            base: "https://openrouter.ai/api/v1".to_string(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build().expect("reqwest client"),
        }
    }

    pub fn with_base(mut self, base: impl Into<String>) -> Self { self.base = base.into(); self }
}

#[derive(Deserialize)]
struct OrResponse {
    choices: Vec<OrChoice>,
    #[serde(default)] usage: Option<OrUsage>,
    #[serde(default)] model: Option<String>,
    #[serde(default)] error: Option<OrError>,
}
#[derive(Deserialize)] struct OrChoice { message: OrMessage }
#[derive(Deserialize)] struct OrMessage { content: String }
#[derive(Deserialize)] struct OrUsage { #[serde(default)] prompt_tokens: usize, #[serde(default)] completion_tokens: usize }
#[derive(Deserialize)] struct OrError { message: String }

#[async_trait]
impl LlmProvider for OpenRouterProvider {
    fn name(&self) -> &str { "openrouter" }

    async fn health(&self) -> ProviderHealth {
        // Cheap probe: GET /models with a short timeout.
        let url = format!("{}/models", self.base);
        match self.client.get(url).bearer_auth(&self.api_key)
            .timeout(std::time::Duration::from_secs(5)).send().await {
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
        let resp = self.client.post(url)
            .bearer_auth(&self.api_key)
            .header("HTTP-Referer", "https://github.com/t-sarverma/forge-os")
            .header("X-Title", "Forge OS")
            .json(&body).send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(LlmError::Provider(format!("HTTP {}: {}", status, text)));
        }
        let parsed: OrResponse = serde_json::from_str(&text)
            .map_err(|e| LlmError::InvalidResponse(format!("{e}: {text}")))?;
        if let Some(err) = parsed.error {
            return Err(LlmError::Provider(err.message));
        }
        let choice = parsed.choices.into_iter().next()
            .ok_or_else(|| LlmError::InvalidResponse("no choices".to_string()))?;
        let usage = parsed.usage.unwrap_or(OrUsage { prompt_tokens: 0, completion_tokens: 0 });
        Ok(CompletionResponse {
            content: choice.message.content,
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            provider: "openrouter".to_string(),
            model: parsed.model.unwrap_or(req.model),
            latency_ms: start.elapsed().as_millis() as u64,
        })
    }
}

// keeps ChatMessage in scope for docs
#[allow(dead_code)]
fn _refs(_: ChatMessage) {}
