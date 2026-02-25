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
    llm::{ContentBlock, LLMEvent, LLMProvider, Message, RichMessage, Role, spawn_completion_rich},
    ui::theme::Theme,
};

use super::Tab;

/// Display prefix added to messages that include terminal context.
const CONTEXT_DISPLAY_PREFIX: &str = "[terminal context shared]";
/// Default question used when the user sends context without typing anything.
const CONTEXT_DEFAULT_QUESTION: &str = "What's happening here?";
/// API prompt template: context block + question.
const CONTEXT_PROMPT_TEMPLATE: &str = "Terminal context:\n```\n{context}\n```\n\n{question}";

/// (line_index, col) in the flattened history line buffer.
type BufPos = (usize, usize);

/// A tool call from Claude awaiting user confirmation.
struct PendingToolCall {
    /// Tool-use id — echoed back in the tool_result.
    id: String,
    command: String,
    description: Option<String>,
    /// Assistant content blocks already received (stored in rich_history on confirm/decline).
    assistant_blocks: Vec<ContentBlock>,
}

pub struct LLMTab {
    pub history: Vec<Message>,
    /// Full API message history including tool calls/results (sent to the API).
    rich_history: Vec<RichMessage>,
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
    /// Rows scrolled up inside the input box (0 = cursor visible at bottom).
    input_scroll: usize,
    /// Saved from last render to hit-test mouse events against the input box.
    last_input_area: Rect,
    /// Code blocks extracted from the latest assistant reply.
    suggestions: Vec<String>,
    /// Which suggestion is currently selected (None = no suggestions / cleared).
    suggestion_idx: Option<usize>,
    /// Tool call from Claude awaiting user confirmation.
    pending_tool_call: Option<PendingToolCall>,
    /// Tool-use id waiting for terminal output before resuming Claude.
    pub awaiting_output_id: Option<String>,
    /// When true, future tool calls execute without asking.
    auto_approve: bool,
    clipboard: Option<arboard::Clipboard>,
    /// Maps each visible chat screen row → (build_lines index, byte offset in that string).
    last_visual_row_map: Vec<(usize, usize)>,
}

