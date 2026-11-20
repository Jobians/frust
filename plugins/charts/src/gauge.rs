//! The `gauge` widget: a radial arc gauge with coloured zones, a value arc, and an optional
//! needle and ticks.
//!
//! A gauge is the round sibling of [`meter`](crate::meter): it takes the same [`MeterValue`]
//! (a fixed `f32`, read through a rebuild, or a live [`SampleSlot`](crate::SampleSlot), read
//! at every paint so a write repaints it with no rebuild), clamps it into its range, and draws
//! a gap as the track alone (zones and ticks stay; no value arc and no needle).
//!
//! Geometry: the gauge lays out square at [`size`](GaugeView::size) (default the smaller of
//! the available width and height) and draws inside the largest circle centred in whatever
//! box it is given, so a non-square constraint never pushes a stroke out of bounds. Angles are
//! radians, 0 at 3 o'clock and growing clockwise, as in [`pie_chart`](crate::pie_chart) and
//! the polar helpers; the default is a three-quarter turn open at the bottom. The track and
//! the value arc are stroked arcs with the chosen [`GaugeCap`]. Zones are butt-ended arcs
//! between their values; with round caps, a zone that reaches an end of the range gets an
//! exact half-disc there, so it covers the track's cap without overlapping itself. Ticks are
//! marks inside the track's inner edge and draw no text. An optional centre child is laid out
//! loose in the square inscribed in the track's inner edge and centred.
//!
//! Colours: the caller's, then the installed theme's (value `primary`, track
//! `surface_container_highest`, needle `on_surface`), then [`Tone::FALLBACK_CALM`],
//! [`MeterView::FALLBACK_TRACK`] and [`GaugeView::FALLBACK_NEEDLE`]. The track, zones and
//! ticks are built once per size and options; only the value arc and the needle follow the
//! value. A gauge never animates and requests no frames; without a centre child it takes no
//! input, and with one it routes events to that child.

use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI, SQRT_2, TAU};

use frust::authoring::scene::{StrokeCap, StrokeJoin, StrokeStyle};
use frust::authoring::{
    AnyView, BezPath, BoxConstraints, Brush, BuildCtx, ChangeFlags, ChildPod, EventCtx,
    EventResult, InputEvent, LayoutCtx, Node, PaintCtx, PaintScene, Point, Role, SemanticsCtx,
    Size, View, Widget, any, build_child, rebuild_child, route_event_single, teardown_child,
};
use frust::{Color, Theme};

use crate::{MeterValue, MeterView, Tone, YDomain, arc_path, point_on_circle, sector_path};

const DEFAULT_RANGE: (f32, f32) = (0.0, 100.0);
/// 7:30 on a clock face, the lower left.
const DEFAULT_START: f64 = 3.0 * FRAC_PI_4;
/// Three quarters of a turn, clockwise.
const DEFAULT_SWEEP: f64 = 3.0 * FRAC_PI_2;
const DEFAULT_THICKNESS: f32 = 10.0;
const DEFAULT_SIZE: f64 = 120.0;
const DEFAULT_NEEDLE_WIDTH: f32 = 3.0;
const DEFAULT_HUB: f32 = 5.0;
const TICK_WIDTH: f64 = 1.0;
/// The space between the track's inner edge and the outer end of each tick.
const TICK_GAP: f64 = 2.0;

/// How the ends of a gauge's track and value arc are drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum GaugeCap {
    /// A half-disc of the arc's thickness past each end.
    #[default]
    Round,
    /// Flush with each end.
    Butt,
}

/// The needle a gauge can draw from its centre to the value; build one with
/// [`NeedleStyle::new`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NeedleStyle {
    color: Option<Color>,
    width: f32,
    length: Option<f32>,
    hub: f32,
}

impl Default for NeedleStyle {
    fn default() -> Self {
        Self::new()
    }
}

