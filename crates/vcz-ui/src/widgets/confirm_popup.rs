use ratatui::{
    prelude::*,
    widgets::{Block, Borders, List, Padding},
};

use crate::{Input, Key, PALETTE};

pub struct ConfirmPopup<'a> {
    block: Block<'a>,
    pub also_delete_from_disk: bool,
}

impl<'a> ConfirmPopup<'a> {
    pub fn new(title: String) -> Self {
        let title = Line::from(vec![
            format!(" Delete {title}? ").into(),
            Span::raw("(╯°o°）╯︵ ┻━┻ ").style(PALETTE.gray),
        ]);
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .title_bottom(Line::from(vec![
                " ".into(),
                Span::raw("[d]").style(PALETTE.purple),
                Span::raw("elete files ").style(PALETTE.gray),
            ]))
            .title_bottom(Line::from(vec![
                Span::raw(" [Esc] ").style(PALETTE.purple),
            ]))
            .title_bottom(Line::from(vec![
                Span::raw(" [Enter] ").style(PALETTE.purple),
            ]));
        Self { block, also_delete_from_disk: false }
    }

    // Returns true if the popup was confirmed
    pub fn handle_event(&mut self, i: &Input) {
        if i.key == Key::Char('d') {
            self.also_delete_from_disk = !self.also_delete_from_disk;
        }
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let second_line: Line = if self.also_delete_from_disk {
            vec![
                Span::raw("[ "),
                Span::raw("X").style(PALETTE.purple),
                Span::raw(" ] "),
                Span::raw("Also delete files"),
            ]
            .into()
        } else {
            Line::raw("[   ] Also delete files")
        };

        let t: List = List::new([
            "Are you sure that you want to delete it?".into(),
            "This action cannot be undone.".into(),
            "".into(),
            second_line,
        ])
        .block(self.block.clone().padding(Padding::new(1, 1, 1, 1)));

        frame.render_widget(t, area);
    }
}
