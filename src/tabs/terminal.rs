use std::{
    io::{Read, Write},
    sync::{Arc, Mutex},
    thread,
};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use log::info;
use portable_pty::{CommandBuilder, MasterPty, NativePtySystem, PtySize, PtySystem};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Paragraph},
};

use crate::{event::Action, ssh::SSHConnection, ui::theme::Theme};

use super::Tab;

/// Circular buffer of terminal output lines.
pub const MAX_LINES: usize = 2000;

/// Number of terminal lines sent to the LLM as context.
pub const CONTEXT_LINES: usize = 50;

/// A (line_index, col) position in the line buffer.
type BufPos = (usize, usize);

/// A text segment with an associated color style.
#[derive(Clone, Debug)]
struct StyledSpan {
    text: String,
    style: Style,
}

/// ANSI SGR state — carried across line boundaries.
#[derive(Clone, Debug, Default)]
struct AnsiState {
    fg: Option<Color>,
    bg: Option<Color>,
    modifiers: Modifier,
}

impl AnsiState {
    fn to_style(&self) -> Style {
        let mut s = Style::default();
        if let Some(fg) = self.fg {
            s = s.fg(fg);
        }
        if let Some(bg) = self.bg {
            s = s.bg(bg);
        }
        if !self.modifiers.is_empty() {
            s = s.add_modifier(self.modifiers);
        }
        s
    }
}

/// Parse a string that may contain ANSI escape codes into styled spans.
/// Only SGR color codes (30-37, 38, 39, 40-47, 48, 49, 90-97, 100-107) are
/// honoured; all other escape sequences are silently dropped.
/// `state` is updated in-place so colors persist across line boundaries.
fn parse_ansi(input: &str, state: &mut AnsiState) -> Vec<StyledSpan> {
    let mut spans: Vec<StyledSpan> = Vec::new();
    let mut text = String::new();
    let mut chars = input.chars().peekable();

    // Flush accumulated text into spans, merging with previous if same style.
    macro_rules! flush {
        () => {
            if !text.is_empty() {
                let style = state.to_style();
                if spans
                    .last()
                    .map(|s: &StyledSpan| s.style == style)
                    .unwrap_or(false)
                {
                    spans.last_mut().unwrap().text.push_str(&text);
                } else {
                    spans.push(StyledSpan {
                        text: std::mem::take(&mut text),
                        style,
                    });
                }
                text.clear();
            }
        };
    }

    while let Some(ch) = chars.next() {
        match ch {
            '\x1b' => match chars.peek() {
                // CSI sequence: \x1b[ <params> <final>
                Some('[') => {
                    chars.next();
                    let mut params = String::new();
                    let mut final_byte = '\0';
                    for c in chars.by_ref() {
                        if c.is_ascii_alphabetic() {
                            final_byte = c;
                            break;
                        }
                        params.push(c);
                    }
                    if final_byte == 'm' {
                        flush!();
                        apply_sgr(&params, state);
                    }
                    // all other CSI sequences (cursor movement, etc.) are dropped
                }
                // OSC sequence: \x1b] ... BEL or ST
                Some(']') => {
                    chars.next();
                    loop {
                        match chars.next() {
                            Some('\x07') | None => break,
                            Some('\x1b') => {
                                if chars.peek() == Some(&'\\') {
                                    chars.next();
                                }
                                break;
                            }
                            _ => {}
                        }
                    }
                }
                _ => {
                    chars.next();
                }
            },
            '\r' => {}
            '\x08' => {
                text.pop();
            }
            c if c.is_control() && c != '\t' => {}
            c => text.push(c),
        }
    }

    flush!();
    spans
}

