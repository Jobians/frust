//! `YDomain::resolve_range` and the `AutoRange` domain: a range from the data's minimum to
//! its maximum, rounded outward to round numbers.

use frust_charts::{YDomain, ticks};

fn assert_usable((lo, hi): (f32, f32)) {
    assert!(
        lo.is_finite() && hi.is_finite(),
        "non-finite range ({lo}, {hi})"
    );
    assert!(hi > lo, "empty range ({lo}, {hi})");
}

#[test]
fn fixed_ignores_the_data() {
    let domain = YDomain::fixed(10.0, 20.0);
    for data in [
        None,
        Some((-500.0, 500.0)),
        Some((12.0, 13.0)),
        Some((f32::NAN, 1.0)),
    ] {
        assert_eq!(domain.resolve_range(data), (10.0, 20.0));
    }
    assert_eq!(
        YDomain::fixed(5.0, -5.0).resolve_range(Some((0.0, 1.0))),
        YDomain::fixed(5.0, -5.0).resolve(None)
    );
}

#[test]
fn auto_matches_resolve_on_the_data_maximum() {
    let domains = [
        YDomain::auto_from_zero(),
        YDomain::Auto {
            floor: -10.0,
            nice: false,
            min_span: 5.0,
        },
        YDomain::Auto {
            floor: 2.0,
            nice: true,
            min_span: 0.0,
        },
    ];
    let data = [
        (3.2, 87.0),
        (-40.0, 12.0),
        (7.0, 7.0),
        (0.0, 0.3),
        (-5.0, -1.0),
    ];
    for domain in domains {
        for (min, max) in data {
            assert_eq!(
                domain.resolve_range(Some((min, max))),
                domain.resolve(Some(max)),
                "{domain:?} over ({min}, {max})"
            );
        }
        assert_eq!(domain.resolve_range(None), domain.resolve(None));
    }
}

#[test]
fn auto_range_rounds_both_ends_outward() {
    let auto = YDomain::auto();
    assert_eq!(
        auto,
        YDomain::AutoRange {
            nice: true,
            min_span: 1.0
        }
    );
    assert_eq!(auto.resolve_range(Some((3.2, 87.0))), (0.0, 100.0));
    assert_eq!(auto.resolve_range(Some((-40.0, 12.0))), (-40.0, 20.0));
    // A reversed pair is the same range.
    assert_eq!(auto.resolve_range(Some((87.0, 3.2))), (0.0, 100.0));

    // The ends land on ticks.
    for data in [(3.2, 87.0), (-40.0, 12.0)] {
        let (lo, hi) = auto.resolve_range(Some(data));
        let ticks = ticks(lo, hi, 3);
        assert_eq!(
            (ticks.first().copied(), ticks.last().copied()),
            (Some(lo), Some(hi)),
            "ticks {ticks:?} over ({lo}, {hi})"
        );
    }

    // The range always covers the data.
    for (min, max) in [
        (0.31, 0.74),
        (-0.002, 0.0031),
        (1200.0, 98000.0),
        (-7.5, -7.4),
    ] {
        let (lo, hi) = auto.resolve_range(Some((min, max)));
        assert!(lo <= min && hi >= max, "({lo}, {hi}) misses ({min}, {max})");
        assert_usable((lo, hi));
    }
}

#[test]
fn auto_range_without_nice_spans_the_data_exactly() {
    let raw = YDomain::AutoRange {
        nice: false,
        min_span: 1.0,
    };
    assert_eq!(raw.resolve_range(Some((3.2, 87.0))), (3.2, 87.0));
    assert_eq!(raw.resolve_range(Some((-40.0, 12.0))), (-40.0, 12.0));
}

#[test]
fn a_flat_range_widens_about_its_middle() {
    assert_eq!(YDomain::auto().resolve_range(Some((5.0, 5.0))), (4.5, 5.5));
    let raw = YDomain::AutoRange {
        nice: false,
        min_span: 4.0,
    };
    assert_eq!(raw.resolve_range(Some((10.0, 11.0))), (8.5, 12.5));
    let none = YDomain::AutoRange {
        nice: true,
        min_span: 0.0,
    };
    assert_usable(none.resolve_range(Some((3.0, 3.0))));
}

#[test]
fn an_empty_range_spans_zero_to_min_span() {
    assert_eq!(YDomain::auto().resolve_range(None), (0.0, 1.0));
    let wide = YDomain::AutoRange {
        nice: false,
        min_span: 40.0,
    };
    assert_eq!(wide.resolve_range(None), (0.0, 40.0));
    let none = YDomain::AutoRange {
        nice: true,
        min_span: 0.0,
    };
    assert_usable(none.resolve_range(None));
}