impl NeedleStyle {
    /// A 3 px needle reaching the track's inner edge, on a hub of radius 5, in the theme's
    /// `on_surface` colour.
    pub fn new() -> Self {
        Self {
            color: None,
            width: DEFAULT_NEEDLE_WIDTH,
            length: None,
            hub: DEFAULT_HUB,
        }
    }

    /// The needle and hub colour (default: the theme's `on_surface`).
    pub fn color(mut self, color: Color) -> Self {
        self.color = Some(color);
        self
    }

    /// The needle's width in logical px (default 3).
    pub fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    /// How far the needle reaches, as a fraction of the gauge's radius, clamped to `0..=1`
    /// (default: to the track's inner edge). Its round tip stays inside that reach.
    pub fn length(mut self, fraction: f32) -> Self {
        self.length = Some(fraction);
        self
    }

    /// The radius of the disc at the needle's base in logical px (default 5; 0 for none).
    pub fn hub(mut self, radius: f32) -> Self {
        self.hub = radius;
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Ticks {
    count: usize,
    length: f32,
    color: Color,
}

#[derive(Clone, Debug)]
struct GaugeSpec {
    value: MeterValue,
    range: (f32, f32),
    /// Radians.
    start: f64,
    /// Radians, within one turn either way.
    sweep: f64,
    thickness: f32,
    cap: GaugeCap,
    track: Option<Color>,
    color: Option<Color>,
    tone: Option<Tone>,
    zones: Vec<(f32, f32, Color)>,
    needle: Option<NeedleStyle>,
    ticks: Option<Ticks>,
    size: Option<f32>,
    label: Option<String>,
}

/// The same fixed value, or the same slot of the same set.
fn same_value(a: &MeterValue, b: &MeterValue) -> bool {
    match (a, b) {
        (MeterValue::Fixed(a), MeterValue::Fixed(b)) => a.map(f32::to_bits) == b.map(f32::to_bits),
        (MeterValue::Live(a), MeterValue::Live(b)) => {
            a.samples().ptr_eq(b.samples()) && a.index() == b.index()
        }
        _ => false,
    }
}

impl GaugeSpec {
    fn new(value: MeterValue) -> Self {
        Self {
            value,
            range: DEFAULT_RANGE,
            start: DEFAULT_START,
            sweep: DEFAULT_SWEEP,
            thickness: DEFAULT_THICKNESS,
            cap: GaugeCap::Round,
            track: None,
            color: None,
            tone: None,
            zones: Vec::new(),
            needle: None,
            ticks: None,
            size: None,
            label: None,
        }
    }

    /// What a widget holding `old` must redo to show `self`.
    fn changes(&self, old: &Self) -> ChangeFlags {
        if self.size != old.size || self.thickness != old.thickness {
            return ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        if !same_value(&self.value, &old.value)
            || self.range != old.range
            || self.start != old.start
            || self.sweep != old.sweep
            || self.cap != old.cap
            || self.track != old.track
            || self.color != old.color
            || self.tone != old.tone
            || self.zones != old.zones
            || self.needle != old.needle
            || self.ticks != old.ticks
            || self.label != old.label
        {
            return ChangeFlags::PAINT;
        }
        ChangeFlags::NONE
    }

    /// Whether the value-independent geometry built for `old` (track, zones, ticks, and the
    /// arc a value maps to) no longer fits `self`.
    fn geometry_changed(&self, old: &Self) -> bool {
        self.range != old.range
            || self.start != old.start
            || self.sweep != old.sweep
            || self.thickness != old.thickness
            || self.cap != old.cap
            || self.zones != old.zones
            || self.ticks != old.ticks
    }

    /// The range made finite and non-degenerate, as [`YDomain::resolve`] does.
    fn domain(&self) -> (f32, f32) {
        YDomain::fixed(self.range.0, self.range.1).resolve(None)
    }

    /// `(start, sweep)` in radians.
    fn radians(&self) -> (f64, f64) {
        (self.start, self.sweep)
    }

    /// The cap actually drawn: a closed ring has no ends to cap.
    fn cap(&self) -> GaugeCap {
        if self.radians().1.abs() >= TAU {
            GaugeCap::Butt
        } else {
            self.cap
        }
    }

    /// The value assistive technology reads, clamped into the range.
    fn reported_value(&self, domain: (f32, f32)) -> Option<f32> {
        self.value.get().map(|v| v.clamp(domain.0, domain.1))
    }
}

/// Where `value` sits in `domain`, clamped to `0..=1`.
fn fraction(value: f32, domain: (f32, f32)) -> f64 {
    let (min, max) = (f64::from(domain.0), f64::from(domain.1));
    ((f64::from(value) - min) / (max - min)).clamp(0.0, 1.0)
}

fn stroke_style(cap: GaugeCap) -> StrokeStyle {
    let cap = match cap {
        GaugeCap::Round => StrokeCap::Round,
        GaugeCap::Butt => StrokeCap::Butt,
    };
    StrokeStyle::new(cap, StrokeJoin::Round)
}

/// The circle a gauge draws in for a box of `size`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Dial {
    center: Point,
    /// The outer radius: half the box's shorter side.
    outer: f64,
    /// The track's thickness, at most `outer`.
    thickness: f64,
}

impl Dial {
    fn new(size: Size, thickness: f32) -> Self {
        let outer = (size.width.min(size.height) / 2.0).max(0.0);
        let thickness = f64::from(thickness).max(0.0).min(outer);
        Self {
            center: Point::new(size.width / 2.0, size.height / 2.0),
            outer,
            thickness,
        }
    }

    /// The radius the track and value arcs are stroked along.
    fn mid(&self) -> f64 {
        self.outer - self.thickness / 2.0
    }

    /// The radius of the track's inner edge.
    fn inner(&self) -> f64 {
        self.outer - self.thickness
    }
}

/// The half-disc capping an arc end at `angle` on the circle of `radius`, bulging towards
/// `outward` (an angle seen from that end).
fn end_cap(dial: &Dial, radius: f64, angle: f64, outward: f64) -> BezPath {
    let at = point_on_circle(dial.center, radius, angle);
    sector_path(at, 0.0, dial.thickness / 2.0, outward - FRAC_PI_2, PI, 0.0)
}

/// One zone's geometry: its arc, and the half-discs where it reaches an end of a round-capped
/// range.
struct ZoneShape {
    arc: BezPath,
    caps: Vec<BezPath>,
    color: Color,
}

/// The geometry that depends on the size and the options but not on the value.
struct Frame {
    size: Size,
    dial: Dial,
    track: BezPath,
    zones: Vec<ZoneShape>,
    ticks: BezPath,
}

impl Frame {
    fn build(spec: &GaugeSpec, size: Size) -> Self {
        let dial = Dial::new(size, spec.thickness);
        let (start, sweep) = spec.radians();
        let mid = dial.mid();
        let track = if dial.thickness > 0.0 {
            arc_path(dial.center, mid, start, sweep)
        } else {
            BezPath::new()
        };
        let domain = spec.domain();
        let round = spec.cap() == GaugeCap::Round;
        let direction = sweep.signum();
        let mut zones = Vec::new();
        for &(from, to, color) in &spec.zones {
            if dial.thickness <= 0.0 {
                break;
            }
            let (lo, hi) = if from <= to { (from, to) } else { (to, from) };
            let (lo, hi) = (lo.max(domain.0), hi.min(domain.1));
            if lo >= hi {
                continue;
            }
            let a0 = start + sweep * fraction(lo, domain);
            let a1 = start + sweep * fraction(hi, domain);
            let mut caps = Vec::new();
            if round && lo <= domain.0 {
                caps.push(end_cap(&dial, mid, a0, a0 - direction * FRAC_PI_2));
            }
            if round && hi >= domain.1 {
                caps.push(end_cap(&dial, mid, a1, a1 + direction * FRAC_PI_2));
            }
            zones.push(ZoneShape {
                arc: arc_path(dial.center, mid, a0, a1 - a0),
                caps,
                color,
            });
        }
        let ticks = spec
            .ticks
            .map_or_else(BezPath::new, |ticks| tick_path(&dial, start, sweep, ticks));
        Self {
            size,
            dial,
            track,
            zones,
            ticks,
        }
    }
}

/// Every tick as one subpath each, from just inside the track's inner edge towards the centre.
fn tick_path(dial: &Dial, start: f64, sweep: f64, ticks: Ticks) -> BezPath {
    let mut path = BezPath::new();
    let outer = dial.inner() - TICK_GAP;
    let length = f64::from(ticks.length);
    if ticks.count == 0 || !(length.is_finite() && length > 0.0) || outer <= 0.0 {
        return path;
    }
    let inner = (outer - length).max(0.0);
    // A closed ring's last tick would land on its first.
    let steps = if sweep.abs() >= TAU {
        ticks.count
    } else {
        ticks.count - 1
    };
    for i in 0..ticks.count {
        let t = if steps == 0 {
            0.0
        } else {
            i as f64 / steps as f64
        };
        let angle = start + sweep * t;
        path.move_to(point_on_circle(dial.center, outer, angle));
        path.line_to(point_on_circle(dial.center, inner, angle));
    }
    path
}

/// The `(value, track, needle)` colours when the caller gives none: the theme's, else the
/// fallbacks.
fn default_colors(ctx: &PaintCtx) -> (Color, Color, Color) {
    match Theme::from_paint_ctx(ctx) {
        Some(theme) => {
            let scheme = theme.scheme();
            (
                scheme.primary,
                scheme.surface_container_highest,
                scheme.on_surface,
            )
        }
        None => (
            Tone::FALLBACK_CALM,
            MeterView::FALLBACK_TRACK,
            GaugeView::<()>::FALLBACK_NEEDLE,
        ),
    }
}

/// A radial arc gauge built by [`gauge`].
pub struct GaugeView<State: 'static = ()> {
    spec: GaugeSpec,
    center: Option<AnyView<State>>,
}

/// A gauge showing `value` (fixed `f32`, `Option<f32>`, or a live
/// [`SampleSlot`](crate::SampleSlot)) on a 0..100 range, as a 10 px thick arc of three quarters
/// of a turn open at the bottom.
pub fn gauge<State: 'static>(value: impl Into<MeterValue>) -> GaugeView<State> {
    GaugeView {
        spec: GaugeSpec::new(value.into()),
        center: None,
    }
}

