use mlua::prelude::*;
use parking_lot::Mutex;

#[derive(Clone, Debug)]
struct Cell {
    ch: String,
    fg: Option<[u8; 4]>,
    bg: Option<[u8; 4]>,
}

impl Cell {
    fn blank(default_fg: [u8; 4]) -> Self {
        Self {
            ch: " ".to_string(),
            fg: Some(default_fg),
            bg: None,
        }
    }
}

struct TerminalBufferInner {
    cols: usize,
    rows: usize,
    scrollback: usize,
    screen: Vec<Vec<Cell>>,
    history: Vec<Vec<Cell>>,
    cursor_row: usize,
    cursor_col: usize,
    default_fg: [u8; 4],
    current_fg: Option<[u8; 4]>,
    current_bg: Option<[u8; 4]>,
    palette: [[u8; 4]; 16],
    escape_state: EscapeState,
    escape_buffer: String,
    osc_esc: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EscapeState {
    None,
    Esc,
    Csi,
    Osc,
}

impl TerminalBufferInner {
    fn new(cols: usize, rows: usize, scrollback: usize, palette: [[u8; 4]; 16], default_fg: [u8; 4]) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let mut inner = Self {
            cols,
            rows,
            scrollback: scrollback.max(1),
            screen: Vec::new(),
            history: Vec::new(),
            cursor_row: 1,
            cursor_col: 1,
            default_fg,
            current_fg: Some(default_fg),
            current_bg: None,
            palette,
            escape_state: EscapeState::None,
            escape_buffer: String::new(),
            osc_esc: false,
        };
        inner.reset_screen();
        inner
    }

    fn blank_row(&self) -> Vec<Cell> {
        (0..self.cols).map(|_| Cell::blank(self.default_fg)).collect()
    }

    fn reset_screen(&mut self) {
        self.screen = (0..self.rows).map(|_| self.blank_row()).collect();
    }

    fn resize(&mut self, cols: usize, rows: usize) {
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
    }

    fn clear(&mut self) {
        self.history.clear();
        self.current_fg = Some(self.default_fg);
        self.current_bg = None;
        self.cursor_row = 1;
        self.cursor_col = 1;
        self.escape_state = EscapeState::None;
        self.escape_buffer.clear();
        self.osc_esc = false;
        self.reset_screen();
    }

    fn push_history(&mut self, row: Vec<Cell>) {
        self.history.push(row);
        if self.history.len() > self.scrollback {
            self.history.remove(0);
        }
    }

    fn scroll_screen(&mut self) {
        if !self.screen.is_empty() {
            let row = self.screen.remove(0);
            self.push_history(row);
        }
        self.screen.push(self.blank_row());
    }

