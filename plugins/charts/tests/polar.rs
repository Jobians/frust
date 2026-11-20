//! Polar geometry: arcs and sectors as cubic paths, and the angle helpers hit tests use.

use std::f64::consts::{FRAC_PI_2, PI, TAU};

use frust::authoring::{BezPath, Point, Shape};
use frust::kurbo::{ParamCurve, PathEl, PathSeg};
use frust_charts::{
    angle_of, arc_path, normalize_angle, point_on_circle, sector_path, sweep_contains,
};

const CENTER: Point = Point::new(120.0, 80.0);

fn segments(path: &BezPath) -> Vec<PathSeg> {
    path.segments().collect()
}

fn distance(p: Point) -> f64 {
    (p - CENTER).hypot()
}

fn assert_near(a: Point, b: Point, what: &str) {
    assert!((a - b).hypot() < 1e-6, "{what}: {a:?} is not {b:?}");
}

#[test]
fn arc_endpoints_and_midpoints_lie_on_the_circle() {
    let radius = 50.0;
    for degrees in [10.0_f64, 90.0, 180.0, 270.0, 360.0] {
        for (start, sign) in [(0.0, 1.0), (-FRAC_PI_2, 1.0), (1.0, -1.0)] {
            let sweep = sign * degrees.to_radians();
            let path = arc_path(CENTER, radius, start, sweep);
            let segs = segments(&path);
            assert_eq!(
                segs.len(),
                (degrees / 90.0).ceil() as usize,
                "at most a quarter turn per cubic"
            );
            assert!(segs.iter().all(|s| matches!(s, PathSeg::Cubic(_))));
            for seg in &segs {
                for t in [0.0, 0.5, 1.0] {
                    let r = distance(seg.eval(t));
                    assert!(
                        (r - radius).abs() < 1e-3,
                        "{degrees} deg from {start}: t={t} at radius {r}"
                    );
                }
                // Between those, the standard approximation strays by under 0.03 %.
                for t in [0.1, 0.25, 0.75, 0.9] {
                    let r = distance(seg.eval(t));
                    assert!((r - radius).abs() < 3e-4 * radius, "t={t} at radius {r}");
                }
            }
            let first = segs.first().unwrap().eval(0.0);
            let last = segs.last().unwrap().eval(1.0);
            assert_near(first, point_on_circle(CENTER, radius, start), "start");
            assert_near(last, point_on_circle(CENTER, radius, start + sweep), "end");
            // The arc's own midpoint, half the sweep round.
            let mid = point_on_circle(CENTER, radius, start + sweep / 2.0);
            let on_path = segs
                .iter()
                .flat_map(|s| [0.0, 0.5, 1.0].map(|t| s.eval(t)))
                .any(|p| (p - mid).hypot() < 1e-6);
            assert!(on_path, "{degrees} deg: the midpoint is a sampled point");
        }
    }
}

#[test]
fn a_positive_sweep_runs_clockwise_on_screen() {
    // From 3 o'clock, a quarter turn ends at 6 o'clock (y down).
    let path = arc_path(CENTER, 10.0, 0.0, FRAC_PI_2);
    let end = segments(&path).last().unwrap().eval(1.0);
    assert_near(end, Point::new(CENTER.x, CENTER.y + 10.0), "6 o'clock");
    assert_near(
        point_on_circle(CENTER, 10.0, -FRAC_PI_2),
        Point::new(CENTER.x, CENTER.y - 10.0),
        "12 o'clock",
    );
}

#[test]
fn degenerate_arcs_are_empty_and_never_panic() {
    for (radius, start, sweep) in [
        (10.0, 0.0, 0.0),
        (10.0, 0.0, -0.0),
        (0.0, 0.0, 1.0),
        (-5.0, 0.0, 1.0),
        (f64::NAN, 0.0, 1.0),
        (10.0, f64::INFINITY, 1.0),
        (10.0, 0.0, f64::NAN),
    ] {
        assert!(arc_path(CENTER, radius, start, sweep).elements().is_empty());
    }
    assert!(
        arc_path(Point::new(f64::NAN, 0.0), 10.0, 0.0, 1.0)
            .elements()
            .is_empty()
    );
    // A sweep beyond a full turn is one turn.
    assert_eq!(segments(&arc_path(CENTER, 10.0, 0.0, 5.0 * TAU)).len(), 4);
    assert_eq!(segments(&arc_path(CENTER, 10.0, 0.0, -5.0 * TAU)).len(), 4);
}

#[test]
fn a_sector_with_inner_radius_zero_is_a_pie_wedge() {
    let path = sector_path(CENTER, 0.0, 40.0, 0.0, FRAC_PI_2, 0.0);
    let els = path.elements();
    assert_eq!(
        els.first(),
        Some(&PathEl::MoveTo(CENTER)),
        "the wedge starts at the centre"
    );
    assert_eq!(els.last(), Some(&PathEl::ClosePath));
    let segs = segments(&path);
    // Centre to rim, the arc, and the closing edge back to the centre.
    assert!(matches!(segs[0], PathSeg::Line(_)));
    assert!(matches!(segs[1], PathSeg::Cubic(_)));
    assert_near(
        segs.last().unwrap().eval(1.0),
        CENTER,
        "closes at the centre",
    );
    let area = path.area().abs();
    let exact = PI * 40.0 * 40.0 / 4.0;
    assert!(
        (area - exact).abs() / exact < 1e-3,
        "area {area} vs {exact}"
    );
}

