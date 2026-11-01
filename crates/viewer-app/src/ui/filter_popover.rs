//! 컬럼 헤더의 필터 버튼으로 여는 필터 창. 연산자와 값을 고르고 적용하거나 해제한다.

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

/// 연산자의 화면 표시.
pub fn op_label(op: FilterOp) -> &'static str {
    match op {
        FilterOp::Eq => "=",
        FilterOp::NotEq => "≠",
        FilterOp::Contains => "포함",
        FilterOp::Lt => "<",
        FilterOp::LtEq => "≤",
        FilterOp::Gt => ">",
        FilterOp::GtEq => "≥",
        FilterOp::IsNull => "null",
        FilterOp::IsNotNull => "null 아님",
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
    /// 이 컬럼에 이미 필터가 있다.
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
            ValueClass::Binary => "16진수 값",
            ValueClass::Scalar => "값",
            _ => "텍스트",
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

    /// 값 입력이 필요한 연산자면 입력에, 아니면 창에 포커스를 둔다.
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

    /// 조건을 확인해 적용한다. 값을 해석할 수 없으면 원인을 표시한다.
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
        // 값 입력이 없는 연산자에서 창 자체에 포커스가 있을 때의 Enter·Esc. 입력에 포커스가
        // 있으면 입력이 처리한다.
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
                        row.child(
                            small_button("filter-remove", "필터 해제").on_click(cx.listener(
                                |this, _: &ClickEvent, _, cx| {
                                    cx.emit(FilterPopoverEvent::Remove(this.column))
                                },
                            )),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        small_button("filter-apply", "적용")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.apply(cx))),
                    ),
            )
    }
}
