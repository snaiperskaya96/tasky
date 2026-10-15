//! Process control: end task, suspend and resume.
//!
//! `TerminateProcess` is a documented Win32 API, but suspending and resuming a
//! whole process is only exposed by ntdll (`NtSuspendProcess` /
//! `NtResumeProcess`), so those two are declared by hand. They have been stable
//! since Windows XP and are what Task Manager itself uses.

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Threading::{
    OpenProcess, TerminateProcess, PROCESS_ACCESS_RIGHTS, PROCESS_QUERY_INFORMATION,
    PROCESS_TERMINATE, PROCESS_SUSPEND_RESUME,
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
    #[allow(dead_code)]
    pub action: &'static str,
    pub message: String,
}

type Result<T> = std::result::Result<T, ControlError>;

fn open(pid: u32, access: PROCESS_ACCESS_RIGHTS, action: &'static str) -> Result<HANDLE> {
    match unsafe { OpenProcess(access, false, pid) } {
        Ok(handle) if !handle.is_invalid() => Ok(handle),
        Ok(_) => Err(ControlError {
            pid,
            action,
            message: "access denied".to_string(),
        }),
        Err(e) => Err(ControlError {
            pid,
            action,
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

pub fn kill_one(pid: u32) -> Result<()> {
    let handle = open(pid, PROCESS_TERMINATE, "terminate")?;
    let result = unsafe { TerminateProcess(handle, 1) };
    unsafe { let _ = CloseHandle(handle); };
    result.map_err(|e| ControlError {
        pid,
        action: "terminate",
        message: e.to_string(),
    })
}

/// Freeze every thread of the process.
pub fn suspend(pid: u32) -> Result<()> {
    let handle = open(pid, PROCESS_SUSPEND_RESUME, "suspend")?;
    let status = unsafe { NtSuspendProcess(handle) };
    unsafe { let _ = CloseHandle(handle); };
    ntok(status, pid, "suspend")
}

/// Unfreeze a process we (or something else) suspended.
pub fn resume(pid: u32) -> Result<()> {
    let handle = open(pid, PROCESS_SUSPEND_RESUME, "resume")?;
    let status = unsafe { NtResumeProcess(handle) };
    unsafe { let _ = CloseHandle(handle); };
    ntok(status, pid, "resume")
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

/// Query-only access, used by callers that need to verify a process is alive.
#[allow(dead_code)]
pub fn is_alive(pid: u32) -> bool {
    match open(pid, PROCESS_QUERY_INFORMATION, "check") {
        Ok(handle) => {
            unsafe { let _ = CloseHandle(handle); };
            true
        }
        Err(_) => false,
    }
}

fn ntok(status: i32, pid: u32, action: &'static str) -> Result<()> {
    // STATUS_SUCCESS == 0; anything else is an NTSTATUS failure code.
    if status != 0 {
        return Err(ControlError {
            pid,
            action,
            message: format!("NTSTATUS 0x{status:08X}"),
        });
    }
    Ok(())
}

#[allow(dead_code)]
fn denied(pid: u32, action: &'static str) -> ControlError {
    ControlError {
        pid,
        action,
        message: "access denied".to_string(),
    }
}
