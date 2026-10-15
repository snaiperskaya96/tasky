//! A global hotkey so Tasky can be summoned from any application.
//!
//! Rather than relying on a third-party package, this uses a low-level
//! `WH_KEYBOARD_LL` hook running on its own message loop, which is what keeps
//! working even when Tasky's window is hidden into the tray.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;

use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, SetWindowsHookExW, TranslateMessage,
    UnhookWindowsHookEx, KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
};

/// Default combination.
pub const DEFAULT_COMBO: &str = "Ctrl+Alt+T";

static PRESSED: AtomicBool = AtomicBool::new(false);
static HOOK: AtomicUsize = AtomicUsize::new(0);

pub struct Hotkey {
    pub combo: String,
}

impl Hotkey {
    pub fn bind() -> Option<Self> {
        thread::spawn(|| unsafe {
            let module = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)
                .ok()
                .map(|h| HINSTANCE(h.0));
            match SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), module, 0) {
                Ok(hook) => {
                    HOOK.store(hook.0 as usize, Ordering::SeqCst);
                    let mut msg = MSG::default();
                    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                    let _ = UnhookWindowsHookEx(hook);
                }
                Err(_) => {}
            }
        });

        Some(Self {
            combo: DEFAULT_COMBO.to_string(),
        })
    }

    /// Whether the hotkey has been pressed since the last poll.
    pub fn take_press() -> bool {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(
            r"C:\Users\jury\AppData\Local\Temp\opencode\tasky_debug.log",
        ) {
            use std::io::Write;
            let _ = writeln!(f, "take_press PRESSED={}", PRESSED.load(Ordering::Relaxed));
        }
        PRESSED.swap(false, Ordering::AcqRel)
    }
}

unsafe extern "system" fn hook_proc(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if code >= 0 && (wparam.0 as u32 == WM_KEYDOWN || wparam.0 as u32 == WM_SYSKEYDOWN)
    {
        let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        // Log a few key presses so we can see whether the hook fires at all.
        if kb.vkCode == 0x54 || kb.vkCode == 0x11 || kb.vkCode == 0x12 {
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(
                r"C:\Users\jury\AppData\Local\Temp\opencode\tasky_debug.log",
            ) {
                use std::io::Write;
                let _ = writeln!(f, "hook vk={} wparam={}", kb.vkCode, wparam.0);
            }
        }
        let ctrl = (GetAsyncKeyState(0x11) as u16 & 0x8000) != 0;
        let alt = (GetAsyncKeyState(0x12) as u16 & 0x8000) != 0;
        if kb.vkCode == 0x54 && ctrl && alt {
            PRESSED.store(true, Ordering::Release);
            crate::sys::instance::show_stored_main_window();
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(
                r"C:\Users\jury\AppData\Local\Temp\opencode\tasky_debug.log",
            ) {
                use std::io::Write;
                let _ = writeln!(f, "HOTKEY TRIGGERED");
            }
        }
    }
    let hook = HOOK.load(Ordering::SeqCst);
    if hook != 0 {
        use windows::Win32::UI::WindowsAndMessaging::HHOOK;
        return CallNextHookEx(Some(HHOOK(hook as *mut _)), code, wparam, lparam);
    }
    CallNextHookEx(None, code, wparam, lparam)
}
