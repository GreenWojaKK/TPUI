//! 값 상세 보기. 선택 셀의 원본 값을 자르지 않고 보여준다.
//!
//! 중첩 값은 펼칠 수 있는 값 트리로, 선택한 노드의 값은 본문으로 표시한다. 본문은 표시 줄 단위로
//! 보이는 줄만 그린다. 문자열은 패널 폭에서 자동 줄바꿈하고, Binary는 16 byte마다 한 줄인
//! hex dump로 표시한다.

use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::ops::Range;
use std::sync::Arc;

use gpui::{
    ClickEvent, Context, Div, EventEmitter, FocusHandle, MouseButton, MouseDownEvent, SharedString,
    Stateful, Window, actions, canvas, div, prelude::*, relative, uniform_list,
};
use unicode_width::UnicodeWidthChar;

use crate::cell_preview::{CellKind, control_picture};
use crate::ui::theme;
use crate::value_detail::{ValueBody, ValueDetail, ValueHeader, ValueNode};

actions!(value, [CopyValue]);

/// 자식이 이보다 많은 노드는 자식을 이 개수 단위의 구간 노드로 묶는다.
pub const GROUP_SIZE: usize = 1_000;
/// hex dump 한 줄의 byte 수.
const HEX_ROW_BYTES: usize = 16;

pub enum ValueEvent {
    /// 노드의 `range` 위치 자식을 요청한다.
    Expand {
        path: Vec<usize>,
        range: Range<usize>,
    },
    /// 노드의 원본 값 전체를 요청한다.
    Select(Vec<usize>),
    /// 노드의 원본 값을 복사한다.
    Copy(Vec<usize>),
    Retry,
}

/// 원본 텍스트를 패널 폭에 맞춘 표시 줄의 byte 범위로 나눈다.
///
/// 줄바꿈(`\n`, `\r\n`, `\r`)에서 줄을 나누고, 표시 폭이 `columns`를 넘으면 그 앞에서 자동
/// 줄바꿈한다. 넓은 문자는 2칸, 제어 문자는 표시 기호 1칸으로 센다.
pub fn wrap_lines(text: &str, columns: usize) -> Vec<Range<usize>> {
    let columns = columns.max(1);
    let mut lines = Vec::new();
    let mut start = 0;
    let mut width = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((ix, ch)) = chars.next() {
        match ch {
            '\n' => {
                lines.push(start..ix);
                start = ix + 1;
                width = 0;
            }
            '\r' => {
                lines.push(start..ix);
                start = ix + 1;
                if let Some((next, '\n')) = chars.peek().copied() {
                    chars.next();
                    start = next + 1;
                }
                width = 0;
            }
            ch => {
                let ch_width = if ch.is_control() {
                    1
                } else {
                    ch.width().unwrap_or(1)
                };
                if width > 0 && width + ch_width > columns {
                    lines.push(start..ix);
                    start = ix;
                    width = 0;
                }
                width += ch_width;
            }
        }
    }
    lines.push(start..text.len());
    lines
}

/// 표시 줄의 탭과 제어 문자를 표시 기호로 바꾼다. 원본 값은 바꾸지 않는다.
fn display_line(text: &str) -> String {
    text.chars()
        .map(|ch| match ch {
            '\t' => '⇥',
            ch if ch.is_control() => control_picture(ch),
            ch => ch,
        })
        .collect()
}

