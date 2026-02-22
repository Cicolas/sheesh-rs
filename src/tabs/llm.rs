use std::sync::{Arc, mpsc};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Paragraph, Wrap},
};

use crate::{
    event::Action,
    llm::{LLMEvent, LLMProvider, Message, Role, spawn_completion},
    ui::theme::Theme,
};

use super::Tab;

/// (line_index, col) in the flattened history line buffer.
type BufPos = (usize, usize);

pub struct LLMTab {
    pub history: Vec<Message>,
    pub input: String,
    pub waiting: bool,
    pub status: String,
    provider: Arc<dyn LLMProvider>,
    tx: mpsc::Sender<LLMEvent>,
    pub rx: mpsc::Receiver<LLMEvent>,
    scroll_offset: usize,
    selection: Option<(BufPos, BufPos)>,
    last_render_start: usize,
    last_chat_area: Rect,
    clipboard: Option<arboard::Clipboard>,
}

impl LLMTab {
    pub fn new(provider: Arc<dyn LLMProvider>) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            history: vec![],
            input: String::new(),
            waiting: false,
            status: String::new(),
            provider,
            tx,
            rx,
            scroll_offset: 0,
            selection: None,
            last_render_start: 0,
            last_chat_area: Rect::default(),
            clipboard: arboard::Clipboard::new().ok(),
        }
    }

    /// Poll the channel for completed LLM responses. Call this each render frame.
    pub fn poll(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            self.waiting = false;
            match event {
                LLMEvent::Response(text) => {
                    self.status = "Response received.".into();
                    self.history.push(Message::assistant(text));
                    // Auto-scroll to bottom on new response
                    self.scroll_offset = 0;
                }
                LLMEvent::Error(err) => {
                    self.status = format!("Error: {}", err);
                    self.history.push(Message::assistant(format!("[error] {}", err)));
                    self.scroll_offset = 0;
                }
            }
        }
    }

    pub fn send_message(&mut self, content: String) {
        if content.trim().is_empty() || self.waiting {
            return;
        }
        self.status = "Sending message...".into();
        self.history.push(Message::user(content));
        self.waiting = true;
        self.scroll_offset = 0;
        self.status = "Waiting for response...".into();
        spawn_completion(
            Arc::clone(&self.provider),
            self.history.clone(),
            self.tx.clone(),
        );
    }

    /// Prepend terminal context and send.
    /// The chat shows a short summary; the full context is only sent to the API.
    pub fn send_with_context(&mut self, context: String, question: String) {
        if self.waiting { return; }

        let display = if question.trim().is_empty() {
            "[terminal context shared] What's happening here?".to_string()
        } else {
            format!("[terminal context shared] {}", question)
        };
        let api_content = if question.trim().is_empty() {
            format!("Terminal context:\n```\n{}\n```\n\nWhat's happening here?", context)
        } else {
            format!("Terminal context:\n```\n{}\n```\n\n{}", context, question)
        };

        self.status = "Waiting for response...".into();
        self.history.push(Message::user(display));
        self.waiting = true;
        self.scroll_offset = 0;

        // Build API messages: history as-is except replace the last entry with the full context.
        let mut api_messages = self.history.clone();
        if let Some(last) = api_messages.last_mut() {
            last.content = api_content;
        }

        spawn_completion(Arc::clone(&self.provider), api_messages, self.tx.clone());
    }

    /// Build the flat list of rendered lines from the message history.
    fn build_lines(&self) -> Vec<(String, Option<Style>)> {
        let mut all: Vec<(String, Option<Style>)> = vec![];
        for msg in &self.history {
            let (prefix, style) = match msg.role {
                Role::User      => ("You: ",    Style::default().fg(Color::Green)),
                Role::Assistant => ("Claude: ", Style::default().fg(Color::Rgb(205, 115, 80))),
            };
            for (i, line) in msg.content.lines().enumerate() {
                if i == 0 {
                    all.push((format!("{}{}", prefix, line), Some(style)));
                } else {
                    all.push((format!("      {}", line), None));
                }
            }
            all.push((String::new(), None));
        }
        all
    }

    fn scroll_up(&mut self) {
        self.scroll_offset += 3;
    }

    fn scroll_down(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(3);
    }

    fn screen_to_buf(&self, col: u16, row: u16) -> Option<BufPos> {
        let area = self.last_chat_area;
        if row < area.y || row >= area.y + area.height { return None; }
        if col < area.x { return None; }
        let buf_line = self.last_render_start + (row - area.y) as usize;
        let buf_col  = (col - area.x) as usize;
        Some((buf_line, buf_col))
    }

    fn selection_range(&self) -> Option<(BufPos, BufPos)> {
        let (a, b) = self.selection?;
        if a.0 < b.0 || (a.0 == b.0 && a.1 <= b.1) { Some((a, b)) } else { Some((b, a)) }
    }

    fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selection_range()?;
        let lines = self.build_lines();
        if start.0 >= lines.len() { return None; }
        let end_line = end.0.min(lines.len() - 1);
        let mut out = String::new();
        for li in start.0..=end_line {
            let line = &lines[li].0;
            let from = if li == start.0 { start.1.min(line.len()) } else { 0 };
            let to   = if li == end_line { end.1.min(line.len()) } else { line.len() };
            out.push_str(&line[from..to]);
            if li < end_line { out.push('\n'); }
        }
        if out.is_empty() { None } else { Some(out) }
    }

    fn copy_selection(&mut self) {
        if let Some(text) = self.selected_text() {
            if let Some(ref mut cb) = self.clipboard {
                let _ = cb.set_text(text);
            }
        }
    }
}

