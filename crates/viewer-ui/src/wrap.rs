//! Text that wraps only at spaces, the way Korean text is usually set (CSS `word-break: keep-all`).
//!
//! GPUI's own wrapping may break between any two Hangul syllables and split a word. Here a line breaks only at
//! whitespace or a newline. A word wider than a whole line is broken between graphemes.
//!
//! [`WrappedText`] draws one style. [`RichText`] draws styled runs and reports the mouse as byte positions, and
//! [`TextSelection`] keeps a selection across several of them.

use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    App, AvailableSpace, Bounds, CursorStyle, DispatchPhase, Element, ElementId, Font, FontStyle,
    FontWeight, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ShapedLine,
    SharedString, Size, StrikethroughStyle, Style, TextAlign, TextRun, UnderlineStyle, Window,
    WindowTextSystem, fill, point, px, relative, size,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::chart::TextShaper;
use crate::theme;

/// Columns between tab stops in text that keeps its line layout.
const TAB_COLUMNS: usize = 4;
/// Words up to this length are measured whole before looking for a place to break them.
const SHORT_WORD_BYTES: usize = 256;
/// How far a selection that runs on past a line's end reaches beyond the line's text.
const SELECTION_TAIL: Pixels = px(4.);

/// Byte ranges of the lines `text` wraps into within `width`, where `measure` gives the width of a slice.
/// Without a width, each paragraph is one line. Lines leave out the whitespace at their ends, and an empty
/// paragraph is an empty line.
pub(crate) fn wrap_ranges(
    text: &str,
    width: Option<Pixels>,
    measure: impl Fn(&str) -> Pixels,
) -> Vec<Range<usize>> {
    wrap_words(text, width, |range| measure(&text[range]))
}

/// [`wrap_ranges`] with `measure` taking byte ranges of `text`, for text whose width depends on where it stands.
fn wrap_words(
    text: &str,
    width: Option<Pixels>,
    measure: impl Fn(Range<usize>) -> Pixels,
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
            let word = start + word.start..start + word.end;
            if let Some(current) = &line {
                if measure(current.start..word.end) <= width {
                    line = Some(current.start..word.end);
                    continue;
                }
                lines.push(current.clone());
            }
            let mut word_start = word.start;
            loop {
                let split = split_point(text, word_start..word.end, width, &measure);
                if split >= word.end - word_start {
                    break;
                }
                lines.push(word_start..word_start + split);
                word_start += split;
            }
            line = Some(word_start..word.end);
        }
        if let Some(current) = line {
            lines.push(current);
        }
    }
    lines
}

