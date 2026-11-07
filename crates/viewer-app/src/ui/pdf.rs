//! PDF tab, for checking what a PDF file holds. Pages stand one under another, fitted to the width of the view up to a
//! limit. Only the pages on screen and one on each side are asked of the PDF worker, and the images of other pages are
//! let go.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use gpui::{
    App, Axis, Bounds, Corners, DispatchPhase, Element, ElementId, Entity, EventEmitter,
    FocusHandle, GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, IntoElement,
    LayoutId, MouseButton, MouseDownEvent, Pixels, RenderImage, ScrollEvent, SharedString, Size,
    Style, Window, div, fill, point, prelude::*, px, relative, size,
};
use image::{Frame, RgbaImage};
use viewer_core::{PdfInfo, PdfPageImage};
use viewer_ui::format::format_quantity;
use viewer_ui::theme;

use super::doc_scrollbar::{ScrollMetrics, ScrollbarDrag, scrollbar};
use super::grid::{MoveDown, MoveToFirstRow, MoveToLastRow, MoveUp, PageDown, PageUp};
use super::style;

/// Share of the view one Page Up or Page Down moves.
const PAGE_SHARE: f64 = 0.9;

/// Where a PDF tab was left: a page and how far down it the top of the view was, as a share of its height. It stays
/// meaningful when the view's width or the file changes.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PdfScroll {
    pub page: usize,
    pub within: f64,
}

/// Where the pages stand for a view width, in logical pixels.
#[derive(Clone, Debug, Default, PartialEq)]
struct PageLayout {
    view_width: f64,
    left: f64,
    width: f64,
    tops: Vec<f64>,
    heights: Vec<f64>,
    bottoms: Vec<f64>,
    total: f64,
}

fn page_layout(info: &PdfInfo, view_width: f64) -> PageLayout {
    let bar = f64::from(f32::from(style::GRID_SCROLLBAR_SIZE));
    let padding = f64::from(f32::from(style::PDF_PADDING));
    let gap = f64::from(f32::from(style::PDF_PAGE_GAP));
    let room = (view_width - bar - padding * 2.).max(1.);
    let width = room.min(f64::from(f32::from(style::PDF_MAX_PAGE_WIDTH)));
    let left = ((view_width - bar - width) / 2.).max(0.);
    let mut layout = PageLayout {
        view_width,
        left,
        width,
        ..PageLayout::default()
    };
    let mut y = padding;
    for page in &info.pages {
        let ratio = if page.width > 0. {
            f64::from(page.height / page.width)
        } else {
            1.
        };
        let height = width * ratio;
        layout.tops.push(y);
        layout.heights.push(height);
        layout.bottoms.push(y + height);
        y += height + gap;
    }
    layout.total = if info.pages.is_empty() {
        padding * 2.
    } else {
        y - gap + padding
    };
    layout
}

impl PageLayout {
    /// The pages that reach into the stretch from `top` down `height`.
    fn visible(&self, top: f64, height: f64) -> Range<usize> {
        let first = self.bottoms.partition_point(|&bottom| bottom <= top);
        let end = self
            .tops
            .partition_point(|&page_top| page_top < top + height);
        first..end.max(first)
    }

    /// The page at a height and how far into it that height is.
    fn anchor(&self, at: f64) -> PdfScroll {
        if self.tops.is_empty() {
            return PdfScroll::default();
        }
        let page = self
            .tops
            .partition_point(|&top| top <= at)
            .saturating_sub(1);
        let within = ((at - self.tops[page]) / self.heights[page].max(1.)).clamp(0., 1.);
        PdfScroll { page, within }
    }

    fn scroll_for(&self, anchor: PdfScroll) -> f64 {
        let Some(last) = self.tops.len().checked_sub(1) else {
            return 0.;
        };
        let page = anchor.page.min(last);
        self.tops[page] + anchor.within * self.heights[page]
    }
}

/// A drawn page and the pixel width it was drawn at.
struct PageBitmap {
    width: u32,
    image: Arc<RenderImage>,
}

fn render_image(image: PdfPageImage) -> Option<Arc<RenderImage>> {
    // GPUI takes BGRA in an RGBA-typed buffer.
    let buffer = RgbaImage::from_raw(image.width, image.height, image.bgra)?;
    Some(Arc::new(RenderImage::new(vec![Frame::new(buffer)])))
}

pub enum PdfViewEvent {
    /// The view needs these pages at this pixel width, nearest first. It replaces the request before.
    Draw { width: u32, pages: Vec<u32> },
}

