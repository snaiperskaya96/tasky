//! The sampling thread.
//!
//! One background thread owns the whole sysinfo state machine, so the UI never
//! blocks on it. It publishes an immutable `Arc<Snapshot>` into a shared slot
//! and pokes the UI to repaint; the UI only swaps in a new Arc pointer when one
//! appears, which keeps filtering and sorting off the frame path.
//!
//! While the window is hidden or minimized the thread parks and costs nothing.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

use sysinfo::{Pid, Process, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::sys::{gpu::GpuSampler, priority, procs::thread_counts};

/// How often the sampler wakes up.
pub const SAMPLE_INTERVAL: Duration = Duration::from_millis(1000);
/// Sampling faster than this makes CPU numbers meaningless.
pub const MIN_SAMPLE_INTERVAL: Duration = Duration::from_millis(250);

/// The parts of a process that never change while it lives. Built once per
/// process and shared by every snapshot, so a sample does not re-allocate
/// hundreds of strings.
#[derive(Debug, Default)]
pub struct ProcessInfo {
    pub name: String,
    pub executable: String,
    pub command_line: String,
    /// Lower-cased name, path, command line and PID, for the search box.
    search_key: String,
}

impl ProcessInfo {
    fn new(pid: u32, process: &Process) -> Self {
        let name = process.name().to_string_lossy().into_owned();
        let executable = process
            .exe()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        let command_line = process
            .cmd()
            .iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        let search_key = format!("{name}\n{executable}\n{command_line}\n{pid}").to_lowercase();
        Self {
            name,
            executable,
            command_line,
            search_key,
        }
    }
}

/// One process in a snapshot.
#[derive(Clone, Debug)]
pub struct Row {
    pub pid: u32,
    pub ppid: u32,
    pub info: Arc<ProcessInfo>,
    /// Percent of total machine CPU, i.e. normalized across all cores, the same
    /// way Task Manager reports it: 100.0 means every core is busy.
    pub cpu: f64,
    /// Private bytes (commit charge), in bytes.
    pub memory: u64,
    /// GPU utilisation in percent.
    pub gpu: f64,
    pub read_bps: f64,
    pub write_bps: f64,
    /// Lifetime byte totals, for the tooltip.
    pub total_read: u64,
    pub total_written: u64,
    /// Only filled in while the Threads column is visible.
    pub threads: u32,
    pub suspended: bool,
}

impl Row {
    /// `needle` must already be lower-cased.
    pub fn matches(&self, needle: &str) -> bool {
        needle.is_empty() || self.info.search_key.contains(needle)
    }
}

/// Machine-wide totals for the status bar.
#[derive(Clone, Debug, Default)]
pub struct Totals {
    pub processes: usize,
    pub cpu: f64,
    pub gpu: f64,
    pub memory_used: u64,
    pub memory_total: u64,
    pub read_bps: f64,
    pub write_bps: f64,
    pub per_core_cpu: Vec<f64>,
}

impl Totals {
    pub fn memory_percent(&self) -> f64 {
        if self.memory_total == 0 {
            0.0
        } else {
            self.memory_used as f64 / self.memory_total as f64 * 100.0
        }
    }
}

/// An immutable picture of the system at one point in time.
#[derive(Default)]
pub struct Snapshot {
    pub rows: Vec<Row>,
    pub totals: Totals,
    /// Monotonic id, so the UI can tell "new data" from "same data".
    pub sequence: u64,
    /// Whether per-process GPU counters were available.
    pub gpu_available: bool,
}

/// Knobs the UI turns while the sampler runs.
struct Controls {
    interval_ms: AtomicU64,
    active: AtomicBool,
    want_threads: AtomicBool,
}

/// Owns the background sampling thread.
pub struct Sampler {
    snapshot: Arc<Mutex<Option<Arc<Snapshot>>>>,
    controls: Arc<Controls>,
    thread: Thread,
}

impl Sampler {
    /// `suspended` is shared with the UI so both can agree on which processes we
    /// have frozen. `on_update` runs on the sampler thread after each snapshot.
    pub fn spawn(
        suspended: Arc<Mutex<HashSet<u32>>>,
        on_update: impl Fn() + Send + 'static,
    ) -> Self {
        let snapshot = Arc::new(Mutex::new(None));
        let controls = Arc::new(Controls {
            interval_ms: AtomicU64::new(SAMPLE_INTERVAL.as_millis() as u64),
            active: AtomicBool::new(true),
            want_threads: AtomicBool::new(false),
        });

        let slot = Arc::clone(&snapshot);
        let shared = Arc::clone(&controls);
        let handle = thread::Builder::new()
            .name("tasky-sampler".into())
            .spawn(move || collect(&slot, &suspended, &shared, on_update))
            .expect("could not start the sampler thread");

        Self {
            snapshot,
            controls,
            thread: handle.thread().clone(),
        }
    }

    /// The newest snapshot, once one has landed.
    pub fn latest(&self) -> Option<Arc<Snapshot>> {
        self.snapshot.lock().unwrap().clone()
    }

    pub fn set_interval(&self, interval: Duration) {
        self.controls
            .interval_ms
            .store(interval.as_millis().max(1) as u64, Ordering::Relaxed);
        self.thread.unpark();
    }

    /// Pause sampling while nobody can see the result.
    pub fn set_active(&self, active: bool) {
        let was_active = self.controls.active.swap(active, Ordering::Relaxed);
        if active && !was_active {
            self.thread.unpark();
        }
    }

    /// Thread counts need their own system-wide snapshot, so only take one
    /// while the column is on screen.
    pub fn set_collect_threads(&self, on: bool) {
        self.controls.want_threads.store(on, Ordering::Relaxed);
    }
}

/// What we ask sysinfo for on every pass. Notably not `everything()`, which
/// also reads each process's environment block, working directory and owner
/// token. Path and command line are read once per process.
fn refresh_kind() -> ProcessRefreshKind {
    ProcessRefreshKind::nothing()
        .with_cpu()
        .with_memory()
        .with_disk_usage()
        .with_exe(UpdateKind::OnlyIfNotSet)
        .with_cmd(UpdateKind::OnlyIfNotSet)
}

/// Per-process state the sampler carries from one pass to the next.
struct State {
    sys: System,
    gpu: GpuSampler,
    /// Keyed by PID and start time, so a recycled PID gets fresh info.
    info: HashMap<(u32, u64), Arc<ProcessInfo>>,
    sequence: u64,
}

fn collect(
    slot: &Mutex<Option<Arc<Snapshot>>>,
    suspended: &Mutex<HashSet<u32>>,
    controls: &Controls,
    on_update: impl Fn(),
) {
    priority::lower_current_thread();

    let mut state = State {
        sys: System::new(),
        gpu: GpuSampler::new(),
        info: HashMap::new(),
        sequence: 0,
    };
    // Total RAM and the CPU list only change when the machine is
    // reconfigured, but sysinfo needs the CPU list to scale process usage.
    state.sys.refresh_memory();
    state.sys.refresh_cpu_all();

    let mut last_sample = prime(&mut state);
    let mut was_active = true;
    // Right after a baseline, sample as soon as rates mean something, so the
    // window fills in quickly at startup and when it is brought back.
    let mut primed = true;

    loop {
        if !controls.active.load(Ordering::Relaxed) {
            was_active = false;
            thread::park();
            continue;
        }
        if !was_active {
            // CPU, disk and GPU are rates; without a fresh baseline the first
            // sample after a long pause would average over the whole pause.
            was_active = true;
            last_sample = prime(&mut state);
            primed = true;
        }

        let interval = if primed {
            MIN_SAMPLE_INTERVAL
        } else {
            Duration::from_millis(controls.interval_ms.load(Ordering::Relaxed))
                .max(MIN_SAMPLE_INTERVAL)
        };
        let now = Instant::now();
        let due = last_sample + interval;
        if now < due {
            // Unparked early when the interval or activity changes.
            thread::park_timeout(due - now);
            continue;
        }

        let elapsed = now.duration_since(last_sample).as_secs_f64().max(1e-3);
        last_sample = now;
        primed = false;

        let want_threads = controls.want_threads.load(Ordering::Relaxed);
        let snapshot = build_snapshot(&mut state, suspended, elapsed, want_threads);
        *slot.lock().unwrap() = Some(snapshot);
        on_update();
    }
}

/// Take a baseline reading for every rate-based counter.
fn prime(state: &mut State) -> Instant {
    state
        .sys
        .refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind());
    state.sys.refresh_cpu_usage();
    state.gpu.sample();
    Instant::now()
}

