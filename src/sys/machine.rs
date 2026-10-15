//! Machine-wide memory and CPU counters.

use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::GetSystemTimes;

use crate::sys::procs::{query_growing, NtQuerySystemInformation};

const SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION_CLASS: u32 = 8;

/// `(used, total)` physical memory in bytes.
pub fn memory() -> (u64, u64) {
    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    if unsafe { GlobalMemoryStatusEx(&mut status) }.is_err() {
        return (0, 0);
    }
    (
        status.ullTotalPhys.saturating_sub(status.ullAvailPhys),
        status.ullTotalPhys,
    )
}

/// Busy and total time, in 100 ns units, summed over every logical processor.
#[derive(Clone, Copy, Default)]
pub struct CpuTimes {
    pub busy: u64,
    pub total: u64,
}

impl CpuTimes {
    /// Percent busy between `earlier` and `self`.
    pub fn percent_since(&self, earlier: CpuTimes) -> f64 {
        let total = self.total.saturating_sub(earlier.total);
        if total == 0 {
            return 0.0;
        }
        (self.busy.saturating_sub(earlier.busy) as f64 / total as f64 * 100.0).clamp(0.0, 100.0)
    }
}

fn filetime(ft: FILETIME) -> u64 {
    (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)
}

/// Whole-machine CPU times.
pub fn cpu_times() -> CpuTimes {
    let (mut idle, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    if unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.is_err() {
        return CpuTimes::default();
    }
    // Kernel time includes idle time.
    let total = filetime(kernel) + filetime(user);
    CpuTimes {
        busy: total.saturating_sub(filetime(idle)),
        total,
    }
}

#[repr(C)]
struct ProcessorPerformance {
    idle_time: i64,
    kernel_time: i64,
    user_time: i64,
    dpc_time: i64,
    interrupt_time: i64,
    interrupt_count: u32,
}

/// Per-logical-processor CPU times. Covers the calling thread's processor
/// group, i.e. up to 64 processors.
pub struct PerCore {
    buf: Vec<u64>,
}

impl PerCore {
    pub fn new() -> Self {
        Self {
            buf: vec![0; 64 * std::mem::size_of::<ProcessorPerformance>() / 8],
        }
    }

    pub fn sample(&mut self) -> Vec<CpuTimes> {
        let Some(written) = query_growing(&mut self.buf, |buffer, length, needed| unsafe {
            NtQuerySystemInformation(
                SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION_CLASS,
                buffer,
                length,
                needed,
            )
        }) else {
            return Vec::new();
        };
        let count = written / std::mem::size_of::<ProcessorPerformance>();
        // SAFETY: the kernel wrote `count` entries at the start of `buf`.
        let entries = unsafe {
            std::slice::from_raw_parts(self.buf.as_ptr().cast::<ProcessorPerformance>(), count)
        };
        entries
            .iter()
            .map(|cpu| {
                let total = (cpu.kernel_time.max(0) + cpu.user_time.max(0)) as u64;
                CpuTimes {
                    busy: total.saturating_sub(cpu.idle_time.max(0) as u64),
                    total,
                }
            })
            .collect()
    }
}
