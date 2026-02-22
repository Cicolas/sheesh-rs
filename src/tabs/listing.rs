use crossterm::event::{Event, KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::{
    event::Action,
    ssh::SSHConnection,
    ui::theme::Theme,
};

use super::Tab;

#[derive(Debug, Clone, PartialEq)]
pub enum ListingMode {
    /// Normal navigation
    Browse,
    /// User is typing a filter string
    Filtering,
    /// User is filling in the add/edit form
    Editing { is_new: bool },
    /// Confirm delete
    ConfirmDelete,
}

/// Form state for add/edit.
#[derive(Default, Clone)]
pub struct EditForm {
    pub name: String,
    pub description: String,
    pub hostname: String,
    pub user: String,
    pub port: String,
    pub identity_file: String,
    pub extra_options: String,
    /// Which field is focused (0-based index)
    pub field: usize,
}

impl EditForm {
    const FIELD_COUNT: usize = 7;

    pub fn from_connection(conn: &SSHConnection) -> Self {
        Self {
            name: conn.name.clone(),
            description: conn.description.clone(),
            hostname: conn.hostname.clone(),
            user: conn.user.clone(),
            port: if conn.port == 0 || conn.port == 22 {
                String::new()
            } else {
                conn.port.to_string()
            },
            identity_file: conn.identity_file.clone().unwrap_or_default(),
            extra_options: conn.extra_options.join(", "),
            field: 0,
        }
    }

    pub fn to_connection(&self) -> SSHConnection {
        SSHConnection {
            name: self.name.trim().to_string(),
            description: self.description.trim().to_string(),
            hostname: self.hostname.trim().to_string(),
            user: self.user.trim().to_string(),
            port: self.port.parse().unwrap_or(22),
            identity_file: {
                let s = self.identity_file.trim().to_string();
                if s.is_empty() { None } else { Some(s) }
            },
            extra_options: self.extra_options
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        }
    }

    fn active_field_mut(&mut self) -> &mut String {
        match self.field {
            0 => &mut self.name,
            1 => &mut self.description,
            2 => &mut self.hostname,
            3 => &mut self.user,
            4 => &mut self.port,
            5 => &mut self.identity_file,
            _ => &mut self.extra_options,
        }
    }

    pub fn push_char(&mut self, ch: char) {
        self.active_field_mut().push(ch);
    }

    pub fn pop_char(&mut self) {
        self.active_field_mut().pop();
    }

    pub fn next_field(&mut self) {
        self.field = (self.field + 1) % Self::FIELD_COUNT;
    }

    pub fn prev_field(&mut self) {
        self.field = self.field.saturating_sub(1);
    }
}

pub struct ListingTab {
    pub connections: Vec<SSHConnection>,
    pub list_state: ListState,
    pub mode: ListingMode,
    pub filter: String,
    pub form: EditForm,
    /// Index of the connection being edited (None = add)
    pub edit_index: Option<usize>,
}

impl ListingTab {
    pub fn new(connections: Vec<SSHConnection>) -> Self {
        let mut list_state = ListState::default();
        if !connections.is_empty() {
            list_state.select(Some(0));
        }
        Self {
            connections,
            list_state,
            mode: ListingMode::Browse,
            filter: String::new(),
            form: EditForm::default(),
            edit_index: None,
        }
    }

    pub fn filtered_indices(&self) -> Vec<usize> {
        if self.filter.is_empty() {
            (0..self.connections.len()).collect()
        } else {
            let f = self.filter.to_lowercase();
            self.connections
                .iter()
                .enumerate()
                .filter(|(_, c)| {
                    c.name.to_lowercase().contains(&f)
                        || c.hostname.to_lowercase().contains(&f)
                        || c.description.to_lowercase().contains(&f)
                })
                .map(|(i, _)| i)
                .collect()
        }
    }

    pub fn selected_connection(&self) -> Option<&SSHConnection> {
        let indices = self.filtered_indices();
        let sel = self.list_state.selected()?;
        indices.get(sel).and_then(|&i| self.connections.get(i))
    }

    fn move_down(&mut self) {
        let len = self.filtered_indices().len();
        if len == 0 {
            return;
        }
        let next = self.list_state.selected().map(|i| (i + 1).min(len - 1)).unwrap_or(0);
        self.list_state.select(Some(next));
    }

    fn move_up(&mut self) {
        let prev = self.list_state.selected().map(|i| i.saturating_sub(1)).unwrap_or(0);
        self.list_state.select(Some(prev));
    }

    fn start_add(&mut self) {
        self.form = EditForm::default();
        self.edit_index = None;
        self.mode = ListingMode::Editing { is_new: true };
    }

    fn start_edit(&mut self) {
        if let Some(conn) = self.selected_connection() {
            let indices = self.filtered_indices();
            let idx = indices[self.list_state.selected().unwrap_or(0)];
            self.form = EditForm::from_connection(conn);
            self.edit_index = Some(idx);
            self.mode = ListingMode::Editing { is_new: false };
        }
    }

    fn confirm_delete(&mut self) {
        if self.selected_connection().is_some() {
            self.mode = ListingMode::ConfirmDelete;
        }
    }

    fn do_delete(&mut self) {
        let indices = self.filtered_indices();
        if let Some(sel) = self.list_state.selected() {
            if let Some(&idx) = indices.get(sel) {
                self.connections.remove(idx);
                let new_len = self.filtered_indices().len();
                if new_len == 0 {
                    self.list_state.select(None);
                } else {
                    self.list_state.select(Some(sel.min(new_len - 1)));
                }
            }
        }
        self.mode = ListingMode::Browse;
    }

    fn save_form(&mut self) {
        let conn = self.form.to_connection();
        if let Some(idx) = self.edit_index {
            self.connections[idx] = conn;
        } else {
            self.connections.push(conn);
            let last = self.connections.len() - 1;
            self.list_state.select(Some(last));
        }
        self.mode = ListingMode::Browse;
    }

}

impl Tab for ListingTab {
    fn title(&self) -> &str {
        "Connections"
    }

    fn key_hints(&self) -> Vec<(&str, &str)> {
        match self.mode {
            ListingMode::Browse => vec![
                ("enter", "connect"),
                ("a", "add"),
                ("e", "edit"),
                ("d", "delete"),
                ("/", "filter"),
                ("q", "quit"),
            ],
            ListingMode::Filtering => vec![
                ("esc", "cancel"),
                ("enter", "confirm"),
            ],
            ListingMode::Editing { .. } => vec![
                ("tab", "next field"),
                ("shift+tab", "prev field"),
                ("enter", "save"),
                ("esc", "cancel"),
            ],
            ListingMode::ConfirmDelete => vec![
                ("y", "confirm delete"),
                ("n / esc", "cancel"),
            ],
        }
    }

    fn handle_event(&mut self, event: &Event) -> Action {
        let Event::Key(KeyEvent { code, .. }) = event else {
            return Action::None;
        };

        match &self.mode {
            ListingMode::Browse => match code {
                KeyCode::Char('j') | KeyCode::Down => {
                    self.move_down();
                    Action::None
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.move_up();
                    Action::None
                }
                KeyCode::Enter => Action::Confirm,
                KeyCode::Char('a') => {
                    self.start_add();
                    Action::None
                }
                KeyCode::Char('e') => {
                    self.start_edit();
                    Action::None
                }
                KeyCode::Char('d') => {
                    self.confirm_delete();
                    Action::None
                }
                KeyCode::Char('/') => {
                    self.filter.clear();
                    self.mode = ListingMode::Filtering;
                    Action::None
                }
                KeyCode::Char('q') => Action::Quit,
                _ => Action::None,
            },

            ListingMode::Filtering => match code {
                KeyCode::Esc => {
                    self.filter.clear();
                    self.mode = ListingMode::Browse;
                    Action::None
                }
                KeyCode::Enter => {
                    self.mode = ListingMode::Browse;
                    Action::None
                }
                KeyCode::Backspace => {
                    self.filter.pop();
                    Action::None
                }
                KeyCode::Char(ch) => {
                    self.filter.push(*ch);
                    Action::None
                }
                _ => Action::None,
            },

            ListingMode::Editing { .. } => match code {
                KeyCode::Esc => {
                    self.mode = ListingMode::Browse;
                    Action::None
                }
                KeyCode::Enter => {
                    self.save_form();
                    Action::None
                }
                KeyCode::Tab => {
                    self.form.next_field();
                    Action::None
                }
                KeyCode::BackTab => {
                    self.form.prev_field();
                    Action::None
                }
                KeyCode::Backspace => {
                    self.form.pop_char();
                    Action::None
                }
                KeyCode::Char(ch) => {
                    self.form.push_char(*ch);
                    Action::None
                }
                _ => Action::None,
            },

            ListingMode::ConfirmDelete => match code {
                KeyCode::Char('y') => {
                    self.do_delete();
                    Action::None
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.mode = ListingMode::Browse;
                    Action::None
                }
                _ => Action::None,
            },
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, focused: bool) {
        let [list_area, detail_area] =
            Layout::horizontal([Constraint::Percentage(65), Constraint::Percentage(35)])
                .areas(area);

        self.render_list(frame, list_area, focused);
        self.render_detail(frame, detail_area);

        // Overlays
        if let ListingMode::Editing { is_new } = &self.mode.clone() {
            self.render_form(frame, area, *is_new);
        }
        if self.mode == ListingMode::ConfirmDelete {
            self.render_confirm_delete(frame, area);
        }
    }
}

impl ListingTab {
    fn render_list(&mut self, frame: &mut Frame, area: Rect, focused: bool) {
        let border_style = if focused {
            Theme::selected_border()
        } else {
            Theme::normal_border()
        };

        let filter_title = if !self.filter.is_empty() {
            format!(" Connections [/{}] ", self.filter)
        } else {
            " Connections ".to_string()
        };

        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(border_style)
            .title(Span::styled(filter_title, Theme::title()));

        let indices = self.filtered_indices();
        let items: Vec<ListItem> = indices
            .iter()
            .map(|&i| {
                let c = &self.connections[i];
                let host_display = if c.hostname.is_empty() {
                    c.name.clone()
                } else {
                    format!("{} ({})", c.name, c.hostname)
                };
                ListItem::new(Line::from(vec![
                    Span::styled("  ", Theme::dimmed()),
                    Span::styled(host_display, Theme::value()),
                ]))
            })
            .collect();

        let list = List::new(items)
            .block(block)
            .highlight_style(Theme::highlight())
            .highlight_symbol("> ");

        frame.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn render_detail(&self, frame: &mut Frame, area: Rect) {
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Theme::normal_border())
            .title(Span::styled(" Detail ", Theme::title()));

        if let Some(conn) = self.selected_connection() {
            let port_str = if conn.port == 0 || conn.port == 22 {
                "22 (default)".to_string()
            } else {
                conn.port.to_string()
            };
            let key_str = conn.identity_file.as_deref().unwrap_or("(none)").to_string();
            let lines: Vec<Line> = vec![
                detail_line("Name", &conn.name),
                detail_line("Host", &conn.hostname),
                detail_line("User", &conn.user),
                detail_line("Port", &port_str),
                detail_line("Key", &key_str),
                Line::default(),
                detail_line("Desc", &conn.description),
            ];

            let para = Paragraph::new(lines)
                .block(block)
                .wrap(Wrap { trim: true });
            frame.render_widget(para, area);
        } else {
            let para = Paragraph::new(Line::styled(
                "  No connection selected",
                Theme::dimmed(),
            ))
            .block(block);
            frame.render_widget(para, area);
        }
    }

    fn render_form(&self, frame: &mut Frame, area: Rect, is_new: bool) {
        let title = if is_new { " Add Connection " } else { " Edit Connection " };
        let popup_area = centered_rect(60, 80, area);

        frame.render_widget(Clear, popup_area);

        let fields = [
            ("Name", &self.form.name),
            ("Description", &self.form.description),
            ("Hostname", &self.form.hostname),
            ("User", &self.form.user),
            ("Port", &self.form.port),
            ("Identity File", &self.form.identity_file),
            ("Extra Options", &self.form.extra_options),
        ];

        let mut lines: Vec<Line> = vec![Line::default()];
        for (i, (label, value)) in fields.iter().enumerate() {
            let focused = i == self.form.field;
            let cursor = if focused { "_" } else { "" };
            let label_style = if focused { Theme::key_hint_key() } else { Theme::label() };
            let value_style = if focused { Theme::highlight() } else { Theme::value() };

            lines.push(Line::from(vec![
                Span::styled(format!("  {:14}", label), label_style),
                Span::styled(format!("{}{}", value, cursor), value_style),
            ]));
        }

        let para = Paragraph::new(lines)
            .block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(Theme::selected_border())
                    .title(Span::styled(title, Theme::title())),
            )
            .wrap(Wrap { trim: false });

        frame.render_widget(para, popup_area);
    }

    fn render_confirm_delete(&self, frame: &mut Frame, area: Rect) {
        let popup_area = centered_rect(40, 20, area);
        frame.render_widget(Clear, popup_area);

        let name = self
            .selected_connection()
            .map(|c| c.name.as_str())
            .unwrap_or("?");

        let para = Paragraph::new(vec![
            Line::default(),
            Line::from(Span::styled(
                format!("  Delete \"{}\"?", name),
                Theme::error(),
            )),
            Line::default(),
            Line::from(vec![
                Span::styled("  [y]", Theme::key_hint_key()),
                Span::styled(" yes   ", Theme::key_hint_desc()),
                Span::styled("[n]", Theme::key_hint_key()),
                Span::styled(" no", Theme::key_hint_desc()),
            ]),
        ])
        .block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Theme::error())
                .title(Span::styled(" Confirm ", Theme::title())),
        );
        frame.render_widget(para, popup_area);
    }
}

fn detail_line<'a>(label: &'a str, value: &'a str) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("  {:14}", label), Theme::label()),
        Span::styled(value.to_string(), Theme::value()),
    ])
}

/// Returns a centered `Rect` as percentage of `area`.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let layout = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ]);
    let [_, middle, _] = layout.areas(area);

    let layout = Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ]);
    let [_, center, _] = layout.areas(middle);
    center
}
