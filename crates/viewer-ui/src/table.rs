//! Header and rows of a simple table whose column widths are split by ratio.
//!
//! [`row`] has a fixed height for `uniform_list`, and content truncated in narrow cells is shown in full as a
//! tooltip. [`wrapped_row`] wraps long text at spaces and grows to fit it, for short tables of sentences.

use gpui::{
    AnyView, App, Context, Div, SharedString, Stateful, Window, div, prelude::*, px, relative,
};

use crate::theme;
use crate::wrap::WrappedText;

/// Column name and width ratio. Functions here also take names known only at run time, as
/// `(SharedString, f32)`.
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

fn total_weight<L>(columns: &[(L, f32)]) -> f32 {
    columns.iter().map(|(_, weight)| weight).sum()
}

/// Table header row with every column left-aligned.
pub fn header<L: Clone + Into<SharedString>>(columns: &[(L, f32)]) -> Div {
    header_aligned(columns, |_| false)
}

/// Table header row. Columns where `align_right(column index)` is true are right-aligned, so numeric
/// column headers sit on the same side as their values.
pub fn header_aligned<L: Clone + Into<SharedString>>(
    columns: &[(L, f32)],
    align_right: impl Fn(usize) -> bool,
) -> Div {
    let total = total_weight(columns);
    let metrics = theme::metrics();
    div()
        .flex_none()
        .h(metrics.list_row_height)
        .px(metrics.cell_padding_x)
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
                .pr(metrics.cell_padding_x)
                .truncate()
                .when(align_right(ix), |cell| cell.flex().justify_end())
                .child(label.clone().into())
        }))
}

/// One table row. Element ids for the row and cells are built from `name` and `row`.
pub fn row<L>(
    name: &'static str,
    row: usize,
    columns: &[(L, f32)],
    cells: Vec<Cell>,
) -> Stateful<Div> {
    let total = total_weight(columns);
    let metrics = theme::metrics();
    div()
        .id((name, row))
        .w_full()
        .h(metrics.list_row_height)
        .px(metrics.cell_padding_x)
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
                    .pr(metrics.cell_padding_x)
                    .truncate()
                    .when(cell.align_right, |cell| cell.flex().justify_end())
                    .when_some(cell.tooltip, |element, tooltip| {
                        element.tooltip(text_tooltip(tooltip))
                    })
                    .child(cell.text)
            },
        ))
}

/// One table row whose cells wrap at spaces, so the row is as tall as its longest cell. Rows differ in height,
/// so they are laid out in an ordinary column rather than a `uniform_list`. Element ids are built as in [`row`].
pub fn wrapped_row<L>(
    name: &'static str,
    row: usize,
    columns: &[(L, f32)],
    cells: Vec<Cell>,
) -> Stateful<Div> {
    let total = total_weight(columns);
    let metrics = theme::metrics();
    div()
        .id((name, row))
        .w_full()
        .min_h(metrics.list_row_height)
        .px(metrics.cell_padding_x)
        .py(metrics.wrapped_row_padding_y)
        .flex()
        .items_start()
        .border_b_1()
        .border_color(theme::grid_line())
        .children(cells.into_iter().zip(columns).enumerate().map(
            |(column, (cell, (_, weight)))| {
                let text = WrappedText::new(cell.text);
                div()
                    .id(SharedString::from(format!("{name}-{row}-{column}")))
                    .w(relative(weight / total))
                    .min_w_0()
                    .pr(metrics.cell_padding_x)
                    .child(if cell.align_right { text.right() } else { text })
                    .when_some(cell.tooltip, |element, tooltip| {
                        element.tooltip(text_tooltip(tooltip))
                    })
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
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::border())
            .text_xs()
            .text_color(theme::text())
            .font_family(theme::ui_font())
            .whitespace_normal()
            .children(self.0.split('\n').map(|line| div().child(line.to_string())))
    }
}
