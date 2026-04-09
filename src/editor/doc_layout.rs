/// Pixel width of a character at a given column, accounting for tab stops.
pub fn char_width_at(column: usize, ch: char, tab_size: usize, cell_width: f64) -> f64 {
    if ch == '\t' {
        let tab_cols = tab_size.max(1);
        let offset = (column.saturating_sub(1)) % tab_cols;
        ((tab_cols - offset) as f64) * cell_width
    } else {
        cell_width
    }
}

/// Pixel x-offset for a given column in a line of text.
pub fn col_x_offset(text: &str, col: usize, tab_size: usize, cell_width: f64) -> f64 {
    let mut x = 0.0;
    let mut column = 1usize;
    for ch in text.chars() {
        if column >= col {
            break;
        }
        x += char_width_at(column, ch, tab_size, cell_width);
        column += ch.len_utf8();
    }
    x
}

/// Column (1-based byte offset) at a given pixel x-offset, snapping to the nearest character.
pub fn x_offset_col(text: &str, x: f64, tab_size: usize, cell_width: f64) -> usize {
    let mut xoffset = 0.0;
    let mut column = 1usize;
    for ch in text.chars() {
        let width = char_width_at(column, ch, tab_size, cell_width);
        if xoffset + width >= x {
            return if x <= xoffset + (width / 2.0) {
                column
            } else {
                column + ch.len_utf8()
            };
        }
        xoffset += width;
        column += ch.len_utf8();
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn col_x_offset_ascii() {
        assert_eq!(col_x_offset("hello", 3, 4, 8.0), 16.0);
    }

    #[test]
    fn col_x_offset_tab() {
        assert_eq!(col_x_offset("\tab", 2, 4, 8.0), 32.0);
    }

    #[test]
    fn x_offset_col_tab_before() {
        assert_eq!(x_offset_col("\tab", 16.0, 4, 8.0), 1);
    }

    #[test]
    fn x_offset_col_tab_after() {
        assert_eq!(x_offset_col("\tab", 24.0, 4, 8.0), 2);
    }

    #[test]
    fn x_offset_col_snap_to_nearest() {
        // cell_width=10, "ab" -> a occupies [0,10), b occupies [10,20)
        // x=4 -> closer to start of 'a' -> col 1
        assert_eq!(x_offset_col("ab", 4.0, 4, 10.0), 1);
        // x=6 -> closer to start of 'b' -> col 2
        assert_eq!(x_offset_col("ab", 6.0, 4, 10.0), 2);
    }

    #[test]
    fn x_offset_col_past_end() {
        assert_eq!(x_offset_col("ab", 100.0, 4, 8.0), 2);
    }

    #[test]
    fn char_width_at_normal() {
        assert_eq!(char_width_at(1, 'a', 4, 8.0), 8.0);
    }

    #[test]
    fn char_width_at_tab_start() {
        // Tab at column 1 with tab_size 4 -> 4 cells
        assert_eq!(char_width_at(1, '\t', 4, 8.0), 32.0);
    }

    #[test]
    fn char_width_at_tab_mid() {
        // Tab at column 3 with tab_size 4 -> 2 cells to next stop
        assert_eq!(char_width_at(3, '\t', 4, 8.0), 16.0);
    }
}
