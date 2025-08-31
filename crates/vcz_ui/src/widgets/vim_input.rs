use ratatui::{
    Frame, crossterm,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List},
};
use std::fmt;
use tui_textarea::{Input, Key};
use unicode_width::UnicodeWidthStr;

use crate::PALETTE;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    #[default]
    Insert,
    Operator(char),
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        match self {
            Self::Normal => write!(f, "NORMAL"),
            Self::Insert => write!(f, "INSERT"),
            Self::Operator(c) => write!(f, "OPERATOR({})", c),
        }
    }
}

impl Mode {
    pub(crate) fn block<'a>(&self) -> Block<'a> {
        let help = match self {
            Self::Normal => "Esc to quit, i to enter insert mode",
            Self::Insert => "Esc to enter normal mode, Enter to submit",
            Self::Operator(_) => "move cursor to apply operator",
        };
        let title = format!("{} MODE ({})", self, help);
        Block::default().borders(Borders::ALL).title(title)
    }

    pub(crate) fn color(&self) -> Color {
        match self {
            Self::Normal => Color::Reset,
            Self::Insert => PALETTE.green,
            Self::Operator(_) => PALETTE.blue,
        }
    }

    pub(crate) fn cursor_style(&self) -> Style {
        let color = self.color();
        Style::default().fg(color).add_modifier(Modifier::REVERSED)
    }
}

// How the Vim emulation state transitions
pub enum Transition {
    Nop,
    Mode(Mode),
    Pending(Input),
    Quit,
}

/// Vim-like input with modes.
pub struct VimInput<'a> {
    pub mode: Mode,
    pub pending: Input,
    pub chars_per_line: usize,
    lines: Vec<String>,
    cursor: (usize, usize), // (row, col)
    block: Block<'a>,
    style: Style,
    cursor_style: Style,
}

impl Default for VimInput<'_> {
    fn default() -> Self {
        let mode = Mode::default();
        Self {
            block: mode.block(),
            style: Style::default(),
            cursor_style: Style::default(),
            mode,
            lines: vec![],
            cursor: (0, 0),
            pending: Input::default(),
            chars_per_line: 50,
        }
    }
}

impl<'a> VimInput<'a> {
    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        self.chars_per_line = area.width as usize - 4;
        let mut lines = Vec::new();

        self.ensure_cursor_in_bounds();
        // self.adjust_scroll_offset(area.height as usize);

        for (i, line) in self.lines.iter().enumerate() {
            let mut spans = Vec::new();

            if i == self.cursor.0 {
                if line.is_empty() {
                    spans.push(Span::styled(" ", self.mode.cursor_style()));
                } else {
                    for (j, c) in line.chars().enumerate() {
                        if j == self.cursor.1 {
                            spans.push(Span::styled(
                                " ",
                                self.mode.cursor_style(),
                            ));
                        } else {
                            spans.push(Span::raw(c.to_string()));
                        }
                    }

                    // if cursor is at the end of the line, show cursor
                    // indicator
                    if self.cursor.1 >= line.len() {
                        spans.push(Span::styled(" ", self.mode.cursor_style()));
                    }
                }
            } else {
                // For non-cursor lines, just show the text
                spans.push(Span::raw(line.clone()));
            }

            lines.push(Line::from(spans));
        }

        // If we're on a line that doesn't exist yet (shouldn't happen, but just
        // in case)
        if self.cursor.0 >= lines.len() {
            let spans = vec![Span::styled(" ", self.mode.cursor_style())];
            lines.push(Line::from(spans));
        }

