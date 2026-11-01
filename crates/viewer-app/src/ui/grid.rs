//! 데이터 그리드. 화면에 보이는 행·열만 계산하고 그린다.
//!
//! 전체 행 수에 행 높이를 곱한 element를 만들지 않는다. 스크롤 위치는 첫 행·첫 컬럼과 그 안의
//! pixel offset으로 저장하고, 화면 안의 상대 좌표로 셀을 배치한다.

use std::collections::HashMap;
use std::ops::Range;

use arrow_schema::SchemaRef;
use gpui::{
    App, Bounds, ClickEvent, ContentMask, Context, DispatchPhase, Element, ElementId, Entity,
    EventEmitter, FocusHandle, Font, FontWeight, GlobalElementId, Hitbox, HitboxBehavior, Hsla,
    InspectorElementId, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Pixels, Point, ScrollEvent, SharedString, Style, TextAlign, TextRun, Window, actions, div,
    fill, point, prelude::*, px, relative, size,
};
use viewer_core::ColumnId;

use crate::cell_preview::{CellKind, CellPreview};
use crate::ui::theme;
use crate::ui::value::CopyValue;
use crate::worker::WindowCells;

actions!(
    grid,
    [
        MoveUp,
        MoveDown,
        MoveLeft,
        MoveRight,
        PageUp,
        PageDown,
        MoveToFirstRow,
        MoveToLastRow
    ]
);

/// 화면 범위의 위·아래에 더 요청하는 행 수.
const OVERSCAN_ROWS: u64 = 8;
/// 화면 범위의 좌·우에 더 요청하는 컬럼 수.
const OVERSCAN_COLUMNS: usize = 1;

fn row_height() -> f64 {
    f64::from(theme::GRID_ROW_HEIGHT / px(1.))
}

fn column_width() -> f64 {
    f64::from(theme::GRID_COLUMN_WIDTH / px(1.))
}

fn to_f64(pixels: Pixels) -> f64 {
    f64::from(pixels / px(1.))
}

/// 그리드의 행·열 범위. 행은 행 보기 위치로 0부터, 열은 원본 스키마 위치다.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Viewport {
    pub rows: Range<u64>,
    pub columns: Range<usize>,
}

/// 데이터 크기와 데이터 영역(헤더·행 번호·스크롤바를 뺀 영역)의 pixel 크기.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Extent {
    total_rows: u64,
    total_columns: usize,
    height: f64,
    width: f64,
}

impl Extent {
    fn content_height(&self) -> f64 {
        self.total_rows as f64 * row_height()
    }

    fn content_width(&self) -> f64 {
        self.total_columns as f64 * column_width()
    }

    fn max_y(&self) -> f64 {
        (self.content_height() - self.height).max(0.)
    }

    fn max_x(&self) -> f64 {
        (self.content_width() - self.width).max(0.)
    }

    /// 데이터 영역에 온전히 들어가는 행 수. Page Up/Down의 이동 단위다.
    fn full_rows(&self) -> u64 {
        ((self.height / row_height()).floor() as u64).max(1)
    }
}

/// 논리 스크롤 위치. 세로는 첫 행과 그 행 안의 offset, 가로는 첫 컬럼과 그 컬럼 안의 offset이다.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScrollPosition {
    first_row: u64,
    row_offset: f64,
    first_column: usize,
    column_offset: f64,
}

impl ScrollPosition {
    fn y(&self) -> f64 {
        self.first_row as f64 * row_height() + self.row_offset
    }

    fn x(&self) -> f64 {
        self.first_column as f64 * column_width() + self.column_offset
    }

    /// pixel 위치를 스크롤 범위 안으로 맞춰 첫 행·컬럼과 offset으로 나눈다.
    fn at(x: f64, y: f64, extent: &Extent) -> Self {
        let y = y.clamp(0., extent.max_y());
        let x = x.clamp(0., extent.max_x());
        let first_row = (y / row_height()).floor() as u64;
        let first_column = (x / column_width()).floor() as usize;
        Self {
            first_row,
            row_offset: y - first_row as f64 * row_height(),
            first_column,
            column_offset: x - first_column as f64 * column_width(),
        }
    }

    fn visible_rows(&self, extent: &Extent) -> Range<u64> {
        let count = ((extent.height + self.row_offset) / row_height()).ceil() as u64;
        let start = self.first_row.min(extent.total_rows);
        start..self.first_row.saturating_add(count).min(extent.total_rows)
    }

    fn visible_columns(&self, extent: &Extent) -> Range<usize> {
        let count = ((extent.width + self.column_offset) / column_width()).ceil() as usize;
        let start = self.first_column.min(extent.total_columns);
        start
            ..self
                .first_column
                .saturating_add(count)
                .min(extent.total_columns)
    }

    /// 셀이 데이터 영역 안에 온전히 보이도록 가장 적게 움직인 위치.
    fn revealing(&self, row: u64, column: usize, extent: &Extent) -> Self {
        let reveal = |current: f64, start: f64, len: f64, view: f64| {
            if start < current {
                start
            } else if start + len > current + view {
                start + len - view
            } else {
                current
            }
        };
        let y = reveal(
            self.y(),
            row as f64 * row_height(),
            row_height(),
            extent.height,
        );
        let x = reveal(
            self.x(),
            column as f64 * column_width(),
            column_width(),
            extent.width,
        );
        Self::at(x, y, extent)
    }
}

/// 화면 범위에 overscan을 더한 요청 범위.
fn request_range(visible: &Viewport, extent: &Extent) -> Viewport {
    if visible.rows.is_empty() || visible.columns.is_empty() {
        return Viewport::default();
    }
    Viewport {
        rows: visible.rows.start.saturating_sub(OVERSCAN_ROWS)
            ..visible
                .rows
                .end
                .saturating_add(OVERSCAN_ROWS)
                .min(extent.total_rows),
        columns: visible.columns.start.saturating_sub(OVERSCAN_COLUMNS)
            ..(visible.columns.end + OVERSCAN_COLUMNS).min(extent.total_columns),
    }
}