/// Apply a semicolon-separated list of SGR codes to `state`.
/// Only color-related codes are handled.
fn apply_sgr(params: &str, state: &mut AnsiState) {
    if params.is_empty() || params == "0" {
        *state = AnsiState::default();
        return;
    }

    let codes: Vec<u8> = params.split(';').filter_map(|s| s.parse().ok()).collect();

    let mut i = 0;
    while i < codes.len() {
        match codes[i] {
            0 => *state = AnsiState::default(),

            // Text styling
            1 => state.modifiers |= Modifier::BOLD,
            2 => state.modifiers |= Modifier::DIM,
            3 => state.modifiers |= Modifier::ITALIC,
            4 => state.modifiers |= Modifier::UNDERLINED,
            5 | 6 => state.modifiers |= Modifier::SLOW_BLINK,
            7 => state.modifiers |= Modifier::REVERSED,
            8 => state.modifiers |= Modifier::HIDDEN,
            9 => state.modifiers |= Modifier::CROSSED_OUT,
            22 => state.modifiers &= !(Modifier::BOLD | Modifier::DIM),
            23 => state.modifiers &= !Modifier::ITALIC,
            24 => state.modifiers &= !Modifier::UNDERLINED,
            25 => state.modifiers &= !Modifier::SLOW_BLINK,
            27 => state.modifiers &= !Modifier::REVERSED,
            28 => state.modifiers &= !Modifier::HIDDEN,
            29 => state.modifiers &= !Modifier::CROSSED_OUT,

            // Standard foreground colors
            30 => state.fg = Some(Color::Black),
            31 => state.fg = Some(Color::Red),
            32 => state.fg = Some(Color::Green),
            33 => state.fg = Some(Color::Yellow),
            34 => state.fg = Some(Color::Blue),
            35 => state.fg = Some(Color::Magenta),
            36 => state.fg = Some(Color::Cyan),
            37 => state.fg = Some(Color::White),
            38 => {
                if i + 1 < codes.len() {
                    match codes[i + 1] {
                        5 if i + 2 < codes.len() => {
                            state.fg = Some(Color::Indexed(codes[i + 2]));
                            i += 2;
                        }
                        2 if i + 4 < codes.len() => {
                            state.fg = Some(Color::Rgb(codes[i + 2], codes[i + 3], codes[i + 4]));
                            i += 4;
                        }
                        _ => {}
                    }
                }
            }
            39 => state.fg = None,

            // Standard background colors
            40 => state.bg = Some(Color::Black),
            41 => state.bg = Some(Color::Red),
            42 => state.bg = Some(Color::Green),
            43 => state.bg = Some(Color::Yellow),
            44 => state.bg = Some(Color::Blue),
            45 => state.bg = Some(Color::Magenta),
            46 => state.bg = Some(Color::Cyan),
            47 => state.bg = Some(Color::White),
            48 => {
                if i + 1 < codes.len() {
                    match codes[i + 1] {
                        5 if i + 2 < codes.len() => {
                            state.bg = Some(Color::Indexed(codes[i + 2]));
                            i += 2;
                        }
                        2 if i + 4 < codes.len() => {
                            state.bg = Some(Color::Rgb(codes[i + 2], codes[i + 3], codes[i + 4]));
                            i += 4;
                        }
                        _ => {}
                    }
                }
            }
            49 => state.bg = None,

            // Bright foreground colors
            90 => state.fg = Some(Color::DarkGray),
            91 => state.fg = Some(Color::LightRed),
            92 => state.fg = Some(Color::LightGreen),
            93 => state.fg = Some(Color::LightYellow),
            94 => state.fg = Some(Color::LightBlue),
            95 => state.fg = Some(Color::LightMagenta),
            96 => state.fg = Some(Color::LightCyan),
            97 => state.fg = Some(Color::Gray),

            // Bright background colors
            100 => state.bg = Some(Color::DarkGray),
            101 => state.bg = Some(Color::LightRed),
            102 => state.bg = Some(Color::LightGreen),
            103 => state.bg = Some(Color::LightYellow),
            104 => state.bg = Some(Color::LightBlue),
            105 => state.bg = Some(Color::LightMagenta),
            106 => state.bg = Some(Color::LightCyan),
            107 => state.bg = Some(Color::Gray),

            _ => {}
        }
        i += 1;
    }
}

/// Extract plain text from a styled line (used for LLM context and clipboard).
fn plain_text(line: &[StyledSpan]) -> String {
    line.iter().map(|s| s.text.as_str()).collect()
}

