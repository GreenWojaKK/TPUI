use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    App, ClickEvent, ClipboardItem, Context, Div, Entity, FocusHandle, Modifiers,
    PathPromptOptions, Pixels, Point, SharedString, Stateful, Subscription, WeakEntity, Window,
    actions, div, prelude::*, relative,
};
use viewer_core::{
    ColumnId, DataSource, DatasetInfo, DocumentKind, Filter, FilterCondition, FilterOp, LogicOp,
    Query, RequestId, SessionId, SortKey, ValueRequest, ViewSpec, ViewerError, WindowRequest,
    check_condition, document_kind, filter_ops, format_condition, format_query, format_sort,
    format_value, is_sortable, is_sqlite_file, op_text, parse_query, sqlite_objects,
};

use crate::open_requests::OpenRequest;
use crate::query_log::{Entry, Origin, QueryLog};
use crate::ui::document::{DocumentEvent, DocumentPosition, DocumentStatus, DocumentView};
use crate::ui::explorer::{ExplorerEvent, ExplorerView};
use crate::ui::find_bar::{FindBar, FindBarEvent, FindStatus};
use crate::ui::grid::{
    GridEvent, GridLoadState, GridPosition, GridRows, GridView, HeaderMarks, MenuTarget,
};
use crate::ui::schema::SchemaView;
use crate::ui::structure::{StructureEvent, StructureView};
use crate::ui::style;
use crate::ui::value::{ValueEvent, ValueView};
use crate::ui::view_bar::{SweepRange, ViewBar, ViewBarAction};
use crate::usage_log::{ActiveFile, Helpers, Measured, UsageEvent, UsageMonitor};
use crate::value_detail::CellValue;
use crate::worker::{
    FindRequest, StructureRequest, ValueOp, ValueOpKind, ViewId, ViewerWorker, WorkerEvent,
};
use viewer_ui::format::{format_bytes, format_count, format_quantity};
use viewer_ui::inbox::Inbox;
#[cfg(target_os = "windows")]
use viewer_ui::input_window::{self, InputEvent, InputWindow};
use viewer_ui::menu::{self, Menu, MenuEvent};
use viewer_ui::tab_bar::{TabBar, TabBarState, TabLabel, TabStatus};
use viewer_ui::theme;
use viewer_ui::widgets::{small_button, toggle_button};

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
        FindPrevious,
        CloseFind
    ]
);

/// File state of a tab. Transitions `Opening → Ready` or `Opening → OpenError`.
enum TabState {
    Opening,
    Ready(DatasetInfo),
    OpenError(SharedString),
    /// A Markdown or JSON file, read by the document view whenever the tab is active.
    Document,
}

/// Row view being computed.
struct PendingView {
    request: RequestId,
    spec: ViewSpec,
    progress: f32,
}

/// Row view state of a tab. The grid shows only completed row views.
#[derive(Default)]
struct TabView {
    /// Conditions, number and row count of the row view shown in the grid.
    applied: ViewSpec,
    id: ViewId,
    rows: u64,
    pending: Option<PendingView>,
    /// Reason for the last failed computation, and its conditions.
    failed: Option<(SharedString, ViewSpec)>,
}

impl TabView {
    fn new(total_rows: u64) -> Self {
        Self {
            rows: total_rows,
            ..Self::default()
        }
    }

    /// Conditions last set by the user. While computing, these are the conditions being computed.
    fn target(&self) -> &ViewSpec {
        self.pending
            .as_ref()
            .map_or(&self.applied, |pending| &pending.spec)
    }
}

/// Content shown in the tab body.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum CenterMode {
    /// Data grid.
    #[default]
    Data,
    /// File structure.
    Structure,
}

/// What a tab shows: a source read by the data engine, or a document read in full.
#[derive(Clone, Debug, PartialEq, Eq)]
enum TabSource {
    Data(DataSource),
    Document { path: PathBuf, kind: DocumentKind },
}

impl TabSource {
    fn path(&self) -> &Path {
        match self {
            Self::Data(source) => source.path(),
            Self::Document { path, .. } => path,
        }
    }

    fn table(&self) -> Option<&str> {
        match self {
            Self::Data(source) => source.table(),
            Self::Document { .. } => None,
        }
    }

    fn document_kind(&self) -> Option<DocumentKind> {
        match self {
            Self::Data(_) => None,
            Self::Document { kind, .. } => Some(*kind),
        }
    }
}

/// Tab with one open source: a Parquet file, a SQLite table, or a Markdown or JSON document. Each tab has its own
/// schema view so it remembers expanded nodes.
struct Tab {
    session: SessionId,
    source: TabSource,
    state: TabState,
    schema: Entity<SchemaView>,
    /// Grid scroll position and selected cell saved when leaving the tab.
    grid_position: GridPosition,
    view: TabView,
    mode: CenterMode,
    /// Where a document tab was left.
    document: DocumentPosition,
}

impl Tab {
    /// File name, or `table · file name` for a SQLite table.
    fn title(&self) -> SharedString {
        let path = self.source.path();
        let file = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        match self.source.table() {
            Some(table) => format!("{table} · {file}").into(),
            None => file.into(),
        }
    }

    fn is_sqlite(&self) -> bool {
        self.source.table().is_some()
    }
}

/// Full path, with `› table` for a SQLite table.
fn source_label(source: &TabSource) -> String {
    match source.table() {
        Some(table) => format!("{} › {table}", source.path().display()),
        None => source.path().display().to_string(),
    }
}

/// Root view of the window. Owns the tab list, the active tab, session and request numbering, and error display, and lays out the areas.
pub struct AppController {
    focus_handle: FocusHandle,
    worker: ViewerWorker,
    last_session: u64,
    last_request: u64,
    /// Latest visible range request of the active tab that the grid may apply.
    latest_request: Option<RequestId>,
    /// Latest value request of the active tab that the Value panel may apply.
    latest_value: Option<RequestId>,
    tabs: Vec<Tab>,
    active: Option<usize>,
    /// Horizontal scroll state of the tab bar. Scrolls to keep the active tab visible when it changes.
    tab_bar: TabBarState,
    explorer: Entity<ExplorerView>,
    grid: Entity<GridView>,
    value: Entity<ValueView>,
    /// Error when the file or folder dialog itself could not be opened.
    dialog_error: Option<SharedString>,
    find_bar: Entity<FindBar>,
    find_open: bool,
    /// Last searched input. While the find bar is open, the grid highlights cells equal to it.
    find_query: Option<String>,
    latest_find: Option<RequestId>,
    /// Open context menu and its position (window coordinates).
    context_menu: Option<(Entity<Menu>, Point<Pixels>, Subscription)>,
    /// A right-clicked cell whose menu opens once its value arrives: the value request, column and position.
    pending_cell_menu: Option<(RequestId, usize, Point<Pixels>)>,
    /// Notice shown briefly in the filter and sort bar.
    notice: Option<SharedString>,
    /// Conditions being swept in the filter and sort bar: where the drag started and where it is, in the order shown.
    sweep: Option<(usize, usize)>,
    /// Body of the active tab when it shows a document.
    document: Entity<DocumentView>,
    structure: Entity<StructureView>,
    /// Latest request the file structure view may apply.
    latest_structure: Option<RequestId>,
    /// Whether to follow the OS app color setting. Stops following once the theme button is pressed.
    follow_system: bool,
    /// Message file other programs write queries to.
    inbox: Option<Inbox>,
    query_log: Option<QueryLog>,
    usage: UsageMonitor,
    /// The status bar's text for the last reading the user asked for.
    usage_label: Option<SharedString>,
    /// A logged query waiting for its row view to finish: the row view request and the entry to complete.
    pending_log: Option<(RequestId, Entry)>,
    /// Last logged query text and its session, so that a click repeating it is not logged twice.
    last_logged: Option<(SessionId, String)>,
    /// A find from a query waits for the query's row view, since find works on finished row views.
    search_after_view: bool,
    /// Open query input window and its number, which tells its events from those of a window closed earlier.
    #[cfg(target_os = "windows")]
    input_window: Option<(u64, InputWindow)>,
    #[cfg(target_os = "windows")]
    input_windows_opened: u64,
    /// Row view request whose result the query input window waits for.
    input_request: Option<RequestId>,
    _subscriptions: Vec<Subscription>,
}

