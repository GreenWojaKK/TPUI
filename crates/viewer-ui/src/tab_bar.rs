//! Tab bar of open files. Draws tab titles and the active marker, and handles switch, close and reorder input.
//!
//! With many tabs, widths shrink evenly down to a minimum; beyond that the bar scrolls horizontally and draws a
//! scroll position indicator at the bottom. Dropping a dragged tab on another tab moves it there, and holding it
//! near either end of the bar while dragging scrolls the bar that way.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    App, Bounds, ClickEvent, Context, DragMoveEvent, Pixels, Point, ScrollHandle, SharedString,
    Window, canvas, div, fill, point, prelude::*, px, size,
};

use crate::theme;

const TAB_MAX_WIDTH: Pixels = px(220.);
const TAB_MIN_WIDTH: Pixels = px(120.);
const TAB_SCROLL_INDICATOR_HEIGHT: Pixels = px(3.);
const TAB_SCROLL_INDICATOR_MIN_WIDTH: Pixels = px(24.);
const TAB_CLOSE_SIZE: Pixels = px(18.);
/// Holding a dragged tab within this width of either end of the tab bar scrolls the bar that way.
const TAB_DRAG_EDGE: Pixels = px(40.);
/// Maximum scroll per frame while a dragged tab is held at an end of the bar.
const TAB_DRAG_SCROLL_STEP: Pixels = px(12.);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TabStatus {
    Opening,
    Ready,
    Error,
}

pub struct TabLabel {
    pub title: SharedString,
    /// Full path shown on hover, so files stay distinguishable when shrunk tabs truncate their names.
    pub path: SharedString,
    pub status: TabStatus,
}

/// Tab bar state kept between renders: the scroll position and the auto-scroll speed while a tab is dragged
/// to an end of the bar.
#[derive(Clone, Default)]
pub struct TabBarState {
    scroll: ScrollHandle,
    /// Horizontal offset added each frame. Negative reveals tabs on the left. 0 when not dragging.
    edge_scroll: Rc<Cell<Pixels>>,
}

impl TabBarState {
    pub fn scroll_to_tab(&self, ix: usize) {
        self.scroll.scroll_to_item(ix);
    }

    /// Moves the horizontal offset by `delta`. `false` if already at the end and nothing moved.
    fn scroll_by(&self, delta: Pixels) -> bool {
        let max = self.scroll.max_offset().x;
        let offset = self.scroll.offset();
        let x = (offset.x + delta).clamp(-max, px(0.));
        self.scroll.set_offset(point(x, offset.y));
        x != offset.x
    }
}

/// The tab being dragged. Also drawn as the drag preview.
#[derive(Clone)]
struct DraggedTab {
    index: usize,
    title: SharedString,
}

impl Render for DraggedTab {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(TAB_MAX_WIDTH)
            .h(theme::metrics().panel_header_height)
            .px_3()
            .flex()
            .items_center()
            .bg(theme::background())
            .border_1()
            .border_color(theme::scrollbar_thumb())
            .text_xs()
            .text_color(theme::text())
            .font_family(theme::ui_font())
            .child(div().min_w_0().truncate().child(self.title.clone()))
    }
}

struct TabTooltip(SharedString);

impl Render for TabTooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(theme::surface())
            .border_1()
            .border_color(theme::border())
            .text_xs()
            .text_color(theme::text())
            .font_family(theme::ui_font())
            .child(self.0.clone())
    }
}

type TabHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;
type MoveHandler = Rc<dyn Fn(usize, usize, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct TabBar {
    labels: Vec<TabLabel>,
    active: Option<usize>,
    state: TabBarState,
    on_select: TabHandler,
    on_close: TabHandler,
    on_move: MoveHandler,
}

impl TabBar {
    pub fn new(
        labels: Vec<TabLabel>,
        active: Option<usize>,
        state: TabBarState,
        on_select: impl Fn(usize, &mut Window, &mut App) + 'static,
        on_close: impl Fn(usize, &mut Window, &mut App) + 'static,
        on_move: impl Fn(usize, usize, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            labels,
            active,
            state,
            on_select: Rc::new(on_select),
            on_close: Rc::new(on_close),
            on_move: Rc::new(on_move),
        }
    }
}

impl RenderOnce for TabBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        if !cx.has_active_drag() {
            self.state.edge_scroll.set(px(0.));
        }
        let edge_scroll = self.state.edge_scroll.get();
        // Redraw every frame until the end is reached to keep scrolling.
        if edge_scroll != px(0.) && self.state.scroll_by(edge_scroll) {
            window.request_animation_frame();
        }

        let last = self.labels.len().saturating_sub(1);
        let tabs = self.labels.into_iter().enumerate().map(|(ix, label)| {
            let active = self.active == Some(ix);
            let on_select = self.on_select.clone();
            let on_close = self.on_close.clone();
            let on_move = self.on_move.clone();
            let title_color = match label.status {
                TabStatus::Error => theme::error(),
                TabStatus::Opening => theme::text_muted(),
                TabStatus::Ready if active => theme::text(),
                TabStatus::Ready => theme::text_muted(),
            };
            let dragged = DraggedTab {
                index: ix,
                title: label.title.clone(),
            };
            div()
                .id(("tab", ix))
                .flex_1()
                .min_w(TAB_MIN_WIDTH)
                .max_w(TAB_MAX_WIDTH)
                .h_full()
                .pl_3()
                .pr_1()
                .flex()
                .items_center()
                .gap_1()
                .border_r_1()
                .border_color(theme::border())
                .cursor_pointer()
                .when(active, |tab| tab.bg(theme::background()))
                .when(!active, |tab| tab.hover(|style| style.bg(theme::hover())))
                .tooltip({
                    let path = label.path.clone();
                    move |_, cx| cx.new(|_| TabTooltip(path.clone())).into()
                })
                .on_click(move |_: &ClickEvent, window, cx| on_select(ix, window, cx))
                .on_drag(dragged, |tab, _: Point<Pixels>, _, cx| {
                    cx.new(|_| tab.clone())
                })
                .drag_over::<DraggedTab>(move |style, tab, _, _| {
                    if tab.index == ix {
                        style
                    } else {
                        style.bg(theme::drop_target())
                    }
                })
                .on_drop(move |tab: &DraggedTab, window, cx| on_move(tab.index, ix, window, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(title_color)
                        .child(label.title),
                )
                .child(
                    div()
                        .id(("tab-close", ix))
                        .flex_none()
                        .w(TAB_CLOSE_SIZE)
                        .h(TAB_CLOSE_SIZE)
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_sm()
                        .text_color(theme::text_muted())
                        .hover(|style| style.bg(theme::button_hover()).text_color(theme::text()))
                        .child("×")
                        .on_click(move |_: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            on_close(ix, window, cx);
                        }),
                )
        });
        // The indicator is drawn after the tab list so it uses the scroll state from this frame's layout.
        let scroll = self.state.scroll.clone();
        let indicator = canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                if let Some((left, width)) = scroll_indicator(&scroll) {
                    let origin = point(bounds.left() + left, bounds.top());
                    let size = size(width, bounds.size.height);
                    window.paint_quad(fill(Bounds::new(origin, size), theme::scrollbar_thumb()));
                }
            },
        )
        .absolute()
        .left_0()
        .bottom_0()
        .w_full()
        .h(TAB_SCROLL_INDICATOR_HEIGHT);
        let edge = self.state.edge_scroll.clone();
        let on_move = self.on_move.clone();
        div()
            .flex_none()
            .relative()
            .h(theme::metrics().panel_header_height)
            .bg(theme::surface())
            .border_b_1()
            .border_color(theme::border())
            .text_xs()
            .child(
                div()
                    .id("tab-bar")
                    .size_full()
                    .flex()
                    .overflow_x_scroll()
                    .track_scroll(&self.state.scroll)
                    .on_drag_move::<DraggedTab>(move |event: &DragMoveEvent<DraggedTab>, _, _| {
                        edge.set(edge_scroll_speed(event.bounds, event.event.position));
                    })
                    // Dropping on empty space, not on a tab, moves to the last slot. Drops on a tab are handled by the tab first.
                    .on_drop(move |tab: &DraggedTab, window, cx| {
                        on_move(tab.index, last, window, cx)
                    })
                    .children(tabs),
            )
            .child(indicator)
    }
}

/// Distance to scroll per frame while a dragged tab is near either end of the bar. Faster closer to the end;
/// 0 outside the bar height or far from the ends.
fn edge_scroll_speed(bounds: gpui::Bounds<Pixels>, position: Point<Pixels>) -> Pixels {
    if position.y < bounds.top() || position.y > bounds.bottom() {
        return px(0.);
    }
    let edge = TAB_DRAG_EDGE;
    let left = bounds.left() + edge - position.x;
    let right = position.x - (bounds.right() - edge);
    let depth = if left > px(0.) {
        -left.min(edge)
    } else if right > px(0.) {
        right.min(edge)
    } else {
        return px(0.);
    };
    // A more negative offset shows more of the right side, so the right end decreases the offset.
    -TAB_DRAG_SCROLL_STEP * (depth / edge)
}

/// Left position and width of the scroll indicator when tabs overflow the bar. `None` if they fit.
fn scroll_indicator(scroll: &ScrollHandle) -> Option<(Pixels, Pixels)> {
    let view = scroll.bounds().size.width;
    let hidden = scroll.max_offset().x;
    if view <= px(0.) || hidden <= px(0.) {
        return None;
    }
    let width = (view * (view / (view + hidden))).max(TAB_SCROLL_INDICATOR_MIN_WIDTH);
    let ratio = (-scroll.offset().x / hidden).clamp(0., 1.);
    Some(((view - width) * ratio, width))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar() -> Bounds<Pixels> {
        Bounds::new(point(px(100.), px(50.)), size(px(600.), px(28.)))
    }

    #[test]
    fn scrolls_only_near_the_bar_edges() {
        let middle = edge_scroll_speed(bar(), point(px(400.), px(60.)));
        assert_eq!(middle, px(0.));
        let below = edge_scroll_speed(bar(), point(px(105.), px(200.)));
        assert_eq!(below, px(0.));
    }

    #[test]
    fn left_edge_reveals_left_tabs_and_right_edge_reveals_right_tabs() {
        // A larger offset (closer to 0) reveals tabs on the left.
        let left = edge_scroll_speed(bar(), point(px(100.), px(60.)));
        assert_eq!(left, TAB_DRAG_SCROLL_STEP);
        let right = edge_scroll_speed(bar(), point(px(700.), px(60.)));
        assert_eq!(right, -TAB_DRAG_SCROLL_STEP);
        let half = edge_scroll_speed(bar(), point(px(680.), px(60.)));
        assert_eq!(half, -TAB_DRAG_SCROLL_STEP * 0.5);
    }

    #[test]
    fn caps_speed_when_the_pointer_leaves_the_bar_sideways() {
        let outside = edge_scroll_speed(bar(), point(px(0.), px(60.)));
        assert_eq!(outside, TAB_DRAG_SCROLL_STEP);
    }
}
