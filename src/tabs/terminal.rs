use std::{
    io::{Read, Write},
    sync::{Arc, Mutex},
    thread,
};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use portable_pty::{CommandBuilder, MasterPty, NativePtySystem, PtySize, PtySystem};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Paragraph},
};
use termwiz::cell::Intensity;
use termwiz::color::{ColorSpec, SrgbaTuple};
use termwiz::escape::csi::{
    CSI, Cursor as TwCursor, DecPrivateMode, DecPrivateModeCode, Edit, EraseInDisplay, EraseInLine,
    Mode, Sgr,
};
use termwiz::escape::parser::Parser as EscapeParser;
use termwiz::escape::{Action as TwAction, ControlCode};

use super::Tab;
use crate::{event::Action, ssh::SSHConnection, ui::theme::Theme};

pub const MAX_LINES: usize = 2000;
pub const CONTEXT_LINES: usize = 50;

/// Selection position: (abs_row, col) in the combined scrollback+screen space.
type SelPos = (usize, u16);

// ── Cell types ────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Default)]
struct CellStyle {
    fg: Option<Color>,
    bg: Option<Color>,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    inverse: bool,
}

#[derive(Clone)]
struct TermCell {
    ch: char,
    style: CellStyle,
}

impl Default for TermCell {
    fn default() -> Self {
        Self {
            ch: ' ',
            style: CellStyle::default(),
        }
    }
}

type TermRow = Vec<TermCell>;

// ── Terminal emulator ─────────────────────────────────────────────────────────

struct TermEmulator {
    rows: usize,
    cols: usize,
    /// Visible (or alternate) screen.
    screen: Vec<TermRow>,
    /// Saved normal screen while in alternate screen mode.
    normal_screen: Vec<TermRow>,
    normal_cursor: (usize, usize),
    in_alt_screen: bool,
    cursor_row: usize,
    cursor_col: usize,
    saved_cursor: (usize, usize),
    cur_style: CellStyle,
    /// Scroll region — inclusive, 0-indexed.
    scroll_top: usize,
    scroll_bot: usize,
    /// Rows that scrolled off the top of the normal screen.
    scrollback: Vec<TermRow>,
    parser: EscapeParser,
}

impl TermEmulator {
    fn new(rows: usize, cols: usize) -> Self {
        let screen = vec![empty_row(cols); rows];
        let normal_screen = screen.clone();
        Self {
            rows,
            cols,
            screen,
            normal_screen,
            normal_cursor: (0, 0),
            in_alt_screen: false,
            cursor_row: 0,
            cursor_col: 0,
            saved_cursor: (0, 0),
            cur_style: CellStyle::default(),
            scroll_top: 0,
            scroll_bot: rows.saturating_sub(1),
            scrollback: Vec::new(),
            parser: EscapeParser::new(),
        }
    }

    fn resize(&mut self, rows: usize, cols: usize) {
        self.rows = rows;
        self.cols = cols;
        resize_grid(&mut self.screen, rows, cols);
        resize_grid(&mut self.normal_screen, rows, cols);
        self.cursor_row = self.cursor_row.min(rows.saturating_sub(1));
        self.cursor_col = self.cursor_col.min(cols.saturating_sub(1));
        self.scroll_top = 0;
        self.scroll_bot = rows.saturating_sub(1);
    }

    fn process(&mut self, data: &[u8]) {
        let actions = self.parser.parse_as_vec(data);
        for action in actions {
            self.apply_action(action);
        }
    }

    // ── Scroll helpers ────────────────────────────────────────────────────────

    fn scroll_up_region(&mut self, count: usize) {
        if self.rows == 0 {
            return;
        }
        let top = self.scroll_top;
        let bot = self.scroll_bot.min(self.rows - 1);
        if top >= bot {
            return;
        }
        let region_size = bot - top + 1;
        let count = count.min(region_size);

        // Capture rows scrolling off into our scrollback buffer,
        // but only when not in alt screen and the region starts at the top.
        if !self.in_alt_screen && top == 0 {
            for i in 0..count {
                self.scrollback.push(self.screen[top + i].clone());
            }
            let len = self.scrollback.len();
            if len > MAX_LINES {
                self.scrollback.drain(0..len - MAX_LINES);
            }
        }

        self.screen[top..=bot].rotate_left(count);
        for i in region_size - count..region_size {
            self.screen[top + i] = empty_row(self.cols);
        }
    }

