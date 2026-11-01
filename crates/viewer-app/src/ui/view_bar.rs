//! Filter and sort bar above the grid. Shows the row view conditions as query text: condition chips joined by
//! operator boxes such as `and ▾`, which open the choice of `and` and `or`, with the conditions of one column joined
//! by `or` in parentheses. Also shows computation progress and cancel, errors and notices.
//!
//! Dragging from one chip's text across others sweeps a range of conditions, in reading order across wrapped lines.
//! Releasing over the conditions makes the range one set of parentheses.

use std::rc::Rc;

use gpui::{
    AnyElement, App, ClickEvent, Context, FontWeight, Pixels, Point, SharedString, Window, div,
    point, prelude::*, px, transparent_black,
};
use viewer_core::LogicOp;

use viewer_ui::theme;
use viewer_ui::widgets::small_button;

#[derive(Clone, Debug)]
pub enum ViewBarAction {
    /// Removes condition `ix` of clause `clause`.
    RemoveFilter {
        clause: usize,
        ix: usize,
    },
    /// Opens the operator choice after condition `ix` of clause `clause`, below the click.
    ChooseJoin {
        clause: usize,
        ix: usize,
        position: Point<Pixels>,
    },
    RemoveSort,
    ClearAll,
    /// Copies the whole query text.
    CopyQuery,
    /// Drops the row view being computed and returns to the previous conditions.
    Cancel,
    /// Recomputes the failed conditions.
    Retry,
    /// A sweep from condition `from` now reaches condition `to`, counted in the order shown.
    Sweep {
        from: usize,
        to: usize,
    },
    /// The sweep was released over the conditions.
    EndSweep,
}

/// Value carried while sweeping: the condition the drag started on.
struct SweepDrag {
    from: usize,
}

/// What follows the cursor while sweeping: nothing, as the chips themselves show the range.
struct SweepGhost;

impl Render for SweepGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// The range being swept, as the bar draws it.
#[derive(Clone, Copy, Debug)]
pub struct SweepRange {
    pub from: usize,
    pub to: usize,
    /// Whether releasing would join the range: it covers two or more conditions on one column.
    pub joinable: bool,
}

impl SweepRange {
    fn mark(&self, flat: usize) -> Mark {
        if flat < self.from.min(self.to) || flat > self.from.max(self.to) {
            Mark::None
        } else if self.joinable || self.from == self.to {
            Mark::Swept
        } else {
            Mark::Refused
        }
    }
}

/// How a chip shows in a sweep.
#[derive(Clone, Copy, PartialEq)]
enum Mark {
    None,
    Swept,
    /// In a range that cannot be joined.
    Refused,
}

