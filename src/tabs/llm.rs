use std::sync::{Arc, mpsc};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
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
                    self.history
                        .push(Message::assistant(format!("[error] {}", err)));
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
        if self.waiting {
            return;
        }

        let display = if question.trim().is_empty() {
            "[terminal context shared] What's happening here?".to_string()
        } else {
            format!("[terminal context shared] {}", question)
        };
        let api_content = if question.trim().is_empty() {
            format!(
                "Terminal context:\n```\n{}\n```\n\nWhat's happening here?",
                context
            )
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
                Role::User => ("You: ", Theme::chat_user()),
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
        if row < area.y || row >= area.y + area.height {
            return None;
        }
        if col < area.x {
            return None;
        }
        let buf_line = self.last_render_start + (row - area.y) as usize;
        let buf_col = (col - area.x) as usize;
        Some((buf_line, buf_col))
    }

    fn selection_range(&self) -> Option<(BufPos, BufPos)> {
        let (a, b) = self.selection?;
        if a.0 < b.0 || (a.0 == b.0 && a.1 <= b.1) {
            Some((a, b))
        } else {
            Some((b, a))
        }
    }

    fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selection_range()?;
        let lines = self.build_lines();
        if start.0 >= lines.len() {
            return None;
        }
        let end_line = end.0.min(lines.len() - 1);
        let mut out = String::new();
        for li in start.0..=end_line {
            let line = &lines[li].0;
            let from = if li == start.0 {
                start.1.min(line.len())
            } else {
                0
            };
            let to = if li == end_line {
                end.1.min(line.len())
            } else {
                line.len()
            };
            out.push_str(&line[from..to]);
            if li < end_line {
                out.push('\n');
            }
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
            ("alt+enter", "newline"),
            ("esc", "clear input"),
            ("ctrl+c", "copy selection"),
        ]
    }

    fn handle_event(&mut self, event: &Event) -> Action {
        match event {
            Event::Key(KeyEvent {
                code, modifiers, ..
            }) => {
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
                if ctrl && *code == KeyCode::Up {
                    self.scroll_up();
                    return Action::None;
                }
                if ctrl && *code == KeyCode::Down {
                    self.scroll_down();
                    return Action::None;
                }

                // Text input
                match code {
                    KeyCode::Enter => {
                        if modifiers.contains(KeyModifiers::ALT) {
                            self.input.push('\n');
                        } else {
                            let msg = std::mem::take(&mut self.input);
                            self.send_message(msg);
                        }
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
                        self.selection =
                            self.screen_to_buf(me.column, me.row).map(|pos| (pos, pos));
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
                            if a == b {
                                self.selection = None;
                            }
                        }
                    }
                    MouseEventKind::ScrollUp => self.scroll_up(),
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
        // Count both explicit newlines and wrapped lines.
        let input_width = inner.width.saturating_sub(2) as usize;
        let content_lines: usize = if input_width == 0 {
            1
        } else {
            self.input
                .split('\n')
                .map(|l| ((l.len() + 1) + input_width - 1) / input_width)
                .sum::<usize>()
                .max(1)
        };
        let input_height = (content_lines + 2).min(7) as u16;

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

        // Pre-compute which lines fall inside a markdown code block.
        let in_code: Vec<bool> = {
            let mut flags = Vec::with_capacity(all.len());
            let mut in_block = false;
            for (text, _) in &all {
                let content = line_content(text);
                if content.trim_start().starts_with("```") {
                    in_block = !in_block;
                    flags.push(true); // fence lines rendered as code
                } else {
                    flags.push(in_block);
                }
            }
            flags
        };

        let visible: Vec<Line> = all
            .into_iter()
            .enumerate()
            .skip(start)
            .take(h)
            .map(|(li, (text, prefix_style))| {
                let Some((sel_start, sel_end)) = sel else {
                    return render_md_line(&text, in_code[li]);
                };

                if li < sel_start.0 || li > sel_end.0 {
                    return render_md_line(&text, in_code[li]);
                }

                // Selection overlay — fall back to plain rendering for this line.
                let len = text.len();
                let from = if li == sel_start.0 { sel_start.1.min(len) } else { 0 };
                let to   = if li == sel_end.0   { sel_end.1.min(len)   } else { len };

                if from >= to {
                    return render_md_line(&text, in_code[li]);
                }

                let mut spans: Vec<Span> = vec![];
                if from > 0 {
                    let seg = text[..from].to_string();
                    spans.push(match prefix_style {
                        Some(s) => Span::styled(seg, s),
                        None => Span::raw(seg),
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

// ── Markdown rendering helpers ────────────────────────────────────────────────

/// Strip the role prefix / indent from a line to get the raw content.
fn line_content(text: &str) -> &str {
    if let Some(rest) = text.strip_prefix("You: ") { rest }
    else if let Some(rest) = text.strip_prefix("Claude: ") { rest }
    else if let Some(rest) = text.strip_prefix("      ") { rest }
    else { text }
}

/// Render a single history line with markdown styling applied.
/// `in_code` means the line falls inside a fenced code block.
fn render_md_line(full_text: &str, in_code: bool) -> Line<'static> {
    if full_text.is_empty() {
        return Line::raw("");
    }

    // Split prefix (role label / indent) from content.
    let (prefix_str, prefix_style, content): (&str, Option<Style>, &str) =
        if let Some(rest) = full_text.strip_prefix("You: ") {
            ("You: ", Some(Theme::chat_user()), rest)
        } else if let Some(rest) = full_text.strip_prefix("Claude: ") {
            ("Claude: ", Some(Style::default().fg(Color::Rgb(205, 115, 80))), rest)
        } else if let Some(rest) = full_text.strip_prefix("      ") {
            ("      ", None, rest)
        } else {
            ("", None, full_text)
        };

    let mut spans: Vec<Span<'static>> = Vec::new();
    if !prefix_str.is_empty() {
        match prefix_style {
            Some(s) => spans.push(Span::styled(prefix_str.to_string(), s)),
            None    => spans.push(Span::raw(prefix_str.to_string())),
        }
    }

    // Code block lines: render as-is with code style.
    if in_code {
        spans.push(Span::styled(content.to_string(), Theme::md_code_block()));
        return Line::from(spans);
    }

    // Headings (line-level).
    if let Some(rest) = content.strip_prefix("### ") {
        spans.push(Span::styled(
            format!("### {}", rest),
            Style::default().add_modifier(Modifier::BOLD),
        ));
    } else if let Some(rest) = content.strip_prefix("## ") {
        spans.push(Span::styled(
            format!("## {}", rest),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ));
    } else if let Some(rest) = content.strip_prefix("# ") {
        spans.push(Span::styled(
            format!("# {}", rest),
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ));
    } else {
        spans.extend(parse_inline_md(content));
    }

    Line::from(spans)
}

/// Parse inline markdown (`**bold**`, `*italic*`, `` `code` ``) into styled spans.
fn parse_inline_md(text: &str) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut i = 0;

    while i < n {
        match chars[i] {
            // **bold** or __bold__
            '*' if i + 1 < n && chars[i + 1] == '*' => {
                if let Some(end) = find_seq(&chars, i + 2, &['*', '*']) {
                    flush_buf(&mut buf, &mut spans);
                    spans.push(Span::styled(
                        chars[i + 2..end].iter().collect::<String>(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ));
                    i = end + 2;
                    continue;
                }
                buf.push('*');
            }
            '_' if i + 1 < n && chars[i + 1] == '_' => {
                if let Some(end) = find_seq(&chars, i + 2, &['_', '_']) {
                    flush_buf(&mut buf, &mut spans);
                    spans.push(Span::styled(
                        chars[i + 2..end].iter().collect::<String>(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ));
                    i = end + 2;
                    continue;
                }
                buf.push('_');
            }
            // *italic* (single star)
            '*' => {
                if let Some(end) = find_char_from(&chars, i + 1, '*') {
                    if end > i + 1 {
                        flush_buf(&mut buf, &mut spans);
                        spans.push(Span::styled(
                            chars[i + 1..end].iter().collect::<String>(),
                            Style::default().add_modifier(Modifier::ITALIC),
                        ));
                        i = end + 1;
                        continue;
                    }
                }
                buf.push('*');
            }
            // _italic_ (single underscore)
            '_' => {
                if let Some(end) = find_char_from(&chars, i + 1, '_') {
                    if end > i + 1 {
                        flush_buf(&mut buf, &mut spans);
                        spans.push(Span::styled(
                            chars[i + 1..end].iter().collect::<String>(),
                            Style::default().add_modifier(Modifier::ITALIC),
                        ));
                        i = end + 1;
                        continue;
                    }
                }
                buf.push('_');
            }
            // `inline code`
            '`' => {
                if let Some(end) = find_char_from(&chars, i + 1, '`') {
                    flush_buf(&mut buf, &mut spans);
                    spans.push(Span::styled(
                        chars[i + 1..end].iter().collect::<String>(),
                        Theme::md_code_inline(),
                    ));
                    i = end + 1;
                    continue;
                }
                buf.push('`');
            }
            c => buf.push(c),
        }
        i += 1;
    }

    flush_buf(&mut buf, &mut spans);
    if spans.is_empty() {
        spans.push(Span::raw(String::new()));
    }
    spans
}

fn flush_buf(buf: &mut String, spans: &mut Vec<Span<'static>>) {
    if !buf.is_empty() {
        spans.push(Span::raw(std::mem::take(buf)));
    }
}

/// Find the first occurrence of `seq` in `chars` at or after `from`.
fn find_seq(chars: &[char], from: usize, seq: &[char]) -> Option<usize> {
    let sl = seq.len();
    if sl == 0 { return Some(from); }
    for i in from..=chars.len().saturating_sub(sl) {
        if chars[i..i + sl] == *seq {
            return Some(i);
        }
    }
    None
}

/// Find the first occurrence of `target` in `chars` at or after `from`.
fn find_char_from(chars: &[char], from: usize, target: char) -> Option<usize> {
    chars[from..].iter().position(|&c| c == target).map(|p| from + p)
}
