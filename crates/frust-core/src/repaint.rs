//! The process-wide repaint request: any thread says "the next frame must repaint".
//!
//! A request means repaint and nothing else. It never implies a rebuild, a layout or a frame
//! loop; it advances a monotonic generation that every live [`RenderRoot`](crate::RenderRoot)
//! compares in [`RenderRoot::paint`](crate::RenderRoot::paint), so each root repaints its whole
//! tree once (retained pages included) and republishes semantics. A shell reads
//! [`RenderRoot::repaint_pending`](crate::RenderRoot::repaint_pending) while gathering frame
//! inputs and drains [`take_repaint_owed`], whose edge re-arms the wake [`set_repaint_hook`]
//! installed.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// The wake a shell installs: run on the requesting thread, it only posts to the loop.
pub type RepaintHook = Arc<dyn Fn() + Send + Sync>;

static GENERATION: AtomicU64 = AtomicU64::new(0);
static OWED: AtomicBool = AtomicBool::new(false);
static HOOK: Mutex<Option<RepaintHook>> = Mutex::new(None);

/// The installed hook, cloned out so it runs with the lock released (a re-entrant hook is safe).
fn hook() -> Option<RepaintHook> {
    HOOK.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// Ask every live root to repaint on its next frame. Callable from any thread and never
/// blocks on the UI thread: it advances the generation, then runs the hook only on the
/// not-owed to owed edge, so any number of requests before a drain post one wake.
pub fn request_repaint() {
    GENERATION.fetch_add(1, Ordering::AcqRel);
    if !OWED.swap(true, Ordering::AcqRel)
        && let Some(hook) = hook()
    {
        hook();
    }
}

/// The current repaint generation; it moves on every [`request_repaint`].
pub fn repaint_generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

/// Clear and return the owed flag: `true` once per debt, then `false`. The shell's drain;
/// it re-arms the hook edge.
pub fn take_repaint_owed() -> bool {
    OWED.swap(false, Ordering::AcqRel)
}

/// Install `hook` (`None` clears it), returning the hook it replaced. A debt already owed
/// when a hook is installed runs it at once, so a request made before the shell was ready
/// is not stranded.
pub fn set_repaint_hook(hook: Option<RepaintHook>) -> Option<RepaintHook> {
    let replaced = std::mem::replace(
        &mut *HOOK.lock().unwrap_or_else(PoisonError::into_inner),
        hook,
    );
    if OWED.load(Ordering::Acquire)
        && let Some(hook) = self::hook()
    {
        hook();
    }
    replaced
}
