//! Text input window of the operating system (Windows), for apps that keep no text input of their own.
//!
//! The window holds the system's multi-line edit control, so typing, IME composition, clipboard and undo are the
//! system's. It runs on its own thread with its own message loop, which also gives `Tab` between controls. The app's
//! thread never waits for that thread: showing a window that has an owner sends messages to the owner's thread, so a
//! wait there would never end. Requests made before the window exists are applied once it does.
//! `Ctrl+Enter` or `Apply` sends the text, and the window stays open for the next edit; `Esc` or `Close` closes it.
//! A status line under the text shows what the app reports back, such as the result of the last text.

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex, Once};

use async_channel::{Receiver, Sender};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    COLOR_BTNFACE, CreateFontIndirectW, DeleteObject, HBRUSH, HFONT, HGDIOBJ,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForWindow, SystemParametersInfoForDpi};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, SetFocus, VK_CONTROL, VK_RETURN};
use windows::Win32::UI::WindowsAndMessaging::{
    BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CreateWindowExW, DefWindowProcW, DestroyWindow,
    DispatchMessageW, ES_AUTOVSCROLL, ES_MULTILINE, ES_WANTRETURN, GWLP_USERDATA, GetClientRect,
    GetMessageW, GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, IDC_ARROW,
    IsDialogMessageW, LoadCursorW, MINMAXINFO, MSG, MoveWindow, NONCLIENTMETRICSW, PostMessageW,
    PostQuitMessage, RegisterClassExW, SPI_GETNONCLIENTMETRICS, SW_SHOWNORMAL, SWP_NOZORDER,
    SendMessageW, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow,
    TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_COMMAND, WM_DESTROY,
    WM_DPICHANGED, WM_GETMINMAXINFO, WM_KEYDOWN, WM_SETFONT, WM_SIZE, WNDCLASSEXW, WS_CHILD,
    WS_EX_CLIENTEDGE, WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};
use windows::core::{HSTRING, PCWSTR, w};

/// What the input window sends to the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputEvent {
    /// The text, with `\n` line ends.
    Submit(String),
    /// The window closed.
    Closed,
    /// The window could not be created.
    Failed(String),
}

/// Changes the app asks for, applied by the window's thread.
#[derive(Default)]
struct Requests {
    text: Option<String>,
    status: Option<String>,
    show: bool,
    close: bool,
}

/// Handle to an open input window. Dropping it closes the window.
pub struct InputWindow {
    /// The window, as an integer so the handle can be shared between threads. 0 until the window exists.
    hwnd: Arc<AtomicIsize>,
    requests: Arc<Mutex<Requests>>,
}

const CLASS_NAME: PCWSTR = w!("ViewerUiInputWindow");
/// Asks the window's thread to apply [`Requests`].
const WM_REQUESTS: u32 = WM_APP + 1;
const ID_APPLY: usize = 1;
const ID_CLOSE: usize = 2;
const EM_SETSEL: u32 = 0x00B1;
/// Size in pixels at 96 DPI.
const WIDTH: i32 = 640;
const HEIGHT: i32 = 300;
const MIN_WIDTH: i32 = 360;
const MIN_HEIGHT: i32 = 200;
const MARGIN: i32 = 10;
const BUTTON_WIDTH: i32 = 88;
const BUTTON_HEIGHT: i32 = 28;
const LINE_HEIGHT: i32 = 20;

impl InputWindow {
    /// Opens the window with `text`, centered over `owner` and kept above it. `hint` is shown above the text.
    /// Returns at once; events, including [`InputEvent::Failed`] when the window cannot be created, arrive on the
    /// returned receiver.
    pub fn open(
        owner: Option<isize>,
        title: &str,
        hint: &str,
        text: &str,
    ) -> std::io::Result<(Self, Receiver<InputEvent>)> {
        let (events, receiver) = async_channel::unbounded();
        let requests = Arc::new(Mutex::new(Requests::default()));
        let handle = Arc::new(AtomicIsize::new(0));
        let (title, hint, text) = (title.to_string(), hint.to_string(), text.to_string());
        let (shared, created) = (requests.clone(), handle.clone());
        std::thread::Builder::new()
            .name("input-window".into())
            .spawn(move || run(owner, title, hint, text, events, shared, created))?;
        Ok((
            Self {
                hwnd: handle,
                requests,
            },
            receiver,
        ))
    }

