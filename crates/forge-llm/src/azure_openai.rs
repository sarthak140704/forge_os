//! Azure OpenAI adapter (Phase 6g).
//!
//! Azure speaks the same chat-completions *body* as OpenAI, but the wire
//! contract differs in three ways:
//!   1. The model is the **deployment name**, embedded in the URL path —
//!      not the request body.
//!   2. Auth is the `api-key` header (NOT `Authorization: Bearer`).
//!   3. An `api-version` query parameter is required.
//!
//! URL shape:
//!   `{endpoint}/openai/deployments/{deployment}/chat/completions?api-version={api_version}`
//! where `{endpoint}` is e.g. `https://my-resource.openai.azure.com`.

use crate::{CompletionRequest, CompletionResponse, LlmError, LlmProvider, ProviderHealth};
use async_trait::async_trait;
use serde::Deserialize;
use std::time::Instant;

pub struct AzureOpenAiProvider {
    api_key:     String,
    /// Base resource endpoint, e.g. `https://my-resource.openai.azure.com`.
    /// Trailing slashes are trimmed at call time.
    endpoint:    String,
    /// The Azure *deployment* name — this is what actually selects the model.
    deployment:  String,
    api_version: String,
    client:      reqwest::Client,
}

impl AzureOpenAiProvider {
    pub fn new(api_key: String, endpoint: String, deployment: String) -> Self {
        Self {
            api_key,
            endpoint,
            deployment,
            api_version: default_api_version(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .expect("reqwest client"),
        }
    }

    pub fn with_api_version(mut self, v: impl Into<String>) -> Self { self.api_version = v.into(); self }

    fn chat_url(&self) -> String {
        format!(
            "{}/openai/deployments/{}/chat/completions?api-version={}",
            self.endpoint.trim_end_matches('/'),
            self.deployment,
            self.api_version,
        )
    }
}

/// Azure's stable GA api-version at time of writing. Overridable via config.
pub fn default_api_version() -> String { "2024-06-01".to_string() }

#[derive(Deserialize)]
struct AzResponse {
    #[serde(default)] choices: Vec<AzChoice>,
    #[serde(default)] usage: Option<AzUsage>,
    #[serde(default)] model: Option<String>,
    #[serde(default)] error: Option<AzError>,
}
#[derive(Deserialize)] struct AzChoice { message: AzMessage }
#[derive(Deserialize)] struct AzMessage { content: Option<String> }
#[derive(Deserialize)] struct AzUsage {
    #[serde(default)] prompt_tokens: usize,
    #[serde(default)] completion_tokens: usize,
}
#[derive(Deserialize)] struct AzError { message: String }

#[async_trait]
impl LlmProvider for AzureOpenAiProvider {
    fn name(&self) -> &str { "azure_openai" }

    async fn health(&self) -> ProviderHealth {
        // Azure exposes a models list on the resource endpoint.
        let url = format!(
            "{}/openai/models?api-version={}",
            self.endpoint.trim_end_matches('/'),
            self.api_version,
        );
        match self.client.get(url)
            .header("api-key", &self.api_key)
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
        // Azure ignores the body `model` field (deployment selects the model),
        // but including it keeps the payload OpenAI-shaped and harmless.
        let mut body = serde_json::json!({
            "messages": req.messages.iter().map(|m| serde_json::json!({
                "role": m.role, "content": m.content,
            })).collect::<Vec<_>>(),
        });
        if let Some(t) = req.temperature { body["temperature"] = serde_json::json!(t); }
        if let Some(m) = req.max_tokens  { body["max_tokens"]  = serde_json::json!(m); }
        if req.json_mode { body["response_format"] = serde_json::json!({ "type": "json_object" }); }

        let resp = self.client.post(self.chat_url())
            .header("api-key", &self.api_key)
            .json(&body)
            .send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(LlmError::Provider(format!("HTTP {}: {}", status, text)));
        }
        let parsed: AzResponse = serde_json::from_str(&text)
            .map_err(|e| LlmError::InvalidResponse(format!("{e}: {text}")))?;
        if let Some(err) = parsed.error { return Err(LlmError::Provider(err.message)); }
        let choice = parsed.choices.into_iter().next()
            .ok_or_else(|| LlmError::InvalidResponse("no choices".to_string()))?;
        let content = choice.message.content
            .ok_or_else(|| LlmError::InvalidResponse("empty message content".to_string()))?;
        let usage = parsed.usage.unwrap_or(AzUsage { prompt_tokens: 0, completion_tokens: 0 });
        Ok(CompletionResponse {
            content,
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            provider: "azure_openai".to_string(),
            // Report the deployment as the model — Azure omits it and it's
            // the most useful identifier for cost attribution.
            model: parsed.model.unwrap_or_else(|| self.deployment.clone()),
            latency_ms: start.elapsed().as_millis() as u64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_deployment_url() {
        let p = AzureOpenAiProvider::new(
            "k".into(),
            "https://my-res.openai.azure.com/".into(), // trailing slash on purpose
            "gpt4o-deploy".into(),
        );
        assert_eq!(
            p.chat_url(),
            "https://my-res.openai.azure.com/openai/deployments/gpt4o-deploy/chat/completions?api-version=2024-06-01"
        );
    }

    #[test]
    fn api_version_override() {
        let p = AzureOpenAiProvider::new("k".into(), "https://x.openai.azure.com".into(), "d".into())
            .with_api_version("2024-10-21");
        assert!(p.chat_url().ends_with("api-version=2024-10-21"));
    }

    #[test]
    fn parses_typical_response() {
        let raw = r#"{"choices":[{"message":{"content":"hi"}}],"usage":{"prompt_tokens":3,"completion_tokens":2},"model":"gpt-4o"}"#;
        let p: AzResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(p.choices[0].message.content.as_deref(), Some("hi"));
        assert_eq!(p.usage.as_ref().unwrap().prompt_tokens, 3);
    }

    #[test]
    fn parses_error_response() {
        let raw = r#"{"error":{"message":"deployment not found"}}"#;
        let p: AzResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(p.error.unwrap().message, "deployment not found");
    }
}