impl LLMTab {
    pub fn new(provider: Arc<dyn LLMProvider>, system_prompt: Option<String>) -> Self {
        let (tx, rx) = mpsc::channel();
        let mut rich_history = vec![];
        if let Some(prompt) = system_prompt {
            rich_history.push(RichMessage::system(prompt));
        }

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
            input_scroll: 0,
            last_input_area: Rect::default(),
            suggestions: vec![],
            suggestion_idx: None,
            pending_tool_call: None,
            awaiting_output_id: None,
            auto_approve: false,
            clipboard: arboard::Clipboard::new().ok(),
            last_visual_row_map: vec![],
            rich_history,
        }
    }

    /// Poll the channel for completed LLM responses. Call this each render frame.
    pub fn poll(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            self.waiting = false;
            match event {
                LLMEvent::Response(text) => {
                    self.status = "Response received.".into();
                    self.suggestions = extract_code_blocks(&text);
                    self.suggestion_idx = if self.suggestions.is_empty() { None } else { Some(0) };
                    self.rich_history.push(RichMessage::assistant_text(&text));
                    self.history.push(Message::assistant(text));
                    self.scroll_offset = 0;
                }
                LLMEvent::ToolCall { id: api_id, command, description, assistant_blocks } => {
                    self.status = "Awaiting confirmation…".into();
                    // Replace the API-generated id with a locally unique one.
                    // Anthropic occasionally reuses ids across turns, which causes
                    // "tool_use ids must be unique" rejections on subsequent requests.
                    let local_id = unique_tool_id();
                    let assistant_blocks: Vec<ContentBlock> = assistant_blocks
                        .into_iter()
                        .map(|b| match b {
                            ContentBlock::ToolUse { id, name, input } if id == api_id => {
                                ContentBlock::ToolUse { id: local_id.clone(), name, input }
                            }
                            other => other,
                        })
                        .collect();

                    // Show any text the model produced before the tool call.
                    let pre_text: String = assistant_blocks
                        .iter()
                        .filter_map(|b| if let ContentBlock::Text { text } = b { Some(text.as_str()) } else { None })
                        .collect::<Vec<_>>()
                        .join("");
                    if !pre_text.trim().is_empty() {
                        self.history.push(Message::assistant(pre_text));
                    }
                    self.pending_tool_call = Some(PendingToolCall {
                        id: local_id,
                        command: command.clone(),
                        description,
                        assistant_blocks,
                    });
                    if self.auto_approve {
                        // Immediately approve without showing the prompt.
                        self.confirm_tool_call(true);
                    }
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

    /// Confirm or decline the pending tool call.
    /// Returns the command string if confirmed (to be forwarded as `SendToTerminal`).
    /// On accept the LLM is NOT resumed yet — `resume_with_output` does that
    /// once `main.rs` has captured the terminal output.
    fn confirm_tool_call(&mut self, accepted: bool) -> Option<String> {
        let ptc = self.pending_tool_call.take()?;

        // Append assistant blocks to rich history.
        self.rich_history.push(crate::llm::RichMessage {
            role: Role::Assistant,
            content: ptc.assistant_blocks,
        });

        if accepted {
            // Store the tool-use id; resume happens after output capture.
            self.awaiting_output_id = Some(ptc.id);
            self.waiting = true; // block new messages until output is captured
            self.status = "Command sent — capturing output…".into();
            Some(ptc.command)
        } else {
            self.rich_history.push(RichMessage::tool_result(
                &ptc.id,
                "User declined to execute the command.",
            ));
            self.waiting = true;
            self.status = "Declined — waiting for Claude…".into();
            spawn_completion_rich(
                Arc::clone(&self.provider),
                self.rich_history.clone(),
                self.tx.clone(),
            );
            None
        }
    }

    /// Called by `main.rs` after the terminal output has been captured.
    /// Appends the output as a tool_result and resumes the LLM.
    pub fn resume_with_output(&mut self, output: String) {
        let id = match self.awaiting_output_id.take() {
            Some(id) => id,
            None => return,
        };
        let result_text = if output.trim().is_empty() {
            "Command executed. No output was captured.".to_string()
        } else {
            format!("Command output:\n```\n{}\n```", output)
        };
        self.rich_history.push(RichMessage::tool_result(&id, &result_text));
        self.waiting = true;
        self.status = "Output captured — waiting for Claude…".into();
        spawn_completion_rich(
            Arc::clone(&self.provider),
            self.rich_history.clone(),
            self.tx.clone(),
        );
    }

    pub fn send_message(&mut self, content: String) {
        if content.trim().is_empty() || self.waiting {
            return;
        }
        self.history.push(Message::user(&content));
        self.rich_history.push(RichMessage::user_text(&content));
        self.waiting = true;
        self.scroll_offset = 0;
        self.status = "Waiting for response…".into();
        spawn_completion_rich(
            Arc::clone(&self.provider),
            self.rich_history.clone(),
            self.tx.clone(),
        );
    }

    /// Prepend terminal context and send.
    pub fn send_with_context(&mut self, context: String, question: String) {
        if self.waiting {
            return;
        }

        let question = if question.trim().is_empty() {
            CONTEXT_DEFAULT_QUESTION.to_string()
        } else {
            question
        };
        let display = format!("{} {}", CONTEXT_DISPLAY_PREFIX, question);
        let api_content = CONTEXT_PROMPT_TEMPLATE
            .replace("{context}", &context)
            .replace("{question}", &question);

        self.history.push(Message::user(&display));
        self.rich_history.push(RichMessage::user_text(api_content));
        self.waiting = true;
        self.scroll_offset = 0;
        self.status = "Waiting for response…".into();
        spawn_completion_rich(
            Arc::clone(&self.provider),
            self.rich_history.clone(),
            self.tx.clone(),
        );
    }

    /// Build the flat list of rendered lines from the message history.
    fn build_lines(&self) -> Vec<(String, Option<Style>)> {
        let mut all: Vec<(String, Option<Style>)> = vec![];
        for msg in &self.history {
            let (prefix, style) = match msg.role {
                Role::User => ("You: ", Theme::chat_user()),
                Role::Assistant => ("Claude: ", Style::default().fg(Color::Rgb(205, 115, 80))),
                Role::System => ("System: ", Theme::dimmed()),
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
        // Trailing padding so the last message can be scrolled above the bottom edge.
        for _ in 0..10 {
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
        let screen_row = (row - area.y) as usize;
        let screen_col = (col - area.x) as usize;

        let &(buf_line, row_byte_start) = self.last_visual_row_map.get(screen_row)?;

        // Convert screen_col (char index within this pre-split row) to a byte offset.
        let all = self.build_lines();
        let text = all.get(buf_line).map(|(t, _)| t.as_str()).unwrap_or("");
        let byte_col: usize = text[row_byte_start..]
            .chars()
            .take(screen_col)
            .map(|c| c.len_utf8())
            .sum();

        Some((buf_line, row_byte_start + byte_col))
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

    fn is_over_input(&self, col: u16, row: u16) -> bool {
        let a = self.last_input_area;
        col >= a.x && col < a.x + a.width && row >= a.y && row < a.y + a.height
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
        let mut hints = vec![
            ("enter", "send"),
            ("alt+enter", "newline"),
            ("esc", "clear input"),
            ("ctrl+c", "copy selection"),
        ];
        if self.suggestion_idx.is_some() {
            hints.push(("tab", "cycle suggestion"));
            hints.push(("F4", "apply to terminal"));
        }
        hints
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

                // Suggestion cycling and application
                if *code == KeyCode::Tab && !self.suggestions.is_empty() {
                    let n = self.suggestions.len();
                    self.suggestion_idx = Some(
                        (self.suggestion_idx.unwrap_or(0) + 1) % n,
                    );
                    return Action::None;
                }
                if *code == KeyCode::BackTab && !self.suggestions.is_empty() {
                    let n = self.suggestions.len();
                    self.suggestion_idx = Some(
                        (self.suggestion_idx.unwrap_or(0) + n - 1) % n,
                    );
                    return Action::None;
                }
                if *code == KeyCode::F(4) {
                    if let Some(idx) = self.suggestion_idx {
                        if let Some(cmd) = self.suggestions.get(idx) {
                            return Action::SendToTerminal(cmd.clone());
                        }
                    }
                    return Action::None;
                }

                // Confirmation prompt keys (when a tool call is pending).
                if self.pending_tool_call.is_some() {
                    match code {
                        KeyCode::Enter | KeyCode::Char('y') => {
                            if let Some(cmd) = self.confirm_tool_call(true) {
                                return Action::SendToTerminal(cmd);
                            }
                        }
                        KeyCode::Char('a') => {
                            self.auto_approve = true;
                            if let Some(cmd) = self.confirm_tool_call(true) {
                                return Action::SendToTerminal(cmd);
                            }
                        }
                        KeyCode::Esc | KeyCode::Char('n') => {
                            self.confirm_tool_call(false);
                        }
                        _ => {}
                    }
                    return Action::None;
                }

                // Text input
                match code {
                    KeyCode::Enter => {
                        if modifiers.contains(KeyModifiers::ALT) {
                            self.input.push('\n');
                            self.input_scroll = 0;
                        } else {
                            let msg = std::mem::take(&mut self.input);
                            self.input_scroll = 0;
                            self.send_message(msg);
                        }
                    }
                    KeyCode::Esc => {
                        self.input.clear();
                        self.input_scroll = 0;
                    }
                    KeyCode::Backspace => {
                        self.input.pop();
                        self.input_scroll = 0;
                    }
                    KeyCode::Char(ch)
                        if modifiers.is_empty() || modifiers.contains(KeyModifiers::SHIFT) =>
                    {
                        self.input.push(*ch);
                        self.input_scroll = 0;
                    }
                    _ => {}
                }
                Action::None
            }

            Event::Mouse(me) => {
                let over_input = self.is_over_input(me.column, me.row);
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
                    MouseEventKind::ScrollUp => {
                        if over_input {
                            self.input_scroll += 1;
                        } else {
                            self.scroll_up();
                        }
                    }
                    MouseEventKind::ScrollDown => {
                        if over_input {
                            self.input_scroll = self.input_scroll.saturating_sub(1);
                        } else {
                            self.scroll_down();
                        }
                    }
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

        // Input height: 1–5 content rows + 2 border = 3–7 total.
        // Grows with content; scrolls internally once it hits the cap.
        let input_width = inner.width.saturating_sub(2) as usize;
        let content_rows = wrapped_line_count(&self.input, input_width).clamp(1, 5);
        let input_height = content_rows as u16 + 2;
        let suggestion_height = if self.suggestion_idx.is_some() { 1u16 } else { 0 };

        let areas = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(suggestion_height),
            Constraint::Length(input_height),
        ])
        .split(inner);

        let (chat_area, status_area, suggestion_area, input_area) =
            (areas[0], areas[1], areas[2], areas[3]);

        self.last_chat_area = chat_area;
        self.last_input_area = input_area;
        self.render_history(frame, chat_area);
        self.render_status(frame, status_area);
        if suggestion_height > 0 {
            self.render_suggestion(frame, suggestion_area);
        }
        self.render_input(frame, input_area, focused);
    }
}

impl LLMTab {
    fn render_history(&mut self, frame: &mut Frame, area: Rect) {
        // Reserve rows at the bottom for the confirmation prompt when pending.
        const CONFIRM_ROWS: u16 = 4;
        let (history_area, confirm_area) = if self.pending_tool_call.is_some() {
            let split = Layout::vertical([
                Constraint::Min(1),
                Constraint::Length(CONFIRM_ROWS),
            ])
            .split(area);
            (split[0], Some(split[1]))
        } else {
            (area, None)
        };

        let all = self.build_lines();
        let total = all.len();
        let h = history_area.height as usize;
        let max_scroll = total.saturating_sub(h);
        self.scroll_offset = self.scroll_offset.min(max_scroll);
        let start = max_scroll - self.scroll_offset;
        self.last_render_start = start;

        let sel = self.selection_range();
        let width = history_area.width.max(1) as usize;

        // Pre-compute which lines fall inside a markdown code block or are tables.
        let in_code: Vec<bool> = {
            let mut flags = Vec::with_capacity(all.len());
            let mut in_block = false;
            for (text, _) in &all {
                let content = line_content(text);
                let trimmed = content.trim_start();
                if trimmed.starts_with("```") {
                    in_block = !in_block;
                    flags.push(true);
                } else if trimmed.starts_with('|') {
                    flags.push(true);
                } else {
                    flags.push(in_block);
                }
            }
            flags
        };

        let mut visual_map: Vec<(usize, usize)> = Vec::new();
        let mut visible: Vec<Line<'static>> = Vec::new();

        'outer: for (li, (text, _)) in all.iter().enumerate().skip(start) {
            let rendered = render_md_line(text, in_code[li]);
            for (chunk_spans, row_byte_start) in wrap_line_spans(rendered.spans, width) {
                if visible.len() >= h {
                    break 'outer;
                }
                visual_map.push((li, row_byte_start));
                visible.push(apply_sel_to_chunk(chunk_spans, li, row_byte_start, sel));
            }
        }

        self.last_visual_row_map = visual_map;
        frame.render_widget(Paragraph::new(visible), history_area);

        // ── Confirmation prompt ────────────────────────────────────────────
        if let (Some(ptc), Some(ca)) = (&self.pending_tool_call, confirm_area) {
            let approve_label = if self.auto_approve { " always (active)" } else { "" };
            let cmd = &ptc.command;
            let first_line = cmd.lines().next().unwrap_or("").to_string();
            let preview = if cmd.lines().count() > 1 {
                format!("{} …", first_line)
            } else {
                first_line
            };

            let desc_span = ptc.description.as_deref().unwrap_or("Run command?");
            let lines = vec![
                Line::from(Span::styled(
                    "─".repeat(ca.width as usize),
                    Theme::dimmed(),
                )),
                Line::from(vec![
                    Span::styled(" ◆ ", Theme::key_hint_key()),
                    Span::styled(desc_span.to_string(), Style::default().add_modifier(Modifier::BOLD)),
                    Span::styled(approve_label, Theme::dimmed()),
                ]),
                Line::from(vec![
                    Span::styled("   $ ", Theme::dimmed()),
                    Span::styled(preview, Theme::md_code_inline()),
                ]),
                Line::from(vec![
                    Span::styled("   [y/enter] ", Theme::key_hint_key()),
                    Span::styled("once", Theme::key_hint_desc()),
                    Span::styled("   [a] ", Theme::key_hint_key()),
                    Span::styled("always", Theme::key_hint_desc()),
                    Span::styled("   [n/esc] ", Theme::key_hint_key()),
                    Span::styled("skip", Theme::key_hint_desc()),
                ]),
            ];
            frame.render_widget(Paragraph::new(lines), ca);
        }
    }

    fn render_suggestion(&self, frame: &mut Frame, area: Rect) {
        let Some(idx) = self.suggestion_idx else {
            return;
        };
        let Some(cmd) = self.suggestions.get(idx) else {
            return;
        };
        let total = self.suggestions.len();
        // Show first line of the command; truncate with … if it has more.
        let first_line = cmd.lines().next().unwrap_or("").to_string();
        let preview = if cmd.lines().count() > 1 {
            format!("{} …", first_line)
        } else {
            first_line
        };
        let line = Line::from(vec![
            Span::styled(format!(" ⟩ [{}/{}] ", idx + 1, total), Theme::key_hint_key()),
            Span::styled(preview, Theme::md_code_inline()),
        ]);
        frame.render_widget(Paragraph::new(line), area);
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

        // Compute scroll: auto-scroll to cursor unless the user has scrolled up.
        let inner_width = area.width.saturating_sub(2) as usize;
        let max_rows = area.height.saturating_sub(2) as usize;
        let total_lines = wrapped_line_count(&content, inner_width);
        // How far from the bottom the user has scrolled (clamped so we can't go past top).
        let scroll_up = self.input_scroll.min(total_lines.saturating_sub(1));
        let scroll_top = total_lines.saturating_sub(max_rows).saturating_sub(scroll_up) as u16;

        let para = Paragraph::new(content)
            .block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(border_style)
                    .title(Span::styled(" Message ", Theme::dimmed())),
            )
            .wrap(Wrap { trim: false })
            .scroll((scroll_top, 0));

        frame.render_widget(para, area);
    }
}

// ── Tool id generation ────────────────────────────────────────────────────────

/// Generate a session-unique tool-use id so we never accidentally reuse one
/// that the API returned in a previous turn.
fn unique_tool_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("local_tool_{}", n)
}

// ── Suggestion helpers ────────────────────────────────────────────────────────

/// Extract all fenced code block contents from an LLM response text.
fn extract_code_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut in_block = false;
    let mut current = String::new();
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            if in_block {
                let trimmed = current.trim().to_string();
                if !trimmed.is_empty() {
                    blocks.push(trimmed);
                }
                current.clear();
                in_block = false;
            } else {
                in_block = true; // skip the fence line itself
            }
        } else if in_block {
            current.push_str(line);
            current.push('\n');
        }
    }
    blocks
}

