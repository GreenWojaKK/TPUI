//! Markdown reading view. Draws only the blocks on screen, in a centered column of a comfortable width.
//!
//! Text wraps at spaces, so Korean words stay whole. Text can be selected with the mouse and copied. Links to the web
//! open in the browser; nothing else in a document acts when clicked.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{
    AnyElement, ClipboardItem, Context, DispatchPhase, Div, Entity, FocusHandle, FontWeight, Hsla,
    ListAlignment, ListOffset, ListState, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ObjectFit, Pixels, RetainAllImageCache, SharedString, StyledImage, TextAlign,
    Window, actions, canvas, div, img, list, point, prelude::*, px, relative,
};
use viewer_core::{
    MarkdownDocument, MdAlign, MdBlock, MdBlockKind, MdMarker, MdSourceKind, MdText,
};
use viewer_ui::theme;
use viewer_ui::wrap::{RichText, RichTextEvent, TextSelection, TextSpan};

use super::doc_scrollbar::{ScrollMetrics, ScrollbarDrag, scrollbar};
use super::grid::{MoveDown, MoveToFirstRow, MoveToLastRow, MoveUp, PageDown, PageUp};
use super::style;
use super::value::CopyValue;

actions!(document, [SelectAll]);

/// Blocks measured beyond the screen, so that scrolling does not show them appear.
const OVERDRAW: Pixels = px(600.);
/// Share of a screen one Page Up or Page Down moves.
const PAGE_SHARE: f32 = 0.9;
/// Share of the distance past the edge scrolled per mouse move while dragging a selection beyond the view.
const DRAG_SCROLL_SHARE: f32 = 0.5;

pub struct MarkdownView {
    focus_handle: FocusHandle,
    doc: Arc<MarkdownDocument>,
    /// Folder of the file. Image paths are relative to it.
    base_dir: PathBuf,
    list: ListState,
    selection: TextSelection,
    /// The link under the last press, opened if the button is released on it without selecting.
    pressed_link: Option<Arc<str>>,
    /// Text whose link is under the mouse.
    hovered_link: Option<u32>,
    /// Images of this document. Dropped with the view.
    images: Entity<RetainAllImageCache>,
    scrollbar: ScrollbarDrag,
}

impl MarkdownView {
    pub fn new(
        doc: Arc<MarkdownDocument>,
        path: &Path,
        top: Option<ListOffset>,
        cx: &mut Context<Self>,
    ) -> Self {
        let list = ListState::new(doc.blocks.len(), ListAlignment::Top, OVERDRAW)
            .with_uniform_item_height(style::DOC_BLOCK_HEIGHT_HINT);
        if let Some(top) = top {
            list.scroll_to(top);
        }
        Self {
            focus_handle: cx.focus_handle(),
            doc,
            base_dir: path.parent().map(Path::to_path_buf).unwrap_or_default(),
            list,
            selection: TextSelection::default(),
            pressed_link: None,
            hovered_link: None,
            images: RetainAllImageCache::new(cx),
            scrollbar: ScrollbarDrag::default(),
        }
    }

    /// Shows a new version of the file at the same place. The selection pointed into the old text, so it is dropped.
    pub fn set_document(&mut self, doc: Arc<MarkdownDocument>, cx: &mut Context<Self>) {
        let top = self.list.logical_scroll_top();
        self.list
            .reset_with_uniform_height(doc.blocks.len(), style::DOC_BLOCK_HEIGHT_HINT);
        self.list.scroll_to(top);
        self.doc = doc;
        self.selection.clear();
        self.pressed_link = None;
        self.hovered_link = None;
        cx.notify();
    }

    pub fn scroll_top(&self) -> ListOffset {
        self.list.logical_scroll_top()
    }

    pub fn block_count(&self) -> usize {
        self.doc.blocks.len()
    }