pub type ActionHandler = Rc<dyn Fn(ViewBarAction, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct ViewBar {
    /// The filter's clauses, each the query text of its conditions, such as `status == "활성"`. The conditions of a
    /// clause are joined by `or` and the clauses by `and`.
    pub filter: Vec<Vec<SharedString>>,
    /// Query text of the sort, such as `-created_at`.
    pub sort: Option<SharedString>,
    /// Progress while computing.
    pub pending: Option<f32>,
    pub failed: Option<SharedString>,
    pub notice: Option<SharedString>,
    /// The range being swept, and what releasing it would do, shown in place of notices.
    pub sweep: Option<(SweepRange, SharedString)>,
    pub on_action: ActionHandler,
}

impl ViewBar {
    /// Whether there is anything to show.
    pub fn is_visible(&self) -> bool {
        !self.filter.is_empty()
            || self.sort.is_some()
            || self.pending.is_some()
            || self.failed.is_some()
            || self.notice.is_some()
    }
}

/// Keyword of the query text between chips.
fn keyword(text: &'static str) -> impl IntoElement {
    div()
        .flex_none()
        .text_color(theme::text_muted())
        .child(text)
}

/// A chip with `×`. A filter chip has `sweep`: its place in the order shown, where a sweep can start from its text
/// and reach it, and how it shows in the current sweep.
fn chip(
    id: SharedString,
    label: SharedString,
    action: ViewBarAction,
    on_action: ActionHandler,
    sweep: Option<(usize, Mark)>,
) -> impl IntoElement {
    let text = div().min_w_0().truncate().child(label);
    let text = match sweep {
        Some((flat, _)) => {
            let on_action = on_action.clone();
            text.id(SharedString::from(format!("{id}-text")))
                .on_drag(SweepDrag { from: flat }, move |drag, _, window, cx| {
                    on_action(
                        ViewBarAction::Sweep {
                            from: drag.from,
                            to: drag.from,
                        },
                        window,
                        cx,
                    );
                    cx.new(|_| SweepGhost)
                })
                .into_any_element()
        }
        None => text.into_any_element(),
    };
    let mark = sweep.map_or(Mark::None, |(_, mark)| mark);
    div()
        .flex_none()
        .max_w(px(260.))
        .pl_2()
        .flex()
        .items_center()
        .gap_1()
        .rounded_sm()
        .border_1()
        .map(|chip| match mark {
            Mark::None => chip.border_color(transparent_black()).bg(theme::button()),
            Mark::Swept => chip.border_color(theme::accent()).bg(theme::selection()),
            Mark::Refused => chip.border_color(theme::error()).bg(theme::button()),
        })
        .when_some(sweep, |chip, (flat, _)| {
            let on_action = on_action.clone();
            chip.on_drag_move::<SweepDrag>(move |event, window, cx| {
                if event.bounds.contains(&event.event.position) {
                    let from = event.drag(cx).from;
                    on_action(ViewBarAction::Sweep { from, to: flat }, window, cx);
                }
            })
        })
        .child(text)
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
                .on_click(move |_: &ClickEvent, window, cx| on_action(action.clone(), window, cx)),
        )
}

/// An operator between chips, drawn as a small box with `▾`. Clicking it opens the operator choice just below.
fn operator(
    id: SharedString,
    op: LogicOp,
    choose: impl Fn(Point<Pixels>) -> ViewBarAction + 'static,
    on_action: ActionHandler,
) -> impl IntoElement {
    div()
        .id(id)
        .flex_none()
        .flex()
        .items_center()
        .gap(px(3.))
        .px_1()
        .rounded_sm()
        .border_1()
        .border_color(theme::border())
        .cursor_pointer()
        .hover(|style| style.bg(theme::button_hover()))
        .child(op.text())
        .child(div().text_color(theme::text_muted()).child("▾"))
        .on_click(move |event: &ClickEvent, window, cx| {
            let at = event.position();
            on_action(choose(point(at.x - px(12.), at.y + px(12.))), window, cx)
        })
}

/// A parenthesis around conditions joined by `or`.
fn paren(text: &'static str) -> AnyElement {
    div()
        .flex_none()
        .text_color(theme::accent())
        .font_weight(FontWeight::BOLD)
        .child(text)
        .into_any_element()
}

/// Appends the filter's chips, joined by operator boxes, to `out`. The pieces are kept flat so that a long filter
/// wraps anywhere, inside parentheses too.
fn push_filter(
    out: &mut Vec<AnyElement>,
    clauses: Vec<Vec<SharedString>>,
    sweep: Option<SweepRange>,
    on_action: &ActionHandler,
) {
    let count = clauses.len();
    let mut flat = 0;
    for (clause, labels) in clauses.into_iter().enumerate() {
        let len = labels.len();
        let join = |ix: usize, op: LogicOp| {
            operator(
                format!("view-join-{clause}-{ix}").into(),
                op,
                move |position| ViewBarAction::ChooseJoin {
                    clause,
                    ix,
                    position,
                },
                on_action.clone(),
            )
            .into_any_element()
        };
        if len > 1 {
            out.push(paren("("));
        }
        for (ix, label) in labels.into_iter().enumerate() {
            let mark = sweep.map_or(Mark::None, |sweep| sweep.mark(flat));
            out.push(
                chip(
                    format!("view-filter-{clause}-{ix}").into(),
                    label,
                    ViewBarAction::RemoveFilter { clause, ix },
                    on_action.clone(),
                    Some((flat, mark)),
                )
                .into_any_element(),
            );
            flat += 1;
            if ix + 1 < len {
                out.push(join(ix, LogicOp::Or));
            }
        }
        if len > 1 {
            out.push(paren(")"));
        }
        if clause + 1 < count {
            out.push(join(len - 1, LogicOp::And));
        }
    }
}