    fn put_char(&mut self, ch: &str) {
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
            ch: ch.to_string(),
            fg: self.current_fg,
            bg: self.current_bg,
        };
        self.cursor_col += 1;
    }

    fn newline(&mut self) {
        self.cursor_col = 1;
        self.cursor_row += 1;
        if self.cursor_row > self.rows {
            self.scroll_screen();
            self.cursor_row = self.rows;
        }
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
        for col in start_col..=end_col.min(self.cols) {
            row[col - 1] = blank.clone();
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
        let params = if params.is_empty() { vec![0] } else { params.to_vec() };
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
        let params = body
            .split(';')
            .map(|item| item.parse::<i64>().unwrap_or(0))
            .collect::<Vec<_>>();
        let p1 = *params.first().unwrap_or(&0);
        let p2 = *params.get(1).unwrap_or(&0);

        match final_char {
            'A' => self.cursor_row = self.cursor_row.saturating_sub(p1.max(1) as usize).clamp(1, self.rows),
            'B' => self.cursor_row = (self.cursor_row + p1.max(1) as usize).clamp(1, self.rows),
            'C' => self.cursor_col = (self.cursor_col + p1.max(1) as usize).clamp(1, self.cols),
            'D' => self.cursor_col = self.cursor_col.saturating_sub(p1.max(1) as usize).clamp(1, self.cols),
            'H' | 'f' => {
                self.cursor_row = (if p1 <= 0 { 1 } else { p1 as usize }).clamp(1, self.rows);
                self.cursor_col = (if p2 <= 0 { 1 } else { p2 as usize }).clamp(1, self.cols);
            }
            'J' => self.clear_screen(p1),
            'K' => self.clear_line(p1),
            'm' => self.apply_sgr(&params),
            _ => {}
        }
    }

    fn decode_utf8_char(bytes: &[u8], i: usize) -> (String, usize) {
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
        (String::from_utf8_lossy(&bytes[i..end]).into_owned(), end)
    }

    fn process_output(&mut self, bytes: &[u8]) {
        let mut i = 0usize;
        while i < bytes.len() {
            let b = bytes[i];
            match self.escape_state {
                EscapeState::Osc => {
                    if b == 7 {
                        self.escape_state = EscapeState::None;
                    } else if b == 27 {
                        self.osc_esc = true;
                    } else if self.osc_esc && b == 92 {
                        self.escape_state = EscapeState::None;
                        self.osc_esc = false;
                    } else {
                        self.osc_esc = false;
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
                            self.osc_esc = false;
                        }
                        b'c' => {
                            self.clear();
                            self.escape_state = EscapeState::None;
                        }
                        _ => self.escape_state = EscapeState::None,
                    }
                    i += 1;
                }
                EscapeState::Csi => {
                    self.escape_buffer.push(b as char);
                    if (b'@'..=b'~').contains(&b) {
                        let sequence = self.escape_buffer.clone();
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
                        let next_tab = (self.cursor_col + (8 - ((self.cursor_col - 1) % 8))).min(self.cols + 1);
                        while self.cursor_col < next_tab {
                            self.put_char(" ");
                        }
                        i += 1;
                    }
                    0..=31 => {
                        i += 1;
                    }
                    _ => {
                        let (ch, next) = Self::decode_utf8_char(bytes, i);
                        self.put_char(&ch);
                        i = next;
                    }
                },
            }
        }
    }

    fn total_rows(&self) -> usize {
        self.history.len() + self.rows
    }

    fn row_at(&self, index: usize) -> Option<&[Cell]> {
        if index == 0 {
            return None;
        }
        if index <= self.history.len() {
            return self.history.get(index - 1).map(Vec::as_slice);
        }
        self.screen.get(index - self.history.len() - 1).map(Vec::as_slice)
    }
}

pub struct TerminalBuffer(Mutex<TerminalBufferInner>);

fn table_to_color(table: LuaTable) -> LuaResult<[u8; 4]> {
    Ok([
        table.raw_get::<u8>(1)?,
        table.raw_get::<u8>(2)?,
        table.raw_get::<u8>(3)?,
        table.raw_get::<u8>(4)?,
    ])
}

fn color_to_table(lua: &Lua, color: [u8; 4]) -> LuaResult<LuaTable> {
    let table = lua.create_table_with_capacity(4, 0)?;
    for (idx, part) in color.into_iter().enumerate() {
        table.raw_set((idx + 1) as i64, part)?;
    }
    Ok(table)
}

fn row_runs(lua: &Lua, row: &[Cell]) -> LuaResult<LuaTable> {
    let out = lua.create_table()?;
    if row.is_empty() {
        return Ok(out);
    }
    let mut idx = 1i64;
    let mut start = 0usize;
    while start < row.len() {
        let fg = row[start].fg;
        let bg = row[start].bg;
        let mut finish = start + 1;
        let mut text = row[start].ch.clone();
        while finish < row.len() && row[finish].fg == fg && row[finish].bg == bg {
            text.push_str(&row[finish].ch);
            finish += 1;
        }
        let run = lua.create_table()?;
        run.set("text", text)?;
        run.set("start_col", (start + 1) as i64)?;
        run.set("end_col", finish as i64)?;
        if let Some(fg) = fg {
            run.set("fg", color_to_table(lua, fg)?)?;
        }
        if let Some(bg) = bg {
            run.set("bg", color_to_table(lua, bg)?)?;
        }
        out.raw_set(idx, run)?;
        idx += 1;
        start = finish;
    }
    Ok(out)
}

