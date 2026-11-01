//! 데이터 엔진을 실행하는 전용 thread와 요청 mailbox.
//!
//! UI는 mailbox에 최신 요청을 넣고 worker를 깨운다. worker는 mailbox 잠금 밖에서 엔진을
//! 호출하고, 결과는 bounded channel로 보낸다. UI thread는 worker의 완료를 기다리지 않는다.
//!
//! 행 보기와 찾기의 계산은 순차 읽기 배치 하나씩 진행한다. 배치 사이마다 열기·닫기·화면 범위·
//! 값 요청을 먼저 처리하므로, 계산 중에도 스크롤과 셀 선택이 동작한다.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use arrow_array::ArrayRef;
use viewer_core::{
    ColumnChunkInfo, ColumnId, DatasetInfo, DecodedBlock, FileStructure, FindIndex, FindQuery,
    FindTask, RequestId, RowView, RowViewTask, SessionId, TaskStep, ValueRequest, ValueSlice,
    ViewSpec, ViewerEngine, ViewerError, WindowRequest,
};

use crate::cell_preview::{CellPreview, preview_column};
use crate::value_detail::{self, ValueBody, ValueDetail, ValueNode};

/// 결과 channel에 쌓일 수 있는 최대 메시지 수.
const RESULT_CHANNEL_CAPACITY: usize = 2;
/// 계산 진행률을 UI로 보내는 최소 간격.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// 행 보기 번호. 행 보기 요청의 `RequestId`이며, `None`은 파일을 연 뒤 행 보기를 바꾸지 않은
/// 원본 순서다.
pub type ViewId = Option<RequestId>;

/// worker가 UI로 보내는 결과.
pub enum WorkerEvent {
    Opened {
        session: SessionId,
        result: Result<DatasetInfo, ViewerError>,
    },
    /// 화면 범위 중 일부 행의 셀 미리보기.
    Cells {
        session: SessionId,
        request: RequestId,
        cells: WindowCells,
    },
    /// 화면 범위 중 읽지 못한 행 범위.
    ReadFailed {
        session: SessionId,
        request: RequestId,
        rows: Range<u64>,
        error: ViewerError,
    },
    /// 선택 셀의 루트 정보와 본문.
    ValueLoaded {
        session: SessionId,
        request: RequestId,
        detail: ValueDetail,
    },
    /// 값 트리 노드의 `start`부터 이어지는 자식.
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
    ValueFailed {
        session: SessionId,
        request: RequestId,
        error: ViewerError,
    },
    /// 행 보기 계산의 진행률. 0부터 1까지다.
    ViewProgress {
        session: SessionId,
        request: RequestId,
        progress: f32,
    },
    /// 행 보기가 완성되었다. 이후 요청은 이 번호로 보낸다.
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
    /// 행 보기 전체의 일치 셀 수와 찾은 셀(행 보기 위치, 컬럼).
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
    /// 파일 구조. 메타데이터만으로 만든다.
    FileStructure {
        session: SessionId,
        request: RequestId,
        result: Result<FileStructure, ViewerError>,
    },
    /// Row Group 하나의 컬럼별 저장 정보.
    RowGroupColumns {
        session: SessionId,
        request: RequestId,
        row_group: usize,
        result: Result<Vec<ColumnChunkInfo>, ViewerError>,
    },
}

/// 파일 구조 요청. `row_group`이 있으면 그 Row Group의 컬럼별 정보를 요청한다.
pub struct StructureRequest {
    pub session: SessionId,
    pub request: RequestId,
    pub row_group: Option<usize>,
}

/// 선택 값에 대한 후속 작업. 값 요청이 바뀌면 폐기한다.
pub struct ValueOp {
    pub request: RequestId,
    pub path: Vec<usize>,
    pub kind: ValueOpKind,
}

pub enum ValueOpKind {
    Children { range: Range<usize> },
    Body,
    Copy,
}

/// 화면 범위의 일부 행의 셀 미리보기. `previews[c][r]`는 `columns[c]` 컬럼의 행 보기 위치
/// `rows.start + r` 행이고, 그 원본 행 번호는 `row_numbers[r]`다.
pub struct WindowCells {
    pub rows: Range<u64>,
    pub row_numbers: Vec<u64>,
    pub columns: Vec<ColumnId>,
    pub previews: Vec<Vec<CellPreview>>,
}

