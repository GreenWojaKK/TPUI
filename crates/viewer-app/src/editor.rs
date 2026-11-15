//! Opens a file in the user's editor with the cursor at a place in it. The editor saves the file, and the document view
//! reads it again once it changes.
//!
//! The editor is `$VISUAL`, then `$EDITOR`, then the system's default app for the file: `xdg-open` on Linux, and on
//! Windows the app registered for the extension, or Notepad when there is none. The place goes to editors whose command
//! line is known by program name; other editors open the file at its start. Editors that run in a terminal open in a
//! terminal window of their own.

use std::env;
use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use viewer_core::SourcePosition;

/// How an editor's command line names a place in a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlaceForm {
    /// `"+call setcursorcharpos(line, column)" file`
    VimCall,
    /// `+line file`
    PlusLine,
    /// `+line,column file`
    PlusComma,
    /// `+line:column file`
    PlusColon,
    /// `-g file:line:column`
    Goto,
    /// `-nline -ccolumn file`
    NotepadPlusPlus,
    /// Only the file.
    File,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EditorKind {
    place: PlaceForm,
    /// Runs in a terminal, so it needs a terminal window.
    terminal: bool,
}

/// What the viewer knows of an editor, by the file name of its program.
fn editor_kind(program: &str) -> EditorKind {
    let name = Path::new(program)
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let (place, terminal) = match name.as_str() {
        "vim" | "nvim" => (PlaceForm::VimCall, true),
        "gvim" | "mvim" => (PlaceForm::VimCall, false),
        // A small vi may lack Vim's functions, but every vi takes a line.
        "vi" => (PlaceForm::PlusLine, true),
        "nano" => (PlaceForm::PlusComma, true),
        "gedit" | "emacs" | "emacsclient" => (PlaceForm::PlusColon, false),
        "code" | "code-insiders" | "codium" | "cursor" => (PlaceForm::Goto, false),
        "notepad++" => (PlaceForm::NotepadPlusPlus, false),
        "micro" | "hx" | "helix" | "kak" => (PlaceForm::File, true),
        _ => (PlaceForm::File, false),
    };
    EditorKind { place, terminal }
}

/// The arguments that open `path` at `at`, after the editor's own arguments.
fn place_args(form: PlaceForm, path: &Path, at: SourcePosition) -> Vec<OsString> {
    let SourcePosition { line, column } = at;
    let file = path.as_os_str().to_os_string();
    match form {
        PlaceForm::VimCall => vec![
            format!("+call setcursorcharpos({line}, {column})").into(),
            file,
        ],
        PlaceForm::PlusLine => vec![format!("+{line}").into(), file],
        PlaceForm::PlusComma => vec![format!("+{line},{column}").into(), file],
        PlaceForm::PlusColon => vec![format!("+{line}:{column}").into(), file],
        PlaceForm::Goto => {
            let mut target = file;
            target.push(format!(":{line}:{column}"));
            vec!["-g".into(), target]
        }
        PlaceForm::NotepadPlusPlus => {
            vec![
                format!("-n{line}").into(),
                format!("-c{column}").into(),
                file,
            ]
        }
        PlaceForm::File => vec![file],
    }
}

/// Splits an editor setting such as `code --wait` into words. Double or single quotes keep spaces in a word, as in a
/// path to the program.
fn split_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote = None;
    for ch in text.chars() {
        match quote {
            Some(open) if ch == open => quote = None,
            Some(_) => word.push(ch),
            None if ch == '"' || ch == '\'' => {
                quote = Some(ch);
                in_word = true;
            }
            None if ch.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            None => {
                word.push(ch);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(word);
    }
    words
}

/// Opens `path` with the cursor at `at`. Returns once the editor has started, or with the reason it could not start.
pub fn open_in_editor(path: &Path, at: SourcePosition) -> Result<(), String> {
    for variable in ["VISUAL", "EDITOR"] {
        let Some(value) = env::var_os(variable) else {
            continue;
        };
        let words = split_words(&value.to_string_lossy());
        let Some((program, own_args)) = words.split_first() else {
            continue;
        };
        let kind = editor_kind(program);
        let mut args: Vec<OsString> = own_args.iter().map(OsString::from).collect();
        args.extend(place_args(kind.place, path, at));
        let started = if kind.terminal {
            start_in_terminal(program, &args)
        } else {
            let mut command = Command::new(program_path(program));
            command.args(&args);
            start(command)
        };
        return started
            .map_err(|error| format!("{program} from ${variable} did not start: {error}"));
    }
    open_with_default_app(path)
}

/// Starts a program without tying it to the viewer: it gets no input or output of the viewer's, and a thread collects
/// its exit so that no finished process lingers.
fn start(mut command: Command) -> io::Result<()> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let child = command.spawn()?;
    reap(child);
    Ok(())
}

