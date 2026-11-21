//! Every per-thread slot, registry and id counter frust-reactive keeps, as the fields of one
//! [`ReactiveLocals`] behind one `thread_local!` static: on Android each static takes one of 128
//! process-wide pthread keys, and a field costs per access what its own static did.

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Weak;

use futures::executor::{LocalPool, LocalSpawner};

use crate::app_lifecycle::AppLifecycleListener;
use crate::back::RegisteredProvider;
use crate::time::TimerInner;

/// One field per slot; the owning modules keep the accessors. No outer cell wraps it, so nested
/// access to two fields behaves as nested access to two statics. Fields drop in declaration order,
/// so the local task queue, whose pending tasks run arbitrary drops, goes first, then the
/// registries; a drop running then reaches this state only through [`try_with`].
pub(crate) struct ReactiveLocals {
    /// The UI thread's local task queue for `!Send` futures, built on first use. Empty on every
    /// other thread (each thread has its own, so pumping off the UI thread is a harmless no-op
    /// rather than a panic).
    pub(crate) local_pool: OnceCell<RefCell<LocalPool>>,

    /// Spawner into `local_pool`, cached on first use to avoid re-deriving it per spawn.
    pub(crate) local_spawner: OnceCell<LocalSpawner>,

    /// The live "can the framework pop?" provider, which `back::handles_back` consults before the
    /// polled flag. Per thread because the facade's closure captures an `Rc`-backed
    /// `NavigatorController` (`!Send`).
    pub(crate) can_pop_provider: RefCell<Option<RegisteredProvider>>,

    /// Created by the first flagged timer and never dropped before the thread exits: it lives as
    /// long as the UI thread.
    pub(crate) flush_listener: RefCell<Option<AppLifecycleListener>>,

    pub(crate) flush_registry: RefCell<Vec<Weak<TimerInner>>>,

    /// Set on the thread that called `executor::init_ui_thread`; gates `spawn_local`.
    pub(crate) is_ui_thread: Cell<bool>,

    /// The next `CanPopRegistration` id, so a stale registration's cleanup can be told apart from
    /// the live one's; starts at `1`.
    pub(crate) can_pop_next_id: Cell<u64>,

    /// Whether this blocking-pool thread has already been lowered to the background class.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) lowered: Cell<bool>,
}

impl ReactiveLocals {
    /// Every field at the initial value its static had; the task queue is still unbuilt.
    const fn new() -> Self {
        Self {
            local_pool: OnceCell::new(),
            local_spawner: OnceCell::new(),
            can_pop_provider: RefCell::new(None),
            flush_listener: RefCell::new(None),
            flush_registry: RefCell::new(Vec::new()),
            is_ui_thread: Cell::new(false),
            can_pop_next_id: Cell::new(1),
            #[cfg(not(target_arch = "wasm32"))]
            lowered: Cell::new(false),
        }
    }

    /// This thread's local task queue, built on first use.
    pub(crate) fn local_pool(&self) -> &RefCell<LocalPool> {
        self.local_pool
            .get_or_init(|| RefCell::new(LocalPool::new()))
    }

    /// The spawner into this thread's local task queue, derived on first use.
    pub(crate) fn local_spawner(&self) -> &LocalSpawner {
        self.local_spawner
            .get_or_init(|| self.local_pool().borrow().spawner())
    }
}

thread_local! {
    static REACTIVE: ReactiveLocals = const { ReactiveLocals::new() };
}

/// Run `f` on this thread's state; panics once the thread's teardown has destroyed it, so a path
/// a `Drop` can reach then (a pending local task or a registered provider dropping) uses
/// [`try_with`].
pub(crate) fn with<R>(f: impl FnOnce(&ReactiveLocals) -> R) -> R {
    REACTIVE.with(f)
}

/// Run `f` on this thread's state, or return `None` once the thread's teardown has destroyed it.
pub(crate) fn try_with<R>(f: impl FnOnce(&ReactiveLocals) -> R) -> Option<R> {
    REACTIVE.try_with(f).ok()
}

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    use any_spawner::Executor;

    use super::*;
    use crate::time::Timer;
    use crate::{ReactiveRuntime, clear_can_pop_provider, handles_back, set_can_pop_provider};

    /// Held by a local task that never completes; its drop reaches back into this crate's
    /// per-thread state while that state is being torn down.
    struct ReachBack;

    impl Drop for ReachBack {
        fn drop(&mut self) {
            clear_can_pop_provider();
            let _ = handles_back();
            let _ = crate::executor::is_ui_thread();
            crate::executor::run_until_stalled();
        }
    }

    #[test]
    fn a_thread_exits_cleanly_with_every_slot_holding_a_value() {
        let _guard = crate::WAKER_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        thread::spawn(|| {
            let rt = ReactiveRuntime::init(Arc::new(|| {}));
            Timer::new(Duration::from_secs(60), || {})
                .flush_on_background()
                .detach();
            let registration = set_can_pop_provider(Box::new(|| true));
            Executor::spawn_local(async move {
                let _reach_back = ReachBack;
                let _registration = registration;
                std::future::pending::<()>().await;
            });
            rt.pump_local();
            with(|t| {
                assert_eq!(t.flush_registry.borrow().len(), 1);
                assert!(t.flush_listener.borrow().is_some());
                assert!(t.can_pop_provider.borrow().is_some());
                assert!(t.local_pool.get().is_some());
            });
        })
        .join()
        .expect("the thread's state tears down without a panic");
    }

    #[test]
    fn a_blocking_pool_thread_never_builds_the_local_task_queue() {
        let built = thread::spawn(|| {
            crate::runtime::lower_blocking_thread_once();
            with(|t| (t.lowered.get(), t.local_pool.get().is_some()))
        })
        .join()
        .expect("no panic");
        assert_eq!(built, (true, false));
    }
}
