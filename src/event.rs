/// Actions that can be emitted by any tab or the main event handler.
#[derive(Debug, Clone)]
pub enum Action {
    /// Quit the application
    Quit,
    /// Confirm / connect
    Confirm,
    /// Disconnect from current SSH session
    Disconnect,
    /// Send a command string to the terminal PTY (no trailing newline).
    SendToTerminal(String),
    /// Cancel an in-progress tool call and return to the user prompt.
    CancelToolCall,
    /// No-op
    None,
}