#[test]
fn non_finite_input_stays_usable() {
    let auto = YDomain::auto();
    // A non-finite end is ignored; the other stands for both.
    assert_eq!(auto.resolve_range(Some((f32::NAN, 5.0))), (4.5, 5.5));
    assert_eq!(auto.resolve_range(Some((5.0, f32::INFINITY))), (4.5, 5.5));
    assert_eq!(
        auto.resolve_range(Some((f32::NAN, f32::NEG_INFINITY))),
        auto.resolve_range(None)
    );
    let odd = [
        YDomain::AutoRange {
            nice: true,
            min_span: f32::NAN,
        },
        YDomain::AutoRange {
            nice: true,
            min_span: f32::INFINITY,
        },
        YDomain::AutoRange {
            nice: false,
            min_span: -3.0,
        },
    ];
    let data = [
        None,
        Some((f32::MIN, f32::MAX)),
        Some((f32::MAX, f32::MAX)),
        Some((1.0e-30, 2.0e-30)),
        Some((f32::NAN, f32::NAN)),
        Some((0.0, 0.0)),
    ];
    for domain in odd.into_iter().chain([YDomain::auto()]) {
        for d in data {
            assert_usable(domain.resolve_range(d));
        }
        for max in [None, Some(f32::NAN), Some(f32::MAX), Some(-4.0)] {
            assert_usable(domain.resolve(max));
        }
    }
}

#[test]
fn resolve_on_auto_range_runs_from_zero() {
    let auto = YDomain::auto();
    assert_eq!(
        auto.resolve(Some(87.0)),
        auto.resolve_range(Some((0.0, 87.0)))
    );
    assert_eq!(auto.resolve(Some(87.0)), (0.0, 100.0));
    assert_eq!(auto.resolve(Some(-12.0)), (-15.0, 0.0));
    assert_eq!(auto.resolve(None), auto.resolve_range(None));
    assert_eq!(auto.resolve(Some(f32::NAN)), auto.resolve_range(None));
}

#[test]
fn auto_range_covers_ends_just_outside_a_round_number() {
    let auto = YDomain::auto();
    for (min, max) in [
        (0.499_999_97_f32, 1.5_f32),
        (3.2, 100.000_01),
        (-0.000_01, 87.0),
    ] {
        let (lo, hi) = auto.resolve_range(Some((min, max)));
        assert!(lo <= min && hi >= max, "({lo}, {hi}) misses ({min}, {max})");
        assert_usable((lo, hi));
    }
}

#[test]
fn auto_range_keeps_ends_that_are_already_round() {
    let auto = YDomain::auto();
    for range in [
        (0.0_f32, 100.0_f32),
        (10.0, 13.0),
        (-40.0, 20.0),
        (2.5, 12.5),
    ] {
        assert_eq!(auto.resolve_range(Some(range)), range);
    }
}

#[test]
fn auto_range_covers_the_data_across_magnitudes() {
    let auto = YDomain::auto();
    let nudges = |v: f32| [v.next_down(), v, v.next_up()];
    let mut checked = 0;
    for exponent in -6..=9 {
        let scale = 10f32.powi(exponent);
        for mantissa in [1.0_f32, 1.3, 2.0, 2.5, 3.7, 5.0, 7.5, 9.99] {
            for span in [1.0_f32, 1.5, 4.0, 10.0, 37.0] {
                for sign in [1.0_f32, -1.0] {
                    let base = sign * mantissa * scale;
                    let far = base + span * scale;
                    for a in nudges(base) {
                        for b in nudges(far) {
                            let (min, max) = (a.min(b), a.max(b));
                            if max - min < 1.0 {
                                // Narrower than `min_span`: widened about the middle instead.
                                continue;
                            }
                            let (lo, hi) = auto.resolve_range(Some((min, max)));
                            assert!(lo <= min && hi >= max, "({lo}, {hi}) misses ({min}, {max})");
                            assert_usable((lo, hi));
                            // Rounding outward adds less than the data's own span at each end.
                            let span = max - min;
                            assert!(
                                min - lo <= span && hi - max <= span,
                                "({lo}, {hi}) is too wide for ({min}, {max})"
                            );
                            checked += 1;
                        }
                    }
                }
            }
        }
    }
    assert!(checked > 2000, "only {checked} pairs checked");
}
