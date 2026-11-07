//! The viewer's own use of memory and CPU: read when something happens, logged one JSON line each in a file per day such
//! as `2026-11-05.jsonl`, and shown only when the user asks.
//!
//! A reading covers the viewer, and the code core and the PDF worker when they run. Memory is the working set, and CPU comes from the time the
//! system counts per process, so the share between two readings is exact however far apart they are. The timer writes a
//! line only when a shown value has changed. Lines are only ever appended, and the user deletes old files when they want
//! to.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Child;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, SecondsFormat};
use viewer_ui::format::format_bytes;

/// How often the timer takes a reading when nothing else has.
const TICK_INTERVAL: Duration = Duration::from_secs(60);
const MIB: u64 = 1024 * 1024;

/// Why a reading was taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageEvent {
    Start,
    /// A Parquet file or SQLite table finished opening.
    Open,
    /// Another tab showing data, or none, became active.
    Tab,
    /// A document finished its first read in its tab.
    Read,
    /// A document was read again after its file changed.
    Reread,
    /// The first tokens of a read document arrived, so the code core has built its table.
    Tokens,
    Snapshot,
    Tick,
    Exit,
}

impl UsageEvent {
    fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Open => "open",
            Self::Tab => "tab",
            Self::Read => "read",
            Self::Reread => "reread",
            Self::Tokens => "tokens",
            Self::Snapshot => "snapshot",
            Self::Tick => "tick",
            Self::Exit => "exit",
        }
    }
}

/// What one process uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessUsage {
    pub id: u32,
    pub ram_bytes: u64,
    /// The largest working set since the process started.
    pub peak_ram_bytes: u64,
    /// CPU time since the process started.
    pub cpu: Duration,
}

impl ProcessUsage {
    fn to_json(self) -> serde_json::Value {
        serde_json::json!({
            "pid": self.id,
            "ram_bytes": self.ram_bytes,
            "peak_ram_bytes": self.peak_ram_bytes,
            "cpu_seconds": self.cpu.as_secs_f64(),
        })
    }
}

#[cfg(target_os = "windows")]
mod system {
    use std::mem::size_of;
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;
    use std::time::Duration;

    use windows::Win32::Foundation::{FILETIME, HANDLE};
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    use super::ProcessUsage;

    /// A FILETIME counts units of 100 nanoseconds.
    fn duration(time: FILETIME) -> Duration {
        let units = u64::from(time.dwHighDateTime) << 32 | u64::from(time.dwLowDateTime);
        Duration::from_nanos(units.saturating_mul(100))
    }

    pub fn usage(process: HANDLE, id: u32) -> Option<ProcessUsage> {
        let mut memory = PROCESS_MEMORY_COUNTERS::default();
        let (mut created, mut ended, mut kernel, mut user) = Default::default();
        // SAFETY: both calls only write to the structures passed, and the handle stays open for the call.
        unsafe {
            GetProcessMemoryInfo(
                process,
                &mut memory,
                size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            )
            .ok()?;
            GetProcessTimes(process, &mut created, &mut ended, &mut kernel, &mut user).ok()?;
        }
        Some(ProcessUsage {
            id,
            ram_bytes: memory.WorkingSetSize as u64,
            peak_ram_bytes: memory.PeakWorkingSetSize as u64,
            cpu: duration(kernel) + duration(user),
        })
    }

    pub fn own_process() -> Option<ProcessUsage> {
        // SAFETY: this returns a constant standing for the current process.
        usage(unsafe { GetCurrentProcess() }, std::process::id())
    }

    pub fn child_process(child: &Child) -> Option<ProcessUsage> {
        usage(HANDLE(child.as_raw_handle()), child.id())
    }
}

#[cfg(not(target_os = "windows"))]
mod system {
    use std::process::Child;

    use super::ProcessUsage;

    pub fn own_process() -> Option<ProcessUsage> {
        None
    }

