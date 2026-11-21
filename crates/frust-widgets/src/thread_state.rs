//! Every per-thread ambient stack frust-widgets keeps, as the fields of one [`WidgetLocals`]
//! behind one `thread_local!` static: on Android each static takes one of 128 process-wide
//! pthread keys, and a field costs per access what its own static did.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::scroll::InnerScrollState;
use crate::slivers::keep_alive::Requests;
use crate::viewport::AxisFrame;

/// One stack per field, each in its own `RefCell`; the owning modules keep the accessors. No
/// outer cell wraps it, so nested access to two stacks behaves as nested access to two statics.
/// The stacks hold `Rc` values that only own plain cells or frames of them, so dropping them at
/// thread exit never reaches back into a thread-local.
pub(crate) struct WidgetLocals {
    /// The stack of per-page **back-reach** cells for the page builders currently
    /// on the call stack — the ambient seam a *nested* navigator learns its
    /// hosting page's input reachability through (rule **R23**: navigator reach
    /// follows input routing, exactly).
    ///
    /// A navigator pushes the reach cell of the page whose builder / reconcile it
    /// is about to run (`with_page_reach`) and pops it again afterwards, so any
    /// navigator built anywhere inside that page's subtree — at any depth, through
    /// any container — reads it with `ambient_page_reach`. A stack rather than a
    /// single slot because navigators nest: each level restores its parent's cell.
    ///
    /// Reactive-free by construction (`frust-widgets` carries no `reactive_graph`
    /// dependency): a plain `Rc<Cell<bool>>`, the same idiom as `depth`/
    /// `back_interest`/`transition`, never a signal. UI-thread-affine for the same
    /// reason those are — the cells are `Rc`-backed and only ever touched from a
    /// build pass.
    ///
    /// Two widgets install reach cells. The navigator installs one per page.
    /// `IndexedStack` is the second installer: it keeps
    /// every child mounted but routes input to the selected one only, so it
    /// installs one cell per child, `true` only for the selected child, and
    /// only while the stack's own ambient parent cell (if any) is `true`. A
    /// navigator nested in an unselected child therefore claims no back press,
    /// which is the same rule as for a navigator on a covered page. Both
    /// installers stay inside this crate: a public reach scope could declare
    /// "no reach" while its children still received input, which would break
    /// the input/back parity R23 requires.
    pub(crate) page_reach: RefCell<Vec<Rc<Cell<bool>>>>,

    /// The stack of per-navigator **swipe-claim** cells for the navigators
    /// currently routing a left-edge `Down` (`R-B3-inner`, mirrors
    /// `page_reach`/`with_page_reach`).
    ///
    /// A left-edge `Down` does not capture: the outer navigator only records
    /// `edge.armed` and forwards it through `route_top`
    /// (children see `Down` first), so a nested navigator's own `event_at` runs
    /// underneath — and may *also* arm. Preemption is not decided at `Down`; it
    /// is decided later, at the decisive `Move` steal site, where the OUTER
    /// navigator — upstream in parent→child routing — reaches its steal branch
    /// before the inner ever sees the event. So a navigator pushes its own
    /// claim cell before forwarding `Down` (`with_swipe_claim`), reads it back
    /// once forwarding returns (`edge.inner_claimed`), and propagates the
    /// combined result into whatever cell is now ambient
    /// (`ambient_swipe_claim`) — its own host navigator's, if any — so a
    /// third nesting level defers too.
    ///
    /// A stack (not a single slot) for the same reason `page_reach` is one:
    /// navigators nest, and each level must see its own immediate host's cell,
    /// not some ancestor's.
    pub(crate) swipe_claim: RefCell<Vec<Rc<Cell<bool>>>>,

    /// The rows currently being built or rebuilt, innermost last — the scopes
    /// a `KeepAliveHandle` binds to. UI-thread-affine like the build pass.
    pub(crate) row_scopes: RefCell<Vec<Requests>>,