// ── Input helpers ─────────────────────────────────────────────────────────────

/// Count the number of visual rows `text` occupies when wrapped to `width` columns.
/// Each `\n` starts a new logical line; long lines are counted as multiple rows.
fn wrapped_line_count(text: &str, width: usize) -> usize {
    if width == 0 {
        return text.lines().count().max(1);
    }
    text.lines()
        .map(|l| {
            let chars = l.chars().count();
            if chars == 0 { 1 } else { (chars + width - 1) / width }
        })
        .sum::<usize>()
        .max(1)
}

// ── Pre-split wrapping helpers ────────────────────────────────────────────────

/// Split a vec of ratatui spans into visual rows of at most `width` chars.
/// Returns `(chunk_spans, byte_offset_in_original_string)` per row.
fn wrap_line_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<(Vec<Span<'static>>, usize)> {
    if width == 0 {
        return vec![(spans, 0)];
    }
    let mut rows: Vec<(Vec<Span<'static>>, usize)> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut chars_in_row: usize = 0;
    let mut line_byte_offset: usize = 0;
    let mut row_byte_start: usize = 0;

    for span in spans {
        let style = span.style;
        let mut remaining = span.content.as_ref().to_string();

        while !remaining.is_empty() {
            let capacity = width - chars_in_row;
            let char_count = remaining.chars().count();

            if char_count <= capacity {
                chars_in_row += char_count;
                line_byte_offset += remaining.len();
                current.push(Span::styled(remaining, style));
                remaining = String::new();
            } else {
                let split_byte: usize =
                    remaining.chars().take(capacity).map(|c| c.len_utf8()).sum();
                let head = remaining[..split_byte].to_string();
                let tail = remaining[split_byte..].to_string();

                if !head.is_empty() {
                    current.push(Span::styled(head.clone(), style));
                }
                line_byte_offset += head.len();

                rows.push((std::mem::take(&mut current), row_byte_start));
                row_byte_start = line_byte_offset;
                chars_in_row = 0;
                remaining = tail;
            }
        }
    }

    rows.push((current, row_byte_start));
    rows
}

/// Apply selection highlight to a pre-split chunk of spans.
/// `row_byte_start` is where this chunk starts within the original logical line string.
fn apply_sel_to_chunk(
    chunk: Vec<Span<'static>>,
    buf_line: usize,
    row_byte_start: usize,
    sel: Option<(BufPos, BufPos)>,
) -> Line<'static> {
    let sel_style = Style::default().bg(Color::White).fg(Color::Black);
    let chunk_len: usize = chunk.iter().map(|s| s.content.len()).sum();

    let sel_range: Option<(usize, usize)> = sel.and_then(|(s, e)| {
        if buf_line < s.0 || buf_line > e.0 {
            return None;
        }
        let full_from = if buf_line == s.0 { s.1 } else { 0 };
        let full_to = if buf_line == e.0 { e.1 } else { usize::MAX };

        let chunk_end = row_byte_start + chunk_len;
        if full_to <= row_byte_start || full_from >= chunk_end {
            return None;
        }
        let from = full_from.saturating_sub(row_byte_start).min(chunk_len);
        let to = if full_to == usize::MAX {
            chunk_len
        } else {
            full_to.saturating_sub(row_byte_start).min(chunk_len)
        };
        if from < to { Some((from, to)) } else { None }
    });

    let Some((sel_from, sel_to)) = sel_range else {
        return Line::from(chunk);
    };

    let mut result: Vec<Span<'static>> = Vec::new();
    let mut pos: usize = 0;

    for span in chunk {
        let text = span.content.as_ref().to_string();
        let style = span.style;
        let len = text.len();
        let span_end = pos + len;

        if sel_to <= pos || sel_from >= span_end {
            result.push(Span::styled(text, style));
        } else {
            let a = sel_from.saturating_sub(pos).min(len);
            let b = sel_to.saturating_sub(pos).min(len);
            let a = (0..=a).rev().find(|&i| text.is_char_boundary(i)).unwrap_or(0);
            let b = (b..=len).find(|&i| text.is_char_boundary(i)).unwrap_or(len);
            if a > 0 { result.push(Span::styled(text[..a].to_string(), style)); }
            if a < b { result.push(Span::styled(text[a..b].to_string(), sel_style)); }
            if b < len { result.push(Span::styled(text[b..].to_string(), style)); }
        }
        pos += len;
    }

    Line::from(result)
}

