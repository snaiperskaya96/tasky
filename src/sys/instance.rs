//! Single instance, plus raising the main window from any thread.
//!
//! The first instance holds a named mutex for its whole lifetime. A dedicated
//! host thread owns a hidden window of a known class and the global hotkey,
//! and runs its own message loop. A second launch finds that window and posts
//! it a message. Because none of this runs on the UI thread, Tasky can still be
//! summoned when the UI is idle behind a hidden window, and when the UI thread
//! has stopped responding (then the emergency window opens instead).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, OnceLock};
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, E_ACCESSDENIED, HANDLE, HINSTANCE, HWND,
    LPARAM, LRESULT, WPARAM,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, ChangeWindowMessageFilterEx, CreateWindowExW, DefWindowProcW,
    DispatchMessageW, FindWindowW, GetMessageW, IsIconic, PostMessageW, RegisterClassW,
    SendMessageTimeoutW, SetForegroundWindow, SetWindowPos, ShowWindow, TranslateMessage, ASFW_ANY,
    HWND_NOTOPMOST, HWND_TOPMOST, MSG, MSGFLT_ALLOW, SMTO_ABORTIFHUNG, SMTO_BLOCK, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SW_RESTORE, SW_SHOW, WINDOW_EX_STYLE, WM_APP, WM_HOTKEY, WM_NULL,
    WNDCLASSW, WS_OVERLAPPED,
};

use crate::sys::hotkey;

const HOST_CLASS: PCWSTR = w!("TaskyHostWindow");
const MUTEX_NAME: PCWSTR = w!("Local\\TaskySingleInstance");
const WM_TASKY_ACTIVATE: u32 = WM_APP + 1;
/// How long the main window gets to answer before it counts as hung.
const RESPONSE_TIMEOUT_MS: u32 = 1000;

static MAIN_WINDOW: AtomicUsize = AtomicUsize::new(0);
static ALWAYS_ON_TOP: AtomicBool = AtomicBool::new(false);
static SHOW_REQUESTED: AtomicBool = AtomicBool::new(false);
static ON_SHOW: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();
static HOTKEY: OnceLock<Option<&'static str>> = OnceLock::new();

/// Proof that this is the only running instance. Keep it alive for as long as
/// the app runs: dropping it releases the mutex.
pub struct Instance {
    mutex: Option<HANDLE>,
}

impl Drop for Instance {
    fn drop(&mut self) {
        if let Some(mutex) = self.mutex {
            unsafe {
                let _ = CloseHandle(mutex);
            }
        }
    }
}

/// Acquire the instance mutex and start the host thread, or return `None` if
/// another instance already holds the mutex.
pub fn claim() -> Option<Instance> {
    let mutex = match unsafe { CreateMutexW(None, false, MUTEX_NAME) } {
        Ok(handle) => {
            if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
                unsafe {
                    let _ = CloseHandle(handle);
                }
                return None;
            }
            Some(handle)
        }
        // An elevated instance's mutex cannot be opened from an unelevated
        // launch, which is just as much proof that one is running.
        Err(error) if error.code() == E_ACCESSDENIED => return None,
        // Better a second copy than none at all.
        Err(_) => None,
    };

    let (ready_tx, ready_rx) = mpsc::channel();
    let started = std::thread::Builder::new()
        .name("tasky-host".into())
        .spawn(move || host_thread(ready_tx))
        .is_ok();
    if started {
        // Wait until the window can be found and the hotkey is settled.
        let _ = ready_rx.recv();
    }

    Some(Instance { mutex })
}

fn host_thread(ready: mpsc::Sender<()>) {
    let hinstance = unsafe { GetModuleHandleW(None) }
        .map(|module| HINSTANCE(module.0))
        .unwrap_or_default();
    let class = WNDCLASSW {
        lpfnWndProc: Some(host_wndproc),
        hInstance: hinstance,
        lpszClassName: HOST_CLASS,
        ..Default::default()
    };
    unsafe {
        RegisterClassW(&class);
        // Never shown; it only exists so a second launch can find us.
        if let Ok(host) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            HOST_CLASS,
            PCWSTR::null(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance),
            None,
        ) {
            // Let a non-elevated launch reach an elevated instance.
            let _ = ChangeWindowMessageFilterEx(host, WM_TASKY_ACTIVATE, MSGFLT_ALLOW, None);
        }
    }
    let _ = HOTKEY.set(hotkey::register());
    let _ = ready.send(());

    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        // The hotkey arrives as a thread message, with no window attached.
        if msg.message == WM_HOTKEY {
            summon();
            continue;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// The label of the registered hotkey, if one could be registered.
pub fn hotkey_label() -> Option<&'static str> {
    HOTKEY.get().copied().flatten()
}

