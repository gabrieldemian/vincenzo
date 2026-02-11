use crate::{
    Input, Key, PALETTE,
    action::{Action, Page},
    centered_rect,
    pages::{self},
    tui::Event,
    widgets::{VimInput, validate_magnet},
};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, Padding, Paragraph},
};
use tokio::sync::mpsc;

pub struct Empty<'a> {
    pub tx: mpsc::UnboundedSender<Action>,
    textarea: Option<VimInput<'a>>,
    lines: Vec<Line<'a>>,
}

impl<'a> Empty<'a> {
    pub fn new(tx: mpsc::UnboundedSender<Action>) -> Self {
        let lines: [Line; _] = [
            "██╗   ██╗ ██████╗███████╗".into(),
            "██║   ██║██╔════╝╚══███╔╝".into(),
            "██║   ██║██║       ███╔╝ ".into(),
            "╚██╗ ██╔╝██║      ███╔╝  ".into(),
            " ╚████╔╝ ╚██████╗███████╗".into(),
            "  ╚═══╝   ╚═════╝╚══════╝".into(),
            "".into(),
            vec![
                Span::raw("Press "),
                Span::raw("[t] ").style(PALETTE.purple),
                Span::raw("to add a new magnet torrent."),
            ]
            .into(),
            "".into(),
            Span::raw("https://github.com/gabrieldemian/vincenzo")
                .italic()
                .into(),
            vec![
                Span::raw("by "),
                Span::raw("@gabrieldemian").style(PALETTE.purple),
            ]
            .into(),
        ];
        Self { tx, lines: lines.to_vec(), textarea: None }
    }

    fn quit(&mut self) {
        if self.textarea.is_some() {
            self.textarea = None;
        } else {
            let _ = self.tx.send(Action::Quit);
        }
    }
}

impl<'a> pages::Page for Empty<'a> {
    fn draw(&mut self, f: &mut ratatui::Frame, area: Rect) {
        let mut widget = Paragraph::new(self.lines.clone()).centered().block(
            Block::default()
                .borders(Borders::ALL)
                .padding(Padding::vertical(3)),
        );
        if self.textarea.is_some() {
            widget = widget.dim();
        }
        f.render_widget(widget, area);
        if let Some(textarea) = self.textarea.as_mut() {
            let area = centered_rect(60, 20, area);
            f.render_widget(Clear, area);
            textarea.draw(f, area);
        }
    }

    fn handle_event(&mut self, event: Event) -> Action {
        let crate::tui::Event::TerminalEvent(event) = event else {
            return self.get_action(event);
        };
        let i = event.into();
        // if the child component is some, let it handle the event.
        if let Some(textarea) = &mut self.textarea
            && textarea.handle_event(&i)
        {
            self.textarea = None;
            return Action::None;
        }
        Action::Input(i)
    }

    fn handle_action(&mut self, action: Action) {
        if let Action::TorrentStates(ref s) = action
            && !s.is_empty()
        {
            let _ = self.tx.send(Action::ChangePage(Page::TorrentList));
        }

        let Action::Input(input) = action else { return };

        if let Input { key: Key::Char('t'), .. } = input {
            let mut textarea = VimInput::default();
            textarea.set_placeholder_text("Paste magnet link here...");
            self.textarea = Some(textarea);
        }

        if let Some(textarea) = &mut self.textarea
            && let Input { key: Key::Enter, .. } = input
            && let Some(magnet) = validate_magnet(textarea)
        {
            let _ = self.tx.send(Action::NewTorrent(magnet.0));
            self.quit();
            // let _ = self.tx.send(Action::ChangePage(Page::TorrentList));
        }

        if let Input { key: Key::Char('q'), .. } = input {
            self.quit();
        }
    }
}
