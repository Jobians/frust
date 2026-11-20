//! Polar geometry as pure functions, shared by the round chart families: circular arcs and
//! annular sectors as [`BezPath`]s (the scene has no arc command), and the point/angle
//! helpers a hit test needs. Nothing here paints or holds state.
//!
//! Angle convention: radians, 0 at 3 o'clock, positive clockwise on screen. Coordinates are
//! y down, so this is the plain `(cos a, sin a)` direction: `PI / 2` points straight down
//! (6 o'clock) and `-PI / 2` straight up (12 o'clock). A sweep is signed: positive runs
//! clockwise from its start angle, negative counter-clockwise. A sweep beyond a full turn is
//! clamped to one turn.
//!
//! Arcs are cubic Bezier approximations, at most a quarter turn per segment, with the
//! standard control distance `4/3 * tan(theta / 4) * radius` for a segment of angle `theta`.
//! Each segment's endpoints and midpoint lie on the circle; between them the curve strays by
//! at most about 0.03 % of the radius. A non-finite input, a zero sweep or a radius that is
//! not positive yields an empty path; nothing here panics.

use std::f64::consts::{FRAC_PI_2, PI, TAU};

use frust::authoring::{BezPath, Point, Vec2};

/// The point at `angle` on the circle of `radius` around `center`.
pub fn point_on_circle(center: Point, radius: f64, angle: f64) -> Point {
    center + Vec2::new(angle.cos(), angle.sin()) * radius
}

/// The angle of `point` seen from `center`, in `0..TAU` (0 at 3 o'clock, growing clockwise).
///
/// `point == center`, or a non-finite coordinate, gives 0.
pub fn angle_of(point: Point, center: Point) -> f64 {
    let d = point - center;
    let angle = d.y.atan2(d.x);
    if angle.is_finite() {
        normalize_angle(angle)
    } else {
        0.0
    }
}

/// `angle` wrapped into `0..TAU`; a non-finite angle gives 0.
pub fn normalize_angle(angle: f64) -> f64 {
    if !angle.is_finite() {
        return 0.0;
    }
    let wrapped = angle.rem_euclid(TAU);
    // `rem_euclid` can round a tiny negative angle up to exactly `TAU`.
    if wrapped >= TAU { 0.0 } else { wrapped }
}

/// Whether `angle` lies on the sweep from `start_angle` through `sweep` (signed, either
/// direction), ends included. A sweep of a full turn or more contains every angle; a zero or
/// non-finite one contains none.
pub fn sweep_contains(start_angle: f64, sweep: f64, angle: f64) -> bool {
    if !(start_angle.is_finite() && sweep.is_finite() && angle.is_finite()) || sweep == 0.0 {
        return false;
    }
    if sweep.abs() >= TAU {
        return true;
    }
    let offset = if sweep > 0.0 {
        normalize_angle(angle - start_angle)
    } else {
        normalize_angle(start_angle - angle)
    };
    offset <= sweep.abs()
}

/// The arc of the circle of `radius` around `center` from `start_angle` through `sweep`, as
/// an open path: a move to the start point, then one cubic per quarter turn or part of one.
///
/// Empty when `sweep` is zero, `radius` is not positive, or any input is non-finite.
pub fn arc_path(center: Point, radius: f64, start_angle: f64, sweep: f64) -> BezPath {
    let mut path = BezPath::new();
    let Some(sweep) = usable_sweep(sweep) else {
        return path;
    };
    if !(usable_point(center) && radius.is_finite() && radius > 0.0 && start_angle.is_finite()) {
        return path;
    }
    path.move_to(point_on_circle(center, radius, start_angle));
    arc_to(&mut path, center, radius, start_angle, sweep);
    path
}

