//! File structure view. Shows the file summary, per-leaf-column storage information and the Row Group list.
//!
//! All values come from the Parquet metadata. Selecting a Row Group switches the column table to that
//! Row Group's information.

use std::ops::Range;

use gpui::{
    ClickEvent, Context, Div, EventEmitter, SharedString, Stateful, Window, div, prelude::*, px,
    relative, uniform_list,
};
use viewer_core::{ColumnChunkInfo, FileStructure, StatValue};

use viewer_ui::format::{format_bytes, format_count, format_quantity, format_ratio};
use viewer_ui::table::{self, Cell, ColumnSpec};
use viewer_ui::theme;

pub enum StructureEvent {
    /// Selects the Row Group shown in the column table. `None` is all of them.
    SelectRowGroup(Option<usize>),
}

enum StructureState {
    Empty,
    Loading,
    Ready(FileStructure),
    Failed(SharedString),
}

pub struct StructureView {
    state: StructureState,
    /// Row Group shown in the column table. `None` is all of them.
    selected: Option<usize>,
    /// Column information of the selected Row Group. `None` until received.
    group_columns: Option<Vec<ColumnChunkInfo>>,
}

impl EventEmitter<StructureEvent> for StructureView {}

/// Column names and width ratios of the column table.
const COLUMN_TABLE: [ColumnSpec; 10] = [
    ("Column", 2.6),
    ("Type", 2.0),
    ("Codec", 1.3),
    ("Compressed", 2.0),
    ("Uncompressed", 2.3),
    ("Ratio", 1.0),
    ("null", 1.0),
    ("Min", 2.2),
    ("Max", 2.2),
    ("Index", 1.2),
];

/// Column names and width ratios of the Row Group table.
const GROUP_TABLE: [ColumnSpec; 5] = [
    ("Row Group", 1.5),
    ("Rows", 1.5),
    ("Compressed", 1.5),
    ("Uncompressed", 1.5),
    ("Ratio", 1.0),
];

impl StructureView {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            state: StructureState::Empty,
            selected: None,
            group_columns: None,
        }
    }

    pub fn set_loading(&mut self, cx: &mut Context<Self>) {
        self.state = StructureState::Loading;
        self.selected = None;
        self.group_columns = None;
        cx.notify();
    }

    pub fn set_structure(&mut self, structure: FileStructure, cx: &mut Context<Self>) {
        self.state = StructureState::Ready(structure);
        self.selected = None;
        self.group_columns = None;
        cx.notify();
    }

    pub fn set_failed(&mut self, message: SharedString, cx: &mut Context<Self>) {
        self.state = StructureState::Failed(message);
        cx.notify();
    }

    /// Receives the selected Row Group's column information. Dropped if another Row Group was selected meanwhile.
    pub fn set_group_columns(
        &mut self,
        row_group: usize,
        columns: Vec<ColumnChunkInfo>,
        cx: &mut Context<Self>,
    ) {
        if self.selected == Some(row_group) {
            self.group_columns = Some(columns);
            cx.notify();
        }
    }

    fn select(&mut self, row_group: Option<usize>, cx: &mut Context<Self>) {
        if self.selected == row_group {
            return;
        }
        self.selected = row_group;
        self.group_columns = None;
        cx.emit(StructureEvent::SelectRowGroup(row_group));
        cx.notify();
    }

    fn columns(&self) -> Option<&[ColumnChunkInfo]> {
        match (&self.state, self.selected) {
            (StructureState::Ready(structure), None) => Some(&structure.columns),
            (StructureState::Ready(_), Some(_)) => self.group_columns.as_deref(),
            _ => None,
        }
    }

    fn render_summary(structure: &FileStructure) -> Div {
        let compressed: u64 = structure
            .row_groups
            .iter()
            .map(|group| group.compressed)
            .sum();
        let uncompressed: u64 = structure
            .row_groups
            .iter()
            .map(|group| group.uncompressed)
            .sum();
        let keys: SharedString = if structure.key_values.is_empty() {
            "none".into()
        } else {
            structure
                .key_values
                .iter()
                .map(|(key, len)| format!("{key} ({})", format_bytes(*len as u64)))
                .collect::<Vec<_>>()
                .join(", ")
                .into()
        };
        let line = |label: &'static str, value: SharedString| {
            div()
                .flex()
                .gap_2()
                .child(
                    div()
                        .flex_none()
                        .w(px(140.))
                        .text_color(theme::text_muted())
                        .child(label),
                )
                .child(div().flex_1().min_w_0().truncate().child(value))
        };
        div()
            .flex_none()
            .p_2()
            .flex()
            .flex_col()
            .gap_1()
            .border_b_1()
            .border_color(theme::border())
            .child(line(
                "Parquet format version",
                structure.format_version.to_string().into(),
            ))
            .child(line(
                "Created by",
                structure
                    .created_by
                    .clone()
                    .unwrap_or_else(|| "not recorded".into())
                    .into(),
            ))
            .child(line(
                "Data size",
                format!(
                    "{} compressed · {} uncompressed · ratio {}",
                    format_bytes(compressed),
                    format_bytes(uncompressed),
                    format_ratio(uncompressed, compressed)
                )
                .into(),
            ))
            .child(line("Key-value metadata", keys))
    }

    fn render_column_row(&self, ix: usize) -> Stateful<Div> {
        let column = &self
            .columns()
            .expect("called only when drawing the column table")[ix];
        let type_label: SharedString = match &column.logical_type {
            Some(logical) => format!("{} · {logical}", column.physical_type).into(),
            None => column.physical_type.clone().into(),
        };
        let index = match (column.page_index, column.bloom_filter) {
            (true, true) => "Page · Bloom",
            (true, false) => "Page",
            (false, true) => "Bloom",
            (false, false) => "-",
        };
        let cells = vec![
            Cell::new(column.path.clone()).tooltip(column.path.clone()),
            Cell::new(type_label.clone()).tooltip(format!(
                "{type_label}\nEncodings: {}",
                column.encodings.join(", ")
            )),
            Cell::new(column.codecs.join(", ")),
            Cell::new(format_bytes(column.compressed)).right(),
            Cell::new(format_bytes(column.uncompressed)).right(),
            Cell::new(format_ratio(column.uncompressed, column.compressed)).right(),
            Cell::new(column.nulls.map_or("-".into(), format_count)).right(),
            stat_cell(column.min.as_ref()),
            stat_cell(column.max.as_ref()),
            Cell::new(index),
        ];
        table::row("structure-column", ix, &COLUMN_TABLE, cells)
    }

    fn render_group_row(&self, ix: usize, cx: &mut Context<Self>) -> Stateful<Div> {
        let StructureState::Ready(structure) = &self.state else {
            unreachable!("the Row Group table is drawn only with a structure")
        };
        // The first row is the whole file; Row Groups follow.
        let row_group = ix.checked_sub(1);
        let (label, rows, compressed, uncompressed) = match row_group {
            None => (
                SharedString::from("All"),
                structure.row_groups.iter().map(|group| group.rows).sum(),
                structure
                    .row_groups
                    .iter()
                    .map(|group| group.compressed)
                    .sum(),
                structure
                    .row_groups
                    .iter()
                    .map(|group| group.uncompressed)
                    .sum(),
            ),
            Some(group) => {
                let info = &structure.row_groups[group];
                (
                    SharedString::from(format!("#{group}")),
                    info.rows,
                    info.compressed,
                    info.uncompressed,
                )
            }
        };
        let cells = vec![
            Cell::new(label),
            Cell::new(format_count(rows)).right(),
            Cell::new(format_bytes(compressed)).right(),
            Cell::new(format_bytes(uncompressed)).right(),
            Cell::new(format_ratio(uncompressed, compressed)).right(),
        ];
        let selected = self.selected == row_group;
        table::row("structure-group", ix, &GROUP_TABLE, cells)
            .cursor_pointer()
            .when(selected, |row| row.bg(theme::selection()))
            .when(!selected, |row| row.hover(|style| style.bg(theme::hover())))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.select(row_group, cx)))
    }
}

