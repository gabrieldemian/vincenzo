use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List},
};
use std::fmt;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
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
        let end_line = (start_line + lines_count).min(self.lines.len());

        let cursor_style = self.mode.cursor_style();
        let purple = PALETTE.purple;

        for i in start_line..end_line {
            let line = &self.lines[i];
            let spans = if i == self.cursor.0 {
                let s = line.as_str();
                let cursor_col = self.cursor.1;
                let mut col = 0;
                let mut target_byte = None;
                let mut target_char = None;
                let mut after_byte = None;

                for (byte_idx, ch) in s.char_indices() {
                    let w = Self::char_width(ch);
                    if col + w > cursor_col {
                        target_byte = Some(byte_idx);
                        target_char = Some(ch);
                        after_byte = Some(byte_idx + ch.len_utf8());
                        break;
                    }
                    col += w;
                }

                if let (Some(byte_idx), Some(ch)) = (target_byte, target_char) {
                    let before = &s[..byte_idx];
                    let after = &s[after_byte.unwrap()..];
                    let mut spans = Vec::with_capacity(3);
                    if !before.is_empty() {
                        spans.push(Span::raw(before.to_string()));
                    }
                    spans.push(Span::styled(ch.to_string(), cursor_style));
                    if !after.is_empty() {
                        spans.push(Span::raw(after.to_string()));
                    }
                    spans
                } else {
                    let mut spans = Vec::with_capacity(2);
                    if !s.is_empty() {
                        spans.push(Span::raw(s.to_string()));
                    }
                    spans.push(Span::styled(" ", cursor_style));
                    spans
                }
            } else {
                vec![Span::raw(line.clone())]
            };

            let line = if i == start_line && self.scroll_offset > 0 {
                let mut prefix = vec![Span::styled(" ↑ ", purple)];
                prefix.extend(spans);
                Line::from(prefix)
            } else if i == end_line - 1 && end_line < self.lines.len() {
                let mut prefix = vec![Span::styled(" ↓ ", purple)];
                prefix.extend(spans);
                Line::from(prefix)
            } else {
                Line::from(spans)
            };
            lines.push(line);
        }

        lines.resize_with(lines_count, || "".into());
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

    #[inline]
    fn insert_char(&mut self, c: char) {
        let (row, col) = self.cursor;
        while row >= self.lines.len() {
            self.lines.push(String::new());
        }

        let line = &mut self.lines[row];
        let (byte_idx, _) = Self::byte_idx_and_width_at_column(line, col);
        line.insert(byte_idx, c);
        self.cursor.1 += Self::char_width(c);

        if Self::column_width(line) > self.chars_per_line {
            self.break_lines_from(row);
        }

        self.adjust_scroll_offset();
    }

    fn break_lines_from(&mut self, start_row: usize) {
        let mut current_row = start_row;
        while current_row < self.lines.len() {
            let line = &self.lines[current_row];
            let total_width = Self::column_width(line);
            if total_width <= self.chars_per_line {
                current_row += 1;
                continue;
            }

            // find split column: we want to keep as many characters as possible
            // without exceeding max width.
            let mut col = 0;
            let mut split_byte = 0;
            for (byte_idx, ch) in line.char_indices() {
                let w = Self::char_width(ch);
                if col + w > self.chars_per_line {
                    split_byte = byte_idx;
                    break;
                }
                col += w;
            }

            // split the line
            let mut line_owned = std::mem::take(&mut self.lines[current_row]);
            let mut remainder = line_owned.split_off(split_byte);
            self.lines[current_row] = line_owned;

            // insert remainder
            if current_row + 1 < self.lines.len() {
                let next_line =
                    std::mem::take(&mut self.lines[current_row + 1]);
                remainder.push_str(&next_line);
                self.lines[current_row + 1] = remainder;
            } else {
                self.lines.insert(current_row + 1, remainder);
            }

            // adjust cursor if it was on the split line
            if self.cursor.0 == current_row && self.cursor.1 > col {
                self.cursor.0 += 1;
                self.cursor.1 -= col;
            }

            current_row += 1;
        }
    }

    #[inline]
    fn insert_newline(&mut self) {
        let (row, col) = self.cursor;
        let line = &mut self.lines[row];
        let remainder = Self::split_line_at_column(line, col);
        self.lines.insert(row + 1, remainder);
        self.cursor.0 += 1;
        self.cursor.1 = 0;
        self.adjust_scroll_offset();
    }

    // this monstruosity of a function handles the edge case where you delete a
    // char at the middle of a line, which is not the last line, and moves
    // the chars of the lines above to fill the gap.
    #[inline]
    fn backspace(&mut self) {
        let (row, col) = self.cursor;

        if col > 0 {
            let line = &mut self.lines[row];
            let mut current_col = 0;
            let mut last_byte = 0;
            let mut last_width = 0;
            for (byte_idx, ch) in line.char_indices() {
                let w = Self::char_width(ch);
                if current_col + w > col {
                    break;
                }
                last_byte = byte_idx;
                last_width = w;
                current_col += w;
            }
            let ch_len = line[last_byte..].chars().next().unwrap().len_utf8();
            line.drain(last_byte..last_byte + ch_len);
            self.cursor.1 -= last_width;

            let mut r = row;
            while r + 1 < self.lines.len() {
                let current_width = Self::column_width(&self.lines[r]);
                if current_width >= self.chars_per_line {
                    break;
                }

                let next_line_str = &self.lines[r + 1];
                if let Some((byte_idx, first_char)) =
                    next_line_str.char_indices().next()
                {
                    let char_width = Self::char_width(first_char);
                    if current_width + char_width > self.chars_per_line {
                        break;
                    }

                    let next_line_mut = &mut self.lines[r + 1];
                    let is_empty = next_line_mut.is_empty();
                    next_line_mut.drain(0..byte_idx + first_char.len_utf8());
                    self.lines[r].push(first_char);

                    if is_empty {
                        self.lines.remove(r + 1);
                    } else {
                        r += 1;
                    }
                } else {
                    self.lines.remove(r + 1);
                }
            }
        } else if row > 0 {
            let current_line = self.lines.remove(row);
            let prev_line = &mut self.lines[row - 1];
            let prev_width = Self::column_width(prev_line);
            prev_line.push_str(&current_line);
            self.cursor.0 -= 1;
            self.cursor.1 = prev_width;
            self.break_lines_from(self.cursor.0);
        }

        self.adjust_scroll_offset();
    }

    #[inline]
    fn move_cursor_left(&mut self) {
        let (row, col) = self.cursor;
        if col > 0 {
            let line = &self.lines[row];
            let mut start = 0;
            let mut prev_start = 0;
            for ch in line.chars() {
                let w = ch.width().unwrap_or(0);
                if col < start + w {
                    // cursor is inside or at the start of this character.
                    self.cursor.1 =
                        if col == start { prev_start } else { start };
                    return;
                }
                prev_start = start;
                start += w;
            }
            // if we get here, col must be exactly at the end of the line.
            self.cursor.1 = prev_start;
        } else if row > 0 {
            self.cursor.0 -= 1;
            self.cursor.1 = self.lines[self.cursor.0].width();
        }
        self.adjust_scroll_offset();
    }

    #[inline]
    fn move_cursor_right(&mut self) {
        let (row, col) = self.cursor;
        let line = &self.lines[row];
        let line_width = line.width();

        if col < line_width {
            let mut start = 0;
            for ch in line.chars() {
                let w = ch.width().unwrap_or(0);
                if col < start + w {
                    self.cursor.1 = start + w;
                    break;
                }
                start += w;
            }
        } else if row < self.lines.len() - 1 {
            self.cursor.0 += 1;
            self.cursor.1 = 0;
        }
        self.adjust_scroll_offset();
    }

    #[inline]
    fn move_cursor_up(&mut self) {
        if self.cursor.0 > 0 {
            self.cursor.0 -= 1;
            let line_width = self.lines[self.cursor.0].width();
            if self.cursor.1 > line_width {
                self.cursor.1 = line_width;
            }
        }
        self.adjust_scroll_offset();
    }

    #[inline]
    fn move_cursor_down(&mut self) {
        if self.cursor.0 < self.lines.len() - 1 {
            self.cursor.0 += 1;
            let line_width = self.lines[self.cursor.0].width();
            if self.cursor.1 > line_width {
                self.cursor.1 = line_width;
            }
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

    #[inline]
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

    #[inline]
    fn char_width(c: char) -> usize {
        UnicodeWidthChar::width(c).unwrap_or(0)
    }

    #[inline]
    fn column_width(s: &str) -> usize {
        s.chars().map(Self::char_width).sum()
    }

    // Find byte index and width of the character at column `col`.
    // Returns (byte_index, char_width) if column is within the line,
    // or (line.len(), 0) if col is beyond the total width.
    #[inline]
    fn byte_idx_and_width_at_column(s: &str, col: usize) -> (usize, usize) {
        let mut current_col = 0;
        for (byte_idx, ch) in s.char_indices() {
            let w = Self::char_width(ch);
            if current_col + w > col {
                return (byte_idx, w);
            }
            current_col += w;
        }
        (s.len(), 0)
    }

    // Split the line at the given column, returning the remainder (right part).
    // The original line is truncated to contain characters up to column `col`.
    #[inline]
    fn split_line_at_column(line: &mut String, col: usize) -> String {
        let (byte_idx, _) = Self::byte_idx_and_width_at_column(line, col);
        line.split_off(byte_idx)
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
