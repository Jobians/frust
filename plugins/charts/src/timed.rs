//! [`TimedSeries`]: a shared handle of timestamped samples, drawn at their true time positions
//! with real gaps, and [`SeriesSource`], which lets a chart take either kind of handle.
//!
//! A write bumps the handle's revision and asks every live root to repaint
//! ([`frust::request_repaint`]); it never rebuilds or lays out, and needs no frame loop. The
//! chart never reads a clock: the newest point sets the right edge, so when writes stop the
//! picture stands still.

use std::fmt;
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::Series;
use crate::geometry::time_span;

/// A cheap-to-clone, `Send + Sync` handle to timestamped samples: `(t, value)` points, `t` in
/// seconds on any clock the writer chooses, held in non-decreasing `t`.
///
/// A push older than the newest point is inserted in order, and a push at a `t` already held
/// replaces that point's value. The points are bounded by a count (`capacity`, oldest dropped
/// first) and/or a duration (`window`: a point older than `newest - window` is dropped). A
/// non-finite time or value is rejected, not stored. Two consecutive points further apart than
/// [`gap_after`](Self::gap_after) are drawn unconnected, so a real outage shows as a gap.
///
/// Every call that changes the content bumps [`revision`](Self::revision) once and calls
/// [`frust::request_repaint`] once, however many points it wrote; a call that changes nothing
/// does neither. Reads take a short read lock; a paint holds it only for the
/// [`with_points`](Self::with_points) closure. A lock poisoned by a panicking writer stays
/// readable and writable: the points are always left consistent.
///
/// ```
/// use frust_charts::TimedSeries;
///
/// // Up to ten minutes of history, an outage longer than 30 s left unconnected.
/// let rx = TimedSeries::windowed(600.0).gap_after(30.0);
/// rx.push(12.0, 3.5);
/// rx.push(10.0, 1.0); // late, so inserted before 12.0
/// rx.push(12.0, 4.0); // same time: replaces
/// rx.with_points(|points| assert_eq!(points, [(10.0, 1.0), (12.0, 4.0)]));
/// assert_eq!(rx.latest(), Some((12.0, 4.0)));
/// ```
#[derive(Clone)]
pub struct TimedSeries {
    inner: Arc<RwLock<Timeline>>,
}

/// The points and their bounds.
struct Timeline {
    points: Vec<(f64, f32)>,
    capacity: Option<usize>,
    window: Option<f64>,
    gap_after: Option<f64>,
    revision: u64,
}

impl Timeline {
    fn empty(capacity: Option<usize>, window: Option<f64>, gap_after: Option<f64>) -> Self {
        Self {
            points: Vec::new(),
            capacity,
            window,
            gap_after,
            revision: 0,
        }
    }

    /// Insert or replace one point; whether the content changed. A point the bounds would
    /// drop at once is not inserted.
    fn insert(&mut self, t: f64, value: f32) -> bool {
        if !(t.is_finite() && value.is_finite()) {
            return false;
        }
        let i = self.points.partition_point(|p| p.0 < t);
        if let Some(point) = self.points.get_mut(i)
            && point.0 == t
        {
            if point.1 == value {
                return false;
            }
            point.1 = value;
            return true;
        }
        if let (Some(window), Some(newest)) = (self.window, self.points.last())
            && t < newest.0 - window
        {
            return false;
        }
        if i == 0 && self.capacity.is_some_and(|cap| self.points.len() >= cap) {
            return false;
        }
        self.points.insert(i, (t, value));
        self.evict();
        true
    }

    /// Drop the points the bounds no longer admit; whether any was dropped.
    fn evict(&mut self) -> bool {
        let mut drop = 0;
        if let (Some(window), Some(newest)) = (self.window, self.points.last()) {
            let start = newest.0 - window;
            drop = self.points.partition_point(|p| p.0 < start);
        }
        if let Some(cap) = self.capacity {
            drop = drop.max(self.points.len().saturating_sub(cap));
        }
        if drop == 0 {
            return false;
        }
        self.points.drain(..drop);
        true
    }
}

/// Whether `seconds` is a usable window or gap threshold: finite and above 0.
fn positive(seconds: f64) -> bool {
    seconds.is_finite() && seconds > 0.0
}

impl TimedSeries {
    /// An empty series holding at most `capacity` points, with no time window: the x axis
    /// runs from the oldest point to the newest.
    ///
    /// # Panics
    ///
    /// If `capacity` is 0.
    pub fn new(capacity: usize) -> Self {
        assert!(
            capacity >= 1,
            "TimedSeries::new: capacity must be at least 1"
        );
        Self::from_timeline(Timeline::empty(Some(capacity), None, None))
    }

