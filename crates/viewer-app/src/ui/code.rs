//! Code reading view. Lines have one height and do not wrap, so a file of any length draws only the lines on screen.
//!
//! The text shows at once. Colors come from the code core for the lines around the screen, and whatever has no colors yet,
//! or never gets any, stays plain text.

use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    App, Axis, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, DispatchPhase, Element,
    ElementId, Entity, EventEmitter, FocusHandle, Font, GlobalElementId, Hitbox, HitboxBehavior,
    Hsla, InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, ScrollEvent, ShapedLine, Size, Style, TextAlign, TextRun, Window,
    div, fill, point, prelude::*, px, relative, size,
};
use viewer_core::CodeDocument;
use viewer_ui::theme;
use viewer_ui::wrap::{TabStop, TextSelection, drawn_index, expand_tabs, text_index};

use super::doc_scrollbar::{ScrollMetrics, ScrollbarDrag, scrollbar};
use super::grid::{
    MoveDown, MoveLeft, MoveRight, MoveToFirstRow, MoveToLastRow, MoveUp, PageDown, PageUp,
};
use super::markdown::SelectAll;
use super::style;
use super::value::CopyValue;

/// Lines around the screen whose tokens are asked for at once.
const TOKEN_MARGIN_LINES: usize = 400;
/// Longest part of a line that is drawn. Laying out text costs by length, and a longer line is a data file or minified
/// code that nobody reads by eye. Copying still gives the whole line.
const MAX_DRAWN_LINE_BYTES: usize = 8 * 1024;
/// Cells for the note after a cut line.
const CUT_NOTE_CELLS: usize = 32;
/// Columns one press of the left or right key moves.
const SCROLL_COLUMNS: f64 = 4.;
/// Share of a screen one Page Up or Page Down moves.
const PAGE_SHARE: f64 = 0.9;
/// Share of the distance past the edge scrolled per mouse move while dragging a selection beyond the view.
const DRAG_SCROLL_SHARE: f64 = 0.5;

pub enum CodeEvent {
    /// The view needs the tokens touching these bytes.
    NeedTokens(Range<usize>),
}

/// Scroll position kept while another tab is shown.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CodeScroll {
    pub x: f64,
    pub y: f64,
}

/// Tokens for a stretch of the file, as parallel arrays sorted by position.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TokenWindow {
    /// The bytes these tokens cover.
    range: Range<usize>,
    starts: Vec<u32>,
    /// Tokens do not overlap, so their ends are in order too.
    ends: Vec<u32>,
    kinds: Vec<u8>,
}

impl TokenWindow {
    /// Reads a flat list of start, length and kind.
    fn new(range: Range<usize>, flat: &[u32]) -> Self {
        let mut window = Self {
            range,
            starts: Vec::with_capacity(flat.len() / 3),
            ends: Vec::with_capacity(flat.len() / 3),
            kinds: Vec::with_capacity(flat.len() / 3),
        };
        for [start, len, kind] in flat.as_chunks::<3>().0 {
            window.starts.push(*start);
            window.ends.push(start.saturating_add(*len));
            window.kinds.push(*kind as u8);
        }
        window
    }

    /// The parts of tokens within `bytes`, in order, as ranges relative to `bytes.start`. A token that does not fit the
    /// text, as after a file changed unnoticed, is left out.
    fn spans_in(&self, bytes: Range<usize>, text: &str) -> Vec<(Range<usize>, u8)> {
        let first = self
            .ends
            .partition_point(|end| *end as usize <= bytes.start);
        let mut spans = Vec::new();
        let mut position = bytes.start;
        for ix in first..self.starts.len() {
            let start = self.starts[ix] as usize;
            if start >= bytes.end {
                break;
            }
            let from = start.max(position);
            let to = (self.ends[ix] as usize).min(bytes.end);
            if from >= to || !text.is_char_boundary(from) || !text.is_char_boundary(to) {
                continue;
            }
            spans.push((from - bytes.start..to - bytes.start, self.kinds[ix]));
            position = to;
        }
        spans
    }
}