/// 스크롤바 thumb의 시작 위치와 길이. 위치 비율은 f64로 계산한다.
fn thumb_span(track: f64, content: f64, view: f64, scroll: f64) -> (f64, f64) {
    let min_thumb = to_f64(theme::GRID_SCROLLBAR_MIN_THUMB).min(track);
    let len = (track * view / content.max(1.)).clamp(min_thumb, track);
    let max_scroll = content - view;
    let start = if max_scroll > 0. {
        (track - len) * (scroll / max_scroll).clamp(0., 1.)
    } else {
        0.
    };
    (start, len)
}

/// thumb 시작 위치를 스크롤 pixel 위치로 바꾼다.
fn scroll_from_thumb(track: f64, content: f64, view: f64, thumb_start: f64) -> f64 {
    let (_, len) = thumb_span(track, content, view, 0.);
    let travel = track - len;
    if travel <= 0. {
        return 0.;
    }
    (thumb_start / travel).clamp(0., 1.) * (content - view).max(0.)
}

/// 탭이 기억하는 그리드 상태.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GridPosition {
    scroll: ScrollPosition,
    selected: Option<(u64, usize)>,
}

impl GridPosition {
    /// 가로 위치는 그대로 두고 첫 행으로 옮기며 선택을 해제한 위치. 행 보기가 바뀔 때 쓴다.
    pub fn at_first_row(self) -> Self {
        Self {
            scroll: ScrollPosition {
                first_row: 0,
                row_offset: 0.,
                ..self.scroll
            },
            selected: None,
        }
    }
}

pub enum GridEvent {
    /// 요청 범위가 바뀌어 새 데이터가 필요하다.
    ViewportChanged,
    /// 컬럼 헤더를 눌러 정렬을 바꾼다.
    SortRequested(usize),
    /// 컬럼 헤더의 필터 버튼을 눌렀다. `anchor`는 헤더 칸 왼쪽 아래의 창 좌표다.
    FilterRequested {
        column: usize,
        anchor: Point<Pixels>,
    },
    /// 선택 셀이 바뀌었다.
    SelectionChanged,
    /// 선택 셀의 원본 값을 복사한다.
    CopyRequested,
    /// 읽기 오류가 난 범위를 다시 읽는다.
    RetryRequested,
    /// 파일이 바뀌어 다시 열어야 한다.
    ReopenRequested,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridLoadState {
    Loading,
    Loaded,
    ReadError,
}

struct GridDataset {
    total_rows: u64,
    column_names: Vec<SharedString>,
    /// 행 보기가 원본 순서이면 행 번호를 위치로 계산한다. 아니면 worker가 보낸 원본 행 번호를 쓴다.
    identity_rows: bool,
    /// 원본 파일의 행 수. 행 번호 영역의 너비를 정한다.
    file_rows: u64,
}

/// 그리드에 표시하는 행 보기의 행 수.
#[derive(Clone, Copy, Debug)]
pub struct GridRows {
    /// 행 보기의 행 수.
    pub rows: u64,
    /// 원본 파일의 행 수.
    pub file_rows: u64,
    /// 행 보기가 원본 순서인지.
    pub identity: bool,
}

/// 헤더에 표시하는 정렬·필터 상태.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HeaderMarks {
    /// 정렬 컬럼과 내림차순 여부.
    pub sort: Option<(usize, bool)>,
    /// 필터가 있는 컬럼.
    pub filtered: Vec<usize>,
}

