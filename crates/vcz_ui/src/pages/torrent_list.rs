use crossterm::event::{KeyCode, KeyEventKind};
use ratatui::{
    crossterm,
    prelude::*,
    widgets::{
        Block, Borders, Clear, List, ListItem, ListState, Paragraph, Scrollbar,
        ScrollbarOrientation, ScrollbarState,
    },
};
use tokio::sync::mpsc;
use vincenzo::{
    magnet::Magnet,
    torrent::{InfoHash, TorrentState, TorrentStatus},
    utils::to_human_readable,
};

use crate::{
    action::Action,
    tui::Event,
    widgets::{network_chart::NetworkChart, vim_input::VimInput},
    PALETTE,
};

use super::Page;

#[derive(Clone)]
pub struct TorrentList<'a> {
    active_torrent: Option<InfoHash>,
    textarea: Option<VimInput<'a>>,
    pub focused: bool,
    pub scroll_state: ScrollbarState,
    pub scroll: usize,
    pub state: ListState,
    pub torrent_infos: Vec<TorrentState>,
    pub network_charts: Vec<NetworkChart>,
    pub tx: mpsc::UnboundedSender<Action>,
}

impl<'a> TorrentList<'a> {
    pub fn new(tx: mpsc::UnboundedSender<Action>) -> Self {
        Self {
            tx,
            network_charts: Vec::new(),
            state: ListState::default(),
            scroll_state: ScrollbarState::default(),
            scroll: 0,
            textarea: None,
            focused: true,
            active_torrent: None,
            torrent_infos: Vec::new(),
        }
    }

    fn new_network_chart(&mut self, info_hash: InfoHash) {
        let chart = NetworkChart::new(info_hash);
        self.network_charts.push(chart);
    }

    /// Validate that the user's magnet link is valid
    fn validate(&mut self) -> Option<Magnet> {
        let Some(textarea) = &mut self.textarea else { return None };

        let magnet_str = textarea.textarea.lines().join("");
        let magnet_str = magnet_str.trim();
        let magnet = magnet_url::Magnet::new(magnet_str);

        match magnet {
            Ok(magnet) => {
                textarea
                    .textarea
                    .set_style(Style::default().fg(PALETTE.success));
                textarea.textarea.set_block(
                    Block::default()
                        .border_style(PALETTE.success)
                        .borders(Borders::ALL)
                        .title(" Ok (Press Enter) "),
                );
                Some(Magnet(magnet))
            }
            Err(err) => {
                textarea.textarea.set_style(PALETTE.error.into());
                textarea.textarea.set_block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(PALETTE.error)
                        .title(format!(" Err: {err} ")),
                );
                None
            }
        }
    }

    /// Handle the list state and scrollbar when moving to another torrent on
    /// the list.
    fn select_relative(&mut self, offset: isize) {
        if self.torrent_infos.is_empty() {
            return;
        }
        self.state.select(Some(self.state.selected().map_or(0, |s| {
            (s as isize + offset).rem_euclid(self.torrent_infos.len() as isize)
                as usize
        })));
        self.scroll = self.state.selected().unwrap_or(0);
        self.scroll_state = self.scroll_state.position(self.scroll);
    }

    fn next(&mut self) {
        self.select_relative(1);
    }

    fn previous(&mut self) {
        self.select_relative(-1);
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

    fn submit_magnet_link(&mut self, magnet: Magnet) {
        self.new_network_chart(magnet.parse_xt_infohash());
        let _ = self.tx.send(Action::NewTorrent(magnet.0));
        self.quit();
    }

    fn delete_torrent(&mut self) {
        let Some(active_idx) = self.state.selected() else { return };
        let Some(active_info_hash) = &self.active_torrent else { return };

        let _ = self.tx.send(Action::DeleteTorrent(active_info_hash.clone()));

        self.network_charts.retain(|v| v.info_hash != *active_info_hash);
        self.torrent_infos.retain(|v| v.info_hash != *active_info_hash);

        if active_idx == 0 {
            self.state.select(None);
            self.active_torrent = None;
        }

        self.previous();
    }
}

