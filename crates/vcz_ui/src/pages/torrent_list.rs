use crossterm::event::{KeyCode, KeyEventKind};
use hashbrown::HashMap;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};
use tokio::sync::mpsc;
use vincenzo::{
    torrent::{InfoHash, TorrentState, TorrentStatus},
    utils::to_human_readable,
};

use crate::{action::Action, tui::Event, AppStyle};

use super::Page;

#[derive(Clone)]
pub struct TorrentList<'a> {
    active_torrent: Option<InfoHash>,
    cursor_position: usize,
    footer: List<'a>,
    input: String,
    show_popup: bool,
    pub focused: bool,
    pub state: ListState,
    pub style: AppStyle,
    // pub torrent_infos: HashMap<InfoHash, TorrentState>,
    pub torrent_infos: Vec<TorrentState>,
    pub tx: mpsc::UnboundedSender<Action>,
}

impl<'a> TorrentList<'a> {
    pub fn new(tx: mpsc::UnboundedSender<Action>) -> Self {
        let style = AppStyle::new();
        let state = ListState::default();
        let k: Line = vec![
            Span::styled("k".to_string(), style.highlight_fg),
            " move up ".into(),
            Span::styled("j".to_string(), style.highlight_fg),
            " move down ".into(),
            Span::styled("t".to_string(), style.highlight_fg),
            " add torrent ".into(),
            Span::styled("p".to_string(), style.highlight_fg),
            " pause/resume ".into(),
            Span::styled("q".to_string(), style.highlight_fg),
            " quit".into(),
        ]
        .into();

        let line = ListItem::new(k);
        let footer_list: Vec<ListItem> = vec![line];

        let footer = List::new(footer_list)
            .block(Block::default().borders(Borders::ALL).title("Keybindings"));

        Self {
            tx,
            focused: true,
            show_popup: false,
            input: String::new(),
            style,
            state,
            active_torrent: None,
            torrent_infos: Vec::new(),
            cursor_position: 0,
            footer,
        }
    }

    /// Go to the next torrent in the list
    fn next(&mut self) {
        if !self.torrent_infos.is_empty() {
            let i = self.state.selected().map_or(0, |v| {
                if v != self.torrent_infos.len() - 1 {
                    v + 1
                } else {
                    0
                }
            });
            self.state.select(Some(i));
        }
    }

    /// Go to the previous torrent in the list
    fn previous(&mut self) {
        if !self.torrent_infos.is_empty() {
            let i = self.state.selected().map_or(0, |v| {
                if v == 0 {
                    self.torrent_infos.len() - 1
                } else {
                    v - 1
                }
            });
            self.state.select(Some(i));
        }
    }

    fn quit(&mut self) {
        self.input.clear();
        if self.show_popup {
            self.show_popup = false;
            self.reset_cursor();
        } else {
            let _ = self.tx.send(Action::Quit);
        }
    }

    /// Return a floating centered Rect
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
            // Method "remove" is not used on the saved text for deleting the
            // selected char. Reason: Using remove on String works
            // on bytes instead of the chars. Using remove would
            // require special care because of char boundaries.

            let current_index = self.cursor_position;
            let from_left_to_current_index = current_index - 1;

            // Getting all characters before the selected character.
            let before_char_to_delete =
                self.input.chars().take(from_left_to_current_index);

            // Getting all characters after selected character.
            let after_char_to_delete = self.input.chars().skip(current_index);

            // Put all characters together except the selected one.
            // By leaving the selected one out, it is forgotten and therefore
            // deleted.
            self.input =
                before_char_to_delete.chain(after_char_to_delete).collect();

            self.move_cursor_left();
        }
    }

    fn clamp_cursor(&self, new_cursor_pos: usize) -> usize {
        new_cursor_pos.clamp(0, self.input.len())
    }

    fn reset_cursor(&mut self) {
        self.cursor_position = 0;
    }

    fn submit_magnet_link(&mut self) {
        let _ =
            self.tx.send(Action::NewTorrent(std::mem::take(&mut self.input)));

        self.quit();
    }
}

