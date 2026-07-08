//! Ollama adapter (https://ollama.com) — local model server, no API key needed.

use crate::{CompletionRequest, CompletionResponse, LlmError, LlmProvider, ProviderHealth};
use async_trait::async_trait;
use serde::Deserialize;
use std::time::Instant;

pub struct OllamaProvider {
    base: String,
    client: reqwest::Client,
}

impl OllamaProvider {
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(180))
                .build().expect("reqwest client"),
        }
    }
    pub fn default_local() -> Self { Self::new("http://127.0.0.1:11434") }
}

#[derive(Deserialize)]
struct OllamaResp {
    #[serde(default)] message: Option<OllamaMsg>,
    #[serde(default)] #[allow(dead_code)] done: bool,
    #[serde(default)] prompt_eval_count: Option<usize>,
    #[serde(default)] eval_count: Option<usize>,
    #[serde(default)] model: Option<String>,
    #[serde(default)] error: Option<String>,
}
#[derive(Deserialize)] struct OllamaMsg { content: String }

#[async_trait]
impl LlmProvider for OllamaProvider {
    fn name(&self) -> &str { "ollama" }

    async fn health(&self) -> ProviderHealth {
        let url = format!("{}/api/tags", self.base);
        match self.client.get(url).timeout(std::time::Duration::from_secs(3)).send().await {
            Ok(r) if r.status().is_success() => ProviderHealth::Up,
            Ok(_) => ProviderHealth::Degraded,
            Err(_) => ProviderHealth::Down,
        }
    }

    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let start = Instant::now();
        let mut body = serde_json::json!({
            "model": req.model,
            "stream": false,
            "messages": req.messages.iter().map(|m| serde_json::json!({
                "role": m.role, "content": m.content,
            })).collect::<Vec<_>>(),
        });
        if req.json_mode { body["format"] = serde_json::json!("json"); }
        if let Some(t) = req.temperature {
            body["options"] = serde_json::json!({ "temperature": t });
        }

        let url = format!("{}/api/chat", self.base);
        let resp = self.client.post(url).json(&body).send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(LlmError::Provider(format!("HTTP {}: {}", status, text)));
        }
        let parsed: OllamaResp = serde_json::from_str(&text)
            .map_err(|e| LlmError::InvalidResponse(format!("{e}: {text}")))?;
        if let Some(err) = parsed.error { return Err(LlmError::Provider(err)); }
        let msg = parsed.message.ok_or_else(|| LlmError::InvalidResponse("no message".to_string()))?;
        Ok(CompletionResponse {
            content: msg.content,
            prompt_tokens:     parsed.prompt_eval_count.unwrap_or(0),
            completion_tokens: parsed.eval_count.unwrap_or(0),
            provider: "ollama".to_string(),
            model: parsed.model.unwrap_or(req.model),
            latency_ms: start.elapsed().as_millis() as u64,
        })
    }
}