    fn request(&self, change: impl FnOnce(&mut Requests)) {
        change(&mut self.requests.lock().expect("input window requests"));
        // Before the window exists, its thread applies the request right after creating it.
        let window = self.hwnd.load(Ordering::Acquire);
        if window != 0 {
            // Fails only when the window is already gone, which needs nothing more.
            let _ = unsafe { PostMessageW(Some(hwnd(window)), WM_REQUESTS, WPARAM(0), LPARAM(0)) };
        }
    }

    /// Replaces the text.
    pub fn set_text(&self, text: &str) {
        let text = text.to_string();
        self.request(|requests| requests.text = Some(text));
    }

    /// Shows `status` under the text.
    pub fn set_status(&self, status: &str) {
        let status = status.to_string();
        self.request(|requests| requests.status = Some(status));
    }

    /// Brings the window to the front with the text focused.
    pub fn show(&self) {
        self.request(|requests| requests.show = true);
    }

    pub fn close(&self) {
        self.request(|requests| requests.close = true);
    }
}

impl Drop for InputWindow {
    fn drop(&mut self) {
        self.close();
    }
}

/// The window's native handle, for [`InputWindow::open`]'s owner.
pub fn window_handle(window: &gpui::Window) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
        _ => None,
    }
}

fn hwnd(value: isize) -> HWND {
    HWND(value as *mut _)
}

fn wide(text: &str) -> HSTRING {
    // The edit control breaks lines at `\r\n`.
    HSTRING::from(text.replace("\r\n", "\n").replace('\n', "\r\n"))
}

/// State of one window, owned by its thread and reachable from the window procedure.
struct Window {
    edit: HWND,
    hint: HWND,
    status: HWND,
    apply: HWND,
    close: HWND,
    font: HFONT,
    events: Sender<InputEvent>,
    requests: Arc<Mutex<Requests>>,
}

impl Window {
    fn controls(&self) -> [HWND; 5] {
        [self.edit, self.hint, self.status, self.apply, self.close]
    }

    fn submit(&self) {
        let text = unsafe {
            let len = GetWindowTextLengthW(self.edit).max(0) as usize;
            let mut buffer = vec![0u16; len + 1];
            let copied = GetWindowTextW(self.edit, &mut buffer).max(0) as usize;
            String::from_utf16_lossy(&buffer[..copied])
        };
        let _ = self
            .events
            .try_send(InputEvent::Submit(text.replace("\r\n", "\n")));
    }