impl Tab for LLMTab {
    fn title(&self) -> &str {
        "LLM"
    }

    fn key_hints(&self) -> Vec<(&str, &str)> {
        vec![
            ("enter", "send"),
            ("esc", "clear input"),
            ("ctrl+c", "copy selection"),
        ]
    }

    fn handle_event(&mut self, event: &Event) -> Action {
        match event {
            Event::Key(KeyEvent { code, modifiers, .. }) => {
                let ctrl = modifiers.contains(KeyModifiers::CONTROL);

                // Ctrl+C — copy selection if any
                if ctrl && *code == KeyCode::Char('c') {
                    if self.selection.is_some() {
                        self.copy_selection();
                        self.selection = None;
                    }
                    return Action::None;
                }

                // Scroll with Ctrl+Up/Down (same as terminal)
                if ctrl && *code == KeyCode::Up   { self.scroll_up();   return Action::None; }
                if ctrl && *code == KeyCode::Down { self.scroll_down(); return Action::None; }

                // Text input
                match code {
                    KeyCode::Enter => {
                        let msg = std::mem::take(&mut self.input);
                        self.send_message(msg);
                    }
                    KeyCode::Esc => {
                        self.input.clear();
                    }
                    KeyCode::Backspace => {
                        self.input.pop();
                    }
                    KeyCode::Char(ch)
                        if modifiers.is_empty() || modifiers.contains(KeyModifiers::SHIFT) =>
                    {
                        self.input.push(*ch);
                    }
                    _ => {}
                }
                Action::None
            }

            Event::Mouse(me) => {
                match me.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        self.selection = self
                            .screen_to_buf(me.column, me.row)
                            .map(|pos| (pos, pos));
                    }
                    MouseEventKind::Drag(MouseButton::Left) => {
                        if let Some((anchor, _)) = self.selection {
                            if let Some(cur) = self.screen_to_buf(me.column, me.row) {
                                self.selection = Some((anchor, cur));
                            }
                        }
                    }
                    MouseEventKind::Up(MouseButton::Left) => {
                        if let Some((a, b)) = self.selection {
                            if a == b { self.selection = None; }
                        }
                    }
                    MouseEventKind::ScrollUp   => self.scroll_up(),
                    MouseEventKind::ScrollDown => self.scroll_down(),
                    _ => {}
                }
                Action::None
            }

