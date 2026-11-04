//! Source files as text and a table of where each line starts. The text is kept whole, so that copying gives back exactly
//! what the file holds and byte positions from outside apply directly.

use std::ops::Range;

/// A source file, without its byte order mark.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeDocument {
    text: String,
    /// Where each line starts. A file has at least one line, and a line break at the end starts an empty last line.
    line_starts: Vec<usize>,
    /// Width of the widest line in cells, with tabs at four columns and wide characters as two.
    widest_line: usize,
}

const TAB_CELLS: usize = 4;

impl CodeDocument {
    pub fn new(text: String) -> Self {
        let bytes = text.as_bytes();
        let mut line_starts = vec![0];
        let mut ix = 0;
        while ix < bytes.len() {
            match bytes[ix] {
                b'\n' => line_starts.push(ix + 1),
                b'\r' => {
                    if bytes.get(ix + 1) == Some(&b'\n') {
                        ix += 1;
                    }
                    line_starts.push(ix + 1);
                }
                _ => {}
            }
            ix += 1;
        }
        let mut document = Self {
            text,
            line_starts,
            widest_line: 0,
        };
        document.widest_line = (0..document.line_count())
            .map(|line| line_cells(&document.text[document.line_range(line)]))
            .max()
            .unwrap_or(0);
        document
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    pub fn widest_line(&self) -> usize {
        self.widest_line
    }

    /// Bytes of a line, without its line break.
    pub fn line_range(&self, line: usize) -> Range<usize> {
        let start = self.line_starts[line];
        let next = self
            .line_starts
            .get(line + 1)
            .copied()
            .unwrap_or(self.text.len());
        let mut end = next;
        let bytes = self.text.as_bytes();
        while end > start && (bytes[end - 1] == b'\n' || bytes[end - 1] == b'\r') {
            end -= 1;
        }
        start..end
    }

    pub fn line(&self, line: usize) -> &str {
        &self.text[self.line_range(line)]
    }

    /// Byte offset of a position in a line.
    pub fn offset(&self, line: usize, byte: usize) -> usize {
        self.line_starts[line] + byte
    }
}

/// Width of a line in cells.
fn line_cells(line: &str) -> usize {
    let mut cells = 0;
    for ch in line.chars() {
        cells += match ch {
            '\t' => TAB_CELLS - cells % TAB_CELLS,
            ch if (ch as u32) < 0x1100 => 1,
            _ => 2,
        };
    }
    cells
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        let document = CodeDocument::new(text.to_string());
        (0..document.line_count())
            .map(|line| document.line(line).to_string())
            .collect()
    }

    #[test]
    fn splits_on_every_line_break_kind() {
        assert_eq!(lines("a\nb\r\nc\rd"), ["a", "b", "c", "d"]);
        assert_eq!(lines("끝\n"), ["끝", ""]);
        assert_eq!(lines(""), [""]);
        assert_eq!(lines("\n\n"), ["", "", ""]);
        assert_eq!(lines("\r\n\r\n"), ["", "", ""]);
    }

    #[test]
    fn maps_lines_to_offsets_in_the_text() {
        let document = CodeDocument::new("가\r\nbc\n".to_string());
        assert_eq!(document.line_range(0), 0..3);
        assert_eq!(document.line_range(1), 5..7);
        assert_eq!(document.line_range(2), 8..8);
        assert_eq!(document.offset(1, 1), 6);
        assert_eq!(
            &document.text()[document.offset(0, 0)..document.offset(1, 1)],
            "가\r\nb"
        );
    }

    #[test]
    fn measures_the_widest_line_in_cells() {
        assert_eq!(CodeDocument::new("ab\n가나다".into()).widest_line(), 6);
        assert_eq!(CodeDocument::new("\tx\ny\t".into()).widest_line(), 5);
        assert_eq!(CodeDocument::new("".into()).widest_line(), 0);
    }
}
