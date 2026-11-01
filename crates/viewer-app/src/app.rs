use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{
    App, ClickEvent, ClipboardItem, Context, Div, Entity, FocusHandle, PathPromptOptions, Pixels,
    Point, SharedString, Stateful, Subscription, Window, actions, anchored, deferred, div,
    prelude::*, relative,
};
use viewer_core::{
    ColumnId, DatasetInfo, FilterCondition, RequestId, SessionId, SortKey, ValueRequest, ViewSpec,
    ViewerError, WindowRequest, is_sortable,
};

use crate::ui::explorer::{ExplorerEvent, ExplorerView};
use crate::ui::filter_popover::{FilterPopover, FilterPopoverEvent, op_label};
use crate::ui::find_bar::{FindBar, FindBarEvent, FindStatus};
use crate::ui::grid::{GridEvent, GridLoadState, GridPosition, GridRows, GridView, HeaderMarks};
use crate::ui::schema::SchemaView;
use crate::ui::structure::{StructureEvent, StructureView};
use crate::ui::value::{ValueEvent, ValueView};
use crate::ui::view_bar::{ViewBar, ViewBarAction};
use crate::worker::{
    FindRequest, StructureRequest, ValueOp, ValueOpKind, ViewId, ViewerWorker, WorkerEvent,
};
use viewer_ui::format::{format_bytes, format_count};
use viewer_ui::tab_bar::{TabBar, TabBarState, TabLabel, TabStatus};
use viewer_ui::theme;
use viewer_ui::widgets::toggle_button;

actions!(
    viewer,
    [
        OpenFile,
        OpenFolder,
        NextTab,
        PreviousTab,
        CloseTab,
        Find,
        FindNext,
        FindPrevious
    ]
);

/// 탭 하나의 파일 상태. `Opening → Ready` 또는 `Opening → OpenError`로 전이한다.
enum TabState {
    Opening,
    Ready(DatasetInfo),
    OpenError(SharedString),
}

/// 계산 중인 행 보기.
struct PendingView {
    request: RequestId,
    spec: ViewSpec,
    progress: f32,
}

/// 탭의 행 보기 상태. 그리드는 완성된 행 보기만 표시한다.
#[derive(Default)]
struct TabView {
    /// 그리드에 표시 중인 행 보기의 조건, 번호, 행 수.
    applied: ViewSpec,
    id: ViewId,
    rows: u64,
    pending: Option<PendingView>,
    /// 마지막 계산 실패의 원인과 그 조건.
    failed: Option<(SharedString, ViewSpec)>,
}

impl TabView {
    fn new(total_rows: u64) -> Self {
        Self {
            rows: total_rows,
            ..Self::default()
        }
    }

    /// 사용자가 마지막으로 정한 조건. 계산 중이면 그 조건이다.
    fn target(&self) -> &ViewSpec {
        self.pending
            .as_ref()
            .map_or(&self.applied, |pending| &pending.spec)
    }
}

/// 탭 본문에 보여주는 내용.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum CenterMode {
    /// 데이터 그리드.
    #[default]
    Data,
    /// 파일 구조.
    Structure,
}

/// 파일 하나를 연 탭. 스키마 view를 탭마다 두어 펼침 상태를 기억한다.
struct Tab {
    session: SessionId,
    path: PathBuf,
    state: TabState,
    schema: Entity<SchemaView>,
    /// 탭을 떠날 때 저장하는 그리드 스크롤 위치와 선택 셀.
    grid_position: GridPosition,
    view: TabView,
    mode: CenterMode,
}

impl Tab {
    fn title(&self) -> SharedString {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
            .into()
    }
}

/// 창의 루트 view. 탭 목록과 활성 탭, 세션·요청 발급, 오류 표시를 소유하고 화면 영역을 배치한다.
pub struct AppController {
    focus_handle: FocusHandle,
    worker: ViewerWorker,
    last_session: u64,
    last_request: u64,
    /// 그리드에 반영할 수 있는 활성 탭의 최신 화면 범위 요청.
    latest_request: Option<RequestId>,
    /// Value 패널에 반영할 수 있는 활성 탭의 최신 값 요청.
    latest_value: Option<RequestId>,
    tabs: Vec<Tab>,
    active: Option<usize>,
    /// 탭 막대의 가로 스크롤 상태. 활성 탭이 바뀌면 그 탭이 보이도록 스크롤한다.
    tab_bar: TabBarState,
    explorer: Entity<ExplorerView>,
    grid: Entity<GridView>,
    value: Entity<ValueView>,
    /// 파일·폴더 선택창 자체를 열지 못한 경우의 오류.
    dialog_error: Option<SharedString>,
    find_bar: Entity<FindBar>,
    find_open: bool,
    /// 마지막으로 찾은 입력. 찾기 막대가 열려 있으면 그리드에서 이 입력과 같은 셀을 강조한다.
    find_query: Option<String>,
    latest_find: Option<RequestId>,
    /// 열린 필터 창과 그 위치(창 좌표).
    filter_popover: Option<(Entity<FilterPopover>, Point<Pixels>, Subscription)>,
    /// 필터·정렬 막대에 잠시 표시하는 안내.
    notice: Option<SharedString>,
    structure: Entity<StructureView>,
    /// 파일 구조 화면에 반영할 수 있는 최신 요청.
    latest_structure: Option<RequestId>,
    /// 운영체제의 앱 색상 설정을 따르는지. 화면 전환 버튼을 누르면 그 뒤로는 따르지 않는다.
    follow_system: bool,
    _subscriptions: Vec<Subscription>,
}