            _ => Action::None,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, focused: bool) {
        self.poll();

        let border_style = if focused {
            Theme::selected_border()
        } else {
            Theme::normal_border()
        };

        let provider_name = self.provider.name();
        let title = if self.waiting {
            Line::from(vec![
                Span::styled(format!(" LLM ({}) ", provider_name), Theme::title()),
                Span::styled(" thinking... ", Theme::dimmed()),
            ])
        } else {
            Line::from(Span::styled(
                format!(" LLM ({}) ", provider_name),
                Theme::title(),
            ))
        };

        let outer_block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(border_style)
            .title(title);

        let inner = outer_block.inner(area);
        frame.render_widget(outer_block, area);

        // Input grows with content (1–5 content lines + 2 border rows).
        let input_width = inner.width.saturating_sub(2) as usize;
        let content_lines = if input_width == 0 {
            1
        } else {
            ((self.input.len() + 1) + input_width - 1) / input_width
        };
        let input_height = (content_lines.max(1) + 2).min(7) as u16;

        let [chat_area, status_area, input_area] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(input_height),
        ])
        .areas(inner);

        self.last_chat_area = chat_area;
        self.render_history(frame, chat_area);
        self.render_status(frame, status_area);
        self.render_input(frame, input_area, focused);
    }
}

impl LLMTab {
    fn render_history(&mut self, frame: &mut Frame, area: Rect) {
        let all = self.build_lines();
        let total = all.len();
        let h = area.height as usize;
        let max_scroll = total.saturating_sub(h);
        self.scroll_offset = self.scroll_offset.min(max_scroll);
        let start = max_scroll - self.scroll_offset;
        self.last_render_start = start;

        let sel = self.selection_range();
        let sel_style = Style::default().bg(Color::White).fg(Color::Black);

        let visible: Vec<Line> = all
            .into_iter()
            .enumerate()
            .skip(start)
            .take(h)
            .map(|(li, (text, prefix_style))| {
                let Some((sel_start, sel_end)) = sel else {
                    return match prefix_style {
                        Some(s) => Line::from(Span::styled(text, s)),
                        None    => Line::from(Span::raw(text)),
                    };
                };

                if li < sel_start.0 || li > sel_end.0 {
                    return match prefix_style {
                        Some(s) => Line::from(Span::styled(text, s)),
                        None    => Line::from(Span::raw(text)),
                    };
                }

                let len = text.len();
                let from = if li == sel_start.0 { sel_start.1.min(len) } else { 0 };
                let to   = if li == sel_end.0   { sel_end.1.min(len)   } else { len };

                if from >= to {
                    return match prefix_style {
                        Some(s) => Line::from(Span::styled(text, s)),
                        None    => Line::from(Span::raw(text)),
                    };
                }

                // Build spans: before | selected | after
                // The prefix style only applies to the first segment if it precedes the selection.
                let mut spans: Vec<Span> = vec![];
                if from > 0 {
                    let seg = text[..from].to_string();
                    spans.push(match prefix_style {
                        Some(s) => Span::styled(seg, s),
                        None    => Span::raw(seg),
                    });
                }
                spans.push(Span::styled(text[from..to].to_string(), sel_style));
                if to < len {
                    spans.push(Span::raw(text[to..].to_string()));
                }
                Line::from(spans)
            })
            .collect();

        frame.render_widget(Paragraph::new(visible).wrap(Wrap { trim: false }), area);
    }

    fn render_status(&self, frame: &mut Frame, area: Rect) {
        let style = if self.waiting {
            Theme::dimmed()
        } else {
            Theme::key_hint_desc()
        };
        let line = Line::from(Span::styled(format!(" {}", self.status), style));
        frame.render_widget(Paragraph::new(line), area);
    }

    fn render_input(&self, frame: &mut Frame, area: Rect, focused: bool) {
        let border_style = if focused {
            Theme::selected_border()
        } else {
            Theme::normal_border()
        };

        let cursor = if focused { "_" } else { "" };
        let content = format!("{}{}", self.input, cursor);

        let para = Paragraph::new(content)
            .block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(border_style)
                    .title(Span::styled(" Message ", Theme::dimmed())),
            )
            .wrap(Wrap { trim: true });

        frame.render_widget(para, area);
    }
}
