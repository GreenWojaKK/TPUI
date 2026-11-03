//! JSON tree view. Draws only the visible rows of the expanded tree. The selected value's full text goes to the
//! Value panel.

use std::collections::HashSet;
use std::ops::Range;
use std::sync::Arc;

use gpui::{
    ClickEvent, ClipboardItem, Context, Div, EventEmitter, FocusHandle, Hsla, MouseButton,
    MouseDownEvent, Pixels, Point, ScrollStrategy, SharedString, Stateful, UniformListScrollHandle,
    Window, div, point, prelude::*, px, relative, uniform_list,
};
use viewer_core::{JsonDocument, JsonKind, JsonNodeId};
use viewer_ui::format::format_quantity;
use viewer_ui::theme;

use super::doc_scrollbar::{ScrollMetrics, ScrollbarDrag, scrollbar};
use super::explorer::OpenSelected;
use super::grid::{
    MoveDown, MoveLeft, MoveRight, MoveToFirstRow, MoveToLastRow, MoveUp, PageDown, PageUp,
};
use super::style;
use super::value::{CopyValue, GROUP_SIZE};
use crate::cell_preview::{CellKind, CellPreview, STRING_PREVIEW_CHARS, control_picture};
use crate::value_detail::{ValueBody, ValueDetail, ValueHeader, ValueNode};

/// Rows one Page Up or Page Down moves.
const PAGE_ROWS: usize = 20;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TreeKey {
    Node(Vec<usize>),
    /// Parent node path and range start position.
    Group(Vec<usize>, usize),
}

/// What a JSON tab remembers while another tab is shown. Paths stay meaningful when the file is read again.
#[derive(Clone, Debug, PartialEq)]
pub struct JsonPosition {
    pub expanded: HashSet<TreeKey>,
    pub selected: Vec<usize>,
    pub offset: Point<Pixels>,
}

