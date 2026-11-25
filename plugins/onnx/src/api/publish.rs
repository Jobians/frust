//! [`Outbox`]: hands a handle's signal writes from its worker thread to the UI thread, in order.
//!
//! `reactive_graph` takes a signal's lock without waiting for it: a read that meets a write made
//! on another thread panics ("already disposed"), and a write that meets a read is dropped. So a
//! handle never writes a signal off the UI thread. Its worker pushes each write here as an
//! update; a task on the UI thread's local queue (`frust::spawn_local`) applies the updates in the
//! order they were pushed, and a request call, which runs on the UI thread, applies the waiting
//! ones itself before it returns. A push wakes the task, and the runtime's waker for a local task
//! also fires the shell's frame waker, so the UI applies the update without waiting for input.
//!
//! `frust-models` keeps its own copy of this module: `frust-onnx`'s `frust-api` feature does not
//! pull `frust-models` in, so neither crate can borrow the other's.

use std::collections::VecDeque;
use std::future::poll_fn;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};
use std::thread::{self, ThreadId};

/// An ordered queue of updates from any thread, applied on the UI thread.
pub(crate) struct Outbox<U> {
    state: Arc<Mutex<State<U>>>,
}

impl<U> Clone for Outbox<U> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
        }
    }
}

struct State<U> {
    updates: VecDeque<U>,
    /// Live [`Work`] tokens: units of work that may still push.
    open: usize,
    /// A drain task is on the UI thread's local queue.
    draining: bool,
    /// Wakes the drain task while it waits for an update.
    waker: Option<Waker>,
    /// The thread the first drain task was spawned on: the UI thread.
    ui: Option<ThreadId>,
}

fn lock<U>(state: &Mutex<State<U>>) -> MutexGuard<'_, State<U>> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One unit of work that may still push. The drain task runs while any token is alive, and ends
/// once the last one is dropped and every update is applied.
pub(crate) struct Work<U> {
    state: Arc<Mutex<State<U>>>,
}

impl<U> Drop for Work<U> {
    fn drop(&mut self) {
        let waker = {
            let mut state = lock(&self.state);
            state.open -= 1;
            if state.open == 0 {
                state.waker.take()
            } else {
                None
            }
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

impl<U: 'static> Outbox<U> {
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                updates: VecDeque::new(),
                open: 0,
                draining: false,
                waker: None,
                ui: None,
            })),
        }
    }

    /// Queues `update` behind every earlier one and wakes the drain task. Any thread; takes a
    /// short lock and never waits for the UI thread.
    pub(crate) fn push(&self, update: U) {
        let waker = {
            let mut state = lock(&self.state);
            state.updates.push_back(update);
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// Starts a unit of work on the UI thread: spawns a drain task that applies updates with
    /// `apply` unless one already runs, and returns the token that keeps it running.
    ///
    /// # Panics
    ///
    /// Off the UI thread, naming `call`, before anything changes: `frust::spawn_local`'s panic
    /// for the first call, this one's afterwards.
    pub(crate) fn begin(&self, call: &str, apply: impl Fn(U) + 'static) -> Work<U> {
        let current = thread::current().id();
        let (ui, draining) = {
            let state = lock(&self.state);
            (state.ui, state.draining)
        };
        assert!(
            ui.is_none_or(|ui| ui == current),
            "{call} was made off the UI thread; make it on the thread the app's components and \
             event handlers run on"
        );
        let spawn = !draining;
        if spawn {
            let outbox = self.clone();
            frust::spawn_local(async move {
                poll_fn(|cx| outbox.drain(cx, &apply)).await;
            });
        }
        let mut state = lock(&self.state);
        state.ui = Some(current);
        state.draining = true;
        state.open += 1;
        Work {
            state: self.state.clone(),
        }
    }

    /// Applies every waiting update now, in order. UI thread only: the caller made the
    /// [`begin`](Self::begin) call that checks it.
    pub(crate) fn flush(&self, apply: impl Fn(U)) {
        loop {
            let Some(update) = lock(&self.state).updates.pop_front() else {
                return;
            };
            apply(update);
        }
    }

    /// The drain task's poll: applies what is queued, then waits for more, or ends once no
    /// work is left.
    fn drain(&self, cx: &mut Context<'_>, apply: &impl Fn(U)) -> Poll<()> {
        loop {
            let update = {
                let mut state = lock(&self.state);
                match state.updates.pop_front() {
                    Some(update) => update,
                    None if state.open == 0 => {
                        state.draining = false;
                        state.waker = None;
                        return Poll::Ready(());
                    }
                    None => {
                        state.waker = Some(cx.waker().clone());
                        return Poll::Pending;
                    }
                }
            };
            // Applied without the lock, so an observer the update calls may read signals.
            apply(update);
        }
    }
}
