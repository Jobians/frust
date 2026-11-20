//! Opt-in pointer scrubbing for `time_series`: the nearest-sample search and the event a
//! chart reports while a drag crosses it.
//!
//! The search runs on x alone, against the plot rect the chart draws in. A pointer on a drawn
//! line (between two connected samples) snaps to the nearer of the two; elsewhere, over a gap
//! or past either end of the data, it snaps to the nearest present sample only when that
//! sample is within [`REACH`] logical pixels. The first layer (in the order the series were
//! given) with a sample in reach places the crosshair; every layer then reports its own sample
//! on the crosshair, or `None` when it has none there.

use frust::authoring::{Point, Rect};

use crate::SeriesSource;
use crate::geometry::{snap, time_span, x_for_slot, x_for_time};

/// How far, in logical pixels, a pointer off any drawn line still reaches a sample: about a
/// fingertip.
pub(crate) const REACH: f64 = 24.0;

/// How far from the crosshair a layer's sample may sit and still be read as on it: half a
/// logical pixel, so only a sample drawn on the crosshair's own column counts.
const ON_CROSSHAIR: f64 = 0.5;

/// Where a layer's slots sit across the plot: spread by capacity (a line or area), or as
/// block columns of a fixed width and gap, right-aligned (a timed layer is always placed by
/// time).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Placement {
    /// Slots spread across the plot by capacity.
    Slots,
    /// One block column per slot, `width` wide with `gap` between, as
    /// [`block_columns`](crate::block_columns) lays them out.
    Columns {
        /// The column width in logical pixels, before snapping.
        width: f64,
        /// The space between columns in logical pixels, before snapping.
        gap: f64,
    },
}

/// Which sample a [`ScrubEvent`] points at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ScrubSample {
    /// A [`Series`](crate::Series) slot, as [`SlotsView::get`](crate::SlotsView::get) indexes
    /// it when the event is made (0 the oldest). A later push moves every slot down by one.
    Index(usize),
    /// A [`TimedSeries`](crate::TimedSeries) point's time.
    Time(f64),
}

/// What a scrubbing chart reports for one pointer position (see `TimeSeriesView::on_scrub`).
#[derive(Clone, Debug, PartialEq)]
pub struct ScrubEvent {
    /// The sample under the crosshair, taken from the first layer with one in reach; `None`
    /// when no layer has a sample in reach, in which case every value is `None` too.
    pub sample: Option<ScrubSample>,
    /// One value per series, in the order the series were given (a mirrored plot's upper
    /// series first): that series' sample on the crosshair, `None` when it has none there.
    pub values: Vec<Option<f32>>,
    /// The crosshair's x, widget-local: the snapped sample's x, or the pointer's x (kept
    /// inside the plot) when nothing is in reach.
    pub x: f64,
    /// The pointer's position, widget-local, as the event delivered it.
    pub position: Point,
}

/// One present sample found by the search.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Hit {
    sample: ScrubSample,
    x: f64,
    value: f32,
}

/// A single pass over one layer's drawn samples, in x order, tracking the nearest sample to
/// `target` and whether `target` lies on a drawn line.
struct Scan {
    target: f64,
    nearest: Option<(f64, Hit)>,
    on_line: bool,
    previous: Option<f64>,
}

impl Scan {
    fn new(target: f64) -> Self {
        Self {
            target,
            nearest: None,
            on_line: false,
            previous: None,
        }
    }

    /// A drawn sample; `joined` when the line runs to it from the previous drawn sample.
    fn sample(&mut self, hit: Hit, joined: bool) {
        if joined
            && let Some(previous) = self.previous
            && (previous..=hit.x).contains(&self.target)
        {
            self.on_line = true;
        }
        let distance = (hit.x - self.target).abs();
        if self.nearest.is_none_or(|(best, _)| distance < best) {
            self.nearest = Some((distance, hit));
        }
        self.previous = Some(hit.x);
    }

    /// A slot or point that is not drawn: it breaks the line.
    fn gap(&mut self) {
        self.previous = None;
    }

    /// The nearest sample, if it is within `reach` or (with `along_line`) the target lies on
    /// a drawn line.
    fn finish(self, reach: f64, along_line: bool) -> Option<Hit> {
        let (distance, hit) = self.nearest?;
        (distance <= reach || (along_line && self.on_line)).then_some(hit)
    }
}