impl AppController {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);

        theme::set_mode(theme::Mode::from_appearance(window.appearance()));
        let appearance_events = cx.observe_window_appearance(window, |this, window, cx| {
            if this.follow_system {
                theme::set_mode(theme::Mode::from_appearance(window.appearance()));
                window.refresh();
                cx.notify();
            }
        });

        // 탐색기의 파일 열기는 Open File과 같은 열기 경로를 쓴다.
        let explorer = cx.new(ExplorerView::new);
        let explorer_events =
            cx.subscribe(
                &explorer,
                |this, _, event: &ExplorerEvent, cx| match event {
                    ExplorerEvent::OpenFile(path) => this.open_path(path.clone(), cx),
                },
            );

        let grid = cx.new(GridView::new);
        let grid_events = cx.subscribe_in(
            &grid,
            window,
            |this, _, event: &GridEvent, window, cx| match event {
                GridEvent::ViewportChanged | GridEvent::RetryRequested => this.request_window(cx),
                GridEvent::ReopenRequested => this.reopen_active(cx),
                GridEvent::SelectionChanged => this.request_value(cx),
                GridEvent::CopyRequested => this.value_op(Vec::new(), ValueOpKind::Copy),
                GridEvent::SortRequested(column) => this.toggle_sort(*column, cx),
                GridEvent::FilterRequested { column, anchor } => {
                    this.open_filter(*column, *anchor, window, cx)
                }
            },
        );

        let structure = cx.new(StructureView::new);
        let structure_events =
            cx.subscribe(
                &structure,
                |this, _, event: &StructureEvent, cx| match event {
                    StructureEvent::SelectRowGroup(row_group) => {
                        if let Some(row_group) = row_group {
                            this.request_structure(Some(*row_group), cx);
                        }
                    }
                },
            );

        let find_bar = cx.new(FindBar::new);
        let find_events = cx.subscribe_in(
            &find_bar,
            window,
            |this, _, event: &FindBarEvent, window, cx| match event {
                FindBarEvent::Search { backward } => this.search(*backward, cx),
                FindBarEvent::Close => this.close_find(window, cx),
            },
        );

        let value = cx.new(ValueView::new);
        let value_events = cx.subscribe(&value, |this, _, event: &ValueEvent, cx| match event {
            ValueEvent::Expand { path, range } => this.value_op(
                path.clone(),
                ValueOpKind::Children {
                    range: range.clone(),
                },
            ),
            ValueEvent::Select(path) => this.value_op(path.clone(), ValueOpKind::Body),
            ValueEvent::Copy(path) => this.value_op(path.clone(), ValueOpKind::Copy),
            ValueEvent::Retry => this.request_value(cx),
        });

        let (worker, events) = ViewerWorker::spawn();
        cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                if this
                    .update(cx, |this, cx| this.handle_worker_event(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        Self {
            focus_handle,
            worker,
            last_session: 0,
            last_request: 0,
            latest_request: None,
            latest_value: None,
            tabs: Vec::new(),
            active: None,
            tab_bar: TabBarState::default(),
            explorer,
            grid,
            value,
            dialog_error: None,
            find_bar,
            find_open: false,
            find_query: None,
            latest_find: None,
            filter_popover: None,
            notice: None,
            structure,
            latest_structure: None,
            follow_system: true,
            _subscriptions: vec![
                appearance_events,
                explorer_events,
                grid_events,
                value_events,
                find_events,
                structure_events,
            ],
        }
    }

    fn open_file(&mut self, _: &OpenFile, _window: &mut Window, cx: &mut Context<Self>) {
        self.prompt_for_path(false, Self::open_path, cx);
    }

    fn open_folder(&mut self, _: &OpenFolder, _window: &mut Window, cx: &mut Context<Self>) {
        self.prompt_for_path(
            true,
            |this, path, cx| {
                this.explorer
                    .update(cx, |explorer, cx| explorer.set_root(path, cx))
            },
            cx,
        );
    }

    /// 네이티브 선택창으로 파일 또는 폴더 하나를 고른다. 취소하면 현재 상태를 유지한다.
    fn prompt_for_path(
        &mut self,
        directories: bool,
        on_pick: impl FnOnce(&mut Self, PathBuf, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let selection = cx.prompt_for_paths(PathPromptOptions {
            files: !directories,
            directories,
            multiple: false,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            let result = selection.await;
            this.update(cx, |this, cx| match result {
                Ok(Ok(Some(paths))) => {
                    if let Some(path) = paths.into_iter().next() {
                        this.dialog_error = None;
                        on_pick(this, path, cx);
                    }
                }
                Ok(Ok(None)) | Err(_) => {}
                Ok(Err(err)) => {
                    this.dialog_error = Some(format!("선택창을 열 수 없습니다: {err:#}").into());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// 파일을 새 탭으로 열고 활성화한다. 이미 탭으로 열린 경로면 그 탭을 활성화한다.
    fn open_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if let Some(ix) = self.tabs.iter().position(|tab| same_path(&tab.path, &path)) {
            self.activate(ix, cx);
            return;
        }
        let session = self.next_session();
        self.tabs.push(Tab {
            session,
            path: path.clone(),
            state: TabState::Opening,
            schema: cx.new(|_| SchemaView::new()),
            grid_position: GridPosition::default(),
            view: TabView::default(),
            mode: CenterMode::default(),
        });
        self.worker.open(session, path);
        self.remember_grid_position(cx);
        self.active = Some(self.tabs.len() - 1);
        self.show_active_tab(cx);
    }

    fn next_session(&mut self) -> SessionId {
        self.last_session += 1;
        SessionId(self.last_session)
    }

    fn active_tab(&self) -> Option<&Tab> {
        self.active.and_then(|ix| self.tabs.get(ix))
    }

    fn activate(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.active == Some(ix) || ix >= self.tabs.len() {
            return;
        }
        self.remember_grid_position(cx);
        self.active = Some(ix);
        self.show_active_tab(cx);
    }

    /// 활성 탭을 떠나기 전에 그 탭의 그리드 스크롤 위치와 선택 셀을 저장한다.
    fn remember_grid_position(&mut self, cx: &mut Context<Self>) {
        let position = self.grid.read(cx).position();
        if let Some(tab) = self.active.and_then(|ix| self.tabs.get_mut(ix))
            && matches!(tab.state, TabState::Ready(_))
        {
            tab.grid_position = position;
        }
    }

    /// 그리드를 활성 탭의 내용으로 바꾼다. 이전 탭의 셀 미리보기와 진행 중인 화면 범위 읽기는 버린다.
    fn show_active_tab(&mut self, cx: &mut Context<Self>) {
        self.worker.cancel_window();
        self.latest_request = None;
        // 찾기 결과와 필터 창은 이전 탭의 것이다.
        self.worker.cancel_find();
        self.latest_find = None;
        self.find_bar
            .update(cx, |bar, cx| bar.set_status(FindStatus::Idle, cx));
        self.filter_popover = None;
        self.notice = None;
        if let Some(ix) = self.active {
            self.tab_bar.scroll_to_tab(ix);
        }
        let active = self.active.and_then(|ix| self.tabs.get(ix));
        match active.map(|tab| &tab.state) {
            // 화면 범위는 그리드가 layout에서 계산한 뒤 요청한다.
            Some(TabState::Ready(info)) => {
                let tab = active.expect("활성 탭");
                let schema = info.schema.clone();
                let position = tab.grid_position;
                let rows = GridRows {
                    rows: tab.view.rows,
                    file_rows: info.total_rows,
                    identity: tab.view.applied.is_identity(),
                };
                let marks = header_marks(&tab.view.applied);
                self.grid.update(cx, |grid, cx| {
                    grid.set_dataset(rows, &schema, marks, position, cx)
                });
            }
            _ => self.grid.update(cx, |grid, cx| grid.clear(cx)),
        }
        if let Some(path) = active.map(|tab| tab.path.clone()) {
            self.explorer
                .update(cx, |explorer, cx| explorer.file_activated(&path, cx));
        }
        let structure_mode = active.is_some_and(|tab| {
            tab.mode == CenterMode::Structure && matches!(tab.state, TabState::Ready(_))
        });
        if structure_mode {
            self.request_structure(None, cx);
        } else {
            self.latest_structure = None;
        }
        // 이전 탭의 선택 값을 해제하고, 새 탭에 선택 셀이 있으면 그 값을 다시 요청한다.
        self.request_value(cx);
        cx.notify();
    }

    /// 탭을 닫고 그 세션을 해제한다. 활성 탭을 닫으면 오른쪽 탭을, 없으면 왼쪽 탭을 활성화한다.
    fn close_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(ix);
        self.worker.close(tab.session);
        match self.active {
            Some(active) if active == ix => {
                self.active = if self.tabs.is_empty() {
                    None
                } else {
                    Some(ix.min(self.tabs.len() - 1))
                };
                self.show_active_tab(cx);
            }
            Some(active) if active > ix => self.active = Some(active - 1),
            _ => {}
        }
        cx.notify();
    }

    /// 탭을 `from`에서 `to` 자리로 옮긴다. 활성 탭은 순서가 바뀌어도 같은 탭으로 유지한다.
    fn move_tab(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        if from == to || from >= self.tabs.len() || to >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        self.active = self.active.map(|active| moved_index(active, from, to));
        cx.notify();
    }

    fn close_active_tab(&mut self, _: &CloseTab, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.active {
            self.close_tab(ix, cx);
        }
    }

    fn next_tab(&mut self, _: &NextTab, _window: &mut Window, cx: &mut Context<Self>) {
        self.cycle_tab(1, cx);
    }

    fn previous_tab(&mut self, _: &PreviousTab, _window: &mut Window, cx: &mut Context<Self>) {
        self.cycle_tab(self.tabs.len().saturating_sub(1), cx);
    }

    fn cycle_tab(&mut self, step: usize, cx: &mut Context<Self>) {
        if let Some(active) = self.active
            && !self.tabs.is_empty()
        {
            self.activate((active + step) % self.tabs.len(), cx);
        }
    }

    /// 파일이 바뀐 활성 탭을 같은 자리에서 새 세션으로 다시 연다.
    fn reopen_active(&mut self, cx: &mut Context<Self>) {
        let Some(ix) = self.active else {
            return;
        };
        self.remember_grid_position(cx);
        let session = self.next_session();
        let tab = &mut self.tabs[ix];
        self.worker.close(tab.session);
        tab.session = session;
        tab.state = TabState::Opening;
        // 행 보기 조건은 유지하고, 파일이 열리면 다시 계산한다.
        let spec = tab.view.target().clone();
        tab.view = TabView {
            applied: spec,
            ..TabView::default()
        };
        tab.schema
            .update(cx, |schema, cx| schema.set_schema(None, cx));
        self.worker.open(session, tab.path.clone());
        self.show_active_tab(cx);
    }

    /// 그리드의 현재 화면 범위를 새 요청 번호로 등록한다. 이전 요청의 결과는 반영하지 않는다.
    fn request_window(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let TabState::Ready(info) = &tab.state else {
            return;
        };
        let (session, view) = (info.session, tab.view.id);
        let viewport = self.grid.read(cx).request_range().clone();
        self.last_request += 1;
        let request = RequestId(self.last_request);
        self.latest_request = Some(request);
        if !viewport.rows.is_empty() && !viewport.columns.is_empty() {
            let find = self.find_open.then(|| self.find_query.clone()).flatten();
            self.worker.request_window(
                view,
                WindowRequest {
                    session,
                    request,
                    rows: viewport.rows,
                    columns: viewport.columns.map(ColumnId).collect(),
                },
                find,
            );
        }
        cx.notify();
    }

    fn is_latest_request(&self, session: SessionId, request: RequestId) -> bool {
        matches!(
            self.active_tab().map(|tab| &tab.state),
            Some(TabState::Ready(info)) if info.session == session
        ) && self.latest_request == Some(request)
    }

    /// 활성 탭의 선택 셀 값을 새 요청 번호로 요청한다. 선택 셀이 없으면 선택 값을 해제한다.
    fn request_value(&mut self, cx: &mut Context<Self>) {
        let (session, view) = match self.active_tab() {
            Some(Tab {
                state: TabState::Ready(info),
                view,
                ..
            }) => (Some(info.session), view.id),
            _ => (None, None),
        };
        let selected = self.grid.read(cx).selected();
        let (Some(session), Some((row, column))) = (session, selected) else {
            self.latest_value = None;
            self.worker.cancel_value();
            self.value.update(cx, |value, cx| value.clear(cx));
            return;
        };
        self.last_request += 1;
        let request = RequestId(self.last_request);
        self.latest_value = Some(request);
        self.worker.request_value(
            view,
            ValueRequest {
                session,
                request,
                row,
                column: ColumnId(column),
            },
        );
        self.value.update(cx, |value, cx| value.set_loading(cx));
    }

    /// 현재 선택 값에 대한 후속 작업을 worker에 보낸다.
    fn value_op(&mut self, path: Vec<usize>, kind: ValueOpKind) {
        if let Some(request) = self.latest_value {
            self.worker.value_op(ValueOp {
                request,
                path,
                kind,
            });
        }
    }

    fn is_latest_value(&self, session: SessionId, request: RequestId) -> bool {
        matches!(
            self.active_tab().map(|tab| &tab.state),
            Some(TabState::Ready(info)) if info.session == session
        ) && self.latest_value == Some(request)
    }

    // ----- 행 보기: 필터와 정렬 -----

    /// 활성 탭의 행 보기 조건을 바꾼다. 표시 중인 조건으로 돌아가면 진행 중인 계산을 버린다.
    fn apply_view(&mut self, spec: ViewSpec, cx: &mut Context<Self>) {
        let Some(ix) = self.active else {
            return;
        };
        if !matches!(self.tabs[ix].state, TabState::Ready(_)) {
            return;
        }
        self.notice = None;
        if &spec == self.tabs[ix].view.target() && self.tabs[ix].view.failed.is_none() {
            return;
        }
        self.tabs[ix].view.failed = None;
        if spec == self.tabs[ix].view.applied {
            if self.tabs[ix].view.pending.take().is_some() {
                self.worker.cancel_view(self.tabs[ix].session);
            }
            cx.notify();
            return;
        }
        self.send_view(ix, spec);
        cx.notify();
    }

    /// 행 보기 계산을 worker에 요청한다.
    fn send_view(&mut self, ix: usize, spec: ViewSpec) {
        self.last_request += 1;
        let request = RequestId(self.last_request);
        let tab = &mut self.tabs[ix];
        self.worker.set_view(tab.session, request, spec.clone());
        tab.view.pending = Some(PendingView {
            request,
            spec,
            progress: 0.,
        });
    }

    /// 헤더를 누를 때마다 오름차순, 내림차순, 정렬 해제 순으로 바꾼다.
    fn toggle_sort(&mut self, column: usize, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let TabState::Ready(info) = &tab.state else {
            return;
        };
        let field = &info.schema.fields()[column];
        if !is_sortable(field.data_type()) {
            self.notice = Some(
                format!(
                    "{} 컬럼({})은 정렬할 수 없습니다.",
                    field.name(),
                    field.data_type()
                )
                .into(),
            );
            cx.notify();
            return;
        }
        let mut spec = tab.view.target().clone();
        spec.sort = match spec.sort {
            Some(SortKey {
                column: current,
                descending: false,
            }) if current.0 == column => Some(SortKey {
                column: ColumnId(column),
                descending: true,
            }),
            Some(SortKey {
                column: current,
                descending: true,
            }) if current.0 == column => None,
            _ => Some(SortKey {
                column: ColumnId(column),
                descending: false,
            }),
        };
        self.apply_view(spec, cx);
    }

    fn open_filter(
        &mut self,
        column: usize,
        anchor: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let TabState::Ready(info) = &tab.state else {
            return;
        };
        let schema = info.schema.clone();
        let existing = tab
            .view
            .target()
            .filters
            .iter()
            .find(|condition| condition.column.0 == column)
            .cloned();
        let popover = cx.new(|cx| FilterPopover::new(schema, ColumnId(column), existing, cx));
        popover.update(cx, |popover, cx| popover.focus(window, cx));
        let subscription = cx.subscribe_in(
            &popover,
            window,
            |this, _, event: &FilterPopoverEvent, window, cx| {
                match event {
                    FilterPopoverEvent::Apply(condition) => this.set_filter(condition.clone(), cx),
                    FilterPopoverEvent::Remove(column) => this.remove_filter(*column, cx),
                    FilterPopoverEvent::Close => {}
                }
                this.close_filter(window, cx);
            },
        );
        self.filter_popover = Some((popover, anchor, subscription));
        cx.notify();
    }

    fn close_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.filter_popover.take().is_some() {
            self.grid.update(cx, |grid, cx| grid.focus(window, cx));
            cx.notify();
        }
    }

    /// 컬럼의 필터 조건을 바꾸거나 추가한다. 컬럼마다 조건은 하나다.
    fn set_filter(&mut self, condition: FilterCondition, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let mut spec = tab.view.target().clone();
        match spec
            .filters
            .iter_mut()
            .find(|existing| existing.column == condition.column)
        {
            Some(existing) => *existing = condition,
            None => spec.filters.push(condition),
        }
        self.apply_view(spec, cx);
    }

    fn remove_filter(&mut self, column: ColumnId, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let mut spec = tab.view.target().clone();
        spec.filters.retain(|condition| condition.column != column);
        self.apply_view(spec, cx);
    }

    fn view_bar_action(&mut self, action: ViewBarAction, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let mut spec = tab.view.target().clone();
        match action {
            ViewBarAction::RemoveFilter(column) => {
                spec.filters.retain(|condition| condition.column != column)
            }
            ViewBarAction::RemoveSort => spec.sort = None,
            ViewBarAction::ClearAll => spec = ViewSpec::default(),
            ViewBarAction::Cancel => spec = tab.view.applied.clone(),
            ViewBarAction::Retry => match &tab.view.failed {
                Some((_, failed)) => spec = failed.clone(),
                None => return,
            },
        }
        self.apply_view(spec, cx);
    }

    // ----- 파일 구조 -----

    /// 활성 탭의 본문을 데이터 그리드와 파일 구조 중 하나로 바꾼다.
    fn set_mode(&mut self, mode: CenterMode, cx: &mut Context<Self>) {
        let Some(ix) = self.active else {
            return;
        };
        if self.tabs[ix].mode == mode {
            return;
        }
        self.tabs[ix].mode = mode;
        self.filter_popover = None;
        if mode == CenterMode::Structure {
            self.request_structure(None, cx);
        }
        cx.notify();
    }

    /// 활성 탭의 파일 구조를 요청한다. `row_group`이 있으면 그 Row Group의 컬럼 정보를 요청한다.
    fn request_structure(&mut self, row_group: Option<usize>, cx: &mut Context<Self>) {
        let Some(TabState::Ready(info)) = self.active_tab().map(|tab| &tab.state) else {
            return;
        };
        let session = info.session;
        self.last_request += 1;
        let request = RequestId(self.last_request);
        self.latest_structure = Some(request);
        self.worker.request_structure(StructureRequest {
            session,
            request,
            row_group,
        });
        if row_group.is_none() {
            self.structure
                .update(cx, |structure, cx| structure.set_loading(cx));
        }
    }

    fn is_latest_structure(&self, session: SessionId, request: RequestId) -> bool {
        self.active_tab().is_some_and(|tab| tab.session == session)
            && self.latest_structure == Some(request)
    }

    // ----- 찾기 -----

    fn open_find(&mut self, _: &Find, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_tab().is_none() {
            return;
        }
        // 찾기는 데이터 그리드에서 한다.
        self.set_mode(CenterMode::Data, cx);
        if !self.find_open {
            self.find_open = true;
            cx.notify();
        }
        self.find_bar.update(cx, |bar, cx| bar.focus(window, cx));
    }

    fn find_next(&mut self, _: &FindNext, _window: &mut Window, cx: &mut Context<Self>) {
        if self.find_open {
            self.search(false, cx);
        }
    }

    fn find_previous(&mut self, _: &FindPrevious, _window: &mut Window, cx: &mut Context<Self>) {
        if self.find_open {
            self.search(true, cx);
        }
    }

    fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.find_open = false;
        self.find_query = None;
        self.latest_find = None;
        self.worker.cancel_find();
        self.find_bar
            .update(cx, |bar, cx| bar.set_status(FindStatus::Idle, cx));
        self.grid.update(cx, |grid, cx| {
            grid.reload_cells(cx);
            grid.focus(window, cx);
        });
        cx.notify();
    }

    /// 선택 셀에서 다음(이전) 일치를 찾는다. 입력이 바뀌었으면 그리드의 강조도 바꾼다.
    fn search(&mut self, backward: bool, cx: &mut Context<Self>) {
        let query = self.find_bar.read(cx).query(cx);
        if query.is_empty() {
            return;
        }
        let Some(tab) = self.active_tab() else {
            return;
        };
        let TabState::Ready(info) = &tab.state else {
            return;
        };
        if tab.view.pending.is_some() {
            self.find_bar.update(cx, |bar, cx| {
                bar.set_status(
                    FindStatus::Failed("필터·정렬을 적용한 뒤에 찾을 수 있습니다.".into()),
                    cx,
                )
            });
            return;
        }
        let (session, view) = (info.session, tab.view.id);
        if self.find_query.as_deref() != Some(query.as_str()) {
            self.find_query = Some(query.clone());
            self.grid.update(cx, |grid, cx| grid.reload_cells(cx));
        }
        let from = self
            .grid
            .read(cx)
            .selected()
            .map(|(row, column)| (row, ColumnId(column)));
        self.last_request += 1;
        let request = RequestId(self.last_request);
        self.latest_find = Some(request);
        self.worker.find(FindRequest {
            session,
            request,
            view,
            query,
            from,
            backward,
        });
        self.find_bar
            .update(cx, |bar, cx| bar.set_status(FindStatus::Searching(0.), cx));
    }

    fn is_latest_find(&self, session: SessionId, request: RequestId) -> bool {
        self.active_tab().is_some_and(|tab| tab.session == session)
            && self.latest_find == Some(request)
    }

    fn handle_worker_event(&mut self, event: WorkerEvent, cx: &mut Context<Self>) {
        match event {
            WorkerEvent::Opened { session, result } => {
                // 닫힌 탭이나 다시 연 탭의 이전 세션 응답은 폐기한다.
                let Some(ix) = self.tabs.iter().position(|tab| tab.session == session) else {
                    return;
                };
                let tab = &mut self.tabs[ix];
                tab.state = match result {
                    Ok(info) => {
                        let schema = info.schema.clone();
                        tab.schema
                            .update(cx, |view, cx| view.set_schema(Some(schema), cx));
                        // 다시 연 탭은 기억한 행 보기 조건을 다시 계산한다.
                        let spec = std::mem::take(&mut tab.view.applied);
                        tab.view = TabView::new(info.total_rows);
                        if !spec.is_identity() {
                            self.last_request += 1;
                            let request = RequestId(self.last_request);
                            self.worker.set_view(session, request, spec.clone());
                            tab.view.pending = Some(PendingView {
                                request,
                                spec,
                                progress: 0.,
                            });
                        }
                        TabState::Ready(info)
                    }
                    Err(err) => TabState::OpenError(open_error_message(&err).into()),
                };
                if self.active == Some(ix) {
                    self.show_active_tab(cx);
                }
                cx.notify();
            }
            WorkerEvent::ViewProgress {
                session,
                request,
                progress,
            } => {
                if let Some(pending) = self
                    .tabs
                    .iter_mut()
                    .find(|tab| tab.session == session)
                    .and_then(|tab| tab.view.pending.as_mut())
                    .filter(|pending| pending.request == request)
                {
                    pending.progress = progress;
                    cx.notify();
                }
            }
            WorkerEvent::ViewReady {
                session,
                request,
                rows,
            } => {
                let Some(ix) = self.tabs.iter().position(|tab| tab.session == session) else {
                    return;
                };
                let tab = &mut self.tabs[ix];
                if tab
                    .view
                    .pending
                    .as_ref()
                    .is_none_or(|pending| pending.request != request)
                {
                    return;
                }
                // 새 행 보기는 첫 행부터 보여준다. 가로 위치는 유지한다.
                let position = if self.active == Some(ix) {
                    self.grid.read(cx).position()
                } else {
                    self.tabs[ix].grid_position
                };
                let tab = &mut self.tabs[ix];
                let pending = tab.view.pending.take().expect("계산 중인 행 보기");
                tab.view.applied = pending.spec;
                tab.view.id = Some(request);
                tab.view.rows = rows;
                tab.view.failed = None;
                tab.grid_position = position.at_first_row();
                if self.active == Some(ix) {
                    self.show_active_tab(cx);
                }
                cx.notify();
            }
            WorkerEvent::ViewFailed {
                session,
                request,
                error,
            } => {
                let Some(tab) = self.tabs.iter_mut().find(|tab| tab.session == session) else {
                    return;
                };
                if tab
                    .view
                    .pending
                    .as_ref()
                    .is_none_or(|pending| pending.request != request)
                {
                    return;
                }
                let pending = tab.view.pending.take().expect("계산 중인 행 보기");
                let message = match &error {
                    ViewerError::InvalidCondition(message) | ViewerError::Unsortable(message) => {
                        message.clone()
                    }
                    other => read_error_message(other),
                };
                tab.view.failed = Some((message.into(), pending.spec));
                cx.notify();
            }
            WorkerEvent::FindProgress {
                session,
                request,
                progress,
            } => {
                if self.is_latest_find(session, request) {
                    self.find_bar.update(cx, |bar, cx| {
                        bar.set_status(FindStatus::Searching(progress), cx)
                    });
                }
            }
            WorkerEvent::FindResult {
                session,
                request,
                cells,
                found,
            } => {
                if !self.is_latest_find(session, request) {
                    return;
                }
                let status = if cells == 0 {
                    FindStatus::NotFound
                } else {
                    FindStatus::Found(cells)
                };
                self.find_bar
                    .update(cx, |bar, cx| bar.set_status(status, cx));
                if let Some((row, column)) = found {
                    self.grid
                        .update(cx, |grid, cx| grid.select_cell(row, column.0, cx));
                }
            }
            WorkerEvent::FileStructure {
                session,
                request,
                result,
            } => {
                if self.is_latest_structure(session, request) {
                    self.structure.update(cx, |structure, cx| match result {
                        Ok(file) => structure.set_structure(file, cx),
                        Err(error) => structure.set_failed(read_error_message(&error).into(), cx),
                    });
                }
            }
            WorkerEvent::RowGroupColumns {
                session,
                request,
                row_group,
                result,
            } => {
                if self.is_latest_structure(session, request) {
                    self.structure.update(cx, |structure, cx| match result {
                        Ok(columns) => structure.set_group_columns(row_group, columns, cx),
                        Err(error) => structure.set_failed(read_error_message(&error).into(), cx),
                    });
                }
            }
            WorkerEvent::FindFailed {
                session,
                request,
                error,
            } => {
                if self.is_latest_find(session, request) {
                    let message = read_error_message(&error).into();
                    self.find_bar.update(cx, |bar, cx| {
                        bar.set_status(FindStatus::Failed(message), cx)
                    });
                }
            }
            WorkerEvent::Cells {
                session,
                request,
                cells,
            } => {
                if self.is_latest_request(session, request) {
                    self.grid
                        .update(cx, |grid, cx| grid.insert_cells(cells, cx));
                    cx.notify();
                }
            }
            WorkerEvent::ReadFailed {
                session,
                request,
                rows,
                error,
            } => {
                if self.is_latest_request(session, request) {
                    self.grid.update(cx, |grid, cx| match error {
                        ViewerError::FileChanged => grid.mark_file_changed(cx),
                        error => grid.mark_failed(rows, read_error_message(&error).into(), cx),
                    });
                    cx.notify();
                }
            }
            WorkerEvent::ValueLoaded {
                session,
                request,
                detail,
            } => {
                if self.is_latest_value(session, request) {
                    self.value
                        .update(cx, |value, cx| value.set_detail(detail, cx));
                }
            }
            WorkerEvent::ValueChildren {
                session,
                request,
                path,
                start,
                children,
            } => {
                if self.is_latest_value(session, request) {
                    self.value.update(cx, |value, cx| {
                        value.set_children(path, start, children, cx)
                    });
                }
            }
            WorkerEvent::ValueBody {
                session,
                request,
                path,
                body,
            } => {
                if self.is_latest_value(session, request) {
                    self.value
                        .update(cx, |value, cx| value.set_body(path, body, cx));
                }
            }
            WorkerEvent::ValueCopy {
                session,
                request,
                text,
            } => {
                if self.is_latest_value(session, request) {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            }
            WorkerEvent::ValueFailed {
                session,
                request,
                error,
            } => {
                if self.is_latest_value(session, request) {
                    let message = read_error_message(&error).into();
                    self.value
                        .update(cx, |value, cx| value.set_failed(message, cx));
                }
            }
        }
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self.active_tab().map(Tab::title).unwrap_or_default();
        div()
            .flex_none()
            .h(theme::TOOLBAR_HEIGHT)
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .bg(theme::surface())
            .border_b_1()
            .border_color(theme::border())
            .child(
                toolbar_button("open-file", "Open File").on_click(cx.listener(
                    |this, _: &ClickEvent, window, cx| this.open_file(&OpenFile, window, cx),
                )),
            )
            .child(
                toolbar_button("open-folder", "Open Folder").on_click(cx.listener(
                    |this, _: &ClickEvent, window, cx| this.open_folder(&OpenFolder, window, cx),
                )),
            )
            .child(div().flex_1())
            .child(div().min_w_0().truncate().child(title))
            .child(
                toolbar_button(
                    "theme-mode",
                    match theme::mode() {
                        theme::Mode::Dark => "Light Mode",
                        theme::Mode::Light => "Dark Mode",
                    },
                )
                .flex_none()
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.follow_system = false;
                    theme::set_mode(theme::mode().toggled());
                    window.refresh();
                    cx.notify();
                })),
            )
    }

    /// 탭이 있으면 탭 막대와 활성 탭의 그리드를, 없으면 빈 상태를 그린다.
    fn render_center(&self, cx: &mut Context<Self>) -> Div {
        let Some(tab) = self.active_tab() else {
            return panel("Data Grid").child(placeholder(
                "Open File 또는 Ctrl+O로 Parquet 파일을 엽니다.",
            ));
        };
        let labels = self
            .tabs
            .iter()
            .map(|tab| TabLabel {
                title: tab.title(),
                path: tab.path.display().to_string().into(),
                status: match tab.state {
                    TabState::Opening => TabStatus::Opening,
                    TabState::Ready(_) => TabStatus::Ready,
                    TabState::OpenError(_) => TabStatus::Error,
                },
            })
            .collect();
        let select = cx.listener(|this, ix: &usize, _, cx| this.activate(*ix, cx));
        let close = cx.listener(|this, ix: &usize, _, cx| this.close_tab(*ix, cx));
        let move_tab =
            cx.listener(|this, (from, to): &(usize, usize), _, cx| this.move_tab(*from, *to, cx));
        let body = match &tab.state {
            TabState::Opening => placeholder("여는 중"),
            TabState::Ready(_) if tab.mode == CenterMode::Structure => {
                div().flex_1().min_h_0().child(self.structure.clone())
            }
            TabState::Ready(_) => div().flex_1().min_h_0().child(self.grid.clone()),
            TabState::OpenError(message) => placeholder("파일을 열 수 없습니다.")
                .flex_col()
                .gap_2()
                .text_color(theme::error())
                .child(div().text_color(theme::text()).child(message.clone()))
                .child(
                    div()
                        .text_color(theme::text_muted())
                        .child(tab.path.display().to_string()),
                ),
        };
        let view_bar = match &tab.state {
            TabState::Ready(info) if tab.mode == CenterMode::Data => {
                let view_action = cx.listener(|this, action: &ViewBarAction, _, cx| {
                    this.view_bar_action(*action, cx)
                });
                let bar = ViewBar {
                    filters: tab
                        .view
                        .target()
                        .filters
                        .iter()
                        .map(|condition| (condition.column, filter_label(info, condition)))
                        .collect(),
                    sort: tab.view.target().sort.map(|sort| {
                        let name = info.schema.fields()[sort.column.0].name();
                        format!("{name} {}", if sort.descending { "▼" } else { "▲" }).into()
                    }),
                    pending: tab.view.pending.as_ref().map(|pending| pending.progress),
                    failed: tab.view.failed.as_ref().map(|(message, _)| message.clone()),
                    notice: self.notice.clone(),
                    on_action: Rc::new(move |action, window, cx| view_action(&action, window, cx)),
                };
                bar.is_visible().then_some(bar)
            }
            _ => None,
        };
        let find_bar = (self.find_open
            && tab.mode == CenterMode::Data
            && matches!(tab.state, TabState::Ready(_)))
        .then(|| self.find_bar.clone());
        let mode = tab.mode;
        let mode_button = |id: &'static str, label: &'static str, target: CenterMode| {
            toggle_button(id, label, mode == target)
                .text_xs()
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_mode(target, cx)))
        };
        let mode_toggle = matches!(tab.state, TabState::Ready(_)).then(|| {
            div()
                .flex_none()
                .h(theme::PANEL_HEADER_HEIGHT)
                .px_2()
                .flex()
                .items_center()
                .gap_1()
                .bg(theme::surface())
                .border_b_1()
                .border_l_1()
                .border_color(theme::border())
                .child(mode_button("mode-data", "데이터", CenterMode::Data))
                .child(mode_button(
                    "mode-structure",
                    "파일 구조",
                    CenterMode::Structure,
                ))
        });
        div()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(
                div()
                    .flex_none()
                    .flex()
                    .child(div().flex_1().min_w_0().child(TabBar::new(
                        labels,
                        self.active,
                        self.tab_bar.clone(),
                        move |ix, window, cx| select(&ix, window, cx),
                        move |ix, window, cx| close(&ix, window, cx),
                        move |from, to, window, cx| move_tab(&(from, to), window, cx),
                    )))
                    .children(mode_toggle),
            )
            .children(find_bar)
            .children(view_bar)
            .child(body)
    }

    fn render_status_bar(&self, cx: &App) -> impl IntoElement {
        let path: SharedString = match self.active_tab() {
            Some(tab) => tab.path.display().to_string().into(),
            None => "파일 없음".into(),
        };
        let mut summary: Vec<String> = Vec::new();
        let mut is_error = false;
        let state = match self.active_tab().map(|tab| &tab.state) {
            None => "대기",
            Some(TabState::Opening) => "여는 중",
            Some(TabState::Ready(info)) => {
                let view = &self.active_tab().expect("활성 탭").view;
                if view.applied.filters.is_empty() {
                    summary.push(format!("{}행", format_count(info.total_rows)));
                } else {
                    summary.push(format!(
                        "{}행 / 전체 {}행",
                        format_count(view.rows),
                        format_count(info.total_rows)
                    ));
                }
                summary.push(format!(
                    "{}컬럼",
                    format_count(info.schema.fields().len() as u64)
                ));
                summary.push(format!(
                    "Row Group {}개",
                    format_count(info.row_group_rows.len() as u64)
                ));
                summary.push(format_bytes(info.file_size));
                match self.grid.read(cx).load_state() {
                    GridLoadState::Loading => "읽는 중",
                    GridLoadState::Loaded => "열림",
                    GridLoadState::ReadError => {
                        is_error = true;
                        "읽기 오류"
                    }
                }
            }
            Some(TabState::OpenError(_)) => {
                is_error = true;
                "열기 실패"
            }
        };
        let state_color = if is_error {
            theme::error()
        } else {
            theme::text_muted()
        };

        div()
            .flex_none()
            .h(theme::STATUS_BAR_HEIGHT)
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .bg(theme::surface())
            .border_t_1()
            .border_color(theme::border())
            .text_xs()
            .text_color(theme::text_muted())
            .child(div().min_w_0().truncate().child(path))
            .children(
                summary
                    .into_iter()
                    .flat_map(|item| [div().flex_none().child("·"), div().flex_none().child(item)]),
            )
            .child(div().flex_none().child("·"))
            .child(div().flex_none().text_color(state_color).child(state))
            .children(
                self.dialog_error
                    .clone()
                    .map(|error| div().text_color(theme::error()).child(error)),
            )
    }
}