    /// An empty series keeping the last `seconds` of history, with no count bound: the x axis
    /// runs over `[newest - seconds, newest]`.
    ///
    /// # Panics
    ///
    /// If `seconds` is not finite and above 0.
    pub fn windowed(seconds: f64) -> Self {
        assert!(
            positive(seconds),
            "TimedSeries::windowed: the window must be finite and above 0"
        );
        Self::from_timeline(Timeline::empty(None, Some(seconds), None))
    }

    fn from_timeline(timeline: Timeline) -> Self {
        Self {
            inner: Arc::new(RwLock::new(timeline)),
        }
    }

    fn read(&self) -> RwLockReadGuard<'_, Timeline> {
        self.inner.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> RwLockWriteGuard<'_, Timeline> {
        self.inner.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// Run `mutate`; when it reports a change, bump the revision and request one repaint
    /// (after the lock is released).
    fn mutate(&self, mutate: impl FnOnce(&mut Timeline) -> bool) {
        let changed = {
            let mut timeline = self.write();
            let changed = mutate(&mut timeline);
            if changed {
                timeline.revision = timeline.revision.wrapping_add(1);
            }
            changed
        };
        if changed {
            frust::request_repaint();
        }
    }

    /// Also bound the points by time: drop every point older than `newest - seconds`, and run
    /// the x axis over `[newest - seconds, newest]`. Shared by every clone; a change that moves
    /// what is drawn counts as a write.
    ///
    /// # Panics
    ///
    /// If `seconds` is not finite and above 0.
    pub fn window(self, seconds: f64) -> Self {
        assert!(
            positive(seconds),
            "TimedSeries::window: the window must be finite and above 0"
        );
        self.mutate(|timeline| {
            if timeline.window == Some(seconds) {
                return false;
            }
            timeline.window = Some(seconds);
            timeline.evict();
            !timeline.points.is_empty()
        });
        self
    }

    /// Leave two consecutive points unconnected when they are more than `seconds` apart, so an
    /// outage draws as a gap (default: every pair is connected). Shared by every clone; a
    /// change on a non-empty series counts as a write.
    ///
    /// # Panics
    ///
    /// If `seconds` is not finite and above 0.
    pub fn gap_after(self, seconds: f64) -> Self {
        assert!(
            positive(seconds),
            "TimedSeries::gap_after: the threshold must be finite and above 0"
        );
        self.mutate(|timeline| {
            if timeline.gap_after == Some(seconds) {
                return false;
            }
            timeline.gap_after = Some(seconds);
            !timeline.points.is_empty()
        });
        self
    }

    /// The most points this series holds; `None` when only a window bounds it.
    pub fn capacity(&self) -> Option<usize> {
        self.read().capacity
    }

    /// The time window in seconds, if any.
    pub fn window_secs(&self) -> Option<f64> {
        self.read().window
    }

    /// The gap threshold in seconds set by [`gap_after`](Self::gap_after), if any.
    pub fn gap_after_secs(&self) -> Option<f64> {
        self.read().gap_after
    }

    /// How many points are held.
    pub fn len(&self) -> usize {
        self.read().points.len()
    }

    /// Whether no point is held.
    pub fn is_empty(&self) -> bool {
        self.read().points.is_empty()
    }

    /// Add the point `(t, value)` in time order, replacing the value of a point already at
    /// `t`. A non-finite `t` or `value`, or a point the bounds would drop at once, is a no-op.
    pub fn push(&self, t: f64, value: f32) {
        self.mutate(|timeline| timeline.insert(t, value));
    }

    /// [`push`](Self::push) every point in order: one revision bump and one repaint request
    /// for the whole call, none when nothing changed.
    pub fn extend(&self, points: impl IntoIterator<Item = (f64, f32)>) {
        self.mutate(|timeline| {
            let mut changed = false;
            for (t, value) in points {
                changed |= timeline.insert(t, value);
            }
            changed
        });
    }

