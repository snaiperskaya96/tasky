//! A global hotkey so Tasky can be summoned from any application.
//!
//! This uses `RegisterHotKey` rather than a low-level keyboard hook. Windows
//! posts `WM_HOTKEY` straight to the registering thread, so Tasky is not called
//! on every keystroke in the system (a hook is, and a slow hook lags everyone's
//! typing before Windows silently removes it). The host thread in
//! `instance.rs` registers it and handles the message, so the hotkey keeps
//! working even if the UI thread is stuck.

use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT,
};

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

/// Register the first free combination for the calling thread, which must
/// run a message loop, and return its label.
pub fn register() -> Option<&'static str> {
    COMBOS
        .iter()
        .find(|(modifiers, key, _)| unsafe {
            RegisterHotKey(None, HOTKEY_ID, *modifiers | MOD_NOREPEAT, *key).is_ok()
        })
        .map(|(_, _, label)| *label)
}
