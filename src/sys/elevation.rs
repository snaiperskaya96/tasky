//! Running elevated without a UAC prompt on every launch.
//!
//! The binary asks for no elevation (`asInvoker`). Instead, Tasky can register
//! a logon scheduled task that runs it with highest privileges. Task Scheduler
//! starts it elevated without a prompt, so it is already running in the tray
//! when it is needed. A later unelevated launch (say, from the Start menu)
//! starts that task instead of itself.
//!
//! The UAC prompt is the wrong thing to depend on when the machine is
//! struggling: the secure desktop it switches to can itself take seconds.

use std::os::windows::process::CommandExt as _;
use std::process::Command;
use std::time::Duration;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, OpenProcess, OpenProcessToken, WaitForSingleObject,
    INFINITE, PROCESS_SYNCHRONIZE,
};
use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

const TASK_NAME: &str = "Tasky";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Whether this process has an elevated (administrator) token.
pub fn is_elevated() -> bool {
    let mut token = HANDLE::default();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.is_err() {
        return false;
    }
    let mut elevation = TOKEN_ELEVATION::default();
    let mut size = 0u32;
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut size,
        )
    }
    .is_ok();
    unsafe {
        let _ = CloseHandle(token);
    }
    ok && elevation.TokenIsElevated != 0
}

fn schtasks(args: &[&str]) -> bool {
    Command::new("schtasks.exe")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Whether the logon task is registered.
pub fn task_installed() -> bool {
    schtasks(&["/Query", "/TN", TASK_NAME])
}

/// Ask Task Scheduler to start the task now. Returns once it has been asked,
/// not once Tasky is up.
pub fn run_task() -> bool {
    schtasks(&["/Run", "/TN", TASK_NAME])
}

/// Register the logon task. Needs elevation.
pub fn install_task() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let user = match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
        (Ok(domain), Ok(name)) => format!("{domain}\\{name}"),
        (_, Ok(name)) => name,
        _ => return Err("could not determine the current user".into()),
    };
    let xml = task_xml(&exe.display().to_string(), &user);

    // schtasks reads task XML most reliably as UTF-16 with a BOM.
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(xml.encode_utf16().flat_map(u16::to_le_bytes));
    let path = std::env::temp_dir().join(format!("tasky-task-{}.xml", std::process::id()));
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    let ok = schtasks(&[
        "/Create",
        "/TN",
        TASK_NAME,
        "/XML",
        &path.display().to_string(),
        "/F",
    ]);
    let _ = std::fs::remove_file(&path);
    if ok {
        Ok(())
    } else {
        Err("schtasks could not create the task".into())
    }
}

/// Remove the logon task. Needs elevation.
pub fn uninstall_task() -> Result<(), String> {
    if schtasks(&["/Delete", "/TN", TASK_NAME, "/F"]) {
        Ok(())
    } else {
        Err("schtasks could not delete the task".into())
    }
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The defaults `schtasks /Create /SC ONLOGON` would give are wrong for a
/// resident tool: the task would not start on battery, be stopped when the
/// laptop unplugs, be killed after three days, and run at below-normal
/// priority. Spell every one of those out.
fn task_xml(exe: &str, user: &str) -> String {
    let exe = xml_escape(exe);
    let user = xml_escape(user);
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Starts Tasky in the tray, elevated, when you sign in.</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>false</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>4</Priority>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>3</Count>
    </RestartOnFailure>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{exe}</Command>
      <Arguments>--background</Arguments>
    </Exec>
  </Actions>
</Task>
"#
    )
}

/// Start this executable elevated (with a UAC prompt) with `args`. When
/// `wait` is set, block until it exits and return whether it succeeded;
/// otherwise return whether it started.
pub fn run_self_elevated(args: &str, wait: bool) -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let file = HSTRING::from(exe.as_os_str());
    let parameters = HSTRING::from(args);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // Fails with ERROR_CANCELLED when the user declines the prompt.
    if unsafe { ShellExecuteExW(&mut info) }.is_err() {
        return false;
    }
    let process = info.hProcess;
    if process.is_invalid() {
        return !wait;
    }
    let mut ok = true;
    if wait {
        let mut code = 1u32;
        ok = unsafe { WaitForSingleObject(process, INFINITE) } == WAIT_OBJECT_0
            && unsafe { GetExitCodeProcess(process, &mut code) }.is_ok()
            && code == 0;
    }
    unsafe {
        let _ = CloseHandle(process);
    }
    ok
}

/// Block until process `pid` has exited, or `timeout` passes.
pub fn wait_for_exit(pid: u32, timeout: Duration) {
    if let Ok(process) = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) } {
        unsafe {
            WaitForSingleObject(process, timeout.as_millis() as u32);
            let _ = CloseHandle(process);
        }
    }
}