/// 찾기 요청. `from`은 출발 셀의 행 보기 위치와 컬럼이다.
pub struct FindRequest {
    pub session: SessionId,
    pub request: RequestId,
    pub view: ViewId,
    pub query: String,
    pub from: Option<(u64, ColumnId)>,
    pub backward: bool,
}

/// 탭 하나의 파일 세션을 열거나 닫는 명령. 요청 순서대로 모두 처리한다.
enum Command {
    Open { session: SessionId, path: PathBuf },
    Close { session: SessionId },
}

/// 화면 범위 요청. 행은 행 보기 위치다. `find`가 있으면 그 입력과 같은 셀을 표시한다.
struct WindowJob {
    view: ViewId,
    request: WindowRequest,
    find: Option<String>,
}

/// 값 요청. 행은 행 보기 위치다.
struct ValueJob {
    view: ViewId,
    request: ValueRequest,
}

/// 처리 대기 중인 요청. 열기·닫기 명령은 순서대로 쌓고, 화면 범위·값·찾기 요청은 최신
/// 하나만, 행 보기 요청은 세션마다 최신 하나만 둔다.
#[derive(Default)]
struct Mailbox {
    commands: VecDeque<Command>,
    window: Option<WindowJob>,
    value: Option<ValueJob>,
    value_ops: VecDeque<ValueOp>,
    /// 활성 탭의 최신 값 요청. 보관한 선택 값과 후속 작업이 아직 유효한지 판단한다.
    latest_value: Option<RequestId>,
    /// 탭으로 열려 있는 세션. 닫힌 탭의 작업과 결과를 버리는 데 쓴다.
    live_sessions: HashSet<SessionId>,
    /// 활성 탭의 최신 화면 범위 요청. 진행 중인 읽기가 아직 유효한지 판단한다.
    latest_request: Option<RequestId>,
    /// 아직 계산을 시작하지 않은 행 보기 요청. 세션마다 최신 하나다.
    views: VecDeque<(SessionId, RequestId, ViewSpec)>,
    /// 세션별 최신 행 보기 요청. 진행 중인 계산이 아직 유효한지 판단한다.
    latest_view: HashMap<SessionId, RequestId>,
    find: Option<FindRequest>,
    latest_find: Option<RequestId>,
    /// 최신 파일 구조 요청.
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

    fn push_open(&mut self, session: SessionId, path: PathBuf) {
        self.live_sessions.insert(session);
        self.commands.push_back(Command::Open { session, path });
    }

    /// 아직 처리하지 않은 열기 명령은 취소하고, 이미 열렸거나 여는 중인 세션은 닫기 명령을 쌓는다.
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

    /// 세션에 계산을 기다리는 행 보기 요청이 있는지.
    fn has_pending_view(&self, session: SessionId) -> bool {
        self.views.iter().any(|(pending, _, _)| *pending == session)
    }

    /// 닫기, 열기(요청 순서), 화면 범위, 값 요청, 값 후속 작업, 파일 구조 순서로 꺼낸다.
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

/// UI가 소유하는 worker handle. drop되면 worker에 종료를 알린다.
pub struct ViewerWorker {
    mailbox: Arc<Mutex<Mailbox>>,
    wake: mpsc::SyncSender<()>,
}

impl ViewerWorker {
    pub fn spawn() -> (Self, async_channel::Receiver<WorkerEvent>) {
        let mailbox = Arc::new(Mutex::new(Mailbox::default()));
        // 깨우는 신호는 용량 1로 합친다. 이미 신호가 쌓여 있으면 새 신호를 버린다.
        let (wake_tx, wake_rx) = mpsc::sync_channel(1);
        let (events_tx, events_rx) = async_channel::bounded(RESULT_CHANNEL_CAPACITY);

        let worker_mailbox = mailbox.clone();
        thread::Builder::new()
            .name("viewer-worker".into())
            .spawn(move || run(worker_mailbox, wake_rx, events_tx))
            .expect("데이터 worker thread를 시작할 수 없습니다");

        (
            Self {
                mailbox,
                wake: wake_tx,
            },
            events_rx,
        )
    }

    fn with_mailbox(&self, update: impl FnOnce(&mut Mailbox)) {
        update(&mut self.mailbox.lock().expect("mailbox 잠금 실패"));
        self.wake();
    }

    /// 새 탭의 파일 열기를 요청한다. 다른 세션은 그대로 둔다.
    pub fn open(&self, session: SessionId, path: PathBuf) {
        self.with_mailbox(|mailbox| mailbox.push_open(session, path));
    }

