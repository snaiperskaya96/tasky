//! Tasky — a lightweight Windows task manager.
//!
//! `windows_subsystem` is dropped for debug builds so `--probe` diagnostics can
//! print to the console; release builds launch without a console window.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod collector;
mod fmt;
mod sys;

use std::time::Duration;

use eframe::egui;
use sys::icon;

const APP_NAME: &str = "Tasky";

fn main() -> eframe::Result<()> {
    if std::env::args().any(|argument| argument == "--probe") {
        probe();
        return Ok(());
    }

    // A second launch activates the running instance and exits.
    let Some(_instance) = sys::instance::claim() else {
        sys::instance::activate_existing();
        return Ok(());
    };
    sys::priority::raise_process();

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

    eframe::run_native(
        APP_NAME,
        native_options,
        Box::new(|cc| Ok(Box::new(app::TaskyApp::new(cc)))),
    )
}

/// Diagnostic mode: print what the sampler can and cannot read.
///
/// Sample twice, because CPU, disk and GPU numbers are rates.
fn probe() {
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};

    println!("Tasky probe");
    about_system();
    about_gpu();

    let suspended = Arc::new(Mutex::new(HashSet::new()));
    let sampler = collector::Sampler::spawn(suspended, || {});
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
    use sysinfo::System;
    let mut system = System::new();
    system.refresh_memory();
    system.refresh_cpu_all();
    println!(
        "  memory       : {} used of {} total",
        fmt::bytes(system.used_memory()),
        fmt::bytes(system.total_memory())
    );
    println!("  cpus         : {}", system.cpus().len());
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