impl<State: 'static> GaugeView<State> {
    /// The needle colour when neither the caller nor the theme gives one.
    pub const FALLBACK_NEEDLE: Color = Color::from_rgb8(0x3c, 0x40, 0x46);

    /// The value range the arc spans (default `0..100`; a reversed or empty one is fixed up
    /// as [`YDomain::resolve`] does).
    pub fn range(mut self, min: f32, max: f32) -> Self {
        self.spec.range = (min, max);
        self
    }

    /// Where the arc starts and how far it sweeps, in radians: 0 at 3 o'clock, positive
    /// clockwise, a negative sweep running counter-clockwise (default `3 * PI / 4, 3 * PI / 2`,
    /// a three-quarter turn open at the bottom). A sweep beyond a full turn is clamped to one;
    /// a non-finite angle keeps the default.
    ///
    /// ```
    /// use std::f32::consts::PI;
    /// use frust_charts::gauge;
    ///
    /// // A half-circle speedometer over the top: from 9 o'clock clockwise to 3 o'clock.
    /// let speed = gauge::<()>(42.0).angles(PI, PI);
    /// # let _ = speed;
    /// ```
    pub fn angles(mut self, start: f32, sweep: f32) -> Self {
        self.spec.start = if start.is_finite() {
            f64::from(start)
        } else {
            DEFAULT_START
        };
        self.spec.sweep = if sweep.is_finite() {
            f64::from(sweep).clamp(-TAU, TAU)
        } else {
            DEFAULT_SWEEP
        };
        self
    }

    /// The track's thickness in logical px (default 10; at most the gauge's radius).
    pub fn thickness(mut self, thickness: f32) -> Self {
        self.spec.thickness = thickness;
        self
    }

    /// How the ends of the track and the value arc are drawn (default [`GaugeCap::Round`]).
    pub fn cap(mut self, cap: GaugeCap) -> Self {
        self.spec.cap = cap;
        self
    }

    /// The track colour (default: the theme's `surface_container_highest`).
    pub fn track(mut self, color: Color) -> Self {
        self.spec.track = Some(color);
        self
    }

    /// The value arc's colour (default: the theme's primary).
    pub fn color(mut self, color: Color) -> Self {
        self.spec.color = Some(color);
        self
    }

    /// Colour the value arc by `tone` at the current value; wins over
    /// [`color`](Self::color).
    pub fn tone(mut self, tone: Tone) -> Self {
        self.spec.tone = Some(tone);
        self
    }

    /// Bands along the track, each `(from, to, color)` in range units, drawn over the track
    /// and under the value arc in the order given. A band is clamped into the range; one
    /// outside it, empty, or with a non-finite end is dropped.
    pub fn zones(mut self, zones: impl IntoIterator<Item = (f32, f32, Color)>) -> Self {
        self.spec.zones = zones
            .into_iter()
            .filter(|(from, to, _)| from.is_finite() && to.is_finite())
            .collect();
        self
    }

    /// Draw a needle from the centre to the value; a gap draws none.
    pub fn needle(mut self, style: NeedleStyle) -> Self {
        self.spec.needle = Some(style);
        self
    }

    /// Draw `count` tick marks of `length` px, evenly spaced from the start of the arc to its
    /// end (around a full turn, without doubling the first), just inside the track.
    pub fn ticks(mut self, count: usize, length: f32, color: Color) -> Self {
        self.spec.ticks = Some(Ticks {
            count,
            length,
            color,
        });
        self
    }

    /// Show `view` in the middle, laid out loose in the square inscribed in the track's
    /// inner edge and centred there.
    pub fn center<V: View<State>>(mut self, view: V) -> Self {
        self.center = Some(any(view));
        self
    }

    /// The gauge's side in logical px, clamped into the constraints (default: the smaller of
    /// the available width and height, or 120 when neither is bounded).
    pub fn size(mut self, size: f32) -> Self {
        self.spec.size = Some(size);
        self
    }

    /// Give the gauge a progress-indicator node with this label, reporting the value and the
    /// range; without one it has no node of its own.
    pub fn semantics(mut self, label: impl Into<String>) -> Self {
        self.spec.label = Some(label.into());
        self
    }
}

