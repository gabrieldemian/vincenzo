use ratatui::prelude::*;
use std::sync::LazyLock;

#[derive(Clone, Debug)]
pub struct AppStyle {
    pub base_style: Style,
    pub highlight_bg: Style,
    pub highlight_fg: Style,

    pub base: Color,
    pub primary: Color,
    pub blue: Color,
    pub green: Color,
    pub purple: Color,
    pub yellow: Color,
    pub success: Color,
    pub error: Color,
    pub warning: Color,
}

impl Default for AppStyle {
    fn default() -> Self {
        let base_color = Color::Gray;
        let blue = Color::from_u32(0x0063A7FF);
        let green = Color::from_u32(0x006EEB83);
        let red = Color::from_u32(0x00f86624);
        let yellow = Color::from_u32(0x00f9c80e);
        let purple = Color::from_u32(0x00cd57ff);
        let highlight_fg = Style::default().fg(purple);
        let highlight_bg = Style::default().bg(purple).fg(base_color);

        Self {
            base: base_color,
            primary: purple,
            blue,
            green,
            purple,
            yellow,

            success: green,
            error: red,
            warning: yellow,

            base_style: Style::default().fg(base_color),
            highlight_fg,
            highlight_bg,
        }
    }
}

pub static PALETTE: LazyLock<AppStyle> = LazyLock::new(AppStyle::default);

impl AppStyle {
    pub fn new() -> Self {
        Self::default()
    }
}