    /// Sets the message font for the window's DPI on every control.
    fn set_font(&mut self, window: HWND) {
        unsafe {
            let dpi = GetDpiForWindow(window);
            let mut metrics = NONCLIENTMETRICSW {
                cbSize: size_of::<NONCLIENTMETRICSW>() as u32,
                ..Default::default()
            };
            if SystemParametersInfoForDpi(
                SPI_GETNONCLIENTMETRICS.0,
                metrics.cbSize,
                Some(&mut metrics as *mut _ as *mut _),
                0,
                dpi,
            )
            .is_err()
            {
                return;
            }
            let font = CreateFontIndirectW(&metrics.lfMessageFont);
            for control in self.controls() {
                SendMessageW(
                    control,
                    WM_SETFONT,
                    Some(WPARAM(font.0 as usize)),
                    Some(LPARAM(1)),
                );
            }
            if !self.font.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(self.font.0));
            }
            self.font = font;
        }
    }

    fn layout(&self, window: HWND) {
        unsafe {
            let scale = |value: i32| value * GetDpiForWindow(window) as i32 / 96;
            let mut client = RECT::default();
            if GetClientRect(window, &mut client).is_err() {
                return;
            }
            let (width, height) = (client.right, client.bottom);
            let (margin, line) = (scale(MARGIN), scale(LINE_HEIGHT));
            let (button_width, button_height) = (scale(BUTTON_WIDTH), scale(BUTTON_HEIGHT));
            let bottom = height - margin - button_height;
            let _ = MoveWindow(self.hint, margin, margin, width - margin * 2, line, true);
            let edit_top = margin + line + margin / 2;
            let _ = MoveWindow(
                self.edit,
                margin,
                edit_top,
                width - margin * 2,
                (bottom - margin - edit_top).max(line),
                true,
            );
            // The status takes the height of the buttons, so a long message wraps onto a second line.
            let status_width = (width - margin * 4 - button_width * 2).max(0);
            let _ = MoveWindow(
                self.status,
                margin,
                bottom,
                status_width,
                button_height,
                true,
            );
            let _ = MoveWindow(
                self.apply,
                width - margin * 2 - button_width * 2,
                bottom,
                button_width,
                button_height,
                true,
            );
            let _ = MoveWindow(
                self.close,
                width - margin - button_width,
                bottom,
                button_width,
                button_height,
                true,
            );
        }
    }

    fn apply_requests(&self, window: HWND) {
        let requests = std::mem::take(&mut *self.requests.lock().expect("input window requests"));
        unsafe {
            if let Some(text) = requests.text {
                let _ = SetWindowTextW(self.edit, &wide(&text));
                // Put the cursor at the end.
                SendMessageW(
                    self.edit,
                    EM_SETSEL,
                    Some(WPARAM(usize::MAX)),
                    Some(LPARAM(-1)),
                );
            }
            if let Some(status) = requests.status {
                let _ = SetWindowTextW(self.status, &HSTRING::from(status));
            }
            if requests.show {
                let _ = ShowWindow(window, SW_SHOWNORMAL);
                let _ = SetForegroundWindow(window);
                let _ = SetFocus(Some(self.edit));
            }
            if requests.close {
                let _ = DestroyWindow(window);
            }
        }
    }
}

fn state(window: HWND) -> Option<&'static mut Window> {
    let pointer = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *mut Window;
    unsafe { pointer.as_mut() }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let Some(state) = state(window) else {
        return unsafe { DefWindowProcW(window, message, wparam, lparam) };
    };
    match message {
        WM_COMMAND => match wparam.0 & 0xffff {
            ID_APPLY => state.submit(),
            ID_CLOSE => {
                let _ = unsafe { DestroyWindow(window) };
            }
            _ => {}
        },
        WM_REQUESTS => state.apply_requests(window),
        WM_SIZE => state.layout(window),
        WM_DPICHANGED => {
            // Move to the size Windows suggests for the new DPI, then refit the font and controls.
            let rect = unsafe { &*(lparam.0 as *const RECT) };
            let _ = unsafe {
                SetWindowPos(
                    window,
                    None,
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_NOZORDER,
                )
            };
            state.set_font(window);
            state.layout(window);
        }
        WM_GETMINMAXINFO => {
            let scale = |value: i32| value * unsafe { GetDpiForWindow(window) } as i32 / 96;
            let info = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
            info.ptMinTrackSize.x = scale(MIN_WIDTH);
            info.ptMinTrackSize.y = scale(MIN_HEIGHT);
        }
        WM_CLOSE => {
            let _ = unsafe { DestroyWindow(window) };
        }
        WM_DESTROY => {
            let _ = state.events.try_send(InputEvent::Closed);
            unsafe { PostQuitMessage(0) };
        }
        _ => return unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
    LRESULT(0)
}

fn register_class() {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH((COLOR_BTNFACE.0 + 1) as usize as *mut _),
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        RegisterClassExW(&class);
    });
}

fn child(
    parent: HWND,
    class: PCWSTR,
    text: &str,
    style: u32,
    ex_style: WINDOW_EX_STYLE,
    id: usize,
) -> windows::core::Result<HWND> {
    unsafe {
        CreateWindowExW(
            ex_style,
            class,
            &HSTRING::from(text),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | style),
            0,
            0,
            0,
            0,
            Some(parent),
            Some(windows::Win32::UI::WindowsAndMessaging::HMENU(id as *mut _)),
            None,
            None,
        )
    }
}

