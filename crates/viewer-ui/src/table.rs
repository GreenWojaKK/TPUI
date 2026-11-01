//! 칸 너비를 비율로 나눈 단순한 표의 머리글과 행. `uniform_list`의 행으로 쓴다.
//!
//! 칸이 좁아 잘리는 내용은 툴팁으로 전체를 보여준다.

use gpui::{
    AnyView, App, Context, Div, SharedString, Stateful, Window, div, prelude::*, px, relative,
};

use crate::theme;

/// 칸 이름과 너비 비율.
pub type ColumnSpec = (&'static str, f32);

/// 표의 한 칸.
#[derive(Clone, Debug, Default)]
pub struct Cell {
    pub text: SharedString,
    /// 마우스를 올리면 보여줄 내용. 여러 줄이면 줄바꿈으로 나눈다.
    pub tooltip: Option<SharedString>,
    pub align_right: bool,
}

impl Cell {
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    /// 수치처럼 오른쪽에 맞춘다.
    pub fn right(mut self) -> Self {
        self.align_right = true;
        self
    }

    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }
}

fn total_weight(columns: &[ColumnSpec]) -> f32 {
    columns.iter().map(|(_, weight)| weight).sum()
}

/// 표의 머리글 행. 모든 칸을 왼쪽에 맞춘다.
pub fn header(columns: &[ColumnSpec]) -> Div {
    header_aligned(columns, |_| false)
}

/// 표의 머리글 행. `align_right(칸 순번)`이 참인 칸은 오른쪽에 맞춘다. 수치 칸의 머리글을
/// 값과 같은 쪽에 둘 때 쓴다.
pub fn header_aligned(columns: &[ColumnSpec], align_right: impl Fn(usize) -> bool) -> Div {
    let total = total_weight(columns);
    div()
        .flex_none()
        .h(theme::LIST_ROW_HEIGHT)
        .px_2()
        .flex()
        .items_center()
        .bg(theme::surface())
        .border_b_1()
        .border_color(theme::border())
        .text_color(theme::text_muted())
        .children(columns.iter().enumerate().map(|(ix, (label, weight))| {
            div()
                .w(relative(weight / total))
                .min_w_0()
                .pr_2()
                .truncate()
                .when(align_right(ix), |cell| cell.flex().justify_end())
                .child(*label)
        }))
}

/// 표의 한 행. `name`과 `row`로 행과 칸의 element id를 만든다.
pub fn row(
    name: &'static str,
    row: usize,
    columns: &[ColumnSpec],
    cells: Vec<Cell>,
) -> Stateful<Div> {
    let total = total_weight(columns);
    div()
        .id((name, row))
        .w_full()
        .h(theme::LIST_ROW_HEIGHT)
        .px_2()
        .flex()
        .items_center()
        .border_b_1()
        .border_color(theme::grid_line())
        .children(cells.into_iter().zip(columns).enumerate().map(
            |(column, (cell, (_, weight)))| {
                div()
                    .id(SharedString::from(format!("{name}-{row}-{column}")))
                    .w(relative(weight / total))
                    .min_w_0()
                    .pr_2()
                    .truncate()
                    .when(cell.align_right, |cell| cell.flex().justify_end())
                    .when_some(cell.tooltip, |element, tooltip| {
                        element.tooltip(text_tooltip(tooltip))
                    })
                    .child(cell.text)
            },
        ))
}

/// 여러 줄 텍스트 툴팁을 만드는 함수. element의 `tooltip`에 넘긴다.
pub fn text_tooltip(text: SharedString) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    move |_, cx| cx.new(|_| TextTooltip(text.clone())).into()
}

struct TextTooltip(SharedString);

impl Render for TextTooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .max_w(px(480.))
            .px_2()
            .py_1()
            .rounded_md()
            .bg(theme::surface())
            .border_1()
            .border_color(theme::border())
            .text_xs()
            .text_color(theme::text())
            .font_family(theme::UI_FONT)
            .whitespace_normal()
            .children(self.0.split('\n').map(|line| div().child(line.to_string())))
    }
}
