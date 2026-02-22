use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::{LLMProvider, Message, Role};

pub struct OllamaProvider {
    host: String,
    model: String,
}

impl OllamaProvider {
    pub fn new(host: String, model: String) -> Self {
        Self { host, model }
    }
}

impl LLMProvider for OllamaProvider {
    fn name(&self) -> &str {
        "Ollama"
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

        let url = format!("{}/api/chat", self.host.trim_end_matches('/'));

        let client = reqwest::blocking::Client::new();
        let resp = client
            .post(&url)
            .json(&json!({
                "model": self.model,
                "messages": msgs,
                "stream": false,
            }))
            .send()
            .context("sending request to Ollama")?;

        let body: Value = resp.json().context("parsing Ollama response")?;

        body["message"]["content"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("unexpected Ollama response: {}", body))
    }
}
