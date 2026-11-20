//! The `candlestick_chart` widget: open, high, low and close candles over evenly spaced
//! slots, coloured by whether each one closed up or down.
//!
//! [`Candles`] is a shared, fixed-capacity ring of [`Candle`] slots, newest last. A write bumps
//! the handle's revision and asks every live root to repaint ([`frust::request_repaint`]); it
//! never rebuilds or lays out, and needs no frame loop. [`Candles::update_last`] rewrites the
//! newest candle in place, for a candle still forming.
//!
//! Slots are right-aligned: the newest candle sits at the plot's right edge, each one `body
//! width` wide with `gap` between, both snapped to whole logical pixels. A candle that would
//! start left of the plot is dropped, not clipped, so the oldest give way. The wick is centred
//! on the body and `wick_width` wide (at most the body's width, at least one pixel). A body
//! shorter than one pixel (a doji) draws one pixel tall, and a candle stays inside the plot.
//! A gap slot leaves its place empty.
//!
//! The default domain is [`YDomain::auto`] over the lowest low and highest high, so the axis is
//! not anchored at zero. Colours: the caller's [`up`](CandlestickView::up) and
//! [`down`](CandlestickView::down), then the installed theme's (`tertiary` for up, `error` for
//! down; grid `outline_variant`), then [`Tone::FALLBACK_CALM`] and [`Tone::FALLBACK_CRITICAL`].
//! A candle is up when its close is at or above its open. The chart draws no text, never
//! animates, requests no frames and answers `Ignored` to every event.

use std::fmt;
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use frust::authoring::{
    BoxConstraints, BuildCtx, ChangeFlags, EventCtx, EventResult, InputEvent, LayoutCtx, PaintCtx,
    PaintScene, Point, Rect, Role, SemanticsCtx, Size, View, Widget,
};
use frust::{Color, Theme};

use crate::geometry::{Orientation, grid_rule_ys, snap};
use crate::{Tone, YDomain};

const DEFAULT_BODY_WIDTH: f32 = 6.0;
const DEFAULT_GAP: f32 = 2.0;
const DEFAULT_WICK_WIDTH: f32 = 1.0;
const DEFAULT_HEIGHT: f64 = 96.0;
const UNBOUNDED_WIDTH: f64 = 160.0;
const FALLBACK_GRID: Color = Color::from_rgb8(0xd0, 0xd4, 0xda);

/// One candle: the prices at the start and end of a period and the extremes between.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Candle {
    /// The price at the start of the period.
    pub open: f32,
    /// The highest price of the period.
    pub high: f32,
    /// The lowest price of the period.
    pub low: f32,
    /// The price at the end of the period.
    pub close: f32,
}

impl Candle {
    /// A candle from its four prices.
    pub const fn new(open: f32, high: f32, low: f32, close: f32) -> Self {
        Self {
            open,
            high,
            low,
            close,
        }
    }

    /// Whether the candle closed at or above its open.
    pub fn is_up(&self) -> bool {
        self.close >= self.open
    }

    /// Whether every field is finite and `high >= low`; a candle that is not is stored as a
    /// gap.
    fn is_valid(&self) -> bool {
        [self.open, self.high, self.low, self.close]
            .iter()
            .all(|v| v.is_finite())
            && self.high >= self.low
    }
}

/// An invalid candle is a gap.
fn sanitize(slot: Option<Candle>) -> Option<Candle> {
    slot.filter(Candle::is_valid)
}

/// A cheap-to-clone, `Send + Sync` handle to a fixed-capacity ring of evenly spaced candles.
///
/// The oldest slot is dropped when a push would exceed the capacity. A slot is an
/// `Option<Candle>`: `None` is "no candle" (a gap), and a candle with a non-finite field or
/// `high < low` is stored as a gap. Clones share one ring, so a background thread can push
/// while a chart paints from it.
///
/// Every call that changes the content bumps [`revision`](Self::revision) once and calls
/// [`frust::request_repaint`] once, however many slots it wrote; a call that changes nothing
/// does neither. A lock poisoned by a panicking writer stays readable and writable: the ring is
/// always left consistent.
#[derive(Clone)]
pub struct Candles {
    inner: Arc<RwLock<Ring>>,
}

