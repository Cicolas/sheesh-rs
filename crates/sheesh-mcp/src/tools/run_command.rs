use crate::{
    context::SshContext,
    tool::Tool,
    types::{ParamType, ToolCall, ToolDef, ToolParam, ToolResult},
};

/// MCP tool: execute a shell command on the remote SSH session.
///
/// The host app is responsible for showing a confirmation dialog before
/// calling `Tool::call` — this struct only contains the pure execution logic.
pub struct RunCommandTool;

impl Tool for RunCommandTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "run_command".into(),
            description: "Execute a shell command on the remote SSH session and return its output."
                .into(),
            params: vec![
                ToolParam {
                    name: "command".into(),
                    description: "The shell command to execute.".into(),
                    ty: ParamType::String,
                    required: true,
                },
                ToolParam {
                    name: "description".into(),
                    description: "One-sentence explanation of what this command does (shown to the user in the confirmation dialog).".into(),
                    ty: ParamType::String,
                    required: false,
                },
            ],
        }
    }

    fn call(&self, call: &ToolCall, ctx: &dyn SshContext) -> anyhow::Result<ToolResult> {
        let command = call
            .arg_str("command")
            .ok_or_else(|| anyhow::anyhow!("missing required argument: command"))?;

        let output = ctx.execute(command)?;

        let text = if output.combined().is_empty() {
            format!("exit code {}", output.exit_code)
        } else {
            format!("exit code {}\n{}", output.exit_code, output.combined())
        };

        if output.succeeded() {
            Ok(ToolResult::ok(&call.id, text))
        } else {
            Ok(ToolResult::error(&call.id, text))
        }
    }
}
