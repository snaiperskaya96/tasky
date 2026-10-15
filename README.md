# Tasky

Tasky is a lightweight, drop-in replacement for the Windows Task Manager, written
in Rust. It lists processes with their CPU, GPU, disk and RAM usage, lets you end,
suspend and resume them, and stays always available through a global hotkey and a
system tray icon.

## Features

- Live, sortable and filterable process table.
- Columns: Name, PID, Status, CPU %, Memory (private bytes), Memory %, GPU %,
  Disk read/write, Threads, Command line. Columns can be hidden in Settings.
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
- Runs at high priority, like Windows' own Task Manager, so it stays
  responsive when something else is pinning every core.
- Elevation via a UAC `requireAdministrator` manifest so it can manage processes
  owned by other users and services.
- GPU usage sourced from the PDH `GPU Engine(*)\Utilization Percentage` counter
  summed per PID (same method Task Manager uses).

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
Because the binary requests administrator rights, Windows will prompt for UAC on
launch.

## Run

Launch `tasky.exe` once; closing the window hides it to the tray. Use
`Ctrl+Alt+T` from anywhere to bring it up, or click the tray icon.

A second launch focuses the already-running instance instead of starting another
one (single-instance mutex plus a message posted to the first instance's hidden
window).

`tasky.exe --probe` prints what the sampler can read (debug builds only, since
release builds have no console).

## Project layout

```
src/
    main.rs                 entry point
    app.rs                  eframe application, UI, table, handlers
    collector.rs            background sampler publishing Arc<Snapshot>
    fmt.rs                  bytes / rates / percentages
    sys/
        mod.rs
        control.rs          kill / suspend / resume / reveal-in-explorer
        gpu.rs              PDH GPU Engine sampler
        hotkey.rs           RegisterHotKey on its own thread
        icon.rs             application icon (generated at startup)
        instance.rs         single-instance + window raising via Win32
        priority.rs         high priority class for the process
        procs.rs            toolhelp32 thread counts per PID
        tray.rs             tray-icon + muda menu
build.rs                    embed-manifest requireAdministrator
Cargo.toml
rust-toolchain.toml
```

## Tech stack

- UI: `eframe` / `egui` 0.32 (+ `egui_extras` tables)
- System info: `sysinfo` 0.36
- Win32: `windows` 0.62 (PDH via `Win32_System_Performance`)
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
- Process path and command line are read once per process, not every sample.
  Thread counts are only collected while the Threads column is visible.
- Suspending a process that depends on a driver or shared service can stall
  that subsystem; use with care. Tasky tracks what it suspended, forgets
  processes that exit, and resumes the rest when it quits.

## License

This project is provided as-is without a specific license; feel free to use and
modify locally.
