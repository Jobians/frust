//! The shared dismiss vocabulary for modal hosts.
//!
//! A modal host (a dialog, sheet, or menu) can be dismissed by an Escape key,
//! a press outside its panel, or a platform back press. The host asks its
//! app whether that dismissal may proceed through an optional **dismiss veto**,
//! `on_dismiss_request(|state, cause| -> decision)`, so an app can keep a
//! dialog open while it holds an unsaved draft. [`DismissCause`] names why the
//! dismissal was attempted and [`DismissDecision`] answers whether it proceeds.
//! Both types live here, in `frust-widgets`, so every design system uses one
//! vocabulary.
//!
//! # The veto contract
//!
//! A host that implements the veto follows these rules:
//!
//! - The veto runs with `&mut State`, before `on_dismiss`, once per dismissal
//!   attempt.
//! - [`DismissDecision::Dismiss`] lets the dismissal proceed as it would
//!   without a veto. [`DismissDecision::Keep`] keeps the host open: it fires
//!   no `on_dismiss`, stages no exit animation, and still consumes the input
//!   that asked for the dismissal.
//! - A host with no veto installed behaves exactly as it did before the veto
//!   existed.
//! - A veto does not hold a dialog open across `Router::go`, which closes
//!   route-less overlays regardless of any veto.

/// Why a modal host was asked to dismiss itself.
///
/// Passed to the dismiss veto so an app can tell the causes apart.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DismissCause {
    /// A bare Escape key reached the host.
    Escape,
    /// A press and release landed outside the host's panel.
    Backdrop,
    /// A platform back press was routed to the host (Android back, or the
    /// navigator's `DismissAnimated` signal).
    Back,
}

/// The veto's answer to a dismissal attempt.
///
/// [`Dismiss`](Self::Dismiss) is the default, so a veto that has no reason to
/// block a dismissal can answer with `DismissDecision::default()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum DismissDecision {
    /// Let the host dismiss as it would without a veto.
    #[default]
    Dismiss,
    /// Keep the host open. It fires no `on_dismiss`, stages no exit, and still
    /// consumes the input that asked for the dismissal.
    Keep,
}

#[cfg(test)]
mod tests {
    use super::{DismissCause, DismissDecision};
    use std::collections::HashSet;

    #[test]
    fn default_decision_is_dismiss() {
        assert_eq!(DismissDecision::default(), DismissDecision::Dismiss);
    }

    #[test]
    fn causes_are_distinct_and_hashable() {
        let causes: HashSet<DismissCause> = [
            DismissCause::Escape,
            DismissCause::Backdrop,
            DismissCause::Back,
            DismissCause::Escape,
        ]
        .into_iter()
        .collect();
        assert_eq!(causes.len(), 3);
    }

    #[test]
    fn decisions_are_distinct() {
        assert_ne!(DismissDecision::Dismiss, DismissDecision::Keep);
    }
}