pub struct PdfView {
    focus_handle: FocusHandle,
    info: Arc<PdfInfo>,
    scroll: f64,
    /// Where to stand once the view knows its width.
    restore: Option<PdfScroll>,
    layout: PageLayout,
    view_height: f64,
    bitmaps: HashMap<usize, PageBitmap>,
    /// The last request made.
    asked: Option<(u32, Vec<usize>)>,
    failure: Option<SharedString>,
    scrollbar: ScrollbarDrag,
}

impl EventEmitter<PdfViewEvent> for PdfView {}

impl PdfView {
    pub fn new(info: Arc<PdfInfo>, position: PdfScroll, cx: &mut Context<Self>) -> Self {
        // GPUI keeps an image on the GPU until it is dropped there, so the view's pages go with the view.
        cx.on_release(|this, cx| {
            for (_, bitmap) in this.bitmaps.drain() {
                cx.drop_image(bitmap.image, None);
            }
        })
        .detach();
        Self {
            focus_handle: cx.focus_handle(),
            info,
            scroll: 0.,
            restore: Some(position),
            layout: PageLayout::default(),
            view_height: 0.,
            bitmaps: HashMap::new(),
            asked: None,
            failure: None,
            scrollbar: ScrollbarDrag::default(),
        }
    }

    /// Shows a new version of the file from the same page. Images of the old version are let go.
    pub fn set_document(&mut self, info: Arc<PdfInfo>, cx: &mut Context<Self>) {
        self.restore = Some(self.position());
        self.info = info;
        self.layout = PageLayout::default();
        for (_, bitmap) in self.bitmaps.drain() {
            cx.drop_image(bitmap.image, None);
        }
        self.asked = None;
        self.failure = None;
        cx.notify();
    }

    /// Takes a drawn page, unless the view has moved on from it.
    pub fn take_page(
        &mut self,
        page: u32,
        width: u32,
        image: PdfPageImage,
        cx: &mut Context<Self>,
    ) {
        let page = page as usize;
        let still_wanted = self
            .asked
            .as_ref()
            .is_some_and(|(asked, _)| *asked == width)
            && self.wanted_pages().contains(&page);
        if !still_wanted {
            return;
        }
        let Some(image) = render_image(image) else {
            return;
        };
        if let Some(old) = self.bitmaps.insert(page, PageBitmap { width, image }) {
            cx.drop_image(old.image, None);
        }
        cx.notify();
    }

    /// Shows why a page or the file could not be drawn, over the pages.
    pub fn page_failed(&mut self, message: String, cx: &mut Context<Self>) {
        self.failure = Some(message.into());
        cx.notify();
    }

    pub fn page_count(&self) -> usize {
        self.info.pages.len()
    }

    /// The page at the middle of the view, from one.
    pub fn current_page(&self) -> usize {
        if self.layout.tops.is_empty() {
            return self.restore.map_or(1, |restore| restore.page + 1);
        }
        self.layout.anchor(self.scroll + self.view_height / 2.).page + 1
    }

    pub fn position(&self) -> PdfScroll {
        if self.layout.tops.is_empty() {
            return self.restore.unwrap_or_default();
        }
        self.layout.anchor(self.scroll)
    }

    fn max_scroll(&self) -> f64 {
        (self.layout.total - self.view_height).max(0.)
    }

    fn set_scroll(&mut self, scroll: f64, cx: &mut Context<Self>) {
        let scroll = scroll.clamp(0., self.max_scroll());
        if scroll != self.scroll {
            self.scroll = scroll;
            cx.notify();
        }
    }

    fn scroll_by(&mut self, delta: f64, cx: &mut Context<Self>) {
        self.set_scroll(self.scroll + delta, cx);
    }

    /// Pages to keep drawn: those on screen and one on each side, nearest to the middle first.
    fn wanted_pages(&self) -> Vec<usize> {
        let visible = self.layout.visible(self.scroll, self.view_height);
        if self.layout.tops.is_empty() {
            return Vec::new();
        }
        let last = self.layout.tops.len() - 1;
        let mut pages: Vec<usize> = visible.clone().collect();
        if visible.start > 0 {
            pages.push(visible.start - 1);
        }
        if visible.end <= last {
            pages.push(visible.end);
        }
        pages
    }

