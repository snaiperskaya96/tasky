//! Single instance, plus raising the main window.
//!
//! A second launch activates the running instance and exits rather than spawning
//! a duplicate. The first instance owns a named mutex, and every instance also
//! owns a hidden host window. A second launch broadcasts a registered message
//! to all top level windows; the running instance picks it up on its next frame.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::{AttachThreadInput, CreateMutexW, GetCurrentThreadId};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, EnumWindows, GetForegroundWindow, PostMessageW,
    RegisterWindowMessageW, SetForegroundWindow, ShowWindow, GW_OWNER, WNDCLASSW, HWND_BROADCAST,
    SW_RESTORE, SW_SHOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_OVERLAPPEDWINDOW, WINDOW_STYLE,
    SetWindowPos,
};

const HOST_CLASS: windows::core::PCWSTR = windows::core::w!("TaskyHostWindow");
const MUTEX_NAME: windows::core::PCWSTR = windows::core::w!("Local\\TaskySingleInstance");
const ASFW_ANY: u32 = u32::MAX;

static ACTIVATED: AtomicBool = AtomicBool::new(false);
static ACTIVATE_MESSAGE: AtomicU32 = AtomicU32::new(0);
static MAIN_WINDOW: AtomicUsize = AtomicUsize::new(0);
pub static ALWAYS_ON_TOP: AtomicBool = AtomicBool::new(false);
pub static RESTORE_REQUESTED: AtomicBool = AtomicBool::new(false);
static MUTEX_HELD: AtomicBool = AtomicBool::new(false);

pub struct HostWindow {
    #[allow(dead_code)]
    hwnd: HWND,
}

/// Create the hidden host window and acquire the instance mutex.
pub fn claim() -> Option<HostWindow> {
    if !acquire_instance_mutex() {
        return None;
    }

    let message = unsafe { RegisterWindowMessageW(windows::core::w!("TaskyActivateWindow")) };
    ACTIVATE_MESSAGE.store(message, Ordering::SeqCst);

    let module = unsafe { windows::Win32::System::LibraryLoader::GetModuleHandleW(None) }
        .ok()
        .map(|handle| handle.0)
        .unwrap_or(std::ptr::null_mut());
    let hinstance = windows::Win32::Foundation::HINSTANCE(module);

    let class = WNDCLASSW {
        lpfnWndProc: Some(host_wndproc),
        hInstance: hinstance,
        lpszClassName: HOST_CLASS,
        ..Default::default()
    };
    unsafe { windows::Win32::UI::WindowsAndMessaging::RegisterClassW(&class) };

    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
            HOST_CLASS,
            windows::core::PCWSTR::null(),
            WINDOW_STYLE(WS_OVERLAPPEDWINDOW.0),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance),
            None,
        )
    };

    match hwnd {
        Ok(hwnd) => Some(HostWindow { hwnd }),
        Err(_) => Some(HostWindow {
            hwnd: HWND::default(),
        }),
    }
}

/// Ask an already running instance (if any) to show itself.
pub fn activate_existing() {
    let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(ASFW_ANY) };
    let message = ACTIVATE_MESSAGE.load(Ordering::SeqCst);
    if message != 0 {
    unsafe { let _ = PostMessageW(Some(HWND_BROADCAST), message, WPARAM(0), windows::Win32::Foundation::LPARAM(0)); };
    }
    // Give the running instance a moment to pick up the message.
    std::thread::sleep(std::time::Duration::from_millis(250));
}

/// True if the running instance was asked to show itself since the last poll.
pub fn take_activation_request() -> bool {
    ACTIVATED.swap(false, Ordering::SeqCst)
}

/// Find this thread's top level window (the egui window) and raise it.
pub fn raise_main_window() {
    unsafe {
        let Some(hwnd) = find_own_window() else {
            return;
        };
        let _ = ShowWindow(hwnd, SW_RESTORE);
        let _ = ShowWindow(hwnd, SW_SHOW);

        let foreground = GetForegroundWindow();
        let foreground_thread = if foreground.0 != std::ptr::null_mut() {
            windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(foreground, None)
        } else {
            0
        };
        let this_thread = GetCurrentThreadId();
        if foreground_thread != 0 && foreground_thread != this_thread {
            let _ = AttachThreadInput(foreground_thread, this_thread, true);
            let _ = SetForegroundWindow(hwnd);
        RESTORE_REQUESTED.store(true, Ordering::SeqCst);
            let _ = AttachThreadInput(foreground_thread, this_thread, false);
        } else {
            let _ = SetForegroundWindow(hwnd);
        RESTORE_REQUESTED.store(true, Ordering::SeqCst);
        }
    }
}