        let list = List::new(lines).block(self.block.clone());
        frame.render_widget(list, area);
    }

    pub fn set_block(&mut self, block: Block<'a>) {
        self.block = block;
    }

    pub fn set_style(&mut self, style: Style) {
        self.style = style;
    }

    pub fn set_cursor_style(&mut self, style: Style) {
        self.cursor_style = style;
    }

    pub fn set_placeholder_text(&mut self, _text: &'a str) {}

    pub fn lines(&self) -> Vec<String> {
        self.lines.clone()
    }

    fn ensure_cursor_in_bounds(&mut self) {
        if self.lines.is_empty() {
            self.lines.push(String::new());
            return;
        }

        // ensure cursor row is within bounds
        if self.cursor.0 >= self.lines.len() {
            self.cursor.0 = self.lines.len() - 1;
        }

        // ensure cursor column is within bounds for the current line
        let max_col = self.lines[self.cursor.0].len();
        if self.cursor.1 > max_col {
            self.cursor.1 = max_col;
        }
    }

    fn insert_char(&mut self, c: char) {
        let (row, col) = self.cursor;

        // ensure we have enough lines
        while row >= self.lines.len() {
            self.lines.push(String::new());
        }

        let line = &mut self.lines[row];

        // If we're at the end of the line, just push the character
        if col >= line.len() {
            line.push(c);
        } else {
            // Insert the character at the cursor position
            line.insert(col, c);
        }

        self.cursor.1 += 1;
        if line.len() >= self.chars_per_line {
            self.break_lines_from(row);
        }
        self.ensure_cursor_in_bounds();
    }

    fn break_lines_from(&mut self, start_row: usize) {
        let mut current_row = start_row;

        while current_row < self.lines.len() {
            let line = &self.lines[current_row];

            if line.len() <= self.chars_per_line {
                current_row += 1;
                continue;
            }

            let line_width = UnicodeWidthStr::width(line.as_str());

            if line_width <= self.chars_per_line {
                current_row += 1;
                continue;
            }

            let break_pos = self.chars_per_line.min(line.len());

            if break_pos == 0 || break_pos == line.len() {
                current_row += 1;
                continue;
            }

            // split the line
            let remainder = line[break_pos..].to_string();
            let line_clone = line.clone();
            self.lines[current_row] = line_clone[..break_pos].to_string();

            // insert the remainder
            if current_row + 1 < self.lines.len() {
                let next_line =
                    std::mem::take(&mut self.lines[current_row + 1]);
                self.lines[current_row + 1] = remainder + &next_line;
            } else {
                self.lines.insert(current_row + 1, remainder);
            }

            // adjust cursor position
            if self.cursor.0 == current_row && self.cursor.1 >= break_pos {
                self.cursor.0 += 1;
                self.cursor.1 -= break_pos;
            }

            current_row += 1;
        }
    }

    fn insert_newline(&mut self) {
        let (row, col) = self.cursor;
        let line = &self.lines[row];

        // split the line at the cursor position
        let remainder = line[col..].to_string();
        self.lines[row] = line[..col].to_string();

        // insert a new line with the remainder
        self.lines.insert(row + 1, remainder);

        // move cursor to the beginning of the new line
        self.cursor.0 += 1;
        self.cursor.1 = 0;
    }

    fn backspace(&mut self) {
        let (row, col) = self.cursor;

        if col > 0 {
            // delete character before cursor
            self.lines[row].remove(col - 1);
            self.cursor.1 -= 1;
        } else if row > 0 {
            // merge with previous line
            let current_line = self.lines.remove(row);
            let prev_line_len = self.lines[row - 1].len();
            self.lines[row - 1] += &current_line;
            self.cursor.0 -= 1;
            self.cursor.1 = prev_line_len;
        }
    }

    fn move_cursor_left(&mut self) {
        if self.cursor.1 > 0 {
            self.cursor.1 -= 1;
        } else if self.cursor.0 > 0 {
            self.cursor.0 -= 1;
            self.cursor.1 = self.lines[self.cursor.0].len();
        }
        self.ensure_cursor_in_bounds();
    }

    fn move_cursor_right(&mut self) {
        if self.cursor.1 < self.lines[self.cursor.0].len() {
            self.cursor.1 += 1;
        } else if self.cursor.0 < self.lines.len() - 1 {
            self.cursor.0 += 1;
            self.cursor.1 = 0;
        }
        self.ensure_cursor_in_bounds();
    }

    fn move_cursor_up(&mut self) {
        if self.cursor.0 > 0 {
            self.cursor.0 -= 1;
            self.cursor.1 = self.cursor.1.min(self.lines[self.cursor.0].len());
        }
        self.ensure_cursor_in_bounds();
    }

    fn move_cursor_down(&mut self) {
        if self.cursor.0 < self.lines.len() - 1 {
            self.cursor.0 += 1;
            self.cursor.1 = self.cursor.1.min(self.lines[self.cursor.0].len());
        }
        self.ensure_cursor_in_bounds();
    }

    /// Return true if should quit the input.
    pub fn handle_event(&mut self, e: crossterm::event::Event) -> bool {
        match self.transition(e.into()) {
            Transition::Mode(mode) if self.mode != mode => {
                self.set_block(mode.block());
                self.set_cursor_style(mode.cursor_style());
                self.mode = mode;
            }
            Transition::Pending(input) => self.pending = input,
            Transition::Quit => return true,
            _ => {}
        };
        false
    }

    pub fn transition(&mut self, input: Input) -> Transition {
        if input.key == Key::Null {
            return Transition::Nop;
        }

        if self.mode == Mode::Normal && (input.key == Key::Esc) {
            return Transition::Quit;
        }

        match self.mode {
            m @ (Mode::Normal | Mode::Insert | Mode::Operator(_)) => {
                match input {
                    Input { key: Key::Esc, .. } if m == Mode::Insert => {
                        Transition::Mode(Mode::Normal)
                    }
                    Input { key: Key::Enter, shift: true, .. }
                        if m == Mode::Insert =>
                    {
                        self.insert_newline();
                        Transition::Nop
                    }
                    Input { key: Key::Char('i'), .. } if m == Mode::Normal => {
                        Transition::Mode(Mode::Insert)
                    }
                    Input { key: Key::Char('h'), .. } if m == Mode::Normal => {
                        self.move_cursor_left();
                        Transition::Mode(Mode::Normal)
                    }
                    Input { key: Key::Char('j'), .. } if m == Mode::Normal => {
                        self.move_cursor_down();
                        Transition::Mode(Mode::Normal)
                    }
                    Input { key: Key::Char('k'), .. } if m == Mode::Normal => {
                        self.move_cursor_up();
                        Transition::Mode(Mode::Normal)
                    }
                    Input { key: Key::Char('l'), .. } if m == Mode::Normal => {
                        self.move_cursor_right();
                        Transition::Mode(Mode::Normal)
                    }
                    Input { key: Key::Backspace, .. } => {
                        self.backspace();
                        Transition::Nop
                    }
                    Input { key: Key::Char(c), .. } if m == Mode::Insert => {
                        self.insert_char(c);
                        Transition::Mode(Mode::Insert)
                    }
                    _ => Transition::Nop,
                }

                // Handle the pending operator
                // match self.mode {
                //     Mode::Operator('y') => {
                //         self.textarea.copy();
                //         Transition::Mode(Mode::Normal)
                //     }
                //     Mode::Operator('d') => {
                //         self.textarea.cut();
                //         Transition::Mode(Mode::Normal)
                //     }
                //     Mode::Operator('c') => {
                //         self.textarea.cut();
                //         Transition::Mode(Mode::Insert)
                //     }
                //     _ => Transition::Nop,
                // }
            } /* Mode::Insert => match input {
               *     Input { key: Key::Esc, .. }
               *     | Input { key: Key::Char('c'), ctrl:
               * true, .. } =>
               * {
               *         Transition::Mode(Mode::Normal)
               *     }
               *     input => {
               *         self.insert_char(c);
               *         Transition::Mode(Mode::Insert)
               *     }
               * }, */
        }
    }
}
