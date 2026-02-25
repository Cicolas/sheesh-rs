pub mod context;
pub mod registry;
pub mod tool;
pub mod tools;
pub mod types;

// Convenience re-exports so users only need `use sheesh_mcp::*` or individual items.
pub use context::{CommandOutput, DirEntry, EntryKind, SshContext};
pub use registry::ToolRegistry;
pub use tool::Tool;
pub use types::{ParamType, ToolCall, ToolContent, ToolDef, ToolParam, ToolResult};
