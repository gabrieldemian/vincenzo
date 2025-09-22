use vincenzo::torrent::{InfoHash, TorrentState};

use crate::Input;

/// A new component to be rendered on the UI.
/// Used in conjunction with [`Action`]
#[derive(Clone, Copy)]
pub enum Page {
    // first page to be rendered
    TorrentList,
    // Details,
}

#[derive(Clone)]
pub enum Action {
    Tick,
    Render,
    Quit,
    Error,
    None,

    TerminalEvent(crossterm::event::Event),

    /// First the page will process TerminalEvent and transform it into Input.
    Input(Input),

    NewTorrent(magnet_url::Magnet),
    TogglePause(InfoHash),
    DeleteTorrent(InfoHash),
    TorrentState(TorrentState),
    TorrentStates(Vec<TorrentState>),
}
