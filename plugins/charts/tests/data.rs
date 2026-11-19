//! The data model: `Series` and `Samples` handles, `YDomain`/`nice_max` and `Tone`.
//!
//! Every write requests a process-wide repaint, so each test that writes or reads the repaint
//! generation holds `STATICS` for its whole run.

use std::sync::{Mutex, MutexGuard, PoisonError};

use frust::{AnyView, Color};
use frust_charts::{Samples, Series, Tone, YDomain, nice_max};
use frust_core::{RenderRoot, repaint_generation};

static STATICS: Mutex<()> = Mutex::new(());

fn hold() -> MutexGuard<'static, ()> {
    STATICS.lock().unwrap_or_else(PoisonError::into_inner)
}

fn slots(series: &Series) -> Vec<Option<f32>> {
    series.with_slots(|view| view.iter().collect())
}

/// How many repaint requests `f` made.
fn repaints(f: impl FnOnce()) -> u64 {
    let before = repaint_generation();
    f();
    repaint_generation() - before
}

#[test]
fn handles_are_send_and_sync() {
    fn check<T: Send + Sync + Clone>() {}
    check::<Series>();
    check::<Samples>();
    check::<frust_charts::SampleSlot>();
}

#[test]
#[should_panic(expected = "capacity must be at least 1")]
fn zero_capacity_panics() {
    let _ = Series::new(0);
}

#[test]
fn ring_wraps_oldest_to_newest() {
    let _g = hold();
    let s = Series::new(3);
    assert!(s.is_empty());
    for v in 1..=5 {
        s.push(v as f32);
    }
    assert_eq!((s.len(), s.capacity()), (3, 3));
    assert_eq!(slots(&s), [Some(3.0), Some(4.0), Some(5.0)]);
    s.with_slots(|view| {
        assert_eq!(view.len(), 3);
        assert_eq!(view.get(0), Some(Some(3.0)));
        assert_eq!(view.get(2), Some(Some(5.0)));
        assert_eq!(view.get(3), None);
        assert_eq!(view.iter().next_back(), Some(Some(5.0)));
    });
    assert_eq!(s.latest(), Some(5.0));
    assert_eq!(s.max(), Some(5.0));

    s.clear();
    assert!(s.is_empty());
    assert_eq!(s.latest(), None);
    s.push(9.0);
    assert_eq!(slots(&s), [Some(9.0)]);
}

#[test]
fn gaps_and_non_finite_inputs_are_gaps() {
    let _g = hold();
    let s = Series::new(9);
    s.push(1.0);
    s.push(f32::NAN);
    s.push_gap();
    s.push(f32::INFINITY);
    s.push_slot(Some(f32::NEG_INFINITY));
    s.push_slot(Some(2.0));
    s.extend([None, Some(f32::NAN), Some(-3.0)]);
    assert_eq!(
        slots(&s),
        [
            Some(1.0),
            None,
            None,
            None,
            None,
            Some(2.0),
            None,
            None,
            Some(-3.0)
        ]
    );
    assert_eq!(s.max(), Some(2.0));
    s.push_gap();
    assert_eq!(s.latest(), None);
    assert_eq!(Series::new(2).max(), None);
}

#[test]
fn replace_keeps_the_newest_capacity_items() {
    let _g = hold();
    let s = Series::new(4);
    s.push(100.0);
    s.replace((0..10).map(|v| Some(v as f32)));
    assert_eq!(slots(&s), [Some(6.0), Some(7.0), Some(8.0), Some(9.0)]);
    s.replace([Some(1.0), None]);
    assert_eq!(slots(&s), [Some(1.0), None]);
    assert_eq!(s.latest(), None);
}

#[test]
fn revision_and_repaint_move_once_per_mutating_call() {
    let _g = hold();
    let s = Series::new(4);
    assert_eq!(s.revision(), 0);
    assert_eq!(repaints(|| s.push(1.0)), 1);
    assert_eq!(repaints(|| s.push_gap()), 1);
    assert_eq!(repaints(|| s.push_slot(Some(2.0))), 1);
    assert_eq!(s.revision(), 3);
    assert_eq!(repaints(|| s.extend((0..10).map(|v| Some(v as f32)))), 1);
    assert_eq!(s.revision(), 4);
    assert_eq!(repaints(|| s.replace((10..20).map(|v| Some(v as f32)))), 1);
    assert_eq!(s.revision(), 5);
    assert_eq!(repaints(|| s.clear()), 1);
    assert_eq!(s.revision(), 6);

    // Calls that change nothing bump nothing.
    assert_eq!(repaints(|| s.clear()), 0);
    assert_eq!(repaints(|| s.extend([])), 0);
    assert_eq!(repaints(|| s.replace([])), 0);
    assert_eq!(s.revision(), 6);
    s.extend([Some(1.0), Some(2.0)]);
    assert_eq!(repaints(|| s.replace([Some(1.0), Some(2.0)])), 0);
    assert_eq!(s.revision(), 7);

    // Clones share the ring and its revision.
    let c = s.clone();
    assert!(c.ptr_eq(&s));
    assert!(!c.ptr_eq(&Series::new(4)));
    c.push(3.0);
    assert_eq!((s.revision(), s.latest()), (8, Some(3.0)));
}