/// Byte ranges of the lines of text that keeps its line layout, such as code: every byte of a line stays on it,
/// indentation included, and a line wider than `width` continues on the next one.
fn wrap_preserving(
    text: &str,
    width: Option<Pixels>,
    measure: impl Fn(Range<usize>) -> Pixels,
) -> Vec<Range<usize>> {
    let mut lines = Vec::new();
    let mut base = 0;
    for paragraph in text.split('\n') {
        let end = base + paragraph.len();
        let mut start = base;
        base = end + 1;
        if let Some(width) = width {
            while start < end {
                let split = split_point(text, start..end, width, &measure);
                if split >= end - start {
                    break;
                }
                lines.push(start..start + split);
                start += split;
            }
        }
        lines.push(start..end);
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

/// Length of the longest grapheme prefix of a word that fits `width`; at least one grapheme. Searches by halves
/// among the graphemes that can fit, so a very long word costs no more than a line of text.
fn split_point(
    text: &str,
    word: Range<usize>,
    width: Pixels,
    measure: &impl Fn(Range<usize>) -> Pixels,
) -> usize {
    if word.len() <= SHORT_WORD_BYTES && measure(word.clone()) <= width {
        return word.len();
    }
    // Graphemes are at least about a pixel wide, so no more than this many fit on a line.
    let limit = (f32::from(width).max(0.) as usize).saturating_add(1);
    let ends: Vec<usize> = text[word.clone()]
        .grapheme_indices(true)
        .take(limit)
        .map(|(ix, grapheme)| ix + grapheme.len())
        .collect();
    let fit = ends.partition_point(|&end| measure(word.start..word.start + end) <= width);
    match fit.checked_sub(1) {
        Some(last) => ends[last],
        None => ends.first().copied().unwrap_or(word.len()),
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

/// A styled run of a [`RichText`]. Text outside every span takes the window's text style.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextSpan {
    pub range: Range<usize>,
    pub font_family: Option<SharedString>,
    pub bold: bool,
    pub italic: bool,
    pub color: Option<Hsla>,
    pub background: Option<Hsla>,
    pub underline: bool,
    pub strikethrough: bool,
}

/// The mouse over a [`RichText`], as byte positions in its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RichTextEvent {
    Down {
        index: usize,
        click_count: usize,
    },
    /// `pressed` tells whether the left button is held.
    Move {
        index: usize,
        pressed: bool,
    },
    Up {
        index: usize,
    },
}

type RichTextListener = dyn Fn(RichTextEvent, &mut Window, &mut App);

/// Text of several styles that fills its parent's width and wraps like [`WrappedText`]. Its height follows the line
/// count. Spans are sorted and do not overlap.
pub struct RichText {
    text: SharedString,
    spans: Rc<[TextSpan]>,
    preserve_lines: bool,
    align: TextAlign,
    selection: Option<Range<usize>>,
    pointer: bool,
    on_event: Option<Rc<RichTextListener>>,
}

impl RichText {
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            spans: Rc::from([]),
            preserve_lines: false,
            align: TextAlign::Left,
            selection: None,
            pointer: false,
            on_event: None,
        }
    }

    pub fn spans(mut self, spans: Vec<TextSpan>) -> Self {
        self.spans = spans.into();
        self
    }

    /// Keeps every space and line of the text, for code. Lines wider than the element continue on the next line, and
    /// tabs draw as spaces up to the next tab stop. Spans do not apply.
    pub fn preserve_lines(mut self) -> Self {
        self.preserve_lines = true;
        self
    }

    pub fn align(mut self, align: TextAlign) -> Self {
        self.align = align;
        self
    }

    /// Bytes drawn on the theme's selection color.
    pub fn selection(mut self, selection: Option<Range<usize>>) -> Self {
        self.selection = selection;
        self
    }

    /// Shows the pointing hand over the text, such as while the mouse is on a link.
    pub fn pointer(mut self, pointer: bool) -> Self {
        self.pointer = pointer;
        self
    }

    /// Reports the mouse over the text. Text with a listener shows the text cursor.
    pub fn on_event(
        mut self,
        listener: impl Fn(RichTextEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_event = Some(Rc::new(listener));
        self
    }
}

/// A position in a set of numbered texts: the text's number and a byte within it. Numbers follow reading order, so
/// positions compare directly.
pub type TextPosition = (u32, usize);

/// A selection across the [`RichText`]s of a view, kept by the view. The view numbers its texts, passes the mouse
/// events of each text here, draws each one with [`Self::bytes_in`] and builds the copy text from [`Self::ordered`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextSelection {
    /// Where the selection started and where it reaches. Either can come first in reading order.
    range: Option<(TextPosition, TextPosition)>,
    /// The left button went down on text and has not been released.
    dragging: bool,
    /// The last press landed on text.
    pressed_text: bool,
}

impl TextSelection {
    /// The left button went down on a text. One click starts an empty selection there, a double click takes the
    /// word, and further clicks take the whole text.
    pub fn press(&mut self, id: u32, text: &str, index: usize, click_count: usize) {
        self.pressed_text = true;
        self.dragging = true;
        let (from, to) = match click_count {
            0 | 1 => (index, index),
            2 => {
                let word = word_range(text, index);
                (word.start, word.end)
            }
            _ => (0, text.len()),
        };
        self.range = Some(((id, from), (id, to)));
    }

