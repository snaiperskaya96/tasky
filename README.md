# Tasky

Tasky is a lightweight, drop-in replacement for the Windows Task Manager, written
in Rust. It lists processes with their CPU, GPU, I/O and RAM usage, lets you end,
suspend and resume them, and stays always available through a global hotkey and a
system tray icon.

## Features

- Live, sortable and filterable process table.
- Columns: Name, PID, Status, CPU %, Memory (private bytes), Memory %, GPU %,
  I/O read/write, Threads, Command line. Columns can be hidden in Settings.
- End task and suspend/resume for a single process or a multi-selection.
  Anything Tasky suspended is resumed when it exits.
- Open file location and copy process details.
- Global hotkey (Ctrl+Alt+T, falling back to Ctrl+Alt+Shift+T if another
  application already holds it).
- System tray icon: click to show, menu with Show / Always on top / Exit.
- Closing the window hides it to the tray; a second launch focuses the
  running instance.
- A Performance tab with a per-core CPU view.
- Settings: refresh interval, theme (system/dark/light), columns.
- Built to stay usable when the rest of the machine is not:
  - Runs at high priority, like Windows' own Task Manager, so it still gets
    CPU time when something else is pinning every core.
  - Pins its working set once warmed up, so Windows does not trim it and it
    does not have to page itself back in from a thrashing disk.
  - Reads the whole process list with one `NtQuerySystemInformation` call per
    sample, without opening process handles.
  - If the main window stops responding (a hung graphics driver, say), the
    hotkey opens an emergency window instead: a plain Win32 list with End
    task, on its own thread, drawn without the GPU. Tasky also falls back to
    it if the main UI cannot start (no OpenGL).
  - A panic resumes anything Tasky suspended before the process exits.
- Elevation without a UAC prompt on every launch, via an optional logon
  scheduled task (see below).
- GPU usage sourced from the PDH `GPU Engine(*)\Utilization Percentage` counter
  summed per PID (same method Task Manager uses), read only while the GPU
  column is visible.

## Download

Every push builds `tasky.exe` on GitHub Actions: open the latest run of the
**Build** workflow under the repository's Actions tab and download the
`tasky-…` artifact. Tagged versions (`v*`) are also published as releases with
the exe attached.

## Requirements

- Windows 10 / 11 (x86_64).
- Rust stable toolchain (1.85+) installed via rustup.
- A Visual C++ toolchain (MSVC) — available through the Visual Studio
  installer's "Desktop development with C++" workload.

## Build

```powershell
# Put cargo on PATH for this shell (recommended to add it to PATH permanently):
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"

cd tasky
cargo build                  # debug
cargo build --release        # optimized, LTO, stripped
```

The resulting binary is `target\debug\tasky.exe` (or `target\release\tasky.exe`).

## Run

Launch `tasky.exe` once; closing the window hides it to the tray. Use
`Ctrl+Alt+T` from anywhere to bring it up, or click the tray icon.

A second launch focuses the already-running instance instead of starting another
one (single-instance mutex plus a message posted to the first instance's hidden
window).

### Administrator rights

Without administrator rights Tasky works, but cannot end or suspend processes
owned by other users or by services. It never asks for elevation on launch,
because a UAC prompt is slow exactly when the machine is struggling. Instead:

- **Settings → Start with Windows, as administrator** registers a logon
  scheduled task that starts Tasky elevated in the tray, with no prompt (one
  UAC prompt to register it). After that, launching `tasky.exe` normally starts
  that task too. The task never times out, runs on battery, and is restarted
  if Tasky crashes.
- **Settings → Restart as administrator** relaunches the running instance
  elevated, once.

The same task can be managed from a terminal:

```powershell
tasky.exe --install-task      # asks for elevation if needed
tasky.exe --uninstall-task
```

### Diagnostics

`tasky.exe --probe` prints what the sampler can read (debug builds only, since
release builds have no console).

## Project layout

```
src/
    main.rs                 entry point
    app.rs                  eframe application, UI, table, handlers
    collector.rs            background sampler publishing Arc<Snapshot>
    emergency.rs            Win32 list-view fallback window
    fmt.rs                  bytes / rates / percentages
    sys/
        mod.rs
        control.rs          kill / suspend / resume / reveal-in-explorer
        elevation.rs        elevation check, logon task, relaunch elevated
        gpu.rs              PDH GPU Engine sampler
        hotkey.rs           RegisterHotKey
        icon.rs             application icon (generated at startup)
        instance.rs         single instance, host thread, window raising
        machine.rs          memory and CPU times, machine-wide and per core
        priority.rs         high priority class, pinned working set
        procs.rs            NtQuerySystemInformation process list
        tray.rs             tray-icon + muda menu
build.rs                    embeds the application manifest (asInvoker)
Cargo.toml
rust-toolchain.toml
```

## Tech stack

- UI: `eframe` / `egui` 0.32 (+ `egui_extras` tables)
- System info: `NtQuerySystemInformation` and Win32, via `windows` 0.62
  (PDH via `Win32_System_Performance`)
- Tray & menu: `tray-icon` 0.24 + `muda`

## Notes

- The process list is refreshed about once per second (0.5 s to 5 s in
  Settings; never faster than 250 ms, since CPU usage is a rate computed between
  two samples). Sampling pauses entirely while the window is hidden or
  minimized.
- Memory is private bytes (commit charge), the same as Task Manager's
  "Commit size" column. Task Manager's default "Memory" column is the private
  *working set*, so the two will differ.
- CPU is normalized to the whole machine, as in Task Manager: 100% means every
  logical processor is busy.
- Process path and command line are read once per process, when it first
  appears, not every sample.
- The I/O columns count all I/O a process does (files, network, devices), as
  Task Manager's I/O columns do, not only disk.
- Per-core CPU covers the first processor group (up to 64 logical
  processors); total CPU covers all of them.
- Suspending a process that depends on a driver or shared service can stall
  that subsystem; use with care. Tasky tracks what it suspended, forgets
  processes that exit, and resumes the rest when it quits.

## License

This project is provided as-is without a specific license; feel free to use and
modify locally.
