//! Dedicated thread running the data engine, and the request mailbox.
//!
//! The UI puts the latest requests in the mailbox and wakes the worker. The worker calls the engine outside
//! the mailbox lock and sends results over a bounded channel. The UI thread never waits for the worker.
//!
//! Row view and find computations advance one scan batch at a time. Open, close, visible range and
//! value requests are handled between batches, so scrolling and cell selection keep working during computation.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use arrow_array::ArrayRef;
use viewer_core::{
    ColumnChunkInfo, ColumnId, DataSource, DatasetInfo, DecodedBlock, FileStructure, FindIndex,
    FindQuery, FindTask, RequestId, RowView, RowViewTask, SessionId, TableStructure, TaskStep,
    ValueRequest, ValueSlice, ViewSpec, ViewerEngine, ViewerError, WindowRequest,
};

use crate::cell_preview::{CellPreview, preview_column};
use crate::value_detail::{self, CellValue, ValueBody, ValueDetail, ValueNode};

/// Maximum number of messages queued in the result channel.
const RESULT_CHANNEL_CAPACITY: usize = 2;
/// Minimum interval between progress updates sent to the UI.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// Row view number: the `RequestId` of the row view request. `None` is the source order, before the
/// row view changed after opening the file.
pub type ViewId = Option<RequestId>;

/// Results the worker sends to the UI.
pub enum WorkerEvent {
    Opened {
        session: SessionId,
        result: Result<DatasetInfo, ViewerError>,
    },
    /// Cell previews of some rows of the visible range.
    Cells {
        session: SessionId,
        request: RequestId,
        cells: WindowCells,
    },
    /// Rows of the visible range that could not be read.
    ReadFailed {
        session: SessionId,
        request: RequestId,
        rows: Range<u64>,
        error: ViewerError,
    },
    /// Root information and body of the selected cell.
    ValueLoaded {
        session: SessionId,
        request: RequestId,
        detail: ValueDetail,
    },
    /// Children of a value tree node, starting at `start`.
    ValueChildren {
        session: SessionId,
        request: RequestId,
        path: Vec<usize>,
        start: usize,
        children: Vec<ValueNode>,
    },
    ValueBody {
        session: SessionId,
        request: RequestId,
        path: Vec<usize>,
        body: ValueBody,
    },
    ValueCopy {
        session: SessionId,
        request: RequestId,
        text: String,
    },
    /// The selected value as filter and find input.
    ValueCell {
        session: SessionId,
        request: RequestId,
        value: CellValue,
    },
    ValueFailed {
        session: SessionId,
        request: RequestId,
        error: ViewerError,
    },
    /// Progress of the row view computation, from 0 to 1.
    ViewProgress {
        session: SessionId,
        request: RequestId,
        progress: f32,
    },
    /// The row view is complete. Later requests use this number.
    ViewReady {
        session: SessionId,
        request: RequestId,
        rows: u64,
    },
    ViewFailed {
        session: SessionId,
        request: RequestId,
        error: ViewerError,
    },
    FindProgress {
        session: SessionId,
        request: RequestId,
        progress: f32,
    },
    /// Number of matching cells in the whole row view, and the found cell (row view position, column).
    FindResult {
        session: SessionId,
        request: RequestId,
        cells: u64,
        found: Option<(u64, ColumnId)>,
    },
    FindFailed {
        session: SessionId,
        request: RequestId,
        error: ViewerError,
    },
    /// Structure of a SQLite table or view.
    TableStructure {
        session: SessionId,
        request: RequestId,
        result: Result<TableStructure, ViewerError>,
    },
    /// File structure, built from metadata alone.
    FileStructure {
        session: SessionId,
        request: RequestId,
        result: Result<FileStructure, ViewerError>,
    },
    /// Per-column storage information of one Row Group.
    RowGroupColumns {
        session: SessionId,
        request: RequestId,
        row_group: usize,
        result: Result<Vec<ColumnChunkInfo>, ViewerError>,
    },
}

/// Structure request. For a SQLite source (`table`), the table structure; for Parquet, the file structure, or with
/// `row_group` that Row Group's per-column information.
pub struct StructureRequest {
    pub session: SessionId,
    pub request: RequestId,
    pub table: bool,
    pub row_group: Option<usize>,
}

/// Follow-up operation on the selected value. Dropped when the value request changes.
pub struct ValueOp {
    pub request: RequestId,
    pub path: Vec<usize>,
    pub kind: ValueOpKind,
}

pub enum ValueOpKind {
    Children {
        range: Range<usize>,
    },
    Body,
    Copy,
    /// The value as filter and find input.
    Cell,
}

