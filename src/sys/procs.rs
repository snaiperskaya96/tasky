//! Process enumeration straight from the kernel.
//!
//! One `NtQuerySystemInformation(SystemProcessInformation)` call returns every
//! process with its parent, thread count, CPU times, private bytes and I/O
//! counters, without opening a single process handle. This is what Task
//! Manager itself does. A handle is only opened once per new process, to read
//! its full image path and command line.
//!
//! `SYSTEM_PROCESS_INFORMATION` is only partly documented; the layout below is
//! the one every Windows version since Vista uses (see phnt's `ntexapi.h`).

use std::ffi::c_void;

use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};

const SYSTEM_PROCESS_INFORMATION_CLASS: u32 = 5;
const PROCESS_COMMAND_LINE_INFORMATION: u32 = 60;
const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;
const STATUS_BUFFER_TOO_SMALL: i32 = 0xC000_0023_u32 as i32;
/// Give up rather than grow a query buffer without bound.
const MAX_QUERY_BYTES: usize = 64 * 1024 * 1024;

#[link(name = "ntdll")]
extern "system" {
    pub(crate) fn NtQuerySystemInformation(
        class: u32,
        buffer: *mut c_void,
        length: u32,
        return_length: *mut u32,
    ) -> i32;
    fn NtQueryInformationProcess(
        process: HANDLE,
        class: u32,
        buffer: *mut c_void,
        length: u32,
        return_length: *mut u32,
    ) -> i32;
}

/// Run an `Nt*` query into `buf`, growing it until the answer fits.
/// Returns the number of bytes written.
pub(crate) fn query_growing(
    buf: &mut Vec<u64>,
    mut call: impl FnMut(*mut c_void, u32, &mut u32) -> i32,
) -> Option<usize> {
    loop {
        let capacity = (buf.len() * 8) as u32;
        let mut needed = 0u32;
        match call(buf.as_mut_ptr().cast(), capacity, &mut needed) {
            0 => return Some(needed.min(capacity) as usize),
            STATUS_INFO_LENGTH_MISMATCH | STATUS_BUFFER_TOO_SMALL
                if buf.len() * 8 < MAX_QUERY_BYTES =>
            {
                // Processes can start between two calls; leave headroom.
                let words = (needed as usize).max(capacity as usize) / 8 + 8 * 1024;
                buf.resize(words, 0);
            }
            _ => return None,
        }
    }
}

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *const u16,
}

impl UnicodeString {
    /// SAFETY: `buffer` must point at `length` valid bytes, or be null.
    unsafe fn to_string_lossy(&self) -> String {
        if self.buffer.is_null() || self.length == 0 {
            return String::new();
        }
        let units = unsafe { std::slice::from_raw_parts(self.buffer, self.length as usize / 2) };
        String::from_utf16_lossy(units)
    }
}

#[repr(C)]
#[allow(dead_code)] // Mirrors the kernel's layout; not every field is read.
struct SystemProcessInformation {
    next_entry_offset: u32,
    number_of_threads: u32,
    working_set_private_size: i64,
    hard_fault_count: u32,
    number_of_threads_high_watermark: u32,
    cycle_time: u64,
    create_time: i64,
    user_time: i64,
    kernel_time: i64,
    image_name: UnicodeString,
    base_priority: i32,
    unique_process_id: usize,
    inherited_from_unique_process_id: usize,
    handle_count: u32,
    session_id: u32,
    unique_process_key: usize,
    peak_virtual_size: usize,
    virtual_size: usize,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
    quota_peak_paged_pool_usage: usize,
    quota_paged_pool_usage: usize,
    quota_peak_non_paged_pool_usage: usize,
    quota_non_paged_pool_usage: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
    private_page_count: usize,
    read_operation_count: i64,
    write_operation_count: i64,
    other_operation_count: i64,
    read_transfer_count: i64,
    write_transfer_count: i64,
    other_transfer_count: i64,
    // Followed by `number_of_threads` SYSTEM_THREAD_INFORMATION entries.
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<SystemProcessInformation>() == 256);

/// The whole process list, as one kernel snapshot.
pub struct ProcessTable {
    /// `u64` so every entry is suitably aligned.
    buf: Vec<u64>,
    len: usize,
}

