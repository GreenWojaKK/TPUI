//! 열린 파일의 탭 막대. 탭 이름과 활성 표시를 그리고 전환·닫기·순서 이동 입력을 받는다.
//!
//! 탭이 많으면 최소 폭까지 폭을 나눠 줄이고, 그래도 넘치면 가로로 스크롤한다. 넘칠 때는 아래쪽에
//! 스크롤 위치 표시를 그린다. 탭을 끌어 다른 탭 위에 놓으면 그 자리로 옮기고, 끄는 중에 막대 양
//! 끝에 두면 막대가 그쪽으로 스크롤된다.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    App, Bounds, ClickEvent, Context, DragMoveEvent, Pixels, Point, ScrollHandle, SharedString,
    Window, canvas, div, fill, point, prelude::*, px, size,
};

use crate::theme;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TabStatus {
    Opening,
    Ready,
    Error,
}

pub struct TabLabel {
    pub title: SharedString,
    /// 마우스를 올리면 보여주는 전체 경로. 줄어든 탭에서 이름이 잘려도 파일을 구별할 수 있다.
    pub path: SharedString,
    pub status: TabStatus,
}

/// 렌더 사이에 유지하는 탭 막대 상태. 스크롤 위치와, 탭을 끌어 막대 끝에 두었을 때의 자동 스크롤
/// 속도를 담는다.
#[derive(Clone, Default)]
pub struct TabBarState {
    scroll: ScrollHandle,
    /// frame마다 더할 가로 offset. 음수는 왼쪽 탭을 드러낸다. 끄는 중이 아니면 0이다.
    edge_scroll: Rc<Cell<Pixels>>,
}

impl TabBarState {
    pub fn scroll_to_tab(&self, ix: usize) {
        self.scroll.scroll_to_item(ix);
    }

    /// 가로 offset을 `delta`만큼 옮긴다. 이미 끝에 닿아 움직이지 않았으면 `false`다.
    fn scroll_by(&self, delta: Pixels) -> bool {
        let max = self.scroll.max_offset().x;
        let offset = self.scroll.offset();
        let x = (offset.x + delta).clamp(-max, px(0.));
        self.scroll.set_offset(point(x, offset.y));
        x != offset.x
    }
}

/// 끌고 있는 탭. 드래그 미리보기로도 그린다.
#[derive(Clone)]
struct DraggedTab {
    index: usize,
    title: SharedString,
}

impl Render for DraggedTab {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(theme::TAB_MAX_WIDTH)
            .h(theme::PANEL_HEADER_HEIGHT)
            .px_3()
            .flex()
            .items_center()
            .bg(theme::background())
            .border_1()
            .border_color(theme::scrollbar_thumb())
            .text_xs()
            .text_color(theme::text())
            .font_family(theme::UI_FONT)
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
            .font_family(theme::UI_FONT)
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
        // 끝에 닿을 때까지 frame마다 다시 그려 스크롤을 이어 간다.
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
                .min_w(theme::TAB_MIN_WIDTH)
                .max_w(theme::TAB_MAX_WIDTH)
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
                        .w(theme::TAB_CLOSE_SIZE)
                        .h(theme::TAB_CLOSE_SIZE)
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
        // 위치 표시는 탭 목록 다음에 그려, 이번 frame의 layout이 정한 스크롤 상태로 계산한다.
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
        .h(theme::TAB_SCROLL_INDICATOR_HEIGHT);
        let edge = self.state.edge_scroll.clone();
        let on_move = self.on_move.clone();
        div()
            .flex_none()
            .relative()
            .h(theme::PANEL_HEADER_HEIGHT)
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
                    // 탭이 아닌 빈 곳에 놓으면 마지막 자리로 옮긴다. 탭 위에 놓으면 탭이 먼저 처리한다.
                    .on_drop(move |tab: &DraggedTab, window, cx| {
                        on_move(tab.index, last, window, cx)
                    })
                    .children(tabs),
            )
            .child(indicator)
    }
}

/// 끌고 있는 탭이 막대 양 끝 가까이 있을 때 frame마다 스크롤할 거리. 끝에 가까울수록 빠르고,
/// 막대 높이를 벗어나거나 끝에서 멀면 0이다.
fn edge_scroll_speed(bounds: gpui::Bounds<Pixels>, position: Point<Pixels>) -> Pixels {
    if position.y < bounds.top() || position.y > bounds.bottom() {
        return px(0.);
    }
    let edge = theme::TAB_DRAG_EDGE;
    let left = bounds.left() + edge - position.x;
    let right = position.x - (bounds.right() - edge);
    let depth = if left > px(0.) {
        -left.min(edge)
    } else if right > px(0.) {
        right.min(edge)
    } else {
        return px(0.);
    };
    // offset이 음수일수록 오른쪽을 보므로, 오른쪽 끝에서는 offset을 줄인다.
    -theme::TAB_DRAG_SCROLL_STEP * (depth / edge)
}

/// 탭이 막대 폭을 넘칠 때 스크롤 위치 표시의 왼쪽 위치와 폭. 넘치지 않으면 `None`이다.
fn scroll_indicator(scroll: &ScrollHandle) -> Option<(Pixels, Pixels)> {
    let view = scroll.bounds().size.width;
    let hidden = scroll.max_offset().x;
    if view <= px(0.) || hidden <= px(0.) {
        return None;
    }
    let width = (view * (view / (view + hidden))).max(theme::TAB_SCROLL_INDICATOR_MIN_WIDTH);
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
        // offset이 커지면(0에 가까워지면) 왼쪽 탭이 드러난다.
        let left = edge_scroll_speed(bar(), point(px(100.), px(60.)));
        assert_eq!(left, theme::TAB_DRAG_SCROLL_STEP);
        let right = edge_scroll_speed(bar(), point(px(700.), px(60.)));
        assert_eq!(right, -theme::TAB_DRAG_SCROLL_STEP);
        let half = edge_scroll_speed(bar(), point(px(680.), px(60.)));
        assert_eq!(half, -theme::TAB_DRAG_SCROLL_STEP * 0.5);
    }

    #[test]
    fn caps_speed_when_the_pointer_leaves_the_bar_sideways() {
        let outside = edge_scroll_speed(bar(), point(px(0.), px(60.)));
        assert_eq!(outside, theme::TAB_DRAG_SCROLL_STEP);
    }
}