/// Cell previews of some rows of the visible range. `previews[c][r]` is column `columns[c]` at row view position
/// `rows.start + r`, whose source row number is `row_numbers[r]`.
pub struct WindowCells {
    pub rows: Range<u64>,
    pub row_numbers: Vec<u64>,
    pub columns: Vec<ColumnId>,
    pub previews: Vec<Vec<CellPreview>>,
}

/// Find request. `from` is the row view position and column of the starting cell.
pub struct FindRequest {
    pub session: SessionId,
    pub request: RequestId,
    pub view: ViewId,
    pub query: String,
    pub from: Option<(u64, ColumnId)>,
    pub backward: bool,
}

/// Command opening or closing the file session of one tab. All are processed in request order.
enum Command {
    Open {
        session: SessionId,
        source: DataSource,
    },
    Close {
        session: SessionId,
    },
}

/// Visible range request. Rows are row view positions. With `find`, cells equal to that input are marked.
struct WindowJob {
    view: ViewId,
    request: WindowRequest,
    find: Option<String>,
}

/// Value request. The row is a row view position.
struct ValueJob {
    view: ViewId,
    request: ValueRequest,
}

/// Pending requests. Open and close commands queue in order; visible range, value and find requests keep only
/// the latest; row view requests keep the latest per session.
#[derive(Default)]
struct Mailbox {
    commands: VecDeque<Command>,
    window: Option<WindowJob>,
    value: Option<ValueJob>,
    value_ops: VecDeque<ValueOp>,
    /// Latest value request of the active tab. Decides whether the kept selected value and follow-ups are still valid.
    latest_value: Option<RequestId>,
    /// Sessions open as tabs. Used to drop work and results of closed tabs.
    live_sessions: HashSet<SessionId>,
    /// Latest visible range request of the active tab. Decides whether an in-flight read is still valid.
    latest_request: Option<RequestId>,
    /// Row view requests whose computation has not started. The latest per session.
    views: VecDeque<(SessionId, RequestId, ViewSpec)>,
    /// Latest row view request per session. Decides whether an in-flight computation is still valid.
    latest_view: HashMap<SessionId, RequestId>,
    find: Option<FindRequest>,
    latest_find: Option<RequestId>,
    /// Latest file structure request.
    structure: Option<StructureRequest>,
    shutdown: bool,
}

impl Mailbox {
    fn is_current(&self, session: SessionId, request: Option<RequestId>) -> bool {
        !self.shutdown
            && self.live_sessions.contains(&session)
            && (request.is_none() || self.latest_request == request)
    }

    fn is_value_current(&self, session: SessionId, request: RequestId) -> bool {
        !self.shutdown
            && self.live_sessions.contains(&session)
            && self.latest_value == Some(request)
    }

    fn is_view_current(&self, session: SessionId, request: RequestId) -> bool {
        !self.shutdown
            && self.live_sessions.contains(&session)
            && self.latest_view.get(&session) == Some(&request)
    }

    fn is_find_current(&self, session: SessionId, request: RequestId) -> bool {
        !self.shutdown && self.live_sessions.contains(&session) && self.latest_find == Some(request)
    }

    fn push_open(&mut self, session: SessionId, source: DataSource) {
        self.live_sessions.insert(session);
        self.commands.push_back(Command::Open { session, source });
    }

    /// Cancels open commands not yet processed, and queues a close command for sessions already open or opening.
    fn push_close(&mut self, session: SessionId) {
        self.live_sessions.remove(&session);
        let pending_open = self.commands.iter().position(
            |command| matches!(command, Command::Open { session: pending, .. } if *pending == session),
        );
        match pending_open {
            Some(ix) => {
                self.commands.remove(ix);
            }
            None => self.commands.push_back(Command::Close { session }),
        }
        if self
            .window
            .as_ref()
            .is_some_and(|window| window.request.session == session)
        {
            self.window = None;
        }
        if self
            .value
            .as_ref()
            .is_some_and(|value| value.request.session == session)
        {
            self.value = None;
            self.value_ops.clear();
        }
        self.cancel_view(session);
        if self
            .find
            .as_ref()
            .is_some_and(|find| find.session == session)
        {
            self.find = None;
        }
        if self
            .structure
            .as_ref()
            .is_some_and(|structure| structure.session == session)
        {
            self.structure = None;
        }
    }

    fn push_view(&mut self, session: SessionId, request: RequestId, spec: ViewSpec) {
        self.latest_view.insert(session, request);
        self.views.retain(|(pending, _, _)| *pending != session);
        self.views.push_back((session, request, spec));
    }

    fn cancel_view(&mut self, session: SessionId) {
        self.latest_view.remove(&session);
        self.views.retain(|(pending, _, _)| *pending != session);
    }

    /// Whether the session has a row view request waiting to be computed.
    fn has_pending_view(&self, session: SessionId) -> bool {
        self.views.iter().any(|(pending, _, _)| *pending == session)
    }