/// The ring itself: `slots[head]` is the oldest of `len` live slots.
struct Ring {
    slots: Box<[Option<Candle>]>,
    head: usize,
    len: usize,
    revision: u64,
}

impl Ring {
    fn new(capacity: usize) -> Self {
        Self {
            slots: vec![None; capacity].into_boxed_slice(),
            head: 0,
            len: 0,
            revision: 0,
        }
    }

    fn push(&mut self, slot: Option<Candle>) {
        let cap = self.slots.len();
        if self.len < cap {
            self.slots[(self.head + self.len) % cap] = slot;
            self.len += 1;
        } else {
            self.slots[self.head] = slot;
            self.head = (self.head + 1) % cap;
        }
    }

    fn view(&self) -> CandleSlots<'_> {
        let cap = self.slots.len();
        let end = self.head + self.len;
        if end <= cap {
            CandleSlots {
                first: &self.slots[self.head..end],
                second: &[],
            }
        } else {
            CandleSlots {
                first: &self.slots[self.head..],
                second: &self.slots[..end - cap],
            }
        }
    }

    /// The `(lowest low, highest high)` of the present candles.
    fn extent(&self) -> Option<(f32, f32)> {
        extent_of(self.view().iter().flatten())
    }
}

impl Candles {
    /// An empty ring holding at most `capacity` slots.
    ///
    /// # Panics
    ///
    /// If `capacity` is 0.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity >= 1, "Candles::new: capacity must be at least 1");
        Self {
            inner: Arc::new(RwLock::new(Ring::new(capacity))),
        }
    }

    fn read(&self) -> RwLockReadGuard<'_, Ring> {
        self.inner.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> RwLockWriteGuard<'_, Ring> {
        self.inner.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// Run `mutate`; when it reports a change, bump the revision and request one repaint
    /// (after the lock is released).
    fn mutate(&self, mutate: impl FnOnce(&mut Ring) -> bool) {
        let changed = {
            let mut ring = self.write();
            let changed = mutate(&mut ring);
            if changed {
                ring.revision = ring.revision.wrapping_add(1);
            }
            changed
        };
        if changed {
            frust::request_repaint();
        }
    }

    /// The most slots this ring holds.
    pub fn capacity(&self) -> usize {
        self.read().slots.len()
    }

    /// How many slots are filled (gaps included), at most [`capacity`](Self::capacity).
    pub fn len(&self) -> usize {
        self.read().len
    }

    /// Whether no slot has been pushed since creation or the last [`clear`](Self::clear).
    pub fn is_empty(&self) -> bool {
        self.read().len == 0
    }

    /// Append a candle (an invalid one is stored as a gap), dropping the oldest when full.
    pub fn push(&self, candle: Candle) {
        self.push_slot(Some(candle));
    }

    /// Append a gap: a slot with no candle.
    pub fn push_gap(&self) {
        self.push_slot(None);
    }

    /// Append one slot: `None` is a gap, an invalid candle is stored as a gap.
    pub fn push_slot(&self, slot: Option<Candle>) {
        self.mutate(|ring| {
            ring.push(sanitize(slot));
            true
        });
    }

    /// Append every slot in order: one revision bump and one repaint request for the whole
    /// call, none when `slots` is empty.
    pub fn extend(&self, slots: impl IntoIterator<Item = Option<Candle>>) {
        self.mutate(|ring| {
            let mut changed = false;
            for slot in slots {
                ring.push(sanitize(slot));
                changed = true;
            }
            changed
        });
    }

    /// Clear, then fill with `slots`, keeping the newest [`capacity`](Self::capacity) of them
    /// (loading a history range). A call that leaves the content unchanged is a no-op.
    pub fn replace(&self, slots: impl IntoIterator<Item = Option<Candle>>) {
        let mut fresh = Ring::new(self.capacity());
        for slot in slots {
            fresh.push(sanitize(slot));
        }
        self.mutate(|ring| {
            if ring.view().iter().eq(fresh.view().iter()) {
                return false;
            }
            fresh.revision = ring.revision;
            *ring = fresh;
            true
        });
    }

    /// Rewrite the newest slot with `candle`, for a candle that is still forming (an invalid
    /// one makes the slot a gap). On an empty ring it pushes. Rewriting a slot to what it
    /// already holds is a no-op.
    pub fn update_last(&self, candle: Candle) {
        let slot = sanitize(Some(candle));
        self.mutate(|ring| {
            if ring.len == 0 {
                ring.push(slot);
                return true;
            }
            let cap = ring.slots.len();
            let last = (ring.head + ring.len - 1) % cap;
            if ring.slots[last] == slot {
                return false;
            }
            ring.slots[last] = slot;
            true
        });
    }

    /// Remove every slot; a no-op on an empty ring.
    pub fn clear(&self) {
        self.mutate(|ring| {
            let changed = ring.len > 0;
            ring.head = 0;
            ring.len = 0;
            changed
        });
    }

    /// The newest slot's candle: `None` when the ring is empty or the newest slot is a gap.
    pub fn latest(&self) -> Option<Candle> {
        let ring = self.read();
        ring.view().iter().next_back().flatten()
    }

    /// The lowest low, ignoring gaps; `None` when there is none.
    pub fn min_low(&self) -> Option<f32> {
        self.read().extent().map(|(low, _)| low)
    }

    /// The highest high, ignoring gaps; `None` when there is none.
    pub fn max_high(&self) -> Option<f32> {
        self.read().extent().map(|(_, high)| high)
    }

    /// A counter that moves on every content change: compare it to skip work.
    pub fn revision(&self) -> u64 {
        self.read().revision
    }

    /// Read the slots oldest to newest without copying, under a short read lock.
    ///
    /// Keep `f` brief (a paint): a writer on another thread waits for it to return, and a
    /// write to this same ring from inside `f` deadlocks.
    pub fn with_slots<R>(&self, f: impl FnOnce(CandleSlots<'_>) -> R) -> R {
        let ring = self.read();
        f(ring.view())
    }

    /// Whether `self` and `other` share one ring (a clone of the same handle).
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl fmt::Debug for Candles {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ring = self.read();
        f.debug_struct("Candles")
            .field("capacity", &ring.slots.len())
            .field("len", &ring.len)
            .field("revision", &ring.revision)
            .finish()
    }
}

