use ratatui::style::{Color, Modifier, Style};

pub struct Theme;

impl Theme {
    pub fn title() -> Style {
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD)
    }

    pub fn highlight() -> Style {
        Style::default()
            .fg(Color::Rgb(0, 0, 0))
            .bg(Color::White)
            .add_modifier(Modifier::BOLD)
    }

    /// Active: panel is capturing input.
    pub fn selected_border() -> Style {
        Style::default().fg(Color::Green)
    }

    /// Selected in navigate mode: highlighted but not capturing input.
    pub fn navigate_border() -> Style {
        Style::default().fg(Color::Yellow)
    }

    pub fn normal_border() -> Style {
        Style::default().fg(Color::DarkGray)
    }

    pub fn key_hint_key() -> Style {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    }

    pub fn key_hint_desc() -> Style {
        Style::default().fg(Color::DarkGray)
    }

    pub fn error() -> Style {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    }

    pub fn label() -> Style {
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD)
    }

    pub fn value() -> Style {
        Style::default().fg(Color::White)
    }

    pub fn dimmed() -> Style {
        Style::default().fg(Color::DarkGray)
    }

    pub fn chat_user() -> Style {
        Style::default().fg(Color::Green)
    }

    pub fn md_code_block() -> Style {
        Style::default().fg(Color::Yellow)
    }

    pub fn md_code_inline() -> Style {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    }
}