    /// Takes requests in order: close, open (request order), visible range, value, value follow-up, file structure.
    fn next_job(&mut self) -> Option<Job> {
        let close = self
            .commands
            .iter()
            .position(|command| matches!(command, Command::Close { .. }));
        let next_command = close.or((!self.commands.is_empty()).then_some(0));
        if let Some(ix) = next_command {
            return self.commands.remove(ix).map(Job::Command);
        }
        if let Some(window) = self.window.take() {
            return Some(Job::Window(window));
        }
        if let Some(value) = self.value.take() {
            return Some(Job::Value(value));
        }
        if let Some(op) = self.value_ops.pop_front() {
            return Some(Job::ValueOp(op));
        }
        self.structure.take().map(Job::Structure)
    }
}

enum Job {
    Command(Command),
    Window(WindowJob),
    Value(ValueJob),
    ValueOp(ValueOp),
    Structure(StructureRequest),
}

/// Worker handle owned by the UI. Dropping it tells the worker to stop.
pub struct ViewerWorker {
    mailbox: Arc<Mutex<Mailbox>>,
    wake: mpsc::SyncSender<()>,
}

impl ViewerWorker {
    pub fn spawn() -> (Self, async_channel::Receiver<WorkerEvent>) {
        let mailbox = Arc::new(Mutex::new(Mailbox::default()));
        // Wake signals coalesce into a capacity of 1. If a signal is already queued, the new one is dropped.
        let (wake_tx, wake_rx) = mpsc::sync_channel(1);
        let (events_tx, events_rx) = async_channel::bounded(RESULT_CHANNEL_CAPACITY);

        let worker_mailbox = mailbox.clone();
        thread::Builder::new()
            .name("viewer-worker".into())
            .spawn(move || run(worker_mailbox, wake_rx, events_tx))
            .expect("cannot start the data worker thread");

        (
            Self {
                mailbox,
                wake: wake_tx,
            },
            events_rx,
        )
    }

    fn with_mailbox(&self, update: impl FnOnce(&mut Mailbox)) {
        update(&mut self.mailbox.lock().expect("mailbox lock poisoned"));
        self.wake();
    }

    /// Requests opening a source for a new tab. Other sessions are left untouched.
    pub fn open(&self, session: SessionId, source: DataSource) {
        self.with_mailbox(|mailbox| mailbox.push_open(session, source));
    }

    /// Closes a tab's session. Cancels open commands not yet processed and closes open sessions in the engine.
    pub fn close(&self, session: SessionId) {
        self.with_mailbox(|mailbox| mailbox.push_close(session));
    }

    /// Stops pending or in-flight visible range reads. Used when the active tab changes.
    pub fn cancel_window(&self) {
        let mut mailbox = self.mailbox.lock().expect("mailbox lock poisoned");
        mailbox.window = None;
        mailbox.latest_request = None;
    }

    /// Requests a visible range read. An earlier range request not yet processed is replaced by this one.
    /// The worker drops it if `view` differs from the session's current row view.
    pub fn request_window(&self, view: ViewId, request: WindowRequest, find: Option<String>) {
        self.with_mailbox(|mailbox| {
            mailbox.latest_request = Some(request.request);
            mailbox.window = Some(WindowJob {
                view,
                request,
                find,
            });
        });
    }

    /// Requests the selected cell's value. The previous value request and its follow-ups are dropped.
    pub fn request_value(&self, view: ViewId, request: ValueRequest) {
        self.with_mailbox(|mailbox| {
            mailbox.latest_value = Some(request.request);
            mailbox.value = Some(ValueJob { view, request });
            mailbox.value_ops.clear();
        });
    }

    /// Releases the selected value. Used when the active tab changes or the selection disappears.
    pub fn cancel_value(&self) {
        let mut mailbox = self.mailbox.lock().expect("mailbox lock poisoned");
        mailbox.value = None;
        mailbox.value_ops.clear();
        mailbox.latest_value = None;
    }

    pub fn value_op(&self, op: ValueOp) {
        self.with_mailbox(|mailbox| {
            if mailbox.latest_value == Some(op.request) {
                mailbox.value_ops.push_back(op);
            }
        });
    }

    /// Changes the session's row view. Drops earlier row view requests and in-flight computation of the same session.
    pub fn set_view(&self, session: SessionId, request: RequestId, spec: ViewSpec) {
        self.with_mailbox(|mailbox| mailbox.push_view(session, request, spec));
    }

    /// Stops the session's row view computation. A completed row view is left as is.
    pub fn cancel_view(&self, session: SessionId) {
        self.with_mailbox(|mailbox| mailbox.cancel_view(session));
    }