impl ProcessTable {
    pub fn new() -> Self {
        Self {
            buf: vec![0; 256 * 1024 / 8],
            len: 0,
        }
    }

    /// Take a fresh snapshot. On failure the table is empty.
    pub fn refresh(&mut self) {
        self.len = query_growing(&mut self.buf, |buffer, length, needed| unsafe {
            NtQuerySystemInformation(SYSTEM_PROCESS_INFORMATION_CLASS, buffer, length, needed)
        })
        .unwrap_or(0);
    }

    pub fn iter(&self) -> impl Iterator<Item = Entry<'_>> {
        let base = self.buf.as_ptr().cast::<u8>();
        let len = self.len;
        let mut offset = (len > 0).then_some(0usize);
        std::iter::from_fn(move || {
            let at = offset?;
            if at + std::mem::size_of::<SystemProcessInformation>() > len {
                offset = None;
                return None;
            }
            // SAFETY: the kernel wrote a chain of 8-byte aligned entries into
            // `buf`, each linked by `next_entry_offset`, and this one lies
            // inside the written part.
            let raw = unsafe { &*base.add(at).cast::<SystemProcessInformation>() };
            offset = match raw.next_entry_offset {
                0 => None,
                next => Some(at + next as usize),
            };
            Some(Entry { raw })
        })
    }
}

/// One process in a [`ProcessTable`].
pub struct Entry<'a> {
    raw: &'a SystemProcessInformation,
}

impl Entry<'_> {
    pub fn pid(&self) -> u32 {
        self.raw.unique_process_id as u32
    }

    pub fn ppid(&self) -> u32 {
        self.raw.inherited_from_unique_process_id as u32
    }

    /// Creation time as a FILETIME; with the PID, identifies the process.
    pub fn create_time(&self) -> i64 {
        self.raw.create_time
    }

    /// User plus kernel time, in 100 ns units.
    pub fn cpu_time(&self) -> u64 {
        (self.raw.user_time.max(0) + self.raw.kernel_time.max(0)) as u64
    }

    pub fn threads(&self) -> u32 {
        self.raw.number_of_threads
    }

    /// Private bytes (commit charge).
    pub fn private_bytes(&self) -> u64 {
        self.raw.private_page_count as u64
    }

    /// Lifetime bytes read and written, across all I/O (not only disk).
    pub fn io_bytes(&self) -> (u64, u64) {
        (
            self.raw.read_transfer_count.max(0) as u64,
            self.raw.write_transfer_count.max(0) as u64,
        )
    }

    pub fn name(&self) -> String {
        // SAFETY: the name buffer lives inside the same snapshot as `raw`.
        unsafe { self.raw.image_name.to_string_lossy() }
    }
}

/// Full image path and command line of a process, read once when it is first
/// seen. Either is empty when the process is protected or already gone.
pub fn image_details(pid: u32) -> (String, String) {
    let Ok(handle) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else {
        return (String::new(), String::new());
    };

    let executable = image_path(handle);

    // Windows 8.1+ hands the command line over directly, without reading the
    // target's memory, and only needs limited query access.
    let mut buf: Vec<u64> = vec![0; 512];
    let command_line = query_growing(&mut buf, |buffer, length, needed| unsafe {
        NtQueryInformationProcess(
            handle,
            PROCESS_COMMAND_LINE_INFORMATION,
            buffer,
            length,
            needed,
        )
    })
    .filter(|&written| written >= std::mem::size_of::<UnicodeString>())
    // SAFETY: the kernel put a UNICODE_STRING pointing into `buf` at its start.
    .map(|_| unsafe { (*buf.as_ptr().cast::<UnicodeString>()).to_string_lossy() })
    .unwrap_or_default();

    unsafe {
        let _ = CloseHandle(handle);
    }
    (executable, command_line)
}

fn image_path(handle: HANDLE) -> String {
    // Most paths fit the first buffer; long paths get a second try.
    for capacity in [1024usize, 32 * 1024] {
        let mut path = vec![0u16; capacity];
        let mut size = capacity as u32;
        let ok = unsafe {
            QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_WIN32,
                PWSTR(path.as_mut_ptr()),
                &mut size,
            )
        }
        .is_ok();
        if ok {
            return String::from_utf16_lossy(&path[..size as usize]);
        }
    }
    String::new()
}