    pub fn child_process(_child: &Child) -> Option<ProcessUsage> {
        None
    }
}

/// What a child process uses, while its handle is open.
pub fn read_child_process(child: &Child) -> Option<ProcessUsage> {
    system::child_process(child)
}

/// What the process behind an open handle uses.
#[cfg(target_os = "windows")]
pub fn process_usage(process: windows::Win32::Foundation::HANDLE, id: u32) -> Option<ProcessUsage> {
    system::usage(process, id)
}

/// The helper processes running beside the viewer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Helpers {
    pub core: Option<ProcessUsage>,
    pub pdf: Option<ProcessUsage>,
}

/// The viewer and its helpers at one moment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Reading {
    viewer: ProcessUsage,
    helpers: Helpers,
}

/// CPU time a helper used since an earlier reading. A helper started in between counts from its start.
fn helper_cpu(now: Option<ProcessUsage>, then: Option<ProcessUsage>) -> Duration {
    match (now, then) {
        (Some(now), Some(then)) if now.id == then.id => now.cpu.saturating_sub(then.cpu),
        (Some(now), _) => now.cpu,
        (None, _) => Duration::ZERO,
    }
}

impl Reading {
    fn ram_bytes(&self) -> u64 {
        let helper = |usage: Option<ProcessUsage>| usage.map_or(0, |usage| usage.ram_bytes);
        self.viewer.ram_bytes + helper(self.helpers.core) + helper(self.helpers.pdf)
    }

    /// CPU time used since an earlier reading.
    fn cpu_since(&self, earlier: &Reading) -> Duration {
        self.viewer.cpu.saturating_sub(earlier.viewer.cpu)
            + helper_cpu(self.helpers.core, earlier.helpers.core)
            + helper_cpu(self.helpers.pdf, earlier.helpers.pdf)
    }
}

/// The share of the whole machine, in percent to one decimal, of CPU time over a stretch of time.
fn cpu_percent(cpu: Duration, elapsed: Duration, cpus: f64) -> Option<f64> {
    (!elapsed.is_zero() && cpus > 0.).then(|| {
        let percent = cpu.as_secs_f64() / elapsed.as_secs_f64() / cpus * 100.;
        (percent * 10.).round() / 10.
    })
}

/// A taken reading, as the status bar and the log show it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Measured {
    reading: Reading,
    /// The share since the previous reading. The first reading has none.
    cpu_percent: Option<f64>,
    logged: bool,
}

impl Measured {
    /// The text for the status bar, as in `88.6 MiB · CPU 0.3% · 14:32:10`.
    pub fn label(&self, time: DateTime<Local>) -> String {
        let cpu = self
            .cpu_percent
            .map_or("CPU -".to_string(), |percent| format!("CPU {percent:.1}%"));
        let ram = format_bytes(self.reading.ram_bytes());
        let clock = time.format("%H:%M:%S");
        if self.logged {
            format!("{ram} · {cpu} · {clock}")
        } else {
            format!("{ram} · {cpu} · {clock} · not logged")
        }
    }
}

/// The file on screen when a reading is taken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveFile {
    pub label: String,
    pub size: Option<u64>,
}

fn log_line(
    event: UsageEvent,
    file: Option<&ActiveFile>,
    reading: &Reading,
    cpu_percent: Option<f64>,
    time: DateTime<Local>,
) -> serde_json::Value {
    serde_json::json!({
        "time": time.to_rfc3339_opts(SecondsFormat::Secs, false),
        "event": event.as_str(),
        "file": file.map(|file| file.label.as_str()),
        "file_bytes": file.and_then(|file| file.size),
        "ram_bytes": reading.ram_bytes(),
        "cpu_percent": cpu_percent,
        "viewer": reading.viewer.to_json(),
        "core": reading.helpers.core.map(ProcessUsage::to_json),
        "pdf_worker": reading.helpers.pdf.map(ProcessUsage::to_json),
    })
}