    /// Finds the next or previous match. An earlier find request is replaced by this one.
    pub fn find(&self, request: FindRequest) {
        self.with_mailbox(|mailbox| {
            mailbox.latest_find = Some(request.request);
            mailbox.find = Some(request);
        });
    }

    /// Requests the file structure. An earlier request is replaced by this one.
    pub fn request_structure(&self, request: StructureRequest) {
        self.with_mailbox(|mailbox| mailbox.structure = Some(request));
    }

    pub fn cancel_find(&self) {
        let mut mailbox = self.mailbox.lock().expect("mailbox lock poisoned");
        mailbox.find = None;
        mailbox.latest_find = None;
    }

    fn wake(&self) {
        let _ = self.wake.try_send(());
    }
}

impl Drop for ViewerWorker {
    fn drop(&mut self) {
        if let Ok(mut mailbox) = self.mailbox.lock() {
            mailbox.shutdown = true;
        }
        self.wake();
    }
}

/// Row view computation in progress.
struct RunningView {
    session: SessionId,
    request: RequestId,
    task: RowViewTask,
    reported: Instant,
}

/// Find computation in progress, and the find request to handle once it finishes.
struct RunningFind {
    request: FindRequest,
    task: FindTask,
    reported: Instant,
}

/// Find computation result. Reused for find requests with the same session, row view and input.
struct StoredFind {
    session: SessionId,
    view: ViewId,
    index: FindIndex,
}

/// State owned by the worker thread.
struct WorkerState {
    engine: ViewerEngine,
    /// Current row view and its number, per session.
    views: HashMap<SessionId, (ViewId, RowView)>,
    /// Only one selected value is kept.
    selected: Option<(RequestId, ValueSlice)>,
    running_view: Option<RunningView>,
    running_find: Option<RunningFind>,
    stored_find: Option<StoredFind>,
    /// Parsed find input used to mark matching cells.
    highlight: Option<(SessionId, FindQuery)>,
}

/// Sends a result to the UI. False once the UI has stopped receiving.
type Send<'a> = &'a dyn Fn(WorkerEvent) -> bool;

fn run(
    mailbox: Arc<Mutex<Mailbox>>,
    wake: mpsc::Receiver<()>,
    events: async_channel::Sender<WorkerEvent>,
) {
    let send = |event: WorkerEvent| events.send_blocking(event).is_ok();
    let mut state = WorkerState {
        engine: ViewerEngine::new(),
        views: HashMap::new(),
        selected: None,
        running_view: None,
        running_find: None,
        stored_find: None,
        highlight: None,
    };
    let mut idle = true;
    loop {
        if idle {
            if wake.recv().is_err() {
                return;
            }
        } else {
            let _ = wake.try_recv();
        }
        // Handle all open, close, visible range and value requests first.
        loop {
            let job = {
                let mut mailbox = mailbox.lock().expect("mailbox lock poisoned");
                if mailbox.shutdown {
                    return;
                }
                if let Some((request, slice)) = &state.selected
                    && !mailbox.is_value_current(slice.session, *request)
                {
                    state.selected = None;
                }
                mailbox.next_job()
            };
            let Some(job) = job else {
                break;
            };
            if !state.handle(job, &mailbox, &send) {
                return;
            }
        }
        // With no requests to handle, advance a computation by one batch.
        match state.step_background(&mailbox, &send) {
            Some(true) => idle = false,
            Some(false) => return,
            None => idle = true,
        }
    }
}

