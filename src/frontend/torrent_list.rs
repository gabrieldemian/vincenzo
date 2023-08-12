use std::{collections::HashMap, sync::Arc};

use crossterm::event::KeyCode;
use ratatui::{
    layout::Constraint,
    widgets::{Block, Borders, Cell, Row, Table, TableState},
};
use tracing::info;

use crate::{frontend::AppStyle, to_human_readable, torrent::TorrentCtx};

use super::{FrMsg, FrontendCtx};

#[derive(Clone)]
pub struct TorrentList<'a> {
    pub state: TableState,
    ctx: Arc<FrontendCtx>,
    rows: HashMap<[u8; 20], Row<'a>>,
    header: Row<'a>,
    style: AppStyle,
}

impl<'a> TorrentList<'a> {
    pub fn new(ctx: Arc<FrontendCtx>) -> Self {
        let style = AppStyle::new();
        let mut state = TableState::default();

        state.select(Some(0));

        let header_cells = [
            "Name",
            "Downloaded",
            "Size",
            "Seeders",
            "Leechers",
            "Status",
        ]
        .into_iter()
        .map(|h| Cell::from(h).style(style.normal_style));

        let header = Row::new(header_cells)
            .style(style.normal_style)
            .height(1)
            .bottom_margin(1);

        let rows = HashMap::new();

        Self {
            ctx,
            style,
            header,
            rows,
            state,
        }
    }

    pub async fn keybindings(&mut self, k: KeyCode) {
        match k {
            KeyCode::Char('q') | KeyCode::Esc => {
                self.ctx.fr_tx.send(FrMsg::Quit).await.unwrap();
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.next();
                self.draw().await;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.previous();
                self.draw().await;
            }
            KeyCode::Enter => {}
            _ => {}
        }
    }

    pub async fn draw(&mut self) {
        let items: Vec<Row> = self.rows.clone().into_values().collect();
        let t = Table::new(items)
            .header(self.header.clone())
            .block(Block::default().borders(Borders::ALL).title("Torrents"))
            .highlight_style(self.style.selected_style)
            .style(self.style.base_style)
            .widths(&[
                Constraint::Percentage(40), // name
                Constraint::Length(10),     // downloaded
                Constraint::Length(10),     // size
                Constraint::Length(10),     // seeders
                Constraint::Length(10),     // leechers
                Constraint::Length(10),     // status
            ]);

        let mut terminal = self.ctx.terminal.write().await;

        terminal
            .draw(|f| {
                f.render_stateful_widget(t, f.size(), &mut self.state);
            })
            .unwrap();
    }

    pub async fn add_row(&mut self, ctx: Arc<TorrentCtx>) {
        let info = ctx.info.read().await;
        let stats = ctx.stats.read().await;
        let status = ctx.status.read().await;
        let downloaded = ctx.downloaded.load(std::sync::atomic::Ordering::Relaxed);

        let mut item: Vec<String> = Vec::new();
        item.push(info.name.clone());
        item.push(to_human_readable(downloaded as f64));
        item.push(to_human_readable(info.get_size() as f64));
        item.push(stats.seeders.to_string());
        item.push(stats.leechers.to_string());
        item.push(status.clone().into());
        info!("added row on UI {item:#?}");

        let height = item
            .iter()
            .map(|content| content.chars().filter(|c| *c == '\n').count())
            .max()
            .unwrap_or(0)
            + 1;

        let cells = item.into_iter().map(|c| Cell::from(c));
        let row = Row::new(cells).height(height as u16);

        self.rows.insert(ctx.info_hash, row);

        self.draw().await;
    }

    /// Generate all the rows and rerender the entire UI
    pub async fn rerender(&mut self, ctxs: &Vec<Arc<TorrentCtx>>) {
        let mut rows = HashMap::new();
        for ctx in ctxs {
            let info = ctx.info.read().await;
            let stats = ctx.stats.read().await;
            let status = ctx.status.read().await;
            let downloaded = ctx.downloaded.load(std::sync::atomic::Ordering::Relaxed);

            let mut cells = Vec::new();
            cells.push(info.name.clone());
            cells.push(to_human_readable(downloaded as f64));
            cells.push(to_human_readable(info.get_size() as f64));
            cells.push(stats.seeders.to_string());
            cells.push(stats.leechers.to_string());
            cells.push(status.clone().into());

            let height = cells
                .iter()
                .map(|content| content.chars().filter(|c| *c == '\n').count())
                .max()
                .unwrap_or(0)
                + 1;

            let row = Row::new(cells).height(height as u16);

            rows.insert(ctx.info_hash, row);
        }

        self.rows = rows;
        self.draw().await;
    }

    pub fn next(&mut self) {
        if self.rows.len() > 0 {
            let i =
                self.state
                    .selected()
                    .map_or(0, |v| if v != self.rows.len() - 1 { v + 1 } else { 0 });
            self.state.select(Some(i));
        }
    }

    pub fn previous(&mut self) {
        if self.rows.len() > 0 {
            let i =
                self.state
                    .selected()
                    .map_or(0, |v| if v == 0 { self.rows.len() - 1 } else { v - 1 });
            self.state.select(Some(i));
        }
    }
}
