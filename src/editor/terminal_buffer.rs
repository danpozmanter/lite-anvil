


use std::collections::VecDeque;

use crate::editor::terminal::pack_color;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: u32,
    pub fg: u32,
    pub bg: u32,
}

impl Cell {
    fn blank(default_fg: [u8; 4]) -> Self {
        Self {
            ch: ' ' as u32,
            fg: pack_color(default_fg),
            bg: 0,
        }
    }
}

/// Default scrollback line limit.
pub const DEFAULT_SCROLLBACK: usize = 2000;

pub struct TerminalBufferInner {
    cols: usize,
    rows: usize,
    scrollback_cap: usize,
    screen: Vec<Vec<Cell>>,
    history: VecDeque<Vec<Cell>>,
    alt_screen: Vec<Vec<Cell>>,
    in_alt_screen: bool,
    cursor_row: usize,
    cursor_col: usize,
    saved_cursor_row: usize,
    saved_cursor_col: usize,
    cursor_visible: bool,
    scroll_top: usize,
    scroll_bottom: usize,
    default_fg: [u8; 4],
    current_fg: Option<[u8; 4]>,
    current_bg: Option<[u8; 4]>,
    palette: [[u8; 4]; 16],
    escape_state: EscapeState,
    escape_buffer: String,
    osc_buffer: String,
    osc_esc: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EscapeState {
    None,
    Esc,
    EscCharset,
    Csi,
    Osc,
}

impl TerminalBufferInner {
    fn normalize_char(ch: char) -> char {
        match ch {
            '❯' | '➜' | '▶' | '›' | '»' => '>',
            '❮' | '◀' | '‹' | '«' => '<',
            '│' | '┃' | '┆' | '┇' | '┊' | '┋' => '|',
            '─' | '━' | '┄' | '┅' | '┈' | '┉' => '-',
            '╭' | '╮' | '╰' | '╯' | '┌' | '┐' | '└' | '┘' | '┼' | '┬' | '┴' | '├' | '┤' | '╞'
            | '╡' | '╪' | '╤' | '╧' | '╟' | '╢' | '╔' | '╗' | '╚' | '╝' | '╠' | '╣' | '╦' | '╩'
            | '╬' => '+',
            _ => ch,
        }
    }

    pub fn new(
        cols: usize,
        rows: usize,
        scrollback_cap: usize,
        palette: [[u8; 4]; 16],
        default_fg: [u8; 4],
    ) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let mut inner = Self {
            cols,
            rows,
            scrollback_cap: scrollback_cap.max(1),
            screen: Vec::new(),
            history: VecDeque::new(),
            alt_screen: Vec::new(),
            in_alt_screen: false,
            cursor_row: 1,
            cursor_col: 1,
            saved_cursor_row: 1,
            saved_cursor_col: 1,
            cursor_visible: true,
            scroll_top: 1,
            scroll_bottom: rows,
            default_fg,
            current_fg: Some(default_fg),
            current_bg: None,
            palette,
            escape_state: EscapeState::None,
            escape_buffer: String::new(),
            osc_buffer: String::new(),
            osc_esc: false,
        };
        inner.reset_screen();
        inner
    }

    fn blank_row(&self) -> Vec<Cell> {
        vec![Cell::blank(self.default_fg); self.cols]
    }

    fn reset_screen(&mut self) {
        self.screen = (0..self.rows).map(|_| self.blank_row()).collect();
        self.scroll_top = 1;
        self.scroll_bottom = self.rows;
    }

    fn sync_saved_screens(&mut self) {
        if self.in_alt_screen {
            self.alt_screen = self.screen.clone();
        }
    }

    /// Read-only access to the screen cell grid.
    pub fn screen(&self) -> &Vec<Vec<Cell>> {
        &self.screen
    }

    /// Current cursor row (1-based).
    pub fn cursor_row(&self) -> usize {
        self.cursor_row
    }

