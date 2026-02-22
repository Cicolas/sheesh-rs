use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::{LLMProvider, Message, Role};

pub struct OpenAIProvider {
    api_key: String,
    model: String,
}

impl OpenAIProvider {
    pub fn new(api_key: String, model: String) -> Self {
        Self { api_key, model }
    }
}

impl LLMProvider for OpenAIProvider {
    fn name(&self) -> &str {
        "OpenAI"
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
            .post("https://api.openai.com/v1/chat/completions")
            .bearer_auth(&self.api_key)
            .json(&json!({
                "model": self.model,
                "messages": msgs,
            }))
            .send()
            .context("sending request to OpenAI")?;

        let body: Value = resp.json().context("parsing OpenAI response")?;

        body["choices"][0]["message"]["content"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("unexpected OpenAI response: {}", body))
    }
}
