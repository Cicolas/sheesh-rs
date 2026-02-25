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
    System,
}

/// Simple message used for UI display.
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

    pub fn system(content: impl Into<String>) -> Self {
        Self { role: Role::System, content: content.into() }
    }
}

// ── Rich content (Anthropic tool-use format) ──────────────────────────────────

/// A single content block inside a rich API message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text { text: String },
    ToolUse { id: String, name: String, input: serde_json::Value },
    ToolResult { tool_use_id: String, content: String },
}

/// Full API message that supports multi-part content (text + tool calls).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RichMessage {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl RichMessage {
    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    pub fn assistant_text(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    pub fn tool_result(tool_use_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: tool_use_id.into(),
                content: content.into(),
            }],
        }
    }
}

// ── Events sent back from the background LLM thread ──────────────────────────

#[derive(Debug)]
pub enum LLMEvent {
    /// Full text response — conversation continues normally.
    Response(String),
    /// Claude wants to run a command; user must confirm before we resume.
    ToolCall {
        /// Tool-use block id — echoed back in the tool_result.
        id: String,
        /// The command Claude wants to execute.
        command: String,
        /// Optional one-line description Claude provided.
        description: Option<String>,
        /// Full assistant content blocks (text + tool_use) for rich history.
        assistant_blocks: Vec<ContentBlock>,
    },
    /// An error occurred.
    Error(String),
}

// ── Provider trait ────────────────────────────────────────────────────────────

pub trait LLMProvider: Send + Sync {
    fn name(&self) -> &str;

    /// Plain completion — used by providers without tool support.
    fn complete(&self, messages: &[Message]) -> Result<String>;

    /// Rich completion with tool definitions included in the request.
    /// Default implementation strips tool content and falls back to `complete`.
    fn complete_rich(&self, messages: &[RichMessage]) -> Result<LLMEvent> {
        let simple: Vec<Message> = messages
            .iter()
            .filter_map(|m| {
                let text: String = m
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        ContentBlock::ToolResult { content, .. } => Some(content.as_str()),
                        ContentBlock::ToolUse { .. } => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if text.trim().is_empty() {
                    return None;
                }
                Some(Message { role: m.role.clone(), content: text })
            })
            .collect();
        self.complete(&simple).map(LLMEvent::Response)
    }
}

// ── Configuration ─────────────────────────────────────────────────────────────

/// Default system prompt injected at the start of every LLM session.
/// Edit this constant to change Claude's persona and behaviour across the app.
pub const DEFAULT_SYSTEM_PROMPT: &str = "\
You are Sheesh, an expert SSH and Linux assistant embedded in a terminal manager. \
You help users understand and manage their remote SSH sessions. \
When the user shares terminal output, analyse it and provide clear, actionable guidance. \
Prefer concise answers; use shell code blocks for any commands you suggest. \
You can run commands directly on the user's remote session via the run_command tool — \
always explain what a command does before proposing to run it.";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LLMConfig {
    pub provider: String,
    pub model: String,
    /// API key stored directly in the config file (takes precedence over `api_key_env`).
    pub api_key: Option<String>,
    /// Name of the environment variable holding the API key (fallback when `api_key` is absent).
    pub api_key_env: String,
    pub ollama_host: String,
    pub ollama_model: String,
    pub system_prompt: Option<String>,
}

impl Default for LLMConfig {
    fn default() -> Self {
        Self {
            provider: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
            api_key: None,
            api_key_env: "ANTHROPIC_API_KEY".into(),
            ollama_host: "http://localhost:11434".into(),
            ollama_model: "llama3".into(),
            system_prompt: Some(DEFAULT_SYSTEM_PROMPT.into()),
        }
    }
}

pub fn build_provider(cfg: &LLMConfig) -> Arc<dyn LLMProvider> {
    let resolve_key = |cfg: &LLMConfig| -> String {
        if let Some(k) = cfg.api_key.as_deref().filter(|k| !k.is_empty()) {
            log::info!("[llm] using api_key from config file");
            return k.to_string();
        }
        match std::env::var(&cfg.api_key_env) {
            Ok(k) if !k.is_empty() => {
                log::info!("[llm] using api_key from env var ${}", cfg.api_key_env);
                k
            }
            _ => {
                log::warn!(
                    "[llm] API key not found — set api_key in ~/.config/sheesh/config.toml or export ${}",
                    cfg.api_key_env
                );
                String::new()
            }
        }
    };

    match cfg.provider.as_str() {
        "openai" => {
            Arc::new(openai::OpenAIProvider::new(resolve_key(cfg), cfg.model.clone()))
        }
        "ollama" => Arc::new(ollama::OllamaProvider::new(
            cfg.ollama_host.clone(),
            cfg.ollama_model.clone(),
        )),
        _ => {
            Arc::new(anthropic::AnthropicProvider::new(resolve_key(cfg), cfg.model.clone()))
        }
    }
}

// ── Background thread helpers ─────────────────────────────────────────────────

pub fn spawn_completion(
    provider: Arc<dyn LLMProvider>,
    messages: Vec<Message>,
    tx: Sender<LLMEvent>,
) {
    std::thread::spawn(move || {
        match provider.complete(&messages) {
            Ok(response) => { let _ = tx.send(LLMEvent::Response(response)); }
            Err(e) => { let _ = tx.send(LLMEvent::Error(e.to_string())); }
        }
    });
}

/// Like `spawn_completion` but uses the rich API path with tool support.
pub fn spawn_completion_rich(
    provider: Arc<dyn LLMProvider>,
    messages: Vec<RichMessage>,
    tx: Sender<LLMEvent>,
) {
    std::thread::spawn(move || {
        match provider.complete_rich(&messages) {
            Ok(event) => { let _ = tx.send(event); }
            Err(e) => { let _ = tx.send(LLMEvent::Error(e.to_string())); }
        }
    });
}
