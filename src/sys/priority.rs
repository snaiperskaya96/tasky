//! Scheduling priority.
//!
//! Like Windows' own Task Manager, Tasky runs at high priority so it still
//! gets CPU time when a runaway process is saturating every core, which is
//! exactly when you need it. It does very little work, so it costs the rest of
//! the system nothing in normal use.

use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, SetPriorityClass, SetThreadPriority, HIGH_PRIORITY_CLASS,
    THREAD_PRIORITY_LOWEST,
};

/// Raise the whole process to the high priority class.
pub fn raise_process() {
    unsafe {
        let _ = SetPriorityClass(GetCurrentProcess(), HIGH_PRIORITY_CLASS);
    }
}

/// Run the calling thread at the bottom of the class: still ahead of normal
/// applications, but behind Tasky's own UI thread.
pub fn lower_current_thread() {
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_LOWEST);
    }
}
