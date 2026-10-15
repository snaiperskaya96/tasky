//! Per-process GPU utilisation, via the "GPU Engine" performance counters.
//!
//! This is exactly how Windows' own Task Manager computes its GPU column: the
//! `GPU Engine` PDH object has one instance per (process, physical GPU, engine)
//! triple, and the instance name embeds the owning PID, e.g.
//! `pid_8792_luid_0x00000000_0x0001D9AB_phys_0_eng_2_engtype_3D`.
//!
//! We therefore add a single wildcard counter, poll it, and sum the reported
//! utilisation of every instance back onto its PID.

use std::collections::HashMap;

use windows::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhOpenQueryW, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY,
};

const GPU_ENGINE_PATH: &str = r"\GPU Engine(*)\Utilization Percentage";

pub struct GpuSampler {
    query: PDH_HQUERY,
    counter: PDH_HCOUNTER,
    path: Vec<u16>,
    buf: Vec<u64>,
    available: bool,
}

impl GpuSampler {
    pub fn new() -> Self {
        let mut sampler = Self {
            query: PDH_HQUERY::default(),
            counter: PDH_HCOUNTER::default(),
            path: wide(GPU_ENGINE_PATH),
            // Over-aligned scratch buffer: PDH writes a struct with a pointer
            // field into it, so it must be at least 8-byte aligned.
            buf: Vec::new(),
            available: false,
        };

        if unsafe { PdhOpenQueryW(None, 0, &mut sampler.query) } != 0 {
            return sampler;
        }
        if unsafe {
            PdhAddEnglishCounterW(
                sampler.query,
                windows::core::PCWSTR(sampler.path.as_ptr()),
                0,
                &mut sampler.counter,
            )
        } != 0
        {
            unsafe { PdhCloseQuery(sampler.query) };
            return sampler;
        }

        // PDH "timer"-style counters are a rate and need two samples, so take a
        // first baseline reading immediately.
        unsafe { PdhCollectQueryData(sampler.query) };
        sampler.available = true;
        sampler
    }

    /// Whether the counters were successfully opened. Some machines (headless,
    /// remote sessions, basic display adapters) have no GPU Engine object.
    pub fn is_available(&self) -> bool {
        self.available
    }

    /// Sample the counter, returning GPU utilisation in percent, summed per PID.
    pub fn sample(&mut self) -> HashMap<u32, f64> {
        let mut out = HashMap::new();
        if !self.available {
            return out;
        }

        if unsafe { PdhCollectQueryData(self.query) } != 0 {
            return out;
        }

        // First call reports the size we need for the whole instance array.
        let mut size = 0u32;
        let mut count = 0u32;
        if unsafe {
            PdhGetFormattedCounterArrayW(
                self.counter,
                PDH_FMT_DOUBLE,
                &mut size,
                &mut count,
                None,
            )
        } != 0
            || size == 0
        {
            return out;
        }

        let words = (size as usize).div_ceil(8);
        if self.buf.len() < words {
            self.buf.resize(words.max(64), 0);
        }

        let mut item_count = count;
        let rc = unsafe {
            PdhGetFormattedCounterArrayW(
                self.counter,
                PDH_FMT_DOUBLE,
                &mut size,
                &mut item_count,
                Some(self.buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W),
            )
        };
        if rc != 0 {
            return out;
        }

        out.reserve(item_count as usize);
        let items = self.buf.as_ptr() as *const PDH_FMT_COUNTERVALUE_ITEM_W;
        for i in 0..item_count as isize {
            // SAFETY: `item_count` entries were just written by PDH into `buf`.
            let item = unsafe { &*items.offset(i) };
            let name = read_pwstr(item.szName.0);
            if let Some(pid) = pid_from_instance_name(&name) {
                // SAFETY: we requested PDH_FMT_DOUBLE, which fills this union arm.
                let value = unsafe { item.FmtValue.Anonymous.doubleValue };
                if value > 0.0 {
                    *out.entry(pid).or_insert(0.0) += value;
                }
            }
        }

        // Multiple engine types (3D, copy, video-decode, ...) can be busy at
        // once; Task Manager caps the reported total at 100%.
        for v in out.values_mut() {
            if *v > 100.0 {
                *v = 100.0;
            }
        }
        out
    }
}

impl Drop for GpuSampler {
    fn drop(&mut self) {
        if self.available {
            unsafe { PdhCloseQuery(self.query) };
        }
    }
}

/// Pull the PID out of an instance name such as `pid_1234_luid_0x123_phys_0_eng_1_engtype_3D`.
fn pid_from_instance_name(name: &str) -> Option<u32> {
    let mut parts = name.split('_');
    if parts.next()? != "pid" {
        return None;
    }
    parts.next()?.parse().ok()
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn read_pwstr(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while unsafe { *ptr.add(len) } != 0 {
        len += 1;
        if len > 512 {
            break;
        }
    }
    let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
    String::from_utf16_lossy(slice)
}