    fn scroll_down_region(&mut self, count: usize) {
        if self.rows == 0 {
            return;
        }
        let top = self.scroll_top;
        let bot = self.scroll_bot.min(self.rows - 1);
        if top >= bot {
            return;
        }
        let region_size = bot - top + 1;
        let count = count.min(region_size);
        self.screen[top..=bot].rotate_right(count);
        for i in 0..count {
            self.screen[top + i] = empty_row(self.cols);
        }
    }

    // ── Action dispatch ───────────────────────────────────────────────────────

    fn apply_action(&mut self, action: TwAction) {
        match action {
            TwAction::Print(c) => self.print_char(c),
            TwAction::PrintString(s) => {
                for c in s.chars() {
                    self.print_char(c);
                }
            }
            TwAction::Control(cc) => self.apply_control(cc),
            TwAction::CSI(csi) => self.apply_csi(csi),
            _ => {}
        }
    }

    fn print_char(&mut self, c: char) {
        if self.cursor_row >= self.rows || self.cursor_col >= self.cols {
            return;
        }
        self.screen[self.cursor_row][self.cursor_col] = TermCell {
            ch: c,
            style: self.cur_style,
        };
        self.cursor_col += 1;
        if self.cursor_col >= self.cols {
            self.cursor_col = 0;
            self.do_linefeed();
        }
    }

    fn do_linefeed(&mut self) {
        if self.cursor_row == self.scroll_bot {
            self.scroll_up_region(1);
        } else {
            self.cursor_row = (self.cursor_row + 1).min(self.rows.saturating_sub(1));
        }
    }

    fn apply_control(&mut self, cc: ControlCode) {
        match cc {
            ControlCode::LineFeed | ControlCode::VerticalTab | ControlCode::FormFeed => {
                self.do_linefeed()
            }
            ControlCode::CarriageReturn => self.cursor_col = 0,
            ControlCode::Backspace => {
                if self.cursor_col > 0 {
                    self.cursor_col -= 1;
                }
            }
            ControlCode::HorizontalTab => {
                let next = (self.cursor_col / 8 + 1) * 8;
                self.cursor_col = next.min(self.cols.saturating_sub(1));
            }
            _ => {}
        }
    }

    fn apply_csi(&mut self, csi: CSI) {
        match csi {
            CSI::Cursor(c) => self.apply_cursor(c),
            CSI::Edit(e) => self.apply_edit(e),
            CSI::Sgr(sgr) => self.apply_sgr(sgr),
            CSI::Mode(mode) => self.apply_mode(mode),
            _ => {}
        }
    }

    fn apply_cursor(&mut self, c: TwCursor) {
        let rows = self.rows;
        let cols = self.cols;
        match c {
            TwCursor::Up(n) => {
                self.cursor_row = self
                    .cursor_row
                    .saturating_sub(n as usize)
                    .max(self.scroll_top);
            }
            TwCursor::Down(n) => {
                self.cursor_row =
                    (self.cursor_row + n as usize).min(self.scroll_bot.min(rows.saturating_sub(1)));
            }
            TwCursor::Left(n) => {
                self.cursor_col = self.cursor_col.saturating_sub(n as usize);
            }
            TwCursor::Right(n) => {
                self.cursor_col = (self.cursor_col + n as usize).min(cols.saturating_sub(1));
            }
            TwCursor::Position { line, col } => {
                self.cursor_row = (line.as_zero_based() as usize).min(rows.saturating_sub(1));
                self.cursor_col = (col.as_zero_based() as usize).min(cols.saturating_sub(1));
            }
            TwCursor::CharacterAbsolute(col) => {
                self.cursor_col = (col.as_zero_based() as usize).min(cols.saturating_sub(1));
            }
            TwCursor::CharacterAndLinePosition { line, col } => {
                self.cursor_row = (line.as_zero_based() as usize).min(rows.saturating_sub(1));
                self.cursor_col = (col.as_zero_based() as usize).min(cols.saturating_sub(1));
            }
            TwCursor::NextLine(n) => {
                self.cursor_row = (self.cursor_row + n as usize).min(rows.saturating_sub(1));
                self.cursor_col = 0;
            }
            TwCursor::PrecedingLine(n) => {
                self.cursor_row = self.cursor_row.saturating_sub(n as usize);
                self.cursor_col = 0;
            }
            TwCursor::SetTopAndBottomMargins { top, bottom } => {
                let t = (top.as_zero_based() as usize).min(rows.saturating_sub(1));
                let b = (bottom.as_zero_based() as usize).min(rows.saturating_sub(1));
                if t < b {
                    self.scroll_top = t;
                    self.scroll_bot = b;
                } else {
                    self.scroll_top = 0;
                    self.scroll_bot = rows.saturating_sub(1);
                }
                self.cursor_row = 0;
                self.cursor_col = 0;
            }
            TwCursor::SaveCursor => {
                self.saved_cursor = (self.cursor_row, self.cursor_col);
            }
            TwCursor::RestoreCursor => {
                self.cursor_row = self.saved_cursor.0.min(rows.saturating_sub(1));
                self.cursor_col = self.saved_cursor.1.min(cols.saturating_sub(1));
            }
            TwCursor::LinePositionAbsolute(n) => {
                self.cursor_row = ((n as usize).saturating_sub(1)).min(rows.saturating_sub(1));
            }
            TwCursor::LinePositionForward(n) => {
                self.cursor_row = (self.cursor_row + n as usize).min(rows.saturating_sub(1));
            }
            TwCursor::LinePositionBackward(n) => {
                self.cursor_row = self.cursor_row.saturating_sub(n as usize);
            }
            _ => {}
        }
    }

