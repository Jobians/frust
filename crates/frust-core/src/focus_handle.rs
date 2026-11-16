//! Programmatic focus: a [`FocusHandle`] takes, releases and observes a control's focus from app
//! code. `request` and `release` only record an intent (safe from handlers, build, or before the
//! handle is attached); the last call wins. A focusable widget binds the handle every paint with
//! [`PaintCtx::bind_focus_handle`](crate::widget::PaintCtx::bind_focus_handle); the root takes
//! the stop from the pod that painted it. An unbound handle is inert and keeps its request.
//! After a paint that leaves a bound handle with a request, and after any app call that records
//! one, the root grants the stop focus at the next rebuild or end of an event pass (the points a
//! focus return runs at) and asks for the frame to get there. The grant is one traversal pass: a
//! stop the walk does not reach (outside a trapping modal, disabled) refuses it and the request
//! is dropped, never retried. It clears the keyboard flag, replaces another stop's live session
//! as a structural release, and beats an owed focus return. `release` ends the session only if
//! its stop holds it. [`FocusHandle::is_focused`] follows the session however it changed.
//! Focus-keeping: [`EventCtx::keep_focus`](crate::event::EventCtx::keep_focus) on a pointer
//! `Down` arms no blur and makes the root ignore any `request_focus` on that press path, so the
//! live holder and its IME state stay as they were. Call it before forwarding the press; it does
//! nothing without a live session. Notification: when a bound handle's state differs from the
//! one the root last saw, the root records the stop and raises `mark_pending_result_flush`. The
//! next rebuild's housekeeping broadcast then reports
//! [`EventCtx::focus_changed`](crate::event::EventCtx::focus_changed) to that stop's widget, with
//! `has_focus()` read from the live session and `&mut State` in scope, for an app callback. A
//! widget compares with the value it last reported: its own pass's changes are announced too.

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

#[derive(Debug, Default)]
struct HandleState {
    intent: Cell<Intent>,
    focused: Cell<bool>,
    /// The `focused` value last compared; a difference owes a notification.
    seen: Cell<bool>,
}

/// A shared token for taking, releasing and observing one control's focus (see the module docs).
#[derive(Clone, Debug, Default)]
pub struct FocusHandle(Rc<HandleState>);

impl FocusHandle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask for focus on the bound control; applied at the root's next rebuild or event pass end.
    pub fn request(&self) {
        self.0.intent.set(Intent::Request);
        crate::event::mark_pending_result_flush();
    }

    /// Ask to give focus up, honoured only if the bound control holds the live session.
    pub fn release(&self) {
        self.0.intent.set(Intent::Release);
        crate::event::mark_pending_result_flush();
    }

    /// Whether the bound control holds the live focus session as of the root's last refresh.
    pub fn is_focused(&self) -> bool {
        self.0.focused.get()
    }

    pub(crate) fn take_intent(&self) -> Intent {
        self.0.intent.replace(Intent::None)
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
    /// `0` no armed press, `1` a pointer `Down` with a live session, `2` that press kept focus.
    static PRESS: Cell<u8> = const { Cell::new(0) };
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

pub(crate) fn open_press(armed: bool) {
    PRESS.with(|state| state.set(u8::from(armed)));
}

/// Behind `EventCtx::keep_focus`.
pub(crate) fn note_keep_focus() {
    PRESS.with(|state| {
        if state.get() == 1 {
            state.set(2);
        }
    });
}

pub(crate) fn press_kept() -> bool {
    PRESS.with(|state| state.get() == 2)
}

pub(crate) fn close_press() -> bool {
    PRESS.with(|state| state.replace(0) == 2)
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
        open_press(false);
        note_keep_focus();
        assert!(!close_press());
        open_press(true);
        assert!(!press_kept());
        note_keep_focus();
        assert!(press_kept());
        assert!(close_press());
        assert!(!press_kept());
    }
}