impl<'a> Page for TorrentList<'a> {
    fn draw(&mut self, f: &mut ratatui::Frame) {
        let selected = self.state.selected();
        let mut rows: Vec<ListItem> = Vec::new();

        for (i, state) in self.torrent_infos.iter().enumerate() {
            let mut download_rate =
                to_human_readable(state.download_rate as f64);
            download_rate.push_str("/s");

            let name = Span::from(state.name.clone()).bold();

            let status_style = match state.status {
                TorrentStatus::Seeding => self.style.success,
                TorrentStatus::Error => self.style.error,
                TorrentStatus::Paused => self.style.warning,
                _ => self.style.highlight_fg,
            };

            let status_txt: &str = state.status.clone().into();
            let mut status_txt = vec![Span::styled(status_txt, status_style)];

            if state.status == TorrentStatus::Downloading {
                let download_and_rate = format!(
                    " {} - {download_rate}",
                    to_human_readable(state.downloaded as f64)
                )
                .into();
                status_txt.push(download_and_rate);
            }

            let s = state.stats.seeders.to_string();
            let l = state.stats.leechers.to_string();
            let sl = format!("Seeders {s} Leechers {l}").into();

            let mut line_top = Line::from("-".repeat(f.area().width as usize));
            let mut line_bottom = line_top.clone();

            if self.state.selected() == Some(i) {
                line_top = line_top.patch_style(self.style.highlight_fg);
                line_bottom = line_bottom.patch_style(self.style.highlight_fg);
            }

            // let total = ctx.connected_peers + ctx.idle_peers;
            let mut items = vec![
                line_top,
                name.into(),
                to_human_readable(state.size as f64).into(),
                sl,
                status_txt.into(),
                format!("Connected to {} peers", state.connected_peers).into(),
                line_bottom,
            ];

            if Some(i) == selected {
                self.active_torrent = Some(state.info_hash.clone());
            }

            if Some(i) != selected && selected > Some(0) {
                items.remove(0);
            }

            rows.push(ListItem::new(items));
        }

        let torrent_list = List::new(rows)
            .block(Block::default().borders(Borders::ALL).title("Torrents"));

        // Create two chunks, the body, and the footer
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Max(98), Constraint::Length(3)].as_ref())
            .split(f.area());

        if self.show_popup {
            let area = self.centered_rect(60, 20, f.area());

            let input = Paragraph::new(self.input.as_str())
                .style(self.style.highlight_fg)
                .block(
                    Block::default().borders(Borders::ALL).title("Add Torrent"),
                );

            f.render_widget(Clear, area);
            f.render_widget(input, area);
            f.set_cursor_position(Position {
                x: area.x + self.cursor_position as u16 + 1,
                y: area.y + 1,
            });
        } else {
            f.render_stateful_widget(torrent_list, chunks[0], &mut self.state);
            f.render_widget(self.footer.clone(), chunks[1]);
        }
    }
    fn get_action(&self, event: crate::tui::Event) -> crate::action::Action {
        match event {
            Event::Error => Action::None,
            Event::Tick => Action::Tick,
            Event::Render => Action::Render,
            Event::Key(key) => Action::Key(key),
            Event::Quit => Action::Quit,
            _ => Action::None,
        }
    }
    fn handle_action(&mut self, action: Action) {
        match action {
            Action::TorrentStates(torrent_states) => {
                self.torrent_infos = torrent_states;
            }
            Action::Key(k)
                if self.show_popup && k.kind == KeyEventKind::Press =>
            {
                match k.code {
                    KeyCode::Enter => self.submit_magnet_link(),
                    KeyCode::Esc => {
                        self.input = "".to_string();
                        self.reset_cursor();
                        self.show_popup = false;
                    }
                    KeyCode::Char(to_insert) => {
                        self.enter_char(to_insert);
                    }
                    KeyCode::Backspace => {
                        self.delete_char();
                    }
                    KeyCode::Left => {
                        self.move_cursor_left();
                    }
                    KeyCode::Right => {
                        self.move_cursor_right();
                    }
                    _ => {}
                }
            }
            Action::Key(k) if k.kind == KeyEventKind::Press => match k.code {
                KeyCode::Char('q') | KeyCode::Esc => {
                    self.quit();
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.next();
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.previous();
                }
                KeyCode::Char('t') => {
                    self.show_popup = true;
                }
                KeyCode::Char('p') => {
                    if let Some(active_torrent) = &self.active_torrent {
                        let _ = self
                            .tx
                            .send(Action::TogglePause(active_torrent.clone()));
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
    fn focus_next(&mut self) {}
    fn focus_prev(&mut self) {}
}