impl Render for AppController {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let schema = self.active_tab().map(|tab| tab.schema.clone());
        div()
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::open_file))
            .on_action(cx.listener(Self::open_folder))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::previous_tab))
            .on_action(cx.listener(Self::close_active_tab))
            .on_action(cx.listener(Self::open_find))
            .on_action(cx.listener(Self::find_next))
            .on_action(cx.listener(Self::find_previous))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::background())
            .font_family(theme::UI_FONT)
            .text_color(theme::text())
            .text_sm()
            .child(self.render_toolbar(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(
                        panel("Explorer")
                            .flex_none()
                            .w(theme::EXPLORER_WIDTH)
                            .border_r_1()
                            .child(div().flex_1().min_h_0().child(self.explorer.clone())),
                    )
                    .child(self.render_center(cx).flex_1().min_w_0())
                    .child(
                        div()
                            .flex_none()
                            .w(theme::SIDE_PANEL_WIDTH)
                            .flex()
                            .flex_col()
                            .border_l_1()
                            .border_color(theme::border())
                            .child(
                                panel("Schema")
                                    .h(relative(0.4))
                                    .border_b_1()
                                    .child(div().flex_1().min_h_0().children(schema)),
                            )
                            .child(
                                panel("Value")
                                    .flex_1()
                                    .min_h_0()
                                    .child(div().flex_1().min_h_0().child(self.value.clone())),
                            ),
                    ),
            )
            .child(self.render_status_bar(cx))
            .children(self.filter_popover.as_ref().map(|(popover, anchor, _)| {
                deferred(
                    anchored()
                        .position(*anchor)
                        .snap_to_window_with_margin(theme::GRID_CELL_PADDING)
                        .child(popover.clone()),
                )
                .with_priority(1)
            }))
    }
}

