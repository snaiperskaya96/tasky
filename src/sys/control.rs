//! Process control: end task, suspend and resume.
//!
//! `TerminateProcess` is a documented Win32 API, but suspending and resuming a
//! whole process is only exposed by ntdll (`NtSuspendProcess` /
//! `NtResumeProcess`), so those two are declared by hand. They have been stable
//! since Windows XP and are what Task Manager itself uses.

use std::collections::HashSet;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Threading::{
    OpenProcess, TerminateProcess, PROCESS_ACCESS_RIGHTS, PROCESS_SUSPEND_RESUME, PROCESS_TERMINATE,
};

#[link(name = "ntdll")]
extern "system" {
    fn NtSuspendProcess(process: HANDLE) -> i32;
    fn NtResumeProcess(process: HANDLE) -> i32;
}

/// Error carrying enough context for the UI to explain what happened.
#[derive(Debug, Clone)]
pub struct ControlError {
    pub pid: u32,
    pub message: String,
}

type Result<T> = std::result::Result<T, ControlError>;

fn open(pid: u32, access: PROCESS_ACCESS_RIGHTS) -> Result<HANDLE> {
    match unsafe { OpenProcess(access, false, pid) } {
        Ok(handle) if !handle.is_invalid() => Ok(handle),
        Ok(_) => Err(ControlError {
            pid,
            message: "access denied".to_string(),
        }),
        Err(e) => Err(ControlError {
            pid,
            message: e.to_string(),
        }),
    }
}

/// Terminate one or more processes. Reports every failure but keeps going, so
/// that ending a whole selection still closes everything we can close.
pub fn kill(pids: &[u32]) -> Vec<ControlError> {
    let mut errors = Vec::new();
    for &pid in pids {
        if let Err(e) = kill_one(pid) {
            errors.push(e);
        }
    }
    errors
}

fn kill_one(pid: u32) -> Result<()> {
    let handle = open(pid, PROCESS_TERMINATE)?;
    let result = unsafe { TerminateProcess(handle, 1) };
    unsafe {
        let _ = CloseHandle(handle);
    }
    result.map_err(|e| ControlError {
        pid,
        message: e.to_string(),
    })
}

/// Freeze every thread of the process.
pub fn suspend(pid: u32) -> Result<()> {
    let handle = open(pid, PROCESS_SUSPEND_RESUME)?;
    let status = unsafe { NtSuspendProcess(handle) };
    unsafe {
        let _ = CloseHandle(handle);
    }
    ntok(status, pid)
}

/// Unfreeze a process we (or something else) suspended.
pub fn resume(pid: u32) -> Result<()> {
    let handle = open(pid, PROCESS_SUSPEND_RESUME)?;
    let status = unsafe { NtResumeProcess(handle) };
    unsafe {
        let _ = CloseHandle(handle);
    }
    ntok(status, pid)
}

/// Open the folder containing an executable and select the file, the way
/// "Open file location" does in Task Manager.
pub fn reveal_in_explorer(path: &str) -> bool {
    use std::os::windows::process::CommandExt as _;

    if path.is_empty() {
        return false;
    }
    std::process::Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{path}\""))
        .spawn()
        .is_ok()
}

fn ntok(status: i32, pid: u32) -> Result<()> {
    // STATUS_SUCCESS == 0; anything else is an NTSTATUS failure code.
    if status != 0 {
        return Err(ControlError {
            pid,
            message: format!("NTSTATUS 0x{status:08X}"),
        });
    }
    Ok(())
}

/// Resume every process we suspended. Called on the way out, so nothing stays
/// frozen after Tasky is gone.
pub fn resume_all(suspended: &mut HashSet<u32>) {
    for pid in suspended.drain() {
        let _ = resume(pid);
    }
}