/// Statistics value cell. Approximations truncated in the file get a `≈` prefix.
fn stat_cell(value: Option<&StatValue>) -> Cell {
    match value {
        None => Cell::new("-"),
        Some(value) if value.exact => Cell::new(value.text.clone()).tooltip(value.text.clone()),
        Some(value) => Cell::new(format!("≈ {}", value.text)).tooltip(format!(
            "{}\nApproximation truncated in the file.",
            value.text
        )),
    }
}

fn placeholder(message: impl Into<SharedString>, color: gpui::Rgba) -> Div {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .text_color(color)
        .child(message.into())
}

impl Render for StructureView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let structure = match &self.state {
            StructureState::Empty => return placeholder("", theme::text_muted()),
            StructureState::Loading => {
                return placeholder("Reading file structure", theme::text_muted());
            }
            StructureState::Failed(message) => {
                return placeholder(message.clone(), theme::error());
            }
            StructureState::Ready(structure) => structure,
        };
        let column_title: SharedString = match self.selected {
            None => format!(
                "{} · all",
                format_quantity(structure.columns.len() as u64, "column", "columns")
            )
            .into(),
            Some(group) => format!(
                "{} · Row Group #{group}",
                format_quantity(structure.columns.len() as u64, "column", "columns")
            )
            .into(),
        };
        let column_count = self.columns().map_or(0, <[ColumnChunkInfo]>::len);
        let group_count = structure.row_groups.len() + 1;
        let section = |title: SharedString| {
            div()
                .flex_none()
                .h(theme::PANEL_HEADER_HEIGHT)
                .px_2()
                .flex()
                .items_center()
                .text_color(theme::text_muted())
                .child(title)
        };
        let column_body = if self.columns().is_some() {
            uniform_list(
                "structure-columns",
                column_count,
                cx.processor(|this, range: Range<usize>, _window, _cx| {
                    range.map(|ix| this.render_column_row(ix)).collect()
                }),
            )
            .flex_1()
            .into_any_element()
        } else {
            placeholder("Reading Row Group information", theme::text_muted())
                .flex_1()
                .into_any_element()
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .text_xs()
            .child(Self::render_summary(structure))
            .child(section(column_title))
            .child(table::header_aligned(&COLUMN_TABLE, |ix| {
                (3..=6).contains(&ix)
            }))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(column_body),
            )
            .child(
                div()
                    .flex_none()
                    .h(relative(0.3))
                    .flex()
                    .flex_col()
                    .border_t_1()
                    .border_color(theme::border())
                    .child(section(
                        format_quantity(
                            structure.row_groups.len() as u64,
                            "Row Group",
                            "Row Groups",
                        )
                        .into(),
                    ))
                    .child(table::header_aligned(&GROUP_TABLE, |ix| ix > 0))
                    .child(
                        uniform_list(
                            "structure-groups",
                            group_count,
                            cx.processor(|this, range: Range<usize>, _window, cx| {
                                range.map(|ix| this.render_group_row(ix, cx)).collect()
                            }),
                        )
                        .flex_1(),
                    ),
            )
    }
}
