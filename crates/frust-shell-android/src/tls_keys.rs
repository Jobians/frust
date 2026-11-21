//! Debug-only pthread key diagnostics for Android builds.
//!
//! When an app runs out of pthread keys, `std` aborts with a message on stderr, which Android does
//! not log: the developer sees a bare SIGABRT. This module makes a debug build say how many keys
//! are in use and how many are still free, twice per launch: [`log_at_startup`] (from
//! `JNI_OnLoad`) and [`log_after_first_present`] (once, after the first frame is presented).
//!
//! # How the count is taken
//!
//! The count asks bionic, slot by slot, whether each key is allocated, without creating keys to
//! fill the table. It relies on two bionic implementation details, which is why it is compiled for
//! debug builds only:
//!
//! - a key is its slot index with a validity flag in the top bit ([`KEY_VALID_FLAG`]), so the key
//!   for slot `i` can be formed without having created it;
//! - for a slot that is not allocated, `pthread_getspecific` returns NULL and
//!   `pthread_setspecific` returns EINVAL; for an allocated slot `pthread_setspecific` returns 0.
//!   Slots past the end of the table answer EINVAL as well, so the scan covers [`SCAN_SLOTS`],
//!   more than the table holds.
//!
//! Per slot the scan reads first: a non-null value means allocated, and nothing is written. Only
//! on NULL does it call `pthread_setspecific(key, NULL)`, which changes no value (it writes NULL
//! over NULL, even if the slot was deleted and re-created in between). It never writes a non-null
//! value and never writes back a value it read.
//!
//! The flag layout is hard-coded rather than derived, and checked at run time: the scan creates
//! ONE key, checks that it carries the flag and an index inside the scanned range, and deletes it
//! before scanning. Holding one key for that instant cannot make another thread's key creation
//! fail unless the table is already completely full, in which case the create fails, the line
//! says zero keys are free, and no scan runs. A key without the expected layout makes the line
//! report that instead of a figure.
//!
//! The scan is not atomic: a key created or deleted on another thread while it runs can make the
//! count off by one.

use std::sync::atomic::{AtomicBool, Ordering};

/// bionic's validity flag on a `pthread_key_t`: the top bit, above the slot index.
const KEY_VALID_FLAG: u32 = 1 << 31;

/// Slot indices the scan covers: bionic's table with margin (indices past it answer as free).
const SCAN_SLOTS: u32 = 256;

/// Size of bionic's key table: the application limit `PTHREAD_KEYS_MAX` (128) plus the slots libc
/// reserves for itself. Measured on Android 15 (API 35, OnePlus 9) as the scan's in-use count plus
/// the keys still creatable = 130; the line logs the in-use figure too, so a different capacity on
/// another Android version is visible.
const KEY_CAPACITY: usize = 130;

/// Free-key count below which the post-first-frame line is a warning: headroom for the keys an app
/// still creates lazily after the first frame (on the reference phone about 20 more are taken
/// between the first frame and a full walk of the demo).
const WARN_BELOW: usize = 24;

/// Once-only latch for [`log_after_first_present`].
static FIRST_PRESENT_LOGGED: AtomicBool = AtomicBool::new(false);

/// What one key count found.
enum Census {
    /// The number of allocated slots.
    InUse(usize),
    /// The one verification key could not be created: no key is free.
    Full,
    /// The verification key does not have bionic's layout; carries that key.
    UnexpectedKey(libc::pthread_key_t),
}

/// Whether the slot behind `key` is allocated, asked without changing any key's value.
fn slot_in_use(key: libc::pthread_key_t) -> bool {
    // SAFETY: bionic range-checks the key and returns NULL for one that is not allocated.
    if !unsafe { libc::pthread_getspecific(key) }.is_null() {
        return true;
    }
    // SAFETY: bionic range-checks the key and returns EINVAL for one that is not allocated;
    // writing NULL over this thread's NULL value changes nothing.
    unsafe { libc::pthread_setspecific(key, std::ptr::null()) == 0 }
}

/// Count the allocated pthread keys (see the module comment for how).
fn census() -> Census {
    let mut probe: libc::pthread_key_t = 0;
    // SAFETY: `probe` is a valid out-pointer and no destructor is registered.
    if unsafe { libc::pthread_key_create(&mut probe, None) } != 0 {
        return Census::Full;
    }
    // SAFETY: `probe` was created above and is deleted exactly once, before the scan.
    unsafe { libc::pthread_key_delete(probe) };
    let bits = probe as u32;
    if bits & KEY_VALID_FLAG == 0 || bits & !KEY_VALID_FLAG >= SCAN_SLOTS {
        return Census::UnexpectedKey(probe);
    }
    let in_use = (0..SCAN_SLOTS)
        .filter(|&slot| slot_in_use((KEY_VALID_FLAG | slot) as libc::pthread_key_t))
        .count();
    Census::InUse(in_use)
}

/// The log line for one moment, and the free-key figure when there is one.
fn describe(moment: &str) -> (String, Option<usize>) {
    match census() {
        Census::InUse(in_use) if in_use == 0 || in_use > KEY_CAPACITY => (
            format!(
                "frust: pthread keys {moment}: {in_use} slots answer as in use, outside 1..={KEY_CAPACITY}; \
                 this platform's key layout is not the expected one, no free-key figure"
            ),
            None,
        ),
        Census::InUse(in_use) => {
            let free = KEY_CAPACITY - in_use;
            (
                format!("frust: pthread keys {moment}: {in_use} in use, {free} free"),
                Some(free),
            )
        }
        Census::Full => (
            format!("frust: pthread keys {moment}: 0 free (could not create one key)"),
            Some(0),
        ),
        Census::UnexpectedKey(key) => (
            format!(
                "frust: pthread keys {moment}: new key {key:#x} lacks the expected layout; \
                 this platform's key layout is not the expected one, no free-key figure"
            ),
            None,
        ),
    }
}

/// Log the key counts as early as the shell runs, on the loading thread.
pub(crate) fn log_at_startup() {
    let (line, free) = describe("at startup");
    if free.is_some() {
        log::info!("{line}");
    } else {
        log::warn!("{line}");
    }
}

/// Log the key counts once, after the first frame has been presented.
pub(crate) fn log_after_first_present() {
    if FIRST_PRESENT_LOGGED.swap(true, Ordering::Relaxed) {
        return;
    }
    let (line, free) = describe("after first frame");
    match free {
        Some(free) if free >= WARN_BELOW => log::info!("{line}"),
        Some(_) => log::warn!(
            "{line} (fewer than {WARN_BELOW}; see limitation android-thread-local-key-limit and \
             scripts/ci/tls-key-count.sh)"
        ),
        None => log::warn!("{line}"),
    }
}
