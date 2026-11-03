//! Value detail view. Shows the raw value of the selected cell without truncation.
//!
//! Nested values appear as an expandable value tree, and the selected node's value as the body. The body draws
//! only visible display lines. Strings wrap at the panel width; Binary is shown as a hex dump of 16 bytes
//! per line.

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
use crate::value_detail::{ValueBody, ValueDetail, ValueHeader, ValueNode};
use viewer_ui::theme;

use super::style;

actions!(value, [CopyValue]);

/// Nodes with more children than this group them into range nodes of this size.
pub const GROUP_SIZE: usize = 1_000;
/// Bytes per hex dump line.
const HEX_ROW_BYTES: usize = 16;

pub enum ValueEvent {
    /// Requests the children of a node at positions `range`.
    Expand {
        path: Vec<usize>,
        range: Range<usize>,
    },
    /// Requests the full raw value of a node.
    Select(Vec<usize>),
    /// Copies the raw value of a node.
    Copy(Vec<usize>),
    Retry,
}

/// Splits raw text into byte ranges of display lines fitted to the panel width.
///
/// Breaks lines at newlines (`\n`, `\r\n`, `\r`), and wraps before the display width exceeds `columns`.
/// Wide characters count as 2 cells; control characters as one display symbol cell.
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

/// Replaces tabs and control characters of a display line with display symbols. The raw value is unchanged.
fn display_line(text: &str) -> String {
    text.chars()
        .map(|ch| match ch {
            '\t' => '⇥',
            ch if ch.is_control() => control_picture(ch),
            ch => ch,
        })
        .collect()
}

/// One hex dump line: offset, 16 bytes in hexadecimal, and ASCII.
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
    /// Parent node path and range start position.
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
        /// Display line index. `None` while being built.
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
    /// Children per `(parent path, range start)`. `None` while requested.
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
    /// Display width for wrapping the body. 0 before the panel is first drawn.
    wrap_columns: usize,
    /// Incremented whenever the body or wrap width changes. Used to drop stale line index results.
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

    /// Shows the root information of the selected cell. For nested values, expands the root and requests its children.
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

    /// Builds the display line index of a string body in a background task.
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
            .h(theme::metrics().list_row_height)
            .pr(style::TREE_ROW_PADDING)
            .flex()
            .items_center()
            .gap_1();
        let indent = |depth: usize| style::TREE_INDENT * depth as f32 + style::TREE_ROW_PADDING;
        let chevron = |expandable: bool, expanded: bool| {
            div()
                .flex_none()
                .w(style::TREE_CHEVRON_WIDTH)
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
                    CellKind::Null => theme::missing(),
                    CellKind::Summary => style::summary_text(),
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
                .child("Reading…"),
        }
    }

    fn render_body(&self, cx: &mut Context<Self>) -> Div {
        let ValueState::Loaded(loaded) = &self.state else {
            return div();
        };
        // Measure the body width to set the wrapping display width.
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
                let usable = bounds.size.width - style::GRID_CELL_PADDING * 2.;
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
                                .h(style::VALUE_LINE_HEIGHT)
                                .px(style::GRID_CELL_PADDING)
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
                .child("Reading…")
                .into_any_element(),
            Body::Null => div()
                .p_2()
                .italic()
                .text_color(theme::missing())
                .child("null")
                .into_any_element(),
            Body::Summary(text) => div()
                .p_2()
                .text_color(style::summary_text())
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
            .font_family(theme::mono_font())
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
            .child(div().truncate().child(match header.row {
                Some(row) => SharedString::from(format!("Row {} · {}", row + 1, header.column)),
                None => header.column.clone(),
            }))
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
            ValueState::Empty => container.child(message("Nothing selected.".into())),
            ValueState::Loading => container.child(message("Reading…".into())),
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
                            .child("Retry")
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
        // Hangul is two cells wide.
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