    fn apply_edit(&mut self, e: Edit) {
        let rows = self.rows;
        let cols = self.cols;
        let cr = self.cursor_row;
        let cc = self.cursor_col;

        match e {
            Edit::EraseInDisplay(eid) => match eid {
                EraseInDisplay::EraseToEndOfDisplay => {
                    for col in cc..cols {
                        self.screen[cr][col] = TermCell::default();
                    }
                    for row in cr + 1..rows {
                        self.screen[row] = empty_row(cols);
                    }
                }
                EraseInDisplay::EraseToStartOfDisplay => {
                    for col in 0..=cc.min(cols.saturating_sub(1)) {
                        self.screen[cr][col] = TermCell::default();
                    }
                    for row in 0..cr {
                        self.screen[row] = empty_row(cols);
                    }
                }
                EraseInDisplay::EraseDisplay => {
                    for row in &mut self.screen {
                        *row = empty_row(cols);
                    }
                }
                _ => {}
            },
            Edit::EraseInLine(eil) => match eil {
                EraseInLine::EraseToEndOfLine => {
                    for col in cc..cols {
                        self.screen[cr][col] = TermCell::default();
                    }
                }
                EraseInLine::EraseToStartOfLine => {
                    for col in 0..=cc.min(cols.saturating_sub(1)) {
                        self.screen[cr][col] = TermCell::default();
                    }
                }
                EraseInLine::EraseLine => {
                    self.screen[cr] = empty_row(cols);
                }
            },
            Edit::DeleteLine(n) => {
                let saved_top = self.scroll_top;
                self.scroll_top = cr;
                for _ in 0..n as usize {
                    let top = self.scroll_top;
                    let bot = self.scroll_bot.min(rows - 1);
                    if top < bot {
                        let sz = bot - top + 1;
                        self.screen[top..=bot].rotate_left(1);
                        self.screen[top + sz - 1] = empty_row(cols);
                    }
                }
                self.scroll_top = saved_top;
            }
            Edit::InsertLine(n) => {
                let saved_top = self.scroll_top;
                self.scroll_top = cr;
                for _ in 0..n as usize {
                    self.scroll_down_region(1);
                }
                self.scroll_top = saved_top;
            }
            Edit::ScrollUp(n) => self.scroll_up_region(n as usize),
            Edit::ScrollDown(n) => self.scroll_down_region(n as usize),
            Edit::DeleteCharacter(n) => {
                if cr < rows {
                    let row = &mut self.screen[cr];
                    let start = cc.min(cols);
                    let count = (n as usize).min(cols.saturating_sub(start));
                    if count > 0 {
                        row.drain(start..start + count);
                        while row.len() < cols {
                            row.push(TermCell::default());
                        }
                    }
                }
            }
            Edit::InsertCharacter(n) => {
                if cr < rows {
                    let row = &mut self.screen[cr];
                    let start = cc.min(cols);
                    let count = (n as usize).min(cols.saturating_sub(start));
                    for _ in 0..count {
                        row.insert(start, TermCell::default());
                    }
                    row.truncate(cols);
                }
            }
            _ => {}
        }
    }