    fn text_event(
        &mut self,
        text: &MdText,
        event: RichTextEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let opens = |index: usize| {
            text.link_at(index)
                .map(|(link, _)| link.clone())
                .filter(|link| is_web_link(link))
        };
        match event {
            RichTextEvent::Down { index, click_count } => {
                self.focus_handle.focus(window, cx);
                self.pressed_link = opens(index);
                self.selection
                    .press(text.id, &text.text, index, click_count);
                cx.notify();
            }
            RichTextEvent::Move {
                index,
                pressed: true,
            } => {
                if self.selection.drag_to(text.id, index) {
                    self.pressed_link = None;
                    cx.notify();
                }
            }
            RichTextEvent::Move {
                index,
                pressed: false,
            } => {
                let hovered = opens(index).map(|_| text.id);
                if self.hovered_link != hovered {
                    self.hovered_link = hovered;
                    cx.notify();
                }
            }
            RichTextEvent::Up { index } => {
                if let Some(link) = self.pressed_link.take()
                    && opens(index).as_deref() == Some(&*link)
                {
                    cx.open_url(&link);
                }
            }
        }
    }

    /// While a selection is dragged past the view's top or bottom, scrolls towards the mouse.
    fn drag_scroll(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if !self.selection.is_dragging() || !event.dragging() {
            return;
        }
        let bounds = self.list.viewport_bounds();
        let y = event.position.y;
        let distance = if y < bounds.top() {
            y - bounds.top()
        } else if y > bounds.bottom() {
            y - bounds.bottom()
        } else {
            return;
        };
        self.list.scroll_by(distance * DRAG_SCROLL_SHARE);
        cx.notify();
    }

