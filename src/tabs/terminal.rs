use std::{
    io::{Read, Write},
    sync::{Arc, Mutex},
    thread,
};

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind,
};
use log::info;
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Paragraph},
};

use crate::{event::Action, ssh::SSHConnection, ui::theme::Theme};

use super::Tab;

/// Circular buffer of terminal output lines.
const MAX_LINES: usize = 2000;

/// A (line_index, col) position in the line buffer.
type BufPos = (usize, usize);

pub struct TerminalTab {
    lines: Arc<Mutex<Vec<String>>>,
    pty_writer: Option<Box<dyn Write + Send>>,
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

        let lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
        let alive: Arc<Mutex<bool>> = Arc::new(Mutex::new(true));
        let clear_signal: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));

        // Reader thread: capture PTY output as fast as possible.
        let lines_clone = Arc::clone(&lines);
        let alive_clone = Arc::clone(&alive);
        let clear_clone = Arc::clone(&clear_signal);
        thread::spawn(move || {
            let mut buf = [0u8; 8192];
            let mut partial = String::new();
            // Whether the last entry in `lines` is an unfinished partial line.
            let mut partial_in_buf = false;
            loop {
                match master_reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        // If the main thread requested a clear, discard our partial too.
                        {
                            let mut sig = clear_clone.lock().unwrap();
                            if *sig {
                                partial.clear();
                                partial_in_buf = false;
                                *sig = false;
                            }
                        }

                        info!("{:?} ({} bytes)", buf, n);
                        let chunk = String::from_utf8_lossy(&buf[..n]);
                        partial.push_str(&chunk);

                        // Drain complete lines outside the lock.
                        let mut complete: Vec<String> = Vec::new();
                        while let Some(pos) = partial.find('\n') {
                            complete.push(strip_ansi(&partial[..pos]));
                            partial.drain(..=pos);
                        }

                        let mut lock = lines_clone.lock().unwrap();
                        // Remove the previous partial line before re-adding.
                        if partial_in_buf && !lock.is_empty() {
                            lock.pop();
                        }
                        lock.extend(complete);
                        // Always push the current partial so prompts show immediately.
                        lock.push(strip_ansi(&partial));
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
            alive,
            clear_signal,
            connection_name: conn.name.clone(),
            scroll_offset: 0,
            selection: None,
            last_render_start: 0,
            last_inner: Rect::default(),
            clipboard: arboard::Clipboard::new().ok(),
        })
    }

    pub fn is_alive(&self) -> bool {
        *self.alive.lock().unwrap()
    }

    /// Snapshot of current terminal output for sending to LLM.
    pub fn visible_text(&self, last_n: usize) -> String {
        let lock = self.lines.lock().unwrap();
        let start = lock.len().saturating_sub(last_n);
        lock[start..].join("\n")
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

    /// Convert a screen (col, row) into a buffer (line_index, col).
    fn screen_to_buf(&self, col: u16, row: u16) -> Option<BufPos> {
        let inner = self.last_inner;
        if row < inner.y || row >= inner.y + inner.height { return None; }
        if col < inner.x { return None; }
        let buf_line = self.last_render_start + (row - inner.y) as usize;
        let buf_col  = (col - inner.x) as usize;
        Some((buf_line, buf_col))
    }

    /// Normalise selection so (start <= end) in reading order.
    fn selection_range(&self) -> Option<(BufPos, BufPos)> {
        let (a, b) = self.selection?;
        if a.0 < b.0 || (a.0 == b.0 && a.1 <= b.1) { Some((a, b)) } else { Some((b, a)) }
    }

    /// Extract the selected text from the line buffer.
    fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selection_range()?;
        let lock = self.lines.lock().unwrap();
        if start.0 >= lock.len() { return None; }
        let end_line = end.0.min(lock.len() - 1);
        let mut out = String::new();
        for li in start.0..=end_line {
            let line = &lock[li];
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
        vec![
            ("F2", "switch panel"),
            ("F3", "send context"),
            ("ctrl+d", "disconnect"),
        ]
    }

    fn handle_event(&mut self, event: &Event) -> Action {
        match event {
            Event::Key(KeyEvent { code, modifiers, .. }) => {
                let ctrl  = modifiers.contains(KeyModifiers::CONTROL);
                let shift = modifiers.contains(KeyModifiers::SHIFT);

                match code {
                    // ── App-level keys (not forwarded to PTY) ──────────────
                    KeyCode::Char('d') if ctrl => return Action::Disconnect,

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
                    KeyCode::Up if ctrl => { self.scroll_up(); return Action::None; }
                    KeyCode::Down if ctrl => { self.scroll_down(); return Action::None; }

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
                    KeyCode::Enter     => self.send_bytes(b"\r"),
                    KeyCode::Backspace => self.send_bytes(b"\x7f"),
                    KeyCode::Tab       => self.send_bytes(b"\t"),
                    KeyCode::Esc       => self.send_bytes(b"\x1b"),
                    KeyCode::Left      => self.send_bytes(b"\x1b[D"),
                    KeyCode::Right     => self.send_bytes(b"\x1b[C"),
                    KeyCode::Up        => self.send_bytes(b"\x1b[A"),
                    KeyCode::Down      => self.send_bytes(b"\x1b[B"),
                    _ => {}
                }
                Action::None
            }

            // ── Mouse — text selection ──────────────────────────────────────
            Event::Mouse(me) => {
                match me.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        // Start a new selection; clear old one
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
                        // If anchor == cursor the user just clicked — clear selection
                        if let Some((a, b)) = self.selection {
                            if a == b { self.selection = None; }
                        }
                    }
                    // Scroll wheel
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

        // Save for mouse coord translation.
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

            lock[start..]
                .iter()
                .enumerate()
                .take(visible_height)
                .map(|(screen_row, line)| {
                    let buf_line = start + screen_row;
                    render_line(line, buf_line, sel)
                })
                .collect()
        };

        frame.render_widget(Paragraph::new(display), inner);
    }
}