    /// Current cursor column (1-based).
    pub fn cursor_col(&self) -> usize {
        self.cursor_col
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let old_screen = std::mem::take(&mut self.screen);
        let old_rows = self.rows;
        let old_cols = self.cols;
        self.cols = cols;
        self.rows = rows;
        self.reset_screen();

        let copy_rows = old_rows.min(rows);
        for i in 0..copy_rows {
            let src_idx = old_rows - 1 - i;
            let dst_idx = rows - 1 - i;
            if let Some(src) = old_screen.get(src_idx) {
                let copy_len = old_cols.min(cols);
                self.screen[dst_idx][..copy_len].clone_from_slice(&src[..copy_len]);
            }
        }

        self.cursor_row = self.cursor_row.clamp(1, self.rows);
        self.cursor_col = self.cursor_col.clamp(1, self.cols);
        self.saved_cursor_row = self.saved_cursor_row.clamp(1, self.rows);
        self.saved_cursor_col = self.saved_cursor_col.clamp(1, self.cols);
        self.scroll_top = self.scroll_top.clamp(1, self.rows);
        self.scroll_bottom = self.scroll_bottom.clamp(self.scroll_top, self.rows);
        self.sync_saved_screens();
    }

    fn clear(&mut self) {
        self.history.clear();
        self.history.shrink_to_fit();
        self.current_fg = Some(self.default_fg);
        self.current_bg = None;
        self.cursor_row = 1;
        self.cursor_col = 1;
        self.saved_cursor_row = 1;
        self.saved_cursor_col = 1;
        self.cursor_visible = true;
        self.escape_state = EscapeState::None;
        self.escape_buffer.clear();
        self.osc_buffer.clear();
        self.osc_esc = false;
        self.in_alt_screen = false;
        self.alt_screen.clear();
        self.alt_screen.shrink_to_fit();
        self.reset_screen();
    }

    fn push_history(&mut self, row: Vec<Cell>) {
        self.history.push_back(row);
        while self.history.len() > self.scrollback_cap {
            self.history.pop_front();
        }
    }

    fn scroll_screen(&mut self) {
        self.scroll_up_in_region(1);
    }

    fn put_char(&mut self, ch: char) {
        let ch = Self::normalize_char(ch);
        if self.cursor_col > self.cols {
            self.cursor_col = 1;
            self.cursor_row += 1;
        }
        if self.cursor_row > self.rows {
            self.scroll_screen();
            self.cursor_row = self.rows;
        }
        let row = &mut self.screen[self.cursor_row - 1];
        row[self.cursor_col - 1] = Cell {
            ch: ch as u32,
            fg: self.current_fg.map(pack_color).unwrap_or(0),
            bg: self.current_bg.map(pack_color).unwrap_or(0),
        };
        self.cursor_col += 1;
    }

    fn newline(&mut self) {
        self.cursor_col = 1;
        if self.cursor_row == self.scroll_bottom {
            self.scroll_up_in_region(1);
        } else {
            self.cursor_row += 1;
            if self.cursor_row > self.rows {
                self.scroll_screen();
                self.cursor_row = self.rows;
            }
        }
    }

    fn save_cursor(&mut self) {
        self.saved_cursor_row = self.cursor_row;
        self.saved_cursor_col = self.cursor_col;
    }

    fn restore_cursor(&mut self) {
        self.cursor_row = self.saved_cursor_row.clamp(1, self.rows);
        self.cursor_col = self.saved_cursor_col.clamp(1, self.cols);
    }

    fn set_scroll_region(&mut self, top: usize, bottom: usize) {
        self.scroll_top = top.clamp(1, self.rows);
        self.scroll_bottom = bottom.clamp(self.scroll_top, self.rows);
        self.cursor_row = 1;
        self.cursor_col = 1;
    }

