//! Filter popover opened from a column header's filter button. Picks an operator and value, then applies or removes the filter.

use arrow_schema::SchemaRef;
use gpui::{
    ClickEvent, Context, Entity, EventEmitter, FocusHandle, KeyDownEvent, SharedString,
    Subscription, Window, div, prelude::*, px,
};
use viewer_core::{
    ColumnId, FilterCondition, FilterOp, ValueClass, check_condition, filter_ops, value_class,
};

use viewer_ui::text_input::{TextInput, TextInputEvent};
use viewer_ui::theme;
use viewer_ui::widgets::small_button;

pub enum FilterPopoverEvent {
    Apply(FilterCondition),
    Remove(ColumnId),
    Close,
}

/// Display label of an operator.
pub fn op_label(op: FilterOp) -> &'static str {
    match op {
        FilterOp::Eq => "=",
        FilterOp::NotEq => "≠",
        FilterOp::Contains => "contains",
        FilterOp::Lt => "<",
        FilterOp::LtEq => "≤",
        FilterOp::Gt => ">",
        FilterOp::GtEq => "≥",
        FilterOp::IsNull => "null",
        FilterOp::IsNotNull => "not null",
    }
}

pub struct FilterPopover {
    focus_handle: FocusHandle,
    schema: SchemaRef,
    column: ColumnId,
    ops: &'static [FilterOp],
    op: FilterOp,
    input: Entity<TextInput>,
    error: Option<SharedString>,
    /// The column already has a filter.
    has_condition: bool,
    _subscription: Subscription,
}

impl EventEmitter<FilterPopoverEvent> for FilterPopover {}

impl FilterPopover {
    pub fn new(
        schema: SchemaRef,
        column: ColumnId,
        existing: Option<FilterCondition>,
        cx: &mut Context<Self>,
    ) -> Self {
        let data_type = schema.fields()[column.0].data_type().clone();
        let ops = filter_ops(&data_type);
        let default_op = if value_class(&data_type) == ValueClass::Text {
            FilterOp::Contains
        } else {
            ops[0]
        };
        let op = existing
            .as_ref()
            .map_or(default_op, |condition| condition.op);
        let placeholder = match value_class(&data_type) {
            ValueClass::Binary => "Hex value",
            ValueClass::Scalar => "Value",
            _ => "Text",
        };
        let input = cx.new(|cx| {
            let mut input = TextInput::new(placeholder, cx);
            if let Some(condition) = &existing {
                input.set_text(condition.value.clone(), cx);
            }
            input
        });
        let subscription =
            cx.subscribe(&input, |this, _, event: &TextInputEvent, cx| match event {
                TextInputEvent::Submit { .. } => this.apply(cx),
                TextInputEvent::Cancel => cx.emit(FilterPopoverEvent::Close),
                TextInputEvent::Changed => {
                    if this.error.take().is_some() {
                        cx.notify();
                    }
                }
            });
        Self {
            focus_handle: cx.focus_handle(),
            schema,
            column,
            ops,
            op,
            input,
            error: None,
            has_condition: existing.is_some(),
            _subscription: subscription,
        }
    }

    /// Focuses the input for operators that take a value, or the popover otherwise.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.op.takes_value() {
            self.input
                .update(cx, |input, cx| input.focus_and_select_all(window, cx));
        } else {
            window.focus(&self.focus_handle, cx);
        }
    }

    fn select_op(&mut self, op: FilterOp, window: &mut Window, cx: &mut Context<Self>) {
        self.op = op;
        self.error = None;
        self.focus(window, cx);
        cx.notify();
    }

    /// Validates and applies the condition. Shows the reason if the value cannot be parsed.
    fn apply(&mut self, cx: &mut Context<Self>) {
        let condition = FilterCondition {
            column: self.column,
            op: self.op,
            value: if self.op.takes_value() {
                self.input.read(cx).text().to_string()
            } else {
                String::new()
            },
        };
        match check_condition(&self.schema, &condition) {
            Ok(()) => cx.emit(FilterPopoverEvent::Apply(condition)),
            Err(message) => {
                self.error = Some(message.into());
                cx.notify();
            }
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // Enter and Esc when the popover itself is focused, for operators without a value input. When the input
        // has focus, the input handles them.
        if !self.focus_handle.is_focused(window) {
            return;
        }
        match event.keystroke.key.as_str() {
            "enter" => self.apply(cx),
            "escape" => cx.emit(FilterPopoverEvent::Close),
            _ => {}
        }
    }
}

impl Render for FilterPopover {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let field = &self.schema.fields()[self.column.0];
        let name: SharedString = field.name().replace(['\r', '\n'], "↵").into();
        let type_label: SharedString = field.data_type().to_string().into();
        let ops = self.ops.iter().map(|&op| {
            let selected = op == self.op;
            div()
                .id(SharedString::from(format!("filter-op-{op:?}")))
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
                .child(op_label(op))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.select_op(op, window, cx)
                }))
        });
        div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(FilterPopoverEvent::Close)))
            .occlude()
            .w(theme::FILTER_POPOVER_WIDTH)
            .p_2()
            .flex()
            .flex_col()
            .gap_2()
            .rounded_md()
            .bg(theme::popover())
            .border_1()
            .border_color(theme::border())
            .shadow_lg()
            .text_xs()
            .text_color(theme::text())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(name),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme::text_muted())
                            .child(type_label),
                    ),
            )
            .child(div().flex().flex_wrap().gap_1().children(ops))
            .when(self.op.takes_value(), |popover| {
                popover.child(self.input.clone())
            })
            .children(
                self.error
                    .clone()
                    .map(|error| div().text_color(theme::error()).child(error)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(self.has_condition, |row| {
                        row.child(small_button("filter-remove", "Remove Filter").on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| {
                                cx.emit(FilterPopoverEvent::Remove(this.column))
                            }),
                        ))
                    })
                    .child(div().flex_1())
                    .child(
                        small_button("filter-apply", "Apply")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.apply(cx))),
                    ),
            )
    }
}