/// Render a single line, highlighting the portion that falls inside `sel`.
fn render_line(line: &str, buf_line: usize, sel: Option<(BufPos, BufPos)>) -> Line<'static> {
    let sel_style = Style::default().bg(Color::White).fg(Color::Black);

    let Some((start, end)) = sel else {
        return Line::from(Span::raw(line.to_string()));
    };

    if buf_line < start.0 || buf_line > end.0 {
        return Line::from(Span::raw(line.to_string()));
    }

    let len = line.len();
    let from = if buf_line == start.0 { start.1.min(len) } else { 0 };
    let to   = if buf_line == end.0   { end.1.min(len)   } else { len };

    if from >= to {
        return Line::from(Span::raw(line.to_string()));
    }

    let mut spans = vec![];
    if from > 0         { spans.push(Span::raw(line[..from].to_string())); }
    spans.push(Span::styled(line[from..to].to_string(), sel_style));
    if to < len         { spans.push(Span::raw(line[to..].to_string())); }
    Line::from(spans)
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\x1b' => {
                match chars.peek() {
                    // CSI: \x1b[ ... <letter>
                    Some('[') => {
                        chars.next();
                        for c in chars.by_ref() {
                            if c.is_ascii_alphabetic() {
                                break;
                            }
                        }
                    }
                    // OSC: \x1b] ... \x07  or  \x1b] ... \x1b\
                    // This carries terminal title, hyperlinks, etc. — never visible text.
                    Some(']') => {
                        chars.next();
                        loop {
                            match chars.next() {
                                // BEL terminates OSC
                                Some('\x07') | None => break,
                                // ESC \ (ST) terminates OSC
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
                    // Any other \x1b sequence: skip the next char only
                    _ => {
                        chars.next();
                    }
                }
            }
            // Carriage return: skip (we split on \n; \r\n endings leave stray \r)
            '\r' => {}
            // Backspace: erase previous character (shell sends \x08 \x08 to visually delete)
            '\x08' => { out.pop(); }
            // Other non-printable control chars: skip
            c if c.is_control() && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
}
