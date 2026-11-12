//! Convert the protocol's UTF-16 columns before indexing UTF-8 source.

use tower_lsp::lsp_types::{Position, Range};

/// Return a zero-based line without its line ending.
pub fn line_at(source: &str, line: u32) -> Option<&str> {
    source
        .split('\n')
        .nth(line as usize)
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
}

/// Out-of-line columns clamp to the end; a split surrogate has no byte boundary.
pub fn byte_column(line: &str, character: u32) -> Option<usize> {
    let mut units = 0;
    for (byte, ch) in line.char_indices() {
        if units == character {
            return Some(byte);
        }
        units += ch.len_utf16() as u32;
        if units > character {
            return None;
        }
    }
    Some(line.len())
}

/// Parser columns count Unicode scalar values and are one-based.
pub fn parser_range(source: &str, line: usize, column: usize) -> Range {
    let line_index = line.saturating_sub(1).min(u32::MAX as usize) as u32;
    let Some(text) = line_at(source, line_index) else {
        return Range::default();
    };
    let scalar = column.saturating_sub(1);
    let start = text
        .chars()
        .take(scalar)
        .map(char::len_utf16)
        .sum::<usize>();
    let width = text.chars().nth(scalar).map_or(0, char::len_utf16);
    Range::new(
        Position::new(line_index, start as u32),
        Position::new(line_index, (start + width) as u32),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_columns_respect_surrogates_and_line_endings() {
        let line = "中😀tempo";
        assert_eq!(byte_column(line, 1), Some(3));
        assert_eq!(byte_column(line, 2), None);
        assert_eq!(byte_column(line, 3), Some(7));
        assert_eq!(byte_column(line, u32::MAX), Some(line.len()));
        assert_eq!(line_at("中\r\n", 0), Some("中"));
        assert_eq!(line_at("中\r\n", 1), Some(""));
        assert_eq!(line_at("中\r\n", u32::MAX), None);
        assert_eq!(
            parser_range(line, 1, 3),
            Range::new(Position::new(0, 3), Position::new(0, 4))
        );
        assert_eq!(parser_range("", 1, 1), Range::default());
    }
}
