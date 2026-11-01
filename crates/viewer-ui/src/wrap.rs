//! Text that wraps only at spaces, the way Korean text is usually set (CSS `word-break: keep-all`).
//!
//! GPUI's own wrapping may break between any two Hangul syllables and split a word. Here a line breaks only at
//! whitespace or a newline. A word wider than a whole line is broken between graphemes.

use std::ops::Range;

use gpui::{
    App, AvailableSpace, Bounds, Element, ElementId, GlobalElementId, InspectorElementId,
    IntoElement, LayoutId, Pixels, ShapedLine, SharedString, Size, Style, TextAlign, Window, point,
    px, relative, size,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::chart::TextShaper;

/// Byte ranges of the lines `text` wraps into within `width`, where `measure` gives the width of a slice.
/// Without a width, each paragraph is one line. Lines leave out the whitespace at their ends, and an empty
/// paragraph is an empty line.
pub(crate) fn wrap_ranges(
    text: &str,
    width: Option<Pixels>,
    measure: impl Fn(&str) -> Pixels,
) -> Vec<Range<usize>> {
    let mut lines = Vec::new();
    let mut base = 0;
    for paragraph in text.split('\n') {
        let start = base;
        base += paragraph.len() + 1;
        let paragraph = paragraph.strip_suffix('\r').unwrap_or(paragraph);
        let words = words(paragraph);
        let (Some(first), Some(last)) = (words.first(), words.last()) else {
            lines.push(start..start);
            continue;
        };
        let Some(width) = width else {
            lines.push(start + first.start..start + last.end);
            continue;
        };
        // Start and end of the line being filled.
        let mut line: Option<Range<usize>> = None;
        for word in words {
            if let Some(current) = &line {
                if measure(&paragraph[current.start..word.end]) <= width {
                    line = Some(current.start..word.end);
                    continue;
                }
                lines.push(start + current.start..start + current.end);
            }
            let mut word_start = word.start;
            loop {
                let rest = &paragraph[word_start..word.end];
                if measure(rest) <= width {
                    break;
                }
                let split = split_point(rest, width, &measure);
                if split >= rest.len() {
                    break;
                }
                lines.push(start + word_start..start + word_start + split);
                word_start += split;
            }
            line = Some(word_start..word.end);
        }
        if let Some(current) = line {
            lines.push(start + current.start..start + current.end);
        }
    }
    lines
}

/// Whether a line may break at `ch`: whitespace other than the no-break spaces.
fn is_break(ch: char) -> bool {
    ch.is_whitespace() && !matches!(ch, '\u{00A0}' | '\u{2007}' | '\u{202F}')
}

/// Byte ranges of the words between break characters.
fn words(text: &str) -> Vec<Range<usize>> {
    let mut words = Vec::new();
    let mut start = None;
    for (ix, ch) in text.char_indices() {
        match (is_break(ch), start) {
            (false, None) => start = Some(ix),
            (true, Some(begin)) => {
                words.push(begin..ix);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(begin) = start {
        words.push(begin..text.len());
    }
    words
}

/// Length of the longest grapheme prefix of `word` that fits `width`; at least one grapheme.
fn split_point(word: &str, width: Pixels, measure: &impl Fn(&str) -> Pixels) -> usize {
    let mut fit = 0;
    for (ix, grapheme) in word.grapheme_indices(true) {
        let end = ix + grapheme.len();
        if measure(&word[..end]) > width {
            break;
        }
        fit = end;
    }
    if fit > 0 {
        fit
    } else {
        word.graphemes(true).next().map_or(word.len(), str::len)
    }
}

/// Width a measured element gets from its layout: the known width, else a definite available width.
pub(crate) fn definite_width(
    known: Size<Option<Pixels>>,
    available: Size<AvailableSpace>,
) -> Option<Pixels> {
    known.width.or(match available.width {
        AvailableSpace::Definite(width) => Some(width),
        _ => None,
    })
}

/// Shapes the lines `text` wraps into within `width`.
pub(crate) fn wrap_lines(
    shaper: &TextShaper,
    text: &str,
    width: Option<Pixels>,
) -> Vec<ShapedLine> {
    wrap_ranges(text, width, |slice| shaper.width(slice))
        .into_iter()
        .map(|range| shaper.shape(text[range].to_string().into()))
        .collect()
}

/// Row labels of a chart wrapped into a left column. The column is as wide as the widest label, up to
/// `max_width`; wider labels wrap at spaces.
pub(crate) struct LabelColumn {
    pub(crate) width: Pixels,
    /// Line ranges of each label.
    pub(crate) lines: Vec<Vec<Range<usize>>>,
}

impl LabelColumn {
    pub(crate) fn new(labels: &[SharedString], max_width: Pixels, shaper: &TextShaper) -> Self {
        let measure = |text: &str| shaper.width(text);
        let widest = labels
            .iter()
            .flat_map(|label| {
                wrap_ranges(label, None, measure)
                    .into_iter()
                    .map(|range| measure(&label[range]))
            })
            .fold(px(0.), Pixels::max);
        let width = widest.min(max_width.max(px(0.)));
        let lines = labels
            .iter()
            .map(|label| wrap_ranges(label, Some(width), measure))
            .collect();
        Self { width, lines }
    }

    /// Height of the label of `row`.
    pub(crate) fn height(&self, row: usize, line_height: Pixels) -> Pixels {
        line_height * self.lines.get(row).map_or(0, Vec::len) as f32
    }
}

/// Text that fills its parent's width and wraps at spaces. Its height follows the line count.
pub struct WrappedText {
    text: SharedString,
    align_right: bool,
}

impl WrappedText {
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            align_right: false,
        }
    }

    /// Aligns each line to the right edge.
    pub fn right(mut self) -> Self {
        self.align_right = true;
        self
    }
}

pub struct WrappedTextLayout {
    lines: Vec<ShapedLine>,
    line_height: Pixels,
}

impl IntoElement for WrappedText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for WrappedText {
    type RequestLayoutState = ();
    type PrepaintState = WrappedTextLayout;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        _cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let shaper = TextShaper::new(window);
        let text = self.text.clone();
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let layout_id =
            window.request_measured_layout(style, move |known, available, _window, _cx| {
                let width = definite_width(known, available);
                let lines = wrap_ranges(&text, width, |slice| shaper.width(slice));
                let width = width.unwrap_or_else(|| {
                    lines
                        .iter()
                        .map(|range| shaper.width(&text[range.clone()]))
                        .fold(px(0.), Pixels::max)
                });
                size(width, shaper.line_height * lines.len() as f32)
            });
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        let shaper = TextShaper::new(window).color(window.text_style().color);
        WrappedTextLayout {
            lines: wrap_lines(&shaper, &self.text, Some(bounds.size.width)),
            line_height: shaper.line_height,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        layout: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        for (ix, line) in layout.lines.iter().enumerate() {
            let x = if self.align_right {
                bounds.right() - line.width()
            } else {
                bounds.left()
            };
            let y = bounds.top() + layout.line_height * ix as f32;
            let _ = line.paint(
                point(x, y),
                layout.line_height,
                TextAlign::Left,
                None,
                window,
                cx,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every character is 1 px wide.
    fn chars(text: &str) -> Pixels {
        px(text.chars().count() as f32)
    }

    fn lines(text: &str, width: f32) -> Vec<&str> {
        wrap_ranges(text, Some(px(width)), chars)
            .into_iter()
            .map(|range| &text[range])
            .collect()
    }

    #[test]
    fn breaks_only_at_spaces() {
        assert_eq!(
            lines("운행 기록이 없습니다", 7.),
            ["운행 기록이", "없습니다"]
        );
        assert_eq!(lines("운행 기록이 없습니다", 20.), ["운행 기록이 없습니다"]);
        assert_eq!(lines("a  b   c", 4.), ["a  b", "c"]);
        // A no-break space joins its neighbours into one word.
        assert_eq!(lines("134번\u{a0}· 1134번", 7.), ["134번\u{a0}·", "1134번"]);
    }

    #[test]
    fn breaks_long_words_between_graphemes() {
        assert_eq!(
            lines("시외고속버스터미널 앞", 4.),
            ["시외고속", "버스터미", "널 앞"]
        );
        // A grapheme wider than the line stays whole.
        assert_eq!(lines("가나", 0.5), ["가", "나"]);
    }

    #[test]
    fn keeps_newlines_and_blank_lines() {
        assert_eq!(lines("첫 줄\n\n셋째 줄", 10.), ["첫 줄", "", "셋째 줄"]);
        assert_eq!(lines("  ", 10.), [""]);
        assert_eq!(lines("", 10.), [""]);
        let unwrapped: Vec<_> = wrap_ranges("a b\r\nc", None, chars)
            .into_iter()
            .map(|range| &"a b\r\nc"[range])
            .collect();
        assert_eq!(unwrapped, ["a b", "c"]);
    }
}