    fn scroll_up_in_region(&mut self, count: usize) {
        if self.screen.is_empty() || self.scroll_top > self.scroll_bottom {
            return;
        }
        for _ in 0..count.max(1) {
            if self.scroll_top == 1 && self.scroll_bottom == self.rows && !self.in_alt_screen {
                if !self.screen.is_empty() {
                    let row = self.screen.remove(0);
                    self.push_history(row);
                    self.screen.push(self.blank_row());
                }
                continue;
            }
            let top = self.scroll_top - 1;
            let bottom = self.scroll_bottom - 1;
            if top >= self.screen.len() || bottom >= self.screen.len() || top >= bottom {
                break;
            }
            for row in top..bottom {
                self.screen[row] = self.screen[row + 1].clone();
            }
            self.screen[bottom] = self.blank_row();
        }
    }

    fn scroll_down_in_region(&mut self, count: usize) {
        if self.screen.is_empty() || self.scroll_top > self.scroll_bottom {
            return;
        }
        for _ in 0..count.max(1) {
            let top = self.scroll_top - 1;
            let bottom = self.scroll_bottom - 1;
            if top >= self.screen.len() || bottom >= self.screen.len() || top >= bottom {
                break;
            }
            for row in (top + 1..=bottom).rev() {
                self.screen[row] = self.screen[row - 1].clone();
            }
            self.screen[top] = self.blank_row();
        }
    }

    fn insert_lines(&mut self, count: usize) {
        if self.cursor_row < self.scroll_top || self.cursor_row > self.scroll_bottom {
            return;
        }
        let count = count.max(1).min(self.scroll_bottom - self.cursor_row + 1);
        let start = self.cursor_row - 1;
        let bottom = self.scroll_bottom - 1;
        for _ in 0..count {
            for row in (start + 1..=bottom).rev() {
                self.screen[row] = self.screen[row - 1].clone();
            }
            self.screen[start] = self.blank_row();
        }
    }

    fn delete_lines(&mut self, count: usize) {
        if self.cursor_row < self.scroll_top || self.cursor_row > self.scroll_bottom {
            return;
        }
        let count = count.max(1).min(self.scroll_bottom - self.cursor_row + 1);
        let start = self.cursor_row - 1;
        let bottom = self.scroll_bottom - 1;
        for _ in 0..count {
            for row in start..bottom {
                self.screen[row] = self.screen[row + 1].clone();
            }
            self.screen[bottom] = self.blank_row();
        }
    }

    fn insert_chars(&mut self, count: usize) {
        let row = &mut self.screen[self.cursor_row - 1];
        let start = self
            .cursor_col
            .saturating_sub(1)
            .min(self.cols.saturating_sub(1));
        let count = count.max(1).min(self.cols.saturating_sub(start));
        for idx in (start..self.cols - count).rev() {
            row[idx + count] = row[idx];
        }
        let blank = Cell::blank(self.default_fg);
        for cell in &mut row[start..(start + count).min(self.cols)] {
            *cell = blank;
        }
    }

    fn delete_chars(&mut self, count: usize) {
        let row = &mut self.screen[self.cursor_row - 1];
        let start = self
            .cursor_col
            .saturating_sub(1)
            .min(self.cols.saturating_sub(1));
        let count = count.max(1).min(self.cols.saturating_sub(start));
        for idx in start..self.cols - count {
            row[idx] = row[idx + count];
        }
        let blank = Cell::blank(self.default_fg);
        for cell in &mut row[self.cols.saturating_sub(count)..self.cols] {
            *cell = blank;
        }
    }

    fn erase_chars(&mut self, count: usize) {
        let row = &mut self.screen[self.cursor_row - 1];
        let start = self
            .cursor_col
            .saturating_sub(1)
            .min(self.cols.saturating_sub(1));
        let end = (start + count.max(1)).min(self.cols);
        let blank = Cell::blank(self.default_fg);
        for cell in &mut row[start..end] {
            *cell = blank;
        }
    }