    /// The mouse moved over a text with the left button held. Returns whether the selection changed.
    pub fn drag_to(&mut self, id: u32, index: usize) -> bool {
        if !self.dragging {
            return false;
        }
        match &mut self.range {
            Some((_, head)) if *head != (id, index) => {
                *head = (id, index);
                true
            }
            _ => false,
        }
    }

    /// The left button went down somewhere in the view. A press that landed on no text clears the selection. Returns
    /// whether the selection changed. The order of this call and [`Self::press`] does not matter.
    pub fn press_elsewhere(&mut self) -> bool {
        if self.pressed_text || self.range.is_none() {
            return false;
        }
        self.range = None;
        true
    }

    /// The left button was released, anywhere.
    pub fn release(&mut self) {
        self.dragging = false;
        self.pressed_text = false;
    }

    pub fn select(&mut self, start: TextPosition, end: TextPosition) {
        self.range = Some((start, end));
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn is_dragging(&self) -> bool {
        self.dragging
    }

    /// Whether no text is selected.
    pub fn is_empty(&self) -> bool {
        self.range.is_none_or(|(anchor, head)| anchor == head)
    }

    /// The selection's start and end in reading order.
    pub fn ordered(&self) -> Option<(TextPosition, TextPosition)> {
        self.range
            .map(|(a, b)| if a <= b { (a, b) } else { (b, a) })
    }

    /// The selected bytes of a text of `len` bytes.
    pub fn bytes_in(&self, id: u32, len: usize) -> Option<Range<usize>> {
        let (start, end) = self.ordered()?;
        if id < start.0 || id > end.0 {
            return None;
        }
        let from = if id == start.0 { start.1 } else { 0 };
        let to = if id == end.0 { end.1 } else { len };
        (from < to).then_some(from..to)
    }
}

/// A tab drawn as spaces: its byte in the text, where it starts in the drawn line, and how many spaces it takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabStop {
    byte: usize,
    start: usize,
    columns: usize,
}

/// A line with its tabs replaced by spaces up to the next tab stop.
pub fn expand_tabs(line: &str) -> (String, Vec<TabStop>) {
    if !line.contains('\t') {
        return (line.to_string(), Vec::new());
    }
    let mut drawn = String::with_capacity(line.len() + TAB_COLUMNS);
    let mut stops = Vec::new();
    let mut column = 0;
    for (byte, ch) in line.char_indices() {
        if ch == '\t' {
            let columns = TAB_COLUMNS - column % TAB_COLUMNS;
            stops.push(TabStop {
                byte,
                start: drawn.len(),
                columns,
            });
            drawn.extend(std::iter::repeat_n(' ', columns));
            column += columns;
        } else {
            drawn.push(ch);
            column += 1;
        }
    }
    (drawn, stops)
}

/// Byte of the text for a byte of the drawn line.
pub fn text_index(stops: &[TabStop], drawn: usize) -> usize {
    let mut shift = 0;
    for stop in stops {
        if drawn <= stop.start {
            break;
        }
        if drawn < stop.start + stop.columns {
            return stop.byte;
        }
        shift += stop.columns - 1;
    }
    drawn - shift
}

/// Byte of the drawn line for a byte of the text.
pub fn drawn_index(stops: &[TabStop], index: usize) -> usize {
    let extra: usize = stops
        .iter()
        .take_while(|stop| stop.byte < index)
        .map(|stop| stop.columns - 1)
        .sum();
    index + extra
}