fn build_snapshot(
    state: &mut State,
    suspended: &Mutex<HashSet<u32>>,
    elapsed: f64,
    want_threads: bool,
) -> Arc<Snapshot> {
    let sys = &mut state.sys;
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind());
    sys.refresh_memory();
    sys.refresh_cpu_usage();

    let gpu_by_pid = state.gpu.sample();
    let threads_by_pid = if want_threads {
        thread_counts()
    } else {
        HashMap::new()
    };

    // sysinfo reports per-process CPU on a per-core scale (100% per core).
    let cpu_count = sys.cpus().len().max(1) as f64;

    // Forget processes we suspended that have since exited, so a recycled
    // PID is not shown as suspended (or resumed on exit).
    let mut suspended_pids = suspended.lock().unwrap();
    suspended_pids.retain(|pid| sys.process(Pid::from_u32(*pid)).is_some());

    let mut info = HashMap::with_capacity(sys.processes().len());
    let mut rows = Vec::with_capacity(sys.processes().len());
    let mut totals = Totals {
        memory_total: sys.total_memory(),
        ..Totals::default()
    };

    for (pid, process) in sys.processes() {
        let pid = pid.as_u32();

        let key = (pid, process.start_time());
        let cached = state.info.remove(&key).filter(|known| {
            // Path and command line can fail to read on first sight and
            // succeed later; pick them up when they do.
            !(known.executable.is_empty() && process.exe().is_some()
                || known.command_line.is_empty() && !process.cmd().is_empty())
        });
        let process_info = cached.unwrap_or_else(|| Arc::new(ProcessInfo::new(pid, process)));
        info.insert(key, Arc::clone(&process_info));

        // sysinfo keeps the bytes moved since the previous refresh; for a
        // process first seen this pass that is everything since it started.
        let usage = process.disk_usage();
        let row = Row {
            pid,
            ppid: process.parent().map(|p| p.as_u32()).unwrap_or(0),
            info: process_info,
            cpu: process.cpu_usage() as f64 / cpu_count,
            memory: process.virtual_memory(),
            gpu: gpu_by_pid.get(&pid).copied().unwrap_or(0.0),
            read_bps: usage.read_bytes as f64 / elapsed,
            write_bps: usage.written_bytes as f64 / elapsed,
            total_read: usage.total_read_bytes,
            total_written: usage.total_written_bytes,
            threads: threads_by_pid.get(&pid).copied().unwrap_or(0),
            suspended: suspended_pids.contains(&pid),
        };

        totals.gpu += row.gpu;
        totals.read_bps += row.read_bps;
        totals.write_bps += row.write_bps;
        rows.push(row);
    }
    drop(suspended_pids);
    // Whatever is left belongs to processes that have exited.
    state.info = info;

    totals.processes = rows.len();
    totals.memory_used = sys.used_memory();
    totals.per_core_cpu = sys.cpus().iter().map(|c| c.cpu_usage() as f64).collect();
    totals.cpu = sys.global_cpu_usage() as f64;
    totals.gpu = totals.gpu.min(100.0);

    state.sequence += 1;
    Arc::new(Snapshot {
        rows,
        totals,
        sequence: state.sequence,
        gpu_available: state.gpu.is_available(),
    })
}