    /// The stack of per-surface **inner-scroll claim** cells for the scroll
    /// surfaces currently forwarding a pointer `Down` — the seam a nested
    /// scrollable reports itself to its nearest enclosing one through
    /// (mirrors `nav::ambient`'s `swipe_claim`/`with_swipe_claim`).
    ///
    /// A `Down` does not decide anything: the outer surface captures, forwards
    /// it, and only at the later `Move` slop does it choose between taking the
    /// drag over and deferring — by which point dispatch order has already put
    /// it upstream of the inner. So the outer pushes a fresh cell before
    /// forwarding the `Down` (`with_scroll_claim`), the nested surface writes
    /// its [`InnerScrollState`] into whatever cell is ambient
    /// (`ambient_scroll_claim`) as that `Down` reaches it, and the outer
    /// reads it back once forwarding returns.
    ///
    /// A **stack**, not a single slot, because the pairing must be
    /// *nearest*-inner: a surface writes its own state into the ambient cell
    /// **before** pushing its own cell for its own children, so its write lands
    /// in its enclosing surface's cell while everything deeper lands in its
    /// own. Three levels deep, the outermost therefore learns only about the
    /// middle surface and the middle only about the innermost — nothing
    /// propagates a grandchild's claim up past its own parent, which is what
    /// makes each layer arbitrate against the layer it actually contains.
    ///
    /// Reactive-free by construction (`frust-widgets` carries no
    /// `reactive_graph` dependency): a plain `Rc<Cell<_>>`, never a signal, and
    /// UI-thread-affine for the same reason `nav::ambient`'s cells are.
    pub(crate) scroll_claim: RefCell<Vec<Rc<Cell<InnerScrollState>>>>,

    /// The stack of per-surface **multi-contact veto** cells for the scroll
    /// surfaces currently forwarding a pointer `Down` — `scroll_claim`'s
    /// live counterpart (the module docs' *Multi-contact veto*). A nested
    /// recognizer that opts into the gesture's other contacts
    /// (`pinch_detector`, `pan_zoom`'s child-owned-press branch)
    /// captures the ambient cell on the claimant's `Down`, while it is still
    /// reachable, and keeps writing to it for the rest of the gesture — long
    /// after the `Down` forward that exposed it has returned, which is
    /// exactly what a `Down`-time snapshot like [`InnerScrollState`] cannot
    /// do (it is read back once, synchronously, before a second contact can
    /// possibly have arrived).
    ///
    /// A separate stack from `scroll_claim` rather than a field folded into
    /// it: `InnerScrollState` stays `Copy` (and byte-identical) for
    /// `ListViewWidget`'s existing consumption of it, and
    /// a plain `Rc<Cell<bool>>` is exactly as `Copy`-free a value as this
    /// seam needs.
    pub(crate) multi_contact_veto: RefCell<Vec<Rc<Cell<bool>>>>,

    /// The viewports currently forwarding a primary `Down`, innermost last —
    /// what lets a nested viewport learn its enclosing surface's axis (the
    /// module docs' *Axis*). UI-thread-affine like the claim stack it shadows.
    pub(crate) viewport_frames: RefCell<Vec<Rc<AxisFrame>>>,
}

impl WidgetLocals {
    /// Every stack empty, as each static started.
    const fn new() -> Self {
        Self {
            page_reach: RefCell::new(Vec::new()),
            swipe_claim: RefCell::new(Vec::new()),
            row_scopes: RefCell::new(Vec::new()),
            scroll_claim: RefCell::new(Vec::new()),
            multi_contact_veto: RefCell::new(Vec::new()),
            viewport_frames: RefCell::new(Vec::new()),
        }
    }
}

thread_local! {
    static WIDGETS: WidgetLocals = const { WidgetLocals::new() };
}

/// Run `f` on this thread's state; panics once the thread's teardown has destroyed it.
pub(crate) fn with<R>(f: impl FnOnce(&WidgetLocals) -> R) -> R {
    WIDGETS.with(f)
}

/// Run `f` on this thread's state, or return `None` once the thread's teardown has destroyed it.
pub(crate) fn try_with<R>(f: impl FnOnce(&WidgetLocals) -> R) -> Option<R> {
    WIDGETS.try_with(f).ok()
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;
    use std::thread;

    use frust_core::ScrollAxis;

    use super::with;
    use crate::scroll::InnerScrollState;
    use crate::viewport::AxisFrame;

    #[test]
    fn every_stack_survives_thread_exit_holding_values() {
        thread::spawn(|| {
            with(|l| {
                l.page_reach.borrow_mut().push(Rc::new(Cell::new(true)));
                l.swipe_claim.borrow_mut().push(Rc::new(Cell::new(true)));
                l.row_scopes.borrow_mut().push(Rc::new(Cell::new(1)));
                let claim = Rc::new(Cell::new(InnerScrollState::default()));
                l.scroll_claim.borrow_mut().push(Rc::clone(&claim));
                l.multi_contact_veto
                    .borrow_mut()
                    .push(Rc::new(Cell::new(true)));
                l.viewport_frames.borrow_mut().push(Rc::new(AxisFrame {
                    claim,
                    axis: ScrollAxis::Vertical,
                    inner_axis: Cell::new(None),
                }));
            });
        })
        .join()
        .expect("thread exits without a panic");
    }
}
