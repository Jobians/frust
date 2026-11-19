//! The signal-to-repaint bridge: [`repaint_on`] turns a change to the signals a closure reads
//! into a core [`request_repaint`](frust_core::request_repaint), never a rebuild.
//!
//! It lives in the facade because it is the one place that sees both `frust-reactive`'s
//! [`RepaintScope`] and `frust-core`'s repaint request; neither crate depends on the other. The
//! scope stays subscribed across notifications (see `frust_reactive::RepaintScope`), so every
//! later change repaints with no per-frame re-track.

use std::sync::Arc;

use frust_reactive::RepaintScope;

/// The owned guard [`repaint_on`] returns; dropping it stops the bridge and unsubscribes it.
///
/// Not `Send`: keep it in the component's state on the UI thread.
#[must_use = "dropping a RepaintBinding stops it at once"]
pub struct RepaintBinding {
    /// Declared first, so it unsubscribes before the closure it tracked is dropped.
    scope: RepaintScope,
    read: Box<dyn Fn()>,
}

/// Repaint (without rebuild or layout) whenever a signal `read` reads changes.
///
/// `read` runs once now, on this thread, with the binding as its observer; its value is
/// discarded, since the closure is for subscription only. Its reads do not subscribe any
/// enclosing rebuild. A write to any signal it read, from any thread, calls
/// [`request_repaint`](crate::request_repaint), and so does every later write.
///
/// Create the binding in `Component::init` (not `build`, which would re-subscribe on every
/// rebuild) and keep it in the component's state, or move it into an `on_cleanup` registered
/// under the component's owner. A root component is never torn down, so a binding in its state
/// lives for the process. When the set of signals `read` touches can change, call
/// [`RepaintBinding::retrack`].
pub fn repaint_on<T>(read: impl Fn() -> T + 'static) -> RepaintBinding {
    let binding = RepaintBinding {
        scope: RepaintScope::every_change(Arc::new(frust_core::request_repaint)),
        read: Box::new(move || {
            let _ = read();
        }),
    };
    binding.retrack();
    binding
}

impl RepaintBinding {
    /// Re-run `read` on this thread and subscribe to exactly the signals it reads now.
    ///
    /// Needed only when that set can differ from the last read (a branch on another signal, a
    /// trigger, which unsubscribes on notify); a plain signal or memo stays subscribed.
    pub fn retrack(&self) {
        self.scope.track(&*self.read);
    }
}