/// Borrowed read access to a [`Candles`] ring's slots, oldest to newest, from
/// [`Candles::with_slots`].
///
/// The ring may wrap, so the slots are two contiguous runs; [`get`](Self::get) and
/// [`iter`](Self::iter) hide the seam.
#[derive(Clone, Copy, Debug)]
pub struct CandleSlots<'a> {
    first: &'a [Option<Candle>],
    second: &'a [Option<Candle>],
}

impl<'a> CandleSlots<'a> {
    /// How many slots there are, gaps included.
    pub fn len(&self) -> usize {
        self.first.len() + self.second.len()
    }

    /// Whether there are no slots.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The slot at `index` (0 is the oldest): `None` past the end, `Some(None)` for a gap.
    pub fn get(&self, index: usize) -> Option<Option<Candle>> {
        match index.checked_sub(self.first.len()) {
            None => self.first.get(index).copied(),
            Some(rest) => self.second.get(rest).copied(),
        }
    }

    /// Every slot, oldest to newest.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = Option<Candle>> + 'a {
        self.first.iter().chain(self.second.iter()).copied()
    }
}

#[derive(Clone, Debug)]
struct Spec {
    candles: Candles,
    domain: YDomain,
    body_width: f32,
    gap: f32,
    wick_width: f32,
    up: Option<Color>,
    down: Option<Color>,
    grid: Vec<f32>,
    grid_color: Option<Color>,
    height: Option<f32>,
    semantics: Option<String>,
}

