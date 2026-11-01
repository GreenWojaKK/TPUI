//! Message file: a text file that other programs write to send the app a message, such as a query from an agent or
//! from an editor, without a text input in the app.
//!
//! Each process has its own file, `inbox/<process id>.txt` in a folder the app chooses, created empty at start and
//! removed when the app ends. Files left by processes that no longer run, such as after a crash, are removed when
//! the next one is created. The app polls it: once the file has changed and then stayed the same for one polling
//! interval, its text is the next message. Waiting for the file to settle skips the empty file a writer leaves for a
//! moment between truncating and writing. A UTF-8 byte order mark is dropped and `\r\n` line ends become `\n`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// What tells one version of the file from another: its length and modification time.
type Signature = (u64, Option<SystemTime>);

pub struct Inbox {
    path: PathBuf,
    /// The version last delivered, or the empty file at start.
    seen: Option<Signature>,
    /// A changed version waiting for the next poll to show it stopped changing.
    settling: Option<Signature>,
}

impl Inbox {
    /// Creates the empty message file of this process in `dir/inbox`.
    pub fn create(dir: &Path) -> io::Result<Self> {
        let folder = dir.join("inbox");
        fs::create_dir_all(&folder)?;
        remove_stale(&folder);
        let path = folder.join(format!("{}.txt", std::process::id()));
        fs::write(&path, "")?;
        let seen = signature(&path).ok();
        Ok(Self {
            path,
            seen,
            settling: None,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Checks the file. Returns its text once it changed and then stayed the same since the previous call.
    pub fn poll(&mut self) -> Option<io::Result<String>> {
        let current = match signature(&self.path) {
            Ok(current) => current,
            // A writer may replace the file by deleting and creating it; wait for it to come back.
            Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
            Err(error) => return Some(Err(error)),
        };
        if Some(current) == self.seen {
            self.settling = None;
            return None;
        }
        if self.settling != Some(current) {
            self.settling = Some(current);
            return None;
        }
        self.settling = None;
        self.seen = Some(current);
        Some(fs::read(&self.path).and_then(|bytes| {
            decode(&bytes).map_err(|message| io::Error::new(io::ErrorKind::InvalidData, message))
        }))
    }
}

impl Drop for Inbox {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Removes message files of processes that no longer run.
fn remove_stale(folder: &Path) {
    let Ok(entries) = fs::read_dir(folder) else {
        return;
    };
    for path in entries.flatten().map(|entry| entry.path()) {
        let pid = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.parse::<u32>().ok());
        if let Some(pid) = pid
            && pid != std::process::id()
            && !process_runs(pid)
        {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(target_os = "windows")]
fn process_runs(pid: u32) -> bool {
    use windows::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return false;
        };
        let mut code = 0u32;
        let runs = GetExitCodeProcess(process, &mut code).is_ok() && code == STILL_ACTIVE.0 as u32;
        let _ = CloseHandle(process);
        runs
    }
}

#[cfg(not(target_os = "windows"))]
fn process_runs(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

fn signature(path: &Path) -> io::Result<Signature> {
    let metadata = fs::metadata(path)?;
    Ok((metadata.len(), metadata.modified().ok()))
}

/// Text of a message: UTF-8 without a byte order mark, with `\n` line ends.
pub fn decode(bytes: &[u8]) -> Result<String, String> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let text =
        std::str::from_utf8(bytes).map_err(|_| "the message file is not UTF-8 text".to_string())?;
    Ok(text.replace("\r\n", "\n"))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn drops_the_byte_order_mark_and_carriage_returns() {
        assert_eq!(
            decode(b"\xEF\xBB\xBFfind a\r\nsort -b\r\n").unwrap(),
            "find a\nsort -b\n"
        );
        assert_eq!(decode("필터".as_bytes()).unwrap(), "필터");
        assert!(decode(&[0xff, 0xfe, 0x00]).is_err());
    }

    #[test]
    fn delivers_a_message_once_the_file_settles() {
        let dir = tempfile::tempdir().unwrap();
        let mut inbox = Inbox::create(dir.path()).unwrap();
        let path = inbox.path().to_path_buf();
        assert_eq!(fs::read(&path).unwrap(), b"");
        assert!(inbox.poll().is_none(), "the empty file is not a message");

        std::thread::sleep(Duration::from_millis(20));
        fs::write(&path, "filter age >= 30").unwrap();
        assert!(inbox.poll().is_none(), "a change waits one poll");
        assert_eq!(inbox.poll().unwrap().unwrap(), "filter age >= 30");
        assert!(inbox.poll().is_none(), "a message is delivered once");

        std::thread::sleep(Duration::from_millis(20));
        fs::write(&path, "sort -age").unwrap();
        assert!(inbox.poll().is_none());
        std::thread::sleep(Duration::from_millis(20));
        fs::write(&path, "sort -age and more").unwrap();
        assert!(inbox.poll().is_none(), "still changing");
        assert_eq!(inbox.poll().unwrap().unwrap(), "sort -age and more");

        drop(inbox);
        assert!(!path.exists(), "the file goes with the app");
    }

    #[test]
    fn removes_files_of_processes_that_ended() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("inbox");
        fs::create_dir_all(&folder).unwrap();
        // Process ids are multiples of four on Windows, so this one never runs.
        let ended = folder.join("4294967295.txt");
        let other = folder.join("notes.txt");
        fs::write(&ended, "").unwrap();
        fs::write(&other, "").unwrap();
        let inbox = Inbox::create(dir.path()).unwrap();
        assert!(!ended.exists());
        assert!(
            other.exists(),
            "only files named by a process id are removed"
        );
        assert!(inbox.path().exists());
    }
}