    fn apply_sgr(&mut self, sgr: Sgr) {
        match sgr {
            Sgr::Reset => self.cur_style = CellStyle::default(),
            Sgr::Foreground(c) => self.cur_style.fg = colorspec_to_color(c),
            Sgr::Background(c) => self.cur_style.bg = colorspec_to_color(c),
            Sgr::Intensity(i) => match i {
                Intensity::Bold => {
                    self.cur_style.bold = true;
                    self.cur_style.dim = false;
                }
                Intensity::Half => {
                    self.cur_style.dim = true;
                    self.cur_style.bold = false;
                }
                Intensity::Normal => {
                    self.cur_style.bold = false;
                    self.cur_style.dim = false;
                }
            },
            Sgr::Italic(v) => self.cur_style.italic = v,
            Sgr::Underline(u) => {
                use termwiz::cell::Underline;
                self.cur_style.underline = !matches!(u, Underline::None);
            }
            Sgr::Inverse(v) => self.cur_style.inverse = v,
            _ => {}
        }
    }

    fn apply_mode(&mut self, mode: Mode) {
        let (set, code) = match mode {
            Mode::SetDecPrivateMode(DecPrivateMode::Code(c)) => (true, c),
            Mode::ResetDecPrivateMode(DecPrivateMode::Code(c)) => (false, c),
            _ => return,
        };
        match code {
            DecPrivateModeCode::ClearAndEnableAlternateScreen
            | DecPrivateModeCode::EnableAlternateScreen
            | DecPrivateModeCode::OptEnableAlternateScreen => {
                if set && !self.in_alt_screen {
                    self.normal_screen = self.screen.clone();
                    self.normal_cursor = (self.cursor_row, self.cursor_col);
                    self.screen = vec![empty_row(self.cols); self.rows];
                    self.in_alt_screen = true;
                } else if !set && self.in_alt_screen {
                    self.screen = self.normal_screen.clone();
                    self.cursor_row = self.normal_cursor.0.min(self.rows.saturating_sub(1));
                    self.cursor_col = self.normal_cursor.1.min(self.cols.saturating_sub(1));
                    self.in_alt_screen = false;
                }
            }
            _ => {}
        }
    }
}

// ── TerminalTab ───────────────────────────────────────────────────────────────

pub struct TerminalTab {
    emulator: Arc<Mutex<TermEmulator>>,
    output_log: Arc<Mutex<Vec<String>>>,
    pty_writer: Option<Box<dyn Write + Send>>,
    pty_master: Option<Box<dyn MasterPty>>,
    alive: Arc<Mutex<bool>>,
    #[allow(dead_code)]
    connection_name: String,
    scroll_offset: usize,
    selection: Option<(SelPos, SelPos)>,
    last_inner: Rect,
    clipboard: Option<arboard::Clipboard>,
    pub user_locked: bool,
    pub tool_locked: bool,
}

impl TerminalTab {
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

        let emulator = Arc::new(Mutex::new(TermEmulator::new(40, 120)));
        let output_log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let alive: Arc<Mutex<bool>> = Arc::new(Mutex::new(true));

        let emulator_c = Arc::clone(&emulator);
        let log_c = Arc::clone(&output_log);
        let alive_c = Arc::clone(&alive);

        thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match master_reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let data = &buf[..n];
                        emulator_c.lock().unwrap().process(data);

