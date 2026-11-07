//! Connection to `tpui-pdf`, the process that reads PDF files and draws their pages.
//!
//! The worker runs while a PDF tab is shown. It is created inside a Job Object of its own that ends it when the job's last
//! handle closes, so it ends with the viewer however the viewer ends, even in the middle of a page; it also ends by itself
//! when its stdin closes. One thread writes requests and one reads answers, so the UI thread never waits for the worker.
//! Only the latest open request and the latest draw request wait to be sent.

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use viewer_core::{PdfInfo, PdfPageImage, PdfReply, PdfRequest, read_frame, write_frame};

use crate::usage_log::ProcessUsage;

/// Where the worker is expected: next to the running viewer.
pub fn worker_path() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    Ok(exe.with_file_name(format!("tpui-pdf{}", std::env::consts::EXE_SUFFIX)))
}

/// What the worker sends back.
#[derive(Debug)]
pub enum WorkerEvent {
    Info {
        doc: u64,
        info: PdfInfo,
    },
    Page {
        doc: u64,
        page: u32,
        width: u32,
        image: PdfPageImage,
    },
    Failed {
        doc: u64,
        page: Option<u32>,
        message: String,
    },
    /// The worker ended. No more events follow.
    Stopped,
}

#[derive(Default)]
struct Outbox {
    open: Option<PdfRequest>,
    draw: Option<PdfRequest>,
    closed: bool,
}

/// A running worker. Dropping it ends the worker at once.
pub struct PdfWorker {
    outbox: Arc<(Mutex<Outbox>, Condvar)>,
    process: Arc<system::Process>,
}

impl PdfWorker {
    /// Starts the worker at `path`. Events arrive on the returned channel until it reports `Stopped`.
    pub fn start(path: &Path) -> Result<(Self, async_channel::Receiver<WorkerEvent>), String> {
        let spawned =
            system::spawn(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let (sender, receiver) = async_channel::bounded(2);
        let outbox: Arc<(Mutex<Outbox>, Condvar)> = Arc::default();
        let process = Arc::new(spawned.process);

        let writer_outbox = outbox.clone();
        let stdin = spawned.stdin;
        thread::Builder::new()
            .name("pdf-worker-writer".into())
            .spawn(move || write_requests(stdin, &writer_outbox))
            .map_err(|error| error.to_string())?;
        let reader_process = process.clone();
        let stdout = spawned.stdout;
        thread::Builder::new()
            .name("pdf-worker-reader".into())
            .spawn(move || {
                read_replies(stdout, &sender);
                // Waiting here, off the UI thread, reaps the process before the viewer hears it ended.
                reader_process.wait();
                let _ = sender.send_blocking(WorkerEvent::Stopped);
            })
            .map_err(|error| error.to_string())?;
        Ok((Self { outbox, process }, receiver))
    }

    fn put(&self, put: impl FnOnce(&mut Outbox)) {
        let (outbox, wake) = &*self.outbox;
        if let Ok(mut outbox) = outbox.lock() {
            put(&mut outbox);
            wake.notify_one();
        }
    }

    /// Asks for the page sizes of a file. A draw request not yet sent belongs to the document before, so it goes.
    pub fn open(&self, doc: u64, path: PathBuf) {
        self.put(|outbox| {
            outbox.open = Some(PdfRequest::Open { doc, path });
            outbox.draw = None;
        });
    }

    /// Asks for pages, replacing a draw request not yet sent.
    pub fn draw(&self, doc: u64, width: u32, pages: Vec<u32>) {
        self.put(|outbox| outbox.draw = Some(PdfRequest::Draw { doc, width, pages }));
    }

    /// What the worker uses now.
    pub fn usage(&self) -> Option<ProcessUsage> {
        self.process.usage()
    }

    #[cfg(test)]
    fn id(&self) -> u32 {
        self.process.id()
    }
}

impl Drop for PdfWorker {
    fn drop(&mut self) {
        self.put(|outbox| outbox.closed = true);
        // Ending the job ends the worker without waiting for the page it is drawing.
        self.process.end();
    }
}

fn write_requests(stdin: File, outbox: &(Mutex<Outbox>, Condvar)) {
    let mut stdin = BufWriter::new(stdin);
    let (lock, wake) = outbox;
    loop {
        let request = {
            let Ok(mut outbox) = lock.lock() else {
                return;
            };
            loop {
                if outbox.closed {
                    return;
                }
                if let Some(request) = outbox.open.take().or_else(|| outbox.draw.take()) {
                    break request;
                }
                outbox = match wake.wait(outbox) {
                    Ok(outbox) => outbox,
                    Err(_) => return,
                };
            }
        };
        if write_frame(&mut stdin, &request.encode()).is_err() {
            return;
        }
    }
}

fn read_replies(stdout: File, sender: &async_channel::Sender<WorkerEvent>) {
    let mut stdout = BufReader::new(stdout);
    while let Ok(Some(body)) = read_frame(&mut stdout) {
        let Ok(reply) = PdfReply::decode(&body) else {
            return;
        };
        let event = match reply {
            PdfReply::Info { doc, info } => WorkerEvent::Info { doc, info },
            PdfReply::Page {
                doc,
                page,
                width,
                image,
            } => WorkerEvent::Page {
                doc,
                page,
                width,
                image,
            },
            PdfReply::Failed { doc, page, message } => WorkerEvent::Failed { doc, page, message },
        };
        if sender.send_blocking(event).is_err() {
            return;
        }
    }
}

#[cfg(target_os = "windows")]
mod system {
    use std::ffi::{OsStr, c_void};
    use std::fs::File;
    use std::io;
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::path::Path;
    use std::sync::Mutex;

