//! Connection to the code core, the Go process that reads source files and answers with their tokens.
//!
//! The core runs only while a code tab is shown. One thread writes requests and one reads answers, so the UI thread never
//! waits for the core. Only the latest request waits to be sent. When the connection ends, the core ends too: closing its
//! stdin stops it, including when this process dies without cleaning up.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use viewer_core::FileStamp;

use crate::usage_log::{ProcessUsage, read_child_process};

/// Name of the core's executable, next to the viewer's.
fn core_file_name() -> String {
    format!("tpui-core{}", std::env::consts::EXE_SUFFIX)
}

/// Where the core is expected: next to the running viewer.
pub fn core_path() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    Ok(exe.with_file_name(core_file_name()))
}

/// A request for the tokens touching a byte range of a file at a stamp.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenRequest {
    pub id: u64,
    pub path: PathBuf,
    pub stamp: FileStamp,
    pub start: usize,
    pub end: usize,
}

impl TokenRequest {
    fn to_line(&self) -> String {
        let modified = self
            .stamp
            .modified
            .and_then(|time: SystemTime| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |since| since.as_nanos());
        let mut line = serde_json::json!({
            "id": self.id,
            "path": self.path.to_string_lossy(),
            "stamp": {"len": self.stamp.len, "modified": modified as u64},
            "start": self.start,
            "end": self.end,
        })
        .to_string();
        line.push('\n');
        line
    }
}

/// What the core sends back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoreEvent {
    /// Tokens as a flat list of start, length and kind. `language` is empty when the file has no rules.
    Tokens {
        id: u64,
        language: String,
        tokens: Vec<u32>,
    },
    /// The file on disk is not the version asked for.
    Stale { id: u64 },
    /// This request failed. The core keeps running.
    Failed { id: u64, message: String },
    /// The connection ended. No more events follow.
    Stopped { reason: String },
}

fn parse_answer(line: &str) -> Result<CoreEvent, String> {
    let value: Value = serde_json::from_str(line).map_err(|error| error.to_string())?;
    let id = value["id"].as_u64().ok_or("an answer has no id")?;
    if value["stale"].as_bool() == Some(true) {
        return Ok(CoreEvent::Stale { id });
    }
    if let Some(message) = value["error"].as_str() {
        return Ok(CoreEvent::Failed {
            id,
            message: message.to_string(),
        });
    }
    let tokens = value["tokens"]
        .as_array()
        .ok_or("an answer has no tokens")?
        .iter()
        .map(|token| token.as_u64().map(|token| token as u32))
        .collect::<Option<Vec<u32>>>()
        .ok_or("a token is not a number")?;
    if tokens.len() % 3 != 0 {
        return Err("tokens do not come in threes".to_string());
    }
    Ok(CoreEvent::Tokens {
        id,
        language: value["language"].as_str().unwrap_or_default().to_string(),
        tokens,
    })
}

#[derive(Default)]
struct Outbox {
    pending: Option<TokenRequest>,
    closed: bool,
}

/// A running core.
pub struct CodeCore {
    outbox: Arc<(Mutex<Outbox>, Condvar)>,
    child: Arc<Mutex<Child>>,
}

impl CodeCore {
    /// Starts the core at `path`. Events arrive on the returned channel until it reports `Stopped`.
    pub fn start(path: &Path) -> Result<(Self, async_channel::Receiver<CoreEvent>), String> {
        let mut command = Command::new(path);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // A window-less app must not flash a console window for its helper.
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let stdin = child.stdin.take().ok_or("the core has no stdin")?;
        let stdout = child.stdout.take().ok_or("the core has no stdout")?;
        let (sender, receiver) = async_channel::bounded(4);
        let outbox = Arc::new((Mutex::new(Outbox::default()), Condvar::new()));
        let child = Arc::new(Mutex::new(child));

        let writer_outbox = outbox.clone();
        thread::Builder::new()
            .name("code-core-writer".into())
            .spawn(move || write_requests(stdin, &writer_outbox))
            .map_err(|error| error.to_string())?;
        let reader_child = child.clone();
        thread::Builder::new()
            .name("code-core-reader".into())
            .spawn(move || {
                let reason = read_answers(stdout, &sender);
                // Reap the process, which also ends it if the connection broke while it was alive.
                if let Ok(mut child) = reader_child.lock() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                let _ = sender.send_blocking(CoreEvent::Stopped { reason });
            })
            .map_err(|error| error.to_string())?;
        Ok((Self { outbox, child }, receiver))
    }

    /// Queues a request, replacing one not yet sent.
    pub fn request(&self, request: TokenRequest) {
        let (outbox, wake) = &*self.outbox;
        if let Ok(mut outbox) = outbox.lock() {
            outbox.pending = Some(request);
            wake.notify_one();
        }
    }

    /// What the process uses now. `None` while the reader thread is ending it, since the UI thread never waits for it.
    pub fn usage(&self) -> Option<ProcessUsage> {
        let child = self.child.try_lock().ok()?;
        read_child_process(&child)
    }

    /// The process id, for checks from outside.
    #[cfg(test)]
    fn id(&self) -> u32 {
        self.child.lock().map(|child| child.id()).unwrap_or(0)
    }
}

impl Drop for CodeCore {
    fn drop(&mut self) {
        let (outbox, wake) = &*self.outbox;
        if let Ok(mut outbox) = outbox.lock() {
            outbox.closed = true;
            wake.notify_one();
        }
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
        }
    }
}

