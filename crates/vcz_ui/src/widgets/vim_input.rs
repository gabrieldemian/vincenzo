use ratatui::{
    crossterm,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::{Block, Borders},
    Frame,
};
use std::fmt;
use tui_textarea::{CursorMove, Input, Key, Scrolling, TextArea};
use vincenzo::error::Error;

use crate::PALETTE;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Normal,
    #[default]
    Insert,
    Visual,
    Operator(char),
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        match self {
            Self::Normal => write!(f, "NORMAL"),
            Self::Insert => write!(f, "INSERT"),
            Self::Visual => write!(f, "VISUAL"),
            Self::Operator(c) => write!(f, "OPERATOR({})", c),
        }
    }
}

impl Mode {
    pub(crate) fn block<'a>(&self) -> Block<'a> {
        let help = match self {
            Self::Normal => "Esc to quit, i to enter insert mode",
            Self::Insert => "Esc to enter normal mode, Enter to submit",
            Self::Visual => {
                "type y to yank, type d to delete, type Esc to back to normal \
                 mode"
            }
            Self::Operator(_) => "move cursor to apply operator",
        };
        let title = format!("{} MODE ({})", self, help);
        Block::default().borders(Borders::ALL).title(title)
    }

    pub(crate) fn cursor_style(&self) -> Style {
        let color = match self {
            Self::Normal => Color::Reset,
            Self::Insert => PALETTE.blue,
            Self::Visual => PALETTE.yellow,
            Self::Operator(_) => PALETTE.green,
        };
        Style::default().fg(color).add_modifier(Modifier::REVERSED)
    }
}

// How the Vim emulation state transitions
enum Transition {
    Nop,
    Mode(Mode),
    Pending(Input),
    Quit,
}

// State of Vim emulation
#[derive(Clone)]
pub(crate) struct VimInput<'a> {
    pub mode: Mode,
    pub pending: Input,
    pub textarea: TextArea<'a>,
    pub chars_per_line: usize,
}

impl Default for VimInput<'_> {
    fn default() -> Self {
        let mut textarea = TextArea::default();
        let mode = Mode::default();
        textarea.set_block(mode.block());
        textarea.set_cursor_style(Mode::Normal.cursor_style());
        Self { mode, pending: Input::default(), textarea, chars_per_line: 50 }
    }
}

impl<'a> VimInput<'a> {
    /// Return ok(true) if should quit the input.
    pub fn draw(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        e: Option<crossterm::event::Event>,
    ) -> Result<bool, Error> {
        self.chars_per_line = area.width as usize - 4;

        frame.render_widget(&self.textarea, area);

        let Some(e) = e else { return Ok(false) };

        match self.transition(e.into()) {
            Transition::Mode(mode) if self.mode != mode => {
                self.textarea.set_block(mode.block());
                self.textarea.set_cursor_style(mode.cursor_style());
                self.mode = mode;
            }
            Transition::Pending(input) => self.pending = input,
            Transition::Quit => return Ok(true),
            _ => {}
        };

        Ok(false)
    }