impl<State: 'static> View<State> for GaugeView<State> {
    type Element = GaugeWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> GaugeWidget {
        GaugeWidget {
            spec: self.spec.clone(),
            child: self.center.as_ref().map(|view| build_child(view, ctx)),
            frame: None,
            value_arc: None,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut GaugeWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = self.spec.changes(&element.spec);
        if !flags.is_empty() {
            if self.spec.geometry_changed(&element.spec) {
                element.frame = None;
                element.value_arc = None;
            }
            element.spec = self.spec.clone();
        }
        match (&prev.center, &self.center, &mut element.child) {
            (Some(old), Some(new), Some(pod)) => flags |= rebuild_child(old, new, pod, ctx),
            (_, Some(new), slot @ None) => {
                *slot = Some(build_child(new, ctx));
                flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
            }
            (Some(old), None, slot) => {
                if let Some(pod) = slot {
                    teardown_child(old, pod, ctx);
                }
                *slot = None;
                flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
            }
            _ => {}
        }
        flags
    }

    fn teardown(&self, element: &mut GaugeWidget, ctx: &mut BuildCtx<'_>) {
        if let (Some(view), Some(pod)) = (&self.center, &mut element.child) {
            teardown_child(view, pod, ctx);
        }
    }
}

/// The retained widget behind a [`GaugeView`].
pub struct GaugeWidget {
    spec: GaugeSpec,
    child: Option<ChildPod>,
    frame: Option<Frame>,
    /// The value arc and the value bits and size it was built for.
    value_arc: Option<(u32, Size, BezPath)>,
}

