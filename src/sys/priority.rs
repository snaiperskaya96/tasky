//! Staying responsive when the rest of the machine is not.
//!
//! Like Windows' own Task Manager, Tasky runs at high priority so it still
//! gets CPU time when a runaway process is saturating every core, which is
//! exactly when you need it. It does very little work, so it costs the rest of
//! the system nothing in normal use.
//!
//! CPU is only half of it. Under memory pressure Windows trims idle programs'
//! working sets, and a trimmed Tasky would have to page itself back in from a
//! disk that is already thrashing. Pinning its working set keeps it resident.

use windows::Win32::System::Memory::{
    SetProcessWorkingSetSizeEx, QUOTA_LIMITS_HARDWS_MAX_DISABLE, QUOTA_LIMITS_HARDWS_MIN_ENABLE,
};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, SetPriorityClass, SetThreadPriority, HIGH_PRIORITY_CLASS,
    THREAD_PRIORITY_LOWEST,
};

/// Never lock more than this, whatever the working set has grown to.
const MAX_PINNED: usize = 256 * 1024 * 1024;

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

/// Make the current working set, plus some headroom, a hard minimum, so
/// Windows does not trim it. Call once the UI has drawn a few frames and the
/// pages it needs are resident.
pub fn pin_working_set() {
    let process = unsafe { GetCurrentProcess() };
    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ..Default::default()
    };
    if unsafe { GetProcessMemoryInfo(process, &mut counters, counters.cb) }.is_err() {
        return;
    }
    let minimum = (counters.WorkingSetSize + counters.WorkingSetSize / 4).min(MAX_PINNED);
    // The maximum stays soft; it only has to be above the minimum.
    let maximum = minimum * 2;
    unsafe {
        let _ = SetProcessWorkingSetSizeEx(
            process,
            minimum,
            maximum,
            QUOTA_LIMITS_HARDWS_MIN_ENABLE | QUOTA_LIMITS_HARDWS_MAX_DISABLE,
        );
    }
}