pub struct UsageMonitor {
    /// The folder of the daily files, when the app has a folder of its own.
    dir: Option<PathBuf>,
    /// Logical processors, which CPU shares are relative to.
    cpus: f64,
    last_reading: Option<(Instant, Reading)>,
    /// Whole MiB and tenths of a percent of the last line written, which the timer compares with.
    last_written: Option<(u64, Option<i64>)>,
}

impl UsageMonitor {
    pub fn new(dir: Option<PathBuf>) -> Self {
        let cpus = std::thread::available_parallelism().map_or(1, |count| count.get());
        Self {
            dir,
            cpus: cpus as f64,
            last_reading: None,
            last_written: None,
        }
    }

    /// Whether the timer should take a reading now, since a minute has passed without one.
    pub fn tick_due(&self, now: Instant) -> bool {
        self.last_reading
            .is_some_and(|(at, _)| now.duration_since(at) >= TICK_INTERVAL)
    }

    /// Takes a reading and logs it. `None` when the system gives no reading.
    pub fn record(
        &mut self,
        event: UsageEvent,
        file: Option<&ActiveFile>,
        helpers: Helpers,
    ) -> Option<Measured> {
        let viewer = system::own_process()?;
        let reading = Reading { viewer, helpers };
        Some(self.take(event, file, reading, Instant::now(), Local::now()))
    }

    fn take(
        &mut self,
        event: UsageEvent,
        file: Option<&ActiveFile>,
        reading: Reading,
        now: Instant,
        time: DateTime<Local>,
    ) -> Measured {
        let cpu_percent = self.last_reading.and_then(|(at, earlier)| {
            let cpu = reading.cpu_since(&earlier);
            cpu_percent(cpu, now.duration_since(at), self.cpus)
        });
        self.last_reading = Some((now, reading));
        let shown = (
            reading.ram_bytes() / MIB,
            cpu_percent.map(|percent| (percent * 10.).round() as i64),
        );
        let mut logged = true;
        if event != UsageEvent::Tick || self.last_written != Some(shown) {
            self.last_written = Some(shown);
            let line = log_line(event, file, &reading, cpu_percent, time);
            logged = self.append(&line, time).is_ok();
        }
        Measured {
            reading,
            cpu_percent,
            logged,
        }
    }

    /// Appends a line to the day's file, in one write so that viewers running together do not mix their lines.
    fn append(&self, line: &serde_json::Value, time: DateTime<Local>) -> io::Result<()> {
        let dir = self.dir.as_ref().ok_or(io::ErrorKind::NotFound)?;
        fs::create_dir_all(dir)?;
        let path = dir.join(format!("{}.jsonl", time.format("%Y-%m-%d")));
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        file.write_all(format!("{line}\n").as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn usage(id: u32, ram_mib: u64, cpu_ms: u64) -> ProcessUsage {
        ProcessUsage {
            id,
            ram_bytes: ram_mib * MIB,
            peak_ram_bytes: (ram_mib + 10) * MIB,
            cpu: Duration::from_millis(cpu_ms),
        }
    }

    fn reading(ram_mib: u64, cpu_ms: u64) -> Reading {
        Reading {
            viewer: usage(1, ram_mib, cpu_ms),
            helpers: Helpers::default(),
        }
    }

    fn time() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 11, 5, 14, 32, 10).unwrap()
    }

