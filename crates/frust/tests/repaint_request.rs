//! A repaint request repaints a mounted tree without rebuilding it, whether it comes straight
//! from [`request_repaint`] or from a signal bound with [`repaint_on`].
//!
//! A counting `Component` hosts a canvas that paints a shared atomic; the test changes the
//! atomic, asks for a repaint, and records a paint-only frame with `Tester::repaint`.
//!
//! Needs the facade's `test-support` feature (`cargo test -p frust-ui --features test-support`);
//! without it the file compiles to nothing.

#![cfg(feature = "test-support")]

use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use frust::authoring::Point;
use frust::testing::{Tester, TesterOptions};
use frust::{
    Color, Component, Get, RepaintBinding, RwSignal, Set, View, canvas, component, repaint_on,
    request_repaint,
};
use frust_reactive::ReactiveRuntime;
use kurbo::Size;

const SIZE: Size = Size::new(200.0, 100.0);

/// The repaint generation is process-wide: every test that requests a repaint holds this.
static SERIAL: Mutex<()> = Mutex::new(());

#[derive(Clone, Default)]
struct Chart {
    builds: Rc<Cell<u32>>,
    value: Arc<AtomicU32>,
    painted: Arc<AtomicU32>,
    revision: Option<RwSignal<u32>>,
}

struct ChartState {
    _binding: Option<RepaintBinding>,
}

impl Component for Chart {
    type State = ChartState;

    fn init(&self) -> ChartState {
        ChartState {
            _binding: self
                .revision
                .map(|revision| repaint_on(move || revision.get())),
        }
    }

    fn build(&self, _state: &mut ChartState) -> impl View<ChartState> {
        self.builds.set(self.builds.get() + 1);
        let value = Arc::clone(&self.value);
        let painted = Arc::clone(&self.painted);
        canvas(move |scene, size, _ctx| {
            let value = value.load(Ordering::SeqCst);
            painted.store(value, Ordering::SeqCst);
            let width = f64::from(value).min(size.width);
            scene.fill_rect(Point::ZERO, Size::new(width, size.height), Color::BLACK);
        })
    }
}

#[test]
fn request_repaint_paints_without_a_rebuild() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let chart = Chart::default();
    chart.value.store(3, Ordering::SeqCst);
    let mounted = chart.clone();
    let mut tester = Tester::new((), move |_: &mut ()| component(mounted.clone()), SIZE);
    let builds = chart.builds.get();
    assert_eq!(chart.painted.load(Ordering::SeqCst), 3);
    assert!(
        tester.repaint().is_none(),
        "no request, no paint-only frame"
    );

    chart.value.store(7, Ordering::SeqCst);
    assert!(
        tester.repaint().is_none(),
        "a data change alone asks for nothing"
    );
    request_repaint();
    assert!(
        tester.repaint().is_some(),
        "a pending request records a frame"
    );
    assert_eq!(
        chart.painted.load(Ordering::SeqCst),
        7,
        "the canvas painted the new value"
    );
    assert_eq!(chart.builds.get(), builds, "a repaint never rebuilds");
    assert!(tester.repaint().is_none(), "the paint honoured the request");
}

#[test]
fn a_bound_signal_repaints_without_a_rebuild_or_dirty_signals() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let mut signals = None;
    let mut chart = Chart::default();
    let mut tester = Tester::new_with(SIZE, &TesterOptions::default(), || {
        let revision = RwSignal::new(0_u32);
        let unbound = RwSignal::new(0_u32);
        signals = Some((revision, unbound));
        chart.revision = Some(revision);
        let mounted = chart.clone();
        ((), move |_: &mut ()| component(mounted.clone()))
    });
    let (revision, unbound) = signals.expect("setup ran");
    let runtime = ReactiveRuntime::get().expect("the Tester brought the runtime up");
    let builds = chart.builds.get();
    runtime.take_signals_dirty();
    assert!(tester.repaint().is_none());

    // Every write repaints: the binding stays subscribed across notifications.
    for (step, value) in [(1, 11), (2, 12)] {
        chart.value.store(value, Ordering::SeqCst);
        revision.set(step);
        assert!(
            !runtime.take_signals_dirty(),
            "the bridge never marks signals dirty"
        );
        assert!(
            tester.repaint().is_some(),
            "write {step} requested a repaint"
        );
        assert_eq!(chart.painted.load(Ordering::SeqCst), value);
        assert_eq!(chart.builds.get(), builds, "write {step} rebuilt nothing");
    }

    // A dropped binding no longer repaints.
    let binding = repaint_on(move || unbound.get());
    drop(binding);
    unbound.set(1);
    assert!(
        tester.repaint().is_none(),
        "a dropped binding is unsubscribed"
    );

    // A cross-thread write repaints as well.
    std::thread::spawn(move || revision.set(3))
        .join()
        .expect("cross-thread writer panicked");
    assert!(
        tester.repaint().is_some(),
        "a write from another thread repaints"
    );
    assert_eq!(chart.builds.get(), builds);
}