    /// 탭의 세션을 닫는다. 아직 처리하지 않은 열기 명령은 취소하고, 열린 세션은 엔진에서 닫는다.
    pub fn close(&self, session: SessionId) {
        self.with_mailbox(|mailbox| mailbox.push_close(session));
    }

    /// 대기 중이거나 진행 중인 화면 범위 읽기를 멈춘다. 활성 탭이 바뀔 때 쓴다.
    pub fn cancel_window(&self) {
        let mut mailbox = self.mailbox.lock().expect("mailbox 잠금 실패");
        mailbox.window = None;
        mailbox.latest_request = None;
    }

    /// 화면 범위 읽기를 요청한다. 아직 처리되지 않은 이전 범위 요청은 이 요청으로 교체된다.
    /// `view`가 세션의 현재 행 보기와 다르면 worker가 버린다.
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

    /// 선택 셀의 값을 요청한다. 이전 값 요청과 그 후속 작업은 폐기된다.
    pub fn request_value(&self, view: ViewId, request: ValueRequest) {
        self.with_mailbox(|mailbox| {
            mailbox.latest_value = Some(request.request);
            mailbox.value = Some(ValueJob { view, request });
            mailbox.value_ops.clear();
        });
    }

    /// 선택 값을 해제한다. 활성 탭이 바뀌거나 선택 셀이 없어질 때 쓴다.
    pub fn cancel_value(&self) {
        let mut mailbox = self.mailbox.lock().expect("mailbox 잠금 실패");
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

    /// 세션의 행 보기를 바꾼다. 같은 세션의 이전 행 보기 요청과 진행 중인 계산은 버린다.
    pub fn set_view(&self, session: SessionId, request: RequestId, spec: ViewSpec) {
        self.with_mailbox(|mailbox| mailbox.push_view(session, request, spec));
    }

    /// 세션의 행 보기 계산을 멈춘다. 이미 완성된 행 보기는 그대로 둔다.
    pub fn cancel_view(&self, session: SessionId) {
        self.with_mailbox(|mailbox| mailbox.cancel_view(session));
    }

    /// 다음·이전 일치를 찾는다. 이전 찾기 요청은 이 요청으로 교체된다.
    pub fn find(&self, request: FindRequest) {
        self.with_mailbox(|mailbox| {
            mailbox.latest_find = Some(request.request);
            mailbox.find = Some(request);
        });
    }

    /// 파일 구조를 요청한다. 이전 요청은 이 요청으로 교체된다.
    pub fn request_structure(&self, request: StructureRequest) {
        self.with_mailbox(|mailbox| mailbox.structure = Some(request));
    }

    pub fn cancel_find(&self) {
        let mut mailbox = self.mailbox.lock().expect("mailbox 잠금 실패");
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

/// 진행 중인 행 보기 계산.
struct RunningView {
    session: SessionId,
    request: RequestId,
    task: RowViewTask,
    reported: Instant,
}

/// 진행 중인 찾기 계산과, 계산이 끝나면 처리할 찾기 요청.
struct RunningFind {
    request: FindRequest,
    task: FindTask,
    reported: Instant,
}

/// 찾기 계산 결과. 세션, 행 보기, 입력이 모두 같은 찾기 요청에 다시 쓴다.
struct StoredFind {
    session: SessionId,
    view: ViewId,
    index: FindIndex,
}

/// worker thread가 소유하는 상태.
struct WorkerState {
    engine: ViewerEngine,
    /// 세션별 현재 행 보기와 그 번호.
    views: HashMap<SessionId, (ViewId, RowView)>,
    /// 선택 값은 하나만 보관한다.
    selected: Option<(RequestId, ValueSlice)>,
    running_view: Option<RunningView>,
    running_find: Option<RunningFind>,
    stored_find: Option<StoredFind>,
    /// 일치 셀 강조에 쓰는 찾기 입력의 해석 결과.
    highlight: Option<(SessionId, FindQuery)>,
}

/// 결과를 UI로 보낸다. UI가 수신을 끝냈으면 false다.
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
        // 열기·닫기·화면 범위·값 요청을 먼저 모두 처리한다.
        loop {
            let job = {
                let mut mailbox = mailbox.lock().expect("mailbox 잠금 실패");
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
        // 처리할 요청이 없으면 계산을 배치 하나만큼 진행한다.
        match state.step_background(&mailbox, &send) {
            Some(true) => idle = false,
            Some(false) => return,
            None => idle = true,
        }
    }
}

impl WorkerState {
    fn handle(&mut self, job: Job, mailbox: &Mutex<Mailbox>, send: Send) -> bool {
        let lock = || mailbox.lock().expect("mailbox 잠금 실패");
        match job {
            Job::Command(Command::Open { session, path }) => {
                let result = self.engine.open(session, &path);
                if lock().is_current(session, None) {
                    if let Ok(info) = &result {
                        self.views
                            .insert(session, (None, RowView::identity(info.total_rows)));
                    }
                    send(WorkerEvent::Opened { session, result })
                } else {
                    // 여는 동안 탭이 닫혔다.
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
                // 이전 행 보기의 요청은 위치가 다른 행을 가리키므로 버린다.
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
                    row_group,
                } = request;
                if !lock().is_current(session, None) {
                    return true;
                }
                match row_group {
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

    /// 찾기 요청, 찾기 계산, 행 보기 계산 순서로 하나를 배치 하나만큼 진행한다. 할 일이 없으면
    /// `None`, UI가 수신을 끝냈으면 `Some(false)`다.
    fn step_background(&mut self, mailbox: &Mutex<Mailbox>, send: Send) -> Option<bool> {
        let lock = || mailbox.lock().expect("mailbox 잠금 실패");

        // 같은 세션의 행 보기 계산이 남아 있지 않은 찾기 요청을 꺼낸다.
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
                    let running = self.running_find.take().expect("진행 중인 찾기");
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

    /// 보관한 찾기 결과가 요청과 맞으면 바로 이동하고, 아니면 찾기 계산을 시작한다.
    fn start_find(&mut self, find: FindRequest, mailbox: &Mutex<Mailbox>, send: Send) -> bool {
        let Some((view_id, view)) = self.views.get(&find.session) else {
            return true;
        };
        // 이전 행 보기 기준의 요청이다. UI가 새 행 보기로 다시 요청한다.
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
        // 같은 입력의 계산이 이미 진행 중이면 이어서 쓴다.
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

    /// 보관한 찾기 결과로 다음·이전 일치를 찾아 보낸다.
    fn navigate(&mut self, find: FindRequest, mailbox: &Mutex<Mailbox>, send: Send) -> bool {
        let (Some(stored), Some((_, view))) =
            (self.stored_find.as_ref(), self.views.get(&find.session))
        else {
            return true;
        };
        let current = || {
            mailbox
                .lock()
                .expect("mailbox 잠금 실패")
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

/// 세션의 찾기 입력 해석 결과. 같은 세션의 같은 입력이면 다시 쓴다.
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

/// 원본 순서의 화면 범위를 블록 순서대로 읽고, 블록마다 화면에 필요한 셀의 미리보기만 보낸다.
///
/// 블록 읽기 전, 배치 디코딩 후, 결과 송신 전에 요청이 아직 최신인지 확인한다.
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
            // 파일이 바뀌면 남은 블록도 같은 결과이므로 더 읽지 않는다.
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

/// 행 목록이 있는 행 보기의 화면 범위를 읽는다. 화면 범위의 원본 행을 그 순서대로 읽어
/// 미리보기와 원본 행 번호를 함께 보낸다.
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

/// 배열의 `local` 위치 값의 미리보기. 찾기 입력이 있으면 일치하는 셀을 표시한다.
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

/// 선택 값에서 노드 자식·본문·복사 텍스트를 만든다. 경로가 값의 구조를 벗어나면 `None`이다.
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
        mailbox.push_open(SessionId(1), "a.parquet".into());
        mailbox.push_open(SessionId(2), "b.parquet".into());
        mailbox.window = Some(window(2, 1));
        mailbox.push_open(SessionId(3), "c.parquet".into());
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
        mailbox.push_open(SessionId(1), "a.parquet".into());
        mailbox.push_open(SessionId(2), "b.parquet".into());
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
        mailbox.push_open(SessionId(1), "a.parquet".into());
        mailbox.latest_request = Some(RequestId(2));

        assert!(mailbox.is_current(SessionId(1), None));
        assert!(mailbox.is_current(SessionId(1), Some(RequestId(2))));
        assert!(!mailbox.is_current(SessionId(1), Some(RequestId(1))));
        assert!(!mailbox.is_current(SessionId(9), None));
    }

    #[test]
    fn keeps_one_view_request_per_session() {
        let mut mailbox = Mailbox::default();
        mailbox.push_open(SessionId(1), "a.parquet".into());
        mailbox.push_open(SessionId(2), "b.parquet".into());
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
