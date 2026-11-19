//! [`RepaintScope`]: a `reactive_graph` subscriber that runs a caller-supplied callback when a
//! tracked signal changes, and nothing else.
//!
//! It is [`TrackedScope`](crate::TrackedScope)'s sibling with the frame consequences removed: a
//! notification never trips the runtime's signals-dirty flag and never fires the frame waker,
//! so it forces neither a mobile rebuild nor a desktop one. What a change means is the
//! callback's business; the facade's repaint bridge hands it the core repaint request.
//!
//! `reactive_graph` signals and memos keep their subscribers across notifications (a signal's
//! `mark_subscribers_check` clones its subscriber set rather than draining it), so a scope stays
//! subscribed until its next [`track`](RepaintScope::track) or its drop. A trigger
//! (`ArcTrigger`) drains its set on notify; a scope reading one must re-track to hear it again.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use reactive_graph::graph::{
    AnySource, AnySubscriber, ReactiveNode, Source, Subscriber, WithObserver,
};

/// The callback a [`RepaintScope`] runs on a change, on whichever thread wrote the signal.
pub type OnChange = Arc<dyn Fn() + Send + Sync>;

/// A reactive observer whose only reaction to a change is a callback.
///
/// Signals read inside [`track`](Self::track) subscribe the scope; a later write to any of them
/// runs the callback. [`new`](Self::new) coalesces to one call per arm, re-armed by `track` or
/// [`rearm`](Self::rearm); [`every_change`](Self::every_change) calls on every notification, for
/// a callback that coalesces itself. Dropping the scope unsubscribes it from every source.
///
/// The callback runs inside the writer's notification, possibly while a memo holds its own
/// lock, so it must not read or write signals; it should post work, not do it.
pub struct RepaintScope {
    inner: Arc<ScopeInner>,
}

struct ScopeInner {
    /// The de-duplicated sources the last `track` read (`reactive_graph`'s `SourceSet` is
    /// crate-private), walked to unsubscribe on re-track and on drop.
    sources: Mutex<Vec<AnySource>>,
    /// Cleared by the notification that runs the callback; set by `track` and `rearm`.
    armed: AtomicBool,
    coalesce: bool,
    on_change: OnChange,
}

impl ScopeInner {
    fn notify(&self) {
        if !self.coalesce || self.armed.swap(false, Ordering::AcqRel) {
            (self.on_change)();
        }
    }
}

impl ReactiveNode for ScopeInner {
    fn mark_dirty(&self) {
        self.notify();
    }

    /// A memo relays its upstream change as a check; like `TrackedScope`, treat it as a change
    /// (a spurious call is correct, a missed one is not).
    fn mark_check(&self) {
        self.notify();
    }

    fn mark_subscribers_check(&self) {}

    fn update_if_necessary(&self) -> bool {
        !self.armed.load(Ordering::Acquire)
    }
}

impl Subscriber for ScopeInner {
    fn add_source(&self, source: AnySource) {
        let mut sources = self.sources.lock().unwrap_or_else(PoisonError::into_inner);
        if !sources.contains(&source) {
            sources.push(source);
        }
    }

    fn clear_sources(&self, subscriber: &AnySubscriber) {
        let drained =
            std::mem::take(&mut *self.sources.lock().unwrap_or_else(PoisonError::into_inner));
        for source in drained {
            source.remove_subscriber(subscriber);
        }
    }
}

impl RepaintScope {
    /// A scope that runs `on_change` once per arm: the first notification after
    /// [`track`](Self::track) or [`rearm`](Self::rearm) calls it, later ones wait for the next arm.
    pub fn new(on_change: OnChange) -> Self {
        Self::with_policy(on_change, true)
    }

    /// A scope that runs `on_change` on every notification and never needs re-arming.
    pub fn every_change(on_change: OnChange) -> Self {
        Self::with_policy(on_change, false)
    }

    fn with_policy(on_change: OnChange, coalesce: bool) -> Self {
        Self {
            inner: Arc::new(ScopeInner {
                sources: Mutex::new(Vec::new()),
                armed: AtomicBool::new(true),
                coalesce,
                on_change,
            }),
        }
    }