fn reap(mut child: Child) {
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}

/// A program named without a folder, found on `PATH` with the extensions Windows runs, so that scripts such as
/// `code.cmd` start too.
#[cfg(target_os = "windows")]
fn program_path(program: &str) -> std::path::PathBuf {
    let path = Path::new(program);
    if path.components().count() > 1 || path.extension().is_some() {
        return path.to_path_buf();
    }
    let extensions = env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    for folder in env::split_paths(&env::var_os("PATH").unwrap_or_default()) {
        for extension in extensions
            .split(';')
            .filter(|extension| !extension.is_empty())
        {
            let candidate = folder.join(format!("{program}{extension}"));
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    path.to_path_buf()
}

#[cfg(not(target_os = "windows"))]
fn program_path(program: &str) -> std::path::PathBuf {
    Path::new(program).to_path_buf()
}

/// Opens a terminal editor in a new console window. `start` gives the editor a console of its own with working input
/// and output, which a window app cannot hand down.
#[cfg(target_os = "windows")]
fn start_in_terminal(program: &str, args: &[OsString]) -> io::Result<()> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // Windows paths and editor arguments hold no double quotes, so quoting each word is enough.
    let quoted = |word: &str| format!("\"{}\"", word.replace('"', ""));
    let mut line = format!(
        "/c start \"\" {}",
        quoted(&program_path(program).to_string_lossy())
    );
    for arg in args {
        line.push(' ');
        line.push_str(&quoted(&arg.to_string_lossy()));
    }
    let mut command = Command::new("cmd");
    command.raw_arg(line).creation_flags(CREATE_NO_WINDOW);
    start(command)
}

/// Terminal programs and the arguments that come before the command they run, in the order tried after `$TERMINAL`.
#[cfg(not(target_os = "windows"))]
const TERMINALS: &[(&str, &[&str])] = &[
    ("x-terminal-emulator", &["-e"]),
    ("gnome-terminal", &["--"]),
    ("konsole", &["-e"]),
    ("xfce4-terminal", &["-x"]),
    ("alacritty", &["-e"]),
    ("kitty", &[]),
    ("foot", &[]),
    ("wezterm", &["start", "--"]),
    ("xterm", &["-e"]),
];

#[cfg(not(target_os = "windows"))]
fn on_path(program: &str) -> bool {
    env::split_paths(&env::var_os("PATH").unwrap_or_default())
        .any(|folder| folder.join(program).is_file())
}

/// Opens a terminal editor in a new terminal window: `$TERMINAL -e` when it is set, or the first terminal found.
#[cfg(not(target_os = "windows"))]
fn start_in_terminal(program: &str, args: &[OsString]) -> io::Result<()> {
    let chosen = env::var("TERMINAL")
        .ok()
        .filter(|terminal| !terminal.trim().is_empty())
        .map(|terminal| (terminal, vec!["-e".to_string()]))
        .or_else(|| {
            TERMINALS
                .iter()
                .find(|(terminal, _)| on_path(terminal))
                .map(|(terminal, before)| {
                    let before = before.iter().map(|arg| arg.to_string()).collect();
                    (terminal.to_string(), before)
                })
        });
    let Some((terminal, before)) = chosen else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no terminal program found to run it in",
        ));
    };
    let mut command = Command::new(terminal);
    command.args(before).arg(program).args(args);
    start(command)
}

#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
fn open_with_default_app(path: &Path) -> Result<(), String> {
    let mut command = Command::new("xdg-open");
    command.arg(path);
    start(command).map_err(|error| format!("xdg-open did not start: {error}"))
}

#[cfg(target_os = "macos")]
fn open_with_default_app(path: &Path) -> Result<(), String> {
    let mut command = Command::new("open");
    command.arg(path);
    start(command).map_err(|error| format!("open did not start: {error}"))
}