impl RenderOnce for ViewBar {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let on_action = self.on_action.clone();
        let button = move |id: &'static str, label: &'static str, action: ViewBarAction| {
            let on_action = on_action.clone();
            small_button(id, label)
                .on_click(move |_: &ClickEvent, window, cx| on_action(action.clone(), window, cx))
        };
        let has_conditions = !self.filter.is_empty() || self.sort.is_some();
        let progress = self
            .pending
            .map(|progress| SharedString::from(format!("Applying {:.0}%", progress * 100.)));
        // Errors and notices get a line of their own under the conditions, so long messages such as query errors
        // are read in full.
        let message = match (&self.sweep, &self.pending, &self.failed, &self.notice) {
            (Some((range, hint)), ..) => Some((
                hint.clone(),
                if range.joinable || range.from == range.to {
                    theme::text_muted()
                } else {
                    theme::error()
                },
            )),
            (None, None, Some(failed), _) => Some((failed.clone(), theme::error())),
            (None, None, None, Some(notice)) => Some((notice.clone(), theme::text_muted())),
            _ => None,
        };
        let sweep = self.sweep.as_ref().map(|(range, _)| *range);
        let on_action = self.on_action.clone();
        let conditions = (has_conditions || progress.is_some()).then(|| {
            div()
                .min_h(theme::metrics().panel_header_height + px(4.))
                .px_2()
                .py_1()
                .flex()
                .items_center()
                .gap_2()
                // The bar reads as the query: `filter a and ▾ ( b or ▾ c ) sort -d`.
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_1()
                        .when(!self.filter.is_empty(), |row| {
                            let mut pieces = Vec::new();
                            push_filter(&mut pieces, self.filter, sweep, &on_action);
                            row.child(keyword("filter")).children(pieces)
                        })
                        .when_some(self.sort, |row, sort| {
                            row.child(keyword("sort")).child(chip(
                                "view-sort".into(),
                                sort,
                                ViewBarAction::RemoveSort,
                                on_action.clone(),
                                None,
                            ))
                        })
                        // Releasing anywhere over the conditions ends the sweep; elsewhere it is dropped, so
                        // leaving the conditions shrinks the range back to where it started.
                        .on_drop::<SweepDrag>({
                            let on_action = on_action.clone();
                            move |_, window, cx| on_action(ViewBarAction::EndSweep, window, cx)
                        })
                        .on_drag_move::<SweepDrag>({
                            let on_action = on_action.clone();
                            move |event, window, cx| {
                                if !event.bounds.contains(&event.event.position) {
                                    let from = event.drag(cx).from;
                                    on_action(ViewBarAction::Sweep { from, to: from }, window, cx);
                                }
                            }
                        }),
                )
                .children(progress.map(|progress| {
                    div()
                        .flex_none()
                        .text_color(theme::text_muted())
                        .child(progress)
                }))
                .when(self.pending.is_some(), |bar| {
                    bar.child(button("view-cancel", "Cancel", ViewBarAction::Cancel))
                })
                .when(has_conditions, |bar| {
                    bar.child(button("view-copy", "Copy Query", ViewBarAction::CopyQuery))
                        .child(button("view-clear", "Clear All", ViewBarAction::ClearAll))
                })
        });
        let retry = self.pending.is_none() && self.failed.is_some();
        div()
            .flex_none()
            .flex()
            .flex_col()
            .bg(theme::surface())
            .border_b_1()
            .border_color(theme::border())
            .text_xs()
            .children(conditions)
            .children(message.map(|(text, color)| {
                div()
                    .min_h(theme::metrics().panel_header_height)
                    .px_2()
                    .py_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().min_w_0().text_color(color).child(text))
                    .when(retry, |row| {
                        row.child(button("view-retry", "Retry", ViewBarAction::Retry))
                    })
            }))
    }
}