enum GridError {
    Read(SharedString),
    FileChanged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Axis {
    Vertical,
    Horizontal,
}

/// 스크롤바 track의 위치와 길이. 한 축의 좌표만 담는다.
#[derive(Clone, Copy, Debug)]
struct Track {
    start: f64,
    len: f64,
}

/// thumb를 끄는 중인 스크롤바. `grab`은 thumb 시작에서 잡은 지점까지의 거리다.
#[derive(Clone, Copy, Debug)]
struct ScrollbarDrag {
    axis: Axis,
    track: Track,
    grab: f64,
}

pub struct GridView {
    focus_handle: FocusHandle,
    dataset: Option<GridDataset>,
    scroll: ScrollPosition,
    selected: Option<(u64, usize)>,
    /// 마지막 layout의 데이터 크기와 데이터 영역 크기.
    extent: Extent,
    /// 화면에 보이는 범위.
    visible: Viewport,
    /// 화면 범위에 overscan을 더해 요청한 범위.
    requested: Viewport,
    cells: HashMap<(u64, ColumnId), CellPreview>,
    /// 행 보기 위치별 원본 행 번호.
    row_numbers: HashMap<u64, u64>,
    marks: HeaderMarks,
    failed_rows: Vec<Range<u64>>,
    error: Option<GridError>,
    drag: Option<ScrollbarDrag>,
}

impl EventEmitter<GridEvent> for GridView {}

impl GridView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            dataset: None,
            scroll: ScrollPosition::default(),
            selected: None,
            extent: Extent::default(),
            visible: Viewport::default(),
            requested: Viewport::default(),
            cells: HashMap::new(),
            row_numbers: HashMap::new(),
            marks: HeaderMarks::default(),
            failed_rows: Vec::new(),
            error: None,
            drag: None,
        }
    }

    /// 행 보기의 행 수와 컬럼을 설정하고 기억해 둔 스크롤 위치·선택 셀을 되돌린다.
    /// 화면 범위는 다음 layout에서 다시 계산한다.
    pub fn set_dataset(
        &mut self,
        rows: GridRows,
        schema: &SchemaRef,
        marks: HeaderMarks,
        position: GridPosition,
        cx: &mut Context<Self>,
    ) {
        let column_names = schema
            .fields()
            .iter()
            .map(|field| single_line(field.name()).into())
            .collect();
        self.dataset = Some(GridDataset {
            total_rows: rows.rows,
            column_names,
            identity_rows: rows.identity,
            file_rows: rows.file_rows,
        });
        self.reset_data();
        self.marks = marks;
        self.scroll = position.scroll;
        self.selected = position.selected;
        cx.notify();
    }

    /// 화면의 셀 미리보기를 버리고 다음 layout에서 다시 요청한다. 찾기 강조가 바뀔 때 쓴다.
    pub fn reload_cells(&mut self, cx: &mut Context<Self>) {
        self.cells.clear();
        self.requested = Viewport::default();
        cx.notify();
    }

    /// 그리드에 키보드 포커스를 옮긴다.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus_handle, cx);
    }

    /// 셀을 선택하고 화면에 보이게 스크롤한다.
    pub fn select_cell(&mut self, row: u64, column: usize, cx: &mut Context<Self>) {
        self.select(row, column, cx);
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.dataset = None;
        self.reset_data();
        self.scroll = ScrollPosition::default();
        self.selected = None;
        cx.notify();
    }

    pub fn position(&self) -> GridPosition {
        GridPosition {
            scroll: self.scroll,
            selected: self.selected,
        }
    }

    pub fn selected(&self) -> Option<(u64, usize)> {
        self.selected
    }

    /// 화면 범위에 overscan을 더한, 데이터를 요청할 범위.
    pub fn request_range(&self) -> &Viewport {
        &self.requested
    }

    pub fn insert_cells(&mut self, cells: WindowCells, cx: &mut Context<Self>) {
        for (row, number) in (cells.rows.start..).zip(&cells.row_numbers) {
            if self.requested.rows.contains(&row) {
                self.row_numbers.insert(row, *number);
            }
        }
        for (column, previews) in cells.columns.iter().zip(cells.previews) {
            for (row, preview) in (cells.rows.start..).zip(previews) {
                if self.requested.rows.contains(&row) {
                    self.cells.insert((row, *column), preview);
                }
            }
        }
        cx.notify();
    }

    pub fn mark_failed(&mut self, rows: Range<u64>, message: SharedString, cx: &mut Context<Self>) {
        self.failed_rows.push(rows);
        if self.error.is_none() {
            self.error = Some(GridError::Read(message));
        }
        cx.notify();
    }

    pub fn mark_file_changed(&mut self, cx: &mut Context<Self>) {
        self.error = Some(GridError::FileChanged);
        cx.notify();
    }

    /// 화면 범위의 읽기 상태. 화면에 비어 있는 셀이 남아 있으면 `Loading`이다.
    pub fn load_state(&self) -> GridLoadState {
        if self.error.is_some() {
            return GridLoadState::ReadError;
        }
        let missing = self.visible.rows.clone().any(|row| {
            self.visible
                .columns
                .clone()
                .any(|column| !self.cells.contains_key(&(row, ColumnId(column))))
        });
        if missing {
            GridLoadState::Loading
        } else {
            GridLoadState::Loaded
        }
    }

    fn reset_data(&mut self) {
        self.extent = Extent::default();
        self.visible = Viewport::default();
        self.requested = Viewport::default();
        self.cells.clear();
        self.row_numbers.clear();
        self.failed_rows.clear();
        self.error = None;
        self.drag = None;
    }

    /// layout에서 정한 크기로 스크롤 위치를 맞추고 화면·요청 범위를 다시 계산한다.
    /// 요청 범위가 바뀌면 겹치는 셀만 남기고 새 데이터를 요청한다.
    fn apply_layout(&mut self, extent: Extent, cx: &mut Context<Self>) {
        self.extent = extent;
        self.scroll = ScrollPosition::at(self.scroll.x(), self.scroll.y(), &extent);
        self.visible = Viewport {
            rows: self.scroll.visible_rows(&extent),
            columns: self.scroll.visible_columns(&extent),
        };
        let requested = request_range(&self.visible, &extent);
        if requested == self.requested {
            return;
        }
        self.cells.retain(|(row, column), _| {
            requested.rows.contains(row) && requested.columns.contains(&column.0)
        });
        self.row_numbers
            .retain(|row, _| requested.rows.contains(row));
        self.failed_rows.clear();
        self.error = None;
        self.requested = requested;
        cx.emit(GridEvent::ViewportChanged);
        cx.notify();
    }

    fn scroll_to(&mut self, x: f64, y: f64, cx: &mut Context<Self>) {
        let scroll = ScrollPosition::at(x, y, &self.extent);
        if scroll != self.scroll {
            self.scroll = scroll;
            cx.notify();
        }
    }

    fn scroll_by(&mut self, dx: f64, dy: f64, cx: &mut Context<Self>) {
        self.scroll_to(self.scroll.x() + dx, self.scroll.y() + dy, cx);
    }

    fn select(&mut self, row: u64, column: usize, cx: &mut Context<Self>) {
        let Some(dataset) = &self.dataset else {
            return;
        };
        if row >= dataset.total_rows || column >= dataset.column_names.len() {
            return;
        }
        if self.selected != Some((row, column)) {
            self.selected = Some((row, column));
            cx.emit(GridEvent::SelectionChanged);
        }
        self.scroll = self.scroll.revealing(row, column, &self.extent);
        cx.notify();
    }

    /// 선택 셀을 행·열 단위로 옮긴다. 선택 셀이 없으면 화면의 첫 셀에서 시작한다.
    fn move_selection(&mut self, rows: i64, columns: i64, cx: &mut Context<Self>) {
        let Some(dataset) = &self.dataset else {
            return;
        };
        if dataset.total_rows == 0 || dataset.column_names.is_empty() {
            return;
        }
        let (row, column) = self
            .selected
            .unwrap_or((self.visible.rows.start, self.visible.columns.start));
        let row = row.saturating_add_signed(rows).min(dataset.total_rows - 1);
        let column = (column as i64 + columns).clamp(0, dataset.column_names.len() as i64 - 1);
        self.select(row, column as usize, cx);
    }

    fn move_to_row(&mut self, row: u64, cx: &mut Context<Self>) {
        let column = self
            .selected
            .map_or(self.visible.columns.start, |(_, column)| column);
        self.select(row, column, cx);
    }

    fn page(&mut self, direction: i64, cx: &mut Context<Self>) {
        let rows = self.extent.full_rows() as i64 * direction;
        self.move_selection(rows, 0, cx);
    }

    fn mouse_down(&mut self, position: Point<Pixels>, hit: &HitAreas, cx: &mut Context<Self>) {
        let x = to_f64(position.x);
        let y = to_f64(position.y);
        if let Some(bar) = &hit.vertical
            && bar.contains(position)
        {
            self.press_scrollbar(Axis::Vertical, bar.track(Axis::Vertical), bar.thumb, y, cx);
            return;
        }
        if let Some(bar) = &hit.horizontal
            && bar.contains(position)
        {
            self.press_scrollbar(
                Axis::Horizontal,
                bar.track(Axis::Horizontal),
                bar.thumb,
                x,
                cx,
            );
            return;
        }
        if hit.header.contains(&position) {
            let column =
                ((x - to_f64(hit.header.left()) + self.scroll.x()) / column_width()).floor();
            let total_columns = self
                .dataset
                .as_ref()
                .map_or(0, |dataset| dataset.column_names.len());
            if column < 0. || column as usize >= total_columns {
                return;
            }
            let column = column as usize;
            let cell_left =
                to_f64(hit.header.left()) + column as f64 * column_width() - self.scroll.x();
            let cell_right = cell_left + column_width();
            if x >= cell_right - to_f64(theme::GRID_FILTER_BUTTON_WIDTH) {
                let anchor = point(
                    px(cell_left.max(to_f64(hit.header.left())) as f32),
                    hit.header.bottom(),
                );
                cx.emit(GridEvent::FilterRequested { column, anchor });
            } else {
                cx.emit(GridEvent::SortRequested(column));
            }
            return;
        }
        if hit.data.contains(&position) {
            let row = ((y - to_f64(hit.data.top()) + self.scroll.y()) / row_height()).floor();
            let column = ((x - to_f64(hit.data.left()) + self.scroll.x()) / column_width()).floor();
            self.select(row as u64, column as usize, cx);
        }
    }

    /// thumb를 누르면 그 자리에서 끌기 시작하고, track을 누르면 thumb 가운데를 그 자리로 옮긴다.
    fn press_scrollbar(
        &mut self,
        axis: Axis,
        track: Track,
        thumb: (f64, f64),
        along: f64,
        cx: &mut Context<Self>,
    ) {
        let (thumb_start, thumb_len) = thumb;
        let pressed = along - track.start;
        let grab = if (thumb_start..thumb_start + thumb_len).contains(&pressed) {
            pressed - thumb_start
        } else {
            thumb_len / 2.
        };
        self.drag = Some(ScrollbarDrag { axis, track, grab });
        self.drag_to(along, cx);
    }

    fn drag_to(&mut self, along: f64, cx: &mut Context<Self>) {
        let Some(drag) = self.drag else {
            return;
        };
        let thumb_start = along - drag.track.start - drag.grab;
        let extent = self.extent;
        match drag.axis {
            Axis::Vertical => {
                let y = scroll_from_thumb(
                    drag.track.len,
                    extent.content_height(),
                    extent.height,
                    thumb_start,
                );
                self.scroll_to(self.scroll.x(), y, cx);
            }
            Axis::Horizontal => {
                let x = scroll_from_thumb(
                    drag.track.len,
                    extent.content_width(),
                    extent.width,
                    thumb_start,
                );
                self.scroll_to(x, self.scroll.y(), cx);
            }
        }
    }

    fn render_error_bar(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let (message, action_label, event): (SharedString, &str, fn() -> GridEvent) =
            match self.error.as_ref()? {
                GridError::Read(message) => (message.clone(), "다시 시도", || {
                    GridEvent::RetryRequested
                }),
                GridError::FileChanged => (
                    "파일이 열린 뒤 변경되었습니다.".into(),
                    "다시 열기",
                    || GridEvent::ReopenRequested,
                ),
            };
        Some(
            div()
                .absolute()
                .bottom_0()
                .left_0()
                .right_0()
                .px_2()
                .py_1()
                .flex()
                .items_center()
                .gap_2()
                .bg(theme::surface())
                .border_t_1()
                .border_color(theme::border())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(theme::error())
                        .child(message),
                )
                .child(
                    div()
                        .id("grid-error-action")
                        .flex_none()
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .bg(theme::button())
                        .hover(|style| style.bg(theme::button_hover()))
                        .cursor_pointer()
                        .child(action_label.to_string())
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.failed_rows.clear();
                            this.error = None;
                            cx.emit(event());
                            cx.notify();
                        })),
                ),
        )
    }
}

