use std::sync::Arc;

use crate::{
    Input, Key, PALETTE,
    action::Action,
    app::State,
    pages::{self},
};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Padding, Paragraph},
};
use tokio::sync::mpsc;
use vcz_lib::{VERSION, config::ResolvedConfig};

pub struct Info<'a> {
    pub tx: mpsc::UnboundedSender<Action>,
    lines: Vec<Line<'a>>,
}

impl<'a> Info<'a> {
    pub fn new(
        tx: mpsc::UnboundedSender<Action>,
        config: &Arc<ResolvedConfig>,
    ) -> Self {
        let lines: [Line; _] = [
            "██╗   ██╗ ██████╗███████╗".into(),
            "██║   ██║██╔════╝╚══███╔╝".into(),
            "██║   ██║██║       ███╔╝ ".into(),
            "╚██╗ ██╔╝██║      ███╔╝  ".into(),
            " ╚████╔╝ ╚██████╗███████╗".into(),
            "  ╚═══╝   ╚═════╝╚══════╝".into(),
            "".into(),
            vec![
                Span::raw("version:").style(PALETTE.primary),
                format!(" {VERSION}").into(),
            ]
            .into(),
            vec![
                Span::raw("peer port:").style(PALETTE.primary),
                format!(" {}", config.local_peer_port).into(),
            ]
            .into(),
            vec![
                Span::raw("daemon addr:").style(PALETTE.primary),
                format!(" {:?}", config.daemon_addr).into(),
            ]
            .into(),
            vec![
                Span::raw("download dir:").style(PALETTE.primary),
                format!(" {}", config.download_dir.to_string_lossy()).into(),
            ]
            .into(),
            vec![
                Span::raw("config file:").style(PALETTE.primary),
                format!(" {}", config.config_file.to_string_lossy()).into(),
            ]
            .into(),
            vec![
                Span::raw("metadata dir:").style(PALETTE.primary),
                format!(" {}", config.metadata_dir.to_string_lossy()).into(),
            ]
            .into(),
            "".into(),
            "-------".into(),
            "".into(),
            Span::raw("https://github.com/gabrieldemian/vincenzo")
                .italic()
                .into(),
            vec![
                Span::raw("by "),
                Span::raw("@gabrieldemian").style(PALETTE.primary),
            ]
            .into(),
        ];
        Self { tx, lines: lines.to_vec() }
    }
}

impl<'a> pages::Page for Info<'a> {
    fn draw(&mut self, f: &mut ratatui::Frame, area: Rect, _: &mut State) {
        let widget = Paragraph::new(self.lines.clone()).centered().block(
            Block::default()
                .borders(Borders::ALL)
                .padding(Padding::vertical(3)),
        );
        f.render_widget(widget, area);
    }

    fn handle_action(&mut self, action: Action, _: &mut State) {
        let Action::Input(input) = action else { return };
        if let Input { key: Key::Char('q'), .. } = input {
            let _ = self.tx.send(Action::Quit);
        }
    }

    fn id(&self) -> crate::action::Page {
        crate::action::Page::Info
    }
}
