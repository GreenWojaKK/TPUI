//! 그리드 위의 찾기 막대. 입력, 이전·다음 버튼, 찾기 상태를 표시한다.

use gpui::{
    ClickEvent, Context, Entity, EventEmitter, SharedString, Subscription, Window, div, prelude::*,
};

use crate::format::format_count;
use crate::ui::text_input::{TextInput, TextInputEvent};
use crate::ui::theme;

pub enum FindBarEvent {
    /// 다음(backward = false) 또는 이전 일치를 찾는다.
    Search {
        backward: bool,
    },
    Close,
}

/// 찾기 막대에 표시하는 상태.
#[derive(Clone, Debug, PartialEq)]
pub enum FindStatus {
    Idle,
    /// 일치 위치를 계산하는 중. 진행률은 0부터 1까지다.
    Searching(f32),
    /// 행 보기 전체의 일치 셀 수.
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
        let input = cx.new(|cx| TextInput::new("값 전체가 같은 셀 찾기", cx));
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

    /// 입력에 포커스를 옮기고 내용 전체를 선택한다.
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
            FindStatus::Idle => ("Enter: 다음 · Shift+Enter: 이전".into(), false),
            FindStatus::Searching(progress) => {
                (format!("찾는 중 {:.0}%", progress * 100.).into(), false)
            }
            FindStatus::Found(cells) => (
                format!("일치하는 셀 {}개", format_count(*cells)).into(),
                false,
            ),
            FindStatus::NotFound => ("일치하는 셀이 없습니다.".into(), true),
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
                    .child("찾기"),
            )
            .child(
                div()
                    .flex_none()
                    .w(gpui::px(280.))
                    .child(self.input.clone()),
            )
            .child(bar_button("find-previous", "이전").on_click(cx.listener(
                |_, _: &ClickEvent, _, cx| cx.emit(FindBarEvent::Search { backward: true }),
            )))
            .child(bar_button("find-next", "다음").on_click(cx.listener(
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
                bar_button("find-close", "×")
                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(FindBarEvent::Close))),
            )
    }
}

fn px_4() -> gpui::Pixels {
    gpui::px(4.)
}

/// 막대 안의 작은 버튼.
pub fn bar_button(id: &'static str, label: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .px_2()
        .py(gpui::px(2.))
        .rounded_sm()
        .bg(theme::button())
        .hover(|style| style.bg(theme::button_hover()))
        .cursor_pointer()
        .child(label)
}
