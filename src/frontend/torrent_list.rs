use std::sync::Arc;

use crossterm::event::KeyCode;
use ratatui::{
    layout::Constraint,
    prelude::{Backend, Direction, Layout},
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState},
    Terminal,
};

use crate::{
    frontend::AppStyle,
    to_human_readable,
    torrent::{TorrentCtx, TorrentStatus},
};

use super::{FrMsg, FrontendCtx};

#[derive(Clone)]
pub struct TorrentList<'a> {
    pub state: ListState,
    rows: Vec<ListItem<'a>>,
    ctx: Arc<FrontendCtx>,
    style: AppStyle,
    pub torrent_ctxs: Vec<Arc<TorrentCtx>>,
}

impl<'a> TorrentList<'a> {
    pub fn new(ctx: Arc<FrontendCtx>) -> Self {
        let style = AppStyle::new();
        let state = ListState::default();
        let rows = vec![];
        // state.select(Some(0));

        // let mut rows: Vec<ListItem> = Vec::new();
        // let line = Line::from("--------");
        // let name = Line::from("Arch linux iso");
        // let download_and_rate = Line::from("3MiB of 10 MiB 2.1MiB/s");
        // let sl = Line::from("Seeders 5 Leechers 2");
        // let status = Span::styled("Downloading", Style::default().fg(Color::LightBlue)).into();
        // rows.push(ListItem::new(vec![
        //     line.clone(),
        //     name,
        //     download_and_rate,
        //     sl,
        //     status,
        //     line,
        // ]));

        Self {
            torrent_ctxs: vec![],
            ctx,
            style,
            state,
            rows,
        }
    }

    pub async fn keybindings<T: Backend>(&mut self, k: KeyCode, terminal: &mut Terminal<T>) {
        match k {
            KeyCode::Char('q') | KeyCode::Esc => {
                self.ctx.fr_tx.send(FrMsg::Quit).await.unwrap();
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.next();
                self.draw(terminal).await;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.previous();
                self.draw(terminal).await;
            }
            KeyCode::Char('t') => {
                // self.previous();
                self.draw(terminal).await;
            }
            KeyCode::Enter => {}
            _ => {}
        }
    }

    pub async fn draw<T: Backend>(&mut self, terminal: &mut Terminal<T>) {
        let selected = self.state.selected();
        let mut rows: Vec<ListItem> = Vec::new();

        for (i, ctx) in self.torrent_ctxs.iter().enumerate() {
            let info = ctx.info.read().await;
            let stats = ctx.stats.read().await;
            let status = ctx.status.read().await;
            let downloaded = ctx.downloaded.load(std::sync::atomic::Ordering::Relaxed);
            let last_second_downloaded = ctx
                .last_second_downloaded
                .load(std::sync::atomic::Ordering::Relaxed);

            let diff = if downloaded > last_second_downloaded {
                downloaded - last_second_downloaded
            } else {
                0
            };

            ctx.last_second_downloaded
                .fetch_add(diff, std::sync::atomic::Ordering::SeqCst);

            let mut download_rate = to_human_readable(diff as f64);
            download_rate.push_str("/s");

            let name = Span::from(info.name.clone()).bold();

            let status_style = match *status {
                TorrentStatus::Seeding => Style::default().fg(Color::LightGreen),
                TorrentStatus::Error => Style::default().fg(Color::Red),
                _ => Style::default().fg(Color::LightBlue),
            };

            let status_txt: &str = status.clone().into();
            let mut status_txt = vec![Span::styled(status_txt, status_style)];

            if *status == TorrentStatus::Downloading {
                let download_and_rate =
                    format!(" {} - {download_rate}", to_human_readable(downloaded as f64)).into();
                status_txt.push(download_and_rate);
            }

            let s = stats.seeders.to_string();
            let l = stats.leechers.to_string();
            let sl = format!("Seeders {s} Leechers {l}").into();

            let mut line_top = Line::from("-".repeat(terminal.size().unwrap().width as usize));
            let mut line_bottom = line_top.clone();

            if self.state.selected() == Some(i) {
                line_top.patch_style(Style::default().fg(Color::LightBlue));
                line_bottom.patch_style(Style::default().fg(Color::LightBlue));
            }

            let mut items = vec![
                line_top,
                name.into(),
                to_human_readable(info.get_size() as f64).into(),
                sl,
                status_txt.into(),
                line_bottom,
            ];

            if Some(i) != selected && selected > Some(0) {
                items.remove(0);
            }

            rows.push(ListItem::new(items));
        }

        let torrent_list =
            List::new(rows).block(Block::default().borders(Borders::ALL).title("Torrents"));

        terminal
            .draw(|f| {
                // Create two chunks, the body, and the footer
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Percentage(100)].as_ref())
                    .split(f.size());

                f.render_stateful_widget(torrent_list, chunks[0], &mut self.state);
            })
            .unwrap();
    }

    pub async fn update_ctx(&mut self, ctx: Arc<TorrentCtx>) {
        self.torrent_ctxs.push(ctx);
    }

    pub fn next(&mut self) {
        if !self.torrent_ctxs.is_empty() {
            let i = self.state.selected().map_or(0, |v| {
                if v != self.torrent_ctxs.len() - 1 {
                    v + 1
                } else {
                    0
                }
            });
            self.state.select(Some(i));
        }
    }

    pub fn previous(&mut self) {
        if !self.torrent_ctxs.is_empty() {
            let i = self.state.selected().map_or(0, |v| {
                if v == 0 {
                    self.torrent_ctxs.len() - 1
                } else {
                    v - 1
                }
            });
            self.state.select(Some(i));
        }
    }
}
