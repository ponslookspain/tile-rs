//! Opt-in profiling, enabled with the `TILE_RS_PROFILE` environment variable.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Set once at startup from `TILE_RS_PROFILE=1`. When off, profiling costs one bool load.
pub static PROFILE: AtomicBool = AtomicBool::new(false);

/// How many times `collect()` (a full `EnumWindows` walk) ran.
pub static COLLECT_CALLS: AtomicU32 = AtomicU32::new(0);

pub fn on() -> bool {
    PROFILE.load(Ordering::Relaxed)
}