    /// Lays the pages out for the view's size, asks for the pages it needs, and lets go of the others.
    fn arrange(
        &mut self,
        view: Size<Pixels>,
        scale: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<(Bounds<Pixels>, Option<Arc<RenderImage>>)> {
        let width = f64::from(f32::from(view.width));
        self.view_height = f64::from(f32::from(view.height));
        if self.layout.view_width != width || self.layout.tops.len() != self.info.pages.len() {
            let anchor = self.restore.take().unwrap_or_else(|| self.position());
            self.layout = page_layout(&self.info, width);
            self.scroll = self.layout.scroll_for(anchor);
        }
        if let Some(anchor) = self.restore.take() {
            self.scroll = self.layout.scroll_for(anchor);
        }
        self.scroll = self.scroll.clamp(0., self.max_scroll());

        let wanted = self.wanted_pages();
        let pixels = ((self.layout.width * f64::from(scale)).round() as u32).max(1);
        let missing: Vec<usize> = wanted
            .iter()
            .copied()
            .filter(|page| {
                self.bitmaps
                    .get(page)
                    .is_none_or(|bitmap| bitmap.width != pixels)
            })
            .collect();
        let request = (pixels, missing);
        if self.asked.as_ref() != Some(&request) {
            if !request.1.is_empty() {
                cx.emit(PdfViewEvent::Draw {
                    width: request.0,
                    pages: request.1.iter().map(|&page| page as u32).collect(),
                });
            }
            self.asked = Some(request);
        }
        let gone: Vec<usize> = self
            .bitmaps
            .keys()
            .copied()
            .filter(|page| !wanted.contains(page))
            .collect();
        for page in gone {
            if let Some(bitmap) = self.bitmaps.remove(&page) {
                cx.drop_image(bitmap.image, Some(window));
            }
        }

        self.layout
            .visible(self.scroll, self.view_height)
            .map(|page| {
                let bounds = Bounds::new(
                    point(
                        px(self.layout.left as f32),
                        px((self.layout.tops[page] - self.scroll) as f32),
                    ),
                    size(
                        px(self.layout.width as f32),
                        px(self.layout.heights[page] as f32),
                    ),
                );
                let image = self.bitmaps.get(&page).map(|bitmap| bitmap.image.clone());
                (bounds, image)
            })
            .collect()
    }
}

impl Render for PdfView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (read, write) = (cx.entity(), cx.entity());
        let step = f64::from(f32::from(style::DOC_SCROLL_STEP));
        div()
            .size_full()
            .relative()
            .bg(theme::background())
            .track_focus(&self.focus_handle)
            .key_context("Document")
            .on_action(cx.listener(move |this, _: &MoveUp, _, cx| this.scroll_by(-step, cx)))
            .on_action(cx.listener(move |this, _: &MoveDown, _, cx| this.scroll_by(step, cx)))
            .on_action(cx.listener(|this, _: &PageUp, _, cx| {
                let page = this.view_height * PAGE_SHARE;
                this.scroll_by(-page, cx)
            }))
            .on_action(cx.listener(|this, _: &PageDown, _, cx| {
                let page = this.view_height * PAGE_SHARE;
                this.scroll_by(page, cx)
            }))
            .on_action(cx.listener(|this, _: &MoveToFirstRow, _, cx| this.set_scroll(0., cx)))
            .on_action(cx.listener(|this, _: &MoveToLastRow, _, cx| this.set_scroll(f64::MAX, cx)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    this.focus_handle.focus(window, cx)
                }),
            )
            .child(PdfElement { view: cx.entity() })
            .child(scrollbar(
                Axis::Vertical,
                self.scrollbar.clone(),
                move |cx| {
                    let view = read.read(cx);
                    ScrollMetrics {
                        offset: px(view.scroll as f32),
                        max: px(view.max_scroll() as f32),
                        viewport: px(view.view_height as f32),
                    }
                },
                move |offset, _window, cx| {
                    let offset = f64::from(f32::from(offset));
                    write.update(cx, |view, cx| view.set_scroll(offset, cx));
                },
            ))
            .children(self.failure.clone().map(|failure| {
                div()
                    .absolute()
                    .top_2()
                    .left_2()
                    .right(style::GRID_SCROLLBAR_SIZE)
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .bg(theme::overlay())
                    .text_color(theme::error())
                    .child(failure)
            }))
    }
}

struct PdfElement {
    view: Entity<PdfView>,
}

impl IntoElement for PdfElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

struct PdfPaint {
    hitbox: Hitbox,
    pages: Vec<(Bounds<Pixels>, Option<Arc<RenderImage>>)>,
}