impl Render for GridView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .relative()
            .track_focus(&self.focus_handle)
            .key_context("Grid")
            .on_action(cx.listener(|this, _: &MoveUp, _, cx| this.move_selection(-1, 0, cx)))
            .on_action(cx.listener(|this, _: &MoveDown, _, cx| this.move_selection(1, 0, cx)))
            .on_action(cx.listener(|this, _: &MoveLeft, _, cx| this.move_selection(0, -1, cx)))
            .on_action(cx.listener(|this, _: &MoveRight, _, cx| this.move_selection(0, 1, cx)))
            .on_action(cx.listener(|this, _: &PageUp, _, cx| this.page(-1, cx)))
            .on_action(cx.listener(|this, _: &PageDown, _, cx| this.page(1, cx)))
            .on_action(cx.listener(|this, _: &MoveToFirstRow, _, cx| this.move_to_row(0, cx)))
            .on_action(cx.listener(|this, _: &MoveToLastRow, _, cx| {
                let last = this
                    .dataset
                    .as_ref()
                    .map_or(0, |dataset| dataset.total_rows.saturating_sub(1));
                this.move_to_row(last, cx)
            }))
            .on_action(cx.listener(|this, _: &CopyValue, _, cx| {
                if this.selected.is_some() {
                    cx.emit(GridEvent::CopyRequested);
                }
            }))
            .child(GridElement { view: cx.entity() })
            .children(self.render_error_bar(cx))
    }
}

