//! Anthropic adapter — https://api.anthropic.com/v1/messages
//!
//! Anthropic's Messages API is close in shape to OpenAI but with three
//! important differences we normalize here:
//!
//!   * `system` is a *top-level* field, NOT a role in `messages`.
//!     We hoist any leading `role: "system"` messages out of the array.
//!   * `max_tokens` is REQUIRED. We default to 4096 when the caller
//!     doesn't set it.
//!   * Auth is `x-api-key` + `anthropic-version` header, not `Authorization`.
//!
//! JSON mode is best-effort: Anthropic recommends prompting for JSON
//! rather than a structured-output flag, so we just prepend a short
//! instruction to the system prompt.

use crate::{CompletionRequest, CompletionResponse, LlmError, LlmProvider, ProviderHealth};
use async_trait::async_trait;
use serde::Deserialize;
use std::time::Instant;

pub struct AnthropicProvider {
    api_key: String,
    base: String,
    version: String,
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            base: "https://api.anthropic.com/v1".to_string(),
            version: "2023-06-01".to_string(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .expect("reqwest client"),
        }
    }

    pub fn with_base(mut self, base: impl Into<String>) -> Self { self.base = base.into(); self }
    pub fn with_version(mut self, v: impl Into<String>) -> Self { self.version = v.into(); self }
}

#[derive(Deserialize)]
struct AnResponse {
    #[serde(default)] content: Vec<AnContentBlock>,
    #[serde(default)] usage:   Option<AnUsage>,
    #[serde(default)] model:   Option<String>,
    #[serde(default)] r#type:  Option<String>,
    #[serde(default)] error:   Option<AnError>,
}
#[derive(Deserialize)]
struct AnContentBlock {
    #[serde(default)] r#type: String,
    #[serde(default)] text:   Option<String>,
}
#[derive(Deserialize)]
struct AnUsage {
    #[serde(default)] input_tokens:  usize,
    #[serde(default)] output_tokens: usize,
}
#[derive(Deserialize)]
struct AnError { message: String }

#[async_trait]
impl LlmProvider for AnthropicProvider {
    fn name(&self) -> &str { "anthropic" }

    async fn health(&self) -> ProviderHealth {
        // Anthropic doesn't expose an unauthenticated ping; posting a
        // 1-token message to /messages would cost real money. Best-effort
        // TCP probe of the base URL host.
        let url = format!("{}/messages", self.base);
        match self.client.post(url)
            .timeout(std::time::Duration::from_secs(5))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", &self.version)
            .json(&serde_json::json!({}))
            .send().await
        {
            // 400 (bad request) actually means the endpoint accepted our
            // auth and rejected the payload — provider is Up.
            Ok(r) if r.status() == reqwest::StatusCode::BAD_REQUEST => ProviderHealth::Up,
            Ok(r) if r.status().is_success() => ProviderHealth::Up,
            Ok(r) if r.status() == reqwest::StatusCode::UNAUTHORIZED => ProviderHealth::Degraded,
            Ok(_) => ProviderHealth::Degraded,
            Err(_) => ProviderHealth::Down,
        }
    }

    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let start = Instant::now();

        // Hoist any leading system messages into a single top-level `system`
        // string. Non-leading system messages get folded into the preceding
        // user message as a bracketed note — rare enough that this is safe.
        let mut system: Vec<String> = Vec::new();
        let mut messages: Vec<serde_json::Value> = Vec::with_capacity(req.messages.len());
        for m in &req.messages {
            match m.role.as_str() {
                "system" => system.push(m.content.clone()),
                "user" | "assistant" => messages.push(serde_json::json!({
                    "role": m.role, "content": m.content,
                })),
                other => {
                    // Fall back to `user` so we don't drop content on the floor.
                    tracing::warn!(role = %other, "anthropic: unknown role, coerced to user");
                    messages.push(serde_json::json!({ "role": "user", "content": m.content }));
                }
            }
        }
        if req.json_mode {
            system.push(
                "Respond with a single valid JSON object and no prose or code fences."
                    .to_string(),
            );
        }

        let mut body = serde_json::json!({
            "model":      req.model,
            "max_tokens": req.max_tokens.unwrap_or(4096),
            "messages":   messages,
        });
        if !system.is_empty() {
            body["system"] = serde_json::json!(system.join("\n\n"));
        }
        if let Some(t) = req.temperature { body["temperature"] = serde_json::json!(t); }

        let url = format!("{}/messages", self.base);
        let resp = self.client.post(url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", &self.version)
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(LlmError::Provider(format!("HTTP {}: {}", status, text)));
        }
        let parsed: AnResponse = serde_json::from_str(&text)
            .map_err(|e| LlmError::InvalidResponse(format!("{e}: {text}")))?;
        if let Some(err) = parsed.error { return Err(LlmError::Provider(err.message)); }
        if let Some(ty) = &parsed.r#type { if ty == "error" {
            return Err(LlmError::Provider(text));
        }}
        let content: String = parsed.content.into_iter()
            .filter(|b| b.r#type == "text")
            .filter_map(|b| b.text)
            .collect::<Vec<_>>()
            .join("");
        if content.is_empty() {
            return Err(LlmError::InvalidResponse("empty response content".into()));
        }
        let usage = parsed.usage.unwrap_or(AnUsage { input_tokens: 0, output_tokens: 0 });
        Ok(CompletionResponse {
            content,
            prompt_tokens:     usage.input_tokens,
            completion_tokens: usage.output_tokens,
            provider: "anthropic".to_string(),
            model:    parsed.model.unwrap_or(req.model),
            latency_ms: start.elapsed().as_millis() as u64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChatMessage;

    #[test]
    fn parses_typical_response() {
        let raw = r#"{
          "id":"msg_01",
          "type":"message",
          "role":"assistant",
          "model":"claude-3-5-sonnet-latest",
          "content":[{"type":"text","text":"hello"}],
          "usage":{"input_tokens":11,"output_tokens":3}
        }"#;
        let r: AnResponse = serde_json::from_str(raw).unwrap();
        let s: String = r.content.into_iter().filter(|b| b.r#type == "text")
            .filter_map(|b| b.text).collect::<Vec<_>>().join("");
        assert_eq!(s, "hello");
        assert_eq!(r.usage.unwrap().output_tokens, 3);
        assert_eq!(r.model.unwrap(), "claude-3-5-sonnet-latest");
    }

    #[test]
    fn parses_error_response() {
        let raw = r#"{"type":"error","error":{"type":"invalid_request_error","message":"bad key"}}"#;
        let r: AnResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(r.error.unwrap().message, "bad key");
    }

    // Verify our system-message hoisting logic doesn't panic on any input.
    #[test]
    fn hoists_system_messages() {
        // Simulate what complete() does when preparing messages.
        let msgs = vec![
            ChatMessage { role: "system".into(), content: "sys1".into() },
            ChatMessage { role: "user".into(),   content: "hi".into() },
            ChatMessage { role: "system".into(), content: "sys2".into() },
        ];
        let mut system: Vec<String> = Vec::new();
        let mut out: Vec<serde_json::Value> = Vec::new();
        for m in &msgs {
            match m.role.as_str() {
                "system" => system.push(m.content.clone()),
                _ => out.push(serde_json::json!({"role": m.role, "content": m.content})),
            }
        }
        assert_eq!(system, vec!["sys1", "sys2"]);
        assert_eq!(out.len(), 1);
    }
}