    fn lines(dir: &std::path::Path) -> Vec<serde_json::Value> {
        let path = dir.join("2026-11-05.jsonl");
        match fs::read_to_string(path) {
            Ok(text) => text
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    fn monitor(dir: &std::path::Path, cpus: f64) -> UsageMonitor {
        UsageMonitor {
            dir: Some(dir.join("usage")),
            cpus,
            last_reading: None,
            last_written: None,
        }
    }

    #[test]
    fn shares_cpu_over_the_whole_machine() {
        let second = Duration::from_secs(1);
        assert_eq!(cpu_percent(second, second, 1.), Some(100.));
        assert_eq!(cpu_percent(second, second, 4.), Some(25.));
        assert_eq!(cpu_percent(second, second * 3, 16.), Some(2.1));
        assert_eq!(cpu_percent(Duration::ZERO, second, 8.), Some(0.));
        assert_eq!(cpu_percent(second, Duration::ZERO, 8.), None);
    }

    #[test]
    fn counts_the_core_and_its_restarts() {
        let mut earlier = reading(50, 1_000);
        earlier.helpers.core = Some(usage(7, 30, 400));
        let mut now = reading(60, 1_500);
        now.helpers.core = Some(usage(7, 34, 700));
        assert_eq!(now.ram_bytes(), 94 * MIB);
        assert_eq!(now.cpu_since(&earlier), Duration::from_millis(800));
        // A new core counts from its start, whatever the old one had used.
        now.helpers.core = Some(usage(8, 5, 100));
        assert_eq!(now.cpu_since(&earlier), Duration::from_millis(600));
        // A core that started after the earlier reading.
        assert_eq!(
            now.cpu_since(&reading(50, 1_000)),
            Duration::from_millis(600)
        );
        now.helpers.core = None;
        assert_eq!(now.cpu_since(&earlier), Duration::from_millis(500));
    }

    #[test]
    fn counts_the_pdf_worker_beside_the_core() {
        let mut earlier = reading(50, 1_000);
        earlier.helpers.pdf = Some(usage(9, 60, 2_000));
        let mut now = reading(50, 1_000);
        now.helpers.pdf = Some(usage(9, 64, 2_250));
        now.helpers.core = Some(usage(7, 30, 100));
        assert_eq!(now.ram_bytes(), (50 + 64 + 30) * MIB);
        assert_eq!(now.cpu_since(&earlier), Duration::from_millis(350));
        // A worker started again counts from its start.
        now.helpers.pdf = Some(usage(11, 40, 120));
        assert_eq!(now.cpu_since(&earlier), Duration::from_millis(220));
    }

    #[test]
    fn shows_a_reading_with_its_time() {
        let measured = Measured {
            reading: reading(88, 0),
            cpu_percent: Some(0.25),
            logged: true,
        };
        assert_eq!(measured.label(time()), "88.0 MiB · CPU 0.2% · 14:32:10");
        let first = Measured {
            cpu_percent: None,
            logged: false,
            ..measured
        };
        assert_eq!(
            first.label(time()),
            "88.0 MiB · CPU - · 14:32:10 · not logged"
        );
    }

    #[test]
    fn the_timer_waits_a_minute_after_any_reading() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = monitor(dir.path(), 1.);
        let start = Instant::now();
        // The start is recorded by the app, not by the timer.
        assert!(!monitor.tick_due(start + TICK_INTERVAL));
        monitor.take(UsageEvent::Start, None, reading(50, 0), start, time());
        assert!(!monitor.tick_due(start + Duration::from_secs(59)));
        let later = start + TICK_INTERVAL;
        assert!(monitor.tick_due(later));
        // Any reading restarts the wait.
        monitor.take(UsageEvent::Snapshot, None, reading(50, 0), later, time());
        assert!(!monitor.tick_due(later + Duration::from_secs(30)));
    }

    #[test]
    fn the_timer_logs_only_changed_values_and_events_always_log() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = monitor(dir.path(), 1.);
        let mut now = Instant::now();
        let mut take = |event, ram, cpu_ms| {
            now += TICK_INTERVAL;
            monitor.take(event, None, reading(ram, cpu_ms), now, time())
        };
        take(UsageEvent::Start, 50, 100);
        // The first tick has a CPU share where the start has none.
        take(UsageEvent::Tick, 50, 100);
        // Nothing moved.
        take(UsageEvent::Tick, 50, 100);
        take(UsageEvent::Tick, 50, 100);
        assert_eq!(lines(&dir.path().join("usage")).len(), 2);
        // Memory moved by a whole MiB, then CPU by a tenth of a percent.
        take(UsageEvent::Tick, 51, 100);
        take(UsageEvent::Tick, 51, 200);
        assert_eq!(lines(&dir.path().join("usage")).len(), 4);
        // An event logs even when nothing moved.
        take(UsageEvent::Snapshot, 51, 200);
        take(UsageEvent::Snapshot, 51, 200);
        let written = lines(&dir.path().join("usage"));
        assert_eq!(written.len(), 6);
        assert_eq!(written[5]["event"], "snapshot");
    }

