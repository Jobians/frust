//! The navigator's two thread-local ambient scopes: the per-page
//! **back-reach** cell a nested navigator learns its hosting page's input
//! reachability through (rule **R23**), and the per-navigator **swipe-claim**
//! cell an inner navigator reports an edge-swipe arm into (**R-B3-inner**).
//!
//! Both seams and their composition rules are documented in
//! [`navigator`](super::navigator)'s module docs; this file holds only the
//! thread-locals, their drop guards, and the scope/read helpers.

use std::cell::Cell;
use std::rc::Rc;

use crate::thread_state;

// --- Back reach: the ambient "is the hosting page input-routed?" seam --------

/// Pops `page_reach` on drop so an unwinding page builder cannot leave a stale
/// scope behind for the rest of the thread's life.
struct PageReachGuard;

impl Drop for PageReachGuard {
    fn drop(&mut self) {
        let _ = thread_state::try_with(|locals| {
            locals.page_reach.borrow_mut().pop();
        });
    }
}

/// Run `f` with `reach` installed as the ambient page-reach cell — i.e. declare
/// "everything built in here lives on the page this cell describes".
///
/// The `page_reach` borrow is released *before* `f` runs, so `f` may nest
/// another `with_page_reach` (a navigator inside a page inside a navigator) or
/// call [`ambient_page_reach`] freely.
pub(crate) fn with_page_reach<R>(reach: &Rc<Cell<bool>>, f: impl FnOnce() -> R) -> R {
    thread_state::with(|locals| locals.page_reach.borrow_mut().push(Rc::clone(reach)));
    let _guard = PageReachGuard;
    f()
}

/// The reach cell of the page currently being built/reconciled, or `None` at the
/// top level (a root navigator, whose reach is unconditional).
pub(crate) fn ambient_page_reach() -> Option<Rc<Cell<bool>>> {
    thread_state::with(|locals| locals.page_reach.borrow().last().cloned())
}

// --- R-B3-inner: the ambient "did anything below me arm an edge-swipe on this
// Down?" seam ------------------------------------------------------------

/// Pops `swipe_claim` on drop, mirroring [`PageReachGuard`].
struct SwipeClaimGuard;

impl Drop for SwipeClaimGuard {
    fn drop(&mut self) {
        let _ = thread_state::try_with(|locals| {
            locals.swipe_claim.borrow_mut().pop();
        });
    }
}

/// Run `f` (a `Down` forward through [`route_top`](NavigatorWidget::route_top))
/// with `claim` installed as the ambient swipe-claim cell, so any navigator
/// reached underneath can report "I armed" into it via [`ambient_swipe_claim`].
///
/// The borrow is released before `f` runs, so `f` may itself nest another
/// `with_swipe_claim` call (a third level of navigator nesting).
pub(super) fn with_swipe_claim<R>(claim: &Rc<Cell<bool>>, f: impl FnOnce() -> R) -> R {
    thread_state::with(|locals| locals.swipe_claim.borrow_mut().push(Rc::clone(claim)));
    let _guard = SwipeClaimGuard;
    f()
}

/// The swipe-claim cell of the navigator currently forwarding a `Down` through
/// [`route_top`](NavigatorWidget::route_top), or `None` if no navigator is (a
/// top-level navigator's `Down` handling, or any non-`Down` event).
pub(super) fn ambient_swipe_claim() -> Option<Rc<Cell<bool>>> {
    thread_state::with(|locals| locals.swipe_claim.borrow().last().cloned())
}

/// Whether a navigator hosted under `host_reach` is itself reachable: `true` at
/// the top level (no hosting page), otherwise whatever the hosting page's cell
/// currently says.
pub(super) fn host_reachable(host_reach: Option<&Rc<Cell<bool>>>) -> bool {
    host_reach.is_none_or(|cell| cell.get())
}