fn write_requests(mut stdin: ChildStdin, outbox: &(Mutex<Outbox>, Condvar)) {
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
                if let Some(request) = outbox.pending.take() {
                    break request;
                }
                outbox = match wake.wait(outbox) {
                    Ok(outbox) => outbox,
                    Err(_) => return,
                };
            }
        };
        if stdin
            .write_all(request.to_line().as_bytes())
            .and_then(|()| stdin.flush())
            .is_err()
        {
            return;
        }
    }
}

/// Forwards answers until the connection ends, and returns why it ended.
fn read_answers(
    stdout: std::process::ChildStdout,
    sender: &async_channel::Sender<CoreEvent>,
) -> String {
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return "the code core ended".to_string(),
            Ok(_) => match parse_answer(&line) {
                Ok(event) => {
                    if sender.send_blocking(event).is_err() {
                        return "the viewer stopped listening".to_string();
                    }
                }
                Err(error) => return format!("the code core sent a bad answer: {error}"),
            },
            Err(error) => return format!("cannot read from the code core: {error}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use viewer_core::file_stamp;

    /// The core built by the build script, next to the test binary's parent folder.
    fn built_core() -> PathBuf {
        let exe = std::env::current_exe().unwrap();
        exe.parent()
            .and_then(Path::parent)
            .unwrap()
            .join(core_file_name())
    }

    fn next_event(events: &async_channel::Receiver<CoreEvent>) -> CoreEvent {
        for _ in 0..500 {
            match events.try_recv() {
                Ok(event) => return event,
                Err(async_channel::TryRecvError::Empty) => thread::sleep(Duration::from_millis(10)),
                Err(error) => panic!("{error}"),
            }
        }
        panic!("no event from the core");
    }

    fn write_source(name: &str, text: &str) -> (tempfile::TempDir, PathBuf, FileStamp) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, text).unwrap();
        let stamp = file_stamp(&path).unwrap();
        (dir, path, stamp)
    }

    #[test]
    fn reads_every_answer_form() {
        assert_eq!(
            parse_answer(r#"{"id":3,"language":"Go","tokens":[0,7,4,8,1,6]}"#),
            Ok(CoreEvent::Tokens {
                id: 3,
                language: "Go".into(),
                tokens: vec![0, 7, 4, 8, 1, 6]
            })
        );
        assert_eq!(
            parse_answer(r#"{"id":4,"stale":true}"#),
            Ok(CoreEvent::Stale { id: 4 })
        );
        assert_eq!(
            parse_answer(r#"{"id":5,"error":"no"}"#),
            Ok(CoreEvent::Failed {
                id: 5,
                message: "no".into()
            })
        );
        assert!(parse_answer("not json").is_err());
        assert!(parse_answer(r#"{"tokens":[]}"#).is_err());
        assert!(parse_answer(r#"{"id":1,"tokens":[1,2]}"#).is_err());
    }

    #[test]
    fn exchanges_tokens_with_the_real_core() {
        let (_dir, path, stamp) = write_source("a.py", "def 함수(): # 끝\n");
        let (core, events) = CodeCore::start(&built_core()).unwrap();
        core.request(TokenRequest {
            id: 9,
            path: path.clone(),
            stamp,
            start: 0,
            end: 1000,
        });
        let CoreEvent::Tokens {
            id,
            language,
            tokens,
        } = next_event(&events)
        else {
            panic!("expected tokens");
        };
        assert_eq!((id, language.as_str()), (9, "Python"));
        // def, the function name in bytes, and the comment.
        assert_eq!(tokens, [0, 3, 4, 4, 6, 7, 14, 5, 1]);

        // A stamp the file does not have.
        let mut other = stamp;
        other.len += 1;
        core.request(TokenRequest {
            id: 10,
            path,
            stamp: other,
            start: 0,
            end: 1000,
        });
        assert_eq!(next_event(&events), CoreEvent::Stale { id: 10 });
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn reads_what_the_running_core_uses() {
        let (_dir, path, stamp) = write_source("a.py", "def f(): pass\n");
        let (core, events) = CodeCore::start(&built_core()).unwrap();
        core.request(TokenRequest {
            id: 1,
            path,
            stamp,
            start: 0,
            end: 100,
        });
        assert!(matches!(next_event(&events), CoreEvent::Tokens { .. }));
        let usage = core.usage().unwrap();
        assert_eq!(usage.id, core.id());
        assert!(usage.ram_bytes > 0 && usage.peak_ram_bytes >= usage.ram_bytes);
    }

    #[test]
    fn reports_when_the_core_cannot_start() {
        let result = CodeCore::start(Path::new("없는 실행 파일.exe"));
        assert!(result.is_err());
    }

    /// Whether a process with this id still runs.
    #[cfg(target_os = "windows")]
    fn is_running(id: u32) -> bool {
        let output = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {id}"), "/NH"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).contains(&id.to_string())
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn dropping_the_connection_ends_the_core() {
        let (core, events) = CodeCore::start(&built_core()).unwrap();
        let id = core.id();
        assert!(is_running(id));
        drop(core);
        assert!(matches!(next_event(&events), CoreEvent::Stopped { .. }));
        assert!(!is_running(id));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn closing_its_input_alone_ends_the_core() {
        // What is left when the viewer dies without cleaning up: only the pipe closes.
        let mut child = Command::new(built_core())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let id = child.id();
        assert!(is_running(id));
        drop(child.stdin.take());
        for _ in 0..200 {
            if child.try_wait().unwrap().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = child.kill();
        panic!("the core kept running after its input closed");
    }
}
