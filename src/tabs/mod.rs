use crossterm::event::Event;
use ratatui::{Frame, layout::Rect};

use crate::event::Action;

pub mod listing;
pub mod llm;
pub mod terminal;

pub trait Tab {
    fn render(&mut self, frame: &mut Frame, area: Rect, focused: bool);
    fn handle_event(&mut self, event: &Event) -> Action;
    fn title(&self) -> &str;
    fn key_hints(&self) -> Vec<(&str, &str)>;
}