impl GaugeWidget {
    /// The value-independent layer: the track, then the zones, then the ticks.
    fn paint_frame(&self, origin: Point, default_track: Color, scene: &mut dyn PaintScene) {
        let Some(frame) = &self.frame else { return };
        let thickness = frame.dial.thickness;
        let butt = stroke_style(GaugeCap::Butt);
        if !frame.track.elements().is_empty() {
            let track = Brush::Solid(self.spec.track.unwrap_or(default_track));
            let style = stroke_style(self.spec.cap());
            scene.stroke_path_styled(origin, &frame.track, thickness, None, style, &track);
        }
        for zone in &frame.zones {
            let brush = Brush::Solid(zone.color);
            scene.stroke_path_styled(origin, &zone.arc, thickness, None, butt, &brush);
            for cap in &zone.caps {
                scene.fill_path(origin, cap, &brush);
            }
        }
        if let Some(ticks) = self.spec.ticks
            && !frame.ticks.elements().is_empty()
        {
            let brush = Brush::Solid(ticks.color);
            scene.stroke_path_styled(origin, &frame.ticks, TICK_WIDTH, None, butt, &brush);
        }
    }

    fn paint_value(&mut self, value: f32, origin: Point, color: Color, scene: &mut dyn PaintScene) {
        let Some(frame) = &self.frame else { return };
        let (dial, size) = (frame.dial, frame.size);
        let bits = value.to_bits();
        let fresh = (self.value_arc.as_ref()).is_some_and(|(b, at, _)| *b == bits && *at == size);
        if !fresh {
            let (start, sweep) = self.spec.radians();
            let swept = sweep * fraction(value, self.spec.domain());
            let path = if swept == 0.0 || dial.thickness <= 0.0 {
                BezPath::new()
            } else {
                arc_path(dial.center, dial.mid(), start, swept)
            };
            self.value_arc = Some((bits, size, path));
        }
        let Some((_, _, path)) = &self.value_arc else {
            return;
        };
        if path.elements().is_empty() {
            return;
        }
        scene.stroke_path_styled(
            origin,
            path,
            dial.thickness,
            None,
            stroke_style(self.spec.cap()),
            &Brush::Solid(color),
        );
    }

