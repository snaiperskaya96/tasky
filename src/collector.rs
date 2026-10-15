//! The sampling thread.
//!
//! One background thread owns the whole sysinfo state machine, so the UI never
//! blocks on it. It publishes an immutable `Arc<Snapshot>` into a shared slot;
//! the UI only swaps in a new Arc pointer when one appears, which keeps
//! filtering and sorting off the frame path.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

use crate::sys::{gpu::GpuSampler, procs::thread_counts};

/// How often the sampler wakes up.
pub const SAMPLE_INTERVAL: Duration = Duration::from_millis(1000);
/// Sampling faster than this makes CPU numbers meaningless.
pub const MIN_SAMPLE_INTERVAL: Duration = Duration::from_millis(250);

/// One process in a snapshot.
#[derive(Clone, Debug, Default)]
pub struct Row {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
    pub executable: String,
    /// Percent of total machine CPU, i.e. normalized across all cores, the same
    /// way Task Manager reports it: 100.0 means every core is busy.
    pub cpu: f64,
    /// Private working set / commit, in bytes.
    pub memory: u64,
    /// GPU utilisation in percent.
    pub gpu: f64,
    pub read_bps: f64,
    pub write_bps: f64,
    /// Lifetime byte totals, for the tooltip.
    pub total_read: u64,
    pub total_written: u64,
    pub threads: u32,
    pub command_line: String,
    pub suspended: bool,
}

impl Row {
pub fn matches(&self, needle: &str) -> bool {
        if needle.is_empty() {
            return true;
        }
        if row_contains(&self.name, needle)
            || row_contains(&self.executable, needle)
            || row_contains(&self.command_line, needle)
            || self.pid.to_string().contains(needle)
        {
            return true;
        }
        false
    }
}

fn row_contains(value: &str, needle: &str) -> bool {
    value.to_lowercase().contains(needle)
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
    pub cpu_cores: usize,
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
pub struct Snapshot {
    pub rows: Vec<Row>,
    pub totals: Totals,
    /// Monotonic id, so the UI can tell "new data" from "same data".
    pub sequence: u64,
    /// Whether per-process GPU counters were available.
    pub gpu_available: bool,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            totals: Totals::default(),
            sequence: 0,
            gpu_available: false,
        }
    }
}

/// Owns the background sampling thread.
pub struct Sampler {
    snapshot: Arc<Mutex<Option<Arc<Snapshot>>>>,
    interval_ms: Arc<AtomicU64>,
}

impl Sampler {
    /// `suspended` is shared with the UI so both can agree on which processes we
    /// have frozen.
    pub fn spawn(suspended: Arc<Mutex<HashSet<u32>>>) -> Self {
        let snapshot = Arc::new(Mutex::new(None));
        let interval_ms = Arc::new(AtomicU64::new(SAMPLE_INTERVAL.as_millis() as u64));

        let slot = Arc::clone(&snapshot);
        let interval = Arc::clone(&interval_ms);

        std::thread::Builder::new()
            .name("tasky-sampler".into())
            .spawn(move || collect(slot, &suspended, interval))
            .expect("could not start the sampler thread");

        Self {
            snapshot,
            interval_ms,
        }
    }

    /// The newest snapshot, once one has landed.
    pub fn latest(&self) -> Option<Arc<Snapshot>> {
        self.snapshot.lock().unwrap().clone()
    }

    pub fn set_interval(&self, interval: Duration) {
        self.interval_ms
            .store(interval.as_millis().max(1) as u64, Ordering::Relaxed);
    }
}