impl WorkerState {
    fn handle(&mut self, job: Job, mailbox: &Mutex<Mailbox>, send: Send) -> bool {
        let lock = || mailbox.lock().expect("mailbox lock poisoned");
        match job {
            Job::Command(Command::Open { session, source }) => {
                let result = self.engine.open_source(session, &source);
                if lock().is_current(session, None) {
                    if let Ok(info) = &result {
                        self.views
                            .insert(session, (None, RowView::identity(info.total_rows)));
                    }
                    send(WorkerEvent::Opened { session, result })
                } else {
                    // The tab was closed while opening.
                    self.engine.close(session);
                    true
                }
            }
            Job::Command(Command::Close { session }) => {
                self.close(session);
                true
            }
            Job::Window(job) => {
                let session = job.request.session;
                let request = job.request.request;
                let is_current = || lock().is_current(session, Some(request));
                let Some((view_id, view)) = self.views.get(&session) else {
                    return true;
                };
                // Requests for the previous row view point to rows at different positions, so drop them.
                if *view_id != job.view {
                    return true;
                }
                let query = match job.find.filter(|text| !text.is_empty()) {
                    Some(text) => {
                        highlight_query(&mut self.highlight, &self.engine, session, &text)
                    }
                    None => None,
                };
                if view.is_identity() {
                    read_window(&mut self.engine, &job.request, query, &is_current, send)
                } else {
                    let view = view.clone();
                    read_view_window(
                        &mut self.engine,
                        &view,
                        &job.request,
                        query,
                        &is_current,
                        send,
                    )
                }
            }
            Job::Value(job) => {
                self.selected = None;
                let request = job.request;
                let session = request.session;
                let current = || lock().is_value_current(session, request.request);
                let file_row = match self.views.get(&session) {
                    Some((view_id, view)) if *view_id == job.view => view.file_row(request.row),
                    _ => return true,
                };
                let Some(file_row) = file_row else {
                    return true;
                };
                let file_request = ValueRequest {
                    row: file_row,
                    ..request
                };
                match self.engine.read_value(&file_request, &|| !current()) {
                    Ok(slice) => {
                        let detail = value_detail::detail(&slice);
                        self.selected = Some((request.request, slice));
                        !current()
                            || send(WorkerEvent::ValueLoaded {
                                session,
                                request: request.request,
                                detail,
                            })
                    }
                    Err(ViewerError::Cancelled | ViewerError::StaleSession) => true,
                    Err(error) => {
                        !current()
                            || send(WorkerEvent::ValueFailed {
                                session,
                                request: request.request,
                                error,
                            })
                    }
                }
            }
            Job::Structure(request) => {
                let StructureRequest {
                    session,
                    request,
                    table,
                    row_group,
                } = request;
                if !lock().is_current(session, None) {
                    return true;
                }
                match row_group {
                    None if table => send(WorkerEvent::TableStructure {
                        session,
                        request,
                        result: self.engine.table_structure(session),
                    }),
                    None => send(WorkerEvent::FileStructure {
                        session,
                        request,
                        result: self.engine.file_structure(session),
                    }),
                    Some(row_group) => send(WorkerEvent::RowGroupColumns {
                        session,
                        request,
                        row_group,
                        result: self.engine.row_group_columns(session, row_group),
                    }),
                }
            }
            Job::ValueOp(op) => match &self.selected {
                Some((request, slice)) if *request == op.request => {
                    let session = slice.session;
                    let request = *request;
                    match value_op_event(slice, op) {
                        Some(event) if lock().is_value_current(session, request) => send(event),
                        _ => true,
                    }
                }
                _ => true,
            },
        }
    }

    fn close(&mut self, session: SessionId) {
        self.engine.close(session);
        self.views.remove(&session);
        if self
            .running_view
            .as_ref()
            .is_some_and(|running| running.session == session)
        {
            self.running_view = None;
        }
        if self
            .running_find
            .as_ref()
            .is_some_and(|running| running.request.session == session)
        {
            self.running_find = None;
        }
        if self
            .stored_find
            .as_ref()
            .is_some_and(|stored| stored.session == session)
        {
            self.stored_find = None;
        }
        if self
            .highlight
            .as_ref()
            .is_some_and(|(highlighted, _)| *highlighted == session)
        {
            self.highlight = None;
        }
    }

    /// Advances one of the find request, the find computation or the row view computation, in that order, by one batch.
    /// `None` if there is nothing to do, `Some(false)` once the UI has stopped receiving.
    fn step_background(&mut self, mailbox: &Mutex<Mailbox>, send: Send) -> Option<bool> {
        let lock = || mailbox.lock().expect("mailbox lock poisoned");

        // Take a find request whose session has no row view computation left.
        let find = {
            let mut mailbox = lock();
            let blocked = mailbox.find.as_ref().is_some_and(|find| {
                mailbox.has_pending_view(find.session)
                    || self
                        .running_view
                        .as_ref()
                        .is_some_and(|running| running.session == find.session)
            });
            if blocked { None } else { mailbox.find.take() }
        };
        if let Some(find) = find {
            return Some(self.start_find(find, mailbox, send));
        }

        if let Some(running) = &mut self.running_find {
            let session = running.request.session;
            let request = running.request.request;
            if !lock().is_find_current(session, request) {
                self.running_find = None;
                return Some(true);
            }
            return Some(match running.task.step() {
                Ok(TaskStep::Progress(progress)) => {
                    if running.reported.elapsed() >= PROGRESS_INTERVAL {
                        running.reported = Instant::now();
                        send(WorkerEvent::FindProgress {
                            session,
                            request,
                            progress,
                        })
                    } else {
                        true
                    }
                }
                Ok(TaskStep::Done(index)) => {
                    let running = self.running_find.take().expect("find in progress");
                    self.stored_find = Some(StoredFind {
                        session,
                        view: running.request.view,
                        index,
                    });
                    self.navigate(running.request, mailbox, send)
                }
                Err(error) => {
                    self.running_find = None;
                    send(WorkerEvent::FindFailed {
                        session,
                        request,
                        error,
                    })
                }
            });
        }

        if let Some(running) = &mut self.running_view {
            let (session, request) = (running.session, running.request);
            if !lock().is_view_current(session, request) {
                self.running_view = None;
                return Some(true);
            }
            return Some(match running.task.step(&self.engine) {
                Ok(TaskStep::Progress(progress)) => {
                    if running.reported.elapsed() >= PROGRESS_INTERVAL {
                        running.reported = Instant::now();
                        send(WorkerEvent::ViewProgress {
                            session,
                            request,
                            progress,
                        })
                    } else {
                        true
                    }
                }
                Ok(TaskStep::Done(view)) => {
                    self.running_view = None;
                    let rows = view.len();
                    self.views.insert(session, (Some(request), view));
                    if self
                        .stored_find
                        .as_ref()
                        .is_some_and(|stored| stored.session == session)
                    {
                        self.stored_find = None;
                    }
                    send(WorkerEvent::ViewReady {
                        session,
                        request,
                        rows,
                    })
                }
                Err(error) => {
                    self.running_view = None;
                    send(WorkerEvent::ViewFailed {
                        session,
                        request,
                        error,
                    })
                }
            });
        }

        let (session, request, spec) = lock().views.pop_front()?;
        Some(match RowViewTask::new(&self.engine, session, &spec) {
            Ok(task) => {
                self.running_view = Some(RunningView {
                    session,
                    request,
                    task,
                    reported: Instant::now(),
                });
                true
            }
            Err(ViewerError::StaleSession) => true,
            Err(error) => send(WorkerEvent::ViewFailed {
                session,
                request,
                error,
            }),
        })
    }

