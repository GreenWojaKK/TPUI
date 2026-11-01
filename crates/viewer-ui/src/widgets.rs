//! Small components shared by several views.

use gpui::{Div, ElementId, SharedString, Stateful, div, prelude::*, px};

use crate::theme;

/// Small button placed in a bar. The caller attaches the click behavior with `on_click`.
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

/// Small toggle button that shows a selected state.
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
