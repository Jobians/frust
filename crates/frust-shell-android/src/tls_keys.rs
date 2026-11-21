//! Debug-only pthread key diagnostics for Android builds.
//!
//! When an app runs out of pthread keys, `std` aborts with a message on stderr, which Android does
//! not log: the developer sees a bare SIGABRT. This module makes a debug build say how many keys
//! are still free, twice per launch: [`log_at_startup`] (from `JNI_OnLoad`, before the shell
//! spawns any thread) and [`log_after_first_present`] (once, after the first frame is presented).
//!
//! # Hazard
//!
//! [`free_pthread_keys`] creates keys until the table is full, then deletes them all. For that
//! instant the probing thread holds every free key, so a `pthread_key_create` on any other thread
//! fails, and `std` turns that failure into an abort. That is why the probe is compiled for debug
//! builds only, runs exactly twice per launch, and never on a timer. Do not call it from any other
//! place.
//!
//! bionic reuses a deleted slot, so the probe leaves the table as it found it.

use std::sync::atomic::{AtomicBool, Ordering};

/// Upper bound on the probe loop, so a platform with no practical limit cannot spin forever.
const PROBE_CAP: usize = 1024;

/// Free-key count below which the post-first-frame line is a warning.
const WARN_BELOW: usize = 24;

/// Once-only latch for [`log_after_first_present`].
static FIRST_PRESENT_LOGGED: AtomicBool = AtomicBool::new(false);

/// Count the free pthread keys by exhausting and then releasing them (see the module hazard note).
fn free_pthread_keys() -> usize {
    let mut keys: Vec<libc::pthread_key_t> = Vec::with_capacity(PROBE_CAP);
    while keys.len() < PROBE_CAP {
        let mut key: libc::pthread_key_t = 0;
        // SAFETY: `key` is a valid out-pointer and no destructor is registered.
        if unsafe { libc::pthread_key_create(&mut key, None) } != 0 {
            break;
        }
        keys.push(key);
    }
    let count = keys.len();
    for key in keys {
        // SAFETY: every key here was created above and is deleted exactly once.
        unsafe { libc::pthread_key_delete(key) };
    }
    count
}

/// Log the free key count as early as the shell runs, on the loading thread.
pub(crate) fn log_at_startup() {
    log::info!(
        "frust: pthread keys free at startup: {}",
        free_pthread_keys()
    );
}

/// Log the free key count once, after the first frame has been presented.
pub(crate) fn log_after_first_present() {
    if FIRST_PRESENT_LOGGED.swap(true, Ordering::Relaxed) {
        return;
    }
    let free = free_pthread_keys();
    if free < WARN_BELOW {
        log::warn!(
            "frust: pthread keys free after first frame: {free} (fewer than {WARN_BELOW}; see limitation \
             android-thread-local-key-limit and scripts/ci/tls-key-count.sh)"
        );
    } else {
        log::info!("frust: pthread keys free after first frame: {free}");
    }
}
