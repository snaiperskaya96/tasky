# Tasky

Tasky is a lightweight, drop-in replacement for the Windows Task Manager, written
in Rust. It lists processes with their CPU, GPU, disk and RAM usage, lets you end,
suspend and resume them, and stays always available through a global hotkey and a
system tray icon.

## Features

- Live, sortable and filterable process table.
- Columns: Name, Status, CPU %, RAM (private commit), Disk read/write, GPU %,
  PID, PPID, thread count, executable path, start time.
- Kill task and suspend/resume for a single process or a multi-selection.
- Restart, open file location, properties, copy command line/PID/image name.
- Global hotkey (Ctrl+Alt+T, with Ctrl+Shift+T and Ctrl+F11 fallbacks).
- System tray icon with Show / Always on top / Exit menu.
- Minimize-to-tray and "open at any time" single-instance window.
- Always-on-top toggle (also available from tray or Settings).
- A Performance tab with a graphical per-core CPU view.
- Settings: refresh interval, theme (system/dark/light), columns, suspend-all-on-exit.
- Elevation via a UAC `requireAdministrator` manifest so it can manage processes
  owned by other users and services.
- GPU usage sourced from the PDH `GPU Engine(*)\Utilization Percentage` counter
  summed per PID (same method Task Manager uses).

## Requirements

- Windows 10 / 11 (x86_64).
- Rust stable toolchain (1.85+ recommended; CI uses 1.99) installed via rustup.
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

Launch `tasky.exe` once (it starts minimized to the tray after the first close).
Use `Ctrl+Alt+T` from anywhere to bring it up, or double-click the tray icon.

A second launch focuses the already-running instance instead of starting another
one (single-instance mutex plus a broadcast `TaskyActivateWindow` message).

## Project layout

```
src/
    main.rs                 entry point
    app.rs                  eframe application, UI, table, handlers
    collector.rs             background sampler publishing Arc<Snapshot>
    fmt.rs                  bytes / rates / percentages
    sys/
        mod.rs
        control.rs          kill / suspend / resume / reveal-in-explorer
        gpu.rs              PDH GPU Engine sampler
        hotkey.rs           global-hotkey binding + polling
        icon.rs             application icon (embedded)
        instance.rs         single-instance + window raising via Win32
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
- Global hotkey: `global-hotkey` 0.6

## Notes

- The process list is refreshed about once per second (configurable in
  Settings, range 250ms-5s). Sampling below ~250ms makes the CPU percentage
  meaningless, since `sysinfo` needs two samples to compute a rate.
- RAM shown is sysinfo's `virtual_memory()` (private commit), which matches
  Task Manager's default "Commit size" column.
- CPU is sysinfo's already-normalized value (0-100 per logical core
  aggregate).
- Suspending a process that depends on a driver or shared service can stall
  that subsystem; use with care. Tasky keeps its own suspended-set and
  reconciles it on every sample.

## License

This project is provided as-is without a specific license; feel free to use and
modify locally.