    fn switch_alt_screen(&mut self, enabled: bool, clear: bool) {
        if enabled == self.in_alt_screen {
            if enabled && clear {
                self.screen = (0..self.rows).map(|_| self.blank_row()).collect();
                self.cursor_row = 1;
                self.cursor_col = 1;
            }
            return;
        }

        if enabled {
            // Save main screen into alt_screen, enter alt mode.
            self.alt_screen = std::mem::take(&mut self.screen);
            self.screen = if clear {
                (0..self.rows).map(|_| self.blank_row()).collect()
            } else {
                (0..self.rows).map(|_| self.blank_row()).collect()
            };
            self.in_alt_screen = true;
        } else {
            // Restore main screen from alt_screen.
            self.screen = if self.alt_screen.is_empty() {
                (0..self.rows).map(|_| self.blank_row()).collect()
            } else {
                std::mem::take(&mut self.alt_screen)
            };
            self.in_alt_screen = false;
        }
        self.cursor_row = 1;
        self.cursor_col = 1;
        self.scroll_top = 1;
        self.scroll_bottom = self.rows;
    }

    fn clear_line(&mut self, mode: i64) {
        let (mut start_col, mut end_col) = (1usize, self.cols);
        if mode == 0 {
            start_col = self.cursor_col;
        } else if mode == 1 {
            end_col = self.cursor_col;
        }
        let blank = Cell::blank(self.default_fg);
        let row = &mut self.screen[self.cursor_row - 1];
        for cell in &mut row[(start_col - 1)..end_col.min(self.cols)] {
            *cell = blank;
        }
    }

    fn clear_screen(&mut self, mode: i64) {
        if mode == 2 {
            self.reset_screen();
            self.cursor_row = 1;
            self.cursor_col = 1;
            return;
        }
        if mode == 0 {
            self.clear_line(0);
            let blank = self.blank_row();
            for row in self.cursor_row..self.rows {
                self.screen[row] = blank.clone();
            }
        } else if mode == 1 {
            self.clear_line(1);
            let blank = self.blank_row();
            for row in 0..self.cursor_row.saturating_sub(1) {
                self.screen[row] = blank.clone();
            }
        }
    }

    fn ansi_color_256(&self, idx: i64) -> [u8; 4] {
        if (0..16).contains(&idx) {
            return self.palette[idx as usize];
        }
        if idx < 232 {
            let idx = idx - 16;
            let levels = [0u8, 95, 135, 175, 215, 255];
            let r = levels[((idx / 36) % 6) as usize];
            let g = levels[((idx / 6) % 6) as usize];
            let b = levels[(idx % 6) as usize];
            return [r, g, b, 0xff];
        }
        let c = (8 + (idx - 232) * 10).clamp(0, 255) as u8;
        [c, c, c, 0xff]
    }