/// 헤더에 표시할 정렬·필터 상태.
fn header_marks(spec: &ViewSpec) -> HeaderMarks {
    HeaderMarks {
        sort: spec.sort.map(|sort| (sort.column.0, sort.descending)),
        filtered: spec
            .filters
            .iter()
            .map(|condition| condition.column.0)
            .collect(),
    }
}

/// 필터·정렬 막대에 표시할 조건. 예: `stop_name 포함 "역"`.
fn filter_label(info: &DatasetInfo, condition: &FilterCondition) -> SharedString {
    let name = info.schema.fields()[condition.column.0].name();
    let op = op_label(condition.op);
    if condition.op.takes_value() {
        format!("{name} {op} \"{}\"", condition.value).into()
    } else {
        format!("{name} {op}").into()
    }
}

/// 탭 하나를 `from`에서 `to`로 옮긴 뒤, 원래 `ix`에 있던 탭의 새 위치.
fn moved_index(ix: usize, from: usize, to: usize) -> usize {
    if ix == from {
        to
    } else if from < ix && ix <= to {
        ix - 1
    } else if to <= ix && ix < from {
        ix + 1
    } else {
        ix
    }
}

/// 같은 파일을 가리키는 경로인지 비교한다. Windows 경로는 대소문자를 구분하지 않는다.
fn same_path(a: &Path, b: &Path) -> bool {
    a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
}

