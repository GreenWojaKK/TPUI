//! Files the viewer is asked to open from outside: on its command line, as Windows does for an associated file, or from a
//! later start of the viewer.
//!
//! The first viewer of a user listens on a named pipe. A later start with files hands them over and ends before it opens a
//! window, so that files open as tabs of one window. A later start without files opens a window of its own. The pipe goes
//! away with the viewer that made it, however that viewer ends.

use std::path::PathBuf;

use async_channel::{Receiver, Sender};

/// A file to open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenRequest {
    pub path: PathBuf,
    /// The file came from a later start, so the window should come to the front.
    pub bring_forward: bool,
}

pub enum Startup {
    /// This process is a viewer, and these are the files it is asked to open.
    Run(Receiver<OpenRequest>),
    /// The running viewer took the files, so this process ends.
    HandedOver,
}

/// The files on the command line. Paths are made absolute here, since a running viewer has another working folder.
fn command_line_paths() -> Vec<PathBuf> {
    std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .map(|path| std::path::absolute(&path).unwrap_or(path))
        .collect()
}

fn queue(paths: Vec<PathBuf>, sender: &Sender<OpenRequest>) {
    for path in paths {
        // The channel is unbounded and its receiver is still here.
        let _ = sender.send_blocking(OpenRequest {
            path,
            bring_forward: false,
        });
    }
}

/// Decides what this process is, before any window exists.
pub fn begin() -> Startup {
    system::begin(command_line_paths())
}

#[cfg(target_os = "windows")]
mod system {
    use std::ffi::{OsStr, OsString};
    use std::fs::{File, OpenOptions};
    use std::io::{self, Read, Write};
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use std::path::{Path, PathBuf};
    use std::thread;
    use std::time::{Duration, Instant};

