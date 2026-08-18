//! Snapshot stubs
//!
//! Boa has no equivalent of the V8 startup snapshot, so the runner gets the
//! same module surface as the other runtimes but no working implementation.

pub struct SnapshotOutput {
    pub output: Vec<u8>,
}

pub fn create_runtime_snapshot() -> Result<SnapshotOutput, String> {
    Err("Boa runtime does not support snapshots".to_string())
}