impl Spec {
    /// What a widget holding `old` must redo to show `self`.
    fn changes(&self, old: &Self) -> ChangeFlags {
        if self.height != old.height {
            return ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        if !self.candles.ptr_eq(&old.candles)
            || self.domain != old.domain
            || self.body_width != old.body_width
            || self.gap != old.gap
            || self.wick_width != old.wick_width
            || self.up != old.up
            || self.down != old.down
            || self.grid != old.grid
            || self.grid_color != old.grid_color
            || self.semantics != old.semantics
        {
            return ChangeFlags::PAINT;
        }
        ChangeFlags::NONE
    }
}

/// A candlestick chart over a [`Candles`] handle. Build one with [`candlestick_chart`].
#[derive(Clone, Debug)]
pub struct CandlestickView {
    spec: Spec,
}

/// A candlestick chart of `candles`, drawn from the handle at every paint so a write shows
/// with no rebuild: 6 px bodies 2 px apart, 1 px wicks, an automatic domain, expanding to the
/// available width.
pub fn candlestick_chart(candles: Candles) -> CandlestickView {
    CandlestickView {
        spec: Spec {
            candles,
            domain: YDomain::auto(),
            body_width: DEFAULT_BODY_WIDTH,
            gap: DEFAULT_GAP,
            wick_width: DEFAULT_WICK_WIDTH,
            up: None,
            down: None,
            grid: Vec::new(),
            grid_color: None,
            height: None,
            semantics: None,
        },
    }
}

impl CandlestickView {
    /// The value range (default [`YDomain::auto`]: the lowest low to the highest high, with
    /// nice bounds, not from zero).
    pub fn domain(mut self, domain: YDomain) -> Self {
        self.spec.domain = domain;
        self
    }

    /// A candle body's width in logical px, snapped to a whole pixel (default 6).
    pub fn body_width(mut self, width: f32) -> Self {
        self.spec.body_width = width;
        self
    }

    /// The space between candles in logical px, snapped to a whole pixel (default 2).
    pub fn gap(mut self, gap: f32) -> Self {
        self.spec.gap = gap;
        self
    }

    /// A wick's width in logical px, snapped to a whole pixel and kept between 1 and the
    /// body's width (default 1).
    pub fn wick_width(mut self, width: f32) -> Self {
        self.spec.wick_width = width;
        self
    }

    /// The colour of a candle that closed at or above its open (default: the theme's
    /// `tertiary`).
    pub fn up(mut self, color: Color) -> Self {
        self.spec.up = Some(color);
        self
    }

    /// The colour of a candle that closed below its open (default: the theme's `error`).
    pub fn down(mut self, color: Color) -> Self {
        self.spec.down = Some(color);
        self
    }

    /// Horizontal grid rules at these fractions of the plot's height, 0.0 the bottom and 1.0
    /// the top (default none), drawn under the candles.
    pub fn grid(mut self, fractions: impl Into<Vec<f32>>) -> Self {
        self.spec.grid = fractions.into();
        self
    }

    /// The grid rule colour (default: the theme's `outline_variant`).
    pub fn grid_color(mut self, color: Color) -> Self {
        self.spec.grid_color = Some(color);
        self
    }

    /// A fixed height in logical px (default: the available height when bounded, else 96).
    /// The width always expands to the available width.
    pub fn height(mut self, height: f32) -> Self {
        self.spec.height = Some(height);
        self
    }

    /// Give the chart an image node with this label; without one it has no node.
    pub fn semantics(mut self, label: impl Into<String>) -> Self {
        self.spec.semantics = Some(label.into());
        self
    }
}

impl<State: 'static> View<State> for CandlestickView {
    type Element = CandlestickWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> CandlestickWidget {
        CandlestickWidget {
            spec: self.spec.clone(),
            cache: None,
        }
    }

    fn rebuild(
        &self,
        _prev: &Self,
        element: &mut CandlestickWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let flags = self.spec.changes(&element.spec);
        if !flags.is_empty() {
            element.spec = self.spec.clone();
            element.cache = None;
        }
        flags
    }
}

/// One candle's geometry in widget-local coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Shape {
    body: Rect,
    wick: Rect,
    up: bool,
}

/// The inputs the cached shapes were built from.
#[derive(Clone, Debug, PartialEq)]
struct CacheKey {
    revision: u64,
    plot: Rect,
}

/// The retained widget behind a [`CandlestickView`].
pub struct CandlestickWidget {
    spec: Spec,
    cache: Option<(CacheKey, Vec<Shape>)>,
}

