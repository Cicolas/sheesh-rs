use std::time::Duration;

use anyhow::{Context, Result};
use log::{debug, error, warn};
use serde_json::{json, Value};

use super::{ContentBlock, LLMEvent, LLMProvider, Message, RichMessage, Role};
use sheesh_tools::{ToolResult, all_tools, dispatch};

const RETRY_DELAYS: &[Duration] = &[
    Duration::from_millis(500),
    Duration::from_millis(2000),
    Duration::from_millis(4000),
];

pub struct AnthropicProvider {
    api_key: String,
    model: String,
}

impl AnthropicProvider {
    pub fn new(api_key: String, model: String) -> Self {
        Self { api_key, model }
    }

    fn post(&self, body: Value) -> Result<Value> {
        debug!("[Anthropic] POST /v1/messages model={} messages={}", self.model, body["messages"].as_array().map(|a| a.len()).unwrap_or(0));

        let client = reqwest::blocking::Client::new();
        let mut last_err: anyhow::Error = anyhow::anyhow!("no attempts made");

        for attempt in 0..=RETRY_DELAYS.len() {
            if attempt > 0 {
                let delay = RETRY_DELAYS[attempt - 1];
                warn!("[Anthropic] retry {}/{} after {}ms", attempt, RETRY_DELAYS.len(), delay.as_millis());
                std::thread::sleep(delay);
            }

            let resp = match client
                .post("https://api.anthropic.com/v1/messages")
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .json(&body)
                .send()
            {
                Ok(r) => r,
                Err(e) => {
                    warn!("[Anthropic] request error (attempt {}): {}", attempt + 1, e);
                    last_err = anyhow::Error::from(e).context("sending request to Anthropic");
                    continue;
                }
            };

            let status = resp.status();
            debug!("[Anthropic] response status={}", status);

            let json: Value = match resp.json().context("parsing Anthropic response") {
                Ok(v) => v,
                Err(e) => {
                    warn!("[Anthropic] parse error (attempt {}): {}", attempt + 1, e);
                    last_err = e;
                    continue;
                }
            };

            if status.is_server_error() || status.as_u16() == 429 {
                error!("[Anthropic] retryable error response (attempt {}): {}", attempt + 1, json);
                last_err = anyhow::anyhow!("Anthropic error {}: {}", status, json);
                continue;
            }

            if !status.is_success() {
                error!("[Anthropic] error response: {}", json);
            }

            return Ok(json);
        }

        Err(last_err)
    }
}

/// Convert a `RichMessage` to the JSON format Anthropic expects.
fn rich_to_json(m: &RichMessage) -> Value {
    let role = match m.role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "user",
    };

    // If there's a single Text block we can use the shorthand string form.
    if m.content.len() == 1
        && let ContentBlock::Text { text } = &m.content[0]
    {
        return json!({ "role": role, "content": text });
    }

    let blocks: Vec<Value> = m
        .content
        .iter()
        .map(|c| match c {
            ContentBlock::Text { text } => json!({ "type": "text", "text": text }),
            ContentBlock::ToolUse { id, name, input } => json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input,
            }),
            ContentBlock::ToolResult { tool_use_id, content } => json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": content,
            }),
        })
        .collect();

    json!({ "role": role, "content": blocks })
}

impl LLMProvider for AnthropicProvider {
    fn name(&self) -> &str {
        "Anthropic"
    }

    fn complete(&self, messages: &[Message]) -> Result<String> {
        debug!("[Anthropic] complete: {} message(s)", messages.len());

        let mut system: Option<String> = None;
        let mut msgs = vec![];

        for m in messages {
            if m.role == Role::System {
                system = Some(m.content.clone());
            } else {
                msgs.push(json!({
                    "role": match m.role { Role::User => "user", Role::Assistant => "assistant", Role::System => unreachable!() },
                    "content": m.content,
                }));
            }
        }

        let mut body = json!({
            "model": self.model,
            "max_tokens": 8096,
            "messages": msgs,
        });

        if let Some(s) = system {
            body["system"] = json!(s);
        }

        let body = self.post(body)?;

        let text = body["content"][0]["text"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("unexpected Anthropic response: {}", body))?;

        debug!("[Anthropic] complete: response {} chars", text.len());
        Ok(text)
    }

    fn complete_rich(&self, messages: &[RichMessage]) -> Result<LLMEvent> {
        debug!("[Anthropic] complete_rich: {} message(s)", messages.len());

        let mut system: Option<String> = None;
        let mut msgs = vec![];

        for m in messages {
            if m.role == Role::System {
                // Combine multiple system messages if they exist (though usually there's only one).
                let text: String = m
                    .content
                    .iter()
                    .filter_map(|c| if let ContentBlock::Text { text } = c { Some(text.as_str()) } else { None })
                    .collect::<Vec<_>>()
                    .join("\n");
                
                if let Some(ref mut existing) = system {
                    existing.push('\n');
                    existing.push_str(&text);
                } else {
                    system = Some(text);
                }
            } else {
                msgs.push(rich_to_json(m));
            }
        }

        let mut body = json!({
            "model": self.model,
            "max_tokens": 8096,
            "tools": all_tools(),
            "messages": msgs,
        });

        if let Some(s) = system {
            body["system"] = json!(s);
        }

        let body = self.post(body)?;

        let stop_reason = body["stop_reason"].as_str().unwrap_or("");
        debug!("[Anthropic] complete_rich: stop_reason={}", stop_reason);
        let content = body["content"].as_array().cloned().unwrap_or_default();

        if stop_reason == "tool_use" {
            // Find the tool_use block.
            let tool_use = content
                .iter()
                .find(|b| b["type"] == "tool_use")
                .ok_or_else(|| anyhow::anyhow!("tool_use stop but no tool_use block"))?;

            let id = tool_use["id"].as_str().unwrap_or("").to_string();
            let name = tool_use["name"].as_str().unwrap_or("").to_string();
            let input = tool_use["input"].clone();

            // Build the content blocks to append to rich history.
            let mut assistant_blocks: Vec<ContentBlock> = vec![];
            for block in &content {
                match block["type"].as_str() {
                    Some("text") => {
                        if let Some(text) = block["text"].as_str()
                            && !text.is_empty()
                        {
                            assistant_blocks.push(ContentBlock::Text { text: text.to_string() });
                        }
                    }
                    Some("tool_use") => {
                        // Use each block's own id/name/input — not the outer `id`/`name`/`input`
                        // — so that multiple tool_use blocks in one response don't share the same id.
                        let block_id = block["id"].as_str().unwrap_or("").to_string();
                        let block_name = block["name"].as_str().unwrap_or("").to_string();
                        let block_input = block["input"].clone();
                        assistant_blocks.push(ContentBlock::ToolUse {
                            id: block_id,
                            name: block_name,
                            input: block_input,
                        });
                    }
                    _ => {}
                }
            }

            // Dispatch by tool name via shared sheesh-tools crate.
            return match dispatch(id, name, &input)? {
                ToolResult::Local { id, name } => Ok(LLMEvent::LocalTool { id, name, assistant_blocks }),
                ToolResult::Command { id, command, description } => Ok(LLMEvent::ToolCall { id, command, description, assistant_blocks }),
            };
        }

        // Normal text response.
        let text = content
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("");

        if text.is_empty() {
            error!("[Anthropic] complete_rich: empty text in response: {}", body);
            return Err(anyhow::anyhow!("unexpected Anthropic response: {}", body));
        }

        debug!("[Anthropic] complete_rich: response {} chars", text.len());
        Ok(LLMEvent::Response(text))
    }
}
