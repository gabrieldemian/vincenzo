use ratatui::{
    Frame,
    layout::Rect,
    widgets::{Block, Borders},
};

use crate::{Input, Key};

pub struct ConfirmPopup<'a> {
    block: Block<'a>,
}

impl<'a> ConfirmPopup<'a> {
    pub fn new(title: &'a str) -> Self {
        let block = Block::default().title(title).borders(Borders::ALL);
        Self { block }
    }

    // Returns true if the popup was confirmed
    pub fn handle_event(&mut self, i: &Input) -> bool {
        i.key == Key::Enter
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        //
    }
}
