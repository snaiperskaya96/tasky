//! Extra per-process data that sysinfo does not surface: thread counts.
//!
//! A Toolhelp *process* snapshot already carries each process's thread count,
//! so there is no need for a thread snapshot (one entry per thread, thousands
//! of them) or for opening a handle to every process.

use std::collections::HashMap;

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

/// Number of threads per live PID.
pub fn thread_counts() -> HashMap<u32, u32> {
    let mut counts = HashMap::new();

    let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return counts;
    };

    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut has_entry = unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok();
    while has_entry {
        counts.insert(entry.th32ProcessID, entry.cntThreads);
        has_entry = unsafe { Process32NextW(snapshot, &mut entry) }.is_ok();
    }

    unsafe {
        let _ = CloseHandle(snapshot);
    }
    counts
}