impl Default for JsonPosition {
    fn default() -> Self {
        Self {
            // The root starts open.
            expanded: HashSet::from([TreeKey::Node(Vec::new())]),
            selected: Vec::new(),
            offset: Point::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TreeRow {
    Node {
        depth: usize,
        path: Vec<usize>,
        node: JsonNodeId,
        expanded: bool,
    },
    Group {
        depth: usize,
        parent: Vec<usize>,
        range: Range<usize>,
        expanded: bool,
    },
}

/// The rows of the expanded tree. Nodes with more children than [`GROUP_SIZE`] group them into ranges.
fn tree_rows(doc: &JsonDocument, expanded: &HashSet<TreeKey>) -> Vec<TreeRow> {
    fn push_node(
        doc: &JsonDocument,
        expanded: &HashSet<TreeKey>,
        rows: &mut Vec<TreeRow>,
        path: Vec<usize>,
        node: JsonNodeId,
        depth: usize,
    ) {
        let count = doc.child_count(node);
        let open = count > 0 && expanded.contains(&TreeKey::Node(path.clone()));
        rows.push(TreeRow::Node {
            depth,
            path: path.clone(),
            node,
            expanded: open,
        });
        if !open {
            return;
        }
        let push_children = |rows: &mut Vec<TreeRow>, range: Range<usize>, depth: usize| {
            for ix in range {
                if let Some(child) = doc.child(node, ix) {
                    let mut child_path = path.clone();
                    child_path.push(ix);
                    push_node(doc, expanded, rows, child_path, child, depth);
                }
            }
        };
        if count <= GROUP_SIZE {
            push_children(rows, 0..count, depth + 1);
            return;
        }
        for start in (0..count).step_by(GROUP_SIZE) {
            let range = start..(start + GROUP_SIZE).min(count);
            let open = expanded.contains(&TreeKey::Group(path.clone(), start));
            rows.push(TreeRow::Group {
                depth: depth + 1,
                parent: path.clone(),
                range: range.clone(),
                expanded: open,
            });
            if open {
                push_children(rows, range, depth + 2);
            }
        }
    }
    let mut rows = Vec::new();
    push_node(doc, expanded, &mut rows, Vec::new(), doc.root(), 0);
    rows
}

/// A value's place in the document, as in `$.routes[3]["운행 일자"]`.
fn path_label(doc: &JsonDocument, path: &[usize]) -> String {
    let mut label = String::from("$");
    let mut node = doc.root();
    for &ix in path {
        let Some(child) = doc.child(node, ix) else {
            break;
        };
        match doc.key(child) {
            Some(key) if is_plain_key(&key) => {
                label.push('.');
                label.push_str(&key);
            }
            Some(key) => {
                label.push_str("[\"");
                label.push_str(&key.replace('\\', "\\\\").replace('"', "\\\""));
                label.push_str("\"]");
            }
            None => {
                label.push('[');
                label.push_str(&ix.to_string());
                label.push(']');
            }
        }
        node = child;
    }
    label
}

fn is_plain_key(key: &str) -> bool {
    !key.is_empty()
        && !key.starts_with(|ch: char| ch.is_ascii_digit())
        && key.chars().all(|ch| ch == '_' || ch.is_alphanumeric())
}

/// A long or multi-line text on one line, with control characters as display symbols.
fn one_line(text: &str, more: bool) -> String {
    let mut line: String = text
        .chars()
        .map(|ch| match ch {
            '\t' => '⇥',
            ch if ch.is_control() => control_picture(ch),
            ch => ch,
        })
        .collect();
    if more {
        line.push('…');
    }
    line
}

fn container_summary(doc: &JsonDocument, node: JsonNodeId) -> String {
    let count = doc.child_count(node) as u64;
    match doc.kind(node) {
        JsonKind::Object => format!("{{{}}}", format_quantity(count, "key", "keys")),
        _ => format!("[{}]", format_quantity(count, "item", "items")),
    }
}

/// The value's text on a row, with the kind that decides its color.
fn row_preview(doc: &JsonDocument, node: JsonNodeId) -> (String, JsonKind) {
    let kind = doc.kind(node);
    let text = match kind {
        JsonKind::String => {
            let (text, more) = doc
                .string_preview(node, STRING_PREVIEW_CHARS)
                .unwrap_or_default();
            format!("\"{}\"", one_line(&text, more))
        }
        JsonKind::Array | JsonKind::Object => container_summary(doc, node),
        JsonKind::Null | JsonKind::Bool | JsonKind::Number => doc.raw(node).to_string(),
    };
    (text, kind)
}

fn kind_color(kind: JsonKind) -> Hsla {
    match kind {
        JsonKind::Null => theme::missing().into(),
        JsonKind::Bool | JsonKind::Number => theme::accent().into(),
        JsonKind::String => theme::text().into(),
        JsonKind::Array | JsonKind::Object => style::summary_text().into(),
    }
}

/// What the Value panel shows for a node. Strings are decoded in full; arrays and objects are summarized, and copying
/// them gives their text in the file.
fn value_detail(doc: &JsonDocument, path: &[usize], node: JsonNodeId) -> ValueDetail {
    let kind = doc.kind(node);
    let (body, size) = match kind {
        JsonKind::Null => (ValueBody::Null, None),
        JsonKind::Bool | JsonKind::Number => {
            (ValueBody::Text(doc.raw(node).to_string().into()), None)
        }
        JsonKind::String => {
            let text = doc.text(node);
            let size = format!(
                "{} · {}",
                format_quantity(text.len() as u64, "byte", "bytes"),
                format_quantity(text.chars().count() as u64, "char", "chars")
            );
            (ValueBody::Text(text.into_owned().into()), Some(size))
        }
        JsonKind::Array | JsonKind::Object => {
            let summary = container_summary(doc, node);
            let inner = summary[1..summary.len() - 1].to_string();
            (ValueBody::Summary(summary.into()), Some(inner))
        }
    };
    let label: SharedString = path_label(doc, path).into();
    let (preview, _) = row_preview(doc, node);
    ValueDetail {
        header: ValueHeader {
            row: None,
            column: label.clone(),
            data_type: kind.name().into(),
            size: size.map(Into::into),
        },
        root: ValueNode {
            label,
            preview: CellPreview::new(preview, CellKind::Text),
            type_label: kind.name().into(),
            child_count: 0,
        },
        body,
    }
}

pub enum JsonEvent {
    /// The selected value changed, or was read again.
    Selected(ValueDetail),
}

pub struct JsonView {
    focus_handle: FocusHandle,
    doc: Arc<JsonDocument>,
    expanded: HashSet<TreeKey>,
    selected: Vec<usize>,
    rows: Vec<TreeRow>,
    scroll: UniformListScrollHandle,
    scrollbar: ScrollbarDrag,
}

impl EventEmitter<JsonEvent> for JsonView {}

impl JsonView {
    pub fn new(doc: Arc<JsonDocument>, position: JsonPosition, cx: &mut Context<Self>) -> Self {
        let scroll = UniformListScrollHandle::new();
        scroll.0.borrow().base_handle.set_offset(position.offset);
        let mut this = Self {
            focus_handle: cx.focus_handle(),
            doc,
            expanded: position.expanded,
            selected: position.selected,
            rows: Vec::new(),
            scroll,
            scrollbar: ScrollbarDrag::default(),
        };
        this.settle_selection();
        this.rows = tree_rows(&this.doc, &this.expanded);
        this
    }

    /// Shows a new version of the file. Expanded values and the selection stay where their paths still lead.
    pub fn set_document(&mut self, doc: Arc<JsonDocument>, cx: &mut Context<Self>) {
        self.doc = doc;
        self.settle_selection();
        self.rows = tree_rows(&self.doc, &self.expanded);
        self.emit_selected(cx);
        cx.notify();
    }

    fn settle_selection(&mut self) {
        if self.doc.node_at(&self.selected).is_none() {
            self.selected.clear();
        }
    }

    pub fn position(&self) -> JsonPosition {
        JsonPosition {
            expanded: self.expanded.clone(),
            selected: self.selected.clone(),
            offset: self.scroll.0.borrow().base_handle.offset(),
        }
    }

    pub fn value_count(&self) -> usize {
        self.doc.value_count()
    }

    pub fn emit_selected(&self, cx: &mut Context<Self>) {
        if let Some(node) = self.doc.node_at(&self.selected) {
            cx.emit(JsonEvent::Selected(value_detail(
                &self.doc,
                &self.selected,
                node,
            )));
        }
    }

    pub fn copy_selected(&self, cx: &mut Context<Self>) {
        if let Some(node) = self.doc.node_at(&self.selected) {
            cx.write_to_clipboard(ClipboardItem::new_string(self.doc.text(node).into_owned()));
        }
    }

    fn select(&mut self, path: Vec<usize>, cx: &mut Context<Self>) {
        if self.selected != path {
            self.selected = path;
            self.emit_selected(cx);
            cx.notify();
        }
    }

    fn toggle(&mut self, key: TreeKey, cx: &mut Context<Self>) {
        if !self.expanded.remove(&key) {
            self.expanded.insert(key);
        }
        self.rows = tree_rows(&self.doc, &self.expanded);
        cx.notify();
    }

    fn selected_row(&self) -> Option<usize> {
        self.rows
            .iter()
            .position(|row| matches!(row, TreeRow::Node { path, .. } if *path == self.selected))
    }

    /// Selects the value `delta` rows away. Range rows are passed over.
    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let current = self.selected_row().unwrap_or(0) as isize;
        let last = self.rows.len() as isize - 1;
        let mut target = (current + delta).clamp(0, last);
        let step = delta.signum();
        while !matches!(self.rows[target as usize], TreeRow::Node { .. }) {
            let next = target + step;
            if !(0..=last).contains(&next) {
                return;
            }
            target = next;
        }
        if let TreeRow::Node { path, .. } = &self.rows[target as usize] {
            self.select(path.clone(), cx);
        }
        self.scroll
            .scroll_to_item(target as usize, ScrollStrategy::Nearest);
    }

    /// Left closes an open value, or else moves to its parent.
    fn move_left(&mut self, cx: &mut Context<Self>) {
        let Some(row) = self.selected_row() else {
            return;
        };
        if let TreeRow::Node {
            path,
            expanded: true,
            ..
        } = &self.rows[row]
        {
            let key = TreeKey::Node(path.clone());
            self.toggle(key, cx);
        } else if !self.selected.is_empty() {
            let mut parent = self.selected.clone();
            parent.pop();
            self.select(parent, cx);
            if let Some(row) = self.selected_row() {
                self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
            }
        }
    }

    /// Right opens a closed value, or else moves to its first child.
    fn move_right(&mut self, cx: &mut Context<Self>) {
        let Some(row) = self.selected_row() else {
            return;
        };
        if let TreeRow::Node {
            path,
            node,
            expanded,
            ..
        } = &self.rows[row]
        {
            if self.doc.child_count(*node) == 0 {
                return;
            }
            if *expanded {
                self.move_selection(1, cx);
            } else {
                let key = TreeKey::Node(path.clone());
                self.toggle(key, cx);
            }
        }
    }

    fn toggle_selected(&mut self, _: &OpenSelected, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(row) = self.selected_row()
            && let TreeRow::Node { path, node, .. } = &self.rows[row]
            && self.doc.child_count(*node) > 0
        {
            let key = TreeKey::Node(path.clone());
            self.toggle(key, cx);
        }
    }

    fn render_row(&self, ix: usize, cx: &mut Context<Self>) -> Stateful<Div> {
        let row = div()
            .id(ix)
            .w_full()
            .h(theme::metrics().list_row_height)
            .pr(style::TREE_ROW_PADDING + style::GRID_SCROLLBAR_SIZE)
            .flex()
            .items_center()
            .gap_1()
            .whitespace_nowrap()
            .cursor_pointer();
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
        match &self.rows[ix] {
            TreeRow::Node {
                depth,
                path,
                node,
                expanded,
            } => {
                let selected = self.selected == *path;
                let expandable = self.doc.child_count(*node) > 0;
                let label: SharedString = match (self.doc.key(*node), path.last()) {
                    (Some(key), _) => one_line(&key, false).into(),
                    (None, Some(position)) => position.to_string().into(),
                    (None, None) => "$".into(),
                };
                let (preview, kind) = row_preview(&self.doc, *node);
                let path = path.clone();
                row.pl(indent(*depth))
                    .when(selected, |row| row.bg(theme::selection()))
                    .when(!selected, |row| row.hover(|style| style.bg(theme::hover())))
                    .child(chevron(expandable, *expanded))
                    .child(
                        div()
                            .flex_none()
                            .max_w(relative(0.4))
                            .truncate()
                            .text_color(theme::text_muted())
                            .child(label),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(kind_color(kind))
                            .child(preview),
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.focus_handle.focus(window, cx);
                        this.select(path.clone(), cx);
                        if expandable {
                            this.toggle(TreeKey::Node(path.clone()), cx);
                        }
                    }))
            }
            TreeRow::Group {
                depth,
                parent,
                range,
                expanded,
            } => {
                let key = TreeKey::Group(parent.clone(), range.start);
                row.pl(indent(*depth))
                    .hover(|style| style.bg(theme::hover()))
                    .text_color(theme::text_muted())
                    .child(chevron(true, *expanded))
                    .child(format!("[{} … {}]", range.start, range.end - 1))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.focus_handle.focus(window, cx);
                        this.toggle(key.clone(), cx);
                    }))
            }
        }
    }
}