fn collect(
    slot: Arc<Mutex<Option<Arc<Snapshot>>>>,
    suspended: &Mutex<HashSet<u32>>,
    interval_ms: Arc<AtomicU64>,
) {
    let mut sys = System::new();
    // One static refresh gets us total RAM and core counts, which only change
    // when the machine is reconfigured.
    sys.refresh_memory();
    sys.refresh_cpu_all();

    let cpu_cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);

    let mut gpu = GpuSampler::new();
    let gpu_available = gpu.is_available();

    // Priming pass: sysinfo's counters are rates, so without one reading first
    // the first real sample would report nonsense.
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        false,
        ProcessRefreshKind::everything(),
    );
    let mut previous_io = seed_io(&sys);

    let mut sequence = 0u64;
    let mut last_sample = Instant::now();

    loop {
        let interval = Duration::from_millis(interval_ms.load(Ordering::Relaxed))
            .max(MIN_SAMPLE_INTERVAL);
        let target = last_sample + interval;
        // Guard against a snapshot landing in under the minimum sampling gap.
        while Instant::now() < target {
            std::thread::sleep(MIN_SAMPLE_INTERVAL);
        }

        let now = Instant::now();
        let elapsed = now.duration_since(last_sample).as_secs_f64().max(1e-3);
        last_sample = now;

        let snapshot = build_snapshot(
            &mut sys,
            &mut gpu,
            &mut previous_io,
            suspended,
            cpu_cores,
            gpu_available,
            elapsed,
            sequence + 1,
        );
        sequence += 1;
        *slot.lock().unwrap() = Some(snapshot);
    }
}

/// Record the current byte counters so the next sample can build rates.
fn seed_io(sys: &System) -> HashMap<u32, (u64, u64)> {
    let mut out = HashMap::new();
    for (pid, process) in sys.processes() {
        let usage = process.disk_usage();
        out.insert(
            pid.as_u32(),
            (usage.total_read_bytes, usage.total_written_bytes),
        );
    }
    out
}

fn build_snapshot(
    sys: &mut System,
    gpu: &mut GpuSampler,
    previous_io: &mut HashMap<u32, (u64, u64)>,
    suspended: &Mutex<HashSet<u32>>,
    cpu_cores: usize,
    gpu_available: bool,
    elapsed: f64,
    sequence: u64,
) -> Arc<Snapshot> {
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::everything(),
    );
    sys.refresh_memory();
    sys.refresh_cpu_all();

    let gpu_by_pid = if gpu_available {
        gpu.sample()
    } else {
        HashMap::new()
    };
    let threads_by_pid = thread_counts();
    let suspended_pids = suspended.lock().unwrap();

    let mut rows = Vec::with_capacity(sys.processes().len());
    let mut totals = Totals {
        cpu_cores,
        memory_total: sys.total_memory(),
        ..Totals::default()
    };

    for (pid, process) in sys.processes() {
        let pid = pid.as_u32();

        let usage = process.disk_usage();
        let (read_at, written_at) = (usage.total_read_bytes, usage.total_written_bytes);
        let (read_bps, write_bps) = match previous_io.get(&pid) {
            // No previous reading means either a brand new process or the very
            // first sample; reporting a rate there would be a lie.
            Some(&(read_before, written_before)) => (
                read_at.saturating_sub(read_before) as f64 / elapsed,
                written_at.saturating_sub(written_before) as f64 / elapsed,
            ),
            None => (0.0, 0.0),
        };
        previous_io.insert(pid, (read_at, written_at));

        let row = Row {
            pid,
            ppid: process.parent().map(|p| p.as_u32()).unwrap_or(0),
            name: process.name().to_string_lossy().into_owned(),
            executable: process
                .exe()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            command_line: process
                .cmd()
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" "),
            cpu: process.cpu_usage() as f64,
            // Task Manager's default memory column is the private working set,
            // which sysinfo reports as `virtual_memory`.
            memory: process.virtual_memory(),
            gpu: gpu_by_pid.get(&pid).copied().unwrap_or(0.0),
            read_bps,
            write_bps,
            total_read: read_at,
            total_written: written_at,
            threads: threads_by_pid.get(&pid).copied().unwrap_or(0),
            suspended: suspended_pids.contains(&pid),
        };

        totals.cpu += row.cpu;
        totals.gpu += row.gpu;
        totals.read_bps += row.read_bps;
        totals.write_bps += row.write_bps;
        rows.push(row);
    }

    totals.processes = rows.len();
    totals.memory_used = sys.used_memory();
    let per_core = sys.cpus().iter().map(|c| c.cpu_usage() as f64).collect::<Vec<_>>();
    totals.per_core_cpu = per_core;
    totals.cpu = sys.global_cpu_usage() as f64;

    Arc::new(Snapshot {
        rows,
        totals,
        sequence,
        gpu_available,
    })
}