    use windows::Win32::Foundation::{
        HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation,
    };
    use windows::Win32::Security::SECURITY_ATTRIBUTES;
    use windows::Win32::System::JobObjects::{
        CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
    };
    use windows::Win32::System::Pipes::CreatePipe;
    use windows::Win32::System::Threading::{
        CREATE_NO_WINDOW, CreateProcessW, DeleteProcThreadAttributeList,
        EXTENDED_STARTUPINFO_PRESENT, INFINITE, InitializeProcThreadAttributeList,
        LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
        PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW,
        UpdateProcThreadAttribute, WaitForSingleObject,
    };
    use windows::core::{PCWSTR, PWSTR};

    use crate::usage_log::{ProcessUsage, process_usage};

    fn owned(handle: HANDLE) -> OwnedHandle {
        // SAFETY: every caller passes a handle it just received and owns.
        unsafe { OwnedHandle::from_raw_handle(handle.0) }
    }

    fn raw(handle: &OwnedHandle) -> HANDLE {
        HANDLE(handle.as_raw_handle())
    }

    /// The worker process and the job that holds it.
    pub struct Process {
        process: OwnedHandle,
        id: u32,
        /// The only handle to the job. Closing it ends the worker.
        job: Mutex<Option<OwnedHandle>>,
    }

    impl Process {
        pub fn end(&self) {
            if let Ok(mut job) = self.job.lock() {
                job.take();
            }
        }

        pub fn wait(&self) {
            // SAFETY: the process handle stays open for as long as `self` lives.
            unsafe {
                WaitForSingleObject(raw(&self.process), INFINITE);
            }
        }

        pub fn usage(&self) -> Option<ProcessUsage> {
            process_usage(raw(&self.process), self.id)
        }

        #[cfg(test)]
        pub fn id(&self) -> u32 {
            self.id
        }
    }

    pub struct Spawned {
        pub stdin: File,
        pub stdout: File,
        pub process: Process,
    }