impl<'a> Page for TorrentList<'a> {
    fn draw(
        &mut self,
        f: &mut ratatui::Frame,
        e: Option<crossterm::event::Event>,
    ) {
        let mut torrent_rows: Vec<ListItem> = Vec::new();

        for (i, state) in self.torrent_infos.iter().enumerate() {
            let mut download_rate = to_human_readable(state.download_rate);
            download_rate.push_str("/s");

            let name = Span::from(state.name.clone()).bold();

            let status_style = match state.status {
                TorrentStatus::Seeding => PALETTE.success,
                TorrentStatus::Error => PALETTE.error,
                TorrentStatus::Paused => PALETTE.warning,
                _ => PALETTE.primary,
            };

            let status_txt: &str = state.status.into();
            let mut status_txt = vec![Span::styled(status_txt, status_style)];

            if state.status == TorrentStatus::Downloading {
                let download_and_rate = format!("   {download_rate}",).into();
                status_txt.push(download_and_rate);
            }

            let s = state.stats.seeders.to_string();
            let l = state.stats.leechers.to_string();
            let sl = format!("Seeders {s} Leechers {l}").into();

            let mut line_top = Line::from("-".repeat(f.area().width as usize));
            let mut line_bottom = line_top.clone();

            if self.state.selected() == Some(i) {
                self.active_torrent = Some(state.info_hash.clone());
                line_top = line_top.patch_style(PALETTE.highlight_fg);
                line_bottom = line_bottom.patch_style(PALETTE.highlight_fg);
            }

            // let total = ctx.connected_peers + ctx.idle_peers;
            let mut items = vec![
                line_top,
                name.into(),
                format!(
                    "{} of {}",
                    to_human_readable(state.downloaded as f64),
                    to_human_readable(state.size as f64)
                )
                .into(),
                sl,
                status_txt.into(),
                format!(
                    "Downloading from {} of {} peers",
                    state.downloading_from, state.connected_peers,
                )
                .into(),
                line_bottom,
            ];

            // remove top line of torrents if the select is the first item or
            // none
            if (self.state.selected().is_none()
                || self.state.selected() == Some(0))
                && i > 0
            {
                items.remove(0);
            }

            // remove top line for items below the selected one
            if matches!(
                self.state.selected(),
                Some(s) if s > 0 && i != s && i > s
            ) {
                items.remove(0);
            }

            // remove bottom line for items above the selected one
            if matches!(
                self.state.selected(),
                Some(s) if s > 0 && i != s && i < s
            ) {
                items.remove(items.len() - 1);
            }

            torrent_rows.push(ListItem::new(items));
        }

        self.scroll_state =
            self.scroll_state.content_length(torrent_rows.len());

        let chunks = Layout::horizontal([
            Constraint::Percentage(100),
            Constraint::Min(3),
        ])
        .split(f.area());

        let body_chunk = chunks[0];
        let scrollbar_chunk = chunks[1];

        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(Some("↑"))
                .end_symbol(Some("↓")),
            scrollbar_chunk.inner(Margin { horizontal: 1, vertical: 0 }),
            &mut self.scroll_state,
        );

        let area = self.centered_rect(60, 30, f.area());

        if let Some(textarea) = self.textarea.as_mut() {
            if textarea.draw(f, area, e).unwrap() {
                self.textarea = None;
            }
        } else {
            let block = Block::bordered().title(" Torrents ");
            let has_active_torrent = self.active_torrent.is_some();

            if self.torrent_infos.is_empty() {
                f.render_widget(
                    Paragraph::new("Press [t] to add a new torrent.")
                        .block(block)
                        .centered(),
                    f.area(),
                );
                return;
            }

            let torrent_list = List::new(torrent_rows).block(block);

            // Create two chunks, the body, and the footer
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints(if has_active_torrent {
                    [Constraint::Length(85), Constraint::Length(15)].as_ref()
                } else {
                    [Constraint::Max(100)].as_ref()
                })
                .split(body_chunk);

            f.render_stateful_widget(torrent_list, chunks[0], &mut self.state);

            if has_active_torrent {
                let selected = self.state.selected().unwrap();
                if let Some(network_chart) = self.network_charts.get(selected) {
                    network_chart.draw(f, chunks[1]);
                }
            }
        }
    }
    fn get_action(&self, event: crate::tui::Event) -> crate::action::Action {
        match event {
            Event::Error => Action::None,
            Event::Tick => Action::Tick,
            Event::Render(v) => Action::Render(v),
            Event::Key(key) => Action::Key(key),
            Event::Quit => Action::Quit,
            _ => Action::None,
        }
    }
    fn handle_action(&mut self, action: Action) {
        match action {
            Action::TorrentStates(torrent_states) => {
                for (i, s) in torrent_states.iter().enumerate() {
                    if !self
                        .network_charts
                        .iter()
                        .any(|v| v.info_hash == s.info_hash)
                    {
                        self.new_network_chart(s.info_hash.clone());
                    }
                    if let Some(chart) = self.network_charts.get_mut(i) {
                        chart.on_tick(s.download_rate, s.upload_rate);
                    }
                }

                self.torrent_infos = torrent_states;
            }

            Action::Key(k)
                if k.code == KeyCode::Enter
                    && let Some(_textarea) = &self.textarea =>
            {
                if k.code == KeyCode::Enter {
                    if let Some(magnet) = self.validate() {
                        self.submit_magnet_link(magnet)
                    }
                }
            }

            Action::Key(k)
                if k.kind == KeyEventKind::Press && self.textarea.is_none() =>
            {
                match k.code {
                    KeyCode::Char('q') => {
                        self.quit();
                    }
                    KeyCode::Char('d') => {
                        self.delete_torrent();
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.next();
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.previous();
                    }
                    KeyCode::Char('t') => {
                        let mut textarea = VimInput::default();

                        textarea
                            .textarea
                            .set_placeholder_text("Paste magnet link here...");

                        self.textarea = Some(textarea);
                    }
                    KeyCode::Char('p') => {
                        if let Some(active_torrent) = &self.active_torrent {
                            let _ = self.tx.send(Action::TogglePause(
                                active_torrent.clone(),
                            ));
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    fn focus_next(&mut self) {}
    fn focus_prev(&mut self) {}
}