    /// Moves immediately if the kept find result matches the request; otherwise starts a find computation.
    fn start_find(&mut self, find: FindRequest, mailbox: &Mutex<Mailbox>, send: Send) -> bool {
        let Some((view_id, view)) = self.views.get(&find.session) else {
            return true;
        };
        // A request based on the previous row view. The UI requests again with the new row view.
        if *view_id != find.view {
            return true;
        }
        let reusable = self.stored_find.as_ref().is_some_and(|stored| {
            stored.session == find.session
                && stored.view == find.view
                && stored.index.query().text() == find.query
        });
        if reusable {
            return self.navigate(find, mailbox, send);
        }
        // If a computation for the same input is already running, continue with it.
        if let Some(running) = &mut self.running_find
            && running.request.session == find.session
            && running.request.view == find.view
            && running.request.query == find.query
        {
            running.request = find;
            return true;
        }
        let schema = match self.engine.schema(find.session) {
            Ok(schema) => schema,
            Err(_) => return true,
        };
        let query = FindQuery::new(&schema, &find.query);
        match FindTask::new(&self.engine, find.session, query, view) {
            Ok(task) => {
                self.running_find = Some(RunningFind {
                    request: find,
                    task,
                    reported: Instant::now(),
                });
                true
            }
            Err(ViewerError::StaleSession) => true,
            Err(error) => send(WorkerEvent::FindFailed {
                session: find.session,
                request: find.request,
                error,
            }),
        }
    }

    /// Finds and sends the next or previous match from the kept find result.
    fn navigate(&mut self, find: FindRequest, mailbox: &Mutex<Mailbox>, send: Send) -> bool {
        let (Some(stored), Some((_, view))) =
            (self.stored_find.as_ref(), self.views.get(&find.session))
        else {
            return true;
        };
        let current = || {
            mailbox
                .lock()
                .expect("mailbox lock poisoned")
                .is_find_current(find.session, find.request)
        };
        let result = stored.index.next(
            &mut self.engine,
            find.session,
            view,
            find.from,
            find.backward,
            &|| !current(),
        );
        if !current() {
            return true;
        }
        match result {
            Ok(found) => send(WorkerEvent::FindResult {
                session: find.session,
                request: find.request,
                cells: stored.index.cell_count(),
                found,
            }),
            Err(ViewerError::Cancelled | ViewerError::StaleSession) => true,
            Err(error) => send(WorkerEvent::FindFailed {
                session: find.session,
                request: find.request,
                error,
            }),
        }
    }
}

/// Parsed find input of a session. Reused for the same input in the same session.
fn highlight_query<'a>(
    cache: &'a mut Option<(SessionId, FindQuery)>,
    engine: &ViewerEngine,
    session: SessionId,
    text: &str,
) -> Option<&'a FindQuery> {
    let stale = !cache
        .as_ref()
        .is_some_and(|(cached, query)| *cached == session && query.text() == text);
    if stale {
        let schema = engine.schema(session).ok()?;
        *cache = Some((session, FindQuery::new(&schema, text)));
    }
    cache.as_ref().map(|(_, query)| query)
}