    #[test]
    fn writes_each_process_and_the_file_on_screen() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = monitor(dir.path(), 2.);
        let start = Instant::now();
        let file = ActiveFile {
            label: r"C:\data\운행.jsonl".into(),
            size: Some(4_096),
        };
        monitor.take(UsageEvent::Start, None, reading(50, 1_000), start, time());
        let mut with_core = reading(60, 2_000);
        with_core.helpers.core = Some(usage(7, 30, 1_000));
        let measured = monitor.take(
            UsageEvent::Read,
            Some(&file),
            with_core,
            start + Duration::from_secs(2),
            time(),
        );
        assert!(measured.logged);
        assert_eq!(measured.cpu_percent, Some(50.));
        let written = lines(&dir.path().join("usage"));
        assert_eq!(written.len(), 2);
        assert_eq!(written[0]["event"], "start");
        assert!(written[0]["cpu_percent"].is_null() && written[0]["core"].is_null());
        assert!(written[0]["file"].is_null() && written[0]["file_bytes"].is_null());
        let line = &written[1];
        assert_eq!(line["event"], "read");
        assert_eq!(line["file"], r"C:\data\운행.jsonl");
        assert_eq!(line["file_bytes"], 4_096);
        assert_eq!(line["ram_bytes"], 90 * MIB);
        assert_eq!(line["cpu_percent"], 50.0);
        assert_eq!(line["viewer"]["pid"], 1);
        assert_eq!(line["viewer"]["peak_ram_bytes"], 70 * MIB);
        assert_eq!(line["viewer"]["cpu_seconds"], 2.0);
        assert_eq!(line["core"]["pid"], 7);
        assert_eq!(line["core"]["ram_bytes"], 30 * MIB);
        assert!(
            line["time"]
                .as_str()
                .unwrap()
                .starts_with("2026-11-05T14:32:10")
        );
    }

    #[test]
    fn says_when_a_line_could_not_be_written() {
        let dir = tempfile::tempdir().unwrap();
        // A file stands where the folder should go.
        let blocked = dir.path().join("blocked");
        fs::write(&blocked, b"").unwrap();
        let mut monitor = UsageMonitor {
            dir: Some(blocked),
            cpus: 1.,
            last_reading: None,
            last_written: None,
        };
        let start = Instant::now();
        let failed = monitor.take(UsageEvent::Snapshot, None, reading(50, 0), start, time());
        assert!(!failed.logged);
        let mut without_folder = UsageMonitor::new(None);
        let failed = without_folder.take(UsageEvent::Snapshot, None, reading(50, 0), start, time());
        assert!(!failed.logged);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn reads_this_process() {
        let first = system::own_process().unwrap();
        assert_eq!(first.id, std::process::id());
        assert!(first.ram_bytes > 0);
        assert!(first.peak_ram_bytes >= first.ram_bytes);
        // CPU time counts up while working.
        let mut sum = 0u64;
        let until = Instant::now() + Duration::from_millis(50);
        while Instant::now() < until {
            sum = sum.wrapping_add(1);
        }
        assert!(sum > 0);
        assert!(system::own_process().unwrap().cpu > first.cpu);
    }
}