                        let stripped = strip_ansi(data);
                        if !stripped.is_empty() {
                            let mut log = log_c.lock().unwrap();
                            log.push(stripped);
                            let len = log.len();
                            if len > MAX_LINES {
                                log.drain(0..len - MAX_LINES);
                            }
                        }
                    }
                }
            }
            *alive_c.lock().unwrap() = false;
        });

        Ok(Self {
            emulator,
            output_log,
            pty_writer: Some(master_writer),
            pty_master: Some(pty_master),
            alive,
            connection_name: conn.name.clone(),
            scroll_offset: 0,
            selection: None,
            last_inner: Rect::default(),
            clipboard: arboard::Clipboard::new().ok(),
            user_locked: false,
            tool_locked: false,
        })
    }

    pub fn is_alive(&self) -> bool {
        *self.alive.lock().unwrap()
    }

    pub fn output_log_arc(&self) -> Arc<Mutex<Vec<String>>> {
        Arc::clone(&self.output_log)
    }

    pub fn line_count(&self) -> usize {
        self.output_log.lock().unwrap().len()
    }

    pub fn capture_since(&self, from: usize) -> String {
        let log = self.output_log.lock().unwrap();
        log[from.min(log.len())..].join("")
    }

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

    pub fn is_locked(&self) -> bool {
        self.user_locked || self.tool_locked
    }

    pub fn set_tool_locked(&mut self, locked: bool) {
        self.tool_locked = locked;
    }

    fn selection_range(&self) -> Option<(SelPos, SelPos)> {
        let (a, b) = self.selection?;
        if a.0 < b.0 || (a.0 == b.0 && a.1 <= b.1) {
            Some((a, b))
        } else {
            Some((b, a))
        }
    }

    fn screen_to_sel_pos(&self, screen_col: u16, screen_row: u16) -> Option<SelPos> {
        let emu = self.emulator.lock().unwrap();
        let sb_len = emu.scrollback.len();
        let total = sb_len + emu.rows;
        let visible_height = self.last_inner.height as usize;
        let first_visible = total.saturating_sub(visible_height + self.scroll_offset);
        if screen_col as usize >= emu.cols {
            return None;
        }
        let abs_row = first_visible + screen_row as usize;
        if abs_row >= total {
            None
        } else {
            Some((abs_row, screen_col))
        }
    }

    fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selection_range()?;
        let emu = self.emulator.lock().unwrap();
        let sb_len = emu.scrollback.len();
        let mut out = String::new();
        for abs_row in start.0..=end.0 {
            let col_start = if abs_row == start.0 {
                start.1 as usize
            } else {
                0
            };
            let col_end = if abs_row == end.0 {
                end.1 as usize
            } else {
                emu.cols
            };
            let text = if abs_row < sb_len {
                row_text(&emu.scrollback[abs_row], col_start, col_end)
            } else {
                let sr = abs_row - sb_len;
                if sr < emu.screen.len() {
                    row_text(&emu.screen[sr], col_start, col_end)
                } else {
                    String::new()
                }
            };
            out.push_str(&text);
            if abs_row < end.0 {
                out.push('\n');
            }
        }
        if out.trim().is_empty() {
            None
        } else {
            Some(out)
        }
    }

    fn copy_selection(&mut self) {
        if let Some(text) = self.selected_text()
            && let Some(ref mut cb) = self.clipboard
        {
            let _ = cb.set_text(text);
        }
    }

    fn paste_from_clipboard(&mut self) {
        if let Some(ref mut cb) = self.clipboard
            && let Ok(text) = cb.get_text()
        {
            self.send_bytes(text.as_bytes());
        }
    }
}

impl Tab for TerminalTab {
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
                    // ── Always-active keys ──────────────────────────────────
                    KeyCode::Char('d') if ctrl => return Action::Disconnect,
                    KeyCode::Char('q') if ctrl => return Action::Quit,
                    KeyCode::Up if ctrl => {
                        self.scroll_up();
                        return Action::None;
                    }
                    KeyCode::Down if ctrl => {
                        self.scroll_down();
                        return Action::None;
                    }

                    // ── Blocked when locked ─────────────────────────────────
                    _ if self.is_locked() => return Action::None,

                    KeyCode::Char('c') if ctrl && !shift => {
                        if self.selection.is_some() {
                            self.copy_selection();
                            self.selection = None;
                        } else {
                            self.send_bytes(&[0x03]);
                        }
                        return Action::None;
                    }
                    KeyCode::Char('v') if ctrl => {
                        self.paste_from_clipboard();
                        return Action::None;
                    }
                    KeyCode::Char('l') if ctrl => {
                        {
                            let mut emu = self.emulator.lock().unwrap();
                            let (rows, cols) = (emu.rows, emu.cols);
                            *emu = TermEmulator::new(rows, cols);
                        }
                        self.output_log.lock().unwrap().clear();
                        self.scroll_offset = 0;
                        self.selection = None;
                        self.send_bytes(&[0x0c]);
                        return Action::None;
                    }