#[test]
fn a_ring_sector_has_the_ring_area() {
    let (inner, outer, sweep) = (20.0, 50.0, 2.0);
    let path = sector_path(CENTER, inner, outer, 0.3, sweep, 0.0);
    let exact = sweep / 2.0 * (outer * outer - inner * inner);
    let area = path.area().abs();
    assert!(
        (area - exact).abs() / exact < 1e-3,
        "area {area} vs {exact}"
    );
    // Reversed radii are swapped.
    let swapped = sector_path(CENTER, outer, inner, 0.3, sweep, 0.0);
    assert!((swapped.area().abs() - area).abs() < 1e-9);

    // A full ring leaves the hole under a nonzero fill: the circles wind opposite ways.
    let ring = sector_path(CENTER, inner, outer, 0.0, TAU, 0.0);
    let exact = PI * (outer * outer - inner * inner);
    assert!((ring.area().abs() - exact).abs() / exact < 1e-3);
    assert_eq!(ring.winding(CENTER), 0);
    assert_ne!(ring.winding(Point::new(CENTER.x + 35.0, CENTER.y)), 0);
}

#[test]
fn rounded_corners_stay_inside_the_sector() {
    let (inner, outer) = (20.0, 50.0);
    for (inner, sweep, corner) in [
        (inner, 1.5_f64, 6.0),
        (inner, -1.5, 6.0),
        (0.0, 1.0, 8.0),
        (inner, 0.05, 30.0),
        (inner, 5.5, 100.0),
    ] {
        let start = 0.4;
        let slack = 1e-9 * sweep.signum();
        let path = sector_path(CENTER, inner, outer, start, sweep, corner);
        assert_eq!(path.elements().last(), Some(&PathEl::ClosePath));
        let sharp = sector_path(CENTER, inner, outer, start, sweep, 0.0);
        assert!(
            path.area().abs() < sharp.area().abs() + 1e-6,
            "rounding only removes area"
        );
        assert!(path.area().abs() > 0.0);
        for seg in segments(&path) {
            for t in [0.0, 0.25, 0.5, 0.75, 1.0] {
                let p = seg.eval(t);
                let r = distance(p);
                // Off a segment's ends and midpoint, a cubic strays by under 0.03 %.
                assert!(r <= outer * (1.0 + 3e-4), "{p:?} outside the outer radius");
                assert!(r >= inner * (1.0 - 3e-4), "{p:?} inside the hole");
                if r > 1e-6 {
                    assert!(
                        sweep_contains(start - slack, sweep + 2.0 * slack, angle_of(p, CENTER)),
                        "{p:?} outside the sweep"
                    );
                }
            }
        }
    }
}

#[test]
fn degenerate_sectors_are_empty_and_never_panic() {
    assert!(
        sector_path(CENTER, 10.0, 20.0, 0.0, 0.0, 2.0)
            .elements()
            .is_empty()
    );
    assert!(
        sector_path(CENTER, 0.0, 0.0, 0.0, 1.0, 0.0)
            .elements()
            .is_empty()
    );
    assert!(
        sector_path(CENTER, -3.0, -1.0, 0.0, 1.0, 0.0)
            .elements()
            .is_empty()
    );
    assert!(
        sector_path(CENTER, 1.0, f64::NAN, 0.0, 1.0, 0.0)
            .elements()
            .is_empty()
    );
    assert!(
        sector_path(CENTER, 1.0, 5.0, 0.0, 1.0, f64::INFINITY)
            .elements()
            .is_empty()
    );
    // A negative sweep is the mirror image and a zero-width ring still closes.
    let back = sector_path(CENTER, 10.0, 20.0, 0.0, -1.0, 3.0);
    let ahead = sector_path(CENTER, 10.0, 20.0, -1.0, 1.0, 3.0);
    assert!((back.area().abs() - ahead.area().abs()).abs() < 1e-6);
    let thin = sector_path(CENTER, 20.0, 20.0, 0.0, 1.0, 3.0);
    assert!(thin.area().abs() < 1e-6);
}

#[test]
fn angles_wrap_and_sweeps_contain_their_span() {
    assert_eq!(angle_of(Point::new(CENTER.x + 5.0, CENTER.y), CENTER), 0.0);
    assert!((angle_of(Point::new(CENTER.x, CENTER.y + 5.0), CENTER) - FRAC_PI_2).abs() < 1e-12);
    assert!((angle_of(Point::new(CENTER.x, CENTER.y - 5.0), CENTER) - 1.5 * PI).abs() < 1e-12);
    assert_eq!(angle_of(CENTER, CENTER), 0.0);
    assert_eq!(angle_of(Point::new(f64::NAN, 0.0), CENTER), 0.0);
    for angle in [-7.0, -1e-18, 0.0, 3.0, TAU, 100.0] {
        let a = normalize_angle(angle);
        assert!((0.0..TAU).contains(&a), "{angle} wrapped to {a}");
    }
    assert_eq!(normalize_angle(f64::NAN), 0.0);
    // Round trip through a point.
    for angle in [0.2, 2.0, 4.0, 6.0] {
        let p = point_on_circle(CENTER, 30.0, angle);
        assert!((angle_of(p, CENTER) - angle).abs() < 1e-9);
    }

    assert!(sweep_contains(-0.5, 1.0, 0.25));
    assert!(sweep_contains(-0.5, 1.0, TAU - 0.25), "wraps through zero");
    assert!(!sweep_contains(-0.5, 1.0, 1.0));
    assert!(sweep_contains(0.5, -1.0, 0.0), "a negative sweep runs back");
    assert!(!sweep_contains(0.5, -1.0, 0.6));
    assert!(sweep_contains(0.0, TAU, 4.0));
    assert!(!sweep_contains(0.0, 0.0, 0.0));
    assert!(!sweep_contains(0.0, f64::NAN, 0.0));
}