/// 스크롤바 하나의 track·thumb 위치.
#[derive(Clone, Copy, Debug)]
struct ScrollbarGeometry {
    track: Bounds<Pixels>,
    /// track 시작에서 thumb 시작까지의 거리와 thumb 길이.
    thumb: (f64, f64),
}

impl ScrollbarGeometry {
    fn contains(&self, position: Point<Pixels>) -> bool {
        self.track.contains(&position)
    }

    fn track(&self, axis: Axis) -> Track {
        match axis {
            Axis::Vertical => Track {
                start: to_f64(self.track.top()),
                len: to_f64(self.track.size.height),
            },
            Axis::Horizontal => Track {
                start: to_f64(self.track.left()),
                len: to_f64(self.track.size.width),
            },
        }
    }

    fn thumb_bounds(&self, axis: Axis) -> Bounds<Pixels> {
        let (start, len) = (px(self.thumb.0 as f32), px(self.thumb.1 as f32));
        match axis {
            Axis::Vertical => Bounds::new(
                point(self.track.left(), self.track.top() + start),
                size(self.track.size.width, len),
            ),
            Axis::Horizontal => Bounds::new(
                point(self.track.left() + start, self.track.top()),
                size(len, self.track.size.height),
            ),
        }
    }
}

/// 마우스 입력을 나누는 영역.
#[derive(Clone, Copy, Debug)]
struct HitAreas {
    /// 데이터 컬럼 위의 헤더 영역.
    header: Bounds<Pixels>,
    data: Bounds<Pixels>,
    vertical: Option<ScrollbarGeometry>,
    horizontal: Option<ScrollbarGeometry>,
}

/// 그리드의 layout·hitbox·paint를 직접 수행하는 element.
struct GridElement {
    view: Entity<GridView>,
}

/// prepaint에서 계산해 paint에 넘기는 배치 정보와 화면 범위의 셀.
struct GridLayout {
    hitbox: Hitbox,
    areas: HitAreas,
    row_number_width: Pixels,
    total_rows: u64,
    /// 첫 행·첫 컬럼이 데이터 영역 위·왼쪽으로 가려진 길이.
    row_offset: Pixels,
    column_offset: Pixels,
    headers: Vec<HeaderCell>,
    rows: Vec<GridRow>,
}

struct HeaderCell {
    name: SharedString,
    /// 정렬 컬럼이면 내림차순 여부.
    sort: Option<bool>,
    filtered: bool,
}

struct GridRow {
    number: SharedString,
    selected_column: Option<usize>,
    failed: bool,
    /// 화면 범위의 컬럼 순서. 아직 도착하지 않은 셀은 `None`이다.
    cells: Vec<Option<CellPreview>>,
}

impl IntoElement for GridElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for GridElement {
    type RequestLayoutState = ();
    type PrepaintState = Option<GridLayout>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let (total_rows, total_columns, file_rows) = {
            let dataset = self.view.read(cx).dataset.as_ref()?;
            (
                dataset.total_rows,
                dataset.column_names.len(),
                dataset.file_rows,
            )
        };

        // 행 번호 영역은 가장 큰 원본 행 번호가 들어갈 너비로 고정한다.
        let text_style = window.text_style();
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let widest = SharedString::from(file_rows.max(1).to_string());
        let run = text_run(widest.len(), text_style.font(), theme::text().into());
        let number_width = window
            .text_system()
            .shape_line(widest, font_size, &[run], None)
            .width();
        let row_number_width =
            (number_width + theme::GRID_CELL_PADDING * 2.).max(theme::GRID_MIN_ROW_NUMBER_WIDTH);

        // 스크롤바가 필요한지 정한다. 한쪽 막대가 생기면 다른 쪽 데이터 영역이 줄어든다.
        let bar = to_f64(theme::GRID_SCROLLBAR_SIZE);
        let body_width = to_f64(bounds.size.width - row_number_width).max(0.);
        let body_height = to_f64(bounds.size.height - theme::GRID_HEADER_HEIGHT).max(0.);
        let content_height = total_rows as f64 * row_height();
        let content_width = total_columns as f64 * column_width();
        let mut needs_vertical = content_height > body_height;
        let needs_horizontal = content_width > body_width - if needs_vertical { bar } else { 0. };
        if needs_horizontal && !needs_vertical {
            needs_vertical = content_height > body_height - bar;
        }
        let data_width = (body_width - if needs_vertical { bar } else { 0. }).max(0.);
        let data_height = (body_height - if needs_horizontal { bar } else { 0. }).max(0.);
        let extent = Extent {
            total_rows,
            total_columns,
            height: data_height,
            width: data_width,
        };
        self.view
            .update(cx, |view, cx| view.apply_layout(extent, cx));

        let data = Bounds::new(
            point(
                bounds.left() + row_number_width,
                bounds.top() + theme::GRID_HEADER_HEIGHT,
            ),
            size(px(data_width as f32), px(data_height as f32)),
        );
        let view = self.view.read(cx);
        let scroll = view.scroll;
        let vertical = needs_vertical.then(|| ScrollbarGeometry {
            track: Bounds::new(
                point(data.right(), data.top()),
                size(theme::GRID_SCROLLBAR_SIZE, data.size.height),
            ),
            thumb: thumb_span(data_height, content_height, data_height, scroll.y()),
        });
        let horizontal = needs_horizontal.then(|| ScrollbarGeometry {
            track: Bounds::new(
                point(data.left(), data.bottom()),
                size(data.size.width, theme::GRID_SCROLLBAR_SIZE),
            ),
            thumb: thumb_span(data_width, content_width, data_width, scroll.x()),
        });

