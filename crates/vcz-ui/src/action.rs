use crate::Input;
use vcz_lib::torrent::{InfoHash, TorrentState};

/// A new component to be rendered on the UI.
/// Used in conjunction with [`Action`]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Page {
    Empty,
    Info,
    // Main page with torrents
    TorrentList,
}

/// Pages handle `Action` in this order:
/// TUI -> page child (optional) OR page
///
/// For example, if a page has a popup, it will handle the event instead of the
/// parent page.
#[derive(Clone, Debug)]
pub enum Action {
    Render,
    Quit,
    Error,
    ChangePage(Page),
    Input(Input),
    NewTorrent(magnet_url::Magnet),
    TogglePause(InfoHash),
    DeleteTorrent(InfoHash),
    TorrentState(TorrentState),
    TorrentStates(Vec<TorrentState>),
}
