use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List},
};
use std::fmt;
use unicode_width::UnicodeWidthStr;
use vcz_lib::magnet::Magnet;

use crate::{Input, Key, PALETTE};

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
        let help: Line = match self {
            Self::Normal => Line::from(vec![
                Span::raw(" [Esc]").style(PALETTE.purple),
                Span::raw(" [i] ").style(PALETTE.purple),
            ]),
            Self::Insert => Line::from(vec![
                Span::raw(" [Esc]").style(PALETTE.purple),
                Span::raw(" [Enter] ").style(PALETTE.purple),
            ]),
            Self::Operator(_) => " move cursor to apply operator ".into(),
        };
        let title = Span::raw(format!(" {self} ")).style(self.color());
        Block::default()
            .borders(Borders::ALL)
            .title_top(" Add new torrent with magnet url ")
            .title_bottom(title)
            .title_bottom(help)
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
pub(crate) enum Transition {
    Nop,
    Mode(Mode),
    Pending(Input),
    Quit,
}

/// Vim-like input with modes.
pub(crate) struct VimInput<'a> {
    pub mode: Mode,
    pending: Input,
    chars_per_line: usize,
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
        let m = Mode::default();
        Self {
            mode: m,
            pending: Input::default(),
            chars_per_line: 50,
            lines: vec!["".to_string()],
            cursor: (0, 0),
            block: m.block(),
            style: Style::default(),
            area: Rect::default(),
            cursor_style: m.cursor_style(),
            scroll_offset: 0,
        }
    }
}

