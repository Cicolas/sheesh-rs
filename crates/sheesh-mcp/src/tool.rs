use crate::{
    context::SshContext,
    types::{ToolCall, ToolDef, ToolResult},
};

/// A tool that can be registered with the MCP registry and called by the LLM.
///
/// # Example
/// ```rust
/// struct EchoTool;
///
/// impl Tool for EchoTool {
///     fn def(&self) -> ToolDef {
///         ToolDef {
///             name: "echo".into(),
///             description: "Return the input unchanged.".into(),
///             params: vec![ToolParam { name: "text".into(), ... }],
///         }
///     }
///
///     fn call(&self, call: &ToolCall, _ctx: &dyn SshContext) -> anyhow::Result<ToolResult> {
///         let text = call.arg_str("text").unwrap_or("");
///         Ok(ToolResult::ok(&call.id, text))
///     }
/// }
/// ```
pub trait Tool: Send + Sync {
    /// Static metadata: name, description, parameter schema.
    fn def(&self) -> ToolDef;

    /// Invoke the tool. Receives the full call (including id and raw arguments)
    /// and a reference to the active SSH session.
    fn call(&self, call: &ToolCall, ctx: &dyn SshContext) -> anyhow::Result<ToolResult>;
}
