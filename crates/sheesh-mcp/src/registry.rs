use std::collections::HashMap;

use crate::{
    context::SshContext,
    tool::Tool,
    types::{ToolCall, ToolDef, ToolResult},
};

/// Holds all registered tools and dispatches calls to the right one.
#[derive(Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool. If a tool with the same name already exists it is replaced.
    pub fn register(&mut self, tool: impl Tool + 'static) {
        let name = tool.def().name.clone();
        self.tools.insert(name, Box::new(tool));
    }

    /// Retrieve a tool by name.
    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|b| b.as_ref())
    }

    /// All tool definitions — pass this to the LLM's `tools` parameter.
    pub fn defs(&self) -> Vec<ToolDef> {
        self.tools.values().map(|t| t.def()).collect()
    }

    /// Names of all registered tools.
    pub fn names(&self) -> Vec<&str> {
        self.tools.keys().map(|s| s.as_str()).collect()
    }

    /// Dispatch a `ToolCall` to the matching tool.
    ///
    /// Returns `ToolResult::error` if no tool with that name is registered,
    /// or if the tool itself returns an `Err`.
    pub fn dispatch(&self, call: &ToolCall, ctx: &dyn SshContext) -> ToolResult {
        let Some(tool) = self.get(&call.name) else {
            return ToolResult::error(
                &call.id,
                format!("unknown tool: {}", call.name),
            );
        };

        match tool.call(call, ctx) {
            Ok(result) => result,
            Err(e) => ToolResult::error(&call.id, e.to_string()),
        }
    }

    /// Dispatch a batch of calls, returning results in the same order.
    pub fn dispatch_all(
        &self,
        calls: &[ToolCall],
        ctx: &dyn SshContext,
    ) -> Vec<ToolResult> {
        calls.iter().map(|c| self.dispatch(c, ctx)).collect()
    }
}
