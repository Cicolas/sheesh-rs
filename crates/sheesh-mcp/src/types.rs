use serde::{Deserialize, Serialize};

// ── Parameter schema ──────────────────────────────────────────────────────────

/// Primitive types a tool parameter can have.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParamType {
    String,
    Integer,
    Boolean,
    Array,
    Object,
}

/// A single parameter in a tool's input schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolParam {
    pub name: String,
    pub description: String,
    #[serde(rename = "type")]
    pub ty: ParamType,
    pub required: bool,
}

// ── Tool definition ───────────────────────────────────────────────────────────

/// Static metadata that describes a tool to the LLM.
/// Maps to the `tools` array in the Anthropic / OpenAI API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub params: Vec<ToolParam>,
}

impl ToolDef {
    /// Render the parameter list as a JSON Schema `properties` object,
    /// which is the format expected by most LLM APIs.
    pub fn input_schema(&self) -> serde_json::Value {
        let properties: serde_json::Map<String, serde_json::Value> = self
            .params
            .iter()
            .map(|p| {
                let schema = serde_json::json!({
                    "type": p.ty,
                    "description": p.description,
                });
                (p.name.clone(), schema)
            })
            .collect();

        let required: Vec<&str> = self
            .params
            .iter()
            .filter(|p| p.required)
            .map(|p| p.name.as_str())
            .collect();

        serde_json::json!({
            "type": "object",
            "properties": properties,
            "required": required,
        })
    }
}

// ── Tool call (LLM → app) ─────────────────────────────────────────────────────

/// A request from the LLM to invoke a specific tool.
/// Matches the `tool_use` block in Anthropic responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Opaque ID from the LLM (echoed back in the result).
    pub id: String,
    pub name: String,
    /// Arbitrary JSON arguments as specified by the tool's input schema.
    pub arguments: serde_json::Value,
}

impl ToolCall {
    pub fn new(id: impl Into<String>, name: impl Into<String>, arguments: serde_json::Value) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }

    /// Convenience: get a string argument by key.
    pub fn arg_str(&self, key: &str) -> Option<&str> {
        self.arguments.get(key)?.as_str()
    }

    /// Convenience: get a bool argument by key.
    pub fn arg_bool(&self, key: &str) -> Option<bool> {
        self.arguments.get(key)?.as_bool()
    }

    /// Convenience: get an i64 argument by key.
    pub fn arg_int(&self, key: &str) -> Option<i64> {
        self.arguments.get(key)?.as_i64()
    }
}

// ── Tool result (app → LLM) ───────────────────────────────────────────────────

/// A content block inside a tool result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolContent {
    Text { text: String },
}

impl ToolContent {
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text { text: s.into() }
    }
}

/// The result of invoking a tool, sent back to the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// Echoes the `id` from the originating `ToolCall`.
    pub tool_call_id: String,
    pub content: Vec<ToolContent>,
    pub is_error: bool,
}

impl ToolResult {
    pub fn ok(tool_call_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            content: vec![ToolContent::text(text)],
            is_error: false,
        }
    }

    pub fn error(tool_call_id: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            content: vec![ToolContent::text(message)],
            is_error: true,
        }
    }
}