#[test]
fn push_from_another_thread_is_visible_and_owes_a_repaint() {
    let _g = hold();
    let s = Series::new(16);
    let root: RenderRoot<(), AnyView<()>> = RenderRoot::new();
    assert!(!root.repaint_pending());

    let writer = s.clone();
    std::thread::spawn(move || writer.push(42.0))
        .join()
        .unwrap();

    assert_eq!(s.latest(), Some(42.0));
    assert_eq!(s.revision(), 1);
    assert!(root.repaint_pending());
}

#[test]
fn samples_set_set_all_and_slot() {
    let _g = hold();
    let set = Samples::new(4);
    assert_eq!((set.len(), set.is_empty()), (4, false));
    assert!(Samples::new(0).is_empty());
    assert_eq!(set.get(0), None);

    assert_eq!(repaints(|| set.set(1, 50.0)), 1);
    assert_eq!((set.get(1), set.revision()), (Some(50.0), 1));
    // Writing the value already held changes nothing.
    assert_eq!(repaints(|| set.set(1, 50.0)), 0);
    assert_eq!(set.revision(), 1);
    set.set(2, f32::NAN);
    assert_eq!(set.get(2), None);
    assert_eq!(set.revision(), 1);
    set.set_gap(1);
    assert_eq!((set.get(1), set.revision()), (None, 2));
    assert_eq!(set.get(99), None);

    assert_eq!(
        repaints(|| set.set_all([Some(1.0), None, Some(f32::INFINITY), Some(4.0), Some(5.0)])),
        1
    );
    assert_eq!(set.revision(), 3);
    set.with_values(|v| assert_eq!(v, [Some(1.0), None, None, Some(4.0)]));
    // A short input leaves the rest as gaps.
    set.set_all([Some(7.0)]);
    set.with_values(|v| assert_eq!(v, [Some(7.0), None, None, None]));
    assert_eq!(repaints(|| set.set_all([Some(7.0)])), 0);

    let slot = set.slot(3);
    let twin = slot.clone();
    assert_eq!((slot.index(), slot.get()), (3, None));
    slot.set(88.0);
    assert_eq!((twin.get(), set.get(3)), (Some(88.0), Some(88.0)));
    assert!(slot.samples().ptr_eq(&set));
    assert_eq!(slot.revision(), set.revision());
    twin.set_gap();
    assert_eq!(slot.get(), None);
}

#[test]
#[should_panic(expected = "out of range")]
fn samples_slot_out_of_range_panics() {
    let _ = Samples::new(2).slot(2);
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "out of range")]
fn samples_set_out_of_range_debug_asserts() {
    Samples::new(2).set(5, 1.0);
}

#[test]
fn nice_max_table() {
    let table = [
        (0.3, 0.5),
        (4.2, 5.0),
        (7.0, 10.0),
        (12.0, 20.0),
        (23.0, 25.0),
        (100.0, 100.0),
        (0.0, 1.0),
        (1.0, 1.0),
        (2.1, 2.5),
        (1000.0, 1000.0),
        (1001.0, 2000.0),
        (-5.0, 1.0),
        (f32::NAN, 1.0),
        (f32::INFINITY, 1.0),
    ];
    for (input, want) in table {
        assert_eq!(nice_max(input), want, "nice_max({input})");
    }
    assert_eq!(nice_max(f32::MAX), f32::MAX);
    for v in [1e-30_f32, 3e-7, 0.07, 9.99, 4321.0, 7.5e12, 2e37] {
        assert!(nice_max(v) >= v, "nice_max({v}) = {}", nice_max(v));
    }
}

