mod app;
mod config;
mod event;
mod llm;
mod logs;
mod ssh;
mod tabs;
mod ui;

use std::time::Duration;

use clap::{Parser, Subcommand};

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

/// Captures terminal output produced by a tool-call command and forwards it
/// to the LLM once the output has been stable (no new lines) for a short period.
struct PendingCapture {
    /// Number of terminal lines present *before* the command was sent.
    snapshot: usize,
    /// Line count at the last tick where output was still growing.
    last_line_count: usize,
    /// When the line count last changed (used to detect output stability).
    last_change: std::time::Instant,
}

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
    /// Pending terminal output capture for an in-flight tool call.
    pending_capture: Option<PendingCapture>,
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
            pending_capture: None,
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
        let output_log = terminal.output_log_arc();
        self.terminal = Some(terminal);
        let mut llm = LLMTab::new(
            provider,
            self.llm_config.system_prompt.clone(),
            conn.clone(),
        );
        llm.set_terminal_output(output_log);
        self.llm = Some(llm);
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
                // Mouse click — focus the panel that was clicked.
                // Do NOT return early for the terminal panel so the click also
                // reaches the terminal handler to start a text selection.
                crossterm::event::Event::Mouse(me)
                    if me.kind == MouseEventKind::Down(MouseButton::Left) =>
                {
                    let col = me.column;
                    let row = me.row;
                    if contains(self.terminal_area, col, row)
                        && let AppState::Connected { ref mut focus, .. } = self.state
                    {
                        *focus = ConnectedFocus::Terminal;
                        // fall through — let terminal handle_event receive the click
                    }
                    if contains(self.llm_area, col, row)
                        && let AppState::Connected { ref mut focus, .. } = self.state
                    {
                        *focus = ConnectedFocus::LLM;
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
                    Action::CancelToolCall => {
                        self.pending_capture = None;
                        if let Some(llm) = &mut self.llm {
                            llm.cancel_tool_call();
                        }
                        if let Some(terminal) = &mut self.terminal {
                            terminal.set_tool_locked(false);
                        }
                    }
                    Action::SendToTerminal(cmd) => {
                        if let Some(t) = &mut self.terminal {
                            let snapshot = t.line_count();
                            t.send_string(&cmd);
                            t.send_string("\r");
                            t.set_tool_locked(true);
                            // Wait for output to stabilise (300 ms of silence) then
                            // forward it to Claude. The user can press ctrl+c to cancel.
                            let now = std::time::Instant::now();
                            self.pending_capture = Some(PendingCapture {
                                snapshot,
                                last_line_count: snapshot,
                                last_change: now,
                            });
                        }
                        if let AppState::Connected { ref mut focus, .. } = self.state {
                            *focus = ConnectedFocus::Terminal;
                        }
                    }
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
                let mut hints = vec![("F2", "switch panel")];
                let panel_hints: Vec<(&str, &str)> = match focus {
                    ConnectedFocus::Terminal => self
                        .terminal
                        .as_ref()
                        .map(|t| t.key_hints())
                        .unwrap_or_default(),
                    ConnectedFocus::LLM => {
                        self.llm.as_ref().map(|l| l.key_hints()).unwrap_or_default()
                    }
                };
                hints.extend(panel_hints);
                hints.push(("ctrl+q", "quit"));
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

fn init_logging(logs_config: &logs::LogsConfig) {
    if logs::is_disabled() {
        return;
    }
    let dir = logs_config.resolved_dir();
    let log_path = logs::session_log_path(&dir);
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    Ftail::new()
        .single_file(&log_path, false, LevelFilter::Debug)
        .init()
        .unwrap();
}

/// sheesh — a TUI SSH connection manager with an embedded LLM assistant.
#[derive(Parser)]
#[command(
    name = "sheesh-rs",
    version,
    about = "A TUI app for managing SSH connections with an embedded LLM assistant.",
    long_about = "\
sheesh-rs lets you manage SSH connections stored in ~/.ssh/config and connect\n\
to them through a split-pane TUI. The right panel hosts an LLM chat assistant\n\
that can receive terminal context and run commands on your behalf.\n\
\n\
CONFIGURATION\n\
  ~/.ssh/config                   SSH connections (source of truth)\n\
  ~/.config/sheesh/config.toml    App settings (LLM provider, log directory)\n\
\n\
CONFIG FILE EXAMPLE\n\
  [llm]\n\
  provider    = \"anthropic\"\n\
  model       = \"claude-sonnet-4-6\"\n\
  api_key_env = \"ANTHROPIC_API_KEY\"\n\
\n\
  [logs]\n\
  dir = \"/tmp/sheesh\"   # default; use a persistent path to retain logs\n\
\n\
TUI KEYBINDINGS\n\
  Listing view\n\
    j / k       Navigate connections\n\
    enter       Connect\n\
    a           Add connection\n\
    e           Edit connection\n\
    d           Delete connection\n\
    /           Filter connections\n\
    q           Quit\n\
\n\
  Connected view\n\
    F2          Cycle focus (terminal ↔ LLM chat)\n\
    F3 / c      Send last 50 terminal lines to LLM\n\
    ctrl+d      Disconnect\n\
    enter       Send LLM message (when LLM focused)\n\
    q           Quit"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Manage session log files
    Log {
        #[command(subcommand)]
        action: LogAction,
    },
}

#[derive(Subcommand)]
enum LogAction {
    /// Remove all session log files from the log directory
    Clean,
    /// Print the most recent session log to stdout
    View,
    /// Disable logging for future sessions
    Disable,
    /// Re-enable logging
    Enable,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Log { action }) => {
            match action {
                LogAction::Clean => {
                    let cfg = load_app_config();
                    return logs::cmd_clean(&cfg.logs.resolved_dir());
                }
                LogAction::View => {
                    let cfg = load_app_config();
                    return logs::cmd_view(&cfg.logs.resolved_dir());
                }
                LogAction::Disable => return logs::cmd_disable(),
                LogAction::Enable => return logs::cmd_enable(),
            }
        }
        None => {}
    }

    let cfg = load_app_config();
    init_logging(&cfg.logs);

    let ssh_path = ssh_config_path();
    let connections = load_connections(&ssh_path).unwrap_or_default();

    let llm_config = cfg.llm;
    let mut app = Sheesh::new(connections, llm_config);

    // Enable mouse before entering the TUI
    execute!(std::io::stdout(), EnableMouseCapture)?;

    let result = ratatui::run(
        |terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>| -> std::io::Result<()> {
            loop {
                terminal.draw(|f| app.draw(f))?;

                // Forward captured terminal output to Claude once output has been
                // stable (no new PTY lines) for 300 ms.
                let should_fire = if let Some(ref mut cap) = app.pending_capture {
                    let now = std::time::Instant::now();
                    let current = app.terminal.as_ref().map_or(0, |t| t.line_count());
                    if current > cap.last_line_count {
                        cap.last_line_count = current;
                        cap.last_change = now;
                    }
                    let silence = now.duration_since(cap.last_change);
                    let has_output = cap.last_line_count > cap.snapshot;
                    // Wait for output to appear, then stabilise for 1100 ms.
                    // If the command produces no output at all, fire after 5 s.
                    (has_output && silence >= Duration::from_millis(1100))
                        || (!has_output && silence >= Duration::from_secs(5))
                } else {
                    false
                };
                if should_fire {
                    let snapshot = app.pending_capture.take().unwrap().snapshot;
                    if let (Some(terminal), Some(llm)) = (&app.terminal, &mut app.llm)
                        && llm.awaiting_output_id.is_some()
                    {
                        let output = terminal.capture_since(snapshot);
                        llm.resume_with_output(output);
                    }
                }

                // Release the tool lock once the LLM finishes the tool-execution cycle.
                if let (Some(terminal), Some(llm)) = (&mut app.terminal, &app.llm)
                    && terminal.tool_locked
                    && !llm.is_executing_tool()
                    && !llm.waiting
                {
                    terminal.set_tool_locked(false);
                }

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

struct AppConfig {
    llm: LLMConfig,
    logs: logs::LogsConfig,
}

fn load_app_config() -> AppConfig {
    let path = dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("sheesh")
        .join("config.toml");

    log::info!("[config] loading config from {}", path.display());

    match std::fs::read_to_string(&path) {
        Err(e) => {
            log::warn!(
                "[config] could not read config file: {} — using defaults",
                e
            );
        }
        Ok(content) => {
            #[derive(serde::Deserialize, Default)]
            struct ConfigFile {
                #[serde(default)]
                llm: LLMConfig,
                #[serde(default)]
                logs: logs::LogsConfig,
            }
            match toml::from_str::<ConfigFile>(&content) {
                Err(e) => {
                    log::error!(
                        "[config] failed to parse config.toml: {} — using defaults",
                        e
                    );
                }
                Ok(cfg) => {
                    log::info!(
                        "[config] loaded: provider={} model={}",
                        cfg.llm.provider,
                        cfg.llm.model
                    );
                    return AppConfig {
                        llm: cfg.llm,
                        logs: cfg.logs,
                    };
                }
            }
        }
    }

    AppConfig {
        llm: LLMConfig::default(),
        logs: logs::LogsConfig::default(),
    }
}