    fn apply_sgr(&mut self, params: &[i64]) {
        let params = if params.is_empty() {
            vec![0]
        } else {
            params.to_vec()
        };
        let mut i = 0usize;
        while i < params.len() {
            let code = params[i];
            match code {
                0 => {
                    self.current_fg = Some(self.default_fg);
                    self.current_bg = None;
                }
                39 => self.current_fg = Some(self.default_fg),
                49 => self.current_bg = None,
                30..=37 => self.current_fg = Some(self.palette[(code - 30) as usize]),
                40..=47 => self.current_bg = Some(self.palette[(code - 40) as usize]),
                90..=97 => self.current_fg = Some(self.palette[(8 + code - 90) as usize]),
                100..=107 => self.current_bg = Some(self.palette[(8 + code - 100) as usize]),
                38 | 48 if i + 1 < params.len() => {
                    let is_fg = code == 38;
                    let mode = params[i + 1];
                    if mode == 5 && i + 2 < params.len() {
                        let color = self.ansi_color_256(params[i + 2]);
                        if is_fg {
                            self.current_fg = Some(color);
                        } else {
                            self.current_bg = Some(color);
                        }
                        i += 2;
                    } else if mode == 2 && i + 4 < params.len() {
                        let color = [
                            params[i + 2].clamp(0, 255) as u8,
                            params[i + 3].clamp(0, 255) as u8,
                            params[i + 4].clamp(0, 255) as u8,
                            0xff,
                        ];
                        if is_fg {
                            self.current_fg = Some(color);
                        } else {
                            self.current_bg = Some(color);
                        }
                        i += 4;
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn execute_csi(&mut self, sequence: &str) {
        let final_char = sequence.chars().last().unwrap_or('m');
        let body = &sequence[..sequence.len().saturating_sub(final_char.len_utf8())];
        let prefix = match body.as_bytes().first().copied() {
            Some(b'?') => '?',
            Some(b'>') => '>',
            Some(b'!') => '!',
            _ => '\0',
        };
        let param_body = if prefix == '\0' { body } else { &body[1..] };
        let params = param_body
            .split(';')
            .map(|item| item.parse::<i64>().unwrap_or(0))
            .collect::<Vec<_>>();
        let p1 = *params.first().unwrap_or(&0);
        let p2 = *params.get(1).unwrap_or(&0);

        match final_char {
            'A' => {
                self.cursor_row = self
                    .cursor_row
                    .saturating_sub(p1.max(1) as usize)
                    .clamp(1, self.rows)
            }
            'B' => self.cursor_row = (self.cursor_row + p1.max(1) as usize).clamp(1, self.rows),
            'C' => self.cursor_col = (self.cursor_col + p1.max(1) as usize).clamp(1, self.cols),
            'D' => {
                self.cursor_col = self
                    .cursor_col
                    .saturating_sub(p1.max(1) as usize)
                    .clamp(1, self.cols)
            }
            'H' | 'f' => {
                self.cursor_row = (if p1 <= 0 { 1 } else { p1 as usize }).clamp(1, self.rows);
                self.cursor_col = (if p2 <= 0 { 1 } else { p2 as usize }).clamp(1, self.cols);
            }
            'd' => {
                self.cursor_row = (if p1 <= 0 { 1 } else { p1 as usize }).clamp(1, self.rows);
            }
            'G' => {
                self.cursor_col = (if p1 <= 0 { 1 } else { p1 as usize }).clamp(1, self.cols);
            }
            'E' => {
                self.cursor_row = (self.cursor_row + p1.max(1) as usize).clamp(1, self.rows);
                self.cursor_col = 1;
            }
            'F' => {
                self.cursor_row = self
                    .cursor_row
                    .saturating_sub(p1.max(1) as usize)
                    .clamp(1, self.rows);
                self.cursor_col = 1;
            }
            'J' => self.clear_screen(p1),
            'K' => self.clear_line(p1),
            'L' => self.insert_lines(p1.max(1) as usize),
            'M' => self.delete_lines(p1.max(1) as usize),
            '@' => self.insert_chars(p1.max(1) as usize),
            'P' => self.delete_chars(p1.max(1) as usize),
            'X' => self.erase_chars(p1.max(1) as usize),
            'S' => self.scroll_up_in_region(p1.max(1) as usize),
            'T' => self.scroll_down_in_region(p1.max(1) as usize),
            's' => self.save_cursor(),
            'u' => self.restore_cursor(),
            'r' => {
                let top = if p1 <= 0 { 1 } else { p1 as usize };
                let bottom = if p2 <= 0 { self.rows } else { p2 as usize };
                self.set_scroll_region(top, bottom);
            }
            'h' => {
                if prefix == '?' {
                    for param in params.iter().copied() {
                        match param {
                            25 => self.cursor_visible = true,
                            47 | 1047 | 1049 => {
                                let save_cursor = param == 1049;
                                if save_cursor {
                                    self.save_cursor();
                                }
                                self.switch_alt_screen(true, true);
                            }
                            _ => {}
                        }
                    }
                }
            }
            'l' => {
                if prefix == '?' {
                    for param in params.iter().copied() {
                        match param {
                            25 => self.cursor_visible = false,
                            47 | 1047 | 1049 => {
                                let restore_cursor = param == 1049;
                                self.switch_alt_screen(false, false);
                                if restore_cursor {
                                    self.restore_cursor();
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            'm' => self.apply_sgr(&params),
            _ => {}
        }
    }

    fn color_to_osc_rgb(color: [u8; 4]) -> String {
        format!(
            "rgb:{:02x}{:02x}/{:02x}{:02x}/{:02x}{:02x}",
            color[0], color[0], color[1], color[1], color[2], color[2]
        )
    }

    fn execute_csi_query(&self, sequence: &str) -> Option<String> {
        let final_char = sequence.chars().last().unwrap_or('m');
        let body = &sequence[..sequence.len().saturating_sub(final_char.len_utf8())];
        let prefix = match body.as_bytes().first().copied() {
            Some(b'?') => '?',
            Some(b'>') => '>',
            Some(b'!') => '!',
            _ => '\0',
        };
        let param_body = if prefix == '\0' { body } else { &body[1..] };
        let params = param_body
            .split(';')
            .filter(|item| !item.is_empty())
            .map(|item| item.parse::<i64>().unwrap_or(0))
            .collect::<Vec<_>>();
        match (prefix, final_char) {
            ('\0', 'n') if params.first().copied().unwrap_or(0) == 6 => {
                Some(format!("\x1b[{};{}R", self.cursor_row, self.cursor_col))
            }
            ('\0', 'c') => Some("\x1b[?62;c".to_string()),
            _ => None,
        }
    }

    fn execute_osc_query(&self, sequence: &str) -> Option<String> {
        let (code, value) = sequence.split_once(';')?;
        let code = code.parse::<i64>().ok()?;
        if value != "?" {
            return None;
        }
        let color = match code {
            10 => self.current_fg.unwrap_or(self.default_fg),
            11 => self.current_bg.unwrap_or([0, 0, 0, 0xff]),
            12 => self.current_fg.unwrap_or(self.default_fg),
            _ => return None,
        };
        Some(format!(
            "\x1b]{};{}\x1b\\",
            code,
            Self::color_to_osc_rgb(color)
        ))
    }

    fn decode_utf8_char(bytes: &[u8], i: usize) -> (char, usize) {
        let b = *bytes.get(i).unwrap_or(&0);
        let end = if b < 0x80 {
            i + 1
        } else if b < 0xE0 {
            (i + 2).min(bytes.len())
        } else if b < 0xF0 {
            (i + 3).min(bytes.len())
        } else {
            (i + 4).min(bytes.len())
        };
        let ch = std::str::from_utf8(&bytes[i..end])
            .ok()
            .and_then(|text| text.chars().next())
            .unwrap_or(char::REPLACEMENT_CHARACTER);
        (ch, end)
    }

    pub fn process_output_and_collect_replies(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut replies = Vec::new();
        let mut i = 0usize;
        while i < bytes.len() {
            let b = bytes[i];
            match self.escape_state {
                EscapeState::Osc => {
                    if b == 7 {
                        if let Some(reply) = self.execute_osc_query(&self.osc_buffer) {
                            replies.extend_from_slice(reply.as_bytes());
                        }
                        self.escape_state = EscapeState::None;
                        self.osc_buffer.clear();
                    } else if b == 27 {
                        self.osc_esc = true;
                    } else if self.osc_esc && b == 92 {
                        if let Some(reply) = self.execute_osc_query(&self.osc_buffer) {
                            replies.extend_from_slice(reply.as_bytes());
                        }
                        self.escape_state = EscapeState::None;
                        self.osc_esc = false;
                        self.osc_buffer.clear();
                    } else {
                        self.osc_esc = false;
                        self.osc_buffer.push(b as char);
                    }
                    i += 1;
                }
                EscapeState::Esc => {
                    match b {
                        b'[' => {
                            self.escape_state = EscapeState::Csi;
                            self.escape_buffer.clear();
                        }
                        b']' => {
                            self.escape_state = EscapeState::Osc;
                            self.osc_buffer.clear();
                            self.osc_esc = false;
                        }
                        b'c' => {
                            self.clear();
                            self.escape_state = EscapeState::None;
                        }
                        b'7' => {
                            self.save_cursor();
                            self.escape_state = EscapeState::None;
                        }
                        b'8' => {
                            self.restore_cursor();
                            self.escape_state = EscapeState::None;
                        }
                        b'D' => {
                            if self.cursor_row == self.scroll_bottom {
                                self.scroll_up_in_region(1);
                            } else {
                                self.cursor_row = (self.cursor_row + 1).clamp(1, self.rows);
                            }
                            self.escape_state = EscapeState::None;
                        }
                        b'E' => {
                            self.newline();
                            self.escape_state = EscapeState::None;
                        }
                        b'M' => {
                            if self.cursor_row == self.scroll_top {
                                self.scroll_down_in_region(1);
                            } else {
                                self.cursor_row = self.cursor_row.saturating_sub(1).max(1);
                            }
                            self.escape_state = EscapeState::None;
                        }
                        b'(' | b')' | b'*' | b'+' | b'-' | b'.' | b'/' => {
                            self.escape_state = EscapeState::EscCharset;
                        }
                        _ => self.escape_state = EscapeState::None,
                    }
                    i += 1;
                }
                EscapeState::EscCharset => {
                    self.escape_state = EscapeState::None;
                    i += 1;
                }
                EscapeState::Csi => {
                    self.escape_buffer.push(b as char);
                    if (b'@'..=b'~').contains(&b) {
                        let sequence = self.escape_buffer.clone();
                        if let Some(reply) = self.execute_csi_query(&sequence) {
                            replies.extend_from_slice(reply.as_bytes());
                        }
                        self.execute_csi(&sequence);
                        self.escape_buffer.clear();
                        self.escape_state = EscapeState::None;
                    }
                    i += 1;
                }
                EscapeState::None => match b {
                    27 => {
                        self.escape_state = EscapeState::Esc;
                        i += 1;
                    }
                    b'\r' => {
                        self.cursor_col = 1;
                        i += 1;
                    }
                    b'\n' => {
                        self.newline();
                        i += 1;
                    }
                    8 => {
                        self.cursor_col = self.cursor_col.saturating_sub(1).max(1);
                        i += 1;
                    }
                    b'\t' => {
                        let next_tab = (self.cursor_col + (8 - ((self.cursor_col - 1) % 8)))
                            .min(self.cols + 1);
                        while self.cursor_col < next_tab {
                            self.put_char(' ');
                        }
                        i += 1;
                    }
                    0..=31 => {
                        i += 1;
                    }
                    _ => {
                        let (ch, next) = Self::decode_utf8_char(bytes, i);
                        self.put_char(ch);
                        i = next;
                    }
                },
            }
        }
        replies
    }

    pub fn process_output(&mut self, bytes: &[u8]) {
        let _ = self.process_output_and_collect_replies(bytes);
    }

    #[allow(dead_code)] // Used for scrollback rendering
    fn total_rows(&self) -> usize {
        self.history.len() + self.rows
    }

    #[allow(dead_code)] // Used for scrollback rendering
    fn row_at(&self, index: usize) -> Option<&[Cell]> {
        if index == 0 {
            return None;
        }
        if index <= self.history.len() {
            return self.history.get(index - 1).map(Vec::as_slice);
        }
        self.screen
            .get(index - self.history.len() - 1)
            .map(Vec::as_slice)
    }
}