/// The nearest drawn sample of `source` to `x` in `plot`: accepted within `reach`, or anywhere
/// on a drawn line when `along_line`. Slots and points are placed exactly as the chart draws
/// them (`placement` says how a [`Series`](crate::Series)' slots are drawn), so a sample found
/// here sits on the plotted line or column.
fn nearest(
    (source, placement): (&SeriesSource, Placement),
    plot: Rect,
    x: f64,
    reach: f64,
    along_line: bool,
) -> Option<Hit> {
    let mut scan = Scan::new(x);
    let mut reach = reach;
    match source {
        SeriesSource::Series(series) => {
            let capacity = series.capacity();
            let column_x = match placement {
                Placement::Slots => None,
                Placement::Columns { width, gap } => {
                    let width = snap(width);
                    if width < 1.0 {
                        return None;
                    }
                    // Over a wide column, the whole column reaches its sample.
                    reach = reach.max(width / 2.0);
                    Some((width, width + snap(gap).max(0.0)))
                }
            };
            series.with_slots(|slots| {
                let len = slots.len();
                let (right, left) = (snap(plot.x1), snap(plot.x0));
                let mut joined = false;
                for (index, slot) in slots.iter().enumerate() {
                    let at = match column_x {
                        None => Some(x_for_slot(index, len, capacity, plot)),
                        Some((width, pitch)) => {
                            let x1 = right - (len - 1 - index) as f64 * pitch;
                            // A column that would start left of the plot is not drawn.
                            (x1 - width >= left).then_some(x1 - width / 2.0)
                        }
                    };
                    match (slot.filter(|v| v.is_finite()), at) {
                        (Some(value), Some(at)) => {
                            let hit = Hit {
                                sample: ScrubSample::Index(index),
                                x: at,
                                value,
                            };
                            scan.sample(hit, joined);
                            joined = true;
                        }
                        _ => {
                            scan.gap();
                            joined = false;
                        }
                    }
                }
            });
        }
        SeriesSource::Timed(series) => {
            let (window, gap_after) = (series.window_secs(), series.gap_after_secs());
            let gap_after = gap_after.filter(|g| g.is_finite() && *g >= 0.0);
            series.with_points(|points| {
                let Some(span) = time_span(points, window) else {
                    return;
                };
                let mut previous: Option<f64> = None;
                for &(t, value) in points {
                    let drawn =
                        t.is_finite() && value.is_finite() && (span.0..=span.1).contains(&t);
                    if !drawn {
                        scan.gap();
                        previous = None;
                        continue;
                    }
                    let joined = match (previous, gap_after) {
                        (Some(previous), Some(gap_after)) => t - previous <= gap_after,
                        (Some(_), None) => true,
                        (None, _) => false,
                    };
                    let hit = Hit {
                        sample: ScrubSample::Time(t),
                        x: x_for_time(t, span, plot),
                        value,
                    };
                    scan.sample(hit, joined);
                    previous = Some(t);
                }
            });
        }
    }
    scan.finish(reach, along_line)
}

/// The scrub at the widget-local `position` over `layers` (each a source and how its slots are
/// placed, in layer order) drawn in `plot`.
pub(crate) fn scrub_at<'a>(
    layers: impl Iterator<Item = (&'a SeriesSource, Placement)> + Clone,
    plot: Rect,
    position: Point,
) -> ScrubEvent {
    let target = clamp_x(position.x, plot);
    let anchor = layers
        .clone()
        .find_map(|layer| nearest(layer, plot, target, REACH, true));
    let Some(anchor) = anchor else {
        return ScrubEvent {
            sample: None,
            values: layers.map(|_| None).collect(),
            x: target,
            position,
        };
    };
    let values = layers
        .map(|layer| nearest(layer, plot, anchor.x, ON_CROSSHAIR, false).map(|hit| hit.value))
        .collect();
    ScrubEvent {
        sample: Some(anchor.sample),
        values,
        x: anchor.x,
        position,
    }
}

/// `x` kept inside the plot's horizontal extent; the plot's left edge for a non-finite `x`.
fn clamp_x(x: f64, plot: Rect) -> f64 {
    if !x.is_finite() {
        return plot.x0;
    }
    x.clamp(plot.x0, plot.x1.max(plot.x0))
}

#[cfg(test)]
mod tests {
    use super::{Placement, REACH, ScrubSample, scrub_at};
    use crate::{Series, SeriesSource, TimedSeries};
    use frust::authoring::{Point, Rect};

    const PLOT: Rect = Rect::new(0.0, 0.0, 300.0, 60.0);

    fn series(capacity: usize, slots: &[Option<f32>]) -> SeriesSource {
        let series = Series::new(capacity);
        series.extend(slots.iter().copied());
        series.into()
    }

    fn at(sources: &[SeriesSource], x: f64) -> (Option<ScrubSample>, Vec<Option<f32>>, f64) {
        let layers = sources.iter().map(|s| (s, Placement::Slots));
        let event = scrub_at(layers, PLOT, Point::new(x, 10.0));
        (event.sample, event.values, event.x)
    }