/// The parts of `range` in order, each with the span covering it, if any.
fn segments(range: Range<usize>, spans: &[TextSpan]) -> Vec<(Range<usize>, Option<&TextSpan>)> {
    let mut parts = Vec::new();
    let mut position = range.start;
    for span in spans {
        let start = span.range.start.max(position);
        let end = span.range.end.min(range.end);
        if start >= end {
            continue;
        }
        if start > position {
            parts.push((position..start, None));
        }
        parts.push((start..end, Some(span)));
        position = end;
    }
    if position < range.end {
        parts.push((position..range.end, None));
    }
    parts
}

/// The word, or run of other text, around a byte of `text`.
pub fn word_range(text: &str, index: usize) -> Range<usize> {
    text.split_word_bound_indices()
        .find(|(start, word)| index < start + word.len())
        .map_or(text.len()..text.len(), |(start, word)| {
            start..start + word.len()
        })
}

/// Shapes a text with the window's text style and the text's spans.
#[derive(Clone)]
struct RichShaper {
    font: Font,
    font_size: Pixels,
    line_height: Pixels,
    color: Hsla,
    text_system: Arc<WindowTextSystem>,
    spans: Rc<[TextSpan]>,
    preserve_lines: bool,
}

impl RichShaper {
    fn new(window: &Window, text: &RichText) -> Self {
        let style = window.text_style();
        Self {
            font: style.font(),
            font_size: style.font_size.to_pixels(window.rem_size()),
            line_height: window.line_height(),
            color: style.color,
            text_system: window.text_system().clone(),
            spans: text.spans.clone(),
            preserve_lines: text.preserve_lines,
        }
    }

    fn plain_run(&self, len: usize) -> TextRun {
        TextRun {
            len,
            font: self.font.clone(),
            color: self.color,
            background_color: None,
            underline: None,
            strikethrough: None,
        }
    }

    fn styled_run(&self, len: usize, span: &TextSpan) -> TextRun {
        let mut font = self.font.clone();
        if let Some(family) = &span.font_family {
            font.family = family.clone();
        }
        if span.bold {
            font.weight = FontWeight::BOLD;
        }
        if span.italic {
            font.style = FontStyle::Italic;
        }
        let color = span.color.unwrap_or(self.color);
        TextRun {
            len,
            font,
            color,
            background_color: span.background,
            underline: span.underline.then_some(UnderlineStyle {
                thickness: px(1.),
                color: Some(color),
                wavy: false,
            }),
            strikethrough: span.strikethrough.then_some(StrikethroughStyle {
                thickness: px(1.),
                color: Some(color),
            }),
        }
    }

    /// Runs of a line of the text.
    fn runs(&self, range: Range<usize>) -> Vec<TextRun> {
        let runs: Vec<TextRun> = segments(range, &self.spans)
            .into_iter()
            .map(|(part, span)| match span {
                Some(span) => self.styled_run(part.len(), span),
                None => self.plain_run(part.len()),
            })
            .collect();
        if runs.is_empty() {
            vec![self.plain_run(0)]
        } else {
            runs
        }
    }

    fn width(&self, text: &str, range: Range<usize>) -> Pixels {
        if range.is_empty() {
            return px(0.);
        }
        if self.preserve_lines {
            let (drawn, _) = expand_tabs(&text[range]);
            let run = self.plain_run(drawn.len());
            return self
                .text_system
                .layout_line(&drawn, self.font_size, &[run], None)
                .width;
        }
        let runs = self.runs(range.clone());
        self.text_system
            .layout_line(&text[range], self.font_size, &runs, None)
            .width
    }

    fn wrap(&self, text: &str, width: Option<Pixels>) -> Vec<Range<usize>> {
        let measure = |range: Range<usize>| self.width(text, range);
        if self.preserve_lines {
            wrap_preserving(text, width, measure)
        } else {
            wrap_words(text, width, measure)
        }
    }

