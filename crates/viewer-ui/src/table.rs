//! Header and rows of a simple table whose column widths are split by ratio. Used as `uniform_list` rows.
//!
//! Content truncated in narrow cells is shown in full as a tooltip.

use gpui::{
    AnyView, App, Context, Div, SharedString, Stateful, Window, div, prelude::*, px, relative,
};

use crate::theme;

/// Column name and width ratio.
pub type ColumnSpec = (&'static str, f32);

/// One table cell.
#[derive(Clone, Debug, Default)]
pub struct Cell {
    pub text: SharedString,
    /// Content shown on hover. Multiple lines are separated by newlines.
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

    /// Right-aligned, like numbers.
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

/// Table header row with every column left-aligned.
pub fn header(columns: &[ColumnSpec]) -> Div {
    header_aligned(columns, |_| false)
}

/// Table header row. Columns where `align_right(column index)` is true are right-aligned, so numeric
/// column headers sit on the same side as their values.
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

/// One table row. Element ids for the row and cells are built from `name` and `row`.
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

/// Builds a multi-line text tooltip. Pass it to an element's `tooltip`.
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