/// The colored runs of a drawn line. Together they cover its whole text.
fn line_runs(
    drawn: &DrawnText,
    spans: &[(Range<usize>, u8)],
    font: &Font,
    plain: Hsla,
) -> Vec<TextRun> {
    let run = |len: usize, color: Hsla| TextRun {
        len,
        font: font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let mut runs = Vec::new();
    let mut position = 0;
    for (range, kind) in spans {
        let start = drawn_index(&drawn.stops, range.start);
        let end = drawn_index(&drawn.stops, range.end);
        if start >= end || start < position {
            continue;
        }
        if start > position {
            runs.push(run(start - position, plain));
        }
        runs.push(run(end - start, style::token_color(*kind).into()));
        position = end;
    }
    let total = drawn.text.len();
    if drawn.prefix_end < total {
        if drawn.prefix_end > position {
            runs.push(run(drawn.prefix_end - position, plain));
        }
        runs.push(run(total - drawn.prefix_end, theme::text_muted().into()));
    } else if total > position {
        runs.push(run(total - position, plain));
    }
    if runs.is_empty() {
        runs.push(run(0, plain));
    }
    runs
}

/// What is drawn for a line: tabs as spaces, cut at the limit with a note of what is left.
struct DrawnText {
    text: String,
    stops: Vec<TabStop>,
    /// Bytes of `text` that come from the line, before the note.
    prefix_end: usize,
    /// Bytes of the line that are drawn.
    shown: usize,
}

fn drawn_text(line: &str) -> DrawnText {
    let mut shown = line.len().min(MAX_DRAWN_LINE_BYTES);
    while !line.is_char_boundary(shown) {
        shown -= 1;
    }
    let (mut text, stops) = expand_tabs(&line[..shown]);
    let prefix_end = text.len();
    if shown < line.len() {
        text.push_str(&format!(" … {} more bytes", line.len() - shown));
    }
    DrawnText {
        text,
        stops,
        prefix_end,
        shown,
    }
}

/// Geometry of the last layout, shared with the scrollbars.
#[derive(Clone, Copy, Debug, Default)]
struct Viewport {
    size: Size<Pixels>,
    gutter: Pixels,
    cell: Pixels,
}

pub struct CodeView {
    focus_handle: FocusHandle,
    doc: Arc<CodeDocument>,
    scroll: CodeScroll,
    selection: TextSelection,
    /// The latest answer for this version of the file.
    tokens: Option<TokenWindow>,
    /// Bytes asked for and not yet answered.
    requested: Option<Range<usize>>,
    /// No tokens can come for this version of the file, so the view stops asking.
    no_tokens: bool,
    language: Option<String>,
    viewport: Rc<Cell<Viewport>>,
    vertical_bar: ScrollbarDrag,
    horizontal_bar: ScrollbarDrag,
}

impl EventEmitter<CodeEvent> for CodeView {}

impl CodeView {
    pub fn new(doc: Arc<CodeDocument>, scroll: CodeScroll, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            doc,
            scroll,
            selection: TextSelection::default(),
            tokens: None,
            requested: None,
            no_tokens: false,
            language: None,
            viewport: Rc::default(),
            vertical_bar: ScrollbarDrag::default(),
            horizontal_bar: ScrollbarDrag::default(),
        }
    }

    /// Shows a new version of the file at the same place. The selection and tokens belonged to the old text.
    pub fn set_document(&mut self, doc: Arc<CodeDocument>, cx: &mut Context<Self>) {
        self.doc = doc;
        self.selection.clear();
        self.tokens = None;
        self.requested = None;
        self.no_tokens = false;
        cx.notify();
    }

    pub fn scroll(&self) -> CodeScroll {
        self.scroll
    }

    pub fn line_count(&self) -> usize {
        self.doc.line_count()
    }

    /// The language the core named for this file.
    pub fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }

    /// Takes the answer to the latest request.
    pub fn set_tokens(&mut self, flat: &[u32], language: String, cx: &mut Context<Self>) {
        if let Some(range) = self.requested.take() {
            self.tokens = Some(TokenWindow::new(range, flat));
        }
        self.language = (!language.is_empty()).then_some(language);
        // A file without rules has nothing more to ask for.
        self.no_tokens = self.language.is_none();
        cx.notify();
    }

    /// Stops asking until the file is read again: the core could not answer for this version.
    pub fn tokens_unavailable(&mut self, cx: &mut Context<Self>) {
        self.requested = None;
        self.no_tokens = true;
        cx.notify();
    }

    /// Asks again, after the core restarted.
    pub fn retry_tokens(&mut self, cx: &mut Context<Self>) {
        self.requested = None;
        self.no_tokens = false;
        cx.notify();
    }

    /// Asks for the tokens around the lines on screen unless they are here or on their way.
    fn ensure_tokens(&mut self, lines: Range<usize>, cx: &mut Context<Self>) {
        if self.no_tokens {
            return;
        }
        let needed = self.doc.offset(lines.start, 0)..self.doc.line_range(lines.end - 1).end;
        let covered = |range: &Range<usize>| range.start <= needed.start && needed.end <= range.end;
        if self
            .tokens
            .as_ref()
            .is_some_and(|tokens| covered(&tokens.range))
            || self.requested.as_ref().is_some_and(&covered)
        {
            return;
        }
        let from = lines.start.saturating_sub(TOKEN_MARGIN_LINES);
        let to = (lines.end + TOKEN_MARGIN_LINES).min(self.doc.line_count());
        let range = self.doc.offset(from, 0)..self.doc.line_range(to - 1).end;
        self.requested = Some(range.clone());
        cx.emit(CodeEvent::NeedTokens(range));
    }

    fn line_height() -> f64 {
        f64::from(f32::from(style::CODE_LINE_HEIGHT))
    }

    fn max_scroll(&self) -> (f64, f64) {
        let viewport = self.viewport.get();
        let bar = f64::from(f32::from(style::GRID_SCROLLBAR_SIZE));
        let widest = self
            .doc
            .widest_line()
            .min(MAX_DRAWN_LINE_BYTES + CUT_NOTE_CELLS);
        let content_x = widest as f64 * f64::from(f32::from(viewport.cell));
        let view_x = f64::from(f32::from(viewport.size.width - viewport.gutter));
        let content_y = self.doc.line_count() as f64 * Self::line_height();
        let view_y = f64::from(f32::from(viewport.size.height));
        (
            (content_x + bar * 2. - view_x).max(0.),
            (content_y + bar - view_y).max(0.),
        )
    }

    fn set_scroll(&mut self, x: f64, y: f64, cx: &mut Context<Self>) {
        let (max_x, max_y) = self.max_scroll();
        let scroll = CodeScroll {
            x: x.clamp(0., max_x),
            y: y.clamp(0., max_y),
        };
        if scroll != self.scroll {
            self.scroll = scroll;
            cx.notify();
        }
    }

    fn scroll_by(&mut self, dx: f64, dy: f64, cx: &mut Context<Self>) {
        self.set_scroll(self.scroll.x + dx, self.scroll.y + dy, cx);
    }

    fn page(&self) -> f64 {
        f64::from(f32::from(self.viewport.get().size.height)) * PAGE_SHARE
    }

    fn mouse_down(&mut self, line: usize, index: usize, clicks: usize, cx: &mut Context<Self>) {
        let doc = self.doc.clone();
        self.selection
            .press(line as u32, doc.line(line), index, clicks);
        cx.notify();
    }

    fn mouse_drag(
        &mut self,
        line: usize,
        index: usize,
        past_edge: (f64, f64),
        cx: &mut Context<Self>,
    ) {
        if !self.selection.is_dragging() {
            return;
        }
        if self.selection.drag_to(line as u32, index) {
            cx.notify();
        }
        self.scroll_by(
            past_edge.0 * DRAG_SCROLL_SHARE,
            past_edge.1 * DRAG_SCROLL_SHARE,
            cx,
        );
    }

    /// The selection as a range of the file's bytes.
    fn selected_bytes(&self) -> Option<Range<usize>> {
        let (start, end) = self.selection.ordered()?;
        let (from, to) = (
            self.doc.offset(start.0 as usize, start.1),
            self.doc.offset(end.0 as usize, end.1),
        );
        (from < to).then_some(from..to)
    }

    fn copy(&mut self, _: &CopyValue, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(bytes) = self.selected_bytes() {
            let text = self.doc.text()[bytes].to_string();
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn select_all(&mut self, _: &SelectAll, _window: &mut Window, cx: &mut Context<Self>) {
        let last = self.doc.line_count() - 1;
        self.selection
            .select((0, 0), (last as u32, self.doc.line(last).len()));
        cx.notify();
    }
}

impl Render for CodeView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let bar = |axis: Axis, drag: ScrollbarDrag| {
            let (read, write) = (view.clone(), view.clone());
            scrollbar(
                axis,
                drag,
                {
                    let viewport = self.viewport.clone();
                    move |cx: &App| {
                        let this = read.read(cx);
                        let (max_x, max_y) = this.max_scroll();
                        let now = viewport.get();
                        match axis {
                            Axis::Vertical => ScrollMetrics {
                                offset: px(this.scroll.y as f32),
                                max: px(max_y as f32),
                                viewport: now.size.height,
                            },
                            Axis::Horizontal => ScrollMetrics {
                                offset: px(this.scroll.x as f32),
                                max: px(max_x as f32),
                                viewport: now.size.width - now.gutter,
                            },
                        }
                    }
                },
                move |offset, _window, cx| {
                    let offset = f64::from(f32::from(offset));
                    write.update(cx, |this, cx| match axis {
                        Axis::Vertical => this.set_scroll(this.scroll.x, offset, cx),
                        Axis::Horizontal => this.set_scroll(offset, this.scroll.y, cx),
                    });
                },
            )
        };
        div()
            .size_full()
            .relative()
            .track_focus(&self.focus_handle)
            .key_context("Document")
            .font_family(theme::mono_font())
            .text_size(style::DOC_CODE_SIZE)
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::select_all))
            .on_action(
                cx.listener(|this, _: &MoveUp, _, cx| this.scroll_by(0., -Self::line_height(), cx)),
            )
            .on_action(
                cx.listener(|this, _: &MoveDown, _, cx| {
                    this.scroll_by(0., Self::line_height(), cx)
                }),
            )
            .on_action(cx.listener(|this, _: &MoveLeft, _, cx| {
                let step = SCROLL_COLUMNS * f64::from(f32::from(this.viewport.get().cell));
                this.scroll_by(-step, 0., cx)
            }))
            .on_action(cx.listener(|this, _: &MoveRight, _, cx| {
                let step = SCROLL_COLUMNS * f64::from(f32::from(this.viewport.get().cell));
                this.scroll_by(step, 0., cx)
            }))
            .on_action(cx.listener(|this, _: &PageUp, _, cx| {
                let page = this.page();
                this.scroll_by(0., -page, cx)
            }))
            .on_action(cx.listener(|this, _: &PageDown, _, cx| {
                let page = this.page();
                this.scroll_by(0., page, cx)
            }))
            .on_action(cx.listener(|this, _: &MoveToFirstRow, _, cx| this.set_scroll(0., 0., cx)))
            .on_action(cx.listener(|this, _: &MoveToLastRow, _, cx| {
                this.set_scroll(this.scroll.x, f64::MAX, cx)
            }))
            .child(CodeElement { view: view.clone() })
            .child(bar(Axis::Vertical, self.vertical_bar.clone()))
            .child(bar(Axis::Horizontal, self.horizontal_bar.clone()))
    }
}