    /// A pipe whose `child` end is inheritable and whose `parent` end is not.
    fn pipe(child_reads: bool) -> io::Result<(OwnedHandle, OwnedHandle)> {
        let inheritable = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: true.into(),
        };
        let (mut read, mut write) = (HANDLE::default(), HANDLE::default());
        // SAFETY: both out-pointers are valid, and the handles returned are owned right below.
        unsafe { CreatePipe(&mut read, &mut write, Some(&inheritable), 0)? };
        let (read, write) = (owned(read), owned(write));
        let (child, parent) = if child_reads {
            (read, write)
        } else {
            (write, read)
        };
        // SAFETY: the handle is open and owned here.
        unsafe { SetHandleInformation(raw(&parent), HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0))? };
        Ok((child, parent))
    }

    /// A job that ends its processes when its last handle closes. The handle is not inheritable, so the worker does not
    /// keep its own job alive.
    fn job() -> io::Result<OwnedHandle> {
        // SAFETY: no attributes and no name; the handle returned is owned right below.
        let job = owned(unsafe { CreateJobObjectW(None, PCWSTR::null())? });
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the structure is the one this information class takes, with its size.
        unsafe {
            SetInformationJobObject(
                raw(&job),
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const c_void,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )?
        };
        Ok(job)
    }

    /// An attribute list naming exactly the handles the worker inherits and the job it starts in.
    struct Attributes {
        buffer: Vec<u8>,
        /// The values the list points at must outlive it.
        _handles: Box<[HANDLE; 2]>,
        _jobs: Box<[HANDLE; 1]>,
    }

    impl Attributes {
        fn new(handles: [HANDLE; 2], job: HANDLE) -> io::Result<Self> {
            let mut size = 0;
            // SAFETY: asking for the size only; this call is expected to fail with the size filled in.
            let _ = unsafe { InitializeProcThreadAttributeList(None, 2, None, &mut size) };
            let mut attributes = Self {
                buffer: vec![0; size],
                _handles: Box::new(handles),
                _jobs: Box::new([job]),
            };
            let list = attributes.list();
            // SAFETY: the buffer has the size asked for, and the values outlive the list as fields of `self`.
            unsafe {
                InitializeProcThreadAttributeList(Some(list), 2, None, &mut size)?;
                UpdateProcThreadAttribute(
                    list,
                    0,
                    PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                    Some(attributes._handles.as_ptr() as *const c_void),
                    size_of::<[HANDLE; 2]>(),
                    None,
                    None,
                )?;
                UpdateProcThreadAttribute(
                    list,
                    0,
                    PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
                    Some(attributes._jobs.as_ptr() as *const c_void),
                    size_of::<[HANDLE; 1]>(),
                    None,
                    None,
                )?;
            }
            Ok(attributes)
        }

        fn list(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
            LPPROC_THREAD_ATTRIBUTE_LIST(self.buffer.as_mut_ptr() as *mut c_void)
        }
    }

    impl Drop for Attributes {
        fn drop(&mut self) {
            // SAFETY: the list was initialized in `new`, which is the only way to make one.
            unsafe { DeleteProcThreadAttributeList(self.list()) };
        }
    }

    pub fn spawn(path: &Path) -> io::Result<Spawned> {
        let job = job()?;
        let (child_stdin, parent_stdin) = pipe(true)?;
        let (child_stdout, parent_stdout) = pipe(false)?;
        let mut attributes = Attributes::new([raw(&child_stdin), raw(&child_stdout)], raw(&job))?;

        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = raw(&child_stdin);
        startup.StartupInfo.hStdOutput = raw(&child_stdout);
        // No stderr: the worker's messages go nowhere.
        startup.StartupInfo.hStdError = HANDLE::default();
        startup.lpAttributeList = attributes.list();

        let application: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        let mut command_line: Vec<u16> = OsStr::new(&format!("\"{}\"", path.display()))
            .encode_wide()
            .chain([0])
            .collect();
        let mut information = PROCESS_INFORMATION::default();
        // SAFETY: every pointer refers to data that lives until the call returns, the command line is a writable buffer,
        // and only the two listed handles are inherited.
        unsafe {
            CreateProcessW(
                PCWSTR(application.as_ptr()),
                Some(PWSTR(command_line.as_mut_ptr())),
                None,
                None,
                true,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_NO_WINDOW,
                None,
                PCWSTR::null(),
                &startup.StartupInfo,
                &mut information,
            )?
        };
        drop(owned(information.hThread));
        let process = owned(information.hProcess);
        // The child's ends close here, so the pipes end when the worker does.
        drop(attributes);
        drop((child_stdin, child_stdout));
        Ok(Spawned {
            stdin: File::from(parent_stdin),
            stdout: File::from(parent_stdout),
            process: Process {
                process,
                id: information.dwProcessId,
                job: Mutex::new(Some(job)),
            },
        })
    }
}

#[cfg(not(target_os = "windows"))]
mod system {
    use std::fs::File;
    use std::io;
    use std::path::Path;

    use crate::usage_log::ProcessUsage;

    pub struct Process;

    impl Process {
        pub fn end(&self) {}

        pub fn wait(&self) {}

        pub fn usage(&self) -> Option<ProcessUsage> {
            None
        }
    }

    pub struct Spawned {
        pub stdin: File,
        pub stdout: File,
        pub process: Process,
    }

