//! Extra per-process data that sysinfo does not surface: thread counts.
//!
//! A Toolhelp thread snapshot is one syscall for the whole system, which is much
//! cheaper than opening a handle to every process.

use std::collections::HashMap;

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, THREADENTRY32, TH32CS_SNAPTHREAD,
};

/// Number of threads per live PID.
pub fn thread_counts() -> HashMap<u32, u32> {
    let mut counts = HashMap::new();

    let snapshot = match unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) } {
        Ok(snapshot) => snapshot,
        Err(_) => return counts,
    };

    let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;

    let mut has_entry = unsafe { Thread32First(snapshot, &mut entry) }.is_ok();
    while has_entry {
        *counts.entry(entry.th32OwnerProcessID).or_insert(0) += 1;

        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        has_entry = unsafe { Thread32Next(snapshot, &mut entry) }.is_ok();
    }

    unsafe { let _ = CloseHandle(snapshot); };
    counts
}