/// Creates the window and runs its message loop until it closes.
fn run(
    owner: Option<isize>,
    title: String,
    hint: String,
    text: String,
    events: Sender<InputEvent>,
    requests: Arc<Mutex<Requests>>,
    created: Arc<AtomicIsize>,
) {
    register_class();
    let owner = owner.map(hwnd);
    let window = match unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS_NAME,
            &HSTRING::from(title),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            WIDTH,
            HEIGHT,
            owner,
            None,
            None,
            None,
        )
    } {
        Ok(window) => window,
        Err(error) => {
            let _ = events.try_send(InputEvent::Failed(error.to_string()));
            return;
        }
    };
    let failed = events.clone();
    let make = || -> windows::core::Result<Window> {
        Ok(Window {
            edit: child(
                window,
                w!("EDIT"),
                "",
                WS_TABSTOP.0
                    | WS_VSCROLL.0
                    | (ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN) as u32,
                WS_EX_CLIENTEDGE,
                0,
            )?,
            hint: child(window, w!("STATIC"), &hint, 0, WINDOW_EX_STYLE(0), 0)?,
            status: child(window, w!("STATIC"), "", 0, WINDOW_EX_STYLE(0), 0)?,
            apply: child(
                window,
                w!("BUTTON"),
                "Apply",
                WS_TABSTOP.0 | BS_DEFPUSHBUTTON as u32,
                WINDOW_EX_STYLE(0),
                ID_APPLY,
            )?,
            close: child(
                window,
                w!("BUTTON"),
                "Close",
                WS_TABSTOP.0 | BS_PUSHBUTTON as u32,
                WINDOW_EX_STYLE(0),
                ID_CLOSE,
            )?,
            font: HFONT::default(),
            events,
            requests,
        })
    };
    let state = match make() {
        Ok(state) => Box::into_raw(Box::new(state)),
        Err(error) => {
            let _ = unsafe { DestroyWindow(window) };
            let _ = failed.try_send(InputEvent::Failed(error.to_string()));
            return;
        }
    };
    unsafe {
        SetWindowLongPtrW(window, GWLP_USERDATA, state as isize);
        let state = &mut *state;
        state.set_font(window);
        // Size for the DPI, centered over the owner.
        let dpi = GetDpiForWindow(window) as i32;
        let (width, height) = (WIDTH * dpi / 96, HEIGHT * dpi / 96);
        let mut area = RECT::default();
        let (x, y) = match owner {
            Some(owner) if GetWindowRect(owner, &mut area).is_ok() => (
                area.left + (area.right - area.left - width) / 2,
                area.top + (area.bottom - area.top - height) / 2,
            ),
            _ => (100, 100),
        };
        let _ = SetWindowPos(window, None, x, y, width, height, SWP_NOZORDER);
        state.layout(window);
        let _ = SetWindowTextW(state.edit, &wide(&text));
        SendMessageW(
            state.edit,
            EM_SETSEL,
            Some(WPARAM(usize::MAX)),
            Some(LPARAM(-1)),
        );
        let _ = ShowWindow(window, SW_SHOWNORMAL);
        let _ = SetForegroundWindow(window);
        let _ = SetFocus(Some(state.edit));
        // From here requests are posted to the window; apply those that came before.
        created.store(window.0 as isize, Ordering::Release);
        state.apply_requests(window);
    }

    let mut message = MSG::default();
    unsafe {
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
            if message.message == WM_KEYDOWN && message.wParam.0 == VK_RETURN.0 as usize && ctrl {
                if let Some(state) = self::state(window) {
                    state.submit();
                }
                continue;
            }
            if IsDialogMessageW(window, &message).as_bool() {
                continue;
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        let state = Box::from_raw(state);
        SetWindowLongPtrW(window, GWLP_USERDATA, 0);
        if !state.font.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(state.font.0));
        }
    }
}