/// One line on screen.
struct LineLayout {
    line: usize,
    shaped: ShapedLine,
    number: ShapedLine,
    stops: Vec<TabStop>,
    /// Bytes of the line that are drawn, and the drawn bytes they take.
    shown: usize,
    prefix_end: usize,
}

impl LineLayout {
    /// The byte of the line at a horizontal position within the text area.
    fn index_for(&self, x: Pixels, line_len: usize) -> usize {
        let drawn = self.shaped.closest_index_for_x(x);
        if drawn >= self.prefix_end {
            // Past the drawn part, a position means the end of the line, so copying takes all of it.
            return if self.shown < line_len && drawn > self.prefix_end {
                line_len
            } else {
                self.shown
            };
        }
        text_index(&self.stops, drawn).min(self.shown)
    }
}

struct CodeLayout {
    hitbox: Hitbox,
    lines: Rc<Vec<LineLayout>>,
    line_height: Pixels,
    gutter: Pixels,
    /// Top of the first laid out line.
    top: Pixels,
    text_left: Pixels,
    scroll: CodeScroll,
}

struct CodeElement {
    view: Entity<CodeView>,
}

impl IntoElement for CodeElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// Where the lines on screen are drawn, for turning window positions into text positions.
#[derive(Clone, Copy)]
struct LinesGeometry {
    top: Pixels,
    line_height: Pixels,
    text_left: Pixels,
    scroll_x: f64,
}

