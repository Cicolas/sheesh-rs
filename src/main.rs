mod app;
mod config;
mod event;
mod llm;
mod ssh;
mod tabs;
mod ui;

use std::{path::Path, time::Duration};

use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, MouseButton, MouseEventKind, poll, read,
};
use crossterm::execute;
use ftail::Ftail;
use log::LevelFilter;
use ratatui::{
    Frame, Terminal,
    layout::{Constraint, Layout, Rect},
    prelude::CrosstermBackend,
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, Paragraph},
};

use app::{AppState, ConnectedFocus};
use config::{load_connections, save_connections, ssh_config_path};
use event::Action;
use llm::{LLMConfig, build_provider};
use tabs::{Tab, listing::ListingTab, llm::LLMTab, terminal::TerminalTab};
use ui::{keybindings::render_keybindings, theme::Theme};

struct Sheesh {
    state: AppState,
    listing: ListingTab,
    terminal: Option<TerminalTab>,
    llm: Option<LLMTab>,
    llm_config: LLMConfig,
    error: Option<String>,
    /// Last known areas for the two connected panels — used for mouse click focus.
    terminal_area: Rect,
    llm_area: Rect,
}

impl Sheesh {
    fn new(connections: Vec<ssh::SSHConnection>, llm_config: LLMConfig) -> Self {
        Self {
            state: AppState::Listing,
            listing: ListingTab::new(connections),
            terminal: None,
            llm: None,
            llm_config,
            terminal_area: Rect::default(),
            llm_area: Rect::default(),
            error: None,
        }
    }

    fn connect(&mut self, name: String) {
        let conn = self
            .listing
            .connections
            .iter()
            .find(|c| c.name == name)
            .cloned();

        let Some(conn) = conn else {
            self.error = Some(format!("Connection '{}' not found", name));
            return;
        };

        let terminal = match TerminalTab::connect(&conn) {
            Ok(t) => t,
            Err(e) => {
                // PTY could not be opened at the OS level — show a terse error
                self.error = Some(format!("PTY error: {}", e));
                return;
            }
        };

        let provider = build_provider(&self.llm_config);
        self.terminal = Some(terminal);
        self.llm = Some(LLMTab::new(provider));
        self.state = AppState::Connected {
            connection_name: name,
            focus: ConnectedFocus::Terminal,
        };
    }

    fn disconnect(&mut self) {
        self.terminal = None;
        self.llm = None;
        self.state = AppState::Listing;
    }

    fn cycle_focus(&mut self) {
        if let AppState::Connected { ref mut focus, .. } = self.state {
            *focus = match focus {
                ConnectedFocus::Terminal => ConnectedFocus::LLM,
                ConnectedFocus::LLM => ConnectedFocus::Terminal,
            };
        }
    }

    fn send_context_to_llm(&mut self) {
        if let (Some(terminal), Some(llm)) = (&self.terminal, &mut self.llm) {
            let ctx = terminal.visible_text(50);
            let question = std::mem::take(&mut llm.input);
            llm.send_with_context(ctx, question);
        }
    }

    fn handle_event(&mut self, event: &crossterm::event::Event) -> bool {
        use crossterm::event::{KeyCode, KeyEvent};

        // Dismiss error on any key
        if self.error.is_some() {
            self.error = None;
            return true;
        }

        if let AppState::Connected { .. } = &self.state {
            match event {
                // F2 — toggle between terminal and LLM
                crossterm::event::Event::Key(KeyEvent {
                    code: KeyCode::F(2),
                    ..
                }) => {
                    self.cycle_focus();
                    return true;
                }
                // F3 — send terminal context to LLM (stay on current panel)
                crossterm::event::Event::Key(KeyEvent {
                    code: KeyCode::F(3),
                    ..
                }) => {
                    self.send_context_to_llm();
                    return true;
                }
                // Mouse click — focus the panel that was clicked.
                // Do NOT return early for the terminal panel so the click also
                // reaches the terminal handler to start a text selection.
                crossterm::event::Event::Mouse(me)
                    if me.kind == MouseEventKind::Down(MouseButton::Left) =>
                {
                    let col = me.column;
                    let row = me.row;
                    if contains(self.terminal_area, col, row) {
                        if let AppState::Connected { ref mut focus, .. } = self.state {
                            *focus = ConnectedFocus::Terminal;
                        }
                        // fall through — let terminal handle_event receive the click
                    }
                    if contains(self.llm_area, col, row) {
                        if let AppState::Connected { ref mut focus, .. } = self.state {
                            *focus = ConnectedFocus::LLM;
                        }
                        // fall through — let LLM handle_event receive the click for selection
                    }
                }
                _ => {}
            }
        }

        match &self.state.clone() {
            AppState::Listing => {
                let action = self.listing.handle_event(event);
                match action {
                    Action::Quit => return false,
                    Action::Confirm => {
                        if let Some(conn) = self.listing.selected_connection() {
                            let name = conn.name.clone();
                            self.connect(name);
                        }
                    }
                    _ => {}
                }
                let _ = save_connections(&ssh_config_path(), &self.listing.connections);
            }

            AppState::Connected { focus, .. } => {
                let action = match focus {
                    ConnectedFocus::Terminal => self
                        .terminal
                        .as_mut()
                        .map(|t| t.handle_event(event))
                        .unwrap_or(Action::None),
                    ConnectedFocus::LLM => self
                        .llm
                        .as_mut()
                        .map(|l| l.handle_event(event))
                        .unwrap_or(Action::None),
                };

                match action {
                    Action::Quit => return false,
                    Action::Disconnect => self.disconnect(),
                    _ => {}
                }
            }
        }

        true
    }

    fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();

        // Split into header, main, footer
        let [header_area, main_area, footer_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .areas(area);

        self.render_header(frame, header_area);
        self.render_main(frame, main_area);
        self.render_footer(frame, footer_area);

        if let Some(ref err) = self.error {
            render_error_popup(frame, area, err);
        }
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let title = match &self.state {
            AppState::Listing => " sheesh ".to_string(),
            AppState::Connected {
                connection_name, ..
            } => {
                format!(" sheesh > {} ", connection_name)
            }
        };

        let line = Line::from(vec![
            Span::styled(title, Theme::title()),
            Span::styled(" [?] help", Theme::key_hint_desc()),
        ]);
        frame.render_widget(Paragraph::new(line), area);
    }

    fn render_main(&mut self, frame: &mut Frame, area: Rect) {
        match &self.state.clone() {
            AppState::Listing => {
                self.listing.render(frame, area, true);
            }
            AppState::Connected { focus, .. } => {
                let [left_area, right_area] =
                    Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)])
                        .areas(area);

                self.terminal_area = left_area;
                self.llm_area = right_area;

                if let Some(t) = &mut self.terminal {
                    t.render(frame, left_area, *focus == ConnectedFocus::Terminal);
                }
                if let Some(l) = &mut self.llm {
                    l.render(frame, right_area, *focus == ConnectedFocus::LLM);
                }
            }
        }
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let hints: Vec<(&str, &str)> = match &self.state {
            AppState::Listing => self.listing.key_hints(),
            AppState::Connected { focus, .. } => {
                let mut hints = vec![("F2", "switch panel"), ("F3", "send context")];
                let panel_hints: Vec<(&str, &str)> = match focus {
                    ConnectedFocus::Terminal => self
                        .terminal
                        .as_ref()
                        .map(|t| t.key_hints())
                        .unwrap_or_default(),
                    ConnectedFocus::LLM => self
                        .llm
                        .as_ref()
                        .map(|l| l.key_hints())
                        .unwrap_or_default(),
                };
                hints.extend(panel_hints);
                hints.push(("q", "quit"));
                hints
            }
        };
        render_keybindings(frame, area, &hints);
    }
}

fn render_error_popup(frame: &mut Frame, area: Rect, msg: &str) {
    let popup_area = centered_rect(60, 20, area);
    frame.render_widget(Clear, popup_area);

    let para = Paragraph::new(vec![
        Line::default(),
        Line::from(Span::styled(format!("  {}", msg), Theme::error())),
        Line::default(),
        Line::from(Span::styled("  Press any key to continue", Theme::dimmed())),
    ])
    .block(
        Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Theme::error())
            .title(Span::styled(" Error ", Theme::error())),
    );

    frame.render_widget(para, popup_area);
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let [_, mid_v, _] = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .areas(area);

    let [_, center, _] = Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .areas(mid_v);

    center
}

fn contains(rect: Rect, col: u16, row: u16) -> bool {
    col >= rect.x && col < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height
}

fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    Ftail::new()
        .single_file(Path::new("logs"), true, LevelFilter::Info)
        .init()
        .unwrap();

    let ssh_path = ssh_config_path();
    let connections = load_connections(&ssh_path).unwrap_or_default();

    let llm_config = load_llm_config();
    let mut app = Sheesh::new(connections, llm_config);

    // Enable mouse before entering the TUI
    execute!(std::io::stdout(), EnableMouseCapture)?;

    let result = ratatui::run(
        |terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>| -> std::io::Result<()> {
            loop {
                terminal.draw(|f| app.draw(f))?;

                if poll(Duration::from_millis(5))? {
                    let ev = read()?;
                    if !app.handle_event(&ev) {
                        break;
                    }
                }
            }
            Ok(())
        },
    );

    execute!(std::io::stdout(), DisableMouseCapture)?;
    result?;
    Ok(())
}

fn load_llm_config() -> LLMConfig {
    let path = dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("sheesh")
        .join("config.toml");

    if let Ok(content) = std::fs::read_to_string(&path) {
        #[derive(serde::Deserialize, Default)]
        struct ConfigFile {
            #[serde(default)]
            llm: LLMConfig,
        }
        if let Ok(cfg) = toml::from_str::<ConfigFile>(&content) {
            return cfg.llm;
        }
    }

    LLMConfig::default()
}
