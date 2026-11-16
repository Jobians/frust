//! Programmatic focus: a [`FocusHandle`] takes, releases and observes a control's focus from app
//! code. `request` and `release` only record an intent (safe from handlers, build, or before the
//! handle is attached); the last call wins. A focusable widget binds the handle every paint with
//! [`PaintCtx::bind_focus_handle`](crate::widget::PaintCtx::bind_focus_handle); the root takes
//! the stop from the pod that painted it. An unbound handle is inert and keeps its request.
//! The root grants a request at the next rebuild or end of an event pass (where a focus return
//! runs) and asks for the frame to get there. The grant is one traversal pass: a stop the walk
//! does not reach (outside a trapping modal, disabled) refuses it. A request made in an event
//! handler is judged against the tree its state produces: the pass end keeps it while the stop
//! is out of reach, and it is dropped if still out of reach at the first rebuild after a paint.
//! Any other request is dropped at once. A grant clears the keyboard flag, replaces another
//! stop's live session as a structural release, and beats an owed focus return. `release` ends
//! the session only if its stop holds it. [`FocusHandle::is_focused`] follows the session.
//! Focus-keeping: [`EventCtx::keep_focus`](crate::event::EventCtx::keep_focus) on a pointer
//! `Down` arms no blur and makes the root ignore any `request_focus` on that press path; call it
//! before forwarding the press. Notification: when a bound handle's state differs from the
//! one the root last saw, the next rebuild's housekeeping broadcast reports
//! [`EventCtx::focus_changed`](crate::event::EventCtx::focus_changed) to that stop's widget,
//! with `has_focus()` read from the live session and `&mut State` in scope. The notice needs
//! every ancestor to forward the broadcast, and a floated surface's owner to forward it inward
//! while [`EventCtx::focus_notice_open`](crate::event::EventCtx::focus_notice_open). A handle's
//! stop holds the session only when it is the holder itself, not an ancestor of it.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::event::PassBracket;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Intent {
    #[default]
    None,
    Request,
    Release,
}

/// How far a request made in an event pass has got toward its final judgement.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Deferral {
    /// Not deferred: an unreachable stop drops the request at a rebuild.
    #[default]
    Fresh,
    /// The stop was out of reach at an event pass end; no paint has followed.
    Awaiting,
    /// A paint followed the deferral; the next unreachable verdict drops the request.
    Painted,
}

#[derive(Debug, Default)]
struct HandleState {
    intent: Cell<Intent>,
    deferral: Cell<Deferral>,
    focused: Cell<bool>,
    /// The `focused` value last compared; a difference owes a notification.
    seen: Cell<bool>,
}

/// A shared token for taking, releasing and observing one control's focus (see the module docs).
///
/// One handle addresses one field in one root. A request on a handle that is never bound stays
/// pending until a field binds it, so release it when it is no longer wanted. A focus callback
/// must not unconditionally request focus on another handle. The root processes handles in bind
/// order, so a later grant can replace an earlier one in the same pass.
#[derive(Clone, Debug, Default)]
pub struct FocusHandle(Rc<HandleState>);

impl FocusHandle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask for focus on the bound control; applied at the root's next rebuild or event pass end.
    pub fn request(&self) {
        self.0.intent.set(Intent::Request);
        self.0.deferral.set(Deferral::Fresh);
        crate::event::mark_pending_result_flush();
    }

    /// Ask to give focus up, honoured only if the bound control holds the live session.
    pub fn release(&self) {
        self.0.intent.set(Intent::Release);
        self.0.deferral.set(Deferral::Fresh);
        crate::event::mark_pending_result_flush();
    }

    /// Whether the bound control holds the live focus session as of the root's last refresh.
    pub fn is_focused(&self) -> bool {
        self.0.focused.get()
    }

    pub(crate) fn take_intent(&self) -> Intent {
        self.0.deferral.set(Deferral::Fresh);
        self.0.intent.replace(Intent::None)
    }

    pub(crate) fn deferral(&self) -> Deferral {
        self.0.deferral.get()
    }

    /// Keep the pending intent past an event pass end whose walk did not reach the stop.
    pub(crate) fn defer(&self) {
        if self.0.deferral.get() == Deferral::Fresh {
            self.0.deferral.set(Deferral::Awaiting);
        }
    }

    /// A paint ran with the intent still pending.
    pub(crate) fn note_painted(&self) {
        if self.0.deferral.get() == Deferral::Awaiting {
            self.0.deferral.set(Deferral::Painted);
        }
    }

    pub(crate) fn has_pending(&self) -> bool {
        self.0.intent.get() != Intent::None
    }

    pub(crate) fn set_focused(&self, focused: bool) {
        self.0.focused.set(focused);
    }

    pub(crate) fn note_seen(&self, focused: bool) -> bool {
        self.0.seen.replace(focused) != focused
    }

    pub(crate) fn same(&self, other: &FocusHandle) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

/// A bound handle with its pod's traversal stop (`inspect_id`, `0` for the root widget).
#[derive(Clone, Debug)]
pub(crate) struct BoundHandle {
    pub(crate) handle: FocusHandle,
    pub(crate) stop: u64,
}

struct Binding {
    handle: FocusHandle,
    stop: Option<u64>,
}

