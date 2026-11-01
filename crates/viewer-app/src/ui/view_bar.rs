//! 그리드 위의 필터·정렬 막대. 행 보기의 조건 목록, 계산 진행률과 취소, 오류와 안내를 표시한다.

use std::rc::Rc;

use gpui::{App, ClickEvent, SharedString, Window, div, prelude::*, px};
use viewer_core::ColumnId;

use crate::ui::find_bar::bar_button;
use crate::ui::theme;

#[derive(Clone, Copy, Debug)]
pub enum ViewBarAction {
    RemoveFilter(ColumnId),
    RemoveSort,
    ClearAll,
    /// 계산 중인 행 보기를 버리고 이전 조건으로 돌아간다.
    Cancel,
    /// 실패한 조건을 다시 계산한다.
    Retry,
}

pub type ActionHandler = Rc<dyn Fn(ViewBarAction, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct ViewBar {
    /// 필터 조건의 컬럼과 표시.
    pub filters: Vec<(ColumnId, SharedString)>,
    pub sort: Option<SharedString>,
    /// 계산 중이면 진행률.
    pub pending: Option<f32>,
    pub failed: Option<SharedString>,
    pub notice: Option<SharedString>,
    pub on_action: ActionHandler,
}

impl ViewBar {
    /// 표시할 내용이 있는지.
    pub fn is_visible(&self) -> bool {
        !self.filters.is_empty()
            || self.sort.is_some()
            || self.pending.is_some()
            || self.failed.is_some()
            || self.notice.is_some()
    }
}

fn chip(
    id: SharedString,
    label: SharedString,
    action: ViewBarAction,
    on_action: ActionHandler,
) -> impl IntoElement {
    div()
        .flex_none()
        .max_w(px(260.))
        .pl_2()
        .flex()
        .items_center()
        .gap_1()
        .rounded_sm()
        .bg(theme::button())
        .child(div().min_w_0().truncate().child(label))
        .child(
            div()
                .id(id)
                .flex_none()
                .px_1()
                .rounded_sm()
                .cursor_pointer()
                .text_color(theme::text_muted())
                .hover(|style| style.bg(theme::button_hover()).text_color(theme::text()))
                .child("×")
                .on_click(move |_: &ClickEvent, window, cx| on_action(action, window, cx)),
        )
}

impl RenderOnce for ViewBar {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let on_action = self.on_action.clone();
        let button = move |id: &'static str, label: &'static str, action: ViewBarAction| {
            let on_action = on_action.clone();
            bar_button(id, label)
                .on_click(move |_: &ClickEvent, window, cx| on_action(action, window, cx))
        };
        let has_conditions = !self.filters.is_empty() || self.sort.is_some();
        let (status, status_color) = match (&self.pending, &self.failed, &self.notice) {
            (Some(progress), _, _) => (
                Some(SharedString::from(format!(
                    "적용 중 {:.0}%",
                    progress * 100.
                ))),
                theme::text_muted(),
            ),
            (None, Some(failed), _) => (Some(failed.clone()), theme::error()),
            (None, None, Some(notice)) => (Some(notice.clone()), theme::text_muted()),
            _ => (None, theme::text_muted()),
        };
        div()
            .flex_none()
            .h(theme::PANEL_HEADER_HEIGHT + px(4.))
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .bg(theme::surface())
            .border_b_1()
            .border_color(theme::border())
            .text_xs()
            .overflow_hidden()
            .when(!self.filters.is_empty(), |bar| {
                bar.child(
                    div()
                        .flex_none()
                        .text_color(theme::text_muted())
                        .child("필터"),
                )
            })
            .children(self.filters.into_iter().map(|(column, label)| {
                chip(
                    format!("view-filter-{}", column.0).into(),
                    label,
                    ViewBarAction::RemoveFilter(column),
                    self.on_action.clone(),
                )
            }))
            .when_some(self.sort, |bar, sort| {
                bar.child(
                    div()
                        .flex_none()
                        .text_color(theme::text_muted())
                        .child("정렬"),
                )
                .child(chip(
                    "view-sort".into(),
                    sort,
                    ViewBarAction::RemoveSort,
                    self.on_action.clone(),
                ))
            })
            .child(div().flex_1())
            .children(status.map(|status| {
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(status_color)
                    .child(status)
            }))
            .when(self.pending.is_some(), |bar| {
                bar.child(button("view-cancel", "취소", ViewBarAction::Cancel))
            })
            .when(self.pending.is_none() && self.failed.is_some(), |bar| {
                bar.child(button("view-retry", "다시 시도", ViewBarAction::Retry))
            })
            .when(has_conditions, |bar| {
                bar.child(button("view-clear", "모두 해제", ViewBarAction::ClearAll))
            })
    }
}