pub struct TerminalTab {
    lines: Arc<Mutex<Vec<Vec<StyledSpan>>>>,
    pty_writer: Option<Box<dyn Write + Send>>,
    pty_master: Option<Box<dyn MasterPty>>,
    alive: Arc<Mutex<bool>>,
    /// Set to true by clear_buffer(); reader thread resets its partial on next tick.
    clear_signal: Arc<Mutex<bool>>,
    #[allow(dead_code)]
    connection_name: String,
    scroll_offset: usize,
    /// Mouse selection: (anchor, cursor) in buffer coordinates.
    selection: Option<(BufPos, BufPos)>,
    /// Saved from last render to convert mouse coords → buffer coords.
    last_render_start: usize,
    last_inner: Rect,
    /// Maps each visible screen row → (buffer line index, byte offset within that line).
    /// Accounts for wrapped lines so mouse hit-testing stays accurate.
    last_visual_row_map: Vec<(usize, usize)>,
    /// Kept alive so the OS clipboard doesn't lose data when we drop it.
    clipboard: Option<arboard::Clipboard>,
}

impl TerminalTab {
    /// Spawn `ssh` inside a PTY for the given connection.
    pub fn connect(conn: &SSHConnection) -> anyhow::Result<Self> {
        let pty_system = NativePtySystem::default();

        let pair = pty_system.openpty(PtySize {
            rows: 40,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut cmd = CommandBuilder::new("ssh");
        for arg in conn.ssh_args() {
            cmd.arg(arg);
        }

        let _child = pair.slave.spawn_command(cmd)?;

        let master_writer = pair.master.take_writer()?;
        let mut master_reader = pair.master.try_clone_reader()?;
        let pty_master = pair.master;

        let lines: Arc<Mutex<Vec<Vec<StyledSpan>>>> = Arc::new(Mutex::new(vec![]));
        let alive: Arc<Mutex<bool>> = Arc::new(Mutex::new(true));
        let clear_signal: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));

        // Reader thread: capture PTY output as fast as possible.
        let lines_clone = Arc::clone(&lines);
        let alive_clone = Arc::clone(&alive);
        let clear_clone = Arc::clone(&clear_signal);
        thread::spawn(move || {
            let mut buf = [0u8; 8192];
            let mut partial = String::new();
            let mut partial_in_buf = false;
            // ANSI color state — persists across line boundaries for the session.
            let mut ansi_state = AnsiState::default();

            loop {
                match master_reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        {
                            let mut sig = clear_clone.lock().unwrap();
                            if *sig {
                                partial.clear();
                                partial_in_buf = false;
                                ansi_state = AnsiState::default();
                                *sig = false;
                            }
                        }

                        let chunk = String::from_utf8_lossy(&buf[..n]);
                        partial.push_str(&chunk);

                        // Extract complete lines, advancing ansi_state through each.
                        let mut complete: Vec<Vec<StyledSpan>> = Vec::new();
                        while let Some(pos) = partial.find('\n') {
                            complete.push(parse_ansi(&partial[..pos], &mut ansi_state));
                            partial.drain(..=pos);
                        }

                        // Parse the remaining partial with a clone so ansi_state only
                        // advances through complete lines.
                        let partial_line = {
                            let mut tmp = ansi_state.clone();
                            parse_ansi(&partial, &mut tmp)
                        };

                        let mut lock = lines_clone.lock().unwrap();
                        if partial_in_buf && !lock.is_empty() {
                            lock.pop();
                        }
                        lock.extend(complete);
                        lock.push(partial_line);
                        partial_in_buf = true;

                        let len = lock.len();
                        if len > MAX_LINES {
                            lock.drain(0..len - MAX_LINES);
                        }
                    }
                }
            }
            *alive_clone.lock().unwrap() = false;
        });

        Ok(Self {
            lines,
            pty_writer: Some(master_writer),
            pty_master: Some(pty_master),
            alive,
            clear_signal,
            connection_name: conn.name.clone(),
            scroll_offset: 0,
            selection: None,
            last_render_start: 0,
            last_inner: Rect::default(),
            last_visual_row_map: vec![],
            clipboard: arboard::Clipboard::new().ok(),
        })
    }

    pub fn is_alive(&self) -> bool {
        *self.alive.lock().unwrap()
    }

    /// Returns the current number of buffered lines.
    pub fn line_count(&self) -> usize {
        self.lines.lock().unwrap().len()
    }

    /// Returns all lines appended since `from_line` as a single string.
    pub fn capture_since(&self, from_line: usize) -> String {
        let lock = self.lines.lock().unwrap();
        let start = from_line.min(lock.len());
        lock[start..]
            .iter()
            .map(|l| plain_text(l))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Snapshot of current terminal output for sending to LLM.
    pub fn visible_text(&self, last_n: usize) -> String {
        let lock = self.lines.lock().unwrap();
        let start = lock.len().saturating_sub(last_n);
        lock[start..]
            .iter()
            .map(|l| plain_text(l))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Write a string into the PTY without appending a newline.
    /// The user can review the command and press Enter themselves.
    pub fn send_string(&mut self, s: &str) {
        self.send_bytes(s.as_bytes());
    }

    fn send_bytes(&mut self, bytes: &[u8]) {
        if let Some(ref mut w) = self.pty_writer {
            let _ = w.write_all(bytes);
            let _ = w.flush();
        }
    }

    fn scroll_up(&mut self) {
        self.scroll_offset += 3;
    }

    fn scroll_down(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(3);
    }

    /// Convert a screen (col, row) into a buffer (line_index, byte_offset).
    /// Uses the visual row map built during the last render; each map entry corresponds
    /// to exactly one pre-split display row so no ratatui wrapping is involved.
    fn screen_to_buf(&self, col: u16, row: u16) -> Option<BufPos> {
        let inner = self.last_inner;
        if row < inner.y || row >= inner.y + inner.height {
            return None;
        }
        if col < inner.x {
            return None;
        }
        let screen_row = (row - inner.y) as usize;
        let screen_col = (col - inner.x) as usize;

        let &(buf_line, row_byte_start) = self.last_visual_row_map.get(screen_row)?;

        // screen_col is a char index within this pre-split row; convert to bytes.
        let lock = self.lines.lock().unwrap();
        let text = plain_text(&lock[buf_line]);
        let byte_col: usize = text[row_byte_start..]
            .chars()
            .take(screen_col)
            .map(|c| c.len_utf8())
            .sum();

        Some((buf_line, row_byte_start + byte_col))
    }

    /// Normalise selection so (start <= end) in reading order.
    fn selection_range(&self) -> Option<(BufPos, BufPos)> {
        let (a, b) = self.selection?;
        if a.0 < b.0 || (a.0 == b.0 && a.1 <= b.1) {
            Some((a, b))
        } else {
            Some((b, a))
        }
    }

    /// Extract the selected text from the line buffer.
    fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selection_range()?;
        let lock = self.lines.lock().unwrap();
        if start.0 >= lock.len() {
            return None;
        }
        let end_line = end.0.min(lock.len() - 1);
        let mut out = String::new();
        for li in start.0..=end_line {
            let text = plain_text(&lock[li]);
            let from = if li == start.0 {
                start.1.min(text.len())
            } else {
                0
            };
            let to = if li == end_line {
                end.1.min(text.len())
            } else {
                text.len()
            };
            let from = (0..=from)
                .rev()
                .find(|&i| text.is_char_boundary(i))
                .unwrap_or(0);
            let to = (to..=text.len())
                .find(|&i| text.is_char_boundary(i))
                .unwrap_or(text.len());
            out.push_str(&text[from..to]);
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

    fn paste_from_clipboard(&mut self) {
        if let Some(ref mut cb) = self.clipboard {
            if let Ok(text) = cb.get_text() {
                self.send_bytes(text.as_bytes());
            }
        }
    }

    fn clear_buffer(&mut self) {
        *self.clear_signal.lock().unwrap() = true;
        self.lines.lock().unwrap().clear();
        self.scroll_offset = 0;
        self.selection = None;
    }
}

impl Tab for TerminalTab {
    fn title(&self) -> &str {
        "Terminal"
    }

    fn key_hints(&self) -> Vec<(&str, &str)> {
        vec![("ctrl+d", "disconnect")]
    }

    fn handle_event(&mut self, event: &Event) -> Action {
        match event {
            Event::Key(KeyEvent {
                code, modifiers, ..
            }) => {
                let ctrl = modifiers.contains(KeyModifiers::CONTROL);
                let shift = modifiers.contains(KeyModifiers::SHIFT);

                match code {
                    // ── App-level keys (not forwarded to PTY) ──────────────
                    KeyCode::Char('d') if ctrl => return Action::Disconnect,
                    KeyCode::Char('q') if ctrl => return Action::Quit,

                    // Ctrl+C — copy selection if any, else send ^C to PTY
                    KeyCode::Char('c') if ctrl && !shift => {
                        if self.selection.is_some() {
                            self.copy_selection();
                            self.selection = None;
                        } else {
                            self.send_bytes(&[0x03]);
                        }
                        return Action::None;
                    }

                    // Ctrl+V — paste clipboard into PTY
                    KeyCode::Char('v') if ctrl => {
                        self.paste_from_clipboard();
                        return Action::None;
                    }

                    // Ctrl+L — clear local buffer and ask remote to redraw
                    KeyCode::Char('l') if ctrl => {
                        self.send_bytes(&[0x0c]);
                        self.clear_buffer();
                        return Action::None;
                    }

                    // Scroll (Ctrl+Up/Down to not conflict with PTY arrow keys)
                    KeyCode::Up if ctrl => {
                        self.scroll_up();
                        return Action::None;
                    }
                    KeyCode::Down if ctrl => {
                        self.scroll_down();
                        return Action::None;
                    }

                    // ── Everything else goes straight to the PTY ────────────
                    KeyCode::Char(ch) => {
                        let mut bytes = [0u8; 4];
                        let encoded = ch.encode_utf8(&mut bytes);
                        if ctrl && ch.is_ascii_alphabetic() {
                            let ctrl_byte = (*ch as u8).to_ascii_uppercase() - b'@';
                            self.send_bytes(&[ctrl_byte]);
                        } else {
                            self.send_bytes(encoded.as_bytes());
                        }
                    }
                    KeyCode::Enter => self.send_bytes(b"\r"),
                    KeyCode::Backspace => self.send_bytes(b"\x7f"),
                    KeyCode::Tab => self.send_bytes(b"\t"),
                    KeyCode::Esc => self.send_bytes(b"\x1b"),
                    KeyCode::Left => self.send_bytes(b"\x1b[D"),
                    KeyCode::Right => self.send_bytes(b"\x1b[C"),
                    KeyCode::Up => self.send_bytes(b"\x1b[A"),
                    KeyCode::Down => self.send_bytes(b"\x1b[B"),
                    _ => {}
                }
                Action::None
            }

            // ── Mouse — text selection ──────────────────────────────────────
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
        let border_style = if focused {
            Theme::selected_border()
        } else {
            Theme::normal_border()
        };

        let status = if self.is_alive() {
            Span::styled(" ● ", Theme::key_hint_key())
        } else {
            Span::styled(" ○ disconnected ", Theme::error())
        };

        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(border_style)
            .title(Line::from(vec![
                Span::styled(" Terminal ", Theme::title()),
                status,
            ]));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        if inner != self.last_inner {
            if let Some(ref master) = self.pty_master {
                let _ = master.resize(PtySize {
                    rows: inner.height.max(1),
                    cols: inner.width.max(1),
                    pixel_width: 0,
                    pixel_height: 0,
                });
            }
        }
        self.last_inner = inner;

        let visible_height = inner.height as usize;
        let sel = self.selection_range();

        let display: Vec<Line> = {
            let lock = self.lines.lock().unwrap();
            let total = lock.len();
            let max_scroll = total.saturating_sub(visible_height);
            self.scroll_offset = self.scroll_offset.min(max_scroll);
            let start = max_scroll - self.scroll_offset;
            self.last_render_start = start;

            let width = inner.width.max(1) as usize;
            let mut visual_map: Vec<(usize, usize)> = Vec::with_capacity(visible_height);
            let mut display: Vec<Line<'static>> = Vec::with_capacity(visible_height);

            'outer: for (buf_idx, line) in lock.iter().enumerate().skip(start) {
                for (chunk, row_byte_start) in wrap_spans(line, width) {
                    if display.len() >= visible_height {
                        break 'outer;
                    }
                    visual_map.push((buf_idx, row_byte_start));
                    display.push(render_chunk(&chunk, buf_idx, row_byte_start, sel));
                }
            }

            self.last_visual_row_map = visual_map;
            display
        };

        frame.render_widget(Paragraph::new(display), inner);
    }
}

/// Split `spans` into visual rows of at most `width` characters each.
/// Returns a list of `(chunk_spans, byte_offset_in_original_line)` pairs.
fn wrap_spans(spans: &[StyledSpan], width: usize) -> Vec<(Vec<StyledSpan>, usize)> {
    if width == 0 {
        return vec![(spans.to_vec(), 0)];
    }
    let mut rows: Vec<(Vec<StyledSpan>, usize)> = Vec::new();
    let mut current: Vec<StyledSpan> = Vec::new();
    let mut chars_in_row: usize = 0;
    let mut line_byte_offset: usize = 0; // bytes consumed from the start of the full line
    let mut row_byte_start: usize = 0;   // byte offset where the current row starts

    for span in spans {
        let mut remaining = span.text.as_str();
        let style = span.style;

        while !remaining.is_empty() {
            let capacity = width - chars_in_row;
            let char_count = remaining.chars().count();

            if char_count <= capacity {
                current.push(StyledSpan { text: remaining.to_string(), style });
                chars_in_row += char_count;
                line_byte_offset += remaining.len();
                remaining = "";
            } else {
                // Take exactly `capacity` chars to fill the current row.
                let split_byte: usize =
                    remaining.chars().take(capacity).map(|c| c.len_utf8()).sum();
                let (head, tail) = remaining.split_at(split_byte);

                if !head.is_empty() {
                    current.push(StyledSpan { text: head.to_string(), style });
                }
                line_byte_offset += head.len();

                // Flush completed row.
                rows.push((std::mem::take(&mut current), row_byte_start));
                row_byte_start = line_byte_offset;
                chars_in_row = 0;
                remaining = tail;
            }
        }
    }

    // Always emit the final (possibly empty) row so blank lines are shown.
    rows.push((current, row_byte_start));
    rows
}

/// Render a pre-split chunk, applying selection highlight using chunk-local byte offsets.
/// `row_byte_start` is the byte offset within the original buffer line where this chunk begins.
fn render_chunk(
    chunk: &[StyledSpan],
    buf_line: usize,
    row_byte_start: usize,
    sel: Option<(BufPos, BufPos)>,
) -> Line<'static> {
    let sel_style = Style::default().bg(Color::White).fg(Color::Black);
    let chunk_len: usize = chunk.iter().map(|s| s.text.len()).sum();

    // Map the full-line selection into chunk-local byte offsets.
    let sel_range: Option<(usize, usize)> = sel.and_then(|(s, e)| {
        if buf_line < s.0 || buf_line > e.0 {
            return None;
        }
        let full_from = if buf_line == s.0 { s.1 } else { 0 };
        let full_to = if buf_line == e.0 { e.1 } else { usize::MAX };

        let chunk_end = row_byte_start + chunk_len;
        // Selection must overlap this chunk's byte range [row_byte_start, chunk_end).
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
        return Line::from(
            chunk
                .iter()
                .filter(|s| !s.text.is_empty())
                .map(|s| Span::styled(s.text.clone(), s.style))
                .collect::<Vec<_>>(),
        );
    };

    let mut result: Vec<Span<'static>> = Vec::new();
    let mut pos: usize = 0;

    for span in chunk {
        let text = &span.text;
        let len = text.len();
        let span_end = pos + len;

        if sel_to <= pos || sel_from >= span_end {
            if !text.is_empty() {
                result.push(Span::styled(text.clone(), span.style));
            }
        } else {
            let a = sel_from.saturating_sub(pos).min(len);
            let b = sel_to.saturating_sub(pos).min(len);
            let a = (0..=a).rev().find(|&i| text.is_char_boundary(i)).unwrap_or(0);
            let b = (b..=len).find(|&i| text.is_char_boundary(i)).unwrap_or(len);
            if a > 0 {
                result.push(Span::styled(text[..a].to_string(), span.style));
            }
            if a < b {
                result.push(Span::styled(text[a..b].to_string(), sel_style));
            }
            if b < len {
                result.push(Span::styled(text[b..].to_string(), span.style));
            }
        }
        pos += len;
    }

    Line::from(result)
}
