use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::{LLMProvider, Message, Role};

pub struct AnthropicProvider {
    api_key: String,
    model: String,
}

impl AnthropicProvider {
    pub fn new(api_key: String, model: String) -> Self {
        Self { api_key, model }
    }
}

impl LLMProvider for AnthropicProvider {
    fn name(&self) -> &str {
        "Anthropic"
    }

    fn complete(&self, messages: &[Message]) -> Result<String> {
        let msgs: Vec<Value> = messages
            .iter()
            .map(|m| {
                json!({
                    "role": match m.role { Role::User => "user", Role::Assistant => "assistant" },
                    "content": m.content,
                })
            })
            .collect();

        let client = reqwest::blocking::Client::new();
        let resp = client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&json!({
                "model": self.model,
                "max_tokens": 1024,
                "messages": msgs,
            }))
            .send()
            .context("sending request to Anthropic")?;

        let body: Value = resp.json().context("parsing Anthropic response")?;

        body["content"][0]["text"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("unexpected Anthropic response: {}", body))
    }
}
