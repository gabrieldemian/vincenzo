#![feature(if_let_guard)]

pub mod action;
pub mod app;
pub mod error;
pub mod pages;
pub mod tui;
pub mod widgets;
use std::sync::LazyLock;

use ratatui::prelude::*;

#[derive(Clone, Debug)]
pub struct AppStyle {
    pub base_style: Style,
    pub highlight_bg: Style,
    pub highlight_fg: Style,

    pub primary: Color,
    pub success: Color,
    pub error: Color,
    pub warning: Color,
}

impl Default for AppStyle {
    fn default() -> Self {
        Self {
            base_style: Style::default().fg(Color::Gray),
            highlight_fg: Style::default().fg(Color::LightBlue),
            highlight_bg: Style::default()
                .bg(Color::LightBlue)
                .fg(Color::DarkGray),

            primary: Color::LightBlue,
            success: Color::LightGreen,
            error: Color::LightRed,
            warning: Color::Yellow,
        }
    }
}

pub static PALETTE: LazyLock<AppStyle> = LazyLock::new(AppStyle::default);

impl AppStyle {
    pub fn new() -> Self {
        Self::default()
    }
}
