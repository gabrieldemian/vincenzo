use crossterm::event::KeyEvent;
use vincenzo::torrent::{InfoHash, TorrentState};

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
    // Key(KeyEvent),

    /// Render another page on the UI
    ChangePage(Page),

    NewTorrent(magnet_url::Magnet),
    TogglePause(InfoHash),
    DeleteTorrent(InfoHash),
    TorrentState(TorrentState),
    TorrentStates(Vec<TorrentState>),
}