/// Remember the main process window so background threads (like the hotkey
/// hook) can show it.
pub fn remember_main_window() {
    if MAIN_WINDOW.load(Ordering::Relaxed) != 0 {
        return;
    }
    if let Some(hwnd) = find_own_window() {
        MAIN_WINDOW.store(hwnd.0 as usize, Ordering::SeqCst);
    }
}
pub fn store_main_window(hwnd: HWND) {
    if MAIN_WINDOW.load(Ordering::Relaxed) == 0 {
        MAIN_WINDOW.store(hwnd.0 as usize, Ordering::SeqCst);
    }
}



/// Show the remembered window from any thread.
pub fn show_stored_main_window() {
    remember_main_window();
    let raw = MAIN_WINDOW.load(Ordering::SeqCst);
    if raw == 0 {
        return;
    }
    unsafe {
        let hwnd = HWND(raw as *mut _);
        let _ = ShowWindow(hwnd, SW_RESTORE);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        RESTORE_REQUESTED.store(true, Ordering::SeqCst);

        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(
            r"C:\Users\jury\AppData\Local\Temp\opencode\tasky_debug.log",
        ) {
            use std::io::Write;
            let _ = writeln!(f, "show_stored_main_window raw={}", raw);
        }
    }
}

pub fn is_always_on_top() -> bool {
    ALWAYS_ON_TOP.load(Ordering::SeqCst)
}

pub fn apply_always_on_top(on: bool) {
    ALWAYS_ON_TOP.store(on, Ordering::SeqCst);
    let raw = MAIN_WINDOW.load(Ordering::SeqCst);
    if raw == 0 {
        return;
    }
    unsafe {
        let hwnd = HWND(raw as *mut _);
        let above = if on {
            windows::Win32::UI::WindowsAndMessaging::HWND_TOPMOST
        } else {
            windows::Win32::UI::WindowsAndMessaging::HWND_NOTOPMOST
        };
        let _ = SetWindowPos(
            hwnd,
            Some(above),
            0,
            0,
            0,
            0,
            windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE
                | windows::Win32::UI::WindowsAndMessaging::SWP_NOSIZE
                | windows::Win32::UI::WindowsAndMessaging::SWP_NOACTIVATE,
        );
    }
}



pub fn take_restore_request() -> bool {
    RESTORE_REQUESTED.swap(false, Ordering::SeqCst)
}

fn acquire_instance_mutex() -> bool {
    let already_held = MUTEX_HELD.swap(true, Ordering::SeqCst);
    if already_held {
        return true;
    }

    let ok = match unsafe { CreateMutexW(None, true, MUTEX_NAME) } {
        Ok(handle) if !handle.is_invalid() => {
            let already_running = unsafe { windows::Win32::Foundation::GetLastError() }
                == windows::Win32::Foundation::ERROR_ALREADY_EXISTS;
            unsafe { let _ = CloseHandle(handle); };
            !already_running
        }
        _ => true,
    };
    if !ok {
        MUTEX_HELD.store(false, Ordering::SeqCst);
    }
    ok
    // `LibraryLoader` lives behind a feature gate; make sure it is enabled by
    // the toolchain crate.
}

fn find_own_window() -> Option<HWND> {
    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
        let search = &mut *(lparam.0 as *mut Search);
        let worker = windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(hwnd, None);
        if worker == search.thread_id
            && windows::Win32::UI::WindowsAndMessaging::GetWindow(hwnd, GW_OWNER)
                .map(|h| h.0)
                .unwrap_or_default()
                .is_null()
        {
            search.found = Some(hwnd);
            return windows::core::BOOL(0);
        }
        windows::core::BOOL(1)
    }

    #[derive(Default)]
    struct Search {
        thread_id: u32,
        found: Option<HWND>,
    }

    let mut search = Search {
        thread_id: unsafe { GetCurrentThreadId() },
        found: None,
    };
    let lparam = LPARAM(&mut search as *mut Search as isize);
    let _ = unsafe { EnumWindows(Some(visit), lparam) };
    search.found
}

unsafe extern "system" fn host_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == ACTIVATE_MESSAGE.load(Ordering::SeqCst) {
        ACTIVATED.store(true, Ordering::SeqCst);
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}