/// Whether another Tasky is running in this session.
pub fn is_running() -> bool {
    unsafe { FindWindowW(HOST_CLASS, PCWSTR::null()) }.is_ok()
}

/// Wait up to `timeout` for a starting instance to come up.
pub fn wait_until_running(timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if is_running() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Ask the already running instance to show itself.
pub fn activate_existing() {
    unsafe {
        if let Ok(host) = FindWindowW(HOST_CLASS, PCWSTR::null()) {
            // We were just launched by the user, so we may hand the
            // foreground over to the running instance.
            let _ = AllowSetForegroundWindow(ASFW_ANY);
            let _ = PostMessageW(Some(host), WM_TASKY_ACTIVATE, WPARAM(0), LPARAM(0));
        }
    }
}

/// Remember the main window so other threads (hotkey, tray, host window) can
/// raise it.
pub fn set_main_window(hwnd: HWND) {
    MAIN_WINDOW.store(hwnd.0 as usize, Ordering::SeqCst);
}

/// Run `callback` whenever the window is raised from outside eframe, so the
/// UI can wake up and catch up with what happened.
pub fn on_show(callback: impl Fn() + Send + Sync + 'static) {
    let _ = ON_SHOW.set(Box::new(callback));
}

fn main_window() -> Option<HWND> {
    match MAIN_WINDOW.load(Ordering::SeqCst) {
        0 => None,
        raw => Some(HWND(raw as *mut _)),
    }
}

/// Whether the window's thread answers a no-op message in time.
fn is_responsive(hwnd: HWND) -> bool {
    let mut result = 0usize;
    unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_NULL,
            WPARAM(0),
            LPARAM(0),
            SMTO_ABORTIFHUNG | SMTO_BLOCK,
            RESPONSE_TIMEOUT_MS,
            Some(&mut result),
        )
    }
    .0 != 0
}

/// What the hotkey and a second launch do: raise the main window, or open the
/// emergency window when there is no main window or it is not responding
/// (a hung graphics driver, say).
pub fn summon() {
    match main_window() {
        Some(hwnd) if is_responsive(hwnd) => show_main_window(),
        _ => crate::emergency::open(),
    }
}

/// Show, restore and focus the main window. Called from the host thread after
/// checking the window responds, and from the UI thread itself (tray).
pub fn show_main_window() {
    SHOW_REQUESTED.store(true, Ordering::SeqCst);
    if let Some(hwnd) = main_window() {
        unsafe {
            let command = if IsIconic(hwnd).as_bool() {
                SW_RESTORE
            } else {
                SW_SHOW
            };
            let _ = ShowWindow(hwnd, command);
            let _ = SetForegroundWindow(hwnd);
        }
    }
    if let Some(callback) = ON_SHOW.get() {
        callback();
    }
}

/// True if the window was raised from outside eframe since the last poll.
pub fn take_show_request() -> bool {
    SHOW_REQUESTED.swap(false, Ordering::SeqCst)
}

pub fn is_always_on_top() -> bool {
    ALWAYS_ON_TOP.load(Ordering::SeqCst)
}

/// Set the window's topmost state natively. eframe is not told, so it never
/// overrides the setting behind our back.
pub fn set_always_on_top(on: bool) {
    ALWAYS_ON_TOP.store(on, Ordering::SeqCst);
    let Some(hwnd) = main_window() else {
        return;
    };
    let above = if on { HWND_TOPMOST } else { HWND_NOTOPMOST };
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(above),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

unsafe extern "system" fn host_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_TASKY_ACTIVATE {
        summon();
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}
