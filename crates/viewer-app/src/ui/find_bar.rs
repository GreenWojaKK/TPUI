//! Find bar above the grid. Shows the value being found, the find status, and previous and next buttons.
//!
//! The bar has no text input. The value comes from a cell's context menu or from a query (`find …`) written in the
//! query window or the message file; `Change…` opens the query window to write another.

use gpui::{ClickEvent, Context, EventEmitter, SharedString, Window, div, prelude::*, px};

use viewer_ui::format::format_quantity;
use viewer_ui::theme;
use viewer_ui::widgets::small_button;

pub enum FindBarEvent {
    /// Finds the next (backward = false) or previous match.
    Search {
        backward: bool,
    },
    /// Opens the query window to change the value.
    Change,
    Close,
}

/// Status shown in the find bar.
#[derive(Clone, Debug, PartialEq)]
pub enum FindStatus {
    Idle,
    /// Computing match positions. Progress runs from 0 to 1.
    Searching(f32),
    /// Number of matching cells in the whole row view.
    Found(u64),
    NotFound,
    Failed(SharedString),
}

#[derive(Default)]
pub struct FindBar {
    query: String,
    status: Option<FindStatus>,
}

impl EventEmitter<FindBarEvent> for FindBar {}

impl FindBar {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// Replaces the value being found.
    pub fn set_query(&mut self, text: String, cx: &mut Context<Self>) {
        self.query = text;
        self.status = None;
        cx.notify();
    }

    pub fn set_status(&mut self, status: FindStatus, cx: &mut Context<Self>) {
        if self.status.as_ref() != Some(&status) {
            self.status = Some(status);
            cx.notify();
        }
    }
}

impl Render for FindBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (status, is_error): (SharedString, bool) =
            match self.status.as_ref().unwrap_or(&FindStatus::Idle) {
                FindStatus::Idle => ("F3: next · Shift+F3: previous".into(), false),
                FindStatus::Searching(progress) => {
                    (format!("Searching {:.0}%", progress * 100.).into(), false)
                }
                FindStatus::Found(cells) => (
                    format_quantity(*cells, "matching cell", "matching cells").into(),
                    false,
                ),
                FindStatus::NotFound => ("No matching cells.".into(), true),
                FindStatus::Failed(message) => (message.clone(), true),
            };
        div()
            .flex_none()
            .h(theme::metrics().panel_header_height + px(4.))
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .bg(theme::surface())
            .border_b_1()
            .border_color(theme::border())
            .text_xs()
            .child(
                div()
                    .flex_none()
                    .text_color(theme::text_muted())
                    .child("Find"),
            )
            .child(
                div()
                    .flex_none()
                    .max_w(px(280.))
                    .px_2()
                    .rounded_sm()
                    .bg(theme::button())
                    .truncate()
                    .child(self.query.clone()),
            )
            .child(
                small_button("find-change", "Change…").on_click(
                    cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(FindBarEvent::Change)),
                ),
            )
            .child(
                small_button("find-previous", "Previous").on_click(cx.listener(
                    |_, _: &ClickEvent, _, cx| cx.emit(FindBarEvent::Search { backward: true }),
                )),
            )
            .child(small_button("find-next", "Next").on_click(cx.listener(
                |_, _: &ClickEvent, _, cx| cx.emit(FindBarEvent::Search { backward: false }),
            )))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(if is_error {
                        theme::error()
                    } else {
                        theme::text_muted()
                    })
                    .child(status),
            )
            .child(
                small_button("find-close", "×")
                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(FindBarEvent::Close))),
            )
    }
}