/// What queries and find answer on a Markdown or JSON tab.
const QUERY_NEEDS_TABLE: &str = "Queries and find work on Parquet and SQLite tabs.";
/// Hint above the text of the query input window.
#[cfg(target_os = "windows")]
const INPUT_HINT: &str = "find, filter and sort lines, such as: filter age >= 30 and status == \"active\". Ctrl+Enter applies.";
/// How often the message file and the open document's file are checked.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

impl AppController {
    pub fn new(
        open_requests: async_channel::Receiver<OpenRequest>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);

        // Esc while sweeping conditions drops the sweep before any key binding sees it.
        let this = cx.entity().downgrade();
        let sweep_escape = cx.intercept_keystrokes(move |event, window, cx| {
            if event.keystroke.key != "escape" {
                return;
            }
            this.update(cx, |this, cx| {
                if this.sweep.take().is_some() {
                    cx.stop_active_drag(window);
                    cx.stop_propagation();
                    cx.notify();
                }
            })
            .ok();
        });

        theme::set_mode(theme::Mode::from_appearance(window.appearance()));
        let appearance_events = cx.observe_window_appearance(window, |this, window, cx| {
            if this.follow_system {
                theme::set_mode(theme::Mode::from_appearance(window.appearance()));
                window.refresh();
                cx.notify();
            }
        });

