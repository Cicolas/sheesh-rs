use anyhow::Result;
use serde_json::{json, Value};

/// All tool definitions in Anthropic's input_schema format.
/// Providers targeting other APIs (OpenAI, Ollama) should convert as needed.
pub fn all_tools() -> Value {
    json!([
        {
            "name": "run_command",
            "description": "Execute an arbitrary shell command on the user's remote SSH session. \
                             The user will be shown the command and must approve before it runs.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The exact shell command to execute." },
                    "description": { "type": "string", "description": "One-sentence plain-English explanation of what this command does." }
                },
                "required": ["command"]
            }
        },
        {
            "name": "system_information",
            "description": "Return the SSH connection settings for the current session (host, user, port, description, identity file, extra options). No PTY interaction needed.",
            "input_schema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "make_dir",
            "description": "Create a directory (and any missing parents) on the remote host using mkdir -p.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute or relative path of the directory to create." }
                },
                "required": ["path"]
            }
        },
        {
            "name": "touch_file",
            "description": "Create an empty file (or update its timestamp) on the remote host using touch.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "file": { "type": "string", "description": "Path of the file to create or touch." }
                },
                "required": ["file"]
            }
        },
        {
            "name": "read_file",
            "description": "Read and return the contents of a file on the remote host using cat.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "file": { "type": "string", "description": "Path of the file to read." }
                },
                "required": ["file"]
            }
        },
        {
            "name": "list_dir",
            "description": "List the contents of a directory on the remote host using ls -la.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Directory path to list. Defaults to current directory." }
                },
                "required": []
            }
        },
        {
            "name": "read_terminal",
            "description": "Read the recent output from the user's terminal. Returns the last lines of captured terminal output. Use this to understand what is currently happening in the SSH session.",
            "input_schema": { "type": "object", "properties": {}, "required": [] }
        }
    ])
}

/// Wrap a path/filename in single quotes, escaping any embedded single quotes.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Provider-agnostic result of dispatching a tool call by name.
/// The caller (LLM provider) maps this to its own event type and appends
/// any provider-specific history blocks before forwarding upstream.
pub enum ToolResult {
    /// Tool is resolved locally by the application (no PTY needed).
    Local { id: String, name: String },
    /// Tool maps to a shell command that should be run on the PTY.
    Command { id: String, command: String, description: Option<String> },
}

/// Dispatch a tool call by `name` + `input` JSON to a [`ToolResult`].
pub fn dispatch(id: impl Into<String>, name: impl Into<String>, input: &Value) -> Result<ToolResult> {
    let id = id.into();
    let name = name.into();

    match name.as_str() {
        "system_information" | "read_terminal" => {
            log::debug!("[sheesh-tools] local tool: {}", name);
            Ok(ToolResult::Local { id, name })
        }
        "run_command" => {
            let command = input["command"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("run_command missing 'command' field"))?
                .to_string();
            let description = input["description"].as_str().map(|s| s.to_string());
            log::debug!("[sheesh-tools] run_command command={:?}", command);
            Ok(ToolResult::Command { id, command, description })
        }
        "make_dir" => {
            let path = input["path"].as_str().unwrap_or(".");
            let command = format!("mkdir -p {}", shell_quote(path));
            let description = Some(format!("Create directory {}", path));
            log::debug!("[sheesh-tools] make_dir path={:?}", path);
            Ok(ToolResult::Command { id, command, description })
        }
        "touch_file" => {
            let file = input["file"].as_str().unwrap_or("");
            let command = format!("touch {}", shell_quote(file));
            let description = Some(format!("Create/touch file {}", file));
            log::debug!("[sheesh-tools] touch_file file={:?}", file);
            Ok(ToolResult::Command { id, command, description })
        }
        "read_file" => {
            let file = input["file"].as_str().unwrap_or("");
            let command = format!("cat {}", shell_quote(file));
            let description = Some(format!("Read file {}", file));
            log::debug!("[sheesh-tools] read_file file={:?}", file);
            Ok(ToolResult::Command { id, command, description })
        }
        "list_dir" => {
            let path = input["path"].as_str().unwrap_or(".");
            let command = format!("ls -la {}", shell_quote(path));
            let description = Some(format!("List directory {}", path));
            log::debug!("[sheesh-tools] list_dir path={:?}", path);
            Ok(ToolResult::Command { id, command, description })
        }
        other => Err(anyhow::anyhow!("unknown tool: {}", other)),
    }
}