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
    area: Rect,
    cursor_style: Style,
    scroll_offset: usize,
}

impl Default for VimInput<'_> {
    fn default() -> Self {
        let mode = Mode::default();
        Self {
            area: Rect::default(),
            block: mode.block(),
            scroll_offset: 0,
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
        let block_inner = self.block.inner(area);
        let lines_count = block_inner.height as usize;
        self.chars_per_line = block_inner.width as usize;

        self.area = block_inner;
        self.chars_per_line = area.width as usize;

        self.ensure_cursor_in_bounds();

        // render only visible lines
        let mut lines = Vec::new();
        let start_line = self.scroll_offset;
        let end_line = (self.scroll_offset + lines_count).min(self.lines.len());

        for i in start_line..end_line {
            let line = &self.lines[i];
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

                    if self.cursor.1 >= line.len() {
                        spans.push(Span::styled(" ", self.mode.cursor_style()));
                    }
                }
            } else {
                spans.push(Span::raw(line.clone()));
            }

            // add scroll indicators if needed
            if i == start_line && self.scroll_offset > 0 {
                let mut indicator_spans =
                    vec![Span::styled("↑", PALETTE.purple), Span::raw(" ")];
                indicator_spans.extend(spans);
                lines.push(Line::from(indicator_spans));
            } else if i == end_line - 1 && end_line < self.lines.len() {
                let mut indicator_spans =
                    vec![Span::styled("↓", PALETTE.purple), Span::raw(" ")];
                indicator_spans.extend(spans);
                lines.push(Line::from(indicator_spans));
            } else {
                lines.push(Line::from(spans));
            }
        }

        while lines.len() < lines_count {
            lines.push(Line::from(""));
        }

        let list = List::new(lines).block(self.block.clone());
        frame.render_widget(list, area);
    }

    fn adjust_scroll_offset(&mut self) {
        let visible_height = self.area.height as usize;

        // If cursor is above visible area, scroll up
        if self.cursor.0 < self.scroll_offset {
            self.scroll_offset = self.cursor.0;
        }

        // If cursor is below visible area, scroll down
        let bottom_line = self.scroll_offset + visible_height - 1;
        if self.cursor.0 > bottom_line {
            self.scroll_offset = self.cursor.0 - visible_height + 1;
        }

        // Ensure scroll_offset doesn't go beyond bounds
        let max_scroll = self.lines.len().saturating_sub(visible_height);
        self.scroll_offset = self.scroll_offset.min(max_scroll);
        self.scroll_offset = self.scroll_offset.max(0);
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

        let max_scroll =
            self.lines.len().saturating_sub(self.area.height as usize);
        self.scroll_offset = self.scroll_offset.min(max_scroll);
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
        self.adjust_scroll_offset();
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
        self.adjust_scroll_offset();
    }

    fn move_cursor_left(&mut self) {
        if self.cursor.1 > 0 {
            self.cursor.1 -= 1;
        } else if self.cursor.0 > 0 {
            self.cursor.0 -= 1;
            self.cursor.1 = self.lines[self.cursor.0].len();
        }
        self.ensure_cursor_in_bounds();
        self.adjust_scroll_offset();
    }

    fn move_cursor_right(&mut self) {
        if self.cursor.1 < self.lines[self.cursor.0].len() {
            self.cursor.1 += 1;
        } else if self.cursor.0 < self.lines.len() - 1 {
            self.cursor.0 += 1;
            self.cursor.1 = 0;
        }
        self.ensure_cursor_in_bounds();
        self.adjust_scroll_offset();
    }

    fn move_cursor_up(&mut self) {
        if self.cursor.0 > 0 {
            self.cursor.0 -= 1;
            self.cursor.1 = self.cursor.1.min(self.lines[self.cursor.0].len());
        }
        self.ensure_cursor_in_bounds();
        self.adjust_scroll_offset();
    }

    fn move_cursor_down(&mut self) {
        if self.cursor.0 < self.lines.len() - 1 {
            self.cursor.0 += 1;
            self.cursor.1 = self.cursor.1.min(self.lines[self.cursor.0].len());
        }
        self.ensure_cursor_in_bounds();
        self.adjust_scroll_offset();
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

    // Scroll up by X lines
    fn scroll_up(&mut self, lines: usize) {
        let new_scroll = self.scroll_offset.saturating_sub(lines);

        let scroll_amount = self.scroll_offset - new_scroll;

        self.scroll_offset = new_scroll;

        if self.cursor.0 > 0 {
            self.cursor.0 = self.cursor.0.saturating_sub(scroll_amount);
            self.cursor.1 = self.cursor.1.min(self.lines[self.cursor.0].len());
        }

        self.ensure_cursor_in_bounds();
    }

    // Scroll down by X lines
    fn scroll_down(&mut self, lines: usize) {
        let max_scroll =
            self.lines.len().saturating_sub(self.area.height as usize);
        let new_scroll = (self.scroll_offset + lines).min(max_scroll);

        let scroll_amount = new_scroll - self.scroll_offset;

        self.scroll_offset = new_scroll;

        if self.cursor.0 < self.lines.len() - 1 {
            self.cursor.0 =
                (self.cursor.0 + scroll_amount).min(self.lines.len() - 1);
            self.cursor.1 = self.cursor.1.min(self.lines[self.cursor.0].len());
        }

        self.ensure_cursor_in_bounds();
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
                    Input { key: Key::Char('e'), ctrl: true, .. }
                        if m == Mode::Normal =>
                    {
                        self.scroll_down(1);
                        Transition::Nop
                    }
                    Input { key: Key::Char('y'), ctrl: true, .. }
                        if m == Mode::Normal =>
                    {
                        self.scroll_up(1);
                        Transition::Nop
                    }
                    Input { key: Key::Char('d'), ctrl: true, .. }
                        if m == Mode::Normal =>
                    {
                        self.scroll_down(self.area.height.div_ceil(2) as usize);
                        Transition::Nop
                    }
                    Input { key: Key::Char('u'), ctrl: true, .. }
                        if m == Mode::Normal =>
                    {
                        self.scroll_up(self.area.height.div_ceil(2) as usize);
                        Transition::Nop
                    }
                    Input { key: Key::Char('f'), ctrl: true, .. }
                        if m == Mode::Normal =>
                    {
                        self.scroll_down(self.area.height as usize);
                        Transition::Nop
                    }
                    Input { key: Key::Char('b'), ctrl: true, .. }
                        if m == Mode::Normal =>
                    {
                        self.scroll_up(self.area.height as usize);
                        Transition::Nop
                    }

                    Input { key: Key::Char('G'), shift: true, .. }
                        if m == Mode::Normal =>
                    {
                        let last_line = self.lines.len().saturating_sub(1);
                        let last_col = if last_line < self.lines.len() {
                            self.lines[last_line].len()
                        } else {
                            0
                        };
                        self.cursor = (last_line, last_col);
                        self.adjust_scroll_offset();
                        Transition::Mode(Mode::Normal)
                    }

                    // --------
                    // operators
                    // --------
                    Input { key: Key::Char('g'), .. }
                        if self.pending.key == Key::Char('g')
                            && m == Mode::Normal =>
                    {
                        self.cursor = (0, 0);
                        self.scroll_offset = 0;
                        self.pending = Input::default();
                        Transition::Mode(Mode::Normal)
                    }

                    // First 'g' of 'gg' command
                    i @ Input { key: Key::Char('g'), .. }
                        if m == Mode::Normal =>
                    {
                        Transition::Pending(i)
                    }
                    _ => Transition::Nop,
                }
            }
        }
    }
}