        let dataset = view.dataset.as_ref()?;
        let visible = &view.visible;
        let headers = visible
            .columns
            .clone()
            .map(|column| HeaderCell {
                name: dataset.column_names[column].clone(),
                sort: view
                    .marks
                    .sort
                    .and_then(|(sorted, descending)| (sorted == column).then_some(descending)),
                filtered: view.marks.filtered.contains(&column),
            })
            .collect();
        let rows = visible
            .rows
            .clone()
            .map(|row| GridRow {
                number: if dataset.identity_rows {
                    (row + 1).to_string().into()
                } else {
                    view.row_numbers
                        .get(&row)
                        .map(|number| (number + 1).to_string().into())
                        .unwrap_or_default()
                },
                selected_column: view.selected.and_then(|(selected_row, column)| {
                    (selected_row == row && visible.columns.contains(&column))
                        .then(|| column - visible.columns.start)
                }),
                failed: view.failed_rows.iter().any(|rows| rows.contains(&row)),
                cells: visible
                    .columns
                    .clone()
                    .map(|column| view.cells.get(&(row, ColumnId(column))).cloned())
                    .collect(),
            })
            .collect();
        let row_offset = px(scroll.row_offset as f32);
        let column_offset = px(scroll.column_offset as f32);
        Some(GridLayout {
            hitbox: window.insert_hitbox(bounds, HitboxBehavior::Normal),
            areas: HitAreas {
                header: Bounds::new(
                    point(data.left(), bounds.top()),
                    size(data.size.width, theme::GRID_HEADER_HEIGHT),
                ),
                data,
                vertical,
                horizontal,
            },
            row_number_width,
            total_rows,
            row_offset,
            column_offset,
            headers,
            rows,
        })
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        layout: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(layout) = layout.as_ref() else {
            return;
        };
        self.paint_grid(bounds, layout, window, cx);
        self.register_mouse_listeners(layout, window);
    }
}