/// A closed annular sector: the ring between `inner_radius` and `outer_radius` from
/// `start_angle` through `sweep`, its four corners rounded by `corner_radius`.
///
/// An `inner_radius` of 0 (or below) is a pie wedge with its tip at `center`; the tip is not
/// rounded, only the two outer corners are. Reversed radii are swapped. `corner_radius` is
/// clamped so the rounding fits: at most half the ring's thickness, and small enough that two
/// corners on one arc do not overlap (so a narrow sector gets smaller corners). A sweep of a
/// full turn draws the whole ring, inner circle wound opposite to the outer one so a nonzero
/// fill leaves the hole, with no corners.
///
/// Empty when `sweep` is zero, the outer radius is not positive, or any input is
/// non-finite.
pub fn sector_path(
    center: Point,
    inner_radius: f64,
    outer_radius: f64,
    start_angle: f64,
    sweep: f64,
    corner_radius: f64,
) -> BezPath {
    let mut path = BezPath::new();
    let Some(sweep) = usable_sweep(sweep) else {
        return path;
    };
    let finite = inner_radius.is_finite() && outer_radius.is_finite() && corner_radius.is_finite();
    if !(finite && usable_point(center) && start_angle.is_finite()) {
        return path;
    }
    let (inner, outer) = {
        let (a, b) = (inner_radius.max(0.0), outer_radius.max(0.0));
        if a <= b { (a, b) } else { (b, a) }
    };
    if outer <= 0.0 {
        return path;
    }
    let end_angle = start_angle + sweep;

    if sweep.abs() >= TAU {
        path.move_to(point_on_circle(center, outer, start_angle));
        arc_to(&mut path, center, outer, start_angle, sweep);
        path.close_path();
        if inner > 0.0 {
            path.move_to(point_on_circle(center, inner, end_angle));
            arc_to(&mut path, center, inner, end_angle, -sweep);
            path.close_path();
        }
        return path;
    }

    let (outer_corner, inner_corner) = corner_radii(inner, outer, sweep, corner_radius);
    let direction = sweep.signum();

    if outer_corner <= 0.0 {
        if inner > 0.0 {
            path.move_to(point_on_circle(center, outer, start_angle));
        } else {
            path.move_to(center);
            path.line_to(point_on_circle(center, outer, start_angle));
        }
        arc_to(&mut path, center, outer, start_angle, sweep);
        if inner > 0.0 {
            path.line_to(point_on_circle(center, inner, end_angle));
            arc_to(&mut path, center, inner, end_angle, -sweep);
        }
        path.close_path();
        return path;
    }

    // Outer corners: a circle of `outer_corner` tangent to the outer arc from inside and to
    // the radial edge, its centre `outer - outer_corner` out at `delta` past the edge.
    let delta = (outer_corner / (outer - outer_corner)).asin();
    let outer_reach = (outer - outer_corner) * delta.cos();
    let first = point_on_circle(center, outer_reach, start_angle);
    if inner > 0.0 {
        path.move_to(first);
    } else {
        path.move_to(center);
        path.line_to(first);
    }
    let start_fillet = point_on_circle(
        center,
        outer - outer_corner,
        start_angle + direction * delta,
    );
    fillet_to(
        &mut path,
        start_fillet,
        outer_corner,
        first,
        point_on_circle(center, outer, start_angle + direction * delta),
    );
    arc_to(
        &mut path,
        center,
        outer,
        start_angle + direction * delta,
        sweep - 2.0 * direction * delta,
    );
    let end_fillet = point_on_circle(center, outer - outer_corner, end_angle - direction * delta);
    fillet_to(
        &mut path,
        end_fillet,
        outer_corner,
        point_on_circle(center, outer, end_angle - direction * delta),
        point_on_circle(center, outer_reach, end_angle),
    );

    if inner > 0.0 {
        if inner_corner > 0.0 {
            // Inner corners: a circle of `inner_corner` tangent to the inner arc from outside
            // and to the radial edge, its centre `inner + inner_corner` out.
            let delta = (inner_corner / (inner + inner_corner)).asin();
            let inner_reach = (inner + inner_corner) * delta.cos();
            let near_end = point_on_circle(center, inner_reach, end_angle);
            path.line_to(near_end);
            fillet_to(
                &mut path,
                point_on_circle(center, inner + inner_corner, end_angle - direction * delta),
                inner_corner,
                near_end,
                point_on_circle(center, inner, end_angle - direction * delta),
            );
            arc_to(
                &mut path,
                center,
                inner,
                end_angle - direction * delta,
                -(sweep - 2.0 * direction * delta),
            );
            fillet_to(
                &mut path,
                point_on_circle(
                    center,
                    inner + inner_corner,
                    start_angle + direction * delta,
                ),
                inner_corner,
                point_on_circle(center, inner, start_angle + direction * delta),
                point_on_circle(center, inner_reach, start_angle),
            );
        } else {
            path.line_to(point_on_circle(center, inner, end_angle));
            arc_to(&mut path, center, inner, end_angle, -sweep);
        }
    }
    path.close_path();
    path
}

