#![allow(dead_code)]
use crate::ssh::SSHConnection;

#[derive(Debug, Clone, PartialEq)]
pub enum ConnectedFocus {
    Terminal,
    LLM,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AppState {
    Listing,
    Connected {
        connection_name: String,
        focus: ConnectedFocus,
    },
}

pub struct App {
    pub state: AppState,
    pub connections: Vec<SSHConnection>,
    pub error: Option<String>,
}

impl App {
    pub fn new(connections: Vec<SSHConnection>) -> Self {
        Self {
            state: AppState::Listing,
            connections,
            error: None,
        }
    }

    pub fn connect(&mut self, name: String) {
        self.state = AppState::Connected {
            connection_name: name,
            focus: ConnectedFocus::Terminal,
        };
    }

    pub fn disconnect(&mut self) {
        self.state = AppState::Listing;
    }

    pub fn cycle_focus(&mut self) {
        if let AppState::Connected { ref mut focus, .. } = self.state {
            *focus = match focus {
                ConnectedFocus::Terminal => ConnectedFocus::LLM,
                ConnectedFocus::LLM => ConnectedFocus::Terminal,
            };
        }
    }

    pub fn set_error(&mut self, msg: String) {
        self.error = Some(msg);
    }

    pub fn clear_error(&mut self) {
        self.error = None;
    }
}