    /// Clear, then fill with `points` as [`extend`](Self::extend) would (loading a history
    /// range). A call that leaves the content unchanged is a no-op.
    pub fn replace(&self, points: impl IntoIterator<Item = (f64, f32)>) {
        let (capacity, window, gap_after) = {
            let timeline = self.read();
            (timeline.capacity, timeline.window, timeline.gap_after)
        };
        let mut fresh = Timeline::empty(capacity, window, gap_after);
        for (t, value) in points {
            fresh.insert(t, value);
        }
        self.mutate(|timeline| {
            if timeline.points == fresh.points {
                return false;
            }
            timeline.points = fresh.points;
            timeline.evict();
            true
        });
    }

    /// Remove every point; a no-op on an empty series.
    pub fn clear(&self) {
        self.mutate(|timeline| {
            let changed = !timeline.points.is_empty();
            timeline.points.clear();
            changed
        });
    }

    /// The newest point, `None` when the series is empty.
    pub fn latest(&self) -> Option<(f64, f32)> {
        self.read().points.last().copied()
    }

    /// The largest value; `None` when there is none. Feeds
    /// [`YDomain::resolve`](crate::YDomain::resolve).
    pub fn max(&self) -> Option<f32> {
        self.read().points.iter().map(|p| p.1).reduce(f32::max)
    }

    /// A counter that moves on every content change: compare it to skip work.
    pub fn revision(&self) -> u64 {
        self.read().revision
    }

    /// Read the points, oldest to newest, without copying, under a short read lock.
    ///
    /// Keep `f` brief (a paint): a writer on another thread waits for it to return, and a
    /// write to this same series from inside `f` deadlocks.
    pub fn with_points<R>(&self, f: impl FnOnce(&[(f64, f32)]) -> R) -> R {
        let timeline = self.read();
        f(&timeline.points)
    }

    /// Whether `self` and `other` share one set of points (a clone of the same handle).
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// The revision and the timing a drawing depends on, read under one lock.
    fn shape(&self) -> (u64, SourceShape) {
        let timeline = self.read();
        let shape = SourceShape::Timed {
            window: timeline.window,
            gap_after: timeline.gap_after,
            span: time_span(&timeline.points, timeline.window),
        };
        (timeline.revision, shape)
    }
}

impl fmt::Debug for TimedSeries {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let timeline = self.read();
        f.debug_struct("TimedSeries")
            .field("capacity", &timeline.capacity)
            .field("window", &timeline.window)
            .field("gap_after", &timeline.gap_after)
            .field("len", &timeline.points.len())
            .field("revision", &timeline.revision)
            .finish()
    }
}

/// The data behind one drawn series: an evenly spaced [`Series`] or a [`TimedSeries`].
///
/// Chart builders take `impl Into<SeriesSource>`, so either handle is passed as it is.
#[derive(Clone, Debug)]
pub enum SeriesSource {
    /// Evenly spaced slots, right-aligned by slot.
    Series(Series),
    /// Timestamped points, placed by time.
    Timed(TimedSeries),
}

impl From<Series> for SeriesSource {
    fn from(series: Series) -> Self {
        Self::Series(series)
    }
}

impl From<TimedSeries> for SeriesSource {
    fn from(series: TimedSeries) -> Self {
        Self::Timed(series)
    }
}

impl SeriesSource {
    /// The handle's revision.
    pub fn revision(&self) -> u64 {
        match self {
            Self::Series(series) => series.revision(),
            Self::Timed(series) => series.revision(),
        }
    }

    /// The handle's largest sample; `None` when there is none.
    pub fn max(&self) -> Option<f32> {
        match self {
            Self::Series(series) => series.max(),
            Self::Timed(series) => series.max(),
        }
    }

    /// Whether `self` and `other` are clones of one handle.
    pub fn ptr_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Series(a), Self::Series(b)) => a.ptr_eq(b),
            (Self::Timed(a), Self::Timed(b)) => a.ptr_eq(b),
            _ => false,
        }
    }

    /// Whether the points are placed by time.
    pub fn is_timed(&self) -> bool {
        matches!(self, Self::Timed(_))
    }

    /// The revision and what else a drawing's x mapping depends on, read consistently.
    pub(crate) fn shape(&self) -> (u64, SourceShape) {
        match self {
            Self::Series(series) => (
                series.revision(),
                SourceShape::Slots {
                    capacity: series.capacity(),
                },
            ),
            Self::Timed(series) => series.shape(),
        }
    }
}

/// What a source's x mapping depends on besides its revision: part of a widget's cache key.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SourceShape {
    /// Slots spaced by capacity.
    Slots { capacity: usize },
    /// Points placed over a time span.
    Timed {
        window: Option<f64>,
        gap_after: Option<f64>,
        span: Option<(f64, f64)>,
    },
}