/// The outer and inner corner radii that fit a sector, from the requested `corner_radius`.
fn corner_radii(inner: f64, outer: f64, sweep: f64, corner_radius: f64) -> (f64, f64) {
    if corner_radius <= 0.0 {
        return (0.0, 0.0);
    }
    // Two fillets on one arc meet when each takes half the sweep; past a half turn of sweep
    // the radial edges diverge and the angle no longer limits the corners.
    let half = sweep.abs() / 2.0;
    let sin_half = if half >= FRAC_PI_2 { 1.0 } else { half.sin() };
    let outer_limit = outer * sin_half / (1.0 + sin_half);
    let outer_corner = corner_radius.min((outer - inner) / 2.0).min(outer_limit);
    let outer_corner = if outer_corner.is_finite() {
        outer_corner.max(0.0)
    } else {
        0.0
    };
    let inner_limit = if sin_half >= 1.0 {
        f64::INFINITY
    } else {
        inner * sin_half / (1.0 - sin_half)
    };
    let inner_corner = if inner > 0.0 {
        outer_corner.min(inner_limit).max(0.0)
    } else {
        0.0
    };
    (outer_corner, inner_corner)
}

/// Append the arc of the circle of `radius` around `center` from `start_angle` through
/// `sweep`, assuming the path's current point is the arc's start.
fn arc_to(path: &mut BezPath, center: Point, radius: f64, start_angle: f64, sweep: f64) {
    let segments = (sweep.abs() / FRAC_PI_2).ceil().max(1.0);
    let step = sweep / segments;
    // `segments` is at most 4 for a sweep of at most a full turn.
    let count = segments as usize;
    let handle = 4.0 / 3.0 * (step / 4.0).tan() * radius;
    for index in 0..count {
        let a0 = start_angle + step * index as f64;
        let a1 = if index + 1 == count {
            start_angle + sweep
        } else {
            a0 + step
        };
        let p0 = point_on_circle(center, radius, a0);
        let p3 = point_on_circle(center, radius, a1);
        let tangent0 = Vec2::new(-a0.sin(), a0.cos());
        let tangent1 = Vec2::new(-a1.sin(), a1.cos());
        path.curve_to(p0 + tangent0 * handle, p3 - tangent1 * handle, p3);
    }
}

/// Append the short arc of the circle of `radius` around `center` from `from` (the current
/// point) to `to`, both on that circle.
fn fillet_to(path: &mut BezPath, center: Point, radius: f64, from: Point, to: Point) {
    let start = angle_of(from, center);
    let mut sweep = angle_of(to, center) - start;
    if sweep > PI {
        sweep -= TAU;
    } else if sweep < -PI {
        sweep += TAU;
    }
    if sweep == 0.0 || radius <= 0.0 {
        path.line_to(to);
        return;
    }
    arc_to(path, center, radius, start, sweep);
}

/// `sweep` clamped to one turn, or `None` when it is zero or non-finite.
fn usable_sweep(sweep: f64) -> Option<f64> {
    (sweep.is_finite() && sweep != 0.0).then(|| sweep.clamp(-TAU, TAU))
}

fn usable_point(point: Point) -> bool {
    point.x.is_finite() && point.y.is_finite()
}