/// Reads the visible range in source order, block by block, sending previews only for the cells needed on screen.
///
/// Checks that the request is still the latest before reading a block, after decoding a batch, and before sending results.
fn read_window(
    engine: &mut ViewerEngine,
    request: &WindowRequest,
    query: Option<&FindQuery>,
    is_current: &dyn Fn() -> bool,
    send: Send,
) -> bool {
    let session = request.session;
    let failed = |rows, error| WorkerEvent::ReadFailed {
        session,
        request: request.request,
        rows,
        error,
    };

    let blocks = match engine.plan_window(request) {
        Ok(blocks) => blocks,
        Err(ViewerError::StaleSession) => return true,
        Err(error) => return !is_current() || send(failed(request.rows.clone(), error)),
    };
    for block in blocks {
        if !is_current() {
            return true;
        }
        let rows = block.rows.start.max(request.rows.start)..block.rows.end.min(request.rows.end);
        let (event, stop) = match engine.read_block(&block, &|| !is_current()) {
            Ok(decoded) => (
                WorkerEvent::Cells {
                    session,
                    request: request.request,
                    cells: window_cells(&decoded, rows, query),
                },
                false,
            ),
            Err(ViewerError::Cancelled | ViewerError::StaleSession) => return true,
            // If the file changed, the remaining blocks give the same result, so stop reading.
            Err(error @ ViewerError::FileChanged) => (failed(rows, error), true),
            Err(error) => (failed(rows, error), false),
        };
        if !is_current() {
            return true;
        }
        if !send(event) {
            return false;
        }
        if stop {
            return true;
        }
    }
    true
}

/// Reads the visible range of a row view with a row list. Reads the source rows of the range in order and
/// sends previews along with their source row numbers.
fn read_view_window(
    engine: &mut ViewerEngine,
    view: &RowView,
    request: &WindowRequest,
    query: Option<&FindQuery>,
    is_current: &dyn Fn() -> bool,
    send: Send,
) -> bool {
    let session = request.session;
    let file_rows = view.file_rows(request.rows.clone());
    if file_rows.is_empty() || request.columns.is_empty() {
        return true;
    }
    let rows = request.rows.start..request.rows.start + file_rows.len() as u64;
    let event = match engine.read_rows(session, &file_rows, &request.columns, &|| !is_current()) {
        Ok(batch) => {
            let mut columns = request.columns.clone();
            columns.sort_unstable();
            columns.dedup();
            let previews = columns
                .iter()
                .zip(batch.columns())
                .map(|(column, array)| column_previews(*column, array, 0..file_rows.len(), query))
                .collect();
            WorkerEvent::Cells {
                session,
                request: request.request,
                cells: WindowCells {
                    rows,
                    row_numbers: file_rows,
                    columns,
                    previews,
                },
            }
        }
        Err(ViewerError::Cancelled | ViewerError::StaleSession) => return true,
        Err(error) => WorkerEvent::ReadFailed {
            session,
            request: request.request,
            rows,
            error,
        },
    };
    !is_current() || send(event)
}

fn window_cells(
    decoded: &DecodedBlock,
    rows: Range<u64>,
    query: Option<&FindQuery>,
) -> WindowCells {
    let local = (rows.start - decoded.start_row) as usize..(rows.end - decoded.start_row) as usize;
    let previews = decoded
        .columns
        .iter()
        .zip(decoded.batch.columns())
        .map(|(column, array)| column_previews(*column, array, local.clone(), query))
        .collect();
    WindowCells {
        row_numbers: rows.clone().collect(),
        rows,
        columns: decoded.columns.clone(),
        previews,
    }
}

/// Preview of the value at position `local` of the array. With a find input, marks matching cells.
fn column_previews(
    column: ColumnId,
    array: &ArrayRef,
    local: Range<usize>,
    query: Option<&FindQuery>,
) -> Vec<CellPreview> {
    let mut previews = preview_column(array.as_ref(), local.clone());
    if let Some(query) = query {
        let slice = array.slice(local.start, local.len());
        if let Ok(Some(matched)) = query.matches(column, &slice) {
            for (preview, matched) in previews.iter_mut().zip(matched.iter()) {
                preview.matched = matched;
            }
        }
    }
    previews
}

