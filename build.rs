//! Embeds the application manifest: Common Controls v6 (used by the emergency
//! window), per-monitor DPI awareness, and the `asInvoker` execution level.
//!
//! Tasky deliberately does not demand elevation in its manifest: that would
//! put a UAC prompt in front of every launch, which is slow exactly when the
//! machine is struggling. It gets elevated through a logon scheduled task
//! instead; see `src/sys/elevation.rs`.

fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let manifest = embed_manifest::new_manifest("Tasky");
        embed_manifest::embed_manifest(manifest).expect("failed to embed the application manifest");
        println!("cargo:rerun-if-changed=build.rs");
    }
}