// ── Markdown rendering helpers ────────────────────────────────────────────────

/// Strip the role prefix / indent from a line to get the raw content.
fn line_content(text: &str) -> &str {
    if let Some(rest) = text.strip_prefix("You: ") {
        rest
    } else if let Some(rest) = text.strip_prefix("Claude: ") {
        rest
    } else if let Some(rest) = text.strip_prefix("System: ") {
        rest
    } else if let Some(rest) = text.strip_prefix("      ") {
        rest
    } else {
        text
    }
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
            (
                "Claude: ",
                Some(Style::default().fg(Color::Rgb(205, 115, 80))),
                rest,
            )
        } else if let Some(rest) = full_text.strip_prefix("System: ") {
            ("System: ", Some(Theme::dimmed()), rest)
        } else if let Some(rest) = full_text.strip_prefix("      ") {
            ("      ", None, rest)
        } else {
            ("", None, full_text)
        };

    let mut spans: Vec<Span<'static>> = Vec::new();
    if !prefix_str.is_empty() {
        match prefix_style {
            Some(s) => spans.push(Span::styled(prefix_str.to_string(), s)),
            None => spans.push(Span::raw(prefix_str.to_string())),
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
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    } else if let Some(rest) = content.strip_prefix("# ") {
        spans.push(Span::styled(
            format!("# {}", rest),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
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
    if sl == 0 {
        return Some(from);
    }
    for i in from..=chars.len().saturating_sub(sl) {
        if chars[i..i + sl] == *seq {
            return Some(i);
        }
    }
    None
}

/// Find the first occurrence of `target` in `chars` at or after `from`.
fn find_char_from(chars: &[char], from: usize, target: char) -> Option<usize> {
    chars[from..]
        .iter()
        .position(|&c| c == target)
        .map(|p| from + p)
}
