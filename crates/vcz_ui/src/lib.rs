pub mod action;
pub mod app;
pub mod error;
pub mod pages;
pub mod tui;
use ratatui::prelude::*;

#[derive(Clone, Debug)]
pub struct AppStyle {
    pub base_style: Style,
    pub highlight_bg: Style,
    pub highlight_fg: Style,
    pub success: Style,
    pub error: Style,
    pub warning: Style,
}

impl Default for AppStyle {
    fn default() -> Self {
        Self::new()
    }
}

impl AppStyle {
    pub fn new() -> Self {
        AppStyle {
            base_style: Style::default().fg(Color::Gray),
            highlight_bg: Style::default()
                .bg(Color::LightBlue)
                .fg(Color::DarkGray),
            highlight_fg: Style::default().fg(Color::LightBlue),
            success: Style::default().fg(Color::LightGreen),
            error: Style::default().fg(Color::Red),
            warning: Style::default().fg(Color::Yellow),
        }
    }
}