#[test]
fn resolve_never_returns_max_at_or_below_min() {
    let domains = [
        YDomain::percent(),
        YDomain::auto_from_zero(),
        YDomain::default(),
        YDomain::fixed(5.0, 5.0),
        YDomain::fixed(10.0, -10.0),
        YDomain::fixed(f32::NAN, f32::INFINITY),
        YDomain::fixed(f32::MAX, f32::MAX),
        YDomain::fixed(-f32::MAX, -f32::MAX),
        YDomain::Auto {
            floor: 0.0,
            nice: false,
            min_span: 0.0,
        },
        YDomain::Auto {
            floor: f32::NAN,
            nice: true,
            min_span: f32::NAN,
        },
        YDomain::Auto {
            floor: 50.0,
            nice: false,
            min_span: -3.0,
        },
    ];
    let data = [
        None,
        Some(0.0),
        Some(-20.0),
        Some(0.3),
        Some(73.0),
        Some(1e30),
        Some(f32::MAX),
        Some(f32::NAN),
    ];
    for domain in domains {
        for data_max in data {
            let (min, max) = domain.resolve(data_max);
            assert!(
                min.is_finite() && max.is_finite() && max > min,
                "{domain:?}.resolve({data_max:?}) = ({min}, {max})"
            );
        }
    }

    assert_eq!(YDomain::percent().resolve(Some(250.0)), (0.0, 100.0));
    assert_eq!(YDomain::fixed(10.0, -10.0).resolve(None), (-10.0, 10.0));
    assert_eq!(YDomain::auto_from_zero().resolve(None), (0.0, 1.0));
    assert_eq!(YDomain::auto_from_zero().resolve(Some(73.0)), (0.0, 100.0));
    assert_eq!(YDomain::auto_from_zero().resolve(Some(0.3)), (0.0, 1.0));
    let raw = YDomain::Auto {
        floor: 10.0,
        nice: false,
        min_span: 5.0,
    };
    assert_eq!(raw.resolve(Some(12.0)), (10.0, 15.0));
    assert_eq!(raw.resolve(Some(42.0)), (10.0, 42.0));
}

#[test]
fn tone_stops_stay_ordered_and_bound_at_the_threshold() {
    let [calm, busy, critical, other] = [
        Color::from_rgb8(1, 0, 0),
        Color::from_rgb8(2, 0, 0),
        Color::from_rgb8(3, 0, 0),
        Color::from_rgb8(4, 0, 0),
    ];
    let tone = Tone::load(calm, busy, critical);
    assert_eq!(tone.color_at(0.0), calm);
    assert_eq!(tone.color_at(59.9), calm);
    assert_eq!(tone.color_at(60.0), busy);
    assert_eq!(tone.color_at(84.9), busy);
    assert_eq!(tone.color_at(85.0), critical);
    assert_eq!(tone.color_at(1000.0), critical);
    assert_eq!(tone.color_at(-5.0), calm);
    assert_eq!(tone.color_at(f32::NAN), calm);

    // Out-of-order stops are sorted; a duplicate threshold replaces; NaN is ignored.
    let tone = Tone::new(calm)
        .stop(85.0, critical)
        .stop(60.0, other)
        .stop(60.0, busy)
        .stop(f32::NAN, other);
    assert_eq!(tone.stops(), [(60.0, busy), (85.0, critical)]);
    assert_eq!(tone, Tone::load(calm, busy, critical));
    assert_eq!(tone.base(), calm);
    assert_eq!(Tone::new(other).color_at(99.0), other);

    let load = Tone::load(
        Tone::FALLBACK_CALM,
        Tone::FALLBACK_BUSY,
        Tone::FALLBACK_CRITICAL,
    );
    assert_eq!(load.color_at(Tone::LOAD_BUSY_AT), Tone::FALLBACK_BUSY);
}

#[test]
fn intensity_is_clamped_to_the_domain() {
    let domain = (0.0, 100.0);
    assert_eq!(Tone::intensity_at(0.0, domain), 0.0);
    assert_eq!(Tone::intensity_at(25.0, domain), 0.25);
    assert_eq!(Tone::intensity_at(100.0, domain), 1.0);
    assert_eq!(Tone::intensity_at(-10.0, domain), 0.0);
    assert_eq!(Tone::intensity_at(150.0, domain), 1.0);
    assert_eq!(Tone::intensity_at(f32::NAN, domain), 0.0);
    assert_eq!(Tone::intensity_at(5.0, (5.0, 5.0)), 0.0);
    assert_eq!(Tone::intensity_at(6.0, (5.0, 5.0)), 1.0);
    let resolved = YDomain::fixed(-1.0, 1.0).resolve(None);
    assert_eq!(Tone::intensity_at(0.0, resolved), 0.5);
}
