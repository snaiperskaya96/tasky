//! The sampling thread.
//!
//! One background thread owns all sampling state, so the UI never blocks on
//! it. It publishes an immutable `Arc<Snapshot>` into a shared slot
//! and pokes the UI to repaint; the UI only swaps in a new Arc pointer when one
//! appears, which keeps filtering and sorting off the frame path.
//!
//! While the window is hidden or minimized the thread parks and costs nothing.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

use crate::sys::gpu::GpuSampler;
use crate::sys::machine::{self, CpuTimes, PerCore};
use crate::sys::priority;
use crate::sys::procs::{self, ProcessTable};

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
    fn new(pid: u32, name: String) -> Self {
        let (executable, command_line) = procs::image_details(pid);
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
    /// `None` while GPU usage is not being collected, otherwise whether the
    /// per-process GPU counters could be read.
    pub gpu_available: Option<bool>,
}

/// Knobs the UI turns while the sampler runs.
struct Controls {
    interval_ms: AtomicU64,
    /// The main window is visible.
    active: AtomicBool,
    /// The emergency window is open.
    emergency: AtomicBool,
    want_gpu: AtomicBool,
}

impl Controls {
    fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed) || self.emergency.load(Ordering::Relaxed)
    }
}

type Callback = Box<dyn Fn() + Send>;

/// Handle to the background sampling thread. Clones share the same thread.
#[derive(Clone)]
pub struct Sampler {
    snapshot: Arc<Mutex<Option<Arc<Snapshot>>>>,
    controls: Arc<Controls>,
    on_update: Arc<Mutex<Option<Callback>>>,
    thread: Thread,
}

impl Sampler {
    /// `suspended` is shared with the UI so both can agree on which processes we
    /// have frozen.
    pub fn spawn(suspended: Arc<Mutex<HashSet<u32>>>) -> Self {
        let snapshot = Arc::new(Mutex::new(None));
        let controls = Arc::new(Controls {
            interval_ms: AtomicU64::new(SAMPLE_INTERVAL.as_millis() as u64),
            active: AtomicBool::new(true),
            emergency: AtomicBool::new(false),
            want_gpu: AtomicBool::new(true),
        });

        let on_update: Arc<Mutex<Option<Callback>>> = Arc::new(Mutex::new(None));

        let slot = Arc::clone(&snapshot);
        let shared = Arc::clone(&controls);
        let callback = Arc::clone(&on_update);
        let handle = thread::Builder::new()
            .name("tasky-sampler".into())
            .spawn(move || {
                collect(&slot, &suspended, &shared, || {
                    if let Some(callback) = lock(&callback).as_ref() {
                        callback();
                    }
                })
            })
            .expect("could not start the sampler thread");

        Self {
            snapshot,
            controls,
            on_update,
            thread: handle.thread().clone(),
        }
    }

    /// Run `callback` on the sampler thread after each new snapshot.
    pub fn set_on_update(&self, callback: impl Fn() + Send + 'static) {
        *lock(&self.on_update) = Some(Box::new(callback));
    }

    /// The newest snapshot, once one has landed.
    pub fn latest(&self) -> Option<Arc<Snapshot>> {
        lock(&self.snapshot).clone()
    }

    pub fn set_interval(&self, interval: Duration) {
        self.controls
            .interval_ms
            .store(interval.as_millis().max(1) as u64, Ordering::Relaxed);
        self.thread.unpark();
    }

    /// Pause sampling while nobody can see the main window.
    pub fn set_active(&self, active: bool) {
        Self::set_flag(&self.controls.active, active, &self.thread);
    }

    /// Keep sampling for the emergency window, whatever the main window does.
    pub fn set_emergency(&self, open: bool) {
        Self::set_flag(&self.controls.emergency, open, &self.thread);
    }

    fn set_flag(flag: &AtomicBool, on: bool, thread: &Thread) {
        if flag.swap(on, Ordering::Relaxed) != on {
            thread.unpark();
        }
    }

    /// The GPU performance counters are by far the most expensive thing to
    /// read, so only read them while GPU usage is on screen.
    pub fn set_collect_gpu(&self, on: bool) {
        self.controls.want_gpu.store(on, Ordering::Relaxed);
    }
}

/// Lock a mutex even if a panicking thread poisoned it; the data here is
/// always left consistent.
pub fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What we remember about a live process between samples.
struct Tracked {
    info: Arc<ProcessInfo>,
    cpu_time: u64,
    read: u64,
    written: u64,
}

