use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, mpsc::Sender};

pub mod anthropic;
pub mod ollama;
pub mod openai;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: Role::User, content: content.into() }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: Role::Assistant, content: content.into() }
    }
}

/// Events sent back from the background LLM thread.
#[derive(Debug)]
pub enum LLMEvent {
    /// Full response arrived.
    Response(String),
    /// An error occurred.
    Error(String),
}

pub trait LLMProvider: Send + Sync {
    fn name(&self) -> &str;
    /// Blocking call — run in a background thread.
    fn complete(&self, messages: &[Message]) -> Result<String>;
}

/// Configuration for the LLM provider (loaded from ~/.config/sheesh/config.toml).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LLMConfig {
    pub provider: String,
    pub model: String,
    /// Environment variable name that holds the API key.
    pub api_key_env: String,
    pub ollama_host: String,
    pub ollama_model: String,
}

impl Default for LLMConfig {
    fn default() -> Self {
        Self {
            provider: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
            api_key_env: "ANTHROPIC_API_KEY".into(),
            ollama_host: "http://localhost:11434".into(),
            ollama_model: "llama3".into(),
        }
    }
}

/// Build a provider from config.
pub fn build_provider(cfg: &LLMConfig) -> Arc<dyn LLMProvider> {
    match cfg.provider.as_str() {
        "openai" => {
            let key = std::env::var(&cfg.api_key_env).unwrap_or_default();
            Arc::new(openai::OpenAIProvider::new(key, cfg.model.clone()))
        }
        "ollama" => Arc::new(ollama::OllamaProvider::new(
            cfg.ollama_host.clone(),
            cfg.ollama_model.clone(),
        )),
        _ => {
            let key = std::env::var(&cfg.api_key_env).unwrap_or_default();
            Arc::new(anthropic::AnthropicProvider::new(key, cfg.model.clone()))
        }
    }
}

/// Spawn a background thread to run the LLM completion and send the result via `tx`.
pub fn spawn_completion(
    provider: std::sync::Arc<dyn LLMProvider>,
    messages: Vec<Message>,
    tx: Sender<LLMEvent>,
) {
    std::thread::spawn(move || {
        match provider.complete(&messages) {
            Ok(response) => {
                let _ = tx.send(LLMEvent::Response(response));
            }
            Err(e) => {
                let _ = tx.send(LLMEvent::Error(e.to_string()));
            }
        }
    });
}
