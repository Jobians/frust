//! Focus-return scopes: when a modal surface closes, keyboard focus goes back to the control that
//! held it as the surface opened (the WAI-ARIA dialog pattern). On by default.
//!
//! A widget that owns focus while visible allocates one [`FocusScopeKey`] per instance and
//! registers it every paint it is open
//! ([`PaintCtx::register_focus_scope`](crate::widget::PaintCtx::register_focus_scope)); the paint
//! it stops in is its close. An Interactive `OverlayKeys::Modal` overlay entry is a scope too.
//!
//! The root keeps the last committed focus target, the deepest traversal stop on the session a
//! claim or Tab grant committed plus the keyboard flag. A user blur (a tap on nothing, a widget's
//! `EventCtx::release_focus`) forgets it; a structural release (the modal trap, a navigator's
//! cleared surface, an unmount) keeps it. An opening scope pushes it; a closing one pops it, with
//! every scope above, and the next rebuild, or the end of the next event pass, grants it back once
//! if the stop is still in the traversal walk. A session held inside the closing scope as it
//! closed is ended first; one started outside it after the close is never taken over. See
//! `docs/CORE_ARCHITECTURE.md`'s traversal notes.

use std::cell::Cell;

use crate::event::PassBracket;
use crate::overlay::OverlayKey;

thread_local! {
    /// The next [`FocusScopeKey::new`] value; starts at `1` so `0` can mean "none".
    static NEXT_FOCUS_SCOPE_KEY: Cell<u64> = const { Cell::new(1) };

    /// Scopes registered so far in this thread's open paint pass, in order, each with whether
    /// its host held the focus path.
    static SCOPE_REGISTRY: Cell<Vec<(FocusScopeKey, bool)>> = const { Cell::new(Vec::new()) };

    static SCOPE_PASS_OPEN: Cell<bool> = const { Cell::new(false) };
}

/// A focus-return scope's identity: allocate one per widget instance, register it every paint the
/// widget is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FocusScopeKey(u64);

impl FocusScopeKey {
    /// Allocate a key unique on this thread. Call once when the scope's widget is built.
    pub fn new() -> Self {
        FocusScopeKey(NEXT_FOCUS_SCOPE_KEY.with(|next| {
            let id = next.get();
            next.set(id.wrapping_add(1));
            id
        }))
    }

    /// The raw integer, for map keys; allocation order is not a contract.
    pub fn value(self) -> u64 {
        self.0
    }
}

/// A fresh key, as [`FocusScopeKey::new`].
impl Default for FocusScopeKey {
    fn default() -> Self {
        Self::new()
    }
}

/// The [`PassBracket`] around one registration pass: enter clears, `take` drains.
pub(crate) struct FocusScopePass(PassBracket<Vec<(FocusScopeKey, bool)>>);

impl FocusScopePass {
    pub(crate) fn enter() -> Self {
        FocusScopePass(PassBracket::enter(&SCOPE_REGISTRY, &SCOPE_PASS_OPEN))
    }

    pub(crate) fn take(&self) -> Vec<(FocusScopeKey, bool)> {
        self.0.take()
    }
}

/// Record `key`, and whether its host `holds` the focus path, in this thread's open pass (behind
/// `PaintCtx::register_focus_scope`).
pub(crate) fn register(key: FocusScopeKey, holds: bool) {
    SCOPE_REGISTRY.with(|registry| {
        let mut keys = registry.take();
        keys.push((key, holds));
        registry.set(keys);
    });
}

/// A scope as the root tracks it; the two key spaces never collide.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScopeId {
    Overlay(OverlayKey),
    Widget(FocusScopeKey),
}

/// A committed focus target: the traversal stop's id (`0` for the root widget) and whether the
/// root's keyboard flag was set for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FocusTarget {
    pub(crate) stop: u64,
    pub(crate) keyboard: bool,
}

/// One open scope and the target committed when it opened (`None`: nothing to return to).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FocusReturn {
    pub(crate) scope: ScopeId,
    pub(crate) target: Option<FocusTarget>,
    /// The epoch of a session held inside the scope: on an open widget scope, the one its host
    /// held at its last paint; on the owed return, the one still held inside as it closed.
    pub(crate) held: Option<u64>,
}

