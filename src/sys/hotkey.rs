//! A global hotkey so Tasky can be summoned from any application.
//!
//! This uses `RegisterHotKey` on a dedicated thread rather than a low-level
//! keyboard hook. Windows posts `WM_HOTKEY` straight to that thread, so Tasky
//! is not called on every keystroke in the system (a hook is, and a slow hook
//! lags everyone's typing before Windows silently removes it). It keeps
//! working while the window is hidden and eframe's loop is idle.

use std::sync::mpsc;
use std::thread;

use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, WM_HOTKEY};

const HOTKEY_ID: i32 = 1;
const VK_T: u32 = b'T' as u32;

/// Tried in order; the first one no other application holds wins.
const COMBOS: [(HOT_KEY_MODIFIERS, u32, &str); 2] = [
    (
        HOT_KEY_MODIFIERS(MOD_CONTROL.0 | MOD_ALT.0),
        VK_T,
        "Ctrl+Alt+T",
    ),
    (
        HOT_KEY_MODIFIERS(MOD_CONTROL.0 | MOD_ALT.0 | MOD_SHIFT.0),
        VK_T,
        "Ctrl+Alt+Shift+T",
    ),
];

/// Register the first free combination and return its label.
pub fn bind() -> Option<&'static str> {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("tasky-hotkey".into())
        .spawn(move || {
            // The hotkey belongs to the thread that registers it, so this
            // thread both registers it and runs the message loop for it.
            let bound = COMBOS.iter().find(|(modifiers, key, _)| unsafe {
                RegisterHotKey(None, HOTKEY_ID, *modifiers | MOD_NOREPEAT, *key).is_ok()
            });
            let _ = tx.send(bound.map(|(_, _, label)| *label));
            if bound.is_none() {
                return;
            }

            let mut msg = MSG::default();
            while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
                if msg.message == WM_HOTKEY {
                    crate::sys::instance::show_main_window();
                }
            }
        })
        .ok()?;
    rx.recv().ok().flatten()
}
