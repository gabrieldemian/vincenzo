//! A page fills the entire screen and can have 0..n components.
//!
//! A page draws on the screen by receiving an Event and transforming it into an
//! Action, and then it uses this Action on it's draw function to do whatever it
//! wants.

use ratatui::Frame;
pub mod torrent_list;

use crate::{action::Action, tui::Event};

pub trait Page {
    /// Draw on the screen, can also call draw on it's components.
    fn draw(&mut self, f: &mut Frame);

    /// Handle an action, for example, key presses, change to another page, etc.
    fn handle_action(&mut self, action: &Action);

    /// get an app event and transform into a page action
    fn get_action(&self, event: Event) -> Action;

    /// Focus on the next component, if available.
    fn focus_next(&mut self);

    /// Focus on the previous component, if available.
    fn focus_prev(&mut self);
}