/// Opens the file with the app registered for its extension, or with Notepad when no app is registered. The shell
/// call runs on a thread of its own, since it needs COM set up on its thread.
#[cfg(target_os = "windows")]
fn open_with_default_app(path: &Path) -> Result<(), String> {
    use windows::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
    };
    use windows::Win32::UI::Shell::{
        ASSOCF_INIT_IGNOREUNKNOWN, ASSOCSTR_EXECUTABLE, AssocQueryStringW, ShellExecuteW,
    };
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{HSTRING, PCWSTR, w};

    let path = path.to_path_buf();
    let opened = std::thread::spawn(move || -> Result<(), String> {
        let extension = path
            .extension()
            .map(|extension| format!(".{}", extension.to_string_lossy()))
            .unwrap_or_default();
        // SAFETY: the strings live through the calls, and COM is set up and taken down on this thread only.
        unsafe {
            let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
            let mut length = 0u32;
            let registered = !extension.is_empty()
                && AssocQueryStringW(
                    ASSOCF_INIT_IGNOREUNKNOWN,
                    ASSOCSTR_EXECUTABLE,
                    &HSTRING::from(extension.as_str()),
                    PCWSTR::null(),
                    None,
                    &mut length,
                )
                .is_ok();
            let result = if registered {
                let file = HSTRING::from(path.as_os_str());
                let instance = ShellExecuteW(
                    None,
                    w!("open"),
                    &file,
                    PCWSTR::null(),
                    PCWSTR::null(),
                    SW_SHOWNORMAL,
                );
                // Values above 32 mean success.
                if instance.0 as usize > 32 {
                    Ok(())
                } else {
                    Err(format!(
                        "Windows could not open the file (code {})",
                        instance.0 as usize
                    ))
                }
            } else {
                let mut command = Command::new("notepad.exe");
                command.arg(&path);
                start(command).map_err(|error| format!("Notepad did not start: {error}"))
            };
            if com.is_ok() {
                CoUninitialize();
            }
            result
        }
    });
    opened
        .join()
        .unwrap_or_else(|_| Err("opening the file failed".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(line: usize, column: usize) -> SourcePosition {
        SourcePosition { line, column }
    }

    fn args(program: &str, path: &str, line: usize, column: usize) -> Vec<String> {
        place_args(
            editor_kind(program).place,
            Path::new(path),
            at(line, column),
        )
        .into_iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
    }

    #[test]
    fn names_the_place_the_way_each_editor_reads_it() {
        assert_eq!(
            args("vim", "a.md", 3, 5),
            ["+call setcursorcharpos(3, 5)", "a.md"]
        );
        assert_eq!(
            args("/usr/bin/nvim", "a.md", 3, 5)[0],
            "+call setcursorcharpos(3, 5)"
        );
        assert_eq!(args("vi", "a.md", 3, 5), ["+3", "a.md"]);
        assert_eq!(args("nano", "a.md", 3, 5), ["+3,5", "a.md"]);
        assert_eq!(args("gedit", "a.md", 3, 5), ["+3:5", "a.md"]);
        assert_eq!(args("emacs", "a.md", 3, 5), ["+3:5", "a.md"]);
        assert_eq!(
            args(r"C:\Programs\Code.exe", "문서 a.md", 3, 5),
            ["-g", "문서 a.md:3:5"]
        );
        assert_eq!(args("notepad++", "a.md", 3, 5), ["-n3", "-c5", "a.md"]);
        assert_eq!(args("kate", "a.md", 3, 5), ["a.md"]);
    }

    #[test]
    fn knows_which_editors_need_a_terminal() {
        for program in ["vim", "nvim", "vi", "nano", "/usr/bin/micro", "hx"] {
            assert!(editor_kind(program).terminal, "{program}");
        }
        for program in ["gvim", "gedit", "code", "emacs", "notepad++", "kate"] {
            assert!(!editor_kind(program).terminal, "{program}");
        }
    }

    #[test]
    fn splits_editor_settings_into_words() {
        assert_eq!(split_words("code --wait"), ["code", "--wait"]);
        assert_eq!(
            split_words(r#""C:\Program Files\Notepad++\notepad++.exe" -multiInst"#),
            [r"C:\Program Files\Notepad++\notepad++.exe", "-multiInst"]
        );
        assert_eq!(split_words("  vim  "), ["vim"]);
        assert_eq!(split_words("'my editor' ''"), ["my editor", ""]);
        assert!(split_words("   ").is_empty());
    }
}