impl GridElement {
    fn paint_grid(
        &self,
        bounds: Bounds<Pixels>,
        layout: &GridLayout,
        window: &mut Window,
        cx: &mut App,
    ) {
        let text_style = window.text_style();
        let painter = CellPainter {
            font: text_style.font(),
            font_size: text_style.font_size.to_pixels(window.rem_size()),
            line_height: window.line_height(),
        };
        let data = layout.areas.data;
        let header_height = theme::GRID_HEADER_HEIGHT;
        let row_height = theme::GRID_ROW_HEIGHT;
        let column_width = theme::GRID_COLUMN_WIDTH;
        let rows_top = data.top() - layout.row_offset;
        let columns_left = data.left() - layout.column_offset;
        let rows_bottom = (rows_top + row_height * layout.rows.len() as f32).min(data.bottom());
        let columns_right =
            (columns_left + column_width * layout.headers.len() as f32).min(data.right());
        let header_strip = Bounds::new(
            point(data.left(), bounds.top()),
            size(data.size.width, header_height),
        );
        let number_strip = Bounds::new(
            point(bounds.left(), data.top()),
            size(layout.row_number_width, data.size.height),
        );
        let header_font = Font {
            weight: FontWeight::SEMIBOLD,
            ..painter.font.clone()
        };

        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            // 고정 헤더와 행 번호 영역의 배경.
            window.paint_quad(fill(
                Bounds::new(bounds.origin, size(bounds.size.width, header_height)),
                theme::surface(),
            ));
            window.paint_quad(fill(
                Bounds::new(
                    point(bounds.left(), data.top()),
                    size(layout.row_number_width, bounds.size.height - header_height),
                ),
                theme::surface(),
            ));

            // 헤더는 가로로만, 행 번호는 세로로만 데이터 영역과 함께 움직인다.
            window.with_content_mask(
                Some(ContentMask {
                    bounds: header_strip,
                }),
                |window| {
                    for (ix, header) in layout.headers.iter().enumerate() {
                        let left = columns_left + column_width * ix as f32;
                        let button = theme::GRID_FILTER_BUTTON_WIDTH;
                        let mark = if header.sort.is_some() {
                            theme::GRID_SORT_MARK_WIDTH
                        } else {
                            px(0.)
                        };
                        // 이름은 정렬 표시와 필터 버튼 자리를 뺀 폭에 그린다.
                        let name = Bounds::new(
                            point(left, bounds.top()),
                            size(
                                column_width - button - mark + theme::GRID_CELL_PADDING,
                                header_height,
                            ),
                        );
                        painter.paint(
                            window,
                            cx,
                            name,
                            header.name.clone(),
                            header_font.clone(),
                            theme::text().into(),
                            TextAlign::Left,
                        );
                        if let Some(descending) = header.sort {
                            painter.paint_glyph(
                                window,
                                cx,
                                Bounds::new(
                                    point(left + column_width - button - mark, bounds.top()),
                                    size(mark, header_height),
                                ),
                                if descending { "▼" } else { "▲" },
                                theme::accent().into(),
                            );
                        }
                        painter.paint_glyph(
                            window,
                            cx,
                            Bounds::new(
                                point(left + column_width - button, bounds.top()),
                                size(button, header_height),
                            ),
                            "▾",
                            if header.filtered {
                                theme::accent().into()
                            } else {
                                theme::text_muted().into()
                            },
                        );
                    }
                },
            );
            window.with_content_mask(
                Some(ContentMask {
                    bounds: number_strip,
                }),
                |window| {
                    for (ix, row) in layout.rows.iter().enumerate() {
                        let cell = Bounds::new(
                            point(bounds.left(), rows_top + row_height * ix as f32),
                            size(layout.row_number_width, row_height),
                        );
                        if row.selected_column.is_some() {
                            window.paint_quad(fill(cell, theme::selection()));
                        }
                        painter.paint(
                            window,
                            cx,
                            cell,
                            row.number.clone(),
                            painter.font.clone(),
                            theme::text_muted().into(),
                            TextAlign::Right,
                        );
                    }
                },
            );

            window.with_content_mask(Some(ContentMask { bounds: data }), |window| {
                for (ix, row) in layout.rows.iter().enumerate() {
                    let top = rows_top + row_height * ix as f32;
                    for (column_ix, preview) in row.cells.iter().enumerate() {
                        let cell = Bounds::new(
                            point(columns_left + column_width * column_ix as f32, top),
                            size(column_width, row_height),
                        );
                        if row.selected_column == Some(column_ix) {
                            window.paint_quad(fill(cell, theme::selection()));
                        } else if preview.as_ref().is_some_and(|preview| preview.matched) {
                            window.paint_quad(fill(cell, theme::find_match()));
                        }
                        let (text, font, color, align) = match preview {
                            Some(preview) => painter.style(preview),
                            None if row.failed => (
                                "읽기 오류".into(),
                                painter.font.clone(),
                                theme::error().into(),
                                TextAlign::Left,
                            ),
                            // 읽는 중인 셀의 placeholder.
                            None => (
                                "…".into(),
                                painter.font.clone(),
                                theme::text_muted().into(),
                                TextAlign::Left,
                            ),
                        };
                        painter.paint(window, cx, cell, text, font, color, align);
                    }
                    window.paint_quad(fill(
                        Bounds::new(
                            point(data.left(), top + row_height - px(1.)),
                            size(columns_right - data.left(), px(1.)),
                        ),
                        theme::grid_line(),
                    ));
                }
            });

            // 열 경계선과 헤더·행 번호 경계선.
            window.with_content_mask(
                Some(ContentMask {
                    bounds: Bounds::from_corners(
                        point(data.left(), bounds.top()),
                        point(data.right(), data.bottom()),
                    ),
                }),
                |window| {
                    for ix in 1..=layout.headers.len() {
                        let x = columns_left + column_width * ix as f32 - px(1.);
                        window.paint_quad(fill(
                            Bounds::new(
                                point(x, bounds.top()),
                                size(px(1.), rows_bottom - bounds.top()),
                            ),
                            theme::grid_line(),
                        ));
                    }
                },
            );
            window.paint_quad(fill(
                Bounds::new(
                    point(bounds.left(), data.top() - px(1.)),
                    size(bounds.size.width, px(1.)),
                ),
                theme::border(),
            ));
            window.paint_quad(fill(
                Bounds::new(
                    point(data.left() - px(1.), bounds.top()),
                    size(px(1.), bounds.size.height),
                ),
                theme::border(),
            ));

            for (axis, bar) in [
                (Axis::Vertical, layout.areas.vertical),
                (Axis::Horizontal, layout.areas.horizontal),
            ] {
                if let Some(bar) = bar {
                    window.paint_quad(fill(bar.track, theme::scrollbar_track()));
                    window.paint_quad(fill(bar.thumb_bounds(axis), theme::scrollbar_thumb()));
                }
            }

            if layout.total_rows == 0 {
                let body = Bounds::new(data.origin, size(data.size.width, row_height));
                painter.paint(
                    window,
                    cx,
                    body,
                    "행이 없습니다.".into(),
                    painter.font.clone(),
                    theme::text_muted().into(),
                    TextAlign::Center,
                );
            }
        });
    }

    /// 휠·Shift+휠 스크롤, 셀 클릭, 스크롤바 클릭·끌기를 처리한다.
    fn register_mouse_listeners(&self, layout: &GridLayout, window: &mut Window) {
        let areas = layout.areas;

        let view = self.view.clone();
        let hitbox = layout.hitbox.clone();
        window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || !hitbox.should_handle_scroll(window) {
                return;
            }
            let delta = event.delta.pixel_delta(theme::GRID_ROW_HEIGHT);
            let (mut dx, mut dy) = (to_f64(delta.x), to_f64(delta.y));
            if event.modifiers.shift && dx == 0. {
                (dx, dy) = (dy, 0.);
            }
            view.update(cx, |view, cx| view.scroll_by(-dx, -dy, cx));
            cx.stop_propagation();
        });

        let view = self.view.clone();
        let hitbox = layout.hitbox.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble
                || event.button != MouseButton::Left
                || !hitbox.is_hovered(window)
            {
                return;
            }
            view.update(cx, |view, cx| {
                view.focus_handle.focus(window, cx);
                view.mouse_down(event.position, &areas, cx);
            });
        });

        let view = self.view.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _window, cx| {
            if phase != DispatchPhase::Bubble || !event.dragging() {
                return;
            }
            view.update(cx, |view, cx| {
                if let Some(drag) = view.drag {
                    let along = match drag.axis {
                        Axis::Vertical => event.position.y,
                        Axis::Horizontal => event.position.x,
                    };
                    view.drag_to(to_f64(along), cx);
                }
            });
        });

        let view = self.view.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, _window, cx| {
            if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                view.update(cx, |view, _| view.drag = None);
            }
        });
    }
}

struct CellPainter {
    font: Font,
    font_size: Pixels,
    line_height: Pixels,
}

impl CellPainter {
    /// 미리보기 종류에 따른 글꼴·색·정렬.
    fn style(&self, preview: &CellPreview) -> (SharedString, Font, Hsla, TextAlign) {
        let text = preview.text.clone();
        match preview.kind {
            CellKind::Text => (
                text,
                self.font.clone(),
                theme::text().into(),
                TextAlign::Left,
            ),
            CellKind::Number => (
                text,
                self.font.clone(),
                theme::text().into(),
                TextAlign::Right,
            ),
            CellKind::Null => (
                text,
                self.font.clone().italic(),
                theme::null_text().into(),
                TextAlign::Left,
            ),
            CellKind::Summary => (
                text,
                self.font.clone(),
                theme::summary_text().into(),
                TextAlign::Left,
            ),
            CellKind::Unsupported => (
                text,
                self.font.clone().italic(),
                theme::text_muted().into(),
                TextAlign::Left,
            ),
        }
    }

