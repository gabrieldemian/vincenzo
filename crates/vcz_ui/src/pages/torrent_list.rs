use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers, ModifierKeyCode};
use magnet_url::Magnet;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};
use tokio::sync::mpsc;
use tui_textarea::{CursorMove, TextArea};
use vincenzo::{
    torrent::{InfoHash, TorrentState, TorrentStatus},
    utils::to_human_readable,
};

use crate::{action::Action, tui::Event, AppStyle};

use super::Page;

#[derive(Clone)]
pub struct TorrentList<'a> {
    active_torrent: Option<InfoHash>,
    footer: List<'a>,
    textarea: Option<TextArea<'a>>,
    chars_per_line: u16,
    pub focused: bool,
    pub state: ListState,
    pub style: AppStyle,
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
            Span::styled("d".to_string(), style.highlight_fg),
            " delete ".into(),
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
            chars_per_line: 50,
            textarea: None,
            focused: true,
            style,
            state,
            active_torrent: None,
            torrent_infos: Vec::new(),
            footer,
        }
    }

    /// Validate that the user's magnet link is valid
    fn validate(&mut self) -> bool {
        let Some(textarea) = &mut self.textarea else { return false };

        let magnet_str = textarea.lines().join("");
        let magnet = Magnet::new(&magnet_str);

        if let Err(err) = magnet {
            textarea.set_style(Style::default().fg(Color::LightRed));
            textarea.set_block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Color::LightRed)
                    .title(format!("Err: {err}")),
            );
            false
        } else {
            textarea.set_style(Style::default().fg(Color::LightGreen));
            textarea.set_block(
                Block::default()
                    .border_style(Color::LightGreen)
                    .borders(Borders::ALL)
                    .title("Ok (Press Enter)"),
            );
            true
        }
    }

    /// Go to the next torrent in the list
    fn next(&mut self) {
        if self.torrent_infos.is_empty() {
            return;
        }
        let i = self.state.selected().map_or(0, |v| {
            if v != self.torrent_infos.len() - 1 {
                v + 1
            } else {
                0
            }
        });
        self.state.select(Some(i));
    }

    /// Go to the previous torrent in the list
    fn previous(&mut self) {
        if self.torrent_infos.is_empty() {
            return;
        }
        let i = self.state.selected().map_or(0, |v| {
            if v == 0 {
                self.torrent_infos.len() - 1
            } else {
                v - 1
            }
        });
        self.state.select(Some(i));
    }

    fn quit(&mut self) {
        if self.textarea.is_some() {
            self.textarea = None;
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

    fn submit_magnet_link(&mut self) {
        if let Some(textarea) = &self.textarea {
            let magnet_str = textarea.lines().join("").trim().to_string();
            let _ = self.tx.send(Action::NewTorrent(magnet_str));
            self.quit();
        }
    }

    fn delete_torrent(&self) {
        if let Some(active_torrent) = &self.active_torrent {
            let _ = self.tx.send(Action::DeleteTorrent(active_torrent.clone()));
        }
    }
}

impl<'a> Page for TorrentList<'a> {
    fn draw(&mut self, f: &mut ratatui::Frame) {
        let selected = self.state.selected();
        let mut rows: Vec<ListItem> = Vec::new();

        for (i, state) in self.torrent_infos.iter().enumerate() {
            let mut download_rate = to_human_readable(state.download_rate);
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
                    to_human_readable(state.downloaded)
                )
                .into();
                status_txt.push(download_and_rate);
            }

            let s = state.stats.seeders.to_string();
            let l = state.stats.leechers.to_string();
            let sl = format!("Seeders {s} Leechers {l}").into();

            let mut line_top = Line::from("-".repeat(f.area().width as usize));
            let mut line_bottom = line_top.clone();

            if selected == Some(i) {
                self.active_torrent = Some(state.info_hash.clone());
                line_top = line_top.patch_style(self.style.highlight_fg);
                line_bottom = line_bottom.patch_style(self.style.highlight_fg);
            }

            // let total = ctx.connected_peers + ctx.idle_peers;
            let mut items = vec![
                line_top,
                name.into(),
                to_human_readable(state.size).into(),
                sl,
                status_txt.into(),
                format!(
                    "Downloading from {} of {} peers",
                    state.downloading_from, state.connected_peers,
                )
                .into(),
                line_bottom,
            ];

            if (selected.is_none() || selected == Some(0)) && i > 0 {
                items.remove(0);
            }

            if matches!(
                selected,
                Some(s) if s > i
            ) {
                items.remove(items.len() - 1);
            }

            rows.push(ListItem::new(items));
        }

        let torrent_list = List::new(rows)
            .block(Block::default().borders(Borders::ALL).title("Torrents"));

        if let Some(textarea) = &self.textarea {
            let area = self.centered_rect(60, 30, f.area());
            self.chars_per_line = area.width - 4;

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints(
                    [Constraint::Min(0), Constraint::Length(1)].as_ref(),
                )
                .split(area);

            f.render_widget(Clear, area);
            f.render_widget(textarea, chunks[0]);

            f.render_widget(Paragraph::new("Shift + [C]lear"), chunks[1]);
        } else {
            // Create two chunks, the body, and the footer
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints(
                    [Constraint::Max(98), Constraint::Length(3)].as_ref(),
                )
                .split(f.area());

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
                if let Some(textarea) = &mut self.textarea
                    && k.kind == KeyEventKind::Press =>
            {
                match k.code {
                    KeyCode::Enter => self.submit_magnet_link(),
                    KeyCode::Esc => {
                        self.textarea = None;
                    }
                    KeyCode::Modifier(ModifierKeyCode::LeftShift) => {}
                    KeyCode::Char(char) => {
                        if k.modifiers.intersects(KeyModifiers::SHIFT)
                            && char == 'C'
                        {
                            for _ in 0..textarea
                                .lines()
                                .iter()
                                .fold(0, |acc, v| acc + v.chars().count())
                            {
                                textarea.delete_word();
                            }
                        } else {
                            if textarea.cursor().1
                                >= self.chars_per_line as usize
                            {
                                textarea.insert_newline();
                            }
                            textarea.insert_char(char);
                        }
                        self.validate();
                    }
                    KeyCode::Backspace => {
                        textarea.delete_char();
                        self.validate();
                    }
                    KeyCode::Up => {
                        textarea.move_cursor(CursorMove::Up);
                    }
                    KeyCode::Down => {
                        textarea.move_cursor(CursorMove::Down);
                    }
                    KeyCode::Left => {
                        textarea.move_cursor(CursorMove::Back);
                    }
                    KeyCode::Right => {
                        textarea.move_cursor(CursorMove::Forward);
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
                KeyCode::Char('d') => {
                    self.delete_torrent();
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.previous();
                }
                KeyCode::Char('t') => {
                    let mut textarea = TextArea::default();

                    textarea.set_placeholder_text("Paste magnet link here...");
                    textarea.set_block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title("Add Torrent (Press Enter or Esc)"),
                    );
                    self.textarea = Some(textarea);
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
