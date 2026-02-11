//! A page fills the entire screen and can have 0..n components.
//!
//! A page draws on the screen by receiving an Event and transforming it into an
//! Action, and then it uses this Action on it's draw function to do whatever it
//! wants.

pub mod empty;
pub mod info;
pub mod torrent_list;
use crate::{
    action::{self, Action},
    app::State,
};
pub use empty::*;
pub use info::*;
pub use ratatui::prelude::*;
pub use torrent_list::*;

pub trait Page {
    /// Draw on the screen, can also call draw on it's components.
    fn draw(&mut self, f: &mut Frame, area: Rect, state: &mut State);

    /// Handle an action, for example, key presses, change to another page, etc.
    fn handle_action(&mut self, action: Action, state: &mut State);

    /// Since pages are trait objects they need an easy way to identify
    /// themselves.
    fn id(&self) -> action::Page;
}