impl FocusReturn {
    /// Whether `live` is held inside this scope as it closes.
    fn holds(&self, live: &LiveSession) -> bool {
        match self.scope {
            ScopeId::Overlay(key) => live.surface == Some(key),
            ScopeId::Widget(_) => self.held == Some(live.epoch),
        }
    }
}

/// The live focus session as a paint settles its scopes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LiveSession {
    pub(crate) epoch: u64,
    /// The floated surface holding it, as resolved before this paint's resolution.
    pub(crate) surface: Option<OverlayKey>,
    /// The widget scopes whose host held its focus path this paint.
    pub(crate) holders: Vec<FocusScopeKey>,
}

/// The root's focus-return bookkeeping (see the module docs).
#[derive(Debug, Default)]
pub(crate) struct FocusReturns {
    last: Option<FocusTarget>,
    /// The scopes the last paint registered.
    painted: Vec<ScopeId>,
    /// One entry per open scope, outermost first.
    stack: Vec<FocusReturn>,
    /// A closed scope's return, owed to the next rebuild or event pass.
    pending: Option<FocusReturn>,
}

impl FocusReturns {
    /// Record the target a claim or grant just committed.
    pub(crate) fn commit(&mut self, target: Option<FocusTarget>) {
        self.last = target;
    }

    /// Forget the target: the user ended the session.
    pub(crate) fn forget(&mut self) {
        self.last = None;
    }

    /// Rewrite the standing target's keyboard flag (a Tab step, a restored return).
    pub(crate) fn set_keyboard(&mut self, keyboard: bool) {
        if let Some(target) = &mut self.last {
            target.keyboard = keyboard;
        }
    }

    /// Diff this paint's `scopes` against the last paint's: the lowest entry whose scope closed
    /// pops itself and every entry above it, becoming the owed return (holding `live`'s epoch when
    /// a closed one held it), then each newly painted scope pushes the standing target. Returns
    /// whether a return became owed.
    pub(crate) fn settle(&mut self, scopes: Vec<ScopeId>, live: Option<&LiveSession>) -> bool {
        let closed = self.stack.iter().position(|e| !scopes.contains(&e.scope));
        if let Some(index) = closed {
            let popped: Vec<FocusReturn> = self.stack.drain(index..).collect();
            let shut = |e: &&FocusReturn| !scopes.contains(&e.scope);
            let inside = live.filter(|live| popped.iter().filter(shut).any(|e| e.holds(live)));
            let held = inside.map(|live| live.epoch);
            self.pending = popped.first().map(|entry| FocusReturn { held, ..*entry });
        }
        let held = |scope: ScopeId| match (scope, live) {
            (ScopeId::Widget(key), Some(live)) if live.holders.contains(&key) => Some(live.epoch),
            _ => None,
        };
        for &scope in &scopes {
            match self.stack.iter_mut().find(|entry| entry.scope == scope) {
                Some(entry) => entry.held = held(scope),
                None if !self.painted.contains(&scope) => {
                    let (target, held) = (self.last, held(scope));
                    self.stack.push(FocusReturn {
                        scope,
                        target,
                        held,
                    });
                }
                None => {}
            }
        }
        self.painted = scopes;
        closed.is_some()
    }

    /// Take the owed return, if any; it runs at most once.
    pub(crate) fn take_pending(&mut self) -> Option<FocusReturn> {
        self.pending.take()
    }

