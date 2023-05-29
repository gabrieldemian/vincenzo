use actix::{ActorContext, Context};
use crossterm::event::KeyCode;
use tui::{
    backend::Backend,
    layout::Constraint,
    widgets::{Block, Borders, Cell, Row, Table, TableState},
    Frame,
};

use crate::frontend::{AppStyle, Frontend};

#[derive(Clone, Debug)]
pub struct TorrentList<'a> {
    pub state: TableState,
    pub items: Vec<Vec<&'a str>>,
}

impl<'a> Default for TorrentList<'a> {
    fn default() -> Self {
        let mut state = TableState::default();
        state.select(Some(0));

        let items = vec![
            vec!["Rust for rustaceans", "1MB", "20", "1", "Downloading"],
            vec!["Movie 4K", "1GB", "57", "5", "Downloading"],
        ];
        Self { state, items }
    }
}

impl<'a> TorrentList<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn keybindings(&mut self, k: KeyCode, ctx: &mut Context<Frontend>, _act: &mut Frontend) {
        match k {
            KeyCode::Char('q') | KeyCode::Esc => ctx.stop(),
            KeyCode::Down | KeyCode::Char('j') => self.next(),
            KeyCode::Up | KeyCode::Char('k') => self.previous(),
            KeyCode::Enter => {}
            _ => {}
        }
    }

    pub fn draw<B: Backend>(&mut self, f: &mut Frame<B>) {
        let style = AppStyle::new();
        let header_cells = ["Name", "Size", "Seeders", "Leechers", "Status"]
            .into_iter()
            .map(|h| Cell::from(h).style(style.normal_style));

        let header = Row::new(header_cells)
            .style(style.normal_style)
            .height(1)
            .bottom_margin(1);

        let rows = self.items.iter().map(|item| {
            let height = item
                .iter()
                .map(|content| content.chars().filter(|c| *c == '\n').count())
                .max()
                .unwrap_or(0)
                + 1;
            let cells = item.iter().map(|c| Cell::from(*c));
            Row::new(cells).height(height as u16)
        });

        let t = Table::new(rows)
            .header(header)
            .block(Block::default().borders(Borders::ALL).title("Torrents"))
            .highlight_style(style.selected_style)
            .style(style.base_style)
            .widths(&[
                Constraint::Percentage(50),
                Constraint::Length(8),
                Constraint::Length(9),
                Constraint::Length(8),
                Constraint::Length(12),
            ]);

        f.render_stateful_widget(t, f.size(), &mut self.state);
    }

    pub fn next(&mut self) {
        let i = self
            .state
            .selected()
            .map_or(0, |v| if v != self.items.len() - 1 { v + 1 } else { 0 });
        self.state.select(Some(i));
    }

    pub fn previous(&mut self) {
        let i = self
            .state
            .selected()
            .map_or(0, |v| if v == 0 { self.items.len() - 1 } else { v - 1 });
        self.state.select(Some(i));
    }
}
