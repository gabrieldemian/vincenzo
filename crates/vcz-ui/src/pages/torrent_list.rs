use crate::{
    Input, Key, PALETTE,
    action::Action,
    app, centered_rect,
    widgets::{NetworkChart, VimInput, validate_magnet},
};
use ratatui::{
    style::Styled,
    widgets::{
        Block, Borders, Clear, List, ListItem, ListState, Scrollbar,
        ScrollbarOrientation, ScrollbarState,
    },
};
use tokio::sync::mpsc;
use vcz_lib::{
    magnet::Magnet,
    torrent::{InfoHash, TorrentState, TorrentStatus},
    utils::to_human_readable,
};

use super::*;

#[repr(u8)]
#[derive(Clone, Copy, Default, PartialEq)]
enum SearchStatus {
    /// User is typing in the seach bar and can't do anything else except Esc.
    Typing,
    /// User pressed Esc and is now moving in the torrent list.
    #[default]
    Moving,
}

pub struct TorrentList<'a> {
    active_torrent: Option<InfoHash>,
    textarea: Option<VimInput<'a>>,
    search: String,
    search_status: SearchStatus,
    pub scroll_state: ScrollbarState,
    pub scroll: usize,
    pub state: ListState,
    pub torrent_infos: Vec<TorrentState>,
    pub rendered_torrent_infos: Vec<usize>,
    pub network_charts: Vec<NetworkChart>,
    pub tx: mpsc::UnboundedSender<Action>,
}

impl<'a> TorrentList<'a> {
    pub fn new(tx: mpsc::UnboundedSender<Action>) -> Self {
        Self {
            tx,
            network_charts: Vec::new(),
            search: String::new(),
            search_status: SearchStatus::default(),
            state: ListState::default(),
            scroll_state: ScrollbarState::default(),
            scroll: 0,
            textarea: None,
            active_torrent: None,
            torrent_infos: Vec::new(),
            rendered_torrent_infos: Vec::new(),
        }
    }