    fn paint_needle(
        &self,
        value: f32,
        style: NeedleStyle,
        origin: Point,
        default: Color,
        scene: &mut dyn PaintScene,
    ) {
        let Some(frame) = &self.frame else { return };
        let dial = frame.dial;
        let brush = Brush::Solid(style.color.unwrap_or(default));
        let reach = match style.length {
            Some(fraction) if fraction.is_finite() => {
                dial.outer * f64::from(fraction).clamp(0.0, 1.0)
            }
            _ => dial.inner(),
        };
        let width = f64::from(style.width).max(0.0);
        let (start, sweep) = self.spec.radians();
        let angle = start + sweep * fraction(value, self.spec.domain());
        let tip = reach - width / 2.0;
        if width > 0.0 && tip > 0.0 {
            let mut path = BezPath::new();
            path.move_to(dial.center);
            path.line_to(point_on_circle(dial.center, tip, angle));
            scene.stroke_path_styled(
                origin,
                &path,
                width,
                None,
                stroke_style(GaugeCap::Round),
                &brush,
            );
        }
        let hub = f64::from(style.hub);
        if hub.is_finite() && hub > 0.0 {
            let hub = hub.min(dial.outer);
            let disc = sector_path(dial.center, 0.0, hub, 0.0, TAU, 0.0);
            scene.fill_path(origin, &disc, &brush);
        }
    }
}

