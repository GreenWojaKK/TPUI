//! 여러 화면이 함께 쓰는 작은 부품.

use gpui::{Div, ElementId, SharedString, Stateful, div, prelude::*, px};

use crate::theme;

/// 막대 안에 놓는 작은 버튼. 누를 때의 동작은 호출한 쪽이 `on_click`으로 붙인다.
pub fn small_button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .px_2()
        .py(px(2.))
        .rounded_sm()
        .bg(theme::button())
        .hover(|style| style.bg(theme::button_hover()))
        .cursor_pointer()
        .child(label.into())
}

/// 고른 상태를 표시하는 작은 전환 버튼.
pub fn toggle_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .px_2()
        .py(px(2.))
        .rounded_sm()
        .cursor_pointer()
        .bg(if selected {
            theme::selection()
        } else {
            theme::button()
        })
        .when(!selected, |button| {
            button.hover(|style| style.bg(theme::button_hover()))
        })
        .child(label.into())
}