/// State the sampler carries from one pass to the next.
struct State {
    table: ProcessTable,
    per_core: PerCore,
    /// Present only while GPU usage is wanted.
    gpu: Option<GpuSampler>,
    /// Keyed by PID and creation time, so a recycled PID starts afresh.
    tracked: HashMap<(u32, i64), Tracked>,
    cpu: CpuTimes,
    cores: Vec<CpuTimes>,
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
        table: ProcessTable::new(),
        per_core: PerCore::new(),
        gpu: None,
        tracked: HashMap::new(),
        cpu: CpuTimes::default(),
        cores: Vec::new(),
        sequence: 0,
    };

    let mut last_sample = prime(&mut state, suspended, controls);
    let mut was_active = true;
    // Right after a baseline, sample as soon as rates mean something, so the
    // window fills in quickly at startup and when it is brought back.
    let mut primed = true;

    loop {
        if !controls.is_active() {
            was_active = false;
            thread::park();
            continue;
        }
        if !was_active {
            // CPU, I/O and GPU are rates; without a fresh baseline the first
            // sample after a long pause would average over the whole pause.
            was_active = true;
            last_sample = prime(&mut state, suspended, controls);
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

        let snapshot = build_snapshot(&mut state, suspended, controls, elapsed);
        *lock(slot) = Some(snapshot);
        on_update();
    }
}

/// Take a baseline reading for every rate-based counter.
fn prime(state: &mut State, suspended: &Mutex<HashSet<u32>>, controls: &Controls) -> Instant {
    build_snapshot(state, suspended, controls, 1.0);
    Instant::now()
}

fn build_snapshot(
    state: &mut State,
    suspended: &Mutex<HashSet<u32>>,
    controls: &Controls,
    elapsed: f64,
) -> Arc<Snapshot> {
    state.table.refresh();
    let cpu = machine::cpu_times();
    let cores = state.per_core.sample();
    let (memory_used, memory_total) = machine::memory();

    // Opening the GPU query takes its own baseline, so a freshly enabled
    // sampler reports nothing until the next pass.
    let want_gpu = controls.want_gpu.load(Ordering::Relaxed);
    let gpu_by_pid = match (&mut state.gpu, want_gpu) {
        (Some(gpu), true) => gpu.sample(),
        (None, true) => {
            state.gpu = Some(GpuSampler::new());
            HashMap::new()
        }
        (_, false) => {
            state.gpu = None;
            HashMap::new()
        }
    };

    // Process CPU time over the machine's total CPU time (all processors,
    // idle included) gives Task Manager's normalized percentage directly.
    let cpu_elapsed = cpu.total.saturating_sub(state.cpu.total).max(1) as f64;

    let mut suspended_pids = lock(suspended);
    let mut tracked = HashMap::with_capacity(state.tracked.len() + 16);
    let mut rows = Vec::with_capacity(state.tracked.len() + 16);
    let mut totals = Totals {
        memory_used,
        memory_total,
        ..Totals::default()
    };

    for entry in state.table.iter() {
        let pid = entry.pid();
        // PID 0 is the idle "process"; it would only ever top the CPU column.
        if pid == 0 {
            continue;
        }
        let key = (pid, entry.create_time());
        let cpu_time = entry.cpu_time();
        let (read, written) = entry.io_bytes();

        // A process we have not seen before started after the previous pass,
        // so everything it has done so far happened within this interval.
        let previous = state.tracked.remove(&key);
        let (info, before) = match previous {
            Some(known) => (known.info, (known.cpu_time, known.read, known.written)),
            None => (Arc::new(ProcessInfo::new(pid, entry.name())), (0, 0, 0)),
        };

        let row = Row {
            pid,
            ppid: entry.ppid(),
            info: Arc::clone(&info),
            cpu: (cpu_time.saturating_sub(before.0) as f64 / cpu_elapsed * 100.0).min(100.0),
            memory: entry.private_bytes(),
            gpu: gpu_by_pid.get(&pid).copied().unwrap_or(0.0),
            read_bps: read.saturating_sub(before.1) as f64 / elapsed,
            write_bps: written.saturating_sub(before.2) as f64 / elapsed,
            total_read: read,
            total_written: written,
            threads: entry.threads(),
            suspended: suspended_pids.contains(&pid),
        };
        tracked.insert(
            key,
            Tracked {
                info,
                cpu_time,
                read,
                written,
            },
        );

        totals.gpu += row.gpu;
        totals.read_bps += row.read_bps;
        totals.write_bps += row.write_bps;
        rows.push(row);
    }

    // Forget processes we suspended that have since exited, so a recycled
    // PID is not shown as suspended (or resumed on exit).
    suspended_pids.retain(|pid| rows.iter().any(|row| row.pid == *pid));
    drop(suspended_pids);

    totals.processes = rows.len();
    totals.cpu = cpu.percent_since(state.cpu);
    totals.per_core_cpu = cores
        .iter()
        .zip(
            state
                .cores
                .iter()
                .chain(std::iter::repeat(&CpuTimes::default())),
        )
        .map(|(now, before)| now.percent_since(*before))
        .collect();
    totals.gpu = totals.gpu.min(100.0);

    // Whatever was not carried over belongs to processes that have exited.
    state.tracked = tracked;
    state.cpu = cpu;
    state.cores = cores;
    state.sequence += 1;

    Arc::new(Snapshot {
        rows,
        totals,
        sequence: state.sequence,
        gpu_available: state.gpu.as_ref().map(GpuSampler::is_available),
    })
}
