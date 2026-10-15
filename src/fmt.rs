//! Human friendly formatting helpers for the process table.

const UNITS: [&str; 7] = ["B", "KB", "MB", "GB", "TB", "PB", "EB"];

/// Pretty-print an absolute byte count, e.g. `12.4 MB`.
pub fn bytes(v: u64) -> String {
    let mut unit = 0;
    let mut scaled = v as f64;
    while scaled >= 1024.0 && unit < UNITS.len() - 1 {
        scaled /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{v} {}", UNITS[0])
    } else {
        format!("{scaled:.2} {}", UNITS[unit])
    }
}

/// Pretty-print a rate in bytes per second, e.g. `1.24 MB/s`.
pub fn rate(bytes_per_second: f64) -> String {
    if bytes_per_second < 1.0 {
        return "0 B/s".to_string();
    }
    format!("{}/s", bytes(bytes_per_second.round() as u64))
}

/// A percentage with a fixed number of decimals.
pub fn pct(v: f64, decimals: usize) -> String {
    if !v.is_finite() {
        return "0".to_string();
    }
    format!("{v:.decimals$}%")
}
