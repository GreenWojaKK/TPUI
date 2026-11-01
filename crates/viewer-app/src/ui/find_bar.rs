//! Find bar above the grid. Shows the input, previous and next buttons, and the find status.

use gpui::{
    ClickEvent, Context, Entity, EventEmitter, SharedString, Subscription, Window, div, prelude::*,
};

use viewer_ui::format::format_quantity;
use viewer_ui::text_input::{TextInput, TextInputEvent};
use viewer_ui::theme;
use viewer_ui::widgets::small_button;

pub enum FindBarEvent {
    /// Finds the next (backward = false) or previous match.
    Search {
        backward: bool,
    },
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

pub struct FindBar {
    input: Entity<TextInput>,
    status: FindStatus,
    _subscription: Subscription,
}

impl EventEmitter<FindBarEvent> for FindBar {}

impl FindBar {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextInput::new("Find cells whose whole value matches", cx));
        let subscription =
            cx.subscribe(&input, |this, _, event: &TextInputEvent, cx| match event {
                TextInputEvent::Submit { backward } => cx.emit(FindBarEvent::Search {
                    backward: *backward,
                }),
                TextInputEvent::Cancel => cx.emit(FindBarEvent::Close),
                TextInputEvent::Changed => {
                    if this.status != FindStatus::Idle {
                        this.status = FindStatus::Idle;
                        cx.notify();
                    }
                }
            });
        Self {
            input,
            status: FindStatus::Idle,
            _subscription: subscription,
        }
    }

    pub fn query(&self, cx: &gpui::App) -> String {
        self.input.read(cx).text().to_string()
    }

    /// Focuses the input and selects all of its content.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.focus_and_select_all(window, cx));
    }

    pub fn set_status(&mut self, status: FindStatus, cx: &mut Context<Self>) {
        if self.status != status {
            self.status = status;
            cx.notify();
        }
    }
}

impl Render for FindBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (status, is_error): (SharedString, bool) = match &self.status {
            FindStatus::Idle => ("Enter: next · Shift+Enter: previous".into(), false),
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
            .h(theme::PANEL_HEADER_HEIGHT + px_4())
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
                    .w(gpui::px(280.))
                    .child(self.input.clone()),
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

fn px_4() -> gpui::Pixels {
    gpui::px(4.)
}