    #[test]
    fn a_pointer_on_a_line_snaps_to_the_nearer_sample() {
        // Capacity 4 over 300 px: slots at x 0, 100, 200, 300.
        let s = [series(4, &[Some(1.0), Some(2.0), Some(3.0), Some(4.0)])];
        assert_eq!(
            at(&s, 140.0),
            (Some(ScrubSample::Index(1)), vec![Some(2.0)], 100.0)
        );
        assert_eq!(
            at(&s, 160.0),
            (Some(ScrubSample::Index(2)), vec![Some(3.0)], 200.0)
        );
        // Past the plot the pointer is kept at its edge.
        assert_eq!(at(&s, 900.0).0, Some(ScrubSample::Index(3)));
        assert_eq!(at(&s, -50.0).0, Some(ScrubSample::Index(0)));
    }

    #[test]
    fn a_gap_reaches_only_a_sample_within_reach() {
        // Slot 2 (x 200) is a gap; its neighbours are 100 px away.
        let s = [series(4, &[Some(1.0), Some(2.0), None, Some(4.0)])];
        assert_eq!(at(&s, 200.0), (None, vec![None], 200.0));
        let near = 100.0 + REACH - 1.0;
        assert_eq!(at(&s, near).0, Some(ScrubSample::Index(1)));
        // A dense series skips a one-slot gap to its nearest neighbour.
        let dense = [series(31, &[Some(5.0); 31])];
        let dense_gap = {
            let slots: Vec<Option<f32>> = (0..31)
                .map(|i| if i == 15 { None } else { Some(i as f32) })
                .collect();
            [series(31, &slots)]
        };
        assert_eq!(at(&dense, 150.0).0, Some(ScrubSample::Index(15)));
        let (sample, values, x) = at(&dense_gap, 150.0);
        assert!(matches!(sample, Some(ScrubSample::Index(14 | 16))));
        assert!(values[0].is_some());
        assert!((x - 150.0).abs() <= 10.0 + 1e-9);
    }

    #[test]
    fn layers_report_their_own_sample_on_the_crosshair() {
        let upper = series(4, &[Some(1.0), Some(2.0), Some(3.0), Some(4.0)]);
        let lower = series(4, &[Some(10.0), None, Some(30.0), Some(40.0)]);
        let both = [upper, lower];
        assert_eq!(at(&both, 95.0).1, vec![Some(2.0), None]);
        assert_eq!(at(&both, 205.0).1, vec![Some(3.0), Some(30.0)]);
        // The first layer with nothing in reach hands the crosshair to the next one.
        let empty = Series::new(4).into();
        let lower = series(4, &[Some(10.0), Some(20.0)]);
        assert_eq!(
            at(&[empty, lower], 290.0),
            (Some(ScrubSample::Index(1)), vec![None, Some(20.0)], 300.0)
        );
    }

    #[test]
    fn timed_points_are_found_by_time_and_a_time_gap_breaks_the_line() {
        let timed = TimedSeries::new(8).gap_after(2.0);
        timed.extend([(0.0, 1.0), (1.0, 2.0), (2.0, 3.0), (10.0, 4.0)]);
        let s = [SeriesSource::from(timed)];
        // Span 0..10 over 300 px: 30 px a second.
        assert_eq!(
            at(&s, 40.0),
            (Some(ScrubSample::Time(1.0)), vec![Some(2.0)], 30.0)
        );
        // Inside the outage, far from either side: nothing.
        assert_eq!(at(&s, 180.0).0, None);
        // Near the point after it: reached.
        assert_eq!(at(&s, 290.0).0, Some(ScrubSample::Time(10.0)));
    }

    #[test]
    fn block_columns_are_found_at_their_centres() {
        // 10 px columns with a 2 px gap, right-aligned: the newest spans 290..300.
        let s = series(8, &[Some(1.0), None, Some(3.0)]);
        let columns = Placement::Columns {
            width: 10.0,
            gap: 2.0,
        };
        let scrub = |x: f64| {
            let event = scrub_at([(&s, columns)].into_iter(), PLOT, Point::new(x, 10.0));
            (event.sample, event.x)
        };
        assert_eq!(scrub(297.0), (Some(ScrubSample::Index(2)), 295.0));
        // The gap column (278..288) is skipped to its nearest neighbour.
        assert_eq!(scrub(284.0), (Some(ScrubSample::Index(2)), 295.0));
        assert_eq!(scrub(272.0), (Some(ScrubSample::Index(0)), 271.0));
        assert_eq!(scrub(100.0).0, None);
    }

    #[test]
    fn empty_sources_report_nothing() {
        assert_eq!(at(&[], 50.0), (None, vec![], 50.0));
        let empty = [SeriesSource::from(TimedSeries::new(4))];
        assert_eq!(at(&empty, 50.0), (None, vec![None], 50.0));
    }
}