impl Widget for GaugeWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let max = bc.max();
        let available = match (max.width.is_finite(), max.height.is_finite()) {
            (true, true) => max.width.min(max.height),
            (true, false) => max.width,
            (false, true) => max.height,
            (false, false) => DEFAULT_SIZE,
        };
        let side = match self.spec.size.map(f64::from) {
            Some(side) if side.is_finite() => side.max(0.0),
            _ => available,
        };
        let size = bc.constrain(Size::new(side, side));
        if let Some(pod) = &mut self.child {
            let dial = Dial::new(size, self.spec.thickness);
            let room = (dial.inner() * SQRT_2).max(0.0);
            let child = pod.layout_child(ctx, &BoxConstraints::loose(Size::new(room, room)));
            pod.set_origin(Point::new(
                dial.center.x - child.width / 2.0,
                dial.center.y - child.height / 2.0,
            ));
        }
        size
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let (origin, size) = (ctx.origin(), ctx.size());
        let drawn = size.width > 0.0 && size.height > 0.0;
        let value = self.spec.value.get().filter(|_| drawn);
        let (default_value, default_track, default_needle) = default_colors(ctx);
        if drawn {
            if self.frame.as_ref().is_none_or(|frame| frame.size != size) {
                self.frame = Some(Frame::build(&self.spec, size));
            }
            self.paint_frame(origin, default_track, scene);
        }
        if let Some(value) = value {
            let spec = &self.spec;
            let color = spec
                .tone
                .as_ref()
                .map(|tone| tone.color_at(value))
                .or(spec.color)
                .unwrap_or(default_value);
            self.paint_value(value, origin, color, scene);
        }
        if let Some(pod) = &mut self.child {
            pod.paint_child(ctx, scene);
        }
        if let (Some(value), Some(style)) = (value, self.spec.needle) {
            self.paint_needle(value, style, origin, default_needle, scene);
        }
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        match &mut self.child {
            Some(pod) => route_event_single(pod, ctx, event),
            None => EventResult::Ignored,
        }
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        let Some(label) = &self.spec.label else {
            if let Some(pod) = &self.child {
                pod.semantics_child(ctx);
            }
            return;
        };
        let domain = self.spec.domain();
        let value = self.spec.reported_value(domain);
        let build = |node: &mut Node| {
            if let Some(value) = value {
                node.set_numeric_value(f64::from(value));
            }
            node.set_min_numeric_value(f64::from(domain.0));
            node.set_max_numeric_value(f64::from(domain.1));
            node.set_label(label.as_str());
        };
        match &self.child {
            Some(pod) => {
                ctx.push_container(Role::ProgressIndicator, build, |ctx| {
                    pod.semantics_child(ctx)
                });
            }
            None => {
                ctx.push_node(Role::ProgressIndicator, build);
            }
        }
    }

    frust::authoring::visit_children!(child);
}

#[cfg(test)]
mod tests {
    use super::{Dial, fraction};
    use frust::authoring::Size;

    #[test]
    fn fraction_clamps_into_the_range() {
        let domain = (0.0, 200.0);
        assert_eq!(fraction(50.0, domain), 0.25);
        assert_eq!(fraction(-10.0, domain), 0.0);
        assert_eq!(fraction(300.0, domain), 1.0);
    }

    #[test]
    fn the_dial_fits_the_shorter_side_and_caps_the_thickness() {
        let dial = Dial::new(Size::new(200.0, 100.0), 12.0);
        assert_eq!(
            (dial.center.x, dial.center.y, dial.outer),
            (100.0, 50.0, 50.0)
        );
        assert_eq!((dial.mid(), dial.inner()), (44.0, 38.0));
        assert_eq!(Dial::new(Size::new(20.0, 20.0), 40.0).thickness, 10.0);
    }
}
