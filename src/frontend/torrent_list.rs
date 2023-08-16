use std::sync::Arc;

use crossterm::event::KeyCode;
use ratatui::{
    layout::Constraint,
    prelude::{Backend, Direction, Layout, Rect},
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    Terminal,
};

use crate::{
    to_human_readable,
    torrent::{TorrentCtx, TorrentStatus},
};

use super::{AppStyle, FrMsg, FrontendCtx};

#[derive(Clone)]
pub struct TorrentList<'a> {
    pub style: AppStyle,
    pub state: ListState,
    pub torrent_ctxs: Vec<Arc<TorrentCtx>>,
    ctx: Arc<FrontendCtx>,
    show_popup: bool,
    input: String,
    cursor_position: usize,
    footer: List<'a>,
}

impl<'a> TorrentList<'a> {
    pub fn new(ctx: Arc<FrontendCtx>) -> Self {
        let style = AppStyle::new();
        let state = ListState::default();

        let k: Line = vec![
            Span::styled("k".to_string(), style.highlight_fg),
            " move up   ".into(),
            Span::styled("j".to_string(), style.highlight_fg),
            " move down   ".into(),
            Span::styled("t".to_string(), style.highlight_fg),
            " add torrent   ".into(),
            Span::styled("q".to_string(), style.highlight_fg),
            " quit".into(),
        ]
        .into();

        let line: ListItem = ListItem::new(k);
        let footer_list: Vec<ListItem> = vec![line].into();

        let footer = List::new(footer_list)
            .block(Block::default().borders(Borders::ALL).title("Keybindings"));

        Self {
            style,
            footer,
            cursor_position: 0,
            input: String::new(),
            torrent_ctxs: vec![],
            show_popup: false,
            ctx,
            state,
        }
    }

    pub async fn keybindings<T: Backend>(&mut self, k: KeyCode, terminal: &mut Terminal<T>) {
        match k {
            // KeyCode::Enter => {}
            KeyCode::Char('q') | KeyCode::Esc => {
                self.reset_cursor();
                self.input.clear();
                self.quit(terminal).await;
            }
            k if self.show_popup => match k {
                KeyCode::Enter => self.submit_magnet_link(terminal).await,
                KeyCode::Char(to_insert) => {
                    self.enter_char(to_insert);
                    self.draw(terminal).await;
                }
                KeyCode::Backspace => {
                    self.delete_char();
                    self.draw(terminal).await;
                }
                KeyCode::Left => {
                    self.move_cursor_left();
                    self.draw(terminal).await;
                }
                KeyCode::Right => {
                    self.move_cursor_right();
                    self.draw(terminal).await;
                }
                KeyCode::Esc => {
                    // self.input_mode = InputMode::Normal;
                    self.quit(terminal).await;
                    self.draw(terminal).await;
                }
                _ => {}
            },
            k => match k {
                KeyCode::Down | KeyCode::Char('j') => {
                    self.next();
                    self.draw(terminal).await;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.previous();
                    self.draw(terminal).await;
                }
                KeyCode::Char('t') => {
                    self.show_popup = true;
                    self.draw(terminal).await;
                }
                _ => {}
            },
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
                TorrentStatus::Seeding => self.style.success,
                TorrentStatus::Error => self.style.error,
                _ => self.style.highlight_fg,
            };

            let status_txt: &str = status.clone().into();
            let mut status_txt = vec![Span::styled(status_txt, status_style)];

            if *status == TorrentStatus::Downloading {
                let download_and_rate = format!(
                    " {} - {download_rate}",
                    to_human_readable(downloaded as f64)
                )
                .into();
                status_txt.push(download_and_rate);
            }

            let s = stats.seeders.to_string();
            let l = stats.leechers.to_string();
            let sl = format!("Seeders {s} Leechers {l}").into();

            let mut line_top = Line::from("-".repeat(terminal.size().unwrap().width as usize));
            let mut line_bottom = line_top.clone();

            if self.state.selected() == Some(i) {
                line_top.patch_style(self.style.highlight_fg);
                line_bottom.patch_style(self.style.highlight_fg);
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
                    .constraints([Constraint::Percentage(90), Constraint::Percentage(10)].as_ref())
                    .split(f.size());

                if self.show_popup {
                    let area = self.centered_rect(60, 20, f.size());

                    let input = Paragraph::new(self.input.as_str())
                        .style(self.style.highlight_fg)
                        .block(Block::default().borders(Borders::ALL).title("Add Torrent"));

                    f.render_widget(Clear, area);
                    f.render_widget(input, area);
                    f.set_cursor(area.x + self.cursor_position as u16 + 1, area.y + 1);
                } else {
                    f.render_stateful_widget(torrent_list, chunks[0], &mut self.state);
                    f.render_widget(self.footer.clone(), chunks[1]);
                }
            })
            .unwrap();
    }

    pub async fn update_ctx(&mut self, ctx: Arc<TorrentCtx>) {
        self.torrent_ctxs.push(ctx);
        if self.state.selected().is_none() {
            self.state.select(Some(0));
        }
    }

    fn next(&mut self) {
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

    fn previous(&mut self) {
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

    async fn quit<T: Backend>(&mut self, terminal: &mut Terminal<T>) {
        if self.show_popup {
            self.show_popup = false;
            self.draw(terminal).await;
            self.reset_cursor();
        } else {
            self.ctx.fr_tx.send(FrMsg::Quit).await.unwrap();
        }
    }

    fn centered_rect(&self, percent_x: u16, percent_y: u16, r: Rect) -> Rect {
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
    fn move_cursor_left(&mut self) {
        let cursor_moved_left = self.cursor_position.saturating_sub(1);
        self.cursor_position = self.clamp_cursor(cursor_moved_left);
    }

    fn move_cursor_right(&mut self) {
        let cursor_moved_right = self.cursor_position.saturating_add(1);
        self.cursor_position = self.clamp_cursor(cursor_moved_right);
    }

    fn enter_char(&mut self, new_char: char) {
        self.input.insert(self.cursor_position, new_char);
        self.move_cursor_right();
    }

    fn delete_char(&mut self) {
        let is_not_cursor_leftmost = self.cursor_position != 0;
        if is_not_cursor_leftmost {
            // Method "remove" is not used on the saved text for deleting the selected char.
            // Reason: Using remove on String works on bytes instead of the chars.
            // Using remove would require special care because of char boundaries.

            let current_index = self.cursor_position;
            let from_left_to_current_index = current_index - 1;

            // Getting all characters before the selected character.
            let before_char_to_delete = self.input.chars().take(from_left_to_current_index);
            // Getting all characters after selected character.
            let after_char_to_delete = self.input.chars().skip(current_index);

            // Put all characters together except the selected one.
            // By leaving the selected one out, it is forgotten and therefore deleted.
            self.input = before_char_to_delete.chain(after_char_to_delete).collect();
            self.move_cursor_left();
        }
    }

    fn clamp_cursor(&self, new_cursor_pos: usize) -> usize {
        new_cursor_pos.clamp(0, self.input.len())
    }

    fn reset_cursor(&mut self) {
        self.cursor_position = 0;
    }

    async fn submit_magnet_link<T: Backend>(&mut self, terminal: &mut Terminal<T>) {
        let _ = self
            .ctx
            .fr_tx
            .send(FrMsg::NewTorrent(std::mem::take(&mut self.input)))
            .await;
        self.quit(terminal).await;
    }
}
