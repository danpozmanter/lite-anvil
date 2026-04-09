/// Extract the leading whitespace (indent) from a line.
pub fn line_indent(text: &str) -> String {
    text.chars()
        .take_while(|c| *c == '\t' || *c == ' ')
        .collect()
}

/// Check if position (line, col) is strictly inside the range (l1,c1)-(l2,c2).
pub fn is_in_selection(line: i64, col: i64, l1: i64, c1: i64, l2: i64, c2: i64) -> bool {
    if line < l1 || line > l2 {
        return false;
    }
    if line == l1 && col <= c1 {
        return false;
    }
    if line == l2 && col > c2 {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_indent_spaces() {
        assert_eq!(line_indent("    hello"), "    ");
    }

    #[test]
    fn line_indent_none() {
        assert_eq!(line_indent("hello"), "");
    }

    #[test]
    fn is_in_selection_inside() {
        assert!(is_in_selection(2, 5, 1, 1, 3, 10));
    }

    #[test]
    fn is_in_selection_outside() {
        assert!(!is_in_selection(0, 5, 1, 1, 3, 10));
    }

}
