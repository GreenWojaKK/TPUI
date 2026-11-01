//! 활성 파일의 스키마 트리. 중첩 타입의 자식을 펼칠 수 있고, 펼쳐진 노드만 행으로 그린다.

use std::collections::HashSet;
use std::ops::Range;

use arrow_schema::{DataType, Field, SchemaRef};
use gpui::{
    ClickEvent, Context, Div, SharedString, Stateful, Window, div, prelude::*, uniform_list,
};

use viewer_ui::theme;

/// 스키마 필드 하나와 그 자식 필드.
#[derive(Debug, PartialEq, Eq)]
struct SchemaNode {
    name: SharedString,
    type_label: SharedString,
    nullable: bool,
    children: Vec<SchemaNode>,
}

impl SchemaNode {
    /// Struct는 필드를, List류는 원소 필드를, Map은 key·value 필드를 자식으로 둔다.
    fn from_field(field: &Field) -> Self {
        let item = |label: &str, item: &Field| (label.to_string(), vec![Self::from_field(item)]);
        let (type_label, children) = match field.data_type() {
            DataType::Struct(fields) => (
                "Struct".to_string(),
                fields.iter().map(|field| Self::from_field(field)).collect(),
            ),
            DataType::List(field) => item("List", field),
            DataType::LargeList(field) => item("LargeList", field),
            DataType::ListView(field) => item("ListView", field),
            DataType::LargeListView(field) => item("LargeListView", field),
            DataType::FixedSizeList(field, len) => item(&format!("FixedSizeList[{len}]"), field),
            DataType::Map(entries, _) => {
                let children = match entries.data_type() {
                    DataType::Struct(fields) => {
                        fields.iter().map(|field| Self::from_field(field)).collect()
                    }
                    _ => Vec::new(),
                };
                ("Map".to_string(), children)
            }
            other => (other.to_string(), Vec::new()),
        };
        Self {
            name: field.name().replace(['\r', '\n'], "↵").into(),
            type_label: type_label.into(),
            nullable: field.is_nullable(),
            children,
        }
    }
}

/// 펼쳐진 트리를 평평하게 펼친 행 하나. `path`는 최상위부터의 자식 위치다.
#[derive(Debug, PartialEq, Eq)]
struct SchemaRow {
    depth: usize,
    path: Vec<usize>,
    name: SharedString,
    type_label: SharedString,
    nullable: bool,
    expandable: bool,
    expanded: bool,
}

fn flatten(
    nodes: &[SchemaNode],
    expanded: &HashSet<Vec<usize>>,
    path: &mut Vec<usize>,
    rows: &mut Vec<SchemaRow>,
) {
    for (ix, node) in nodes.iter().enumerate() {
        path.push(ix);
        let is_expanded = !node.children.is_empty() && expanded.contains(path);
        rows.push(SchemaRow {
            depth: path.len() - 1,
            path: path.clone(),
            name: node.name.clone(),
            type_label: node.type_label.clone(),
            nullable: node.nullable,
            expandable: !node.children.is_empty(),
            expanded: is_expanded,
        });
        if is_expanded {
            flatten(&node.children, expanded, path, rows);
        }
        path.pop();
    }
}

pub struct SchemaView {
    nodes: Vec<SchemaNode>,
    expanded: HashSet<Vec<usize>>,
    rows: Vec<SchemaRow>,
}

impl SchemaView {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            expanded: HashSet::new(),
            rows: Vec::new(),
        }
    }

    pub fn set_schema(&mut self, schema: Option<SchemaRef>, cx: &mut Context<Self>) {
        self.nodes = schema
            .map(|schema| {
                schema
                    .fields()
                    .iter()
                    .map(|field| SchemaNode::from_field(field))
                    .collect()
            })
            .unwrap_or_default();
        self.expanded.clear();
        self.rebuild_rows();
        cx.notify();
    }

    fn toggle(&mut self, path: &[usize], cx: &mut Context<Self>) {
        if !self.expanded.remove(path) {
            self.expanded.insert(path.to_vec());
        }
        self.rebuild_rows();
        cx.notify();
    }

    fn rebuild_rows(&mut self) {
        let mut rows = Vec::new();
        flatten(&self.nodes, &self.expanded, &mut Vec::new(), &mut rows);
        self.rows = rows;
    }

    fn render_row(&self, ix: usize, cx: &mut Context<Self>) -> Stateful<Div> {
        let row = &self.rows[ix];
        let chevron = match (row.expandable, row.expanded) {
            (true, true) => "▾",
            (true, false) => "▸",
            (false, _) => "",
        };
        let path = row.path.clone();
        div()
            .id(ix)
            .w_full()
            .h(theme::LIST_ROW_HEIGHT)
            .pl(theme::TREE_INDENT * row.depth as f32 + theme::TREE_ROW_PADDING)
            .pr(theme::TREE_ROW_PADDING)
            .flex()
            .items_center()
            .gap_1()
            .when(row.expandable, |row| {
                row.cursor_pointer()
                    .hover(|style| style.bg(theme::hover()))
                    .on_click(
                        cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle(&path, cx)),
                    )
            })
            .child(
                div()
                    .flex_none()
                    .w(theme::TREE_CHEVRON_WIDTH)
                    .text_color(theme::text_muted())
                    .child(chevron),
            )
            .child(div().flex_1().min_w_0().truncate().child(row.name.clone()))
            .child(
                div()
                    .flex_none()
                    .max_w(theme::SCHEMA_TYPE_MAX_WIDTH)
                    .truncate()
                    .text_color(theme::text_muted())
                    .child(row.type_label.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .w(theme::SCHEMA_NULLABLE_WIDTH)
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(if row.nullable { "nullable" } else { "not null" }),
            )
    }
}

