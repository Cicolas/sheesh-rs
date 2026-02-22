use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::Paragraph,
};

use super::theme::Theme;

/// A (key, description) hint pair.
pub type KeyHint<'a> = (&'a str, &'a str);

/// Render a row of key hints at the bottom of `area`.
pub fn render_keybindings(frame: &mut Frame, area: Rect, hints: &[KeyHint]) {
    let mut spans: Vec<Span> = vec![];

    for (i, (key, desc)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", Theme::dimmed()));
        }
        spans.push(Span::styled(format!("[{}]", key), Theme::key_hint_key()));
        spans.push(Span::styled(format!(" {}", desc), Theme::key_hint_desc()));
    }

    let line = Line::from(spans);
    let para = Paragraph::new(line);
    frame.render_widget(para, area);
}