impl LinesGeometry {
    /// The line and byte at a window position. A position beyond the text takes the nearest line and column.
    fn position_in(
        &self,
        lines: &[LineLayout],
        doc: &CodeDocument,
        position: Point<Pixels>,
    ) -> Option<(usize, usize)> {
        let row = f32::from(position.y - self.top) / f32::from(self.line_height);
        let row = (row.max(0.) as usize).min(lines.len().checked_sub(1)?);
        let line = &lines[row];
        let x = f64::from(f32::from((position.x - self.text_left).max(px(0.)))) + self.scroll_x;
        Some((
            line.line,
            line.index_for(px(x as f32), doc.line(line.line).len()),
        ))
    }
}

impl Element for CodeElement {
    type RequestLayoutState = ();
    type PrepaintState = CodeLayout;

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
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let text_style = window.text_style();
        let font = text_style.font();
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let plain: Hsla = theme::text().into();
        let run = |len, color| TextRun {
            len,
            font: font.clone(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let system = window.text_system().clone();
        let cell = system
            .shape_line("0".into(), font_size, &[run(1, plain)], None)
            .width();
        let line_height = style::CODE_LINE_HEIGHT;

        // Measure first, since the scroll limits depend on the size.
        let line_count = self.view.read(cx).doc.line_count();
        let digits = line_count.to_string().len().max(3);
        let gutter = cell * digits as f32 + style::CODE_GUTTER_PADDING * 2.;
        self.view.update(cx, |view, _| {
            view.viewport.set(Viewport {
                size: bounds.size,
                gutter,
                cell,
            });
            let (max_x, max_y) = view.max_scroll();
            view.scroll.x = view.scroll.x.clamp(0., max_x);
            view.scroll.y = view.scroll.y.clamp(0., max_y);
        });

        let scroll = self.view.read(cx).scroll;
        let height = CodeView::line_height();
        let first = (scroll.y / height).floor() as usize;
        let visible = (f64::from(f32::from(bounds.size.height)) / height).ceil() as usize + 1;
        let end = (first + visible).min(line_count);
        let first = first.min(end.saturating_sub(1));
        self.view
            .update(cx, |view, cx| view.ensure_tokens(first..end, cx));

        let (doc, muted) = (self.view.read(cx).doc.clone(), theme::text_muted().into());
        let tokens = &self.view.read(cx).tokens;
        let lines: Vec<LineLayout> = (first..end)
            .map(|line| {
                let range = doc.line_range(line);
                let drawn = drawn_text(&doc.text()[range.clone()]);
                let bytes = range.start..range.start + drawn.shown;
                let spans = tokens
                    .as_ref()
                    .map_or_else(Vec::new, |tokens| tokens.spans_in(bytes, doc.text()));
                let runs = line_runs(&drawn, &spans, &font, plain);
                let label = (line + 1).to_string();
                let number = system.shape_line(
                    label.clone().into(),
                    font_size,
                    &[run(label.len(), muted)],
                    None,
                );
                LineLayout {
                    line,
                    shaped: system.shape_line(drawn.text.into(), font_size, &runs, None),
                    number,
                    stops: drawn.stops,
                    shown: drawn.shown,
                    prefix_end: drawn.prefix_end,
                }
            })
            .collect();
        CodeLayout {
            hitbox: window.insert_hitbox(bounds, HitboxBehavior::Normal),
            lines: Rc::new(lines),
            line_height,
            gutter,
            top: bounds.top() - px((scroll.y - first as f64 * height) as f32),
            text_left: bounds.left() + gutter,
            scroll,
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
        let text_area = Bounds::from_corners(
            point(layout.text_left, bounds.top()),
            point(bounds.right(), bounds.bottom()),
        );
        let scroll_x = px(layout.scroll.x as f32);
        let selection = self.view.read(cx).selection;
        let doc = self.view.read(cx).doc.clone();
        window.with_content_mask(Some(ContentMask { bounds: text_area }), |window| {
            for (row, line) in layout.lines.iter().enumerate() {
                let origin = point(
                    layout.text_left - scroll_x,
                    layout.top + line_height * row as f32,
                );
                let line_len = doc.line(line.line).len();
                if let Some(range) = selection.bytes_in(line.line as u32, line_len) {
                    let start = drawn_index(&line.stops, range.start.min(line.shown));
                    let end = drawn_index(&line.stops, range.end.min(line.shown));
                    let (from, to) = (line.shaped.x_for_index(start), line.shaped.x_for_index(end));
                    window.paint_quad(fill(
                        Bounds::new(
                            point(origin.x + from, origin.y),
                            size(to - from, line_height),
                        ),
                        theme::selection(),
                    ));
                }
                let _ = line
                    .shaped
                    .paint(origin, line_height, TextAlign::Left, None, window, cx);
            }
        });
        let gutter_right = bounds.left() + layout.gutter - style::CODE_GUTTER_PADDING;
        let gutter_area = Bounds::new(bounds.origin, size(layout.gutter, bounds.size.height));
        window.with_content_mask(
            Some(ContentMask {
                bounds: gutter_area,
            }),
            |window| {
                for (row, line) in layout.lines.iter().enumerate() {
                    let origin = point(
                        gutter_right - line.number.width(),
                        layout.top + line_height * row as f32,
                    );
                    let _ =
                        line.number
                            .paint(origin, line_height, TextAlign::Left, None, window, cx);
                }
            },
        );
        self.register_mouse(layout, window, cx);
    }
}

/// How far a coordinate lies beyond a span, negative before its start.
fn past(at: Pixels, start: Pixels, end: Pixels) -> f64 {
    if at < start {
        f64::from(f32::from(at - start))
    } else if at > end {
        f64::from(f32::from(at - end))
    } else {
        0.
    }
}

impl CodeElement {
    fn register_mouse(&self, layout: &CodeLayout, window: &mut Window, cx: &App) {
        window.set_cursor_style(CursorStyle::IBeam, &layout.hitbox);
        let geometry = LinesGeometry {
            top: layout.top,
            line_height: layout.line_height,
            text_left: layout.text_left,
            scroll_x: layout.scroll.x,
        };
        let doc = self.view.read(cx).doc.clone();

        let (view, hitbox) = (self.view.clone(), layout.hitbox.clone());
        window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || !hitbox.should_handle_scroll(window) {
                return;
            }
            let delta = event.delta.pixel_delta(style::CODE_LINE_HEIGHT);
            let (mut dx, mut dy) = (f64::from(f32::from(delta.x)), f64::from(f32::from(delta.y)));
            if event.modifiers.shift && dx == 0. {
                (dx, dy) = (dy, 0.);
            }
            view.update(cx, |view, cx| view.scroll_by(-dx, -dy, cx));
            cx.stop_propagation();
        });

        let (view, hitbox) = (self.view.clone(), layout.hitbox.clone());
        let (lines, down_doc) = (layout.lines.clone(), doc.clone());
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble
                || event.button != MouseButton::Left
                || !hitbox.is_hovered(window)
            {
                return;
            }
            let at = geometry.position_in(&lines, &down_doc, event.position);
            view.update(cx, |view, cx| {
                view.focus_handle.focus(window, cx);
                if let Some((line, index)) = at {
                    view.mouse_down(line, index, event.click_count, cx);
                }
            });
        });

        let view = self.view.clone();
        let lines = layout.lines.clone();
        let (top, bottom) = (layout.hitbox.top(), layout.hitbox.bottom());
        let (left, right) = (layout.text_left, layout.hitbox.right());
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _window, cx| {
            if phase != DispatchPhase::Bubble || !event.dragging() {
                return;
            }
            let past_edge = (
                past(event.position.x, left, right),
                past(event.position.y, top, bottom),
            );
            if let Some((line, index)) = geometry.position_in(&lines, &doc, event.position) {
                view.update(cx, |view, cx| view.mouse_drag(line, index, past_edge, cx));
            }
        });

        let view = self.view.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, _window, cx| {
            if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                view.update(cx, |view, _| view.selection.release());
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_token_parts_inside_a_line() {
        let text = "가 = 'a\nb' + 1";
        let window = TokenWindow::new(0..text.len(), &[6, 5, 2, 14, 1, 3]);
        // The string starts on the first line.
        assert_eq!(window.spans_in(0..8, text), [(6..8, 2)]);
        // It ends on the second, and a number follows.
        assert_eq!(window.spans_in(9..15, text), [(0..2, 2), (5..6, 3)]);
    }

    #[test]
    fn leaves_out_tokens_that_do_not_fit_the_text() {
        let text = "가나다";
        // A token ending inside a character, as from a file changed unnoticed.
        let window = TokenWindow::new(0..9, &[1, 3, 4, 6, 3, 2]);
        assert_eq!(window.spans_in(0..9, text), [(6..9, 2)]);
        // Tokens past the end of the line are not its business.
        let window = TokenWindow::new(0..100, &[50, 3, 4]);
        assert!(window.spans_in(0..9, text).is_empty());
    }

    #[test]
    fn cuts_very_long_lines_and_keeps_tabs_in_step() {
        let long = "가".repeat(MAX_DRAWN_LINE_BYTES);
        let drawn = drawn_text(&long);
        assert!(drawn.shown <= MAX_DRAWN_LINE_BYTES);
        assert!(long.is_char_boundary(drawn.shown));
        assert!(drawn.text.ends_with("more bytes"));
        assert_eq!(drawn.prefix_end, drawn.shown);

        let tabbed = drawn_text("\ta");
        assert_eq!(tabbed.text, "    a");
        assert_eq!(tabbed.prefix_end, 5);
        assert_eq!(tabbed.shown, 2);
    }

    #[test]
    fn splits_a_line_into_runs_that_add_up() {
        let font = Font::default();
        let plain: Hsla = theme::text().into();
        for (line, spans) in [
            ("\tdef f", vec![(1..4, 4u8), (5..6, 7)]),
            ("plain", vec![]),
            ("", vec![]),
            ("x\ty", vec![(1..2, 1)]),
        ] {
            let drawn = drawn_text(line);
            let runs = line_runs(&drawn, &spans, &font, plain);
            let total: usize = runs.iter().map(|run| run.len).sum();
            assert_eq!(total, drawn.text.len(), "{line:?}");
        }
        let long = "a".repeat(MAX_DRAWN_LINE_BYTES + 10);
        let drawn = drawn_text(&long);
        let runs = line_runs(&drawn, &[(0..3, 4)], &font, plain);
        assert_eq!(
            runs.iter().map(|run| run.len).sum::<usize>(),
            drawn.text.len()
        );
    }
}