impl Render for SchemaView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        uniform_list(
            "schema-rows",
            self.rows.len(),
            cx.processor(|this, range: Range<usize>, _window, cx| {
                range.map(|ix| this.render_row(ix, cx)).collect()
            }),
        )
        .size_full()
    }
}

#[cfg(test)]
mod tests {
    use arrow_schema::Fields;

    use super::*;

    fn nested_fields() -> Vec<SchemaNode> {
        let point = Field::new(
            "point",
            DataType::Struct(Fields::from(vec![
                Field::new("x", DataType::Int32, false),
                Field::new("y", DataType::Int32, true),
            ])),
            true,
        );
        let tags = Field::new_list("tags", Field::new_list_field(DataType::Utf8, true), true);
        let attrs = Field::new_map(
            "attrs",
            "entries",
            Field::new("key", DataType::Utf8, false),
            Field::new("value", DataType::Int64, true),
            false,
            true,
        );
        [Field::new("id", DataType::Int64, false), point, tags, attrs]
            .iter()
            .map(SchemaNode::from_field)
            .collect()
    }

    fn row_labels(rows: &[SchemaRow]) -> Vec<(usize, &str, &str)> {
        rows.iter()
            .map(|row| (row.depth, row.name.as_ref(), row.type_label.as_ref()))
            .collect()
    }

    #[test]
    fn builds_children_for_nested_types() {
        let nodes = nested_fields();
        assert!(nodes[0].children.is_empty());
        assert_eq!(nodes[1].type_label.as_ref(), "Struct");
        assert_eq!(nodes[1].children.len(), 2);
        assert_eq!(nodes[2].type_label.as_ref(), "List");
        assert_eq!(nodes[2].children[0].type_label.as_ref(), "Utf8");
        assert_eq!(nodes[3].type_label.as_ref(), "Map");
        let map_children: Vec<_> = nodes[3]
            .children
            .iter()
            .map(|child| child.name.as_ref())
            .collect();
        assert_eq!(map_children, vec!["key", "value"]);
    }

    #[test]
    fn includes_only_expanded_children() {
        let nodes = nested_fields();
        let mut rows = Vec::new();
        flatten(&nodes, &HashSet::new(), &mut Vec::new(), &mut rows);
        assert_eq!(rows.len(), 4);
        assert!(rows.iter().all(|row| !row.expanded));

        let expanded: HashSet<Vec<usize>> = [vec![1], vec![3]].into_iter().collect();
        let mut rows = Vec::new();
        flatten(&nodes, &expanded, &mut Vec::new(), &mut rows);
        assert_eq!(
            row_labels(&rows),
            vec![
                (0, "id", "Int64"),
                (0, "point", "Struct"),
                (1, "x", "Int32"),
                (1, "y", "Int32"),
                (0, "tags", "List"),
                (0, "attrs", "Map"),
                (1, "key", "Utf8"),
                (1, "value", "Int64"),
            ]
        );
        assert_eq!(rows[2].path, vec![1, 0]);
        assert!(!rows[2].nullable);
    }

    #[test]
    fn ignores_expansion_of_leaf_fields() {
        let nodes = vec![SchemaNode::from_field(&Field::new(
            "id",
            DataType::Int64,
            false,
        ))];
        let expanded: HashSet<Vec<usize>> = [vec![0]].into_iter().collect();
        let mut rows = Vec::new();
        flatten(&nodes, &expanded, &mut Vec::new(), &mut rows);
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].expandable);
        assert!(!rows[0].expanded);
    }
}
