use crossterm::event::KeyEvent;
use vincenzo::torrent::TorrentState;

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
    Key(KeyEvent),
    Quit,
    Render,
    None,

    /// Render another page on the UI
    ChangePage(Page),

    NewTorrent(String),
    TogglePause([u8; 20]),
    TorrentState(TorrentState),
}
