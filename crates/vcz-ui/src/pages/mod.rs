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
    tui::Event,
};

pub use empty::*;
pub use info::*;
pub use ratatui::prelude::*;
pub use torrent_list::*;

pub trait Page {
    /// Draw on the screen, can also call draw on it's components.
    fn draw(&mut self, f: &mut Frame, area: Rect, state: &mut State);

    fn handle_event(&mut self, event: Event, state: &mut State) -> Action;

    /// Handle an action, for example, key presses, change to another page, etc.
    fn handle_action(&mut self, action: Action, state: &mut State);

    /// get an app event and transform into a page action
    fn get_action(&self, event: Event) -> Action {
        match event {
            Event::Tick => Action::Tick,
            Event::Render => Action::Render,
            Event::Quit => Action::Quit,
            Event::Error => Action::Error,
            Event::TerminalEvent(e) => Action::TerminalEvent(e),
        }
    }

    fn id(&self) -> action::Page;
}
