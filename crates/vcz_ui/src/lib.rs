#![feature(if_let_guard)]

use ratatui::layout::{Constraint, Direction, Flex, Layout, Rect};

/// Return a floating centered Rect
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    // let vertical = Layout::vertical([Constraint::Percentage(percent_y)])
    //     .flex(Flex::Center);
    // let horizontal = Layout::horizontal([Constraint::Percentage(percent_x)])
    //     .flex(Flex::Center);
    // let [area] = vertical.areas(r);
    // let [area] = horizontal.areas(area);
    // area

    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Percentage((100 - percent_y) / 2),
                Constraint::Percentage(percent_y),
                Constraint::Percentage((100 - percent_y) / 2),
            ]
            .as_ref(),
        )
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints(
            [
                Constraint::Percentage((100 - percent_x) / 2),
                Constraint::Percentage(percent_x),
                Constraint::Percentage((100 - percent_x) / 2),
            ]
            .as_ref(),
        )
        .split(popup_layout[1])[1]
}

pub mod action;
pub mod app;
pub mod error;
pub mod pages;
pub mod palette;
pub mod tui;
pub mod widgets;
pub use palette::*;
