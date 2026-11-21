//! Per-thread rebuild flags a pass deep in a view diff raises for `RenderRoot::rebuild` to drain.
//! Per-thread, not global `AtomicBool`s: the `!Send` callbacks they stand for run only on the
//! queuing thread, so another thread's root must never drain them.

use crate::thread_state;

/// Owe one [`InputEvent::Housekeeping`](crate::event::InputEvent::Housekeeping) broadcast before
/// the frame ends, for a callback queued needing `&mut State`. Idempotent; a no-op during the
/// thread's teardown, where a dropping callback may still call it.
pub fn mark_pending_result_flush() {
    let _ = thread_state::try_with(|t| t.pending_result_flush.set(true));
}

/// Take (and clear) the [`mark_pending_result_flush`] flag; drained by `RenderRoot::rebuild`.
pub fn take_pending_result_flush() -> bool {
    thread_state::with(|t| t.pending_result_flush.replace(false))
}

/// Peek the flag without draining it, for a mobile shell's frame gate deciding whether to run.
pub fn has_pending_result_flush() -> bool {
    thread_state::with(|t| t.pending_result_flush.get())
}

/// Owe a full focus/IME session release: a rebuild severed the live focus path. Raise only when the
/// pod's `focused` flag AND [`BuildCtx::has_focus`](crate::view::BuildCtx::has_focus) hold — a
/// stale deeper flag must never release a field focused elsewhere. Idempotent; a no-op during the
/// thread's teardown.
pub fn mark_focus_orphaned() {
    let _ = thread_state::try_with(|t| t.focus_orphaned.set(true));
}

/// Take (and clear) the [`mark_focus_orphaned`] flag; drained by `RenderRoot::rebuild`.
pub fn take_focus_orphaned() -> bool {
    thread_state::with(|t| t.focus_orphaned.replace(false))
}

/// Owe `root` a hover end; raised from `Drop` by a pod whose stamp names the live link.
pub(crate) fn mark_hover_orphaned(root: u64) {
    thread_state::with(|t| t.hover_orphaned.set(Some(root)));
}

/// Take (and clear) a mark raised for `root`; another root's mark is left standing.
pub(crate) fn take_hover_orphaned(root: u64) -> bool {
    thread_state::with(|t| {
        if t.hover_orphaned.get() == Some(root) {
            t.hover_orphaned.set(None);
            true
        } else {
            false
        }
    })
}

/// Publish `root`'s live hover epoch on this thread (`0` while nothing holds the link).
pub(crate) fn set_live_hover_link(root: u64, epoch: u64) {
    thread_state::with(|t| t.live_hover_link.set((root, epoch)));
}

/// Whether `(root, epoch)` is the live link; epoch `0` never matches.
pub(crate) fn live_hover_link_is(root: u64, epoch: u64) -> bool {
    epoch != 0 && thread_state::with(|t| t.live_hover_link.get()) == (root, epoch)
}