    fn transition(&mut self, input: Input) -> Transition {
        if input.key == Key::Null {
            return Transition::Nop;
        }

        if self.mode == Mode::Normal && (input.key == Key::Esc) {
            return Transition::Quit;
        }

        match self.mode {
            _m @ (Mode::Normal | Mode::Visual | Mode::Operator(_)) => {
                match input {
                    Input { key: Key::Char('h'), .. } => {
                        self.textarea.move_cursor(CursorMove::Back)
                    }
                    Input { key: Key::Char('j'), .. } => {
                        self.textarea.move_cursor(CursorMove::Down)
                    }
                    Input { key: Key::Char('k'), .. } => {
                        self.textarea.move_cursor(CursorMove::Up)
                    }
                    Input { key: Key::Char('l'), .. } => {
                        self.textarea.move_cursor(CursorMove::Forward)
                    }
                    Input { key: Key::Char('w'), .. } => {
                        self.textarea.move_cursor(CursorMove::WordForward)
                    }
                    Input { key: Key::Char('e'), ctrl: false, .. } => {
                        self.textarea.move_cursor(CursorMove::WordEnd);
                        if matches!(self.mode, Mode::Operator(_)) {
                            // Include the text under the cursor
                            self.textarea.move_cursor(CursorMove::Forward);
                        }
                    }
                    Input { key: Key::Char('b'), ctrl: false, .. } => {
                        self.textarea.move_cursor(CursorMove::WordBack)
                    }
                    Input { key: Key::Char('^'), .. } => {
                        self.textarea.move_cursor(CursorMove::Head)
                    }
                    Input { key: Key::Char('$'), .. } => {
                        self.textarea.move_cursor(CursorMove::End)
                    }
                    Input { key: Key::Char('D'), .. } => {
                        self.textarea.delete_line_by_end();
                        return Transition::Mode(Mode::Normal);
                    }
                    Input { key: Key::Char('C'), .. } => {
                        self.textarea.delete_line_by_end();
                        self.textarea.cancel_selection();
                        return Transition::Mode(Mode::Insert);
                    }
                    Input { key: Key::Char('p'), .. } => {
                        self.textarea.paste();
                        return Transition::Mode(Mode::Normal);
                    }
                    Input { key: Key::Char('u'), ctrl: false, .. } => {
                        self.textarea.undo();
                        return Transition::Mode(Mode::Normal);
                    }
                    Input { key: Key::Char('r'), ctrl: true, .. } => {
                        self.textarea.redo();
                        return Transition::Mode(Mode::Normal);
                    }
                    Input { key: Key::Char('x'), .. } => {
                        self.textarea.delete_next_char();
                        return Transition::Mode(Mode::Normal);
                    }
                    Input { key: Key::Char('i'), .. } => {
                        self.textarea.cancel_selection();
                        return Transition::Mode(Mode::Insert);
                    }
                    Input { key: Key::Char('a'), .. } => {
                        self.textarea.cancel_selection();
                        self.textarea.move_cursor(CursorMove::Forward);
                        return Transition::Mode(Mode::Insert);
                    }
                    Input { key: Key::Char('A'), .. } => {
                        self.textarea.cancel_selection();
                        self.textarea.move_cursor(CursorMove::End);
                        return Transition::Mode(Mode::Insert);
                    }
                    Input { key: Key::Char('o'), .. } => {
                        self.textarea.move_cursor(CursorMove::End);
                        self.textarea.insert_newline();
                        return Transition::Mode(Mode::Insert);
                    }
                    Input { key: Key::Char('O'), .. } => {
                        self.textarea.move_cursor(CursorMove::Head);
                        self.textarea.insert_newline();
                        self.textarea.move_cursor(CursorMove::Up);
                        return Transition::Mode(Mode::Insert);
                    }
                    Input { key: Key::Char('I'), .. } => {
                        self.textarea.cancel_selection();
                        self.textarea.move_cursor(CursorMove::Head);
                        return Transition::Mode(Mode::Insert);
                    }
                    Input { key: Key::Char('e'), ctrl: true, .. } => {
                        self.textarea.scroll((1, 0))
                    }
                    Input { key: Key::Char('y'), ctrl: true, .. } => {
                        self.textarea.scroll((-1, 0))
                    }
                    Input { key: Key::Char('d'), ctrl: true, .. } => {
                        self.textarea.scroll(Scrolling::HalfPageDown)
                    }
                    Input { key: Key::Char('u'), ctrl: true, .. } => {
                        self.textarea.scroll(Scrolling::HalfPageUp)
                    }
                    Input { key: Key::Char('f'), ctrl: true, .. } => {
                        self.textarea.scroll(Scrolling::PageDown)
                    }
                    Input { key: Key::Char('b'), ctrl: true, .. } => {
                        self.textarea.scroll(Scrolling::PageUp)
                    }
                    Input { key: Key::Char('v'), ctrl: false, .. }
                        if self.mode == Mode::Normal =>
                    {
                        self.textarea.start_selection();
                        return Transition::Mode(Mode::Visual);
                    }
                    Input { key: Key::Char('V'), ctrl: false, .. }
                        if self.mode == Mode::Normal =>
                    {
                        self.textarea.move_cursor(CursorMove::Head);
                        self.textarea.start_selection();
                        self.textarea.move_cursor(CursorMove::End);
                        return Transition::Mode(Mode::Visual);
                    }
                    Input { key: Key::Esc, .. }
                    | Input { key: Key::Char('v'), ctrl: false, .. }
                        if self.mode == Mode::Visual =>
                    {
                        self.textarea.cancel_selection();
                        return Transition::Mode(Mode::Normal);
                    }
                    Input { key: Key::Char('g'), ctrl: false, .. }
                        if matches!(
                            self.pending,
                            Input { key: Key::Char('g'), ctrl: false, .. }
                        ) =>
                    {
                        self.textarea.move_cursor(CursorMove::Top)
                    }
                    Input { key: Key::Char('G'), ctrl: false, .. } => {
                        self.textarea.move_cursor(CursorMove::Bottom)
                    }
                    Input { key: Key::Char(c), ctrl: false, .. }
                        if self.mode == Mode::Operator(c) =>
                    {
                        // Handle yy, dd, cc. (This is not strictly the same
                        // behavior as Vim)
                        self.textarea.move_cursor(CursorMove::Head);
                        self.textarea.start_selection();
                        let cursor = self.textarea.cursor();
                        self.textarea.move_cursor(CursorMove::Down);
                        if cursor == self.textarea.cursor() {
                            // At the last line, move to end of the line instead
                            self.textarea.move_cursor(CursorMove::End);
                        }
                    }
                    Input {
                        key: Key::Char(op @ ('y' | 'd' | 'c')),
                        ctrl: false,
                        ..
                    } if self.mode == Mode::Normal => {
                        self.textarea.start_selection();
                        return Transition::Mode(Mode::Operator(op));
                    }
                    Input { key: Key::Char('y'), ctrl: false, .. }
                        if self.mode == Mode::Visual =>
                    {
                        // Vim's text selection is inclusive
                        self.textarea.move_cursor(CursorMove::Forward);
                        self.textarea.copy();
                        return Transition::Mode(Mode::Normal);
                    }
                    Input { key: Key::Char('d'), ctrl: false, .. }
                        if self.mode == Mode::Visual =>
                    {
                        // Vim's text selection is inclusive
                        self.textarea.move_cursor(CursorMove::Forward);
                        self.textarea.cut();
                        return Transition::Mode(Mode::Normal);
                    }
                    Input { key: Key::Char('c'), ctrl: false, .. }
                        if self.mode == Mode::Visual =>
                    {
                        // Vim's text selection is inclusive
                        self.textarea.move_cursor(CursorMove::Forward);
                        self.textarea.cut();
                        return Transition::Mode(Mode::Insert);
                    }
                    input => return Transition::Pending(input),
                }

                // Handle the pending operator
                match self.mode {
                    Mode::Operator('y') => {
                        self.textarea.copy();
                        Transition::Mode(Mode::Normal)
                    }
                    Mode::Operator('d') => {
                        self.textarea.cut();
                        Transition::Mode(Mode::Normal)
                    }
                    Mode::Operator('c') => {
                        self.textarea.cut();
                        Transition::Mode(Mode::Insert)
                    }
                    _ => Transition::Nop,
                }
            }
            Mode::Insert => match input {
                Input { key: Key::Esc, .. }
                | Input { key: Key::Char('c'), ctrl: true, .. } => {
                    Transition::Mode(Mode::Normal)
                }
                input => {
                    let last_line = self.textarea.lines().last();
                    let should_break_line = last_line.map_or_else(
                        || false,
                        |v| v.chars().count() > self.chars_per_line,
                    );
                    if should_break_line {
                        self.textarea.insert_newline();
                    }
                    self.textarea.input(input);
                    Transition::Mode(Mode::Insert)
                }
            },
        }
    }
}
