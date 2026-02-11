use vcz_lib::torrent::{InfoHash, TorrentState};

use crate::Input;

/// A new component to be rendered on the UI.
/// Used in conjunction with [`Action`]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Page {
    Empty,
    Info,
    // Main page with torrents
    TorrentList,
}

#[derive(Clone, Debug)]
pub enum Action {
    Tick,
    Render,
    Quit,
    Error,
    None,

    ChangePage(Page),

    TerminalEvent(crossterm::event::Event),

    /// First the page will process TerminalEvent and transform it into Input.
    Input(Input),

    NewTorrent(magnet_url::Magnet),
    TogglePause(InfoHash),
    DeleteTorrent(InfoHash),
    TorrentState(TorrentState),
    TorrentStates(Vec<TorrentState>),
}