impl Element for PdfElement {
    type RequestLayoutState = ();
    type PrepaintState = PdfPaint;

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
        let scale = window.scale_factor();
        let pages = self
            .view
            .update(cx, |view, cx| view.arrange(bounds.size, scale, window, cx))
            .into_iter()
            .map(|(page, image)| (page + bounds.origin, image))
            .collect();
        PdfPaint {
            hitbox: window.insert_hitbox(bounds, HitboxBehavior::Normal),
            pages,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        paint: &mut Self::PrepaintState,
        window: &mut Window,
        _cx: &mut App,
    ) {
        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            for (page, image) in &paint.pages {
                window.paint_quad(fill(*page, style::pdf_page()));
                if let Some(image) = image {
                    let _ = window.paint_image(
                        *page,
                        *page,
                        Corners::default(),
                        image.clone(),
                        0,
                        false,
                    );
                }
                window.paint_quad(gpui::outline(
                    *page,
                    theme::border(),
                    gpui::BorderStyle::Solid,
                ));
            }
        });
        let (view, hitbox) = (self.view.clone(), paint.hitbox.clone());
        window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || !hitbox.should_handle_scroll(window) {
                return;
            }
            let delta = event.delta.pixel_delta(style::CODE_LINE_HEIGHT);
            let dy = f64::from(f32::from(delta.y));
            view.update(cx, |view, cx| view.scroll_by(-dy, cx));
            cx.stop_propagation();
        });
    }
}

/// Facts about the open PDF for the status bar.
pub fn summary(view: &PdfView) -> Vec<String> {
    vec![
        format_quantity(view.page_count() as u64, "page", "pages"),
        format!("page {}", view.current_page()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use viewer_core::PdfPageSize;

    fn info(pages: &[(f32, f32)]) -> PdfInfo {
        PdfInfo {
            pages: pages
                .iter()
                .map(|&(width, height)| PdfPageSize { width, height })
                .collect(),
        }
    }

    #[test]
    fn fits_pages_to_the_width_up_to_a_limit() {
        let narrow = page_layout(&info(&[(100., 200.), (200., 100.)]), 500.);
        let bar = f64::from(f32::from(style::GRID_SCROLLBAR_SIZE));
        let padding = f64::from(f32::from(style::PDF_PADDING));
        let gap = f64::from(f32::from(style::PDF_PAGE_GAP));
        let width = 500. - bar - padding * 2.;
        assert_eq!(narrow.width, width);
        assert_eq!(narrow.heights, [width * 2., width / 2.]);
        assert_eq!(narrow.tops, [padding, padding + width * 2. + gap]);
        assert_eq!(narrow.total, padding * 2. + width * 2.5 + gap);

        let wide = page_layout(&info(&[(100., 100.)]), 5_000.);
        let limit = f64::from(f32::from(style::PDF_MAX_PAGE_WIDTH));
        assert_eq!(wide.width, limit);
        // Centered in the room left of the scrollbar.
        assert_eq!(wide.left, (5_000. - bar - limit) / 2.);
        assert_eq!(page_layout(&info(&[]), 500.).total, padding * 2.);
    }

    #[test]
    fn finds_the_pages_on_screen() {
        let layout = page_layout(&info(&[(100., 100.); 10]), 300.);
        let (top, height) = (layout.tops[3], layout.heights[3]);
        assert_eq!(layout.visible(top, height), 3..4);
        // One pixel into the gaps around a page shows only that page; one pixel into the next pages shows them too.
        assert_eq!(layout.visible(top - 1., height + 2.), 3..4);
        let (above, below) = (layout.bottoms[2] - 1., layout.tops[4] + 1.);
        assert_eq!(layout.visible(above, below - above), 2..5);
        // The padding above the first page holds no page.
        assert_eq!(layout.visible(0., 1.), 0..0);
        assert_eq!(layout.visible(0., layout.tops[0] + 1.), 0..1);
        assert_eq!(layout.visible(layout.total, 100.), 10..10);
    }

    #[test]
    fn keeps_the_place_across_widths() {
        let pages = info(&[(100., 140.); 5]);
        let narrow = page_layout(&pages, 400.);
        let place = PdfScroll {
            page: 3,
            within: 0.25,
        };
        let scroll = narrow.scroll_for(place);
        assert_eq!(narrow.anchor(scroll), place);
        let wide = page_layout(&pages, 900.);
        assert_eq!(wide.anchor(wide.scroll_for(place)), place);
        // A place past the last page goes to the last page.
        let beyond = PdfScroll {
            page: 40,
            within: 0.,
        };
        assert_eq!(narrow.anchor(narrow.scroll_for(beyond)).page, 4);
    }

    #[test]
    fn hands_pixels_to_gpui_unchanged() {
        let image = PdfPageImage {
            width: 2,
            height: 1,
            bgra: vec![1, 2, 3, 255, 4, 5, 6, 255],
        };
        let rendered = render_image(image).unwrap();
        assert_eq!(rendered.as_bytes(0).unwrap(), &[1, 2, 3, 255, 4, 5, 6, 255]);
        assert!(
            render_image(PdfPageImage {
                width: 3,
                height: 3,
                bgra: vec![0; 4],
            })
            .is_none()
        );
    }
}