/// 엔진 오류를 사용자 문구로 바꾼다.
fn open_error_message(err: &ViewerError) -> String {
    match err {
        ViewerError::Io(io_err) => match io_err.kind() {
            io::ErrorKind::NotFound => "파일이 없습니다.".into(),
            io::ErrorKind::PermissionDenied => "파일을 읽을 권한이 없습니다.".into(),
            _ => format!("파일을 읽을 수 없습니다. ({io_err})"),
        },
        ViewerError::InvalidParquet(parquet_err) => {
            format!("Parquet 파일이 아니거나 메타데이터가 손상되었습니다. ({parquet_err})")
        }
        ViewerError::InvalidRowCount => "Row Group의 행 수 정보가 올바르지 않습니다.".into(),
        other => other.to_string(),
    }
}

/// 데이터 읽기 오류를 사용자 문구로 바꾼다.
fn read_error_message(err: &ViewerError) -> String {
    match err {
        ViewerError::Decode(decode_err) => {
            format!("데이터를 디코딩할 수 없습니다. ({decode_err})")
        }
        ViewerError::Io(io_err) => format!("파일을 읽을 수 없습니다. ({io_err})"),
        other => other.to_string(),
    }
}

fn toolbar_button(id: &'static str, label: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .px_3()
        .py_1()
        .rounded_md()
        .bg(theme::button())
        .hover(|style| style.bg(theme::button_hover()))
        .cursor_pointer()
        .child(label)
}

