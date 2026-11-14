//! Focus-return scopes: when a modal surface closes, keyboard focus goes back to the control that
//! held it as the surface opened (the WAI-ARIA dialog pattern). On by default.
//!
//! A widget that owns focus while visible registers a [`FocusScopeKey`] every paint it is open
//! ([`PaintCtx::register_focus_scope`](crate::widget::PaintCtx::register_focus_scope)); the paint
//! it stops in is its close. An Interactive `OverlayKeys::Modal` overlay entry is a scope too.
//!
//! The root keeps the last committed focus target, the deepest traversal stop on the session a
//! claim or Tab grant committed plus the keyboard flag. A user blur (a tap on nothing, a widget's
//! `EventCtx::release_focus`) forgets it; a structural release (the modal trap, a navigator's
//! cleared surface, an unmount) keeps it. An opening scope pushes it; a closing one pops it, with
//! every scope above, and the next rebuild or event pass grants it back once, unless a session is
//! live or the stop has left the traversal walk. See `docs/CORE_ARCHITECTURE.md`'s traversal notes.

use std::cell::Cell;

use crate::event::PassBracket;
use crate::overlay::OverlayKey;

thread_local! {
    /// The next [`FocusScopeKey::new`] value; starts at `1` so `0` can mean "none".
    static NEXT_FOCUS_SCOPE_KEY: Cell<u64> = const { Cell::new(1) };

    /// Scopes registered so far in this thread's open paint pass, in order.
    static SCOPE_REGISTRY: Cell<Vec<FocusScopeKey>> = const { Cell::new(Vec::new()) };

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
pub(crate) struct FocusScopePass(PassBracket<Vec<FocusScopeKey>>);

impl FocusScopePass {
    pub(crate) fn enter() -> Self {
        FocusScopePass(PassBracket::enter(&SCOPE_REGISTRY, &SCOPE_PASS_OPEN))
    }

    pub(crate) fn take(&self) -> Vec<FocusScopeKey> {
        self.0.take()
    }
}

/// Record `key` in this thread's open pass (behind `PaintCtx::register_focus_scope`).
pub(crate) fn register(key: FocusScopeKey) {
    SCOPE_REGISTRY.with(|registry| {
        let mut keys = registry.take();
        keys.push(key);
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
    /// pops itself and every entry above it, becoming the owed return, then each newly painted
    /// scope pushes the standing target. Returns whether a return became owed.
    pub(crate) fn settle(&mut self, scopes: Vec<ScopeId>) -> bool {
        let closed = self.stack.iter().position(|e| !scopes.contains(&e.scope));
        if let Some(index) = closed {
            self.pending = self.stack.drain(index..).next();
        }
        for &scope in &scopes {
            let tracked = self.stack.iter().any(|entry| entry.scope == scope);
            if !self.painted.contains(&scope) && !tracked {
                let target = self.last;
                self.stack.push(FocusReturn { scope, target });
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
        register(a);
        register(b);
        assert_eq!(pass.take(), vec![a, b]);
        drop(pass);
        register(a);
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
        assert!(!returns.settle(vec![scope]));
        returns.commit(target(9));
        assert!(!returns.settle(vec![scope]), "a scope still painting stays");
        assert!(returns.take_pending().is_none());
        assert!(returns.settle(Vec::new()));
        let owed = returns.take_pending().expect("the close owes a return");
        assert_eq!(owed.target, target(7), "the target as the scope opened");
        assert!(returns.take_pending().is_none(), "taken once");
    }

    #[test]
    fn closing_an_outer_scope_pops_the_scopes_above_it_without_reopening_them() {
        let mut returns = FocusReturns::default();
        let (outer, inner) = (widget(), widget());
        returns.commit(target(1));
        returns.settle(vec![outer]);
        returns.commit(target(2));
        returns.settle(vec![outer, inner]);
        assert_eq!(returns.stack().len(), 2);

        assert!(returns.settle(vec![inner]));
        assert_eq!(returns.take_pending().unwrap().target, target(1));
        assert!(returns.stack().is_empty(), "the inner entry went too");
        returns.settle(Vec::new());
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
        returns.settle(vec![scope, scope]);
        assert_eq!(returns.stack().len(), 1, "a repeated key is one scope");
        returns.settle(Vec::new());
        assert_eq!(returns.take_pending().unwrap().target, None);
    }
}