    fn line(&self, text: &str, range: Range<usize>) -> RichLine {
        let (shaped, stops) = if self.preserve_lines {
            let (drawn, stops) = expand_tabs(&text[range.clone()]);
            let run = self.plain_run(drawn.len());
            let shaped = self
                .text_system
                .shape_line(drawn.into(), self.font_size, &[run], None);
            (shaped, stops)
        } else {
            // A tab inside a line of prose is one space wide, so bytes stay in step with the text.
            let drawn = text[range.clone()].replace('\t', " ");
            let runs = self.runs(range.clone());
            let shaped = self
                .text_system
                .shape_line(drawn.into(), self.font_size, &runs, None);
            (shaped, Vec::new())
        };
        RichLine {
            range,
            shaped,
            stops,
        }
    }
}

struct RichLine {
    /// Bytes of the text on this line.
    range: Range<usize>,
    shaped: ShapedLine,
    stops: Vec<TabStop>,
}

impl RichLine {
    fn x_for(&self, index: usize) -> Pixels {
        let local = index.saturating_sub(self.range.start);
        self.shaped.x_for_index(drawn_index(&self.stops, local))
    }

    fn index_for(&self, x: Pixels) -> usize {
        let local = text_index(&self.stops, self.shaped.closest_index_for_x(x));
        (self.range.start + local).min(self.range.end)
    }
}

/// Lines of a laid out text and where they stand, for painting and for finding the byte under the mouse.
pub struct RichTextLayout {
    lines: Rc<[RichLine]>,
    line_height: Pixels,
    hitbox: Option<Hitbox>,
}

/// How far a line starts from the element's left edge.
fn line_offset(align: TextAlign, available: Pixels, line: &RichLine) -> Pixels {
    let free = (available - line.shaped.width()).max(px(0.));
    match align {
        TextAlign::Left => px(0.),
        TextAlign::Center => free / 2.,
        TextAlign::Right => free,
    }
}

/// The byte of the text at a window position. A position beyond the text's lines takes the nearest line.
fn index_at(
    lines: &[RichLine],
    bounds: Bounds<Pixels>,
    line_height: Pixels,
    align: TextAlign,
    position: Point<Pixels>,
) -> usize {
    if lines.is_empty() {
        return 0;
    }
    let row = (f32::from(position.y - bounds.top()) / f32::from(line_height)).floor();
    let line = &lines[(row.max(0.) as usize).min(lines.len() - 1)];
    let x = position.x - bounds.left() - line_offset(align, bounds.size.width, line);
    line.index_for(x)
}