    /// The type-erased handle the graph keys identity on, built by hand because the orphan rule
    /// forbids `ToAnySubscriber` for `Arc<ScopeInner>` here.
    fn any_subscriber(&self) -> AnySubscriber {
        AnySubscriber(
            Arc::as_ptr(&self.inner) as usize,
            Arc::downgrade(&self.inner) as Weak<dyn Subscriber + Send + Sync>,
        )
    }

    /// Run `f` with this scope as the observer and return its value: the previous sources are
    /// unsubscribed, the scope is armed, and every signal `f` reads subscribes it.
    pub fn track<T>(&self, f: impl FnOnce() -> T) -> T {
        let any = self.any_subscriber();
        any.clear_sources(&any);
        self.inner.armed.store(true, Ordering::Release);
        any.with_observer(f)
    }

    /// Re-arm a coalescing scope without re-tracking, so the next change calls back again.
    pub fn rearm(&self) {
        self.inner.armed.store(true, Ordering::Release);
    }
}

impl Drop for RepaintScope {
    fn drop(&mut self) {
        let any = self.any_subscriber();
        any.clear_sources(&any);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use reactive_graph::computed::Memo;
    use reactive_graph::signal::RwSignal;
    use reactive_graph::traits::{Get, Set};

    use super::*;
    use crate::{FrameWaker, ReactiveRuntime};

    fn counting() -> (OnChange, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&calls);
        let on_change: OnChange = Arc::new(move || {
            calls.fetch_add(1, Ordering::SeqCst);
        });
        (on_change, seen)
    }

    /// Holds the waker lock throughout: the runtime's signals-dirty flag and frame waker are
    /// process-wide, and the tracked-scope tests trip both.
    #[test]
    fn repaint_scope_calls_back_without_dirtying_or_waking() {
        let _guard = crate::WAKER_TEST_LOCK
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&wakes);
        let waker: FrameWaker = Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        let rt = ReactiveRuntime::init(waker);
        rt.take_signals_dirty();

        // A tracked write calls back once per arm, and neither dirties nor wakes.
        let (on_change, calls) = counting();
        let scope = RepaintScope::new(on_change);
        let sig = rt.with_owner(|| RwSignal::new(0));
        scope.track(|| sig.get());
        let wakes_before = wakes.load(Ordering::SeqCst);
        sig.set(1);
        sig.set(2);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "one call per arm");
        assert!(
            !rt.take_signals_dirty(),
            "a repaint scope must not mark signals dirty"
        );
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            wakes_before,
            "nor fire the frame waker"
        );

        // Re-arming without re-tracking hears the same source again: it stayed subscribed.
        scope.rearm();
        sig.set(3);
        assert_eq!(calls.load(Ordering::SeqCst), 2, "rearm re-opens the edge");

        // An untracked signal does not call back.
        let other = rt.with_owner(|| RwSignal::new(0));
        scope.rearm();
        other.set(9);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "an untracked write is silent"
        );

        // After drop, nothing calls back.
        drop(scope);
        sig.set(4);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "a dropped scope is unsubscribed"
        );

        // `every_change` calls on each notification, across threads and through a memo.
        let (on_change, calls) = counting();
        let every = RepaintScope::every_change(on_change);
        let base = rt.with_owner(|| RwSignal::new(1));
        let doubled = rt.with_owner(|| Memo::new(move |_| base.get() * 2));
        assert_eq!(every.track(|| (sig.get(), doubled.get())), (4, 2));
        sig.set(5);
        sig.set(6);
        base.set(2);
        base.set(3);
        std::thread::spawn(move || sig.set(7))
            .join()
            .expect("cross-thread writer panicked");
        assert_eq!(calls.load(Ordering::SeqCst), 5, "every write calls back");
        assert!(!rt.take_signals_dirty(), "still never marks signals dirty");
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            wakes_before,
            "still never wakes"
        );
    }
}
