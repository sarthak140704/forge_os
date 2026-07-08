//! Deterministic mock provider — used by unit + integration tests so we
//! don't depend on external HTTP for CI.

use crate::{CompletionRequest, CompletionResponse, LlmError, LlmProvider, ProviderHealth};
use async_trait::async_trait;
use parking_lot::Mutex;

pub struct MockProvider {
    canned: Mutex<Vec<Result<String, String>>>,
    name: String,
}

impl MockProvider {
    pub fn new(name: impl Into<String>, responses: Vec<Result<String, String>>) -> Self {
        Self { canned: Mutex::new(responses), name: name.into() }
    }
}

#[async_trait]
impl LlmProvider for MockProvider {
    fn name(&self) -> &str { &self.name }
    async fn health(&self) -> ProviderHealth { ProviderHealth::Up }
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let next = self.canned.lock().pop().unwrap_or_else(|| Ok("{}".to_string()));
        match next {
            Ok(content) => Ok(CompletionResponse {
                content,
                prompt_tokens: 0, completion_tokens: 0,
                provider: self.name.clone(),
                model: req.model,
                latency_ms: 0,
            }),
            Err(e) => Err(LlmError::Provider(e)),
        }
    }
}