impl IntoElement for RichText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for RichText {
    type RequestLayoutState = ();
    type PrepaintState = RichTextLayout;

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
        let shaper = RichShaper::new(window, self);
        let text = self.text.clone();
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let layout_id =
            window.request_measured_layout(style, move |known, available, _window, _cx| {
                let width = definite_width(known, available);
                let lines = shaper.wrap(&text, width);
                let width = width.unwrap_or_else(|| {
                    lines
                        .iter()
                        .map(|range| shaper.width(&text, range.clone()))
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
        let shaper = RichShaper::new(window, self);
        let lines: Vec<RichLine> = shaper
            .wrap(&self.text, Some(bounds.size.width))
            .into_iter()
            .map(|range| shaper.line(&self.text, range))
            .collect();
        RichTextLayout {
            lines: lines.into(),
            line_height: shaper.line_height,
            hitbox: self
                .on_event
                .is_some()
                .then(|| window.insert_hitbox(bounds, HitboxBehavior::Normal)),
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
        let line_height = layout.line_height;
        for (ix, line) in layout.lines.iter().enumerate() {
            let origin = point(
                bounds.left() + line_offset(self.align, bounds.size.width, line),
                bounds.top() + line_height * ix as f32,
            );
            let _ = line.shaped.paint_background(
                origin,
                line_height,
                TextAlign::Left,
                None,
                window,
                cx,
            );
            if let Some(selection) = &self.selection {
                let start = selection.start.max(line.range.start);
                let end = selection.end.min(line.range.end);
                let runs_on = selection.end > line.range.end && selection.start <= line.range.end;
                if start < end || runs_on {
                    let (from, to) = (line.x_for(start), line.x_for(end));
                    let tail = if runs_on { SELECTION_TAIL } else { px(0.) };
                    window.paint_quad(fill(
                        Bounds::new(
                            point(origin.x + from, origin.y),
                            size(to - from + tail, line_height),
                        ),
                        theme::selection(),
                    ));
                }
            }
            let _ = line
                .shaped
                .paint(origin, line_height, TextAlign::Left, None, window, cx);
        }
        let (Some(listener), Some(hitbox)) = (self.on_event.clone(), layout.hitbox.clone()) else {
            return;
        };
        let cursor = if self.pointer {
            CursorStyle::PointingHand
        } else {
            CursorStyle::IBeam
        };
        window.set_cursor_style(cursor, &hitbox);
        let align = self.align;

        let (on_down, lines, area) = (listener.clone(), layout.lines.clone(), hitbox.clone());
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble
                && event.button == MouseButton::Left
                && area.is_hovered(window)
            {
                let index = index_at(&lines, bounds, line_height, align, event.position);
                on_down(
                    RichTextEvent::Down {
                        index,
                        click_count: event.click_count,
                    },
                    window,
                    cx,
                );
            }
        });

        let (on_move, lines, area) = (listener.clone(), layout.lines.clone(), hitbox.clone());
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && area.is_hovered(window) {
                let index = index_at(&lines, bounds, line_height, align, event.position);
                on_move(
                    RichTextEvent::Move {
                        index,
                        pressed: event.dragging(),
                    },
                    window,
                    cx,
                );
            }
        });

        let lines = layout.lines.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble
                && event.button == MouseButton::Left
                && hitbox.is_hovered(window)
            {
                let index = index_at(&lines, bounds, line_height, align, event.position);
                listener(RichTextEvent::Up { index }, window, cx);
            }
        });
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

    fn preserved(text: &str, width: f32) -> Vec<&str> {
        wrap_preserving(text, Some(px(width)), |range| chars(&text[range]))
            .into_iter()
            .map(|range| &text[range])
            .collect()
    }

    #[test]
    fn keeps_the_layout_of_preserved_lines() {
        assert_eq!(
            preserved("    fn a() {\n\n  b  \n}", 80.),
            ["    fn a() {", "", "  b  ", "}"]
        );
        // A line wider than the element continues on the next one, spaces included.
        assert_eq!(preserved("ab cd ef", 3.), ["ab ", "cd ", "ef"]);
        assert_eq!(preserved("", 3.), [""]);
        assert_eq!(preserved("a\n", 3.), ["a", ""]);
    }

    #[test]
    fn breaks_very_long_words_quickly() {
        let word = "가".repeat(20_000);
        let calls = std::cell::Cell::new(0);
        let ranges = wrap_words(&word, Some(px(100.)), |range| {
            calls.set(calls.get() + 1);
            chars(&word[range])
        });
        assert_eq!(ranges.len(), 200);
        assert!(ranges.iter().all(|range| range.len() == 300));
        assert!(calls.get() < 200 * 10, "{} measurements", calls.get());
    }

    #[test]
    fn expands_tabs_to_tab_stops() {
        let (drawn, stops) = expand_tabs("\tab\tc");
        assert_eq!(drawn, "    ab  c");
        assert_eq!(text_index(&stops, 0), 0);
        // Inside a tab's spaces is the tab.
        assert_eq!(text_index(&stops, 2), 0);
        assert_eq!(text_index(&stops, 4), 1);
        assert_eq!(text_index(&stops, 6), 3);
        assert_eq!(text_index(&stops, 7), 3);
        assert_eq!(text_index(&stops, 8), 4);
        assert_eq!(drawn_index(&stops, 0), 0);
        assert_eq!(drawn_index(&stops, 1), 4);
        assert_eq!(drawn_index(&stops, 3), 6);
        assert_eq!(drawn_index(&stops, 4), 8);
        assert_eq!(expand_tabs("없음"), ("없음".to_string(), Vec::new()));
    }

    #[test]
    fn splits_lines_into_styled_and_plain_parts() {
        let span = |range: Range<usize>| TextSpan {
            range,
            bold: true,
            ..TextSpan::default()
        };
        let spans = [span(2..4), span(6..8)];
        let parts: Vec<_> = segments(0..10, &spans)
            .into_iter()
            .map(|(range, span)| (range, span.is_some()))
            .collect();
        assert_eq!(
            parts,
            [
                (0..2, false),
                (2..4, true),
                (4..6, false),
                (6..8, true),
                (8..10, false)
            ]
        );
        // A line that starts or ends inside a span takes the part on the line.
        let parts: Vec<_> = segments(3..7, &spans)
            .into_iter()
            .map(|(range, span)| (range, span.is_some()))
            .collect();
        assert_eq!(parts, [(3..4, true), (4..6, false), (6..7, true)]);
        assert!(segments(5..5, &spans).is_empty());
    }

    #[test]
    fn selects_across_texts_in_reading_order() {
        let mut selection = TextSelection::default();
        assert!(selection.is_empty());
        assert_eq!(selection.bytes_in(0, 5), None);

        selection.press(2, "abcde", 3, 1);
        assert!(selection.is_empty());
        assert_eq!(selection.bytes_in(2, 5), None);
        assert!(selection.drag_to(4, 1));
        assert!(!selection.drag_to(4, 1));
        assert!(!selection.is_empty());
        assert_eq!(selection.ordered(), Some(((2, 3), (4, 1))));
        assert_eq!(selection.bytes_in(1, 5), None);
        assert_eq!(selection.bytes_in(2, 5), Some(3..5));
        assert_eq!(selection.bytes_in(3, 7), Some(0..7));
        assert_eq!(selection.bytes_in(4, 5), Some(0..1));
        assert_eq!(selection.bytes_in(5, 5), None);

        // Dragging back past the start reverses the order.
        assert!(selection.drag_to(1, 2));
        assert_eq!(selection.ordered(), Some(((1, 2), (2, 3))));

        selection.release();
        assert!(!selection.drag_to(9, 0));
    }

    #[test]
    fn selects_words_and_whole_texts_by_click_count() {
        let mut selection = TextSelection::default();
        selection.press(0, "운행 기록", 8, 2);
        assert_eq!(selection.bytes_in(0, 13), Some(7..13));
        selection.release();
        selection.press(0, "운행 기록", 8, 3);
        assert_eq!(selection.bytes_in(0, 13), Some(0..13));
    }

    #[test]
    fn a_press_elsewhere_clears_only_when_no_text_was_pressed() {
        let mut selection = TextSelection::default();
        selection.select((0, 0), (1, 1));
        // A press on empty space.
        assert!(selection.press_elsewhere());
        assert!(selection.is_empty());
        assert!(!selection.press_elsewhere());

        // A press on text, with the view's own handler running after the text's.
        selection.press(1, "abc", 1, 1);
        assert!(selection.drag_to(1, 2));
        assert!(!selection.press_elsewhere());
        assert_eq!(selection.bytes_in(1, 3), Some(1..2));
        selection.release();
        assert!(selection.press_elsewhere());
    }

    #[test]
    fn finds_the_word_around_a_byte() {
        let text = "운행 기록, abc";
        assert_eq!(&text[word_range(text, 0)], "운행");
        assert_eq!(&text[word_range(text, 3)], "운행");
        assert_eq!(&text[word_range(text, 6)], " ");
        assert_eq!(&text[word_range(text, 7)], "기록");
        assert_eq!(&text[word_range(text, text.len() - 1)], "abc");
        assert_eq!(word_range(text, text.len()), text.len()..text.len());
    }
}