    fn copy(&mut self, _: &CopyValue, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some((start, end)) = self.selection.ordered() {
            let text = self.doc.text_between(start, end);
            if !text.is_empty() {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
        }
    }

    fn select_all(&mut self, _: &SelectAll, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some((start, end)) = self.doc.full_range() {
            self.selection.select(start, end);
            cx.notify();
        }
    }

    fn scroll_lines(&mut self, distance: Pixels, cx: &mut Context<Self>) {
        self.list.scroll_by(distance);
        cx.notify();
    }

    fn page(&self) -> Pixels {
        self.list.viewport_bounds().size.height * PAGE_SHARE
    }

    fn render_text(&self, text: &MdText, cx: &mut Context<Self>) -> RichText {
        let view = cx.entity();
        let owned = text.clone();
        RichText::new(SharedString::from(text.text.clone()))
            .spans(inline_spans(text))
            .selection(self.selection.bytes_in(text.id, text.text.len()))
            .pointer(self.hovered_link == Some(text.id))
            .on_event(move |event, window, cx| {
                view.update(cx, |this, cx| {
                    this.text_event(&owned, event, window, cx);
                });
            })
    }

    /// A framed block of source text in the monospace font.
    fn render_source(
        &self,
        label: Option<SharedString>,
        text: &MdText,
        color: Hsla,
        cx: &mut Context<Self>,
    ) -> Div {
        div()
            .w_full()
            .p_3()
            .flex()
            .flex_col()
            .gap_1()
            .rounded_md()
            .bg(theme::surface())
            .border_1()
            .border_color(theme::border())
            .children(
                label.map(|label| div().text_xs().text_color(theme::text_muted()).child(label)),
            )
            .child(
                div()
                    .font_family(theme::mono_font())
                    .text_size(style::DOC_CODE_SIZE)
                    .line_height(relative(style::DOC_CODE_LINE_HEIGHT))
                    .text_color(color)
                    .child(self.render_text(text, cx).preserve_lines()),
            )
    }

    fn render_table_row(
        &self,
        cells: &[MdText],
        head: bool,
        alignments: &[MdAlign],
        widths: &[f32],
        cx: &mut Context<Self>,
    ) -> Div {
        div()
            .w_full()
            .flex()
            .border_b_1()
            .border_l_1()
            .border_color(theme::border())
            .when(head, |row| {
                row.border_t_1()
                    .bg(theme::surface())
                    .font_weight(FontWeight::SEMIBOLD)
            })
            .children(cells.iter().enumerate().map(|(ix, cell)| {
                let align = match alignments.get(ix) {
                    Some(MdAlign::Center) => TextAlign::Center,
                    Some(MdAlign::Right) => TextAlign::Right,
                    _ => TextAlign::Left,
                };
                let width = widths.get(ix).copied().unwrap_or(0.);
                div()
                    .w(relative(width))
                    .min_w_0()
                    .px_2()
                    .py_1()
                    .border_r_1()
                    .border_color(theme::border())
                    .child(self.render_text(cell, cx).align(align))
            }))
    }

    fn render_image(&self, url: &str, alt: &str) -> Div {
        let alt_text: SharedString = alt.to_string().into();
        let Some(path) = image_path(&self.base_dir, url) else {
            // Images on the web are not fetched.
            return div()
                .text_color(theme::text_muted())
                .child(SharedString::from(format!("{alt} ({url})")));
        };
        let fallback = alt_text.clone();
        div().w_full().child(
            img(path)
                .image_cache(&self.images)
                .max_w_full()
                .object_fit(ObjectFit::ScaleDown)
                .with_fallback(move || {
                    div()
                        .text_color(theme::text_muted())
                        .child(fallback.clone())
                        .into_any_element()
                }),
        )
    }

    fn render_body(&self, block: &MdBlock, cx: &mut Context<Self>) -> Div {
        match &block.kind {
            MdBlockKind::Heading { level, text } => {
                let (size, top) = style::doc_heading(*level);
                div()
                    .pt(top)
                    .text_size(size)
                    .line_height(relative(style::DOC_HEADING_LINE_HEIGHT))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(self.render_text(text, cx))
            }
            MdBlockKind::Paragraph(text) => div().child(self.render_text(text, cx)),
            MdBlockKind::Code { language, text } => {
                let label = language.clone().map(SharedString::from);
                self.render_source(label, text, theme::text().into(), cx)
            }
            MdBlockKind::Source { kind, text } => {
                let label = match kind {
                    MdSourceKind::Html => "HTML",
                    MdSourceKind::Metadata => "Metadata",
                };
                self.render_source(Some(label.into()), text, theme::text_muted().into(), cx)
            }
            MdBlockKind::TableRow {
                cells,
                head,
                alignments,
                widths,
                ..
            } => self.render_table_row(cells, *head, alignments, widths, cx),
            MdBlockKind::Image { url, alt } => self.render_image(url, alt),
            MdBlockKind::Rule => div().w_full().h(px(1.)).bg(theme::border()),
        }
    }

    fn render_block(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let block = &self.doc.blocks[ix];
        let body = self.render_body(block, cx);
        let depth = block.list_depth as f32;
        let content = match &block.marker {
            Some(marker) => div()
                .pl(style::DOC_LIST_INDENT * (depth - 1.).max(0.))
                .flex()
                .child(render_marker(marker, block.list_depth))
                .child(div().flex_1().min_w_0().child(body)),
            None => div().pl(style::DOC_LIST_INDENT * depth).child(body),
        };
        let next = self.doc.blocks.get(ix + 1);
        let content = content.pb(bottom_gap(block, next));
        let quoted = (0..block.quote_depth).fold(content, |inner, _| {
            div()
                .flex()
                .child(
                    div()
                        .flex_none()
                        .w(style::DOC_QUOTE_BAR)
                        .mr(style::DOC_QUOTE_INDENT - style::DOC_QUOTE_BAR)
                        .bg(theme::border()),
                )
                .child(div().flex_1().min_w_0().child(inner))
        });
        div()
            .w_full()
            .flex()
            .justify_center()
            .when(ix == 0, |row| row.pt(style::DOC_PADDING_Y))
            .when(next.is_none(), |row| row.pb(style::DOC_PADDING_Y))
            .child(
                div()
                    .w_full()
                    .max_w(style::DOC_READING_WIDTH)
                    .px(style::DOC_PADDING_X)
                    .child(quoted),
            )
            .into_any_element()
    }
}

/// Space below a block. List items sit close together, and table rows touch.
fn bottom_gap(block: &MdBlock, next: Option<&MdBlock>) -> Pixels {
    if let MdBlockKind::TableRow { last: false, .. } = block.kind {
        return px(0.);
    }
    let in_list = block.list_depth > 0 && next.is_some_and(|next| next.list_depth > 0);
    if in_list {
        style::DOC_LIST_GAP
    } else {
        style::DOC_BLOCK_GAP
    }
}

/// The column holding a list item's marker. Task boxes are drawn, since systems show the box symbols as colored emoji.
fn render_marker(marker: &MdMarker, depth: u16) -> Div {
    let column = div()
        .flex_none()
        .w(style::DOC_LIST_INDENT)
        .text_color(theme::text_muted());
    let text: SharedString = match marker {
        MdMarker::Bullet => match depth {
            0 | 1 => "•".into(),
            2 => "◦".into(),
            _ => "▪".into(),
        },
        MdMarker::Number(number) => format!("{number}.").into(),
        MdMarker::Footnote(label) => format!("[{label}]").into(),
        MdMarker::Task { checked } => {
            return column.pt(style::DOC_TASK_BOX_TOP).child(
                div()
                    .size(style::DOC_TASK_BOX)
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_sm()
                    .border_1()
                    .border_color(theme::text_muted())
                    .text_size(style::DOC_TASK_MARK_SIZE)
                    .line_height(relative(1.))
                    .when(*checked, |task| {
                        task.bg(theme::accent())
                            .border_color(theme::accent())
                            .text_color(theme::background())
                            .child("✓")
                    }),
            );
        }
    };
    column.child(text)
}

/// The drawn form of a text's inline styles. Colors and fonts come from the theme.
fn inline_spans(text: &MdText) -> Vec<TextSpan> {
    text.spans
        .iter()
        .map(|span| TextSpan {
            range: span.range.clone(),
            font_family: span.style.code.then(theme::mono_font),
            bold: span.style.bold,
            italic: span.style.italic,
            color: if span.link.is_some() {
                Some(theme::accent().into())
            } else if span.style.muted {
                Some(theme::text_muted().into())
            } else {
                None
            },
            background: span.style.code.then(|| theme::button().into()),
            underline: span.link.is_some(),
            strikethrough: span.style.strikethrough,
        })
        .collect()
}

/// Links opened in the browser. Other destinations, such as other documents, stay where they are.
fn is_web_link(link: &str) -> bool {
    ["http://", "https://", "mailto:"].iter().any(|scheme| {
        link.len() >= scheme.len() && link[..scheme.len()].eq_ignore_ascii_case(scheme)
    })
}

/// The file an image points to: a path relative to the document, or an absolute one. Addresses with a scheme are not
/// files. Percent escapes are decoded, and a query or fragment is left out.
fn image_path(base_dir: &Path, url: &str) -> Option<PathBuf> {
    let has_scheme = url.split_once(':').is_some_and(|(scheme, _)| {
        scheme.len() > 1 && scheme.chars().all(|ch| ch.is_ascii_alphanumeric())
    });
    if url.is_empty() || has_scheme || url.starts_with("//") {
        return None;
    }
    let file = url.split(['?', '#']).next().unwrap_or(url);
    Some(base_dir.join(percent_decoded(file)))
}

fn percent_decoded(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut ix = 0;
    while ix < bytes.len() {
        let escaped = (bytes[ix] == b'%')
            .then(|| text.get(ix + 1..ix + 3))
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match escaped {
            Some(byte) => {
                out.push(byte);
                ix += 3;
            }
            None => {
                out.push(bytes[ix]);
                ix += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl Render for MarkdownView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let container =
            div()
                .size_full()
                .relative()
                .track_focus(&self.focus_handle)
                .key_context("Document")
                .text_size(style::DOC_TEXT_SIZE)
                .line_height(relative(style::DOC_LINE_HEIGHT))
                .on_action(cx.listener(Self::copy))
                .on_action(cx.listener(Self::select_all))
                .on_action(cx.listener(|this, _: &MoveUp, _, cx| {
                    this.scroll_lines(-style::DOC_SCROLL_STEP, cx)
                }))
                .on_action(cx.listener(|this, _: &MoveDown, _, cx| {
                    this.scroll_lines(style::DOC_SCROLL_STEP, cx)
                }))
                .on_action(cx.listener(|this, _: &PageUp, _, cx| {
                    let page = this.page();
                    this.scroll_lines(-page, cx)
                }))
                .on_action(cx.listener(|this, _: &PageDown, _, cx| {
                    let page = this.page();
                    this.scroll_lines(page, cx)
                }))
                .on_action(cx.listener(|this, _: &MoveToFirstRow, _, cx| {
                    this.list.scroll_to(ListOffset::default());
                    cx.notify();
                }))
                .on_action(cx.listener(|this, _: &MoveToLastRow, _, cx| {
                    this.list.scroll_to_end();
                    cx.notify();
                }))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, window, cx| {
                        this.focus_handle.focus(window, cx);
                        if this.selection.press_elsewhere() {
                            cx.notify();
                        }
                    }),
                );
        if self.doc.blocks.is_empty() {
            return container
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme::text_muted())
                .child("This document is empty.");
        }
        let view = cx.entity();
        let drag_listeners = canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                let up = view.clone();
                window.on_mouse_event(move |event: &MouseUpEvent, phase, _window, cx| {
                    if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                        up.update(cx, |this, _| this.selection.release());
                    }
                });
                let moved = view.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _window, cx| {
                    if phase == DispatchPhase::Bubble {
                        moved.update(cx, |this, cx| this.drag_scroll(event, cx));
                    }
                });
            },
        )
        .absolute()
        .size_0();
        let (read, write) = (self.list.clone(), self.list.clone());
        container
            .child(
                list(
                    self.list.clone(),
                    cx.processor(|this, ix: usize, _window, cx| this.render_block(ix, cx)),
                )
                .size_full(),
            )
            .child(scrollbar(
                self.scrollbar.clone(),
                move || ScrollMetrics {
                    offset: -read.scroll_px_offset_for_scrollbar().y,
                    max: read.max_offset_for_scrollbar().y,
                    viewport: read.viewport_bounds().size.height,
                },
                move |offset, window, _cx| {
                    write.set_offset_from_scrollbar(point(px(0.), -offset));
                    window.refresh();
                },
            ))
            .child(drag_listeners)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_only_web_links() {
        assert!(is_web_link("https://example.com"));
        assert!(is_web_link("HTTP://example.com"));
        assert!(is_web_link("mailto:a@b.c"));
        for link in ["usage.md", "#제목", "file:///C:/a", "javascript:x", "ht"] {
            assert!(!is_web_link(link), "{link}");
        }
    }

    #[test]
    fn finds_image_files_next_to_the_document() {
        let base = Path::new("base");
        assert_eq!(
            image_path(base, "assets/a%20b.png"),
            Some(base.join("assets/a b.png"))
        );
        assert_eq!(
            image_path(base, "%EA%B7%B8%EB%A6%BC.png?raw=1#top"),
            Some(base.join("그림.png"))
        );
        assert_eq!(image_path(base, "100%.png"), Some(base.join("100%.png")));
        for url in [
            "https://x/a.png",
            "data:image/png;base64,AA",
            "//x/a.png",
            "",
        ] {
            assert_eq!(image_path(base, url), None, "{url}");
        }
    }

    #[test]
    fn spaces_blocks_by_their_kind() {
        let doc = MarkdownDocument::parse("- a\n- b\n\nc\n\n| x |\n|---|\n| y |\n");
        let gap = |ix: usize| bottom_gap(&doc.blocks[ix], doc.blocks.get(ix + 1));
        assert_eq!(gap(0), style::DOC_LIST_GAP);
        assert_eq!(gap(1), style::DOC_BLOCK_GAP);
        assert_eq!(gap(2), style::DOC_BLOCK_GAP);
        assert_eq!(gap(3), px(0.));
        assert_eq!(gap(4), style::DOC_BLOCK_GAP);
    }
}
