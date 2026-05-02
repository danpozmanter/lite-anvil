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

    /// Number of rows currently held in scrollback history.
    pub fn history_len(&self) -> usize {
        self.history.len()
    }

    /// Return `rows` rows of cell-grid for rendering, with the bottom of
    /// the view `scrollback` lines above the current bottom (i.e.
    /// `scrollback == 0` shows the live screen, larger values reveal
    /// history). `scrollback` is clamped to `history_len()`. The
    /// returned vector always has exactly `rows` entries; positions
    /// before history begins are padded with blank rows, matching how
    /// most terminals render when you scroll past the top.
    pub fn visible_rows(
        &self,
        rows: usize,
        scrollback: usize,
    ) -> Vec<std::borrow::Cow<'_, [Cell]>> {
        use std::borrow::Cow;
        let scrollback = scrollback.min(self.history.len());
        let total_hist = self.history.len();
        // Top of the visible window within the concatenated (history ++
        // screen) sequence. `first_idx == total_hist` is the live view.
        let first_idx = total_hist.saturating_sub(scrollback);
        let mut out: Vec<Cow<'_, [Cell]>> = Vec::with_capacity(rows);
        for offset in 0..rows {
            let i = first_idx + offset;
            if i < total_hist {
                out.push(Cow::Borrowed(self.history[i].as_slice()));
            } else {
                let scr_idx = i - total_hist;
                if scr_idx < self.screen.len() {
                    out.push(Cow::Borrowed(self.screen[scr_idx].as_slice()));
                } else {
                    out.push(Cow::Owned(vec![Cell::blank(self.default_fg); self.cols]));
                }
            }
        }
        out
    }

    /// Replace the ANSI palette and default foreground. Subsequent SGR
    /// color escapes resolve against the new palette; cells already in
    /// the screen keep their original RGBA.
    pub fn set_palette(&mut self, palette: [[u8; 4]; 16], default_fg: [u8; 4]) {
        self.palette = palette;
        if self.current_fg == Some(self.default_fg) {
            self.current_fg = Some(default_fg);
        }
        self.default_fg = default_fg;
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
            'h' if prefix == '?' => {
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
            'l' if prefix == '?' => {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::terminal::unpack_color;

    /// 16-color test palette: indices 0..7 standard, 8..15 bright.
    /// Distinct values per slot make assertion failures easy to read.
    const PALETTE: [[u8; 4]; 16] = [
        [0x00, 0x00, 0x00, 0xff], // 0 black
        [0xcc, 0x00, 0x00, 0xff], // 1 red
        [0x00, 0xcc, 0x00, 0xff], // 2 green
        [0xcc, 0xcc, 0x00, 0xff], // 3 yellow
        [0x00, 0x00, 0xcc, 0xff], // 4 blue
        [0xcc, 0x00, 0xcc, 0xff], // 5 magenta
        [0x00, 0xcc, 0xcc, 0xff], // 6 cyan
        [0xcc, 0xcc, 0xcc, 0xff], // 7 white
        [0x55, 0x55, 0x55, 0xff], // 8 bright black
        [0xff, 0x55, 0x55, 0xff], // 9 bright red
        [0x55, 0xff, 0x55, 0xff], // 10 bright green
        [0xff, 0xff, 0x55, 0xff], // 11 bright yellow
        [0x55, 0x55, 0xff, 0xff], // 12 bright blue
        [0xff, 0x55, 0xff, 0xff], // 13 bright magenta
        [0x55, 0xff, 0xff, 0xff], // 14 bright cyan
        [0xff, 0xff, 0xff, 0xff], // 15 bright white
    ];
    const DEFAULT_FG: [u8; 4] = [0xee, 0xee, 0xee, 0xff];

    fn buf(cols: usize, rows: usize) -> TerminalBufferInner {
        TerminalBufferInner::new(cols, rows, 100, PALETTE, DEFAULT_FG)
    }

    fn row_text(b: &TerminalBufferInner, row: usize) -> String {
        b.screen()[row]
            .iter()
            .map(|c| char::from_u32(c.ch).unwrap_or('?'))
            .collect()
    }

    fn visible_row_text(b: &TerminalBufferInner, rows: usize, scrollback: usize) -> Vec<String> {
        b.visible_rows(rows, scrollback)
            .iter()
            .map(|row| {
                row.iter()
                    .map(|c| char::from_u32(c.ch).unwrap_or('?'))
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn visible_rows_with_no_scrollback_returns_live_screen() {
        let mut t = buf(6, 3);
        t.process_output(b"a\r\nb\r\nc");
        let got = visible_row_text(&t, 3, 0);
        assert_eq!(got.len(), 3);
        assert_eq!(&got[0][..1], "a");
        assert_eq!(&got[1][..1], "b");
        assert_eq!(&got[2][..1], "c");
    }

    #[test]
    fn visible_rows_walks_back_into_history() {
        let mut t = buf(6, 3);
        // Pump 6 lines through a 3-row screen so 3 rows end up in history.
        t.process_output(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix");
        assert!(t.history_len() >= 3, "history should have scrollback");
        // scrollback = 3 should scroll the view so the top visible row
        // is `one` (the oldest scrollback line).
        let got = visible_row_text(&t, 3, 3);
        assert_eq!(got.len(), 3);
        assert_eq!(&got[0][..3], "one");
    }

    #[test]
    fn visible_rows_clamps_scrollback_past_history() {
        let mut t = buf(6, 3);
        t.process_output(b"x");
        // Asking for 10 lines of scrollback when there's none must not
        // panic or return garbage -- it should silently clamp.
        let got = visible_row_text(&t, 3, 10);
        assert_eq!(got.len(), 3);
    }

    #[test]
    fn plain_ascii_writes_to_screen() {
        let mut t = buf(10, 3);
        t.process_output(b"hello");
        assert_eq!(&row_text(&t, 0)[..5], "hello");
        assert_eq!(t.cursor_row(), 1);
        assert_eq!(t.cursor_col(), 6);
    }

    #[test]
    fn cr_resets_column_lf_advances_row() {
        let mut t = buf(10, 3);
        t.process_output(b"hi\r\nbye");
        assert_eq!(&row_text(&t, 0)[..2], "hi");
        assert_eq!(&row_text(&t, 1)[..3], "bye");
        assert_eq!(t.cursor_row(), 2);
        assert_eq!(t.cursor_col(), 4);
    }

    #[test]
    fn backspace_moves_cursor_left_without_erasing() {
        let mut t = buf(10, 3);
        t.process_output(b"abc\x08x");
        // Backspace moves left without erasing; the next char overwrites.
        assert_eq!(&row_text(&t, 0)[..3], "abx");
    }

    #[test]
    fn tab_advances_to_next_tab_stop() {
        let mut t = buf(20, 3);
        t.process_output(b"ab\tx");
        // After "ab" cursor is at col 3; tab jumps to col 9; then 'x' goes at col 9.
        assert_eq!(t.cursor_col(), 10);
        assert_eq!(t.screen()[0][8].ch, 'x' as u32);
    }

    #[test]
    fn sgr_basic_color_sets_fg() {
        let mut t = buf(10, 3);
        t.process_output(b"\x1b[31mR");
        assert_eq!(unpack_color(t.screen()[0][0].fg), Some(PALETTE[1]));
    }

    #[test]
    fn sgr_reset_returns_to_default_fg() {
        let mut t = buf(10, 3);
        t.process_output(b"\x1b[31mR\x1b[0mD");
        assert_eq!(unpack_color(t.screen()[0][0].fg), Some(PALETTE[1]));
        assert_eq!(unpack_color(t.screen()[0][1].fg), Some(DEFAULT_FG));
    }

    #[test]
    fn sgr_bright_color_uses_high_palette_slot() {
        let mut t = buf(10, 3);
        t.process_output(b"\x1b[91mB"); // bright red
        assert_eq!(unpack_color(t.screen()[0][0].fg), Some(PALETTE[9]));
    }

    #[test]
    fn sgr_256_color_indexed() {
        let mut t = buf(10, 3);
        // 256-color index 196 = (196-16) = 180; r = (180/36)%6 = 5 → 255, g = (180/6)%6 = 0 → 0, b = 180%6 = 0 → 0.
        t.process_output(b"\x1b[38;5;196mX");
        assert_eq!(
            unpack_color(t.screen()[0][0].fg),
            Some([0xff, 0x00, 0x00, 0xff])
        );
    }

    #[test]
    fn sgr_256_color_grayscale_ramp() {
        let mut t = buf(10, 3);
        // 256-color index 232 = first grayscale, value = 8.
        t.process_output(b"\x1b[38;5;232mG");
        assert_eq!(unpack_color(t.screen()[0][0].fg), Some([8, 8, 8, 0xff]));
    }

    #[test]
    fn sgr_truecolor_rgb() {
        let mut t = buf(10, 3);
        t.process_output(b"\x1b[38;2;100;200;50mY");
        assert_eq!(
            unpack_color(t.screen()[0][0].fg),
            Some([100, 200, 50, 0xff])
        );
    }

    #[test]
    fn sgr_truecolor_bg() {
        let mut t = buf(10, 3);
        t.process_output(b"\x1b[48;2;10;20;30mB");
        assert_eq!(unpack_color(t.screen()[0][0].bg), Some([10, 20, 30, 0xff]));
    }

    #[test]
    fn cursor_absolute_position() {
        let mut t = buf(20, 10);
        t.process_output(b"\x1b[5;7H");
        assert_eq!(t.cursor_row(), 5);
        assert_eq!(t.cursor_col(), 7);
    }

    #[test]
    fn cursor_move_relative() {
        let mut t = buf(20, 10);
        t.process_output(b"\x1b[5;5H");
        t.process_output(b"\x1b[2A"); // up 2
        assert_eq!(t.cursor_row(), 3);
        t.process_output(b"\x1b[3B"); // down 3
        assert_eq!(t.cursor_row(), 6);
        t.process_output(b"\x1b[4C"); // right 4
        assert_eq!(t.cursor_col(), 9);
        t.process_output(b"\x1b[2D"); // left 2
        assert_eq!(t.cursor_col(), 7);
    }

    #[test]
    fn cursor_position_clamped_at_edges() {
        let mut t = buf(10, 5);
        t.process_output(b"\x1b[100;100H"); // way past bottom-right
        assert_eq!(t.cursor_row(), 5);
        assert_eq!(t.cursor_col(), 10);
    }

    #[test]
    fn save_and_restore_cursor_via_csi() {
        let mut t = buf(20, 10);
        t.process_output(b"\x1b[3;4H\x1b[s\x1b[8;9H\x1b[u");
        assert_eq!(t.cursor_row(), 3);
        assert_eq!(t.cursor_col(), 4);
    }

    #[test]
    fn save_and_restore_cursor_via_esc_7_8() {
        let mut t = buf(20, 10);
        t.process_output(b"\x1b[3;4H\x1b7\x1b[8;9H\x1b8");
        assert_eq!(t.cursor_row(), 3);
        assert_eq!(t.cursor_col(), 4);
    }

    #[test]
    fn erase_in_line_full() {
        let mut t = buf(10, 3);
        t.process_output(b"hello\x1b[2K");
        for cell in &t.screen()[0] {
            assert_eq!(cell.ch, ' ' as u32);
        }
    }

    #[test]
    fn erase_in_line_to_end() {
        let mut t = buf(10, 3);
        t.process_output(b"hello\x1b[3D\x1b[0K");
        // After "hello" cursor is at col 6, then back 3 → col 3, then erase to end.
        // Cells 0,1 should still be 'h','e'; cells 2..9 should be blank.
        assert_eq!(t.screen()[0][0].ch, 'h' as u32);
        assert_eq!(t.screen()[0][1].ch, 'e' as u32);
        assert_eq!(t.screen()[0][2].ch, ' ' as u32);
    }

    #[test]
    fn erase_in_display_full_clears_screen() {
        let mut t = buf(10, 3);
        t.process_output(b"abc\r\ndef\x1b[2J");
        for row in t.screen() {
            for cell in row {
                assert_eq!(cell.ch, ' ' as u32);
            }
        }
    }

    #[test]
    fn alt_screen_toggle_preserves_main() {
        let mut t = buf(10, 3);
        t.process_output(b"main");
        t.process_output(b"\x1b[?1049h"); // enter alt
        // Alt screen is blank.
        for cell in &t.screen()[0] {
            assert_eq!(cell.ch, ' ' as u32);
        }
        t.process_output(b"alt");
        t.process_output(b"\x1b[?1049l"); // exit alt
        // Main screen restored.
        assert_eq!(&row_text(&t, 0)[..4], "main");
    }

    #[test]
    fn scroll_region_set_does_not_panic() {
        let mut t = buf(10, 10);
        t.process_output(b"\x1b[3;7r");
        // Setting a scroll region returns the cursor to home per VT spec.
        assert_eq!(t.cursor_row(), 1);
        assert_eq!(t.cursor_col(), 1);
    }

    #[test]
    fn dsr_cursor_position_report() {
        let mut t = buf(20, 10);
        t.process_output(b"\x1b[5;7H");
        let reply = t.process_output_and_collect_replies(b"\x1b[6n");
        assert_eq!(reply, b"\x1b[5;7R");
    }

    #[test]
    fn da_device_attributes_query_returns_reply() {
        let mut t = buf(20, 10);
        let reply = t.process_output_and_collect_replies(b"\x1b[c");
        assert!(!reply.is_empty(), "expected DA reply");
        assert!(
            reply.starts_with(b"\x1b["),
            "reply should be CSI: {reply:?}"
        );
    }

    #[test]
    fn malformed_csi_does_not_panic() {
        let mut t = buf(10, 3);
        // Garbage params, oversized values, empty final-char sequences.
        t.process_output(b"\x1b[;;;;m");
        t.process_output(b"\x1b[99999A");
        t.process_output(b"\x1b[99999B");
        t.process_output(b"\x1b[99999C");
        // Reset to a known state, then verify the parser still writes plain text.
        t.process_output(b"\x1b[2J\x1b[Hok");
        assert_eq!(&row_text(&t, 0)[..2], "ok");
    }

    #[test]
    fn split_escape_sequence_across_calls() {
        let mut t = buf(10, 3);
        t.process_output(b"\x1b[3");
        t.process_output(b"1m");
        t.process_output(b"R");
        // The split sequence must still apply red.
        assert_eq!(unpack_color(t.screen()[0][0].fg), Some(PALETTE[1]));
    }

    #[test]
    fn newline_at_bottom_scrolls_screen() {
        let mut t = buf(5, 3);
        t.process_output(b"r1\r\nr2\r\nr3\r\nr4");
        // After 4 rows in a 3-row screen, "r1" should be scrolled into history;
        // visible screen should now show r2, r3, r4.
        assert_eq!(&row_text(&t, 0)[..2], "r2");
        assert_eq!(&row_text(&t, 1)[..2], "r3");
        assert_eq!(&row_text(&t, 2)[..2], "r4");
    }

    #[test]
    fn box_drawing_chars_normalize_to_ascii() {
        let mut t = buf(10, 3);
        // Box-drawing chars from normalize_char's table.
        t.process_output("│─┌".as_bytes());
        assert_eq!(t.screen()[0][0].ch, '|' as u32);
        assert_eq!(t.screen()[0][1].ch, '-' as u32);
        assert_eq!(t.screen()[0][2].ch, '+' as u32);
    }

    #[test]
    fn utf8_multi_byte_char_writes_one_cell() {
        let mut t = buf(10, 3);
        t.process_output("é".as_bytes());
        assert_eq!(t.screen()[0][0].ch, 'é' as u32);
        assert_eq!(t.cursor_col(), 2);
    }

    #[test]
    fn delete_chars_shifts_left_and_blanks_tail() {
        let mut t = buf(10, 3);
        t.process_output(b"abcdefgh\x1b[1G\x1b[2P");
        // Delete 2 chars from col 1 → "cdefgh  "
        assert_eq!(&row_text(&t, 0)[..6], "cdefgh");
        assert_eq!(t.screen()[0][8].ch, ' ' as u32);
    }

    #[test]
    fn insert_chars_shifts_right() {
        let mut t = buf(10, 3);
        t.process_output(b"abcdef\x1b[1G\x1b[2@");
        // Insert 2 chars at col 1 → "  abcdef"
        assert_eq!(t.screen()[0][0].ch, ' ' as u32);
        assert_eq!(t.screen()[0][1].ch, ' ' as u32);
        assert_eq!(t.screen()[0][2].ch, 'a' as u32);
    }

    #[test]
    fn esc_c_full_reset() {
        let mut t = buf(10, 3);
        t.process_output(b"\x1b[31mhello\x1b[5;5H");
        t.process_output(b"\x1bc");
        // Cursor home, default fg, screen blank.
        assert_eq!(t.cursor_row(), 1);
        assert_eq!(t.cursor_col(), 1);
        assert_eq!(t.screen()[0][0].ch, ' ' as u32);
    }

    #[test]
    fn resize_preserves_recent_rows() {
        let mut t = buf(10, 5);
        t.process_output(b"r1\r\nr2\r\nr3\r\nr4\r\nr5");
        t.resize(10, 3);
        // The bottom 3 rows (r3, r4, r5) should be retained.
        assert_eq!(&row_text(&t, 0)[..2], "r3");
        assert_eq!(&row_text(&t, 1)[..2], "r4");
        assert_eq!(&row_text(&t, 2)[..2], "r5");
    }

    #[test]
    fn empty_input_is_noop() {
        let mut t = buf(10, 3);
        t.process_output(b"");
        assert_eq!(t.cursor_row(), 1);
        assert_eq!(t.cursor_col(), 1);
    }

    #[test]
    fn null_byte_is_ignored() {
        let mut t = buf(10, 3);
        t.process_output(b"a\x00b");
        assert_eq!(t.screen()[0][0].ch, 'a' as u32);
        assert_eq!(t.screen()[0][1].ch, 'b' as u32);
    }
}