/// Builds node children, body or copy text from the selected value. `None` if the path leaves the value's structure.
fn value_op_event(slice: &ValueSlice, op: ValueOp) -> Option<WorkerEvent> {
    let (array, row) = value_detail::node_at(&slice.value, &op.path)?;
    let (session, request) = (slice.session, op.request);
    Some(match op.kind {
        ValueOpKind::Children { range } => WorkerEvent::ValueChildren {
            session,
            request,
            start: range.start,
            children: value_detail::children(&array, row, range),
            path: op.path,
        },
        ValueOpKind::Body => WorkerEvent::ValueBody {
            session,
            request,
            body: value_detail::body(&array, row),
            path: op.path,
        },
        ValueOpKind::Copy => WorkerEvent::ValueCopy {
            session,
            request,
            text: value_detail::copy_text(&array, row),
        },
        ValueOpKind::Cell => WorkerEvent::ValueCell {
            session,
            request,
            value: value_detail::cell_value(&array, row),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job_kind(job: Option<Job>) -> Option<(&'static str, SessionId)> {
        job.map(|job| match job {
            Job::Command(Command::Open { session, .. }) => ("open", session),
            Job::Command(Command::Close { session }) => ("close", session),
            Job::Window(job) => ("window", job.request.session),
            Job::Value(job) => ("value", job.request.session),
            Job::ValueOp(_) => ("value-op", SessionId(0)),
            Job::Structure(request) => ("structure", request.session),
        })
    }

    fn window(session: u64, request: u64) -> WindowJob {
        WindowJob {
            view: None,
            request: WindowRequest {
                session: SessionId(session),
                request: RequestId(request),
                rows: 0..10,
                columns: vec![ColumnId(0)],
            },
            find: None,
        }
    }

    #[test]
    fn keeps_every_open_in_order_and_closes_first() {
        let mut mailbox = Mailbox::default();
        mailbox.push_open(SessionId(1), DataSource::Parquet("a.parquet".into()));
        mailbox.push_open(SessionId(2), DataSource::Parquet("b.parquet".into()));
        mailbox.window = Some(window(2, 1));
        mailbox.push_open(SessionId(3), DataSource::Parquet("c.parquet".into()));
        assert_eq!(job_kind(mailbox.next_job()), Some(("open", SessionId(1))));
        mailbox.push_close(SessionId(1));

        assert_eq!(job_kind(mailbox.next_job()), Some(("close", SessionId(1))));
        assert_eq!(job_kind(mailbox.next_job()), Some(("open", SessionId(2))));
        assert_eq!(job_kind(mailbox.next_job()), Some(("open", SessionId(3))));
        assert_eq!(job_kind(mailbox.next_job()), Some(("window", SessionId(2))));
        assert_eq!(job_kind(mailbox.next_job()), None);
    }

    #[test]
    fn closing_before_open_cancels_the_open() {
        let mut mailbox = Mailbox::default();
        mailbox.push_open(SessionId(1), DataSource::Parquet("a.parquet".into()));
        mailbox.push_open(SessionId(2), DataSource::Parquet("b.parquet".into()));
        mailbox.window = Some(window(1, 1));
        mailbox.push_close(SessionId(1));

        assert!(!mailbox.is_current(SessionId(1), None));
        assert!(mailbox.window.is_none());
        assert_eq!(job_kind(mailbox.next_job()), Some(("open", SessionId(2))));
        assert_eq!(job_kind(mailbox.next_job()), None);
    }

    #[test]
    fn only_the_latest_window_request_is_current() {
        let mut mailbox = Mailbox::default();
        mailbox.push_open(SessionId(1), DataSource::Parquet("a.parquet".into()));
        mailbox.latest_request = Some(RequestId(2));

        assert!(mailbox.is_current(SessionId(1), None));
        assert!(mailbox.is_current(SessionId(1), Some(RequestId(2))));
        assert!(!mailbox.is_current(SessionId(1), Some(RequestId(1))));
        assert!(!mailbox.is_current(SessionId(9), None));
    }

    #[test]
    fn keeps_one_view_request_per_session() {
        let mut mailbox = Mailbox::default();
        mailbox.push_open(SessionId(1), DataSource::Parquet("a.parquet".into()));
        mailbox.push_open(SessionId(2), DataSource::Parquet("b.parquet".into()));
        mailbox.push_view(SessionId(1), RequestId(1), ViewSpec::default());
        mailbox.push_view(SessionId(2), RequestId(2), ViewSpec::default());
        mailbox.push_view(SessionId(1), RequestId(3), ViewSpec::default());

        let pending: Vec<_> = mailbox
            .views
            .iter()
            .map(|(session, request, _)| (*session, *request))
            .collect();
        assert_eq!(
            pending,
            [(SessionId(2), RequestId(2)), (SessionId(1), RequestId(3))]
        );
        assert!(!mailbox.is_view_current(SessionId(1), RequestId(1)));
        assert!(mailbox.is_view_current(SessionId(1), RequestId(3)));

        mailbox.cancel_view(SessionId(1));
        assert!(!mailbox.has_pending_view(SessionId(1)));
        assert!(!mailbox.is_view_current(SessionId(1), RequestId(3)));
        mailbox.push_close(SessionId(2));
        assert!(mailbox.views.is_empty());
    }
}