impl Render for JsonView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let base = self.scroll.0.borrow().base_handle.clone();
        let (read, write) = (base.clone(), base);
        div()
            .size_full()
            .relative()
            .track_focus(&self.focus_handle)
            .key_context("Document")
            .font_family(theme::mono_font())
            .on_action(cx.listener(|this, _: &CopyValue, _, cx| this.copy_selected(cx)))
            .on_action(cx.listener(Self::toggle_selected))
            .on_action(cx.listener(|this, _: &MoveUp, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &MoveDown, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &MoveLeft, _, cx| this.move_left(cx)))
            .on_action(cx.listener(|this, _: &MoveRight, _, cx| this.move_right(cx)))
            .on_action(
                cx.listener(|this, _: &PageUp, _, cx| {
                    this.move_selection(-(PAGE_ROWS as isize), cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &PageDown, _, cx| {
                    this.move_selection(PAGE_ROWS as isize, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &MoveToFirstRow, _, cx| {
                this.move_selection(-(this.rows.len() as isize), cx)
            }))
            .on_action(cx.listener(|this, _: &MoveToLastRow, _, cx| {
                this.move_selection(this.rows.len() as isize, cx)
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    this.focus_handle.focus(window, cx)
                }),
            )
            .child(
                uniform_list(
                    "json-rows",
                    self.rows.len(),
                    cx.processor(|this, range: Range<usize>, _window, cx| {
                        range.map(|ix| this.render_row(ix, cx)).collect()
                    }),
                )
                .track_scroll(&self.scroll)
                .size_full(),
            )
            .child(scrollbar(
                self.scrollbar.clone(),
                move || ScrollMetrics {
                    offset: -read.offset().y,
                    max: read.max_offset().y,
                    viewport: read.bounds().size.height,
                },
                move |offset, window, _cx| {
                    write.set_offset(point(px(0.), -offset));
                    window.refresh();
                },
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> JsonDocument {
        JsonDocument::parse(text.to_string()).unwrap()
    }

    fn shown(doc: &JsonDocument, rows: &[TreeRow]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                TreeRow::Node { depth, path, .. } => {
                    format!("{depth} {}", path_label(doc, path))
                }
                TreeRow::Group { depth, range, .. } => {
                    format!("{depth} [{}..{}]", range.start, range.end)
                }
            })
            .collect()
    }

    #[test]
    fn flattens_only_expanded_values() {
        let doc = parse(r#"{"a":{"b":1},"c":[2,3]}"#);
        let mut position = JsonPosition::default();
        assert_eq!(
            shown(&doc, &tree_rows(&doc, &position.expanded)),
            ["0 $", "1 $.a", "1 $.c"]
        );
        position.expanded.insert(TreeKey::Node(vec![1]));
        assert_eq!(
            shown(&doc, &tree_rows(&doc, &position.expanded)),
            ["0 $", "1 $.a", "1 $.c", "2 $.c[0]", "2 $.c[1]"]
        );
        position.expanded.clear();
        assert_eq!(shown(&doc, &tree_rows(&doc, &position.expanded)), ["0 $"]);
    }

    #[test]
    fn groups_long_arrays_into_ranges() {
        let items = vec!["1"; 2_500].join(",");
        let doc = parse(&format!("[{items}]"));
        let mut expanded = JsonPosition::default().expanded;
        let rows = tree_rows(&doc, &expanded);
        assert_eq!(
            shown(&doc, &rows),
            ["0 $", "1 [0..1000]", "1 [1000..2000]", "1 [2000..2500]"]
        );
        expanded.insert(TreeKey::Group(Vec::new(), 2_000));
        let rows = tree_rows(&doc, &expanded);
        assert_eq!(rows.len(), 4 + 500);
        assert_eq!(shown(&doc, &rows)[4], "2 $[2000]");
    }

    #[test]
    fn writes_paths_readably() {
        let doc = parse(r#"{"routes":[{"운행 일자":1,"노선":2,"a\"b":3,"":4,"1x":5}]}"#);
        let label = |path: &[usize]| path_label(&doc, path);
        assert_eq!(label(&[]), "$");
        assert_eq!(label(&[0, 0, 0]), "$.routes[0][\"운행 일자\"]");
        assert_eq!(label(&[0, 0, 1]), "$.routes[0].노선");
        assert_eq!(label(&[0, 0, 2]), r#"$.routes[0]["a\"b"]"#);
        assert_eq!(label(&[0, 0, 3]), r#"$.routes[0][""]"#);
        assert_eq!(label(&[0, 0, 4]), r#"$.routes[0]["1x"]"#);
    }

    #[test]
    fn previews_values_on_one_line() {
        let doc = parse(r#"["줄\n바꿈\t끝", 1.50, null, true, {"a":1,"b":2}, [1], []]"#);
        let preview = |ix: usize| row_preview(&doc, doc.child(doc.root(), ix).unwrap()).0;
        assert_eq!(preview(0), "\"줄␊바꿈⇥끝\"");
        assert_eq!(preview(1), "1.50");
        assert_eq!(preview(2), "null");
        assert_eq!(preview(3), "true");
        assert_eq!(preview(4), "{2 keys}");
        assert_eq!(preview(5), "[1 item]");
        assert_eq!(preview(6), "[0 items]");
    }

    #[test]
    fn shortens_long_strings_in_rows_but_not_in_the_value_panel() {
        let long = "가".repeat(STRING_PREVIEW_CHARS + 5);
        let doc = parse(&format!("[\"{long}\"]"));
        let node = doc.child(doc.root(), 0).unwrap();
        let (preview, _) = row_preview(&doc, node);
        assert_eq!(preview.chars().count(), STRING_PREVIEW_CHARS + 3);
        assert!(preview.ends_with("…\""));
        let detail = value_detail(&doc, &[0], node);
        assert_eq!(detail.body, ValueBody::Text(long.clone().into()));
        assert_eq!(detail.header.data_type, "string");
        assert_eq!(detail.root.child_count, 0);
        assert_eq!(detail.header.size.as_deref(), Some("783 bytes · 261 chars"));
    }

    #[test]
    fn describes_every_kind_in_the_value_panel() {
        let doc = parse(r#"{"n":null,"x":1e3,"o":{"a":1}}"#);
        let detail = |ix: usize| value_detail(&doc, &[ix], doc.child(doc.root(), ix).unwrap());
        assert_eq!(detail(0).body, ValueBody::Null);
        assert_eq!(detail(1).body, ValueBody::Text("1e3".into()));
        assert_eq!(detail(2).body, ValueBody::Summary("{1 key}".into()));
        assert_eq!(detail(2).header.size.as_deref(), Some("1 key"));
        assert_eq!(detail(2).header.column, "$.o");
    }
}