/// 제목 행이 있는 빈 패널. 호출한 쪽이 크기와 본문을 붙인다.
fn panel(title: &'static str) -> Div {
    div()
        .flex()
        .flex_col()
        .overflow_hidden()
        .border_color(theme::border())
        .child(
            div()
                .flex_none()
                .h(theme::PANEL_HEADER_HEIGHT)
                .px_2()
                .flex()
                .items_center()
                .border_b_1()
                .border_color(theme::border())
                .text_xs()
                .text_color(theme::text_muted())
                .child(title),
        )
}

fn placeholder(message: impl Into<SharedString>) -> Div {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .p_4()
        .text_color(theme::text_muted())
        .child(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_track_of_a_tab_when_another_tab_moves() {
        let move_and_find = |len: usize, ix: usize, from: usize, to: usize| {
            let mut tabs: Vec<usize> = (0..len).collect();
            let tab = tabs.remove(from);
            tabs.insert(to, tab);
            tabs.iter().position(|&t| t == ix).unwrap()
        };
        for from in 0..5 {
            for to in 0..5 {
                for ix in 0..5 {
                    assert_eq!(moved_index(ix, from, to), move_and_find(5, ix, from, to));
                }
            }
        }
    }

    #[test]
    fn compares_windows_paths_without_case() {
        assert!(same_path(
            Path::new(r"C:\Data\a.parquet"),
            Path::new(r"c:\data\A.PARQUET")
        ));
        assert!(!same_path(
            Path::new(r"C:\Data\a.parquet"),
            Path::new(r"C:\Data\b.parquet")
        ));
    }
}
