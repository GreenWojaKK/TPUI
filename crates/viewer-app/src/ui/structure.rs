//! 파일 구조 화면. 파일 요약, leaf 컬럼별 저장 정보, Row Group 목록을 보여준다.
//!
//! 모든 값은 Parquet 메타데이터에 적힌 정보다. Row Group을 고르면 컬럼 표가 그 Row Group의
//! 정보로 바뀐다.

use std::ops::Range;

use gpui::{
    ClickEvent, Context, Div, EventEmitter, SharedString, Stateful, Window, div, prelude::*, px,
    relative, uniform_list,
};
use viewer_core::{ColumnChunkInfo, FileStructure, StatValue};

use viewer_ui::format::{format_bytes, format_count, format_ratio};
use viewer_ui::table::{self, Cell, ColumnSpec};
use viewer_ui::theme;

pub enum StructureEvent {
    /// 컬럼 표에 보일 Row Group을 고른다. `None`은 전체다.
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
    /// 컬럼 표에 보이는 Row Group. `None`은 전체다.
    selected: Option<usize>,
    /// 고른 Row Group의 컬럼 정보. 받기 전에는 `None`이다.
    group_columns: Option<Vec<ColumnChunkInfo>>,
}

impl EventEmitter<StructureEvent> for StructureView {}

/// 컬럼 표의 칸 이름과 너비 비율.
const COLUMN_TABLE: [ColumnSpec; 10] = [
    ("컬럼", 3.0),
    ("타입", 2.4),
    ("압축", 1.4),
    ("압축 후", 1.4),
    ("압축 전", 1.4),
    ("압축률", 1.1),
    ("null", 1.3),
    ("최솟값", 2.4),
    ("최댓값", 2.4),
    ("인덱스", 1.3),
];

/// Row Group 표의 칸 이름과 너비 비율.
const GROUP_TABLE: [ColumnSpec; 5] = [
    ("Row Group", 1.5),
    ("행 수", 1.5),
    ("압축 후", 1.5),
    ("압축 전", 1.5),
    ("압축률", 1.0),
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

    /// 고른 Row Group의 컬럼 정보를 받는다. 그 사이 다른 Row Group을 골랐으면 버린다.
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
            "없음".into()
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
                "Parquet 형식 버전",
                structure.format_version.to_string().into(),
            ))
            .child(line(
                "만든 프로그램",
                structure
                    .created_by
                    .clone()
                    .unwrap_or_else(|| "기록 없음".into())
                    .into(),
            ))
            .child(line(
                "데이터 크기",
                format!(
                    "압축 후 {} · 압축 전 {} · 압축률 {}",
                    format_bytes(compressed),
                    format_bytes(uncompressed),
                    format_ratio(uncompressed, compressed)
                )
                .into(),
            ))
            .child(line("key-value 메타데이터", keys))
    }

    fn render_column_row(&self, ix: usize) -> Stateful<Div> {
        let column = &self.columns().expect("컬럼 표를 그릴 때만 부른다")[ix];
        let type_label: SharedString = match &column.logical_type {
            Some(logical) => format!("{} · {logical}", column.physical_type).into(),
            None => column.physical_type.clone().into(),
        };
        let index = match (column.page_index, column.bloom_filter) {
            (true, true) => "페이지 · Bloom",
            (true, false) => "페이지",
            (false, true) => "Bloom",
            (false, false) => "-",
        };
        let cells = vec![
            Cell::new(column.path.clone()).tooltip(column.path.clone()),
            Cell::new(type_label.clone()).tooltip(format!(
                "{type_label}\n인코딩: {}",
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
            unreachable!("Row Group 표는 구조가 있을 때만 그린다")
        };
        // 첫 행은 전체, 그다음부터 Row Group이다.
        let row_group = ix.checked_sub(1);
        let (label, rows, compressed, uncompressed) = match row_group {
            None => (
                SharedString::from("전체"),
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

/// 통계 값 칸. 파일에 잘라 적힌 근삿값은 `≈`를 붙인다.
fn stat_cell(value: Option<&StatValue>) -> Cell {
    match value {
        None => Cell::new("-"),
        Some(value) if value.exact => Cell::new(value.text.clone()).tooltip(value.text.clone()),
        Some(value) => Cell::new(format!("≈ {}", value.text))
            .tooltip(format!("{}\n파일에 잘라 적힌 근삿값입니다.", value.text)),
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
                return placeholder("파일 구조를 읽는 중", theme::text_muted());
            }
            StructureState::Failed(message) => {
                return placeholder(message.clone(), theme::error());
            }
            StructureState::Ready(structure) => structure,
        };
        let column_title: SharedString = match self.selected {
            None => format!("컬럼 {}개 · 전체", structure.columns.len()).into(),
            Some(group) => {
                format!("컬럼 {}개 · Row Group #{group}", structure.columns.len()).into()
            }
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
            placeholder("Row Group 정보를 읽는 중", theme::text_muted())
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
            .child(table::header(&COLUMN_TABLE))
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
                        format!("Row Group {}개", structure.row_groups.len()).into(),
                    ))
                    .child(table::header(&GROUP_TABLE))
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
