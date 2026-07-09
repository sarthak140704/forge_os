//! Google Gemini adapter — `generativelanguage.googleapis.com`
//!
//! Google's REST API takes the API key as a query-string param (`?key=`),
//! uses `user` / `model` role names (not `assistant`), and puts the system
//! prompt in a separate `systemInstruction` field.
//!
//! We normalize all three so callers see the same `ChatMessage` shape as
//! every other provider.

use crate::{CompletionRequest, CompletionResponse, LlmError, LlmProvider, ProviderHealth};
use async_trait::async_trait;
use serde::Deserialize;
use std::time::Instant;

pub struct GeminiProvider {
    api_key: String,
    base: String,
    client: reqwest::Client,
}

impl GeminiProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            base: "https://generativelanguage.googleapis.com/v1beta".to_string(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .expect("reqwest client"),
        }
    }

    pub fn with_base(mut self, base: impl Into<String>) -> Self { self.base = base.into(); self }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GmResponse {
    #[serde(default)] candidates:     Vec<GmCandidate>,
    #[serde(default)] usage_metadata: Option<GmUsage>,
    #[serde(default)] error:          Option<GmError>,
    #[serde(default)] model_version:  Option<String>,
}
#[derive(Deserialize)]
struct GmCandidate {
    #[serde(default)] content: Option<GmContent>,
}
#[derive(Deserialize)]
struct GmContent {
    #[serde(default)] parts: Vec<GmPart>,
    #[serde(default, rename = "role")] _role:  Option<String>,
}
#[derive(Deserialize)]
struct GmPart {
    #[serde(default)] text: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GmUsage {
    #[serde(default)] prompt_token_count:     usize,
    #[serde(default)] candidates_token_count: usize,
}
#[derive(Deserialize)]
struct GmError {
    #[serde(default)] message: String,
    #[serde(default)] status:  Option<String>,
}

#[async_trait]
impl LlmProvider for GeminiProvider {
    fn name(&self) -> &str { "gemini" }

    async fn health(&self) -> ProviderHealth {
        // Cheap check: list models with the API key.
        let url = format!("{}/models?key={}", self.base, self.api_key);
        match self.client.get(url)
            .timeout(std::time::Duration::from_secs(5))
            .send().await
        {
            Ok(r) if r.status().is_success() => ProviderHealth::Up,
            Ok(_) => ProviderHealth::Degraded,
            Err(_) => ProviderHealth::Down,
        }
    }

    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let start = Instant::now();

        // Gemini uses `user` / `model` (not `assistant`) and puts system
        // prompts in a separate top-level field. Hoist + rename.
        let mut system: Vec<String> = Vec::new();
        let mut contents: Vec<serde_json::Value> = Vec::with_capacity(req.messages.len());
        for m in &req.messages {
            match m.role.as_str() {
                "system" => system.push(m.content.clone()),
                "user" => contents.push(serde_json::json!({
                    "role": "user",
                    "parts": [{"text": m.content}],
                })),
                "assistant" | "model" => contents.push(serde_json::json!({
                    "role": "model",
                    "parts": [{"text": m.content}],
                })),
                other => {
                    tracing::warn!(role = %other, "gemini: unknown role, coerced to user");
                    contents.push(serde_json::json!({
                        "role": "user",
                        "parts": [{"text": m.content}],
                    }));
                }
            }
        }
        if req.json_mode {
            system.push(
                "Respond with a single valid JSON object and no prose or code fences."
                    .to_string(),
            );
        }

        let mut body = serde_json::json!({ "contents": contents });
        if !system.is_empty() {
            body["systemInstruction"] = serde_json::json!({
                "parts": [{"text": system.join("\n\n")}],
            });
        }
        let mut gen = serde_json::Map::new();
        if let Some(t) = req.temperature { gen.insert("temperature".into(), serde_json::json!(t)); }
        if let Some(m) = req.max_tokens  { gen.insert("maxOutputTokens".into(), serde_json::json!(m)); }
        if req.json_mode { gen.insert("responseMimeType".into(), serde_json::json!("application/json")); }
        if !gen.is_empty() { body["generationConfig"] = serde_json::Value::Object(gen); }

        let url = format!("{}/models/{}:generateContent?key={}", self.base, req.model, self.api_key);
        let resp = self.client.post(url).json(&body).send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(LlmError::Provider(format!("HTTP {}: {}", status, text)));
        }
        let parsed: GmResponse = serde_json::from_str(&text)
            .map_err(|e| LlmError::InvalidResponse(format!("{e}: {text}")))?;
        if let Some(err) = parsed.error {
            let status = err.status.unwrap_or_default();
            return Err(LlmError::Provider(format!("{status}: {}", err.message)));
        }
        let content: String = parsed.candidates
            .into_iter()
            .filter_map(|c| c.content)
            .flat_map(|c| c.parts)
            .filter_map(|p| p.text)
            .collect::<Vec<_>>()
            .join("");
        if content.is_empty() {
            return Err(LlmError::InvalidResponse("empty response content".into()));
        }
        let usage = parsed.usage_metadata.unwrap_or(GmUsage {
            prompt_token_count: 0,
            candidates_token_count: 0,
        });
        Ok(CompletionResponse {
            content,
            prompt_tokens:     usage.prompt_token_count,
            completion_tokens: usage.candidates_token_count,
            provider: "gemini".to_string(),
            model:    parsed.model_version.unwrap_or(req.model),
            latency_ms: start.elapsed().as_millis() as u64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_response() {
        let raw = r#"{
          "candidates":[{"content":{"parts":[{"text":"hi there"}],"role":"model"},"finishReason":"STOP"}],
          "usageMetadata":{"promptTokenCount":8,"candidatesTokenCount":2,"totalTokenCount":10},
          "modelVersion":"gemini-1.5-flash-002"
        }"#;
        let r: GmResponse = serde_json::from_str(raw).unwrap();
        let s: String = r.candidates.into_iter()
            .filter_map(|c| c.content).flat_map(|c| c.parts)
            .filter_map(|p| p.text).collect::<Vec<_>>().join("");
        assert_eq!(s, "hi there");
        let u = r.usage_metadata.unwrap();
        assert_eq!(u.prompt_token_count, 8);
        assert_eq!(u.candidates_token_count, 2);
        assert_eq!(r.model_version.unwrap(), "gemini-1.5-flash-002");
    }

    #[test]
    fn parses_error_response() {
        let raw = r#"{"error":{"code":401,"message":"bad key","status":"UNAUTHENTICATED"}}"#;
        let r: GmResponse = serde_json::from_str(raw).unwrap();
        let e = r.error.unwrap();
        assert_eq!(e.message, "bad key");
        assert_eq!(e.status.unwrap(), "UNAUTHENTICATED");
    }
}