/// hex dump 한 줄. offset, 16 byte의 16진수, ASCII를 표시한다.
fn hex_line(bytes: &[u8], line: usize) -> String {
    let start = line * HEX_ROW_BYTES;
    let chunk = &bytes[start..(start + HEX_ROW_BYTES).min(bytes.len())];
    let mut text = format!("{start:08x}  ");
    for ix in 0..HEX_ROW_BYTES {
        match chunk.get(ix) {
            Some(byte) => {
                let _ = write!(text, "{byte:02x} ");
            }
            None => text.push_str("   "),
        }
        if ix == 7 {
            text.push(' ');
        }
    }
    text.push(' ');
    text.extend(chunk.iter().map(|&byte| {
        if byte.is_ascii_graphic() || byte == b' ' {
            byte as char
        } else {
            '.'
        }
    }));
    text
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum TreeKey {
    Node(Vec<usize>),
    /// 부모 노드 경로와 구간 시작 위치.
    Group(Vec<usize>, usize),
}

enum TreeRow {
    Node {
        depth: usize,
        path: Vec<usize>,
        node: ValueNode,
        expanded: bool,
    },
    Group {
        depth: usize,
        parent: Vec<usize>,
        range: Range<usize>,
        expanded: bool,
    },
    Loading {
        depth: usize,
    },
}

enum Body {
    Loading,
    Null,
    Summary(SharedString),
    Unsupported(SharedString),
    Bytes(Arc<[u8]>),
    Text {
        text: SharedString,
        /// 표시 줄 인덱스. 만드는 동안은 `None`이다.
        lines: Option<Arc<Vec<Range<usize>>>>,
    },
}

impl From<ValueBody> for Body {
    fn from(body: ValueBody) -> Self {
        match body {
            ValueBody::Null => Body::Null,
            ValueBody::Text(text) => Body::Text { text, lines: None },
            ValueBody::Bytes(bytes) => Body::Bytes(bytes),
            ValueBody::Summary(text) => Body::Summary(text),
            ValueBody::Unsupported(text) => Body::Unsupported(text),
        }
    }
}

struct Loaded {
    header: ValueHeader,
    root: ValueNode,
    /// `(부모 경로, 구간 시작)`별 자식. 요청 중이면 `None`이다.
    children: HashMap<(Vec<usize>, usize), Option<Vec<ValueNode>>>,
    expanded: HashSet<TreeKey>,
    selected: Vec<usize>,
    body: Body,
    rows: Vec<TreeRow>,
}

impl Loaded {
    fn rebuild_rows(&mut self) {
        let mut rows = Vec::new();
        self.push_node(&mut rows, Vec::new(), self.root.clone(), 0);
        self.rows = rows;
    }

    fn push_node(&self, rows: &mut Vec<TreeRow>, path: Vec<usize>, node: ValueNode, depth: usize) {
        let count = node.child_count;
        let expanded = count > 0 && self.expanded.contains(&TreeKey::Node(path.clone()));
        rows.push(TreeRow::Node {
            depth,
            path: path.clone(),
            node,
            expanded,
        });
        if !expanded {
            return;
        }
        if count <= GROUP_SIZE {
            self.push_children(rows, &path, 0, depth + 1);
            return;
        }
        for start in (0..count).step_by(GROUP_SIZE) {
            let expanded = self.expanded.contains(&TreeKey::Group(path.clone(), start));
            rows.push(TreeRow::Group {
                depth: depth + 1,
                parent: path.clone(),
                range: start..(start + GROUP_SIZE).min(count),
                expanded,
            });
            if expanded {
                self.push_children(rows, &path, start, depth + 2);
            }
        }
    }

    fn push_children(&self, rows: &mut Vec<TreeRow>, parent: &[usize], start: usize, depth: usize) {
        match self.children.get(&(parent.to_vec(), start)) {
            Some(Some(children)) => {
                for (offset, child) in children.iter().enumerate() {
                    let mut path = parent.to_vec();
                    path.push(start + offset);
                    self.push_node(rows, path, child.clone(), depth);
                }
            }
            _ => rows.push(TreeRow::Loading { depth }),
        }
    }
}

enum ValueState {
    Empty,
    Loading,
    Failed(SharedString),
    Loaded(Box<Loaded>),
}

pub struct ValueView {
    focus_handle: FocusHandle,
    state: ValueState,
    /// 본문 자동 줄바꿈의 표시 폭. 패널을 처음 그리기 전에는 0이다.
    wrap_columns: usize,
    /// 본문이나 줄바꿈 폭이 바뀔 때마다 증가한다. 이전 줄 인덱스 결과를 버리는 데 쓴다.
    wrap_generation: u64,
}

impl EventEmitter<ValueEvent> for ValueView {}

impl ValueView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            state: ValueState::Empty,
            wrap_columns: 0,
            wrap_generation: 0,
        }
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.state = ValueState::Empty;
        cx.notify();
    }

    pub fn set_loading(&mut self, cx: &mut Context<Self>) {
        self.state = ValueState::Loading;
        cx.notify();
    }

    pub fn set_failed(&mut self, message: SharedString, cx: &mut Context<Self>) {
        self.state = ValueState::Failed(message);
        cx.notify();
    }

    /// 선택 셀의 루트 정보를 표시한다. 중첩 값이면 루트를 펼쳐 자식을 요청한다.
    pub fn set_detail(&mut self, detail: ValueDetail, cx: &mut Context<Self>) {
        let mut loaded = Loaded {
            header: detail.header,
            root: detail.root,
            children: HashMap::new(),
            expanded: HashSet::new(),
            selected: Vec::new(),
            body: detail.body.into(),
            rows: Vec::new(),
        };
        loaded.rebuild_rows();
        self.state = ValueState::Loaded(Box::new(loaded));
        if self.root_child_count() > 0 {
            self.toggle_node(Vec::new(), cx);
        }
        self.start_wrap(cx);
        cx.notify();
    }

    pub fn set_children(
        &mut self,
        path: Vec<usize>,
        start: usize,
        children: Vec<ValueNode>,
        cx: &mut Context<Self>,
    ) {
        if let ValueState::Loaded(loaded) = &mut self.state {
            loaded.children.insert((path, start), Some(children));
            loaded.rebuild_rows();
            cx.notify();
        }
    }

    pub fn set_body(&mut self, path: Vec<usize>, body: ValueBody, cx: &mut Context<Self>) {
        if let ValueState::Loaded(loaded) = &mut self.state
            && loaded.selected == path
        {
            loaded.body = body.into();
            self.start_wrap(cx);
            cx.notify();
        }
    }

    fn root_child_count(&self) -> usize {
        match &self.state {
            ValueState::Loaded(loaded) => loaded.root.child_count,
            _ => 0,
        }
    }

    fn set_wrap_columns(&mut self, columns: usize, cx: &mut Context<Self>) {
        if self.wrap_columns != columns {
            self.wrap_columns = columns;
            self.start_wrap(cx);
        }
    }

    /// 문자열 본문의 표시 줄 인덱스를 background task에서 만든다.
    fn start_wrap(&mut self, cx: &mut Context<Self>) {
        self.wrap_generation += 1;
        let ValueState::Loaded(loaded) = &mut self.state else {
            return;
        };
        let Body::Text { text, lines } = &mut loaded.body else {
            return;
        };
        *lines = None;
        if self.wrap_columns == 0 {
            return;
        }
        let (text, columns, generation) = (text.clone(), self.wrap_columns, self.wrap_generation);
        cx.spawn(async move |this, cx| {
            let lines = cx
                .background_spawn(async move { wrap_lines(&text, columns) })
                .await;
            this.update(cx, |this, cx| {
                if this.wrap_generation != generation {
                    return;
                }
                if let ValueState::Loaded(loaded) = &mut this.state
                    && let Body::Text { lines: slot, .. } = &mut loaded.body
                {
                    *slot = Some(Arc::new(lines));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn toggle_node(&mut self, path: Vec<usize>, cx: &mut Context<Self>) {
        let ValueState::Loaded(loaded) = &mut self.state else {
            return;
        };
        let key = TreeKey::Node(path.clone());
        if !loaded.expanded.remove(&key) {
            loaded.expanded.insert(key);
            let count = node_child_count(loaded, &path);
            if count <= GROUP_SIZE && !loaded.children.contains_key(&(path.clone(), 0)) {
                loaded.children.insert((path.clone(), 0), None);
                cx.emit(ValueEvent::Expand {
                    path,
                    range: 0..count,
                });
            }
        }
        loaded.rebuild_rows();
        cx.notify();
    }

    fn toggle_group(&mut self, parent: Vec<usize>, range: Range<usize>, cx: &mut Context<Self>) {
        let ValueState::Loaded(loaded) = &mut self.state else {
            return;
        };
        let key = TreeKey::Group(parent.clone(), range.start);
        if !loaded.expanded.remove(&key) {
            loaded.expanded.insert(key);
            if let std::collections::hash_map::Entry::Vacant(entry) =
                loaded.children.entry((parent.clone(), range.start))
            {
                entry.insert(None);
                cx.emit(ValueEvent::Expand {
                    path: parent,
                    range,
                });
            }
        }
        loaded.rebuild_rows();
        cx.notify();
    }

    fn select_node(&mut self, path: Vec<usize>, cx: &mut Context<Self>) {
        let ValueState::Loaded(loaded) = &mut self.state else {
            return;
        };
        if loaded.selected != path {
            loaded.selected = path.clone();
            loaded.body = Body::Loading;
            self.wrap_generation += 1;
            cx.emit(ValueEvent::Select(path));
            cx.notify();
        }
    }

    fn copy(&mut self, _: &CopyValue, _window: &mut Window, cx: &mut Context<Self>) {
        if let ValueState::Loaded(loaded) = &self.state {
            cx.emit(ValueEvent::Copy(loaded.selected.clone()));
        }
    }

    fn render_tree_row(&self, ix: usize, cx: &mut Context<Self>) -> Stateful<Div> {
        let ValueState::Loaded(loaded) = &self.state else {
            return div().id(ix);
        };
        let row = div()
            .id(ix)
            .w_full()
            .h(theme::LIST_ROW_HEIGHT)
            .pr(theme::TREE_ROW_PADDING)
            .flex()
            .items_center()
            .gap_1();
        let indent = |depth: usize| theme::TREE_INDENT * depth as f32 + theme::TREE_ROW_PADDING;
        let chevron = |expandable: bool, expanded: bool| {
            div()
                .flex_none()
                .w(theme::TREE_CHEVRON_WIDTH)
                .text_color(theme::text_muted())
                .child(match (expandable, expanded) {
                    (true, true) => "▾",
                    (true, false) => "▸",
                    _ => "",
                })
        };
        match &loaded.rows[ix] {
            TreeRow::Node {
                depth,
                path,
                node,
                expanded,
            } => {
                let selected = loaded.selected == *path;
                let path = path.clone();
                let expandable = node.child_count > 0;
                let preview_color = match node.preview.kind {
                    CellKind::Null => theme::null_text(),
                    CellKind::Summary => theme::summary_text(),
                    CellKind::Unsupported => theme::text_muted(),
                    CellKind::Text | CellKind::Number => theme::text(),
                };
                row.pl(indent(*depth))
                    .cursor_pointer()
                    .when(selected, |row| row.bg(theme::selection()))
                    .when(!selected, |row| row.hover(|style| style.bg(theme::hover())))
                    .child(chevron(expandable, *expanded))
                    .child(
                        div()
                            .flex_none()
                            .max_w(relative(0.4))
                            .truncate()
                            .text_color(theme::text_muted())
                            .child(node.label.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(preview_color)
                            .child(node.preview.text.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child(node.type_label.clone()),
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.select_node(path.clone(), cx);
                        if expandable {
                            this.toggle_node(path.clone(), cx);
                        }
                    }))
            }
            TreeRow::Group {
                depth,
                parent,
                range,
                expanded,
            } => {
                let (parent, range) = (parent.clone(), range.clone());
                let label = format!("[{} … {}]", range.start, range.end - 1);
                row.pl(indent(*depth))
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::hover()))
                    .text_color(theme::text_muted())
                    .child(chevron(true, *expanded))
                    .child(label)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.toggle_group(parent.clone(), range.clone(), cx)
                    }))
            }
            TreeRow::Loading { depth } => row
                .pl(indent(*depth))
                .text_color(theme::text_muted())
                .child(chevron(false, false))
                .child("읽는 중…"),
        }
    }

    fn render_body(&self, cx: &mut Context<Self>) -> Div {
        let ValueState::Loaded(loaded) = &self.state else {
            return div();
        };
        // 본문 폭을 재서 자동 줄바꿈의 표시 폭을 정한다.
        let view = cx.entity();
        let measure = canvas(
            move |bounds, window, cx| {
                let style = window.text_style();
                let font_size = style.font_size.to_pixels(window.rem_size());
                let run = gpui::TextRun {
                    len: 1,
                    font: style.font(),
                    color: theme::text().into(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let char_width = window
                    .text_system()
                    .shape_line("0".into(), font_size, &[run], None)
                    .width();
                let usable = bounds.size.width - theme::GRID_CELL_PADDING * 2.;
                let columns = ((usable / char_width).floor() as usize).max(8);
                view.update(cx, |view, cx| view.set_wrap_columns(columns, cx));
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();

        let lines = |count: usize, render: Box<dyn Fn(usize) -> String>| {
            uniform_list(
                "value-body-lines",
                count,
                move |range: Range<usize>, _window, _cx| {
                    range
                        .map(|ix| {
                            div()
                                .h(theme::VALUE_LINE_HEIGHT)
                                .px(theme::GRID_CELL_PADDING)
                                .whitespace_nowrap()
                                .child(render(ix))
                        })
                        .collect()
                },
            )
            .size_full()
        };
        let content = match &loaded.body {
            Body::Loading => div()
                .p_2()
                .text_color(theme::text_muted())
                .child("읽는 중…")
                .into_any_element(),
            Body::Null => div()
                .p_2()
                .italic()
                .text_color(theme::null_text())
                .child("null")
                .into_any_element(),
            Body::Summary(text) => div()
                .p_2()
                .text_color(theme::summary_text())
                .child(text.clone())
                .into_any_element(),
            Body::Unsupported(text) => div()
                .p_2()
                .italic()
                .text_color(theme::text_muted())
                .child(text.clone())
                .into_any_element(),
            Body::Bytes(bytes) => {
                let bytes = bytes.clone();
                let count = bytes.len().div_ceil(HEX_ROW_BYTES).max(1);
                lines(count, Box::new(move |ix| hex_line(&bytes, ix))).into_any_element()
            }
            Body::Text { text, lines: index } => match index {
                Some(index) => {
                    let (text, index) = (text.clone(), index.clone());
                    lines(
                        index.len(),
                        Box::new(move |ix| display_line(&text[index[ix].clone()])),
                    )
                    .into_any_element()
                }
                None => div()
                    .p_2()
                    .text_color(theme::text_muted())
                    .child("Loading")
                    .into_any_element(),
            },
        };
        div()
            .flex_1()
            .min_h_0()
            .relative()
            .font_family(theme::MONO_FONT)
            .child(measure)
            .child(content)
    }

    fn render_header(header: &ValueHeader) -> Div {
        div()
            .flex_none()
            .px_2()
            .py_1()
            .flex()
            .flex_col()
            .gap_0p5()
            .border_b_1()
            .border_color(theme::border())
            .child(
                div()
                    .truncate()
                    .child(format!("행 {} · {}", header.row + 1, header.column)),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(div().min_w_0().truncate().child(header.data_type.clone()))
                    .children(header.size.clone()),
            )
    }
}

fn node_child_count(loaded: &Loaded, path: &[usize]) -> usize {
    loaded
        .rows
        .iter()
        .find_map(|row| match row {
            TreeRow::Node {
                path: row_path,
                node,
                ..
            } if row_path == path => Some(node.child_count),
            _ => None,
        })
        .unwrap_or(loaded.root.child_count)
}

impl Render for ValueView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let container = div()
            .size_full()
            .flex()
            .flex_col()
            .track_focus(&self.focus_handle)
            .key_context("ValuePanel")
            .on_action(cx.listener(Self::copy))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    this.focus_handle.focus(window, cx)
                }),
            );
        let message = |text: SharedString| {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .text_color(theme::text_muted())
                .child(text)
        };
        match &self.state {
            ValueState::Empty => container.child(message("선택한 셀이 없습니다.".into())),
            ValueState::Loading => container.child(message("읽는 중…".into())),
            ValueState::Failed(error) => container.child(
                message(error.clone())
                    .flex_col()
                    .gap_2()
                    .text_color(theme::error())
                    .child(
                        div()
                            .id("value-retry")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .bg(theme::button())
                            .hover(|style| style.bg(theme::button_hover()))
                            .cursor_pointer()
                            .text_color(theme::text())
                            .child("다시 시도")
                            .on_click(
                                cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(ValueEvent::Retry)),
                            ),
                    ),
            ),
            ValueState::Loaded(loaded) => {
                let tree = (loaded.root.child_count > 0).then(|| {
                    div()
                        .flex_none()
                        .h(relative(0.45))
                        .border_b_1()
                        .border_color(theme::border())
                        .child(
                            uniform_list(
                                "value-tree",
                                loaded.rows.len(),
                                cx.processor(|this, range: Range<usize>, _window, cx| {
                                    range.map(|ix| this.render_tree_row(ix, cx)).collect()
                                }),
                            )
                            .size_full(),
                        )
                });
                container
                    .child(Self::render_header(&loaded.header))
                    .children(tree)
                    .child(self.render_body(cx))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines_of(text: &str, columns: usize) -> Vec<&str> {
        wrap_lines(text, columns)
            .into_iter()
            .map(|range| &text[range])
            .collect()
    }

    #[test]
    fn splits_on_every_line_break_kind() {
        assert_eq!(lines_of("a\nb\r\nc\rd", 80), vec!["a", "b", "c", "d"]);
        assert_eq!(lines_of("end\n", 80), vec!["end", ""]);
        assert_eq!(lines_of("", 80), vec![""]);
    }

    #[test]
    fn wraps_by_display_width() {
        assert_eq!(lines_of("abcdefg", 3), vec!["abc", "def", "g"]);
        // 한글은 2칸이다.
        assert_eq!(lines_of("가나다라", 5), vec!["가나", "다라"]);
        assert_eq!(lines_of(r"C:\a\b", 3), vec![r"C:\", r"a\b"]);
    }

    #[test]
    fn keeps_every_byte_of_the_original() {
        let text = "줄1\r\n\t탭\u{0}끝".repeat(50);
        let joined: usize = wrap_lines(&text, 7).iter().map(|range| range.len()).sum();
        let breaks = text.matches("\r\n").count() * 2;
        assert_eq!(joined + breaks, text.len());
        assert_eq!(display_line("\t\u{0}"), "⇥␀");
    }

    #[test]
    fn formats_hex_dump_lines() {
        let bytes: Vec<u8> = (0x41..0x41 + 20).collect();
        assert_eq!(
            hex_line(&bytes, 0),
            "00000000  41 42 43 44 45 46 47 48  49 4a 4b 4c 4d 4e 4f 50  ABCDEFGHIJKLMNOP"
        );
        assert!(hex_line(&bytes, 1).starts_with("00000010  51 52 53 54"));
        assert!(hex_line(&bytes, 1).ends_with("QRST"));
    }
}