    use async_channel::Sender;
    use windows::Win32::Foundation::{
        ERROR_NO_DATA, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, HANDLE,
    };
    use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_INBOUND};
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };
    use windows::core::HSTRING;

    use super::{OpenRequest, Startup, queue};

    /// How long a later start keeps trying to reach a viewer that is between two connections, or just starting.
    const HANDOVER_WAIT: Duration = Duration::from_secs(2);
    const HANDOVER_RETRY: Duration = Duration::from_millis(20);
    /// Most bytes read from one later start, far above what a command line can hold.
    const MAX_MESSAGE_BYTES: u64 = 1 << 20;
    const PIPE_BUFFER_BYTES: u32 = 64 * 1024;

    /// The pipe's name, one per app folder and so one per user. FNV-1a keeps it the same across builds.
    pub(super) fn pipe_name(app_dir: &Path) -> String {
        let hash = app_dir
            .as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
            });
        format!(r"\\.\pipe\tpui-open-{hash:016x}")
    }

    /// Paths as UTF-16, each ended by a zero, which no path holds.
    pub(super) fn encode_paths(paths: &[PathBuf]) -> Vec<u8> {
        paths
            .iter()
            .flat_map(|path| path.as_os_str().encode_wide().chain([0]))
            .flat_map(u16::to_le_bytes)
            .collect()
    }

    pub(super) fn decode_paths(bytes: &[u8]) -> Vec<PathBuf> {
        let (units, _) = bytes.as_chunks::<2>();
        let units: Vec<u16> = units.iter().map(|unit| u16::from_le_bytes(*unit)).collect();
        units
            .split(|unit| *unit == 0)
            .filter(|path| !path.is_empty())
            .map(|path| PathBuf::from(OsString::from_wide(path)))
            .collect()
    }

    /// Makes a listening end of the pipe. As the first one, this fails with `PermissionDenied` when the name is taken.
    pub(super) fn create_instance(name: &str, first: bool) -> io::Result<File> {
        let flags = if first {
            PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE
        } else {
            PIPE_ACCESS_INBOUND
        };
        // SAFETY: the name outlives the call, and the handle returned is owned by the `File` below.
        unsafe {
            let handle = CreateNamedPipeW(
                &HSTRING::from(OsStr::new(name)),
                flags,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                0,
                PIPE_BUFFER_BYTES,
                0,
                None,
            );
            if handle.is_invalid() {
                return Err(io::Error::last_os_error());
            }
            Ok(File::from_raw_handle(handle.0))
        }
    }

    /// Waits for a later start to connect. A start that connected before this call also counts, even one that has
    /// written and closed already: what it wrote can still be read.
    fn wait_for_client(pipe: &File) -> io::Result<()> {
        // SAFETY: the handle is open for as long as `pipe` lives, and the call runs without overlapped I/O.
        match unsafe { ConnectNamedPipe(HANDLE(pipe.as_raw_handle()), None) } {
            Ok(()) => Ok(()),
            Err(error)
                if error.code() == ERROR_PIPE_CONNECTED.to_hresult()
                    || error.code() == ERROR_NO_DATA.to_hresult() =>
            {
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Serves later starts until the app ends. The next end of the pipe exists before this one is read, so that a start
    /// arriving meanwhile finds it.
    pub(super) fn listen(first: File, name: String, sender: Sender<OpenRequest>) {
        let mut pipe = first;
        loop {
            if wait_for_client(&pipe).is_err() {
                return;
            }
            let Ok(next) = create_instance(&name, false) else {
                return;
            };
            let mut message = Vec::new();
            // A client that went away leaves whatever it wrote.
            let _ = (&pipe).take(MAX_MESSAGE_BYTES).read_to_end(&mut message);
            for path in decode_paths(&message) {
                let request = OpenRequest {
                    path,
                    bring_forward: true,
                };
                if sender.send_blocking(request).is_err() {
                    return;
                }
            }
            pipe = next;
        }
    }

    /// Gives the paths to the viewer listening on the pipe.
    pub(super) fn hand_over(name: &str, paths: &[PathBuf]) -> io::Result<()> {
        let mut pipe = OpenOptions::new().write(true).open(name)?;
        pipe.write_all(&encode_paths(paths))
    }

    /// Whether another try may reach the viewer: its pipe is between two connections.
    fn may_retry(error: &io::Error) -> bool {
        error.kind() == io::ErrorKind::NotFound
            || error.raw_os_error() == Some(ERROR_PIPE_BUSY.0 as i32)
    }

    pub(super) fn begin(paths: Vec<PathBuf>) -> Startup {
        let (sender, receiver) = async_channel::unbounded();
        let app_dir = viewer_ui::storage::app_dir("TPUI");
        if let Some(name) = app_dir.as_deref().map(pipe_name) {
            let deadline = Instant::now() + HANDOVER_WAIT;
            loop {
                match create_instance(&name, true) {
                    Ok(first) => {
                        let listener = sender.clone();
                        // Without this thread the viewer still works, and later starts open their own windows.
                        let _ = thread::Builder::new()
                            .name("open-requests".into())
                            .spawn(move || listen(first, name, listener));
                        break;
                    }
                    Err(error)
                        if error.kind() == io::ErrorKind::PermissionDenied && !paths.is_empty() =>
                    {
                        match hand_over(&name, &paths) {
                            Ok(()) => return Startup::HandedOver,
                            Err(error) if may_retry(&error) && Instant::now() < deadline => {
                                thread::sleep(HANDOVER_RETRY);
                            }
                            Err(_) => break,
                        }
                    }
                    // A start without files opens its own window, as does one that cannot use the pipe.
                    Err(_) => break,
                }
            }
        }
        queue(paths, &sender);
        Startup::Run(receiver)
    }
}

#[cfg(not(target_os = "windows"))]
mod system {
    use std::path::PathBuf;

    use super::{Startup, queue};

    pub(super) fn begin(paths: Vec<PathBuf>) -> Startup {
        let (sender, receiver) = async_channel::unbounded();
        queue(paths, &sender);
        Startup::Run(receiver)
    }
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::system::*;
    use super::*;
    use std::ffi::OsString;
    use std::io;
    use std::os::windows::ffi::OsStringExt;
    use std::path::Path;
    use std::thread;
    use std::time::Duration;

    /// A pipe name no other test or viewer uses.
    fn unique_name(test: &str) -> String {
        format!(r"\\.\pipe\tpui-test-{test}-{}", std::process::id())
    }

    #[test]
    fn carries_any_path_a_file_can_have() {
        let paths = vec![
            PathBuf::from(r"C:\data\운행 자료\한글 파일 (1).jsonl"),
            PathBuf::from(r"\\server\share\a b.py"),
            // Windows paths may hold half of a surrogate pair, which is not valid Unicode.
            PathBuf::from(OsString::from_wide(&[0x43, 0xD800, 0x44])),
        ];
        assert_eq!(decode_paths(&encode_paths(&paths)), paths);
        assert!(decode_paths(&[]).is_empty());
        assert!(decode_paths(&[0, 0, 0, 0]).is_empty());
        // A cut message keeps what is whole.
        let bytes = encode_paths(&[PathBuf::from("ab")]);
        assert_eq!(decode_paths(&bytes[..3]), [PathBuf::from("a")]);
    }

    #[test]
    fn names_the_pipe_by_the_app_folder() {
        let name = pipe_name(Path::new(r"C:\Users\a\AppData\Local\TPUI"));
        assert_eq!(name, pipe_name(Path::new(r"C:\Users\a\AppData\Local\TPUI")));
        assert_ne!(name, pipe_name(Path::new(r"C:\Users\b\AppData\Local\TPUI")));
        assert!(name.starts_with(r"\\.\pipe\tpui-open-"));
        assert_eq!(name.len(), r"\\.\pipe\tpui-open-".len() + 16);
    }

    #[test]
    fn only_the_first_viewer_gets_the_name_and_it_frees_on_exit() {
        let name = unique_name("first");
        let first = create_instance(&name, true).unwrap();
        let taken = create_instance(&name, true).unwrap_err();
        assert_eq!(taken.kind(), io::ErrorKind::PermissionDenied);
        // Further ends of the same viewer's pipe are fine.
        drop(create_instance(&name, false).unwrap());
        drop(first);
        drop(create_instance(&name, true).unwrap());
    }

    #[test]
    fn says_when_no_viewer_listens() {
        let error = hand_over(&unique_name("nobody"), &[PathBuf::from("a")]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn later_starts_hand_their_files_to_the_listening_viewer() {
        let name = unique_name("listen");
        let first = create_instance(&name, true).unwrap();
        let (sender, receiver) = async_channel::unbounded();
        let listener_name = name.clone();
        thread::spawn(move || listen(first, listener_name, sender));
        // Starts close together: each retries until the listener has its next end ready.
        let starts: Vec<_> = (0..8)
            .map(|start| {
                let name = name.clone();
                thread::spawn(move || {
                    let paths = [
                        PathBuf::from(format!(r"C:\자료\{start}-a.py")),
                        PathBuf::from(format!(r"C:\자료\{start}-b.py")),
                    ];
                    for _ in 0..200 {
                        if hand_over(&name, &paths).is_ok() {
                            return true;
                        }
                        thread::sleep(Duration::from_millis(10));
                    }
                    false
                })
            })
            .collect();
        for start in starts {
            assert!(start.join().unwrap());
        }
        let mut received = Vec::new();
        for _ in 0..16 {
            let request = receiver.recv_blocking().unwrap();
            assert!(request.bring_forward);
            received.push(request.path);
        }
        // Each start's files stay in order, and none is lost.
        for start in 0..8 {
            let a = PathBuf::from(format!(r"C:\자료\{start}-a.py"));
            let b = PathBuf::from(format!(r"C:\자료\{start}-b.py"));
            let at = |path| received.iter().position(|found| *found == path).unwrap();
            assert!(at(a) < at(b));
        }
    }

    #[test]
    fn files_of_this_start_do_not_ask_for_the_front() {
        let (sender, receiver) = async_channel::unbounded();
        queue(vec![PathBuf::from("a.md")], &sender);
        assert_eq!(
            receiver.recv_blocking().unwrap(),
            OpenRequest {
                path: PathBuf::from("a.md"),
                bring_forward: false,
            }
        );
    }
}