thread_local! {
    static BINDINGS: Cell<Vec<Binding>> = const { Cell::new(Vec::new()) };
    static BIND_PASS_OPEN: Cell<bool> = const { Cell::new(false) };
    /// The stops named by the open notification broadcast.
    static NOTICE: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
    static PRESS: Cell<Press> = const { Cell::new(Press::Idle) };
}

/// The keep-focus state of the pointer `Down` being dispatched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Press {
    Idle,
    /// A pointer `Down` with a live session: a keep may land.
    Armed,
    Kept,
}

pub(crate) struct FocusBindPass(PassBracket<Vec<Binding>>);

impl FocusBindPass {
    pub(crate) fn enter() -> Self {
        FocusBindPass(PassBracket::enter(&BINDINGS, &BIND_PASS_OPEN))
    }

    pub(crate) fn take(&self) -> Vec<BoundHandle> {
        self.0
            .take()
            .into_iter()
            .filter_map(|binding| {
                Some(BoundHandle {
                    handle: binding.handle,
                    stop: binding.stop?,
                })
            })
            .collect()
    }
}

pub(crate) fn bind(handle: &FocusHandle) {
    BINDINGS.with(|cell| {
        let mut bindings = cell.take();
        bindings.push(Binding {
            handle: handle.clone(),
            stop: None,
        });
        cell.set(bindings);
    });
}

pub(crate) fn bind_mark() -> usize {
    BINDINGS.with(|cell| {
        let bindings = cell.take();
        let len = bindings.len();
        cell.set(bindings);
        len
    })
}

/// Give each unresolved binding made since `mark` the painting pod's stop; nested pods resolve
/// their own first, and `stop` runs only if needed.
pub(crate) fn resolve_binds(mark: usize, stop: impl FnOnce() -> u64) {
    BINDINGS.with(|cell| {
        let mut bindings = cell.take();
        if bindings.len() > mark {
            let mut stop = Some(stop);
            let mut id = None;
            for binding in &mut bindings[mark..] {
                if binding.stop.is_none() {
                    let id = *id.get_or_insert_with(|| (stop.take().expect("called once"))());
                    binding.stop = Some(id);
                }
            }
        }
        cell.set(bindings);
    });
}

pub(crate) struct FocusNoticePass {
    previous: Vec<u64>,
}

impl FocusNoticePass {
    pub(crate) fn enter(stops: Vec<u64>) -> Self {
        let previous = NOTICE.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), stops));
        FocusNoticePass { previous }
    }
}

impl Drop for FocusNoticePass {
    fn drop(&mut self) {
        let previous = std::mem::take(&mut self.previous);
        NOTICE.with(|slot| *slot.borrow_mut() = previous);
    }
}

pub(crate) fn notice_names(stop: u64) -> bool {
    NOTICE.with(|slot| slot.borrow().contains(&stop))
}

/// Brackets one root dispatch's press state; dropping it disarms the press.
pub(crate) struct PressPass(());

impl PressPass {
    pub(crate) fn open(armed: bool) -> Self {
        PRESS.with(|state| state.set(if armed { Press::Armed } else { Press::Idle }));
        PressPass(())
    }

    /// Whether the press kept focus, read before the guard drops.
    pub(crate) fn kept(&self) -> bool {
        press_kept()
    }
}

impl Drop for PressPass {
    fn drop(&mut self) {
        PRESS.with(|state| state.set(Press::Idle));
    }
}

/// Behind `EventCtx::keep_focus`.
pub(crate) fn note_keep_focus() {
    PRESS.with(|state| {
        if state.get() == Press::Armed {
            state.set(Press::Kept);
        }
    });
}

pub(crate) fn press_kept() -> bool {
    PRESS.with(|state| state.get() == Press::Kept)
}

/// Whether a deferred focus notice is open (see `FocusNoticePass`).
pub(crate) fn notice_open() -> bool {
    NOTICE.with(|slot| !slot.borrow().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handle_records_the_last_intent_and_clones_share_state() {
        let handle = FocusHandle::new();
        let clone = handle.clone();
        assert!(!handle.has_pending());
        handle.request();
        clone.release();
        assert_eq!(handle.take_intent(), Intent::Release, "the last call wins");
        assert!(!clone.has_pending(), "taken once");
        assert!(handle.same(&clone));
        assert!(!handle.same(&FocusHandle::new()));
        let _ = crate::event::take_pending_result_flush();
    }

    #[test]
    fn bindings_resolve_to_the_innermost_pod_that_painted_them() {
        let pass = FocusBindPass::enter();
        let (outer, inner) = (FocusHandle::new(), FocusHandle::new());
        let outer_mark = bind_mark();
        bind(&outer);
        let inner_mark = bind_mark();
        bind(&inner);
        resolve_binds(inner_mark, || 7);
        resolve_binds(outer_mark, || 3);
        let bound = pass.take();
        assert_eq!(bound.len(), 2);
        assert_eq!((bound[0].stop, bound[1].stop), (3, 7));
    }

    #[test]
    fn a_press_keeps_focus_only_when_armed() {
        {
            let press = PressPass::open(false);
            note_keep_focus();
            assert!(!press.kept());
        }
        let press = PressPass::open(true);
        assert!(!press.kept());
        note_keep_focus();
        assert!(press.kept());
        drop(press);
        assert!(!press_kept(), "dropping the guard disarms the press");
    }
}
