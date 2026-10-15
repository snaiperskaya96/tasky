//! Windows-only platform glue.
//!
//! Everything in here talks to Win32 / PDH directly. The rest of the app only
//! ever sees plain Rust types from these modules.

pub mod control;
pub mod elevation;
pub mod gpu;
pub mod hotkey;
pub mod icon;
pub mod instance;
pub mod machine;
pub mod priority;
pub mod procs;
pub mod tray;