                    // ── PTY passthrough ─────────────────────────────────────
                    _ => {
                        self.scroll_offset = 0;
                        match code {
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
                            KeyCode::Home => self.send_bytes(b"\x1b[H"),
                            KeyCode::End => self.send_bytes(b"\x1b[F"),
                            KeyCode::Delete => self.send_bytes(b"\x1b[3~"),
                            KeyCode::PageUp => self.send_bytes(b"\x1b[5~"),
                            KeyCode::PageDown => self.send_bytes(b"\x1b[6~"),
                            _ => {}
                        }
                    }
                }
                Action::None
            }

            Event::Mouse(me) => {
                let inner = self.last_inner;
                match me.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        if me.row >= inner.y
                            && me.row < inner.y + inner.height
                            && me.column >= inner.x
                            && me.column < inner.x + inner.width
                        {
                            let sc = me.column - inner.x;
                            let sr = me.row - inner.y;
                            if let Some(pos) = self.screen_to_sel_pos(sc, sr) {
                                self.selection = Some((pos, pos));
                            }
                        }
                    }
                    MouseEventKind::Drag(MouseButton::Left) => {
                        if let Some((anchor, _)) = self.selection
                            && me.row >= inner.y && me.column >= inner.x
                        {
                            let sc = me.column - inner.x;
                            let sr = me.row - inner.y;
                            if let Some(cur) = self.screen_to_sel_pos(sc, sr) {
                                self.selection = Some((anchor, cur));
                            }
                        }
                    }
                    MouseEventKind::Up(MouseButton::Left) => {
                        if let Some((a, b)) = self.selection
                            && a == b
                        {
                            self.selection = None;
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

        let lock_span = if self.user_locked {
            Span::styled(" 🔒 locked ", Theme::error())
        } else if self.tool_locked {
            Span::styled(" 🔒 tool running ", Theme::dimmed())
        } else {
            Span::raw("")
        };

        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(border_style)
            .title(Line::from(vec![
                Span::styled(" Terminal ", Theme::title()),
                status,
                lock_span,
            ]));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        // Resize PTY and emulator when the visible area changes.
        if inner != self.last_inner {
            let rows = inner.height.max(1) as usize;
            let cols = inner.width.max(1) as usize;
            if let Some(ref master) = self.pty_master {
                let _ = master.resize(PtySize {
                    rows: rows as u16,
                    cols: cols as u16,
                    pixel_width: 0,
                    pixel_height: 0,
                });
            }
            self.emulator.lock().unwrap().resize(rows, cols);
        }
        self.last_inner = inner;

        let visible_height = inner.height as usize;
        let sel = self.selection_range();

        let (display, cursor_screen_pos): (Vec<Line>, Option<(u16, u16)>) = {
            let emu = self.emulator.lock().unwrap();
            let sb_len = emu.scrollback.len();
            let total = sb_len + emu.rows;

            let max_scroll = total.saturating_sub(visible_height);
            self.scroll_offset = self.scroll_offset.min(max_scroll);
            let first_visible = total.saturating_sub(visible_height + self.scroll_offset);

            let mut display: Vec<Line<'static>> = Vec::with_capacity(visible_height);
            for vis_row in 0..visible_height {
                let abs_row = first_visible + vis_row;
                if abs_row >= total {
                    display.push(Line::default());
                    continue;
                }
                let row_data: &TermRow = if abs_row < sb_len {
                    &emu.scrollback[abs_row]
                } else {
                    let sr = abs_row - sb_len;
                    if sr < emu.screen.len() {
                        &emu.screen[sr]
                    } else {
                        display.push(Line::default());
                        continue;
                    }
                };
                display.push(render_term_row(row_data, abs_row, sel));
            }

            // Compute cursor screen position.
            let abs_cursor = sb_len + emu.cursor_row;
            let cursor_pos = if abs_cursor >= first_visible
                && abs_cursor < first_visible + visible_height
                && emu.cursor_col < emu.cols
            {
                let vis_row = abs_cursor - first_visible;
                Some((
                    inner.x + emu.cursor_col as u16,
                    inner.y + vis_row as u16,
                ))
            } else {
                None
            };

            (display, cursor_pos)
        };

        frame.render_widget(Paragraph::new(display), inner);

        if focused
            && let Some((cx, cy)) = cursor_screen_pos
        {
            frame.set_cursor_position((cx, cy));
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn empty_row(cols: usize) -> TermRow {
    vec![TermCell::default(); cols]
}

fn resize_grid(grid: &mut Vec<TermRow>, rows: usize, cols: usize) {
    grid.resize(rows, empty_row(cols));
    for row in grid.iter_mut() {
        row.resize(cols, TermCell::default());
    }
}

fn colorspec_to_color(c: ColorSpec) -> Option<Color> {
    match c {
        ColorSpec::Default => None,
        ColorSpec::PaletteIndex(idx) => Some(match idx {
            0 => Color::Black,
            1 => Color::Red,
            2 => Color::Green,
            3 => Color::Yellow,
            4 => Color::Blue,
            5 => Color::Magenta,
            6 => Color::Cyan,
            7 => Color::Gray,
            8 => Color::DarkGray,
            9 => Color::LightRed,
            10 => Color::LightGreen,
            11 => Color::LightYellow,
            12 => Color::LightBlue,
            13 => Color::LightMagenta,
            14 => Color::LightCyan,
            15 => Color::White,
            n => Color::Indexed(n),
        }),
        ColorSpec::TrueColor(SrgbaTuple(r, g, b, _)) => Some(Color::Rgb(
            (r * 255.0) as u8,
            (g * 255.0) as u8,
            (b * 255.0) as u8,
        )),
    }
}

fn cell_style_to_ratatui(style: &CellStyle) -> Style {
    let mut s = Style::default();
    let (fg, bg) = if style.inverse {
        (style.bg, style.fg.or(Some(Color::Reset)))
    } else {
        (style.fg, style.bg)
    };
    if let Some(fg) = fg {
        s = s.fg(fg);
    }
    if let Some(bg) = bg {
        s = s.bg(bg);
    }
    if style.bold {
        s = s.add_modifier(Modifier::BOLD);
    }
    if style.dim {
        s = s.add_modifier(Modifier::DIM);
    }
    if style.italic {
        s = s.add_modifier(Modifier::ITALIC);
    }
    if style.underline {
        s = s.add_modifier(Modifier::UNDERLINED);
    }
    s
}

fn render_term_row(row: &TermRow, abs_row: usize, sel: Option<(SelPos, SelPos)>) -> Line<'static> {
    let sel_style = Style::default().bg(Color::White).fg(Color::Black);
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut cur_text = String::new();
    let mut cur_style = Style::default();

    for (col, cell) in row.iter().enumerate() {
        let style = if in_sel(abs_row, col as u16, sel) {
            sel_style
        } else {
            cell_style_to_ratatui(&cell.style)
        };
        if style == cur_style {
            cur_text.push(cell.ch);
        } else {
            if !cur_text.is_empty() {
                spans.push(Span::styled(cur_text.clone(), cur_style));
            }
            cur_text = cell.ch.to_string();
            cur_style = style;
        }
    }
    // Only trim trailing spaces from the last span so column alignment is preserved.
    if !cur_text.is_empty() {
        let trimmed = cur_text.trim_end_matches(' ').to_string();
        if !trimmed.is_empty() {
            spans.push(Span::styled(trimmed, cur_style));
        }
    }
    Line::from(spans)
}

fn row_text(row: &TermRow, col_start: usize, col_end: usize) -> String {
    row[col_start..col_end.min(row.len())]
        .iter()
        .map(|c| c.ch)
        .collect::<String>()
        .trim_end()
        .to_string()
}

fn in_sel(abs_row: usize, col: u16, sel: Option<(SelPos, SelPos)>) -> bool {
    let Some((s, e)) = sel else { return false };
    (abs_row > s.0 || (abs_row == s.0 && col >= s.1))
        && (abs_row < e.0 || (abs_row == e.0 && col < e.1))
}

fn strip_ansi(data: &[u8]) -> String {
    let s = String::from_utf8_lossy(data);
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\x1b' => match chars.peek() {
                Some('[') => {
                    chars.next();
                    for c in chars.by_ref() {
                        if c.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
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
            c => out.push(c),
        }
    }
    out
}
