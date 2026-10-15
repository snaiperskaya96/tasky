//! Embeds a UAC manifest so Tasky always runs elevated.
//!
//! Tasky terminates and suspends arbitrary processes, and reads the per-process
//! GPU performance counters. Both need an administrator token, so instead of
//! failing half the time we ask for elevation once, at launch.

fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let manifest = embed_manifest::new_manifest("Tasky").requested_execution_level(
            embed_manifest::manifest::ExecutionLevel::RequireAdministrator,
        );

        embed_manifest::embed_manifest(manifest).expect("failed to embed the application manifest");
        println!("cargo:rerun-if-changed=build.rs");
    }
}