        // Opening a file from the explorer uses the same path as Open File.
        let explorer = cx.new(ExplorerView::new);
        let explorer_events =
            cx.subscribe(
                &explorer,
                |this, _, event: &ExplorerEvent, cx| match event {
                    ExplorerEvent::Open(source) => this.open_source(source.clone(), cx),
                    ExplorerEvent::OpenDocument(path, kind) => {
                        this.open_document(path.clone(), *kind, cx)
                    }
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
                GridEvent::ContextMenu { target, position } => {
                    this.open_context_menu(*target, *position, window, cx)
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

        let find_bar = cx.new(|_| FindBar::new());
        let find_events = cx.subscribe_in(
            &find_bar,
            window,
            |this, _, event: &FindBarEvent, window, cx| match event {
                FindBarEvent::Search { backward } => this.search(*backward, cx),
                FindBarEvent::Change => this.write_find(window, cx),
                FindBarEvent::Close => this.close_find(window, cx),
            },
        );

        let document = cx.new(|_| DocumentView::new());
        let document_events =
            cx.subscribe(
                &document,
                |this, _, event: &DocumentEvent, cx| match event {
                    DocumentEvent::Value(detail) => this
                        .value
                        .update(cx, |value, cx| value.set_detail((**detail).clone(), cx)),
                    DocumentEvent::ValueGone => this.value.update(cx, |value, cx| value.clear(cx)),
                    DocumentEvent::Read { again: false } => {
                        this.record_usage_soon(UsageEvent::Read, cx)
                    }
                    DocumentEvent::Read { again: true } => {
                        this.record_usage_soon(UsageEvent::Reread, cx)
                    }
                    DocumentEvent::TokensReady => this.record_usage_soon(UsageEvent::Tokens, cx),
                },
            );
        let document_changes = cx.observe(&document, |_, _, cx| cx.notify());

        let value = cx.new(ValueView::new);
        let value_events = cx.subscribe(&value, |this, _, event: &ValueEvent, cx| match event {
            ValueEvent::Expand { path, range } => this.value_op(
                path.clone(),
                ValueOpKind::Children {
                    range: range.clone(),
                },
            ),
            ValueEvent::Select(path) => this.value_op(path.clone(), ValueOpKind::Body),
            ValueEvent::Copy(path) => {
                if this.active_is_document() {
                    this.document
                        .update(cx, |document, cx| document.copy_selected(cx));
                } else {
                    this.value_op(path.clone(), ValueOpKind::Copy);
                }
            }
            ValueEvent::Retry => this.request_value(cx),
        });

        let app_dir = viewer_ui::storage::app_dir("TPUI");
        let inbox = app_dir.as_deref().and_then(|dir| Inbox::create(dir).ok());
        let query_log = app_dir
            .as_deref()
            .map(|dir| QueryLog::new(dir.join("query-log")));
        let mut usage = UsageMonitor::new(app_dir.map(|dir| dir.join("usage-log")));
        usage.record(UsageEvent::Start, None, Helpers::default());
        // Closing the window releases this view on the way to quitting.
        cx.on_release(|this, cx| {
            this.record_usage(UsageEvent::Exit, cx);
        })
        .detach();
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                if this
                    .update_in(cx, |this, window, cx| {
                        this.poll_inbox(window, cx);
                        this.document
                            .update(cx, |document, cx| document.check_for_changes(cx));
                        if this.usage.tick_due(Instant::now()) {
                            this.record_usage(UsageEvent::Tick, cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        cx.spawn_in(window, async move |this, cx| {
            while let Ok(request) = open_requests.recv().await {
                if this
                    .update_in(cx, |this, window, cx| {
                        this.open_path(request.path, cx);
                        if request.bring_forward {
                            window.activate_window();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        // The message file and the input window go with the app.
        let quit = cx.on_app_quit(|this, _| {
            this.inbox = None;
            #[cfg(target_os = "windows")]
            {
                this.input_window = None;
            }
            async {}
        });

        let (worker, events) = ViewerWorker::spawn();
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = events.recv().await {
                if this
                    .update_in(cx, |this, window, cx| {
                        this.handle_worker_event(event, window, cx)
                    })
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
            context_menu: None,
            pending_cell_menu: None,
            notice: None,
            sweep: None,
            document,
            structure,
            latest_structure: None,
            follow_system: true,
            inbox,
            query_log,
            usage,
            usage_label: None,
            pending_log: None,
            last_logged: None,
            search_after_view: false,
            #[cfg(target_os = "windows")]
            input_window: None,
            #[cfg(target_os = "windows")]
            input_windows_opened: 0,
            input_request: None,
            _subscriptions: vec![
                sweep_escape,
                appearance_events,
                explorer_events,
                grid_events,
                value_events,
                find_events,
                structure_events,
                document_events,
                document_changes,
                quit,
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

    /// Picks one file or folder with the native dialog. Cancelling keeps the current state.
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
                    this.dialog_error = Some(format!("Cannot open the dialog: {err:#}").into());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Opens a picked file. A Markdown, JSON or source file opens as a document, and a SQLite database opens its first
    /// table or view by name and reveals the others in the explorer; any other file opens as Parquet.
    fn open_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if let Some(kind) = document_kind(&path) {
            self.open_document(path, kind, cx);
            return;
        }
        if !is_sqlite_file(&path) {
            self.open_source(DataSource::Parquet(path), cx);
            return;
        }
        match sqlite_objects(&path) {
            Ok(objects) => match objects.into_iter().next() {
                Some(first) => self.open_source(
                    DataSource::Sqlite {
                        path,
                        table: first.name,
                    },
                    cx,
                ),
                None => {
                    self.dialog_error =
                        Some(format!("No tables or views in {}.", path.display()).into());
                    cx.notify();
                }
            },
            Err(err) => {
                self.dialog_error = Some(open_error_message(&err).into());
                cx.notify();
            }
        }
    }

    /// Opens a source in a new tab and activates it. If the source is already open in a tab, activates that tab.
    fn open_source(&mut self, source: DataSource, cx: &mut Context<Self>) {
        let tab_source = TabSource::Data(source.clone());
        if self.activate_open_tab(&tab_source, cx) {
            return;
        }
        let session = self.next_session();
        self.worker.open(session, source);
        self.push_tab(session, tab_source, TabState::Opening, cx);
    }

    /// Opens a document file in a new tab and activates it, or activates the tab that already shows it.
    fn open_document(&mut self, path: PathBuf, kind: DocumentKind, cx: &mut Context<Self>) {
        let tab_source = TabSource::Document { path, kind };
        if self.activate_open_tab(&tab_source, cx) {
            return;
        }
        let session = self.next_session();
        self.push_tab(session, tab_source, TabState::Document, cx);
    }

    fn activate_open_tab(&mut self, source: &TabSource, cx: &mut Context<Self>) -> bool {
        let open = self
            .tabs
            .iter()
            .position(|tab| same_tab_source(&tab.source, source));
        if let Some(ix) = open {
            self.activate(ix, cx);
        }
        open.is_some()
    }

    fn push_tab(
        &mut self,
        session: SessionId,
        source: TabSource,
        state: TabState,
        cx: &mut Context<Self>,
    ) {
        self.remember_position(cx);
        self.tabs.push(Tab {
            session,
            source,
            state,
            schema: cx.new(|_| SchemaView::new()),
            grid_position: GridPosition::default(),
            view: TabView::default(),
            mode: CenterMode::default(),
            document: DocumentPosition::default(),
        });
        self.active = Some(self.tabs.len() - 1);
        self.show_active_tab(cx);
    }

    /// The file on screen, for the usage log.
    fn active_file(&self, cx: &App) -> Option<ActiveFile> {
        let tab = self.active_tab()?;
        let size = match &tab.state {
            TabState::Ready(info) => Some(info.file_size),
            TabState::Document => self.document.read(cx).file_size(),
            TabState::Opening | TabState::OpenError(_) => None,
        };
        Some(ActiveFile {
            label: source_label(&tab.source),
            size,
        })
    }

    fn record_usage(&mut self, event: UsageEvent, cx: &App) -> Option<Measured> {
        let file = self.active_file(cx);
        let document = self.document.read(cx);
        let helpers = Helpers {
            core: document.core_usage(),
            pdf: document.pdf_usage(),
        };
        self.usage.record(event, file.as_ref(), helpers)
    }

    /// Records usage once this update has finished, so that what it dropped is gone from the reading.
    fn record_usage_soon(&mut self, event: UsageEvent, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        cx.defer(move |cx| {
            this.update(cx, |this, cx| {
                this.record_usage(event, cx);
            })
            .ok();
        });
    }

    /// Records usage after the active tab changed. A document records when its read finishes, and a tab still opening
    /// when it is open.
    fn record_tab_change(&mut self, cx: &mut Context<Self>) {
        let waiting = matches!(
            self.active_tab().map(|tab| &tab.state),
            Some(TabState::Document | TabState::Opening)
        );
        if !waiting {
            self.record_usage_soon(UsageEvent::Tab, cx);
        }
    }

    /// Takes the reading the user asked for and shows it.
    fn take_usage_snapshot(&mut self, cx: &mut Context<Self>) {
        let label = match self.record_usage(UsageEvent::Snapshot, cx) {
            Some(measured) => measured.label(chrono::Local::now()),
            None => "Usage unavailable".to_string(),
        };
        self.usage_label = Some(label.into());
        cx.notify();
    }

    fn next_session(&mut self) -> SessionId {
        self.last_session += 1;
        SessionId(self.last_session)
    }

    fn active_tab(&self) -> Option<&Tab> {
        self.active.and_then(|ix| self.tabs.get(ix))
    }

    fn active_is_document(&self) -> bool {
        self.active_tab()
            .is_some_and(|tab| matches!(tab.state, TabState::Document))
    }

    fn activate(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.active == Some(ix) || ix >= self.tabs.len() {
            return;
        }
        self.remember_position(cx);
        self.active = Some(ix);
        self.show_active_tab(cx);
        self.record_tab_change(cx);
    }

    /// Saves the active tab's place before leaving it: the grid's scroll position and selected cell, or the document's.
    fn remember_position(&mut self, cx: &mut Context<Self>) {
        let position = self.grid.read(cx).position();
        let document = self.document.read(cx).position(cx);
        if let Some(tab) = self.active.and_then(|ix| self.tabs.get_mut(ix)) {
            match tab.state {
                TabState::Ready(_) => tab.grid_position = position,
                TabState::Document => tab.document = document,
                TabState::Opening | TabState::OpenError(_) => {}
            }
        }
    }

    /// Switches the grid to the active tab's content. Drops the previous tab's cell previews and in-flight range reads.
    fn show_active_tab(&mut self, cx: &mut Context<Self>) {
        self.worker.cancel_window();
        self.latest_request = None;
        // Find results and context menus belong to the previous tab.
        self.worker.cancel_find();
        self.latest_find = None;
        self.find_bar
            .update(cx, |bar, cx| bar.set_status(FindStatus::Idle, cx));
        self.context_menu = None;
        self.pending_cell_menu = None;
        self.notice = None;
        if let Some(ix) = self.active {
            self.tab_bar.scroll_to_tab(ix);
        }
        let active = self.active.and_then(|ix| self.tabs.get(ix));
        match active.map(|tab| &tab.state) {
            // The visible range is requested after the grid computes it during layout.
            Some(TabState::Ready(info)) => {
                let tab = active.expect("active tab");
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
        // Only the active tab's document stays in memory. Returning to a tab reads its file again.
        match active.map(|tab| (&tab.source, &tab.document)) {
            Some((TabSource::Document { path, kind }, position)) => {
                let (path, kind, position) = (path.clone(), *kind, position.clone());
                self.document
                    .update(cx, |document, cx| document.open(path, kind, position, cx));
            }
            _ => self.document.update(cx, |document, cx| document.clear(cx)),
        }
        if let Some(source) = active.map(|tab| tab.source.clone()) {
            self.explorer.update(cx, |explorer, cx| {
                explorer.file_activated(source.path(), source.table(), cx)
            });
        }
        let structure_mode = active.is_some_and(|tab| {
            tab.mode == CenterMode::Structure && matches!(tab.state, TabState::Ready(_))
        });
        if structure_mode {
            self.request_structure(None, cx);
        } else {
            self.latest_structure = None;
        }
        // Release the previous tab's selected value, and request the new tab's selected cell value if any.
        self.request_value(cx);
        cx.notify();
    }

    /// Closes a tab and releases its session. Closing the active tab activates the tab to its right, or else to its left.
    fn close_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(ix);
        if matches!(tab.source, TabSource::Data(_)) {
            self.worker.close(tab.session);
        }
        match self.active {
            Some(active) if active == ix => {
                self.active = if self.tabs.is_empty() {
                    None
                } else {
                    Some(ix.min(self.tabs.len() - 1))
                };
                self.show_active_tab(cx);
                self.record_tab_change(cx);
            }
            Some(active) if active > ix => self.active = Some(active - 1),
            _ => {}
        }
        cx.notify();
    }

    /// Moves a tab from slot `from` to `to`. The active tab stays the same tab after reordering.
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

    /// Reopens the active tab, whose file changed, in place with a new session.
    fn reopen_active(&mut self, cx: &mut Context<Self>) {
        let Some(ix) = self.active else {
            return;
        };
        let TabSource::Data(source) = self.tabs[ix].source.clone() else {
            return;
        };
        self.remember_position(cx);
        let session = self.next_session();
        let tab = &mut self.tabs[ix];
        self.worker.close(tab.session);
        tab.session = session;
        tab.state = TabState::Opening;
        // Keep the row view conditions and recompute them once the file opens.
        let spec = tab.view.target().clone();
        tab.view = TabView {
            applied: spec,
            ..TabView::default()
        };
        tab.schema
            .update(cx, |schema, cx| schema.set_schema(None, cx));
        self.worker.open(session, source);
        self.show_active_tab(cx);
    }

    /// Registers the grid's current visible range under a new request number. Results of earlier requests are ignored.
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

    /// Requests the active tab's selected cell value under a new request number. Without a selected cell, releases the selected value.
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

    /// Sends a follow-up operation on the current selected value to the worker.
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

    // ----- Row view: filter and sort -----

    /// Changes the row view conditions of the active tab. Returning to the displayed conditions drops the in-flight computation.
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

    /// Asks the worker to compute the row view.
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

    /// Each header click cycles ascending, descending, then no sort.
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
                    "Column {} ({}) cannot be sorted.",
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
        self.apply_click(spec, cx);
    }

    /// Sets the sort of the active tab: ascending, descending, or none.
    fn set_sort(&mut self, column: usize, descending: Option<bool>, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let mut spec = tab.view.target().clone();
        spec.sort = descending.map(|descending| SortKey {
            column: ColumnId(column),
            descending,
        });
        self.apply_click(spec, cx);
    }

    /// Adds a condition as a click does (see [`Filter::add`]); `or` asks to join the column's conditions with `or`.
    /// Shows the reason when the value does not parse as the column type.
    fn add_filter(&mut self, condition: FilterCondition, or: bool, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let TabState::Ready(info) = &tab.state else {
            return;
        };
        if let Err(message) = check_condition(&info.schema, &condition) {
            self.notice = Some(message.into());
            cx.notify();
            return;
        }
        let mut spec = tab.view.target().clone();
        spec.filter.add(condition, or);
        self.apply_click(spec, cx);
    }

    fn remove_column_filters(&mut self, column: ColumnId, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let mut spec = tab.view.target().clone();
        spec.filter.retain(|condition| condition.column != column);
        self.apply_click(spec, cx);
    }

    // ----- Context menus -----

    fn open_context_menu(
        &mut self,
        target: MenuTarget,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.context_menu = None;
        self.pending_cell_menu = None;
        match target {
            MenuTarget::Header(column) => {
                if let Some(menu) = self.header_menu(column, cx) {
                    self.show_menu(menu, position, window, cx);
                }
            }
            // Selecting the cell already requested its value. The menu opens once the value arrives as filter input.
            MenuTarget::Cell { column, .. } => {
                if let Some(request) = self.latest_value {
                    self.pending_cell_menu = Some((request, column, position));
                    self.value_op(Vec::new(), ValueOpKind::Cell);
                }
            }
        }
        cx.notify();
    }

    fn show_menu(
        &mut self,
        menu: Entity<Menu>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if menu.read(cx).is_empty() {
            return;
        }
        menu.update(cx, |menu, cx| menu.focus(window, cx));
        let subscription = cx.subscribe_in(
            &menu,
            window,
            |this, _, MenuEvent::Dismissed, window, cx| {
                if this.context_menu.take().is_some() {
                    this.grid.update(cx, |grid, cx| grid.focus(window, cx));
                    cx.notify();
                }
            },
        );
        self.context_menu = Some((menu, position, subscription));
        cx.notify();
    }

    /// Sort and null filters of a column, and removal of its conditions.
    fn header_menu(&self, column: usize, cx: &mut Context<Self>) -> Option<Entity<Menu>> {
        let tab = self.active_tab()?;
        let TabState::Ready(info) = &tab.state else {
            return None;
        };
        let data_type = info.schema.fields()[column].data_type().clone();
        let spec = tab.view.target();
        let sorted = spec
            .sort
            .filter(|sort| sort.column.0 == column)
            .map(|sort| sort.descending);
        let has_filters = spec
            .filter
            .conditions()
            .iter()
            .any(|condition| condition.column.0 == column);
        let ops = filter_ops(&data_type);
        let filter = Rc::new(spec.filter.clone());
        let this = cx.entity().downgrade();
        Some(cx.new(|cx| {
            let mut menu = Menu::new(cx);
            if is_sortable(&data_type) {
                for (label, descending) in [("Sort Ascending", false), ("Sort Descending", true)] {
                    let this = this.clone();
                    menu = menu.item(label, move |_, cx| {
                        this.update(cx, |this, cx| this.set_sort(column, Some(descending), cx))
                            .ok();
                    });
                }
            } else {
                menu = menu.disabled_item("Sort (not sortable)");
            }
            if sorted.is_some() {
                let this = this.clone();
                menu = menu.item("Clear Sort", move |_, cx| {
                    this.update(cx, |this, cx| this.set_sort(column, None, cx))
                        .ok();
                });
            }
            menu = null_items(menu.separator(), &filter, ColumnId(column), ops, &this);
            if has_filters {
                menu = menu.separator().item("Remove Conditions on This Column", {
                    let this = this.clone();
                    move |_, cx| {
                        this.update(cx, |this, cx| {
                            this.remove_column_filters(ColumnId(column), cx)
                        })
                        .ok();
                    }
                });
            }
            menu.separator().item("Write Query…", move |window, cx| {
                this.update(cx, |this, cx| this.write_query(window, cx))
                    .ok();
            })
        }))
    }

    /// Conditions built from a cell's value, finding the value and copying it.
    fn cell_menu(
        &self,
        column: usize,
        value: CellValue,
        cx: &mut Context<Self>,
    ) -> Option<Entity<Menu>> {
        let tab = self.active_tab()?;
        let TabState::Ready(info) = &tab.state else {
            return None;
        };
        let ops = filter_ops(info.schema.fields()[column].data_type());
        let schema = info.schema.clone();
        let filter = Rc::new(tab.view.target().filter.clone());
        let this = cx.entity().downgrade();
        let finder = cx.entity().downgrade();
        let copier = cx.entity().downgrade();
        let writer = cx.entity().downgrade();
        Some(cx.new(|cx| {
            let mut menu = Menu::new(cx);
            match &value {
                CellValue::Null => {
                    menu = null_items(menu, &filter, ColumnId(column), ops, &this);
                }
                CellValue::Text(text) => {
                    let shown: Rc<str> =
                        menu_value(&format_value(&schema, ColumnId(column), text)).into();
                    for op in [
                        FilterOp::Eq,
                        FilterOp::NotEq,
                        FilterOp::Gt,
                        FilterOp::GtEq,
                        FilterOp::Lt,
                        FilterOp::LtEq,
                    ] {
                        if ops.contains(&op) {
                            let shown = shown.clone();
                            menu = condition_item(
                                menu,
                                &filter,
                                FilterCondition {
                                    column: ColumnId(column),
                                    op,
                                    value: text.clone(),
                                },
                                move |op, or| {
                                    let join = if or { "or " } else { "" };
                                    format!("{join}{} {shown}", op_text(op))
                                },
                                &this,
                            );
                        }
                    }
                    menu = menu.note(MODIFIER_NOTE);
                    let text = text.clone();
                    menu = menu
                        .separator()
                        .item(format!("Find {shown}"), move |window, cx| {
                            finder
                                .update(cx, |this, cx| {
                                    this.find_value(text.clone(), window, cx);
                                    this.record(Origin::Click, cx);
                                })
                                .ok();
                        });
                }
                CellValue::NotComparable => {
                    menu = menu.disabled_item("No conditions for nested values");
                }
            }
            menu.separator()
                .item("Copy Value", move |_, cx| {
                    copier
                        .update(cx, |this, _| this.value_op(Vec::new(), ValueOpKind::Copy))
                        .ok();
                })
                .item("Write Query…", move |window, cx| {
                    writer
                        .update(cx, |this, cx| this.write_query(window, cx))
                        .ok();
                })
        }))
    }

    /// Applies conditions changed with the mouse and logs the resulting query.
    fn apply_click(&mut self, spec: ViewSpec, cx: &mut Context<Self>) {
        self.apply_view(spec, cx);
        self.record(Origin::Click, cx);
    }

    // ----- Queries from text -----

    /// Applies a query text from the input window or the message file. The text states the whole view: conditions,
    /// sort and find value. A text that does not parse changes nothing, and the reason is shown and logged.
    fn apply_message(
        &mut self,
        text: String,
        origin: Origin,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let schema = match self.active_tab().map(|tab| &tab.state) {
            Some(TabState::Ready(info)) => info.schema.clone(),
            Some(TabState::Document) => return self.report_message(QUERY_NEEDS_TABLE, cx),
            Some(_) => return self.report_message("The file is still opening.", cx),
            None => return self.report_message("Open a file before sending a query.", cx),
        };
        let query = match parse_query(&schema, &text) {
            Ok(query) => query,
            Err(error) => {
                let message = error.to_string();
                if let Some(tab) = self.active_tab() {
                    let entry = Entry {
                        source: tab.source.path().to_path_buf(),
                        table: tab.source.table().map(str::to_string),
                        origin,
                        query: text.trim().to_string(),
                        rows: None,
                        error: Some(message.clone()),
                    };
                    self.write_log(&entry);
                }
                self.report_message(&message, cx);
                return;
            }
        };
        self.set_mode(CenterMode::Data, cx);
        self.apply_view(query.view, cx);
        match query.find {
            Some(value) => self.find_value(value, window, cx),
            None if self.find_open => self.close_find(window, cx),
            None => {}
        }
        self.record(origin, cx);
    }

    /// Shows a problem with a query text in the filter and sort bar and in the input window.
    fn report_message(&mut self, message: &str, cx: &mut Context<Self>) {
        self.notice = Some(message.to_string().into());
        self.set_input_status(message);
        cx.notify();
    }

    /// Logs the active tab's query after a change. A query that starts a row view computation is logged with its
    /// row count when the computation ends.
    fn record(&mut self, origin: Origin, cx: &mut Context<Self>) {
        let Some(query) = self.query_text() else {
            return;
        };
        let Some(tab) = self.active_tab() else {
            return;
        };
        let session = tab.session;
        let pending = tab.view.pending.as_ref().map(|pending| pending.request);
        let rows = tab.view.rows;
        let entry = Entry {
            source: tab.source.path().to_path_buf(),
            table: tab.source.table().map(str::to_string),
            origin,
            query: query.clone(),
            rows: None,
            error: None,
        };
        if origin == Origin::Click && self.last_logged.as_ref() == Some(&(session, query.clone())) {
            return;
        }
        self.last_logged = Some((session, query));
        let waiting = origin == Origin::Window;
        match pending {
            Some(request) => {
                self.pending_log = Some((request, entry));
                if waiting {
                    self.input_request = Some(request);
                    self.set_input_status("Applying…");
                }
            }
            None => {
                self.write_log(&Entry {
                    rows: Some(rows),
                    ..entry
                });
                if waiting {
                    self.input_request = None;
                    let status = self.rows_status(session, rows);
                    self.set_input_status(&status);
                }
            }
        }
        cx.notify();
    }

    /// Completes the log entry and the input window status of a finished row view.
    fn finish_view_log(
        &mut self,
        session: SessionId,
        request: RequestId,
        result: Result<u64, String>,
    ) {
        if let Some((pending, _)) = &self.pending_log
            && *pending == request
            && let Some((_, entry)) = self.pending_log.take()
        {
            let (rows, error) = match &result {
                Ok(rows) => (Some(*rows), None),
                Err(message) => (None, Some(message.clone())),
            };
            self.write_log(&Entry {
                rows,
                error,
                ..entry
            });
        }
        if self.input_request == Some(request) {
            self.input_request = None;
            let status = match result {
                Ok(rows) => self.rows_status(session, rows),
                Err(message) => message,
            };
            self.set_input_status(&status);
        }
    }

    fn write_log(&mut self, entry: &Entry) {
        if let Some(log) = &self.query_log
            && let Err(error) = log.append(entry)
        {
            self.notice = Some(format!("Cannot write the query log: {error}").into());
        }
    }

    /// `Applied · 1,081 of 280,797 rows`.
    fn rows_status(&self, session: SessionId, rows: u64) -> String {
        let total = self.tabs.iter().find_map(|tab| match &tab.state {
            TabState::Ready(info) if tab.session == session => Some(info.total_rows),
            _ => None,
        });
        match total {
            Some(total) => format!(
                "Applied · {} of {}",
                format_count(rows),
                format_quantity(total, "row", "rows")
            ),
            None => format!("Applied · {}", format_quantity(rows, "row", "rows")),
        }
    }

    fn poll_inbox(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(inbox) = &mut self.inbox else {
            return;
        };
        match inbox.poll() {
            None => {}
            Some(Ok(text)) => self.apply_message(text, Origin::File, window, cx),
            Some(Err(error)) => {
                self.notice = Some(format!("Cannot read the message file: {error}").into());
                cx.notify();
            }
        }
    }

    /// Opens the query input window with the active tab's query, or brings it to the front. Systems without the
    /// input window are pointed to the message file.
    fn write_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.query_text().unwrap_or_default();
        self.open_input(text, window, cx);
    }

    /// Opens the query window to find a value: the conditions first, then a `find` line with the current value, or
    /// an empty one, where the cursor waits.
    fn write_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(TabState::Ready(info)) = self.active_tab().map(|tab| &tab.state) else {
            return self.write_query(window, cx);
        };
        let view = self.active_tab().map(|tab| tab.view.target().clone());
        let conditions = format_query(
            &info.schema,
            &Query {
                view: view.unwrap_or_default(),
                find: None,
            },
        );
        let find = match self.find_open.then(|| self.find_query.clone()).flatten() {
            Some(value) => format_query(
                &info.schema,
                &Query {
                    view: ViewSpec::default(),
                    find: Some(value),
                },
            ),
            None => "find ".to_string(),
        };
        let text = if conditions.is_empty() {
            find
        } else {
            format!("{conditions}\n{find}")
        };
        self.open_input(text, window, cx);
    }

    fn open_input(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(target_os = "windows")]
        {
            if let Some((_, input)) = &self.input_window {
                input.set_text(&text);
                input.set_status("");
                input.show();
                return;
            }
            let owner = input_window::window_handle(window);
            match InputWindow::open(owner, "Query", INPUT_HINT, &text) {
                Ok((input, events)) => {
                    self.input_windows_opened += 1;
                    let number = self.input_windows_opened;
                    self.input_window = Some((number, input));
                    cx.spawn_in(window, async move |this, cx| {
                        while let Ok(event) = events.recv().await {
                            let closed =
                                matches!(event, InputEvent::Closed | InputEvent::Failed(_));
                            let alive = this.update_in(cx, |this, window, cx| match event {
                                InputEvent::Submit(text) => {
                                    this.apply_message(text, Origin::Window, window, cx)
                                }
                                InputEvent::Failed(error) => {
                                    this.notice = Some(
                                        format!("Cannot open the query window: {error}").into(),
                                    );
                                    this.input_window = None;
                                    cx.notify();
                                }
                                InputEvent::Closed => {
                                    if this
                                        .input_window
                                        .as_ref()
                                        .is_some_and(|(open, _)| *open == number)
                                    {
                                        this.input_window = None;
                                        this.input_request = None;
                                    }
                                }
                            });
                            if alive.is_err() || closed {
                                break;
                            }
                        }
                    })
                    .detach();
                }
                Err(error) => {
                    self.notice = Some(format!("Cannot open the query window: {error}").into());
                    cx.notify();
                }
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (window, text);
            self.notice = Some(match &self.inbox {
                Some(inbox) => format!("Write the query to {}", inbox.path().display()).into(),
                None => "No message file is available.".into(),
            });
            cx.notify();
        }
    }

    fn set_input_status(&self, status: &str) {
        #[cfg(target_os = "windows")]
        if let Some((_, input)) = &self.input_window {
            input.set_status(status);
        }
        #[cfg(not(target_os = "windows"))]
        let _ = status;
    }

    /// Query actions of the toolbar.
    fn open_query_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = cx.entity().downgrade();
        let inbox = self.inbox.as_ref().map(|inbox| inbox.path().to_path_buf());
        let log_dir = self.query_log.as_ref().map(|log| log.dir().to_path_buf());
        let has_query = self.query_text().is_some();
        let menu = cx.new(|cx| {
            let writer = this.clone();
            let mut menu = Menu::new(cx).item("Write Query…", move |window, cx| {
                writer
                    .update(cx, |this, cx| this.write_query(window, cx))
                    .ok();
            });
            if has_query {
                let copier = this.clone();
                menu = menu.item("Copy Query", move |_, cx| {
                    copier
                        .update(cx, |this, cx| {
                            let text = this.query_text().unwrap_or_default();
                            cx.write_to_clipboard(ClipboardItem::new_string(text));
                        })
                        .ok();
                });
            }
            menu = menu.separator();
            if let Some(path) = inbox {
                menu = menu.item("Copy Message File Path", move |_, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(path.display().to_string()));
                });
            }
            if let Some(dir) = log_dir {
                menu = menu.item("Open Query Log Folder", move |_, cx| {
                    let _ = std::fs::create_dir_all(&dir);
                    cx.open_with_system(&dir);
                });
            }
            menu
        });
        self.show_menu(menu, position, window, cx);
    }

    /// The active tab's query as text: its target conditions and, while the find bar is open, the find value.
    fn query_text(&self) -> Option<String> {
        let tab = self.active_tab()?;
        let TabState::Ready(info) = &tab.state else {
            return None;
        };
        let query = Query {
            view: tab.view.target().clone(),
            find: self.find_open.then(|| self.find_query.clone()).flatten(),
        };
        Some(format_query(&info.schema, &query))
    }

    fn view_bar_action(
        &mut self,
        action: ViewBarAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let mut spec = tab.view.target().clone();
        match action {
            ViewBarAction::RemoveFilter { clause, ix } => {
                spec.filter.remove(clause, ix);
            }
            ViewBarAction::ChooseJoin {
                clause,
                ix,
                position,
            } => return self.open_logic_menu(clause, ix, position, window, cx),
            ViewBarAction::RemoveSort => spec.sort = None,
            ViewBarAction::CopyQuery => {
                let text = self.query_text().unwrap_or_default();
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                return;
            }
            ViewBarAction::Sweep { from, to } => {
                if self.sweep != Some((from, to)) {
                    self.sweep = Some((from, to));
                    cx.notify();
                }
                return;
            }
            ViewBarAction::EndSweep => {
                let Some((from, to)) = self.sweep.take() else {
                    return;
                };
                cx.notify();
                if !spec.filter.join_range(from, to) {
                    return;
                }
            }
            ViewBarAction::ClearAll => spec = ViewSpec::default(),
            ViewBarAction::Cancel => spec = tab.view.applied.clone(),
            ViewBarAction::Retry => match &tab.view.failed {
                Some((_, failed)) => spec = failed.clone(),
                None => return,
            },
        }
        self.apply_click(spec, cx);
    }

    /// The choice of `and` and `or` after condition `ix` of clause `clause`. Inside a clause `and` splits it there;
    /// between two clauses on one column `or` merges them. Between columns only `and` can be chosen.
    fn open_logic_menu(
        &mut self,
        clause: usize,
        ix: usize,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let Some((current, changeable)) = tab.view.target().filter.join_after(clause, ix) else {
            return;
        };
        let this = cx.entity().downgrade();
        let menu = cx.new(|cx| {
            let mut menu = Menu::new(cx);
            for op in LogicOp::ALL {
                if op == current {
                    menu = menu.item(format!("✓ {}", op.text()), |_, _| {});
                } else if changeable {
                    let this = this.clone();
                    menu = menu.item(format!("   {}", op.text()), move |_, cx| {
                        this.update(cx, |this, cx| this.set_join(clause, ix, op, cx))
                            .ok();
                    });
                } else {
                    menu = menu
                        .disabled_item(format!("   {}", op.text()))
                        .note("or joins conditions on one column only");
                }
            }
            menu
        });
        self.show_menu(menu, position, window, cx);
    }

    fn set_join(&mut self, clause: usize, ix: usize, op: LogicOp, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let mut spec = tab.view.target().clone();
        spec.filter.set_join(clause, ix, op);
        self.apply_click(spec, cx);
    }

    // ----- File structure -----

    /// Switches the active tab's body between the data grid and the file structure.
    fn set_mode(&mut self, mode: CenterMode, cx: &mut Context<Self>) {
        let Some(ix) = self.active else {
            return;
        };
        if self.tabs[ix].mode == mode {
            return;
        }
        self.tabs[ix].mode = mode;
        self.context_menu = None;
        self.pending_cell_menu = None;
        if mode == CenterMode::Structure {
            self.request_structure(None, cx);
        }
        cx.notify();
    }

    /// Requests the active tab's file or table structure. With `row_group`, requests that Row Group's column
    /// information.
    fn request_structure(&mut self, row_group: Option<usize>, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let TabState::Ready(info) = &tab.state else {
            return;
        };
        let session = info.session;
        let table = tab.is_sqlite();
        self.last_request += 1;
        let request = RequestId(self.last_request);
        self.latest_structure = Some(request);
        self.worker.request_structure(StructureRequest {
            session,
            request,
            table,
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

    // ----- Find -----

    /// `Ctrl+F`: the find bar has no input, so the query window opens with a `find` line to write the value in.
    fn open_find(&mut self, _: &Find, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_tab().is_none() {
            return;
        }
        if self.active_is_document() {
            self.notice = Some(QUERY_NEEDS_TABLE.into());
            cx.notify();
            return;
        }
        // Find works in the data grid.
        self.set_mode(CenterMode::Data, cx);
        self.write_find(window, cx);
    }

    fn close_find_action(&mut self, _: &CloseFind, window: &mut Window, cx: &mut Context<Self>) {
        if self.find_open {
            self.close_find(window, cx);
        }
    }

    /// Opens the find bar with `text` and finds its next match from the selected cell.
    fn find_value(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.set_mode(CenterMode::Data, cx);
        self.find_open = true;
        self.find_bar
            .update(cx, |bar, cx| bar.set_query(text.clone(), cx));
        // Find works on finished row views; a find sent with new conditions runs once they are applied.
        if self
            .active_tab()
            .is_some_and(|tab| tab.view.pending.is_some())
        {
            self.find_query = Some(text);
            self.search_after_view = true;
        } else {
            self.search(false, cx);
        }
        self.grid.update(cx, |grid, cx| grid.focus(window, cx));
        cx.notify();
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

    /// Finds the next (previous) match from the selected cell. If the input changed, updates the grid highlight too.
    fn search(&mut self, backward: bool, cx: &mut Context<Self>) {
        let query = self.find_bar.read(cx).query().to_string();
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
                    FindStatus::Failed(
                        "Find is available once the filter and sort are applied.".into(),
                    ),
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

    fn handle_worker_event(
        &mut self,
        event: WorkerEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            WorkerEvent::Opened { session, result } => {
                // Drop responses for previous sessions of closed or reopened tabs.
                let Some(ix) = self.tabs.iter().position(|tab| tab.session == session) else {
                    return;
                };
                let tab = &mut self.tabs[ix];
                tab.state = match result {
                    Ok(info) => {
                        let schema = info.schema.clone();
                        tab.schema
                            .update(cx, |view, cx| view.set_schema(Some(schema), cx));
                        // A reopened tab recomputes its remembered row view conditions.
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
                    self.record_usage_soon(UsageEvent::Open, cx);
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
                // A new row view starts from the first row. The horizontal position is kept.
                let position = if self.active == Some(ix) {
                    self.grid.read(cx).position()
                } else {
                    self.tabs[ix].grid_position
                };
                let tab = &mut self.tabs[ix];
                let pending = tab.view.pending.take().expect("row view being computed");
                tab.view.applied = pending.spec;
                tab.view.id = Some(request);
                tab.view.rows = rows;
                tab.view.failed = None;
                tab.grid_position = position.at_first_row();
                if self.active == Some(ix) {
                    self.show_active_tab(cx);
                    if std::mem::take(&mut self.search_after_view) {
                        self.search(false, cx);
                    }
                }
                self.finish_view_log(session, request, Ok(rows));
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
                let pending = tab.view.pending.take().expect("row view being computed");
                let message = match &error {
                    ViewerError::InvalidCondition(message) | ViewerError::Unsortable(message) => {
                        message.clone()
                    }
                    other => read_error_message(other),
                };
                tab.view.failed = Some((message.clone().into(), pending.spec));
                self.search_after_view = false;
                self.finish_view_log(session, request, Err(message));
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
            WorkerEvent::TableStructure {
                session,
                request,
                result,
            } => {
                if self.is_latest_structure(session, request) {
                    self.structure.update(cx, |structure, cx| match result {
                        Ok(table) => structure.set_table(table, cx),
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
            WorkerEvent::ValueCell {
                session,
                request,
                value,
            } => {
                if self.is_latest_value(session, request)
                    && let Some((pending, column, position)) = self.pending_cell_menu
                    && pending == request
                {
                    self.pending_cell_menu = None;
                    if let Some(menu) = self.cell_menu(column, value, cx) {
                        self.show_menu(menu, position, window, cx);
                    }
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
            .h(style::TOOLBAR_HEIGHT)
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
            .child(toolbar_button("query", "Query").on_click(cx.listener(
                |this, event: &ClickEvent, window, cx| {
                    this.open_query_menu(event.position(), window, cx)
                },
            )))
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

    /// Draws the tab bar and the active tab's grid if there are tabs, or the empty state otherwise.
    fn render_center(&self, cx: &mut Context<Self>) -> Div {
        let Some(tab) = self.active_tab() else {
            return panel("Data Grid").child(placeholder(
                "Open a Parquet, SQLite, Markdown or JSON file with Open File or Ctrl+O.",
            ));
        };
        let labels = self
            .tabs
            .iter()
            .map(|tab| TabLabel {
                title: tab.title(),
                path: source_label(&tab.source).into(),
                status: match tab.state {
                    TabState::Opening => TabStatus::Opening,
                    TabState::Ready(_) => TabStatus::Ready,
                    TabState::OpenError(_) => TabStatus::Error,
                    TabState::Document => TabStatus::Ready,
                },
            })
            .collect();
        let select = cx.listener(|this, ix: &usize, _, cx| this.activate(*ix, cx));
        let close = cx.listener(|this, ix: &usize, _, cx| this.close_tab(*ix, cx));
        let move_tab =
            cx.listener(|this, (from, to): &(usize, usize), _, cx| this.move_tab(*from, *to, cx));
        let body = match &tab.state {
            TabState::Opening => placeholder("Opening"),
            TabState::Ready(_) if tab.mode == CenterMode::Structure => {
                div().flex_1().min_h_0().child(self.structure.clone())
            }
            TabState::Ready(_) => div().flex_1().min_h_0().child(self.grid.clone()),
            TabState::Document => div().flex_1().min_h_0().child(self.document.clone()),
            TabState::OpenError(message) => placeholder(if tab.is_sqlite() {
                "Cannot open the table."
            } else {
                "Cannot open the file."
            })
            .flex_col()
            .gap_2()
            .text_color(theme::error())
            .child(div().text_color(theme::text()).child(message.clone()))
            .child(
                div()
                    .text_color(theme::text_muted())
                    .child(source_label(&tab.source)),
            ),
        };
        let view_bar = match &tab.state {
            TabState::Ready(info) if tab.mode == CenterMode::Data => {
                let view_action = cx.listener(|this, action: &ViewBarAction, window, cx| {
                    this.view_bar_action(action.clone(), window, cx)
                });
                let filter = &tab.view.target().filter;
                let sweep = self.sweep.map(|(from, to)| {
                    let joinable = filter.can_join_range(from, to);
                    let hint: SharedString = if from == to {
                        "Drag across conditions on one column to join them with or".into()
                    } else if joinable {
                        format!(
                            "Release to join {} conditions with or",
                            from.abs_diff(to) + 1
                        )
                        .into()
                    } else {
                        "or joins conditions on one column only".into()
                    };
                    (SweepRange { from, to, joinable }, hint)
                });
                let bar = ViewBar {
                    sweep,
                    filter: tab
                        .view
                        .target()
                        .filter
                        .clauses()
                        .iter()
                        .map(|clause| {
                            clause
                                .conditions()
                                .iter()
                                .map(|condition| format_condition(&info.schema, condition).into())
                                .collect()
                        })
                        .collect(),
                    sort: tab
                        .view
                        .target()
                        .sort
                        .map(|sort| format_sort(&info.schema, sort).into()),
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
                .h(theme::metrics().panel_header_height)
                .px_2()
                .flex()
                .items_center()
                .gap_1()
                .bg(theme::surface())
                .border_b_1()
                .border_l_1()
                .border_color(theme::border())
                .child(mode_button("mode-data", "Data", CenterMode::Data))
                .child(mode_button(
                    "mode-structure",
                    if tab.is_sqlite() {
                        "Table Structure"
                    } else {
                        "File Structure"
                    },
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

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let path: SharedString = match self.active_tab() {
            Some(tab) => source_label(&tab.source).into(),
            None => "No file".into(),
        };
        let mut summary: Vec<String> = Vec::new();
        let mut is_error = false;
        let state = match self.active_tab().map(|tab| &tab.state) {
            None => "Idle",
            Some(TabState::Opening) => "Opening",
            Some(TabState::Ready(info)) => {
                let view = &self.active_tab().expect("active tab").view;
                if view.applied.filter.is_empty() {
                    summary.push(format_quantity(info.total_rows, "row", "rows"));
                } else {
                    summary.push(format!(
                        "{} of {}",
                        format_count(view.rows),
                        format_quantity(info.total_rows, "row", "rows")
                    ));
                }
                summary.push(format_quantity(
                    info.schema.fields().len() as u64,
                    "column",
                    "columns",
                ));
                if info.source.table().is_none() {
                    summary.push(format_quantity(
                        info.row_group_rows.len() as u64,
                        "Row Group",
                        "Row Groups",
                    ));
                }
                summary.push(format_bytes(info.file_size));
                match self.grid.read(cx).load_state() {
                    GridLoadState::Loading => "Reading",
                    GridLoadState::Loaded => "Open",
                    GridLoadState::ReadError => {
                        is_error = true;
                        "Read error"
                    }
                }
            }
            Some(TabState::OpenError(_)) => {
                is_error = true;
                "Open failed"
            }
            Some(TabState::Document) => {
                let document = self.document.read(cx);
                summary.extend(document.summary(cx));
                match document.status() {
                    DocumentStatus::Reading => "Reading",
                    DocumentStatus::Open => "Open",
                    DocumentStatus::Failed => {
                        is_error = true;
                        "Read error"
                    }
                }
            }
        };
        let state_color = if is_error {
            theme::error()
        } else {
            theme::text_muted()
        };

        div()
            .flex_none()
            .h(style::STATUS_BAR_HEIGHT)
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
            .children(
                self.active_is_document()
                    .then(|| self.notice.clone())
                    .flatten()
                    .map(|notice| div().min_w_0().truncate().child(notice)),
            )
            .children(
                self.active_is_document()
                    .then(|| self.document.read(cx).plain_note())
                    .flatten()
                    .map(|note| div().min_w_0().truncate().child(note)),
            )
            .child(
                small_button(
                    "usage-snapshot",
                    self.usage_label.clone().unwrap_or_else(|| "Usage".into()),
                )
                .ml_auto()
                .on_click(cx.listener(|this, _, _, cx| this.take_usage_snapshot(cx))),
            )
    }
}

impl Render for AppController {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A sweep released away from the conditions ends without a drop.
        if self.sweep.is_some() && !cx.has_active_drag() {
            self.sweep = None;
        }
        let schema = self.active_tab().map(|tab| tab.schema.clone());
        // Markdown, code and PDF use the whole width, and a JSON document has no schema of its own.
        let document_kind = self.active_tab().and_then(|tab| tab.source.document_kind());
        let reads_across = matches!(
            document_kind,
            Some(DocumentKind::Markdown | DocumentKind::Code | DocumentKind::Pdf)
        );
        let side_panel = (!reads_across).then(|| {
            div()
                .flex_none()
                .w(style::SIDE_PANEL_WIDTH)
                .flex()
                .flex_col()
                .border_l_1()
                .border_color(theme::border())
                .when(document_kind.is_none(), |side| {
                    side.child(
                        panel("Schema")
                            .h(relative(0.4))
                            .border_b_1()
                            .child(div().flex_1().min_h_0().children(schema)),
                    )
                })
                .child(
                    panel("Value")
                        .flex_1()
                        .min_h_0()
                        .child(div().flex_1().min_h_0().child(self.value.clone())),
                )
        });
        div()
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::open_file))
            .on_action(cx.listener(Self::open_folder))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::previous_tab))
            .on_action(cx.listener(Self::close_active_tab))
            .on_action(cx.listener(Self::open_find))
            .on_action(cx.listener(Self::close_find_action))
            .on_action(cx.listener(Self::find_next))
            .on_action(cx.listener(Self::find_previous))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::background())
            .font_family(theme::ui_font())
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
                            .w(style::EXPLORER_WIDTH)
                            .border_r_1()
                            .child(div().flex_1().min_h_0().child(self.explorer.clone())),
                    )
                    .child(self.render_center(cx).flex_1().min_w_0())
                    .children(side_panel),
            )
            .child(self.render_status_bar(cx))
            .children(
                self.context_menu
                    .as_ref()
                    .map(|(menu, position, _)| menu::overlay(menu, *position)),
            )
    }
}

/// Sort and filter state shown in a header.
fn header_marks(spec: &ViewSpec) -> HeaderMarks {
    HeaderMarks {
        sort: spec.sort.map(|sort| (sort.column.0, sort.descending)),
        filtered: spec
            .filter
            .conditions()
            .iter()
            .map(|condition| condition.column.0)
            .collect(),
    }
}

/// What Shift and Alt do on the menu items that add conditions.
const MODIFIER_NOTE: &str = "Shift: or · Alt: not";

/// A menu item adding a condition. A plain click adds it as [`Filter::add`] does, Shift+click joins its column's
/// conditions with `or`, and Alt+click adds the opposite condition (`!=` for `==`). The label, from
/// `label(op, or)`, shows what a click with the keys held adds, or a check mark when the condition is already
/// there.
fn condition_item(
    menu: Menu,
    filter: &Rc<Filter>,
    condition: FilterCondition,
    label: impl Fn(FilterOp, bool) -> String + 'static,
    this: &WeakEntity<AppController>,
) -> Menu {
    fn chosen(condition: &FilterCondition, modifiers: Modifiers) -> FilterCondition {
        let op = match modifiers.alt {
            true => condition.op.opposite().unwrap_or(condition.op),
            false => condition.op,
        };
        FilterCondition {
            op,
            ..condition.clone()
        }
    }
    let (filter, shown, this) = (filter.clone(), condition.clone(), this.clone());
    menu.item_with_modifiers(
        move |modifiers| {
            let condition = chosen(&shown, modifiers);
            if !filter.adds(&condition, modifiers.shift) {
                return format!("✓ {}", label(condition.op, false)).into();
            }
            let or = filter.or_clause(&condition, modifiers.shift).is_some();
            label(condition.op, or).into()
        },
        move |modifiers, _, cx| {
            let condition = chosen(&condition, modifiers);
            this.update(cx, |this, cx| {
                this.add_filter(condition, modifiers.shift, cx)
            })
            .ok();
        },
    )
}

/// `Only Nulls` and `Exclude Nulls` for a column, with the note on Shift and Alt.
fn null_items(
    mut menu: Menu,
    filter: &Rc<Filter>,
    column: ColumnId,
    ops: &[FilterOp],
    this: &WeakEntity<AppController>,
) -> Menu {
    for op in [FilterOp::IsNull, FilterOp::IsNotNull] {
        if ops.contains(&op) {
            let condition = FilterCondition {
                column,
                op,
                value: String::new(),
            };
            menu = condition_item(
                menu,
                filter,
                condition,
                |op, or| match (op, or) {
                    (_, true) => format!("or {}", op_text(op)),
                    (FilterOp::IsNull, false) => "Only Nulls".into(),
                    _ => "Exclude Nulls".into(),
                },
                this,
            );
        }
    }
    menu.note(MODIFIER_NOTE)
}

/// A value in query form as shown in a menu item, shortened when long. A shortened quoted value keeps its closing
/// quote.
fn menu_value(value: &str) -> String {
    const LIMIT: usize = 40;
    let mut chars = value.chars();
    let shown: String = chars.by_ref().take(LIMIT).collect();
    if chars.next().is_none() {
        return shown;
    }
    if value.starts_with('"') {
        format!("{shown}…\"")
    } else {
        format!("{shown}…")
    }
}

/// New position of the tab originally at `ix` after moving one tab from `from` to `to`.
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

/// Whether two paths point to the same file. Windows paths are case-insensitive.
fn same_path(a: &Path, b: &Path) -> bool {
    a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
}

/// Whether two sources are the same file, and for SQLite the same table. Table names compare exactly.
fn same_source(a: &DataSource, b: &DataSource) -> bool {
    match (a, b) {
        (DataSource::Parquet(a), DataSource::Parquet(b)) => same_path(a, b),
        (DataSource::Sqlite { path: a, table: x }, DataSource::Sqlite { path: b, table: y }) => {
            same_path(a, b) && x == y
        }
        _ => false,
    }
}

/// Whether two tabs show the same thing.
fn same_tab_source(a: &TabSource, b: &TabSource) -> bool {
    match (a, b) {
        (TabSource::Data(a), TabSource::Data(b)) => same_source(a, b),
        (TabSource::Document { path: a, .. }, TabSource::Document { path: b, .. }) => {
            same_path(a, b)
        }
        _ => false,
    }
}

/// Turns an engine error into a user message.
fn open_error_message(err: &ViewerError) -> String {
    match err {
        ViewerError::Io(io_err) => match io_err.kind() {
            io::ErrorKind::NotFound => "File not found.".into(),
            io::ErrorKind::PermissionDenied => "No permission to read the file.".into(),
            _ => format!("Cannot read the file. ({io_err})"),
        },
        ViewerError::InvalidParquet(parquet_err) => {
            format!("Not a Parquet file, or its metadata is corrupted. ({parquet_err})")
        }
        ViewerError::InvalidRowCount => "The Row Group row counts are invalid.".into(),
        ViewerError::Database(message) => format!("Cannot read the database. ({message})"),
        other => other.to_string(),
    }
}

/// Turns a data read error into a user message.
fn read_error_message(err: &ViewerError) -> String {
    match err {
        ViewerError::Decode(decode_err) => {
            format!("Cannot decode the data. ({decode_err})")
        }
        ViewerError::Io(io_err) => format!("Cannot read the file. ({io_err})"),
        ViewerError::Database(message) => format!("Cannot read the database. ({message})"),
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

/// Empty panel with a title row. The caller adds the size and body.
fn panel(title: &'static str) -> Div {
    div()
        .flex()
        .flex_col()
        .overflow_hidden()
        .border_color(theme::border())
        .child(
            div()
                .flex_none()
                .h(theme::metrics().panel_header_height)
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
    fn shortens_long_menu_values() {
        assert_eq!(menu_value("\"중구01\""), "\"중구01\"");
        assert_eq!(menu_value("1.5"), "1.5");
        let quoted = format!("\"{}\"", "가".repeat(45));
        assert_eq!(menu_value(&quoted), format!("\"{}…\"", "가".repeat(39)));
        assert_eq!(menu_value(&"9".repeat(50)), format!("{}…", "9".repeat(40)));
    }
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

    #[test]
    fn tells_sqlite_tables_of_one_file_apart() {
        let table = |path: &str, table: &str| DataSource::Sqlite {
            path: path.into(),
            table: table.into(),
        };
        assert!(same_source(
            &table(r"C:\Data\x.db", "a"),
            &table(r"c:\data\X.DB", "a")
        ));
        assert!(!same_source(
            &table(r"C:\Data\x.db", "a"),
            &table(r"C:\Data\x.db", "b")
        ));
        assert!(!same_source(
            &table(r"C:\Data\x.db", "a"),
            &DataSource::Parquet(r"C:\Data\x.db".into())
        ));
    }

    #[test]
    fn tells_documents_and_data_sources_apart() {
        let document = |path: &str| TabSource::Document {
            path: path.into(),
            kind: DocumentKind::Markdown,
        };
        assert!(same_tab_source(
            &document(r"C:\Data\a.md"),
            &document(r"c:\data\A.MD")
        ));
        assert!(!same_tab_source(
            &document(r"C:\Data\a.md"),
            &document(r"C:\Data\b.md")
        ));
        assert!(!same_tab_source(
            &document(r"C:\Data\a.md"),
            &TabSource::Data(DataSource::Parquet(r"C:\Data\a.md".into()))
        ));
    }
}
