//! Tasky — a lightweight Windows task manager.
//!
//! `windows_subsystem` is dropped for debug builds so `--probe` diagnostics can
//! print to the console; release builds launch without a console window.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod collector;
mod emergency;
mod fmt;
mod sys;

use std::collections::HashSet;
use std::sync::{Arc, Mutex, TryLockError};
use std::time::Duration;

use eframe::egui;
use sys::{control, elevation, icon, instance};

const APP_NAME: &str = "Tasky";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |flag: &str| args.iter().any(|argument| argument == flag);

    if has("--probe") {
        probe();
        return;
    }
    if has("--install-task") {
        std::process::exit(task_command(elevation::install_task, "--install-task"));
    }
    if has("--uninstall-task") {
        std::process::exit(task_command(elevation::uninstall_task, "--uninstall-task"));
    }

    // "Restart as administrator" starts us before the old instance is gone.
    if let Some(pid) = args
        .iter()
        .position(|argument| argument == "--wait-for")
        .and_then(|at| args.get(at + 1))
        .and_then(|pid| pid.parse().ok())
    {
        elevation::wait_for_exit(pid, Duration::from_secs(15));
    }

    // A second launch brings the running instance forward and exits.
    if instance::is_running() {
        instance::activate_existing();
        return;
    }

    if !elevation::is_elevated() {
        // Prefer the elevated logon task to running with a limited token: it
        // needs no UAC prompt.
        if elevation::task_installed()
            && elevation::run_task()
            && instance::wait_until_running(Duration::from_secs(10))
        {
            instance::activate_existing();
            return;
        }
        if has("--elevate") && elevation::run_self_elevated("", false) {
            return;
        }
    }

    let Some(_instance) = instance::claim() else {
        instance::activate_existing();
        return;
    };
    sys::priority::raise_process();

    let suspended = Arc::new(Mutex::new(HashSet::new()));
    resume_suspended_on_panic(Arc::clone(&suspended));
    let sampler = collector::Sampler::spawn(Arc::clone(&suspended));
    emergency::init(sampler.clone());

    let generated = icon::app_icon();
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([980.0, 640.0])
            .with_min_inner_size([520.0, 320.0])
            .with_title(APP_NAME)
            .with_icon(egui::IconData {
                rgba: generated.rgba,
                width: generated.width,
                height: generated.height,
            }),
        ..Default::default()
    };

    let background = has("--background");
    let started = eframe::run_native(
        APP_NAME,
        native_options,
        Box::new(move |cc| {
            Ok(Box::new(app::TaskyApp::new(
                cc, sampler, suspended, background,
            )))
        }),
    );
    if started.is_err() {
        // No usable OpenGL (a broken or missing driver, a bare VM): fall back
        // to the plain Win32 window rather than not running at all.
        emergency::run_blocking();
    }
}

/// `--install-task` / `--uninstall-task`: do it if elevated, otherwise run
/// ourselves elevated to do it. Returns the process exit code.
fn task_command(action: fn() -> Result<(), String>, flag: &str) -> i32 {
    if !elevation::is_elevated() {
        return if elevation::run_self_elevated(flag, true) {
            0
        } else {
            1
        };
    }
    match action() {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{flag}: {error}");
            1
        }
    }
}

/// Never leave processes frozen because Tasky crashed. Release builds abort on
/// panic, so this runs just before the process goes away.
fn resume_suspended_on_panic(suspended: Arc<Mutex<HashSet<u32>>>) {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // The panicking thread may be the one holding the lock; never wait.
        match suspended.try_lock() {
            Ok(mut pids) => control::resume_all(&mut pids),
            Err(TryLockError::Poisoned(poisoned)) => {
                control::resume_all(&mut poisoned.into_inner())
            }
            Err(TryLockError::WouldBlock) => {}
        }
        default_hook(info);
    }));
}

/// Diagnostic mode: print what the sampler can and cannot read.
///
/// Sample twice, because CPU, disk and GPU numbers are rates.
fn probe() {
    println!("Tasky probe");
    about_system();
    about_gpu();

    let suspended = Arc::new(Mutex::new(HashSet::new()));
    let sampler = collector::Sampler::spawn(suspended);
    std::thread::sleep(Duration::from_millis(1600));

    let snapshot = sampler
        .latest()
        .unwrap_or_else(|| Arc::new(collector::Snapshot::default()));
    println!("  processes    : {}", snapshot.rows.len());
    let mut busiest: Vec<_> = snapshot.rows.iter().collect();
    busiest.sort_by(|a, b| b.cpu.total_cmp(&a.cpu));
    for row in busiest.iter().take(10) {
        println!(
            "    pid {:>6}  {:<24} cpu {:>5.1}%  gpu {:>4.0}%  mem {:>9}  {}",
            row.pid,
            row.info.name,
            row.cpu,
            row.gpu,
            fmt::bytes(row.memory),
            row.info.executable
        );
    }
    let totals = &snapshot.totals;
    println!(
        "  totals       : cpu {:.1}%  ram {} / {}  gpu {:.1}%  disk ↓ {} ↑ {}",
        totals.cpu,
        fmt::bytes(totals.memory_used),
        fmt::bytes(totals.memory_total),
        totals.gpu,
        fmt::rate(totals.read_bps),
        fmt::rate(totals.write_bps)
    );
}

fn about_system() {
    let (used, total) = sys::machine::memory();
    println!(
        "  memory       : {} used of {} total",
        fmt::bytes(used),
        fmt::bytes(total)
    );
    println!(
        "  cpus         : {} (first processor group)",
        sys::machine::PerCore::new().sample().len()
    );
    println!("  elevated     : {}", sys::elevation::is_elevated());
}

fn about_gpu() {
    let mut gpu = sys::gpu::GpuSampler::new();
    if !gpu.is_available() {
        println!("  gpu counters : unavailable");
        return;
    }

    // The counter is a rate, so it needs two readings before it settles.
    std::thread::sleep(Duration::from_millis(600));
    let mut sample: Vec<(u32, f64)> = gpu.sample().into_iter().collect();
    sample.sort_by(|a, b| b.1.total_cmp(&a.1));
    println!("  gpu counters : available, top users:");
    for (pid, value) in sample.iter().take(8) {
        println!("    pid {pid:>6}  {value:>5.1}%");
    }
}