impl LuaUserData for TerminalBuffer {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("resize", |_, this, (cols, rows): (usize, usize)| {
            this.0.lock().resize(cols, rows);
            Ok(true)
        });
        methods.add_method("clear", |_, this, ()| {
            this.0.lock().clear();
            Ok(true)
        });
        methods.add_method("process_output", |_, this, text: LuaString| {
            this.0.lock().process_output(text.as_bytes().as_ref());
            Ok(true)
        });
        methods.add_method("set_palette", |_, this, (palette_table, default_fg): (LuaTable, LuaTable)| {
            let mut palette = [[0u8; 4]; 16];
            for i in 1..=16 {
                palette[i - 1] = table_to_color(palette_table.raw_get::<LuaTable>(i as i64)?)?;
            }
            let mut inner = this.0.lock();
            inner.palette = palette;
            inner.default_fg = table_to_color(default_fg)?;
            Ok(true)
        });
        methods.add_method("total_rows", |_, this, ()| Ok(this.0.lock().total_rows() as i64));
        methods.add_method("cursor", |lua, this, ()| {
            let inner = this.0.lock();
            let table = lua.create_table()?;
            table.set("row", inner.cursor_row as i64)?;
            table.set("col", inner.cursor_col as i64)?;
            table.set("history", inner.history.len() as i64)?;
            Ok(table)
        });
        methods.add_method("render_rows", |lua, this, (first, last): (usize, usize)| {
            let inner = this.0.lock();
            let out = lua.create_table()?;
            let mut idx = 1i64;
            for row_index in first..=last {
                if let Some(row) = inner.row_at(row_index) {
                    let row_table = lua.create_table()?;
                    row_table.set("index", row_index as i64)?;
                    row_table.set("runs", row_runs(lua, row)?)?;
                    out.raw_set(idx, row_table)?;
                    idx += 1;
                }
            }
            Ok(out)
        });
    }
}

pub fn make_module(lua: &Lua) -> LuaResult<LuaTable> {
    let module = lua.create_table()?;
    module.set(
        "new",
        lua.create_function(|_, (cols, rows, scrollback, palette_table, default_fg): (usize, usize, usize, LuaTable, LuaTable)| {
            let mut palette = [[0u8; 4]; 16];
            for i in 1..=16 {
                palette[i - 1] = table_to_color(palette_table.raw_get::<LuaTable>(i as i64)?)?;
            }
            let default_fg = table_to_color(default_fg)?;
            Ok(TerminalBuffer(Mutex::new(TerminalBufferInner::new(
                cols,
                rows,
                scrollback,
                palette,
                default_fg,
            ))))
        })?,
    )?;
    Ok(module)
}

#[cfg(test)]
mod tests {
    use super::TerminalBufferInner;

    fn palette() -> [[u8; 4]; 16] {
        [[255, 255, 255, 255]; 16]
    }

    #[test]
    fn processes_basic_output() {
        let mut buf = TerminalBufferInner::new(8, 2, 10, palette(), [255, 255, 255, 255]);
        buf.process_output(b"abc");
        assert_eq!(buf.screen[0][0].ch, "a");
        assert_eq!(buf.cursor_col, 4);
    }

    #[test]
    fn scrolls_into_history() {
        let mut buf = TerminalBufferInner::new(4, 1, 10, palette(), [255, 255, 255, 255]);
        buf.process_output(b"one\ntwo\n");
        assert!(!buf.history.is_empty());
    }

    #[test]
    fn applies_sgr_colors() {
        let mut colors = palette();
        colors[1] = [255, 0, 0, 255];
        let mut buf = TerminalBufferInner::new(4, 1, 10, colors, [255, 255, 255, 255]);
        buf.process_output(b"\x1b[31mx");
        assert_eq!(buf.screen[0][0].fg, Some([255, 0, 0, 255]));
    }
}