impl<'a> VimInput<'a> {
    /// Return true if should quit the input.
    pub(crate) fn handle_event(&mut self, e: &Input) -> bool {
        match self.transition(e) {
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

    pub(crate) fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let block_inner = self.block.inner(area);
        let lines_count = block_inner.height as usize;

        self.chars_per_line = block_inner.width as usize;
        self.area = block_inner;

        let mut lines = Vec::with_capacity(lines_count);
        let start_line = self.scroll_offset;
        let end_line = (self.scroll_offset + lines_count).min(self.lines.len());

        let cursor_style = self.mode.cursor_style();

        // render only visible lines
        for i in start_line..end_line {
            let line = &self.lines[i];
            let spans = if i == self.cursor.0 {
                let line_str = line.as_str();
                let cursor_pos = self.cursor.1;

                match line_str.char_indices().nth(cursor_pos) {
                    // make the char at cursor_pos have a background of
                    // cursor_style
                    Some((byte_idx, ch)) => {
                        let before = &line_str[..byte_idx];
                        let after = &line_str[byte_idx + ch.len_utf8()..];
                        let mut spans = Vec::with_capacity(3);
                        if !before.is_empty() {
                            spans.push(before.into());
                        }
                        spans.push(Span::styled(ch.to_string(), cursor_style));
                        if !after.is_empty() {
                            spans.push(after.into());
                        }
                        spans
                    }
                    None => {
                        let mut spans = Vec::with_capacity(2);
                        if !line_str.is_empty() {
                            spans.push(line_str.into());
                        }
                        spans.push(Span::styled(" ", cursor_style));
                        spans
                    }
                }
            } else {
                vec![Span::raw(line)]
            };

            let line = if i == start_line && self.scroll_offset > 0 {
                let mut indicator = vec![Span::styled(" ↑ ", PALETTE.purple)];
                indicator.extend(spans);
                Line::from(indicator)
            } else if i == end_line - 1 && end_line < self.lines.len() {
                let mut indicator = vec![Span::styled(" ↓ ", PALETTE.purple)];
                indicator.extend(spans);
                Line::from(indicator)
            } else {
                Line::from(spans)
            };
            lines.push(line);
        }

        while lines.len() < lines_count {
            lines.push("".into());
        }

        let list = List::new(lines).block(self.block.clone());
        frame.render_widget(list, area);
    }

    fn adjust_scroll_offset(&mut self) {
        // if cursor is above visible area, scroll up
        if self.cursor.0 < self.scroll_offset {
            self.scroll_offset = self.cursor.0;
        }

        let visible_height = self.area.height as usize;

        // if cursor is below visible area, scroll down
        let bottom_line = self.scroll_offset + visible_height - 1;
        if self.cursor.0 > bottom_line {
            self.scroll_offset = self.cursor.0 - visible_height + 1;
        }

        let max_scroll = self.lines.len().saturating_sub(visible_height);
        self.scroll_offset = self.scroll_offset.min(max_scroll).max(0);
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

        // only check for line breaking if the line might be too long
        if line.len() > self.chars_per_line {
            self.break_lines_from(row);
        }

        self.adjust_scroll_offset();
    }

    #[inline]
    fn break_lines_from(&mut self, start_row: usize) {
        let mut current_row = start_row;
        while current_row < self.lines.len() {
            let mut line = std::mem::take(&mut self.lines[current_row]);
            let line_len_chars = line.chars().count();

            if line_len_chars <= self.chars_per_line {
                // line fits; put it back and move on
                self.lines[current_row] = line;
                current_row += 1;
                continue;
            }

            let mut split_byte = 0;
            for (chars_so_far, (byte_idx, _)) in line.char_indices().enumerate()
            {
                if chars_so_far == self.chars_per_line {
                    split_byte = byte_idx;
                    break;
                }
            }

            if split_byte == 0 {
                split_byte = line.len();
            }

            // split the line: `line` now holds the prefix (first
            // `chars_per_line` chars)
            let mut remainder = line.split_off(split_byte);
            self.lines[current_row] = line;

            // insert the remainder into the next line
            if current_row + 1 < self.lines.len() {
                let next_line =
                    std::mem::take(&mut self.lines[current_row + 1]);
                remainder.push_str(&next_line);
                self.lines[current_row + 1] = remainder;
            } else {
                self.lines.insert(current_row + 1, remainder);
            }

            // adjust cursor if it was on the split line and after the split
            // point
            if self.cursor.0 == current_row
                && self.cursor.1 >= self.chars_per_line
            {
                self.cursor.0 += 1;
                self.cursor.1 -= self.chars_per_line;
            }

            current_row += 1;
        }
    }

    fn insert_newline(&mut self) {
        let (row, col) = self.cursor;
        let line = &self.lines[row];

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
        self.adjust_scroll_offset();
    }

    fn move_cursor_right(&mut self) {
        if self.cursor.1 < self.lines[self.cursor.0].len() {
            self.cursor.1 += 1;
        } else if self.cursor.0 < self.lines.len() - 1 {
            self.cursor.0 += 1;
            self.cursor.1 = 0;
        }
        self.adjust_scroll_offset();
    }

    fn move_cursor_up(&mut self) {
        if self.cursor.0 > 0 {
            self.cursor.0 -= 1;
            self.cursor.1 = self.cursor.1.min(self.lines[self.cursor.0].len());
        }
        self.adjust_scroll_offset();
    }

    fn move_cursor_down(&mut self) {
        if self.cursor.0 < self.lines.len() - 1 {
            self.cursor.0 += 1;
            self.cursor.1 = self.cursor.1.min(self.lines[self.cursor.0].len());
        }
        self.adjust_scroll_offset();
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
    }

    fn handle_normal_mode(&mut self, input: &Input) -> Transition {
        match input {
            Input { key: Key::Char('i'), .. } => Transition::Mode(Mode::Insert),
            Input { key: Key::Char('h'), .. } => {
                self.move_cursor_left();
                Transition::Mode(Mode::Normal)
            }
            Input { key: Key::Char('j'), .. } => {
                self.move_cursor_down();
                Transition::Mode(Mode::Normal)
            }
            Input { key: Key::Char('k'), .. } => {
                self.move_cursor_up();
                Transition::Mode(Mode::Normal)
            }
            Input { key: Key::Char('l'), .. } => {
                self.move_cursor_right();
                Transition::Mode(Mode::Normal)
            }
            Input { key: Key::Char('e'), ctrl: true, .. } => {
                self.scroll_down(1);
                Transition::Nop
            }
            Input { key: Key::Char('y'), ctrl: true, .. } => {
                self.scroll_up(1);
                Transition::Nop
            }
            Input { key: Key::Char('d'), ctrl: true, .. } => {
                self.scroll_down(self.area.height.div_ceil(2) as usize);
                Transition::Nop
            }
            Input { key: Key::Char('u'), ctrl: true, .. } => {
                self.scroll_up(self.area.height.div_ceil(2) as usize);
                Transition::Nop
            }
            Input { key: Key::Char('f'), ctrl: true, .. } => {
                self.scroll_down(self.area.height as usize);
                Transition::Nop
            }
            Input { key: Key::Char('b'), ctrl: true, .. } => {
                self.scroll_up(self.area.height as usize);
                Transition::Nop
            }
            Input { key: Key::Char('G'), shift: true, .. } => {
                let last_line = self.lines.len().saturating_sub(1);
                let last_col = self.lines[last_line].len();
                self.cursor = (last_line, last_col);
                self.adjust_scroll_offset();
                Transition::Mode(Mode::Normal)
            }
            Input { key: Key::Char('g'), .. }
                if self.pending.key == Key::Char('g') =>
            {
                self.cursor = (0, 0);
                self.scroll_offset = 0;
                self.pending = Input::default();
                Transition::Mode(Mode::Normal)
            }
            i @ Input { key: Key::Char('g'), .. } => {
                Transition::Pending(i.clone())
            }
            _ => Transition::Nop,
        }
    }

    fn handle_insert_mode(&mut self, input: &Input) -> Transition {
        match input {
            Input { key: Key::Esc, .. } => Transition::Mode(Mode::Normal),
            Input { key: Key::Enter, shift: true, .. } => {
                self.insert_newline();
                Transition::Nop
            }
            Input { key: Key::Backspace, .. } => {
                self.backspace();
                Transition::Nop
            }
            Input { key: Key::Char(c), .. } => {
                self.insert_char(*c);
                Transition::Mode(Mode::Insert)
            }
            _ => Transition::Nop,
        }
    }

    pub(crate) fn transition(&mut self, input: &Input) -> Transition {
        if input.key == Key::Null {
            return Transition::Nop;
        }

        if self.mode == Mode::Normal && (input.key == Key::Esc) {
            return Transition::Quit;
        }

        match self.mode {
            Mode::Normal => self.handle_normal_mode(input),
            Mode::Insert => self.handle_insert_mode(input),
            Mode::Operator(_) => Transition::Nop,
        }
    }

    pub(crate) fn set_block(&mut self, block: Block<'a>) {
        self.block = block;
    }

    pub(crate) fn set_style(&mut self, style: Style) {
        self.style = style;
    }

    pub(crate) fn set_cursor_style(&mut self, style: Style) {
        self.cursor_style = style;
    }

    pub(crate) fn set_placeholder_text(&mut self, _text: &'a str) {}
}

pub(crate) fn validate_magnet<'a>(
    textarea: &mut VimInput<'a>,
) -> Option<Magnet> {
    let magnet_str = textarea.lines.join("");
    let magnet_str = magnet_str.trim();
    let magnet = magnet_url::Magnet::new(magnet_str);

    match magnet {
        Ok(magnet) => {
            textarea.set_style(Style::default().fg(PALETTE.success));
            textarea.set_block(
                Block::default()
                    .border_style(PALETTE.success)
                    .borders(Borders::ALL)
                    .title(" Ok (Press Enter) "),
            );
            Some(Magnet(magnet))
        }
        Err(err) => {
            textarea.set_style(PALETTE.error.into());
            textarea.set_block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(PALETTE.error)
                    .title(format!(" Err: {err} ")),
            );
            None
        }
    }
}