    /// The open scopes' entries, outermost first.
    #[cfg(test)]
    pub(crate) fn stack(&self) -> &[FocusReturn] {
        &self.stack
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(stop: u64) -> Option<FocusTarget> {
        Some(FocusTarget {
            stop,
            keyboard: false,
        })
    }

    fn widget() -> ScopeId {
        ScopeId::Widget(FocusScopeKey::new())
    }

    #[test]
    fn keys_are_distinct_and_never_collide_with_an_overlay_key() {
        let (first, second) = (FocusScopeKey::new(), FocusScopeKey::default());
        assert_ne!(first, second);
        let overlay = OverlayKey::next();
        assert_ne!(
            ScopeId::Widget(FocusScopeKey(overlay.value())),
            ScopeId::Overlay(overlay),
            "equal raw values in the two spaces are distinct scopes"
        );
    }

    #[test]
    fn the_registry_is_pass_scoped_and_keeps_registration_order() {
        let (a, b) = (FocusScopeKey::new(), FocusScopeKey::new());
        let pass = FocusScopePass::enter();
        assert!(pass.take().is_empty());
        register(a, false);
        register(b, true);
        assert_eq!(pass.take(), vec![(a, false), (b, true)]);
        drop(pass);
        register(a, false);
        assert!(
            FocusScopePass::enter().take().is_empty(),
            "a stray registration does not survive into the next pass"
        );
    }

    #[test]
    fn an_opening_scope_captures_the_standing_target_and_its_close_owes_it() {
        let mut returns = FocusReturns::default();
        let scope = widget();
        returns.commit(target(7));
        assert!(!returns.settle(vec![scope], None));
        returns.commit(target(9));
        assert!(
            !returns.settle(vec![scope], None),
            "a scope still painting stays"
        );
        assert!(returns.take_pending().is_none());
        assert!(returns.settle(Vec::new(), None));
        let owed = returns.take_pending().expect("the close owes a return");
        assert_eq!(owed.target, target(7), "the target as the scope opened");
        assert!(returns.take_pending().is_none(), "taken once");
    }

    #[test]
    fn closing_an_outer_scope_pops_the_scopes_above_it_without_reopening_them() {
        let mut returns = FocusReturns::default();
        let (outer, inner) = (widget(), widget());
        returns.commit(target(1));
        returns.settle(vec![outer], None);
        returns.commit(target(2));
        returns.settle(vec![outer, inner], None);
        assert_eq!(returns.stack().len(), 2);

        assert!(returns.settle(vec![inner], None));
        assert_eq!(returns.take_pending().unwrap().target, target(1));
        assert!(returns.stack().is_empty(), "the inner entry went too");
        returns.settle(Vec::new(), None);
        assert!(
            returns.take_pending().is_none(),
            "a popped scope closing later owes nothing"
        );
    }

    #[test]
    fn a_forgotten_target_opens_a_scope_with_nothing_to_return() {
        let mut returns = FocusReturns::default();
        let scope = widget();
        returns.commit(target(3));
        returns.forget();
        returns.settle(vec![scope, scope], None);
        assert_eq!(returns.stack().len(), 1, "a repeated key is one scope");
        returns.settle(Vec::new(), None);
        assert_eq!(returns.take_pending().unwrap().target, None);
    }

    fn live(epoch: u64, surface: Option<OverlayKey>, holders: Vec<FocusScopeKey>) -> LiveSession {
        LiveSession {
            epoch,
            surface,
            holders,
        }
    }

    #[test]
    fn a_close_owes_the_session_its_host_held_at_its_last_paint() {
        let mut returns = FocusReturns::default();
        let key = FocusScopeKey::new();
        returns.commit(target(1));
        returns.settle(vec![ScopeId::Widget(key)], Some(&live(4, None, vec![key])));
        returns.settle(Vec::new(), Some(&live(4, None, Vec::new())));
        assert_eq!(returns.take_pending().unwrap().held, Some(4));

        returns.settle(vec![ScopeId::Widget(key)], Some(&live(4, None, vec![key])));
        returns.settle(Vec::new(), Some(&live(6, None, Vec::new())));
        let moved = returns.take_pending().unwrap();
        assert_eq!(moved.held, None, "a session started after that paint");
    }

    #[test]
    fn an_overlay_close_owes_the_session_its_surface_held() {
        let mut returns = FocusReturns::default();
        let (key, other) = (OverlayKey::next(), OverlayKey::next());
        returns.commit(target(1));
        returns.settle(vec![ScopeId::Overlay(key)], None);
        returns.settle(Vec::new(), Some(&live(3, Some(key), Vec::new())));
        assert_eq!(returns.take_pending().unwrap().held, Some(3));

        returns.settle(vec![ScopeId::Overlay(key)], None);
        returns.settle(Vec::new(), Some(&live(3, Some(other), Vec::new())));
        assert_eq!(returns.take_pending().unwrap().held, None);
    }
}