    fn search(&mut self) {
        let SearchStatus::Typing = self.search_status else { return };
        self.rendered_torrent_infos.clear();
        for (i, _) in self
            .torrent_infos
            .iter()
            .enumerate()
            .filter(|v| v.1.name.starts_with(&self.search))
        {
            self.rendered_torrent_infos.push(i);
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

    #[inline]
    fn next(&mut self) {
        self.select_relative(1);
    }

    #[inline]
    fn previous(&mut self) {
        self.select_relative(-1);
    }

    fn submit_magnet_link(&mut self, magnet: Magnet) {
        let _ = self.tx.send(Action::NewTorrent(magnet.0));
        let _ = self.tx.send(Action::Quit);
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
        area: Rect,
        state: &mut app::State,
    ) {
        let mut torrent_rows: Vec<ListItem> =
            Vec::with_capacity(self.torrent_infos.len());

        for (i, rendered_i) in self.rendered_torrent_infos.iter().enumerate() {
            let state = &self.torrent_infos[*rendered_i];
            let mut download_rate = to_human_readable(state.download_rate);
            download_rate.push_str("/s");

            let mut name = Span::from(state.name.clone()).bold();

            let status_style = match state.status {
                TorrentStatus::Seeding => PALETTE.success,
                TorrentStatus::Error(_) => PALETTE.error,
                TorrentStatus::Paused => PALETTE.warning,
                TorrentStatus::Downloading => PALETTE.blue,
                _ => PALETTE.primary,
            };

            let status_txt: &str = state.status.into();
            let mut status_txt = vec![Span::styled(status_txt, status_style)];

            if state.status == TorrentStatus::Downloading {
                let download_and_rate = format!("  {download_rate}",).into();
                status_txt.push(download_and_rate);
            }

            let s = state.stats.seeders.to_string();
            let l = state.stats.leechers.to_string();
            let sl = format!("Seeders {s} Leechers {l}").into();

            let mut line_top = Line::from("-".repeat(area.width as usize));
            let mut line_bottom = line_top.clone();

            if self.state.selected() == Some(i) {
                self.active_torrent = Some(state.info_hash.clone());
                line_top = line_top.patch_style(PALETTE.highlight_fg);
                line_bottom = line_bottom.patch_style(PALETTE.highlight_fg);
                name = name.fg(PALETTE.primary);
            }

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
                if let TorrentStatus::Error(e) = state.status {
                    e.to_string().set_style(PALETTE.error).into()
                } else {
                    format!(
                        "Downloading from {} of {} peers",
                        state.downloading_from, state.connected_peers,
                    )
                    .into()
                },
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
        .split(area);

        let body_chunk = chunks[0];
        let scrollbar_chunk = chunks[1];

        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(Some("↑"))
                .end_symbol(Some("↓")),
            scrollbar_chunk.inner(Margin { horizontal: 1, vertical: 0 }),
            &mut self.scroll_state,
        );

        let has_active_torrent = self.active_torrent.is_some();

        let mut block =
            Block::default().borders(Borders::ALL).title(" Torrents ");

        if self.search_status == SearchStatus::Typing {
            block = block.title_bottom(format!(
                " Search: {} (press enter) ",
                self.search
            ));
        }

        if self.search_status == SearchStatus::Moving && !self.search.is_empty()
        {
            block = block.title_bottom(format!(" Search: {} ", self.search));
        }

        let mut torrent_list = List::new(torrent_rows).block(block);

        if state.should_dim {
            torrent_list = torrent_list.dim();
        }

        // one chunk for the torrent list, another for the network chart
        let chunks =
            Layout::vertical(if has_active_torrent && state.show_network {
                [Constraint::Length(85), Constraint::Length(15)].as_ref()
            } else {
                [Constraint::Max(100)].as_ref()
            })
            .split(body_chunk);

        f.render_stateful_widget(torrent_list, chunks[0], &mut self.state);

        if has_active_torrent && state.show_network {
            let Some(rendered_i) = self.state.selected() else { return };
            let Some(info_i) =
                self.rendered_torrent_infos.get(rendered_i).cloned()
            else {
                return;
            };
            if let Some(network_chart) = self.network_charts.get(info_i) {
                network_chart.draw(f, chunks[1], self.textarea.is_some());
            }
        }

        if let Some(textarea) = self.textarea.as_mut() {
            let area = centered_rect(60, 20, area);
            f.render_widget(Clear, area);
            textarea.draw(f, area);
        }
    }

    fn handle_action(&mut self, action: Action, state: &mut app::State) {
        if let Action::Input(ref input) = action {
            // if textarea is rendered, let it handle the event first.
            if let Some(textarea) = &mut self.textarea
                && textarea.handle_event(input)
            {
                self.textarea = None;
                state.should_dim = false;
                return;
            }
        }

        match action {
            Action::TorrentStates(torrent_states) => {
                for (i, s) in torrent_states.iter().enumerate() {
                    if !self
                        .network_charts
                        .iter()
                        .any(|v| v.info_hash == s.info_hash)
                    {
                        let chart = NetworkChart::new(s.info_hash.clone());
                        self.network_charts.push(chart);
                    }
                    if let Some(chart) = self.network_charts.get_mut(i) {
                        chart.on_tick(s.download_rate, s.upload_rate);
                    }
                }
                if self.search.is_empty() {
                    self.rendered_torrent_infos =
                        (0..torrent_states.len()).collect();
                }
                self.torrent_infos = torrent_states;
            }

            Action::Input(input)
                if let SearchStatus::Moving = &self.search_status
                    && let Input { key: Key::Esc, .. } = input =>
            {
                self.rendered_torrent_infos =
                    (0..self.torrent_infos.len()).collect();
                self.search.clear();
            }

            Action::Input(input)
                if let SearchStatus::Typing = &self.search_status =>
            {
                match input {
                    Input { key: Key::Char(c), .. } => {
                        self.search.push(c);
                        self.search();
                    }
                    Input { key: Key::Esc, .. } => {
                        self.rendered_torrent_infos =
                            (0..self.torrent_infos.len()).collect();
                        self.search_status = SearchStatus::Moving;
                    }
                    Input { key: Key::Enter, .. } => {
                        self.search_status = SearchStatus::Moving;
                        self.next();
                    }
                    Input { key: Key::Backspace, .. } => {
                        self.search.pop();
                        self.search();
                    }
                    _ => {}
                }
            }

            Action::Input(input) if self.textarea.is_some() => {
                if let Some(textarea) = &mut self.textarea
                    && let Input { key: Key::Enter, .. } = input
                    && let Some(magnet) = validate_magnet(textarea)
                {
                    self.submit_magnet_link(magnet);
                }
            }

            Action::Input(input) if self.textarea.is_none() => match input {
                Input { key: Key::Char('/'), .. } => {
                    self.state.select(None);
                    self.active_torrent = None;
                    self.search_status = SearchStatus::Typing;
                }
                Input { key: Key::Char('q'), .. } => {
                    state.should_dim = false;
                    let _ = self.tx.send(Action::Quit);
                }
                Input { key: Key::Char('g'), .. } => {
                    if self.torrent_infos.is_empty() {
                        return;
                    }
                    self.state.select(Some(0));
                    self.scroll =
                        unsafe { self.state.selected().unwrap_unchecked() };
                    self.scroll_state = self.scroll_state.position(self.scroll);
                }
                Input { key: Key::Char('G'), .. } => {
                    if self.torrent_infos.is_empty() {
                        return;
                    }
                    self.state.select(Some(self.torrent_infos.len() - 1));
                    self.scroll =
                        unsafe { self.state.selected().unwrap_unchecked() };
                    self.scroll_state = self.scroll_state.position(self.scroll);
                }
                Input { key: Key::Char('d'), .. } => {
                    self.delete_torrent();
                }
                Input { key: Key::Char('j'), .. } => {
                    self.next();
                }
                Input { key: Key::Char('k'), .. } => {
                    self.previous();
                }
                Input { key: Key::Char('n'), .. } => {
                    state.show_network = !state.show_network;
                }
                Input { key: Key::Char('t'), .. } => {
                    let mut textarea = VimInput::default();
                    textarea.set_placeholder_text("Paste magnet link here...");
                    state.should_dim = true;
                    self.textarea = Some(textarea);
                }
                Input { key: Key::Char('p'), .. }
                    if let Some(active_torrent) = &self.active_torrent =>
                {
                    let _ = self
                        .tx
                        .send(Action::TogglePause(active_torrent.clone()));
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn id(&self) -> crate::action::Page {
        crate::action::Page::TorrentList
    }
}