    pub fn spawn(_path: &Path) -> io::Result<Spawned> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "PDF pages are drawn on Windows",
        ))
    }
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::Duration;

    /// The worker built next to the test binary's parent folder, as for the code core.
    fn built_worker() -> PathBuf {
        let exe = std::env::current_exe().unwrap();
        exe.parent()
            .and_then(Path::parent)
            .unwrap()
            .join("tpui-pdf.exe")
    }

    fn next_event(events: &async_channel::Receiver<WorkerEvent>) -> WorkerEvent {
        for _ in 0..1000 {
            match events.try_recv() {
                Ok(event) => return event,
                Err(async_channel::TryRecvError::Empty) => thread::sleep(Duration::from_millis(10)),
                Err(error) => panic!("{error}"),
            }
        }
        panic!("no event from the PDF worker");
    }

    fn is_running(id: u32) -> bool {
        let output = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {id}"), "/NH"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).contains(&id.to_string())
    }

    fn sample(dir: &Path) -> PathBuf {
        // Two pages: 200 x 100 points and 100 x 300 points.
        let content = "0 0 0 rg 0 0 100 100 re f";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 5 0 R >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 300] >>".to_string(),
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            ),
        ];
        let mut out = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (ix, object) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.push_str(&format!("{} 0 obj\n{object}\nendobj\n", ix + 1));
        }
        let xref = out.len();
        out.push_str("xref\n0 6\n0000000000 65535 f \n");
        for offset in offsets {
            out.push_str(&format!("{offset:010} 00000 n \n"));
        }
        out.push_str(&format!(
            "trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
        ));
        let path = dir.join("두 쪽.pdf");
        std::fs::write(&path, out).unwrap();
        path
    }

    #[test]
    fn reads_sizes_and_pages_through_the_worker() {
        let dir = tempfile::tempdir().unwrap();
        let path = sample(dir.path());
        let (worker, events) = PdfWorker::start(&built_worker()).unwrap();
        worker.open(1, path.clone());
        let WorkerEvent::Info { doc: 1, info } = next_event(&events) else {
            panic!("expected sizes");
        };
        assert_eq!(info.pages.len(), 2);
        worker.draw(1, 300, vec![1, 0]);
        let mut pages = Vec::new();
        for _ in 0..2 {
            let WorkerEvent::Page {
                doc: 1,
                page,
                width: 300,
                image,
            } = next_event(&events)
            else {
                panic!("expected a page");
            };
            assert_eq!(image.bgra.len(), (image.width * image.height * 4) as usize);
            pages.push(page);
        }
        assert_eq!(pages, [1, 0]);
        // A draw request for a document the worker no longer shows is dropped, and a missing file reports itself.
        worker.open(2, dir.path().join("없음.pdf"));
        worker.draw(1, 300, vec![0]);
        let WorkerEvent::Failed {
            doc: 2,
            page: None,
            message,
        } = next_event(&events)
        else {
            panic!("expected a failure");
        };
        assert_eq!(message, "file not found");
        assert!(worker.usage().is_some_and(|usage| usage.ram_bytes > 0));
        // The file can be rewritten in place between requests, once the worker has closed it after the last page.
        rewrite_soon(&path);
    }

    /// Rewrites a file in place, giving the worker a moment to close it after its last page.
    fn rewrite_soon(path: &Path) {
        for _ in 0..100 {
            if std::fs::write(path, b"%PDF-1.4\n").is_ok() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("the PDF stayed locked after the worker finished with it");
    }

    #[test]
    fn dropping_the_connection_ends_the_worker() {
        let (worker, events) = PdfWorker::start(&built_worker()).unwrap();
        let id = worker.id();
        assert!(is_running(id));
        drop(worker);
        assert!(matches!(next_event(&events), WorkerEvent::Stopped));
        assert!(!is_running(id));
    }

    #[test]
    fn the_worker_runs_inside_a_job_that_ends_it() {
        // The job is the worker's only tie to this process: what is left when the viewer dies is the job closing.
        let (worker, events) = PdfWorker::start(&built_worker()).unwrap();
        let id = worker.id();
        worker.process.end();
        assert!(matches!(next_event(&events), WorkerEvent::Stopped));
        assert!(!is_running(id));
    }

    #[test]
    fn reports_when_the_worker_cannot_start() {
        assert!(PdfWorker::start(Path::new("없는 실행 파일.exe")).is_err());
    }
}