    /// 칸 가운데에 기호 하나를 그린다.
    fn paint_glyph(
        &self,
        window: &mut Window,
        cx: &mut App,
        cell: Bounds<Pixels>,
        glyph: &'static str,
        color: Hsla,
    ) {
        let run = text_run(glyph.len(), self.font.clone(), color);
        let line = window
            .text_system()
            .shape_line(glyph.into(), self.font_size, &[run], None);
        let origin = point(
            cell.left() + (cell.size.width - line.width()) / 2.,
            cell.top() + (cell.size.height - self.line_height) / 2.,
        );
        let _ = line.paint(origin, self.line_height, TextAlign::Left, None, window, cx);
    }

    /// 셀 안쪽 여백 안에 한 줄 텍스트를 그린다. 셀 밖으로 나가는 부분은 잘린다.
    #[allow(clippy::too_many_arguments)]
    fn paint(
        &self,
        window: &mut Window,
        cx: &mut App,
        cell: Bounds<Pixels>,
        text: SharedString,
        font: Font,
        color: Hsla,
        align: TextAlign,
    ) {
        if text.is_empty() {
            return;
        }
        let padding = theme::GRID_CELL_PADDING;
        let inner = Bounds::new(
            point(cell.left() + padding, cell.top()),
            size(
                (cell.size.width - padding * 2.).max(px(0.)),
                cell.size.height,
            ),
        );
        let run = text_run(text.len(), font, color);
        let line = window
            .text_system()
            .shape_line(text, self.font_size, &[run], None);
        // 칸보다 긴 값은 정렬과 관계없이 앞부분이 보이도록 왼쪽에 맞춘다.
        let align = if line.width() > inner.size.width {
            TextAlign::Left
        } else {
            align
        };
        let origin = point(
            inner.left(),
            cell.top() + (cell.size.height - self.line_height) / 2.,
        );
        window.with_content_mask(Some(ContentMask { bounds: inner }), |window| {
            let _ = line.paint(
                origin,
                self.line_height,
                align,
                Some(inner.size.width),
                window,
                cx,
            );
        });
    }
}

fn text_run(len: usize, font: Font, color: Hsla) -> TextRun {
    TextRun {
        len,
        font,
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

/// 헤더처럼 한 줄로 그리는 텍스트의 줄바꿈을 표시 기호로 바꾼다.
fn single_line(text: &str) -> String {
    text.replace("\r\n", "↵").replace(['\r', '\n'], "↵")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extent(total_rows: u64, total_columns: usize, height: f64, width: f64) -> Extent {
        Extent {
            total_rows,
            total_columns,
            height,
            width,
        }
    }

    #[test]
    fn clamps_scroll_to_the_last_full_screen() {
        // 행 높이 24px, 컬럼 너비 160px.
        let extent = extent(1_000, 10, 240., 480.);
        let end = ScrollPosition::at(f64::MAX, f64::MAX, &extent);
        assert_eq!(end.y(), 1_000. * 24. - 240.);
        assert_eq!(end.first_row, 990);
        assert_eq!(end.visible_rows(&extent), 990..1_000);
        assert_eq!(end.x(), 1_600. - 480.);
        assert_eq!(end.visible_columns(&extent), 7..10);

        let start = ScrollPosition::at(-5., -5., &extent);
        assert_eq!(start, ScrollPosition::default());
    }

    #[test]
    fn splits_pixel_position_into_row_and_offset() {
        let extent = extent(1_000, 10, 240., 480.);
        let scroll = ScrollPosition::at(170., 50., &extent);
        assert_eq!((scroll.first_row, scroll.row_offset), (2, 2.));
        assert_eq!((scroll.first_column, scroll.column_offset), (1, 10.));
        // 일부만 보이는 위·아래 행을 함께 그린다.
        assert_eq!(scroll.visible_rows(&extent), 2..13);
        assert_eq!(scroll.visible_columns(&extent), 1..5);
    }

    #[test]
    fn keeps_precision_for_huge_row_counts() {
        let total_rows = 10_000_000_000_000u64;
        let extent = extent(total_rows, 3, 240., 480.);
        let end = ScrollPosition::at(0., f64::MAX, &extent);
        assert_eq!(end.visible_rows(&extent).end, total_rows);
        assert_eq!(end.first_row, total_rows - 10);
    }

    #[test]
    fn reveals_cells_with_the_smallest_move() {
        let extent = extent(1_000, 10, 240., 480.);
        let scroll = ScrollPosition::default();
        let below = scroll.revealing(20, 0, &extent);
        assert_eq!(below.y(), 21. * 24. - 240.);
        let above = below.revealing(3, 0, &extent);
        assert_eq!(above.y(), 3. * 24.);
        assert_eq!(above.revealing(5, 0, &extent), above);
        let right = scroll.revealing(0, 5, &extent);
        assert_eq!(right.x(), 6. * 160. - 480.);
    }

    #[test]
    fn adds_overscan_within_bounds() {
        let extent = extent(100, 5, 240., 480.);
        let visible = Viewport {
            rows: 3..13,
            columns: 1..4,
        };
        assert_eq!(
            request_range(&visible, &extent),
            Viewport {
                rows: 0..21,
                columns: 0..5,
            }
        );
        assert_eq!(
            request_range(&Viewport::default(), &extent),
            Viewport::default()
        );
    }

    #[test]
    fn maps_thumb_and_scroll_both_ways() {
        let (start, len) = thumb_span(200., 2_000., 200., 900.);
        assert_eq!(len, 24f64.max(20.));
        assert!((scroll_from_thumb(200., 2_000., 200., start) - 900.).abs() < 1e-6);
        assert_eq!(thumb_span(200., 100., 200., 0.), (0., 200.));
        assert_eq!(scroll_from_thumb(200., 2_000., 200., 10_000.), 1_800.);
    }
}
