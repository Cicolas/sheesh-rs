use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

/// Actions that can be emitted by any tab or the main event handler.
#[derive(Debug, Clone)]
pub enum Action {
    /// Quit the application
    Quit,
    /// Move focus to the next panel
    NextPanel,
    /// Move focus to the previous panel
    PrevPanel,
    /// Navigate the listing cursor down
    Down,
    /// Navigate the listing cursor up
    Up,
    /// Confirm / connect
    Confirm,
    /// Open add-connection form
    Add,
    /// Open edit-connection form
    Edit,
    /// Delete selected connection
    Delete,
    /// Start filtering the list
    Filter,
    /// Send terminal context to the LLM
    SendContext,
    /// Disconnect from current SSH session
    Disconnect,
    /// Toggle the help overlay
    Help,
    /// A raw input character (for text fields / terminal passthrough)
    Input(char),
    /// Backspace in a text field
    Backspace,
    /// Enter key in a text field
    Enter,
    /// Escape / cancel
    Escape,
    /// No-op
    None,
}

pub fn map_event(event: &Event) -> Action {
    match event {
        Event::Key(KeyEvent {
            code, modifiers, ..
        }) => match code {
            KeyCode::Char('q') if modifiers.is_empty() => Action::Quit,
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => Action::Quit,
            KeyCode::Tab => Action::NextPanel,
            KeyCode::BackTab => Action::PrevPanel,
            KeyCode::Char('j') | KeyCode::Down => Action::Down,
            KeyCode::Char('k') | KeyCode::Up => Action::Up,
            KeyCode::Enter => Action::Enter,
            KeyCode::Char('a') if modifiers.is_empty() => Action::Add,
            KeyCode::Char('e') if modifiers.is_empty() => Action::Edit,
            KeyCode::Char('d') if modifiers.is_empty() => Action::Delete,
            KeyCode::Char('/') => Action::Filter,
            KeyCode::Char('c') if modifiers.is_empty() => Action::SendContext,
            KeyCode::Char('?') => Action::Help,
            KeyCode::Backspace => Action::Backspace,
            KeyCode::Esc => Action::Escape,
            KeyCode::Char(ch) => Action::Input(*ch),
            _ => Action::None,
        },
        _ => Action::None,
    }
}