/// The shapes of `candles` in `plot`, oldest first.
fn shapes(spec: &Spec, plot: Rect) -> Vec<Shape> {
    let width = snap(f64::from(spec.body_width));
    if !(plot.width() > 0.0 && plot.height() > 0.0) || width < 1.0 {
        return Vec::new();
    }
    let wick_width = snap(f64::from(spec.wick_width)).clamp(1.0, width);
    let wick_offset = ((width - wick_width) / 2.0).floor();
    let pitch = width + snap(f64::from(spec.gap)).max(0.0);
    let (right, left_limit) = (snap(plot.x1), snap(plot.x0));
    let (top_limit, bottom_limit) = (snap(plot.y0), snap(plot.y1));
    let capacity = spec.candles.capacity();
    let extent = spec
        .candles
        .with_slots(|slots| extent_of(slots.iter().flatten()));
    let domain = spec.domain.resolve_range(extent);
    // A whole-pixel row span of `a..b` in the plot, at least one pixel tall.
    let rows = |a: f32, b: f32| {
        let y = |v| snap(Orientation::Upright.y_for(v, domain, plot));
        let (top, bottom) = (y(a).min(y(b)), y(a).max(y(b)));
        let top = top.clamp(top_limit, (bottom_limit - 1.0).max(top_limit));
        (top, bottom.max(top + 1.0).min(bottom_limit).max(top + 1.0))
    };
    let mut out = Vec::new();
    spec.candles.with_slots(|slots| {
        for (age, slot) in slots.iter().rev().take(capacity).enumerate() {
            let x1 = right - age as f64 * pitch;
            let x0 = x1 - width;
            if x0 < left_limit {
                break;
            }
            let Some(candle) = slot else { continue };
            let (body_top, body_bottom) = rows(candle.open, candle.close);
            let (wick_top, wick_bottom) = rows(candle.high, candle.low);
            let wick_x = x0 + wick_offset;
            out.push(Shape {
                body: Rect::new(x0, body_top, x1, body_bottom),
                wick: Rect::new(
                    wick_x,
                    wick_top.min(body_top),
                    wick_x + wick_width,
                    wick_bottom.max(body_bottom),
                ),
                up: candle.is_up(),
            });
        }
    });
    out.reverse();
    out
}

fn extent_of(candles: impl Iterator<Item = Candle>) -> Option<(f32, f32)> {
    candles.fold(None, |acc, c| match acc {
        None => Some((c.low, c.high)),
        Some((lo, hi)) => Some((lo.min(c.low), hi.max(c.high))),
    })
}

impl Widget for CandlestickWidget {
    fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let max = bc.max();
        let width = if max.width.is_finite() {
            max.width
        } else {
            UNBOUNDED_WIDTH
        };
        let height = match self.spec.height.map(f64::from) {
            Some(height) if height.is_finite() => height.max(0.0),
            _ if max.height.is_finite() => max.height,
            _ => DEFAULT_HEIGHT,
        };
        bc.constrain(Size::new(width, height))
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let (origin, size) = (ctx.origin(), ctx.size());
        if !(size.width > 0.0 && size.height > 0.0) {
            return;
        }
        let plot = Rect::new(0.0, 0.0, size.width, size.height);
        let (up, down, grid) = match Theme::from_paint_ctx(ctx) {
            Some(theme) => {
                let scheme = theme.scheme();
                (scheme.tertiary, scheme.error, scheme.outline_variant)
            }
            None => (Tone::FALLBACK_CALM, Tone::FALLBACK_CRITICAL, FALLBACK_GRID),
        };
        let (up, down) = (self.spec.up.unwrap_or(up), self.spec.down.unwrap_or(down));
        let grid = self.spec.grid_color.unwrap_or(grid);
        let key = CacheKey {
            revision: self.spec.candles.revision(),
            plot,
        };
        if self.cache.as_ref().map(|(k, _)| k) != Some(&key) {
            self.cache = Some((key, shapes(&self.spec, plot)));
        }
        let Some((_, shapes)) = &self.cache else {
            return;
        };
        let at = |x: f64, y: f64| Point::new(origin.x + x, origin.y + y);
        scene.push_clip(origin, size);
        for y in grid_rule_ys(&self.spec.grid, plot) {
            scene.fill_rect(at(plot.x0, y), Size::new(plot.width(), 1.0), grid);
        }
        for shape in shapes {
            let color = if shape.up { up } else { down };
            for r in [shape.wick, shape.body] {
                scene.fill_rect(at(r.x0, r.y0), Size::new(r.width(), r.height()), color);
            }
        }
        scene.pop_clip();
    }

    fn event(&mut self, _ctx: &mut EventCtx, _event: &InputEvent) -> EventResult {
        EventResult::Ignored
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        if let Some(label) = &self.spec.semantics {
            ctx.push_node(Role::Image, |node| node.set_label(label.as_str()));
        }
    }
}
