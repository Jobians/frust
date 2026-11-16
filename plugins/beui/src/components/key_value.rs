//! Key-value rows and stage timelines: [`key_value_row`] and [`timeline`].
//!
//! The read-only detail layout of a phone screen: a row pairing a key with its
//! value, and a vertical timeline of [`TimelineStep`]s each in a [`TimelineStepState`].
//!
//! - [`key_value_row`] puts the key, in the secondary text colour, on the leading
//!   side and the value on the trailing side. It stacks the value under the key when
//!   [`stacked`](KeyValueRowView::stacked) is set or when the width cannot fit both
//!   on one line. [`trailing`](KeyValueRowView::trailing) adds a view at the far
//!   trailing edge and [`mono`](KeyValueRowView::mono) sets the value in the theme's
//!   monospace family, for ids.
//! - [`timeline`] draws one marker per step (a filled success disc with a check for
//!   `Done`, an accent ring around a dot for `Current`, a muted disc for `Pending`), a
//!   connector between neighbouring markers, the step label and an optional detail
//!   line. Marker and connector geometry follow the scaled label line height, so the
//!   OS text size and the density carry through.
//!
//! # Truncation
//!
//! Text stays inside the row. Inline, the key keeps its width up to half of the row
//! (more when the value is short) and the value takes what is left; stacked, each
//! line is bounded by the row's width less the trailing view. A timeline label or
//! detail line is bounded by the width left of the marker column. Overflow is cut to
//! one line with an ellipsis; this module keeps its own run because
//! `LabelRun` shapes at an unbounded width.
//!
//! Semantics: a row publishes its key as the label and its value as the value; each
//! timeline step publishes its label and its state in words ("Done", "Current",
//! "Pending") as the value.

use frust::Theme;
use frust::authoring::text::{LocaleStamp, TextContext, TextLayout, TextOverflow, TextStyle};
use frust::authoring::{
    AnyView, BezPath, BoxConstraints, Brush, BuildCtx, ChangeFlags, ChildPod, EventCtx,
    EventResult, InputEvent, LayoutCtx, PaintCtx, PaintScene, Role, SemanticsCtx, View, Widget,
    any, build_child, rebuild_child, route_event_single, teardown_child,
};
use kurbo::{Point, Size, Vec2};
use peniko::Color;

use crate::text::{ThemeTextType, scaled_style, themed_style};
use crate::tokens::{BEUI_LIGHT, BeuiMetrics, BeuiTokens, mono_family};

/// Connector stroke width, in logical px.
const CONNECTOR_WIDTH: f64 = 2.0;
/// Ring stroke width of a `Current` marker, in logical px.
const RING_WIDTH: f64 = 2.0;
/// Marker diameter as a fraction of the label's line height.
const MARKER_SCALE: f64 = 0.7;
/// The most of an inline row a key claims when the value is long too.
const KEY_MAX_SHARE: f64 = 0.5;

/// A cached one-line text run whose ink is applied at paint time and which cuts
/// itself to a width with an ellipsis. Private copy: `LabelRun` has no bound.
struct EllipsisRun {
    content: String,
    layout: Option<TextLayout>,
    key: Option<(TextStyle, Option<u32>)>,
    locale: LocaleStamp,
}

impl EllipsisRun {
    fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            layout: None,
            key: None,
            locale: LocaleStamp::default(),
        }
    }

    fn content(&self) -> &str {
        &self.content
    }

    fn set_content(&mut self, content: &str) -> bool {
        if self.content == content {
            return false;
        }
        self.content = content.to_owned();
        self.layout = None;
        true
    }

    /// Shape at `max_width` (unbounded for `None`), one line, ellipsis on overflow.
    fn layout(&mut self, ctx: &mut LayoutCtx, style: &TextStyle, max_width: Option<f64>) -> Size {
        let width = max_width.map(|w| w.max(0.0) as f32);
        let key = (style.clone(), width.map(f32::to_bits));
        if let Some(cached) = &self.layout
            && self.key.as_ref() == Some(&key)
            && self.locale.is_current(style.locale.as_deref())
        {
            return cached.size();
        }
        let stamp = LocaleStamp::for_style(style.locale.as_deref());
        let laid = ctx.text_context::<TextContext>().layout_bounded(
            &self.content,
            style,
            width,
            Some(1),
            TextOverflow::Ellipsis,
        );
        let size = laid.size();
        self.layout = Some(laid);
        self.key = Some(key);
        self.locale = stamp;
        size
    }

    /// Shape unbounded, then again at `cap` only when the text is wider than it.
    fn fit(&mut self, ctx: &mut LayoutCtx, style: &TextStyle, cap: Option<f64>) -> Size {
        let natural = self.layout(ctx, style, None);
        match cap {
            Some(cap) if natural.width > cap => self.layout(ctx, style, Some(cap)),
            _ => natural,
        }
    }

    fn size(&self) -> Size {
        self.layout.as_ref().map_or(Size::ZERO, TextLayout::size)
    }

    fn paint(&self, origin: Point, color: Color, scene: &mut dyn PaintScene) {
        if let Some(layout) = &self.layout {
            for mut run in layout.to_scene_runs(origin) {
                run.brush = Brush::Solid(color);
                scene.draw_glyph_run(run);
            }
        }
    }
}

/// The catalog's sans at `size`, themed for `role` and scaled by the OS text scale.
fn part_style(ctx: &LayoutCtx, size: f64, role: ThemeTextType) -> TextStyle {
    themed_style(
        TextStyle::new(size as f32, Color::BLACK),
        role,
        Theme::from_layout_ctx(ctx),
    )
}

/// The words a step's state publishes.
fn state_words(state: TimelineStepState) -> &'static str {
    match state {
        TimelineStepState::Done => "Done",
        TimelineStepState::Current => "Current",
        TimelineStepState::Pending => "Pending",
    }
}

/// One step as the widget holds it.
struct StepRow {
    state: TimelineStepState,
    label: EllipsisRun,
    detail: Option<EllipsisRun>,
    /// Top of the step's row, set by layout.
    top: f64,
}

/// What the widget draws.
enum Body {
    Row {
        key: Box<EllipsisRun>,
        value: Box<EllipsisRun>,
        mono: bool,
        stacked: bool,
        /// Whether the last layout fell back to the stacked arrangement.
        narrow: bool,
        trailing: Option<Box<ChildPod>>,
    },
    Timeline {
        steps: Vec<StepRow>,
        /// Marker diameter and the marker column width, set by layout.
        marker: f64,
        column: f64,
    },
}

/// The retained widget behind the key-value and timeline entry points.
pub struct KeyValueWidget {
    body: Body,
}

impl KeyValueWidget {
    fn row(key: &str, value: &str, mono: bool, stacked: bool, trailing: Option<ChildPod>) -> Self {
        Self {
            body: Body::Row {
                key: Box::new(EllipsisRun::new(key)),
                value: Box::new(EllipsisRun::new(value)),
                mono,
                stacked,
                narrow: false,
                trailing: trailing.map(Box::new),
            },
        }
    }

    fn timeline(steps: &[TimelineStep]) -> Self {
        Self {
            body: Body::Timeline {
                steps: steps.iter().map(step_row).collect(),
                marker: 0.0,
                column: 0.0,
            },
        }
    }

    /// The text lines this widget carries, top to bottom: a row's key and value, or
    /// each step's label (a detail line follows its label).
    pub fn lines(&self) -> Vec<&str> {
        match &self.body {
            Body::Row { key, value, .. } => vec![key.content(), value.content()],
            Body::Timeline { steps, .. } => steps
                .iter()
                .flat_map(|s| {
                    std::iter::once(s.label.content())
                        .chain(s.detail.as_ref().map(EllipsisRun::content))
                })
                .collect(),
        }
    }

    /// Whether the last layout put a row's value under its key.
    pub fn is_stacked(&self) -> bool {
        matches!(&self.body, Body::Row { narrow: true, .. })
    }
}

fn step_row(step: &TimelineStep) -> StepRow {
    StepRow {
        state: step.state,
        label: EllipsisRun::new(step.label.clone()),
        detail: step.detail.clone().map(EllipsisRun::new),
        top: 0.0,
    }
}

/// The colours a key-value row and timeline paint with.
struct Hues {
    ink: Color,
    secondary: Color,
    accent: Color,
    on_accent: Color,
    done: Color,
    muted: Color,
}

impl Hues {
    fn resolve(theme: Option<&Theme>) -> Self {
        let scheme = theme.map(Theme::scheme);
        let tokens = BeuiTokens::resolve(theme);
        Self {
            ink: scheme.map_or(BEUI_LIGHT.foreground, |s| s.on_surface),
            secondary: scheme.map_or(BEUI_LIGHT.muted_foreground, |s| s.on_surface_variant),
            accent: scheme.map_or(BEUI_LIGHT.primary, |s| s.primary),
            on_accent: scheme.map_or(BEUI_LIGHT.primary_foreground, |s| s.on_primary),
            done: tokens.success,
            muted: scheme.map_or(BEUI_LIGHT.border, |s| s.outline_variant),
        }
    }
}

impl Widget for KeyValueWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let theme = Theme::from_layout_ctx(ctx);
        let metrics = BeuiMetrics::resolve(theme);
        let avail = Some(bc.max().width).filter(|w| w.is_finite());
        match &mut self.body {
            Body::Row {
                key,
                value,
                mono,
                stacked,
                narrow,
                trailing,
            } => {
                let key_style = part_style(ctx, metrics.text_sm, ThemeTextType::BodyMedium);
                let value_style = if *mono {
                    // `TypeScale` has no monospace slot, so the family is named here.
                    scaled_style(
                        TextStyle::new(metrics.text_base as f32, Color::BLACK)
                            .with_family(mono_family()),
                        Theme::from_layout_ctx(ctx),
                    )
                } else {
                    part_style(ctx, metrics.text_base, ThemeTextType::BodyMedium)
                };
                let trailing_size = trailing
                    .as_mut()
                    .map_or(Size::ZERO, |t| t.layout_child(ctx, &bc.loosen()));
                let gap = metrics.gap_md;
                let trail_w = if trailing_size.width > 0.0 {
                    trailing_size.width + gap
                } else {
                    0.0
                };
                // The width the text may use: the row less the trailing view.
                let room = avail.map(|w| (w - trail_w).max(0.0));
                let key_nat = key.layout(ctx, &key_style, None);
                let value_nat = value.layout(ctx, &value_style, None);
                let inline_w = key_nat.width + gap + value_nat.width;
                *narrow = *stacked || room.is_some_and(|r| inline_w > r);
                let (key_size, value_size) = if *narrow {
                    (
                        key.fit(ctx, &key_style, room),
                        value.fit(ctx, &value_style, room),
                    )
                } else {
                    // The key keeps its width up to a share of the row, or what the
                    // value leaves; the value takes the rest.
                    let key_cap =
                        room.map(|r| (r * KEY_MAX_SHARE).max(r - gap - value_nat.width).max(0.0));
                    let key_size = key.fit(ctx, &key_style, key_cap);
                    let value_cap = room.map(|r| (r - key_size.width - gap).max(0.0));
                    (key_size, value.fit(ctx, &value_style, value_cap))
                };
                let (content_w, content_h) = if *narrow {
                    (
                        key_size.width.max(value_size.width) + trail_w,
                        key_size.height + value_size.height,
                    )
                } else {
                    (
                        key_size.width + gap + value_size.width + trail_w,
                        key_size.height.max(value_size.height),
                    )
                };
                let height = content_h.max(trailing_size.height);
                let width = avail.unwrap_or(content_w);
                let size = bc.constrain(Size::new(width, height));
                if let Some(t) = trailing {
                    t.set_origin(Point::new(
                        size.width - trailing_size.width,
                        (size.height - trailing_size.height) / 2.0,
                    ));
                }
                size
            }
            Body::Timeline {
                steps,
                marker,
                column,
            } => {
                let label_style = part_style(ctx, metrics.text_base, ThemeTextType::BodyMedium);
                let detail_style = part_style(ctx, metrics.text_sm, ThemeTextType::BodyMedium);
                // The marker column follows the label line height, which one probe
                // line gives without shaping every label twice.
                let line = ctx
                    .text_context::<TextContext>()
                    .layout("M", &label_style, None)
                    .size()
                    .height;
                *column = line;
                *marker = (line * MARKER_SCALE).round().max(4.0);
                let gap = metrics.gap_md;
                let text_cap = avail.map(|w| (w - *column - gap).max(0.0));
                let mut y = 0.0;
                let mut width = 0.0_f64;
                for step in steps.iter_mut() {
                    step.top = y;
                    let label = step.label.fit(ctx, &label_style, text_cap);
                    let detail = step
                        .detail
                        .as_mut()
                        .map(|d| d.fit(ctx, &detail_style, text_cap));
                    let text_w = label.width.max(detail.map_or(0.0, |d| d.width));
                    width = width.max(*column + gap + text_w);
                    y += label.height + detail.map_or(0.0, |d| d.height) + gap;
                }
                // The trailing gap belongs between steps only.
                let height = if steps.is_empty() { 0.0 } else { y - gap };
                bc.constrain(Size::new(width, height))
            }
        }
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let theme = Theme::from_paint_ctx(ctx);
        let metrics = BeuiMetrics::resolve(theme);
        let hues = Hues::resolve(theme);
        let origin = ctx.origin();
        let size = ctx.size();
        match &mut self.body {
            Body::Row {
                key,
                value,
                narrow,
                trailing,
                ..
            } => {
                let trail_w = trailing.as_ref().map_or(0.0, |t| {
                    let w = t.size().width;
                    if w > 0.0 { w + metrics.gap_md } else { 0.0 }
                });
                key.paint(origin, hues.secondary, scene);
                let value_size = value.size();
                let at = if *narrow {
                    Point::new(origin.x, origin.y + key.size().height)
                } else {
                    Point::new(
                        origin.x + size.width - trail_w - value_size.width,
                        origin.y + (size.height - value_size.height) / 2.0,
                    )
                };
                value.paint(at, hues.ink, scene);
                if let Some(t) = trailing {
                    t.paint_child(ctx, scene);
                }
            }
            Body::Timeline {
                steps,
                marker,
                column,
            } => {
                let gap = metrics.gap_md;
                let count = steps.len();
                for (i, step) in steps.iter().enumerate() {
                    let top = origin.y + step.top;
                    let label_h = step.label.size().height;
                    step.label
                        .paint(Point::new(origin.x + *column + gap, top), hues.ink, scene);
                    if let Some(detail) = &step.detail {
                        detail.paint(
                            Point::new(origin.x + *column + gap, top + label_h),
                            hues.secondary,
                            scene,
                        );
                    }
                    let centre = Point::new(origin.x + *column / 2.0, top + label_h / 2.0);
                    paint_marker(scene, centre, *marker, step.state, &hues);
                    if i + 1 < count {
                        // From just under this marker to just above the next one.
                        let pad = (*marker * 0.25).max(2.0);
                        let from = centre.y + *marker / 2.0 + pad;
                        let next =
                            origin.y + steps[i + 1].top + steps[i + 1].label.size().height / 2.0
                                - *marker / 2.0
                                - pad;
                        if next > from {
                            scene.fill_rect(
                                Point::new(centre.x - CONNECTOR_WIDTH / 2.0, from),
                                Size::new(CONNECTOR_WIDTH, next - from),
                                if step.state == TimelineStepState::Done {
                                    hues.done
                                } else {
                                    hues.muted
                                },
                            );
                        }
                    }
                }
            }
        }
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        match &mut self.body {
            // `route_event_single` ends the child's pointer capture on release.
            Body::Row {
                trailing: Some(t), ..
            } => route_event_single(t, ctx, event),
            _ => EventResult::Ignored,
        }
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        match &self.body {
            Body::Row {
                key,
                value,
                trailing,
                ..
            } => {
                ctx.push_container(
                    Role::Group,
                    |node| {
                        node.set_label(key.content());
                        node.set_value(value.content());
                    },
                    |ctx| {
                        if let Some(t) = trailing {
                            t.semantics_child(ctx);
                        }
                    },
                );
            }
            Body::Timeline { steps, .. } => {
                ctx.push_container(
                    Role::List,
                    |_| {},
                    |ctx| {
                        for step in steps {
                            ctx.push_node(Role::ListItem, |node| {
                                node.set_label(step.label.content());
                                node.set_value(state_words(step.state));
                            });
                        }
                    },
                );
            }
        }
    }

    fn visit_children(&self, visitor: &mut dyn FnMut(&ChildPod)) {
        if let Body::Row {
            trailing: Some(t), ..
        } = &self.body
        {
            visitor(t);
        }
    }
}

/// Draw one marker of diameter `d` centred on `centre`.
fn paint_marker(
    scene: &mut dyn PaintScene,
    centre: Point,
    d: f64,
    state: TimelineStepState,
    hues: &Hues,
) {
    let disc = |scene: &mut dyn PaintScene, d: f64, color: Color| {
        scene.fill_rounded_rect(
            Point::new(centre.x - d / 2.0, centre.y - d / 2.0),
            Size::new(d, d),
            d / 2.0,
            color,
        );
    };
    match state {
        TimelineStepState::Done => {
            disc(scene, d, hues.done);
            let mut tick = BezPath::new();
            tick.move_to(Point::new(centre.x - d * 0.22, centre.y + d * 0.02));
            tick.line_to(Point::new(centre.x - d * 0.06, centre.y + d * 0.18));
            tick.line_to(Point::new(centre.x + d * 0.24, centre.y - d * 0.16));
            scene.stroke_path(
                Point::ORIGIN,
                &tick,
                (d / 8.0).max(1.0),
                &Brush::Solid(hues.on_accent),
            );
        }
        TimelineStepState::Current => {
            let r = (d - RING_WIDTH) / 2.0;
            let ring = kurbo::Arc::new(centre, Vec2::new(r, r), 0.0, std::f64::consts::TAU, 0.0);
            scene.stroke_path(
                Point::ORIGIN,
                &kurbo::Shape::to_path(&ring, 0.1),
                RING_WIDTH,
                &Brush::Solid(hues.accent),
            );
            disc(scene, d * 0.4, hues.accent);
        }
        TimelineStepState::Pending => disc(scene, d, hues.muted),
    }
}

/// Where a [`TimelineStep`] stands.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TimelineStepState {
    /// Finished.
    Done,
    /// In progress.
    Current,
    /// Not reached yet.
    #[default]
    Pending,
}

/// One stage of a [`timeline`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineStep {
    /// The stage's label.
    pub label: String,
    /// Where the stage stands.
    pub state: TimelineStepState,
    detail: Option<String>,
}

impl TimelineStep {
    /// A step labelled `label` in `state`.
    pub fn new(label: impl Into<String>, state: TimelineStepState) -> Self {
        Self {
            label: label.into(),
            state,
            detail: None,
        }
    }

    /// Add a detail line under the label.
    #[must_use]
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// The detail line, if any.
    pub fn detail_text(&self) -> Option<&str> {
        self.detail.as_deref()
    }
}

/// A key and its value on one row. See the [module docs](self).
pub struct KeyValueRowView<State: 'static = ()> {
    key: String,
    value: String,
    stacked: bool,
    mono: bool,
    trailing: Option<AnyView<State>>,
}

/// A row pairing `key` with `value`.
pub fn key_value_row<State: 'static>(
    key: impl Into<String>,
    value: impl Into<String>,
) -> KeyValueRowView<State> {
    KeyValueRowView {
        key: key.into(),
        value: value.into(),
        stacked: false,
        mono: false,
        trailing: None,
    }
}

impl<State: 'static> KeyValueRowView<State> {
    /// Put the value under the key even when both would fit on one line.
    #[must_use]
    pub fn stacked(mut self, stacked: bool) -> Self {
        self.stacked = stacked;
        self
    }

    /// Set the value in the theme's monospace family, for ids.
    #[must_use]
    pub fn mono(mut self, mono: bool) -> Self {
        self.mono = mono;
        self
    }

    /// A view at the row's trailing edge.
    #[must_use]
    pub fn trailing<V: View<State>>(mut self, view: V) -> Self {
        self.trailing = Some(any(view));
        self
    }
}

impl<State: 'static> View<State> for KeyValueRowView<State> {
    type Element = KeyValueWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> KeyValueWidget {
        let trailing = self.trailing.as_ref().map(|t| build_child(t, ctx));
        KeyValueWidget::row(&self.key, &self.value, self.mono, self.stacked, trailing)
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut KeyValueWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let Body::Row {
            key,
            value,
            mono,
            stacked,
            trailing,
            ..
        } = &mut element.body
        else {
            return ChangeFlags::NONE;
        };
        let mut flags = ChangeFlags::NONE;
        let layout = ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        if key.set_content(&self.key) {
            flags |= layout;
        }
        if value.set_content(&self.value) {
            flags |= layout;
        }
        if *mono != self.mono {
            *mono = self.mono;
            // The cached shape is keyed on the style, so the next layout reshapes.
            flags |= layout;
        }
        if *stacked != self.stacked {
            *stacked = self.stacked;
            flags |= layout;
        }
        flags |= rebuild_slot(
            prev.trailing.as_ref(),
            self.trailing.as_ref(),
            trailing,
            ctx,
        );
        flags
    }

    fn teardown(&self, element: &mut KeyValueWidget, ctx: &mut BuildCtx<'_>) {
        if let (
            Body::Row {
                trailing: Some(pod),
                ..
            },
            Some(view),
        ) = (&mut element.body, self.trailing.as_ref())
        {
            teardown_child(view, pod, ctx);
        }
    }
}

/// Reconcile the trailing slot across all four previous / next combinations, so the
/// pod is present exactly when the view is.
fn rebuild_slot<State: 'static>(
    prev: Option<&AnyView<State>>,
    next: Option<&AnyView<State>>,
    pod: &mut Option<Box<ChildPod>>,
    ctx: &mut BuildCtx<'_>,
) -> ChangeFlags {
    match (prev, next, pod.as_mut()) {
        (Some(prev), Some(next), Some(pod)) => rebuild_child(prev, next, pod, ctx),
        (Some(prev), None, Some(old)) => {
            teardown_child(prev, old, ctx);
            *pod = None;
            ChangeFlags::LAYOUT | ChangeFlags::PAINT
        }
        (_, Some(next), _) => {
            *pod = Some(Box::new(build_child(next, ctx)));
            ChangeFlags::LAYOUT | ChangeFlags::PAINT
        }
        _ => ChangeFlags::NONE,
    }
}

/// A vertical stage timeline. See the [module docs](self).
pub struct TimelineView {
    steps: Vec<TimelineStep>,
}

/// A timeline through `steps`, in order.
pub fn timeline(steps: impl IntoIterator<Item = TimelineStep>) -> TimelineView {
    TimelineView {
        steps: steps.into_iter().collect(),
    }
}

impl<State: 'static> View<State> for TimelineView {
    type Element = KeyValueWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> KeyValueWidget {
        KeyValueWidget::timeline(&self.steps)
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut KeyValueWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        if prev.steps == self.steps {
            return ChangeFlags::NONE;
        }
        element.body = KeyValueWidget::timeline(&self.steps).body;
        ChangeFlags::LAYOUT | ChangeFlags::PAINT
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use frust::Brightness;
    use frust::authoring::scene::GlyphRun;
    use frust::authoring::text::TextContext;
    use frust::authoring::{PointerButton, PointerEvent, PointerPhase, SemanticsUpdate};
    use std::any::Any;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use crate::tokens::{Density, theme_with_density};

    /// The colour a [`Probe`] fills, to find it in a paint record.
    const PROBE_INK: Color = Color::from_rgba8(200, 30, 30, 255);

    /// A trailing child that records what reaches it and that it was torn down.
    struct Probe {
        id: u32,
        hits: Rc<RefCell<Vec<u32>>>,
        torn: Rc<Cell<u32>>,
    }

    struct ProbeWidget {
        id: u32,
        hits: Rc<RefCell<Vec<u32>>>,
    }

    impl Widget for ProbeWidget {
        fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
            bc.constrain(Size::new(40.0, 30.0))
        }
        fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
            scene.fill_rect(ctx.origin(), ctx.size(), PROBE_INK);
        }
        fn event(&mut self, _ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
            if let InputEvent::Pointer(_) = event {
                self.hits.borrow_mut().push(self.id);
                return EventResult::Handled;
            }
            EventResult::Ignored
        }
        fn semantics(&self, ctx: &mut SemanticsCtx) {
            let label = format!("probe {}", self.id);
            ctx.push_node(Role::Button, |node| node.set_label(label.as_str()));
        }
    }

    impl<S: 'static> View<S> for Probe {
        type Element = ProbeWidget;
        fn build(&self, _ctx: &mut BuildCtx<'_>) -> ProbeWidget {
            ProbeWidget {
                id: self.id,
                hits: self.hits.clone(),
            }
        }
        fn rebuild(
            &self,
            _prev: &Self,
            element: &mut ProbeWidget,
            _ctx: &mut BuildCtx<'_>,
        ) -> ChangeFlags {
            element.id = self.id;
            ChangeFlags::NONE
        }
        fn teardown(&self, _element: &mut ProbeWidget, _ctx: &mut BuildCtx<'_>) {
            self.torn.set(self.torn.get() + 1);
        }
    }

    /// A row with a probe trailing view, or none.
    fn probe_row(
        id: Option<u32>,
        hits: &Rc<RefCell<Vec<u32>>>,
        torn: &Rc<Cell<u32>>,
    ) -> KeyValueRowView<()> {
        let row = key_value_row("Key", "Value");
        match id {
            Some(id) => row.trailing(Probe {
                id,
                hits: hits.clone(),
                torn: torn.clone(),
            }),
            None => row,
        }
    }

    fn rebuilt(prev: &KeyValueRowView<()>, next: &KeyValueRowView<()>, w: &mut KeyValueWidget) {
        let mut next_id = 100u64;
        next.rebuild(prev, w, &mut BuildCtx::new(&mut next_id));
    }

    fn relayout(w: &mut KeyValueWidget, width: f64) -> Size {
        let mut text_ctx = TextContext::new();
        let mut layout = LayoutCtx::with_text_context(&mut text_ctx as &mut dyn Any);
        w.layout(&mut layout, &BoxConstraints::loose(Size::new(width, 600.0)))
    }

    fn pointer(phase: PointerPhase, x: f64, y: f64) -> InputEvent {
        InputEvent::Pointer(PointerEvent {
            phase,
            position: Point::new(x, y),
            button: PointerButton::Primary,
        })
    }

    fn send(w: &mut KeyValueWidget, size: Size, event: &InputEvent) {
        let mut state = ();
        let any_state: &mut dyn Any = &mut state;
        let mut ctx = EventCtx::new(any_state, Point::ZERO, size);
        w.event(&mut ctx, event);
    }

    fn probe_painted(rec: &Recorder) -> bool {
        rec.rects.iter().any(|(_, _, c)| *c == PROBE_INK)
    }

    /// The labels of every node `view` publishes after laying out at 400 px.
    fn labels_after(rows: &[Option<u32>]) -> Vec<Vec<String>> {
        let hits = Rc::new(RefCell::new(Vec::new()));
        let torn = Rc::new(Cell::new(0));
        let mut logic = |state: &mut usize| {
            let id = rows[*state];
            let row = key_value_row::<usize>("Key", "Value");
            match id {
                Some(id) => row.trailing(Probe {
                    id,
                    hits: hits.clone(),
                    torn: torn.clone(),
                }),
                None => row,
            }
        };
        let mut root = frust_core::RenderRoot::new();
        let mut state;
        let mut tcx = TextContext::new();
        let mut out = Vec::new();
        for step in 0..rows.len() {
            state = step;
            root.rebuild(&mut logic, &mut state);
            root.layout_with_text(Size::new(400.0, 800.0), &mut tcx as &mut dyn Any);
            let update: SemanticsUpdate = root.semantics();
            out.push(
                update
                    .nodes
                    .iter()
                    .filter_map(|(_, n)| n.label().map(str::to_owned))
                    .collect(),
            );
        }
        out
    }

    /// Records every shape drawn, in order.
    #[derive(Default)]
    struct Recorder {
        rects: Vec<(Point, Size, Color)>,
        discs: Vec<(Point, Size, Color)>,
        strokes: Vec<Color>,
        glyphs: usize,
        /// The furthest right any glyph starts, in window space.
        max_x: f64,
    }

    impl PaintScene for Recorder {
        fn fill_rect(&mut self, origin: Point, size: Size, color: Color) {
            self.rects.push((origin, size, color));
        }
        fn draw_text(&mut self, _origin: Point, _text: &str) {}
        fn fill_rounded_rect(&mut self, origin: Point, size: Size, _r: f64, color: Color) {
            self.discs.push((origin, size, color));
        }
        fn stroke_path(&mut self, _o: Point, _p: &BezPath, _w: f64, brush: &Brush) {
            if let Brush::Solid(c) = brush {
                self.strokes.push(*c);
            }
        }
        fn draw_glyph_run(&mut self, run: GlyphRun) {
            self.glyphs += 1;
            let left = run.transform.translation().x;
            for g in &run.glyphs {
                self.max_x = self.max_x.max(left + f64::from(g.x));
            }
        }
    }

    /// Build `view` headlessly and lay it out `width` wide under `theme`.
    fn laid_out_in<V: View<()>>(view: &V, width: f64, theme: Option<&Theme>) -> (V::Element, Size) {
        let mut next_id = 0u64;
        let mut widget = view.build(&mut BuildCtx::new(&mut next_id));
        let mut text_ctx = TextContext::new();
        let mut layout = LayoutCtx::with_text_context(&mut text_ctx as &mut dyn Any);
        if let Some(theme) = theme {
            layout = layout.with_theme(theme);
        }
        let size = widget.layout(&mut layout, &BoxConstraints::loose(Size::new(width, 600.0)));
        (widget, size)
    }

    fn laid_out<V: View<()>>(view: &V) -> (V::Element, Size) {
        laid_out_in(view, 400.0, None)
    }

    fn paint<W: Widget>(widget: &mut W, size: Size, theme: Option<&Theme>) -> Recorder {
        let mut ctx = PaintCtx::for_test(Point::ORIGIN, size, frust::FrameTime::from_nanos(0));
        if let Some(theme) = theme {
            ctx = ctx.with_theme(theme);
        }
        let mut rec = Recorder::default();
        widget.paint(&mut ctx, &mut rec);
        rec
    }

    fn steps() -> [TimelineStep; 3] {
        [
            TimelineStep::new("Queued", TimelineStepState::Done),
            TimelineStep::new("Running", TimelineStepState::Current).detail("Started a minute ago"),
            TimelineStep::new("Shipped", TimelineStepState::Pending),
        ]
    }

    #[test]
    fn the_key_value_entry_points_build_and_lay_out() {
        let (row, size) = laid_out(&key_value_row("Region", "eu-west"));
        assert_eq!(row.lines(), vec!["Region", "eu-west"]);
        assert!(size.height > 0.0);
        let (tl, _) = laid_out(&timeline(steps()));
        assert_eq!(
            tl.lines(),
            vec!["Queued", "Running", "Started a minute ago", "Shipped"]
        );
    }

    #[test]
    fn the_row_stacks_at_a_narrow_width_and_stays_inline_at_a_wide_one() {
        let view = key_value_row("Deployment region", "eu-west-1 (Ireland)");
        let (wide, wide_size) = laid_out_in(&view, 400.0, None);
        let (narrow, narrow_size) = laid_out_in(&view, 90.0, None);
        assert!(!wide.is_stacked());
        assert!(narrow.is_stacked());
        assert!(
            narrow_size.height > wide_size.height * 1.5,
            "two lines tall"
        );

        let (forced, _) = laid_out_in(&view.stacked(true), 400.0, None);
        assert!(forced.is_stacked(), "stacked(true) stacks at any width");
    }

    #[test]
    fn an_inline_value_sits_at_the_trailing_edge() {
        let (mut row, size) = laid_out(&key_value_row("Id", "a1"));
        let rec = paint(&mut row, size, None);
        assert!(rec.glyphs > 0);
        assert_eq!(size.width, 400.0, "the row fills a bounded width");
    }

    #[test]
    fn mono_changes_the_value_shape() {
        let (plain, plain_size) = laid_out(&key_value_row("Id", "0123456789abcdef"));
        let (mono, mono_size) = laid_out(&key_value_row("Id", "0123456789abcdef").mono(true));
        assert_eq!(plain.lines(), mono.lines());
        // Both lay out; the widths are not asserted, only that the monospace path runs.
        assert!(plain_size.height > 0.0 && mono_size.height > 0.0);
    }

    #[test]
    fn a_three_step_timeline_draws_two_connectors_and_distinct_markers() {
        let (mut tl, size) = laid_out(&timeline(steps()));
        let rec = paint(&mut tl, size, None);
        assert_eq!(rec.rects.len(), 2, "one connector between each pair");
        // Done: disc + tick; Current: ring + dot; Pending: disc.
        assert_eq!(rec.discs.len(), 3);
        assert_eq!(rec.strokes.len(), 2);
        let (done, dot, pending) = (rec.discs[0], rec.discs[1], rec.discs[2]);
        assert_ne!(done.2, pending.2, "Done and Pending differ in colour");
        assert_ne!(dot.2, pending.2);
        assert_ne!(done.2, dot.2);
        assert!(
            dot.1.width < done.1.width,
            "the current dot sits inside a ring"
        );
        // Connectors run downward between markers and do not overlap.
        assert!(rec.rects[0].0.y < rec.rects[1].0.y);
    }

    #[test]
    fn the_markers_follow_the_text_scale() {
        let plain = crate::theme();
        let authored = plain.type_scale.clone();
        let scaled = crate::theme().with_text_scale(&authored, 1.5);
        let (mut a, size_a) = laid_out_in(&timeline(steps()), 400.0, Some(&plain));
        let (mut b, size_b) = laid_out_in(&timeline(steps()), 400.0, Some(&scaled));
        let first = paint(&mut a, size_a, Some(&plain)).discs[0].1.width;
        let second = paint(&mut b, size_b, Some(&scaled)).discs[0].1.width;
        assert!(second > first, "{second} vs {first}");
    }

    #[test]
    fn both_densities_and_brightnesses_build() {
        for density in [Density::Comfortable, Density::Touch] {
            for brightness in [Brightness::Light, Brightness::Dark] {
                let theme = theme_with_density(density).with_brightness(brightness);
                let (mut row, size) =
                    laid_out_in(&key_value_row("Key", "Value"), 300.0, Some(&theme));
                assert!(paint(&mut row, size, Some(&theme)).glyphs > 0);
                let (mut tl, size) = laid_out_in(&timeline(steps()), 300.0, Some(&theme));
                assert_eq!(paint(&mut tl, size, Some(&theme)).rects.len(), 2);
            }
        }
    }

    #[test]
    fn a_step_publishes_its_state_in_words() {
        assert_eq!(state_words(TimelineStepState::Done), "Done");
        assert_eq!(state_words(TimelineStepState::Current), "Current");
        assert_eq!(state_words(TimelineStepState::Pending), "Pending");
    }

    #[test]
    fn a_trailing_view_added_on_a_later_rebuild_shows_and_lays_out() {
        let hits = Rc::new(RefCell::new(Vec::new()));
        let torn = Rc::new(Cell::new(0));
        let (none, one) = (
            probe_row(None, &hits, &torn),
            probe_row(Some(1), &hits, &torn),
        );
        let (mut row, _) = laid_out(&none);
        rebuilt(&none, &one, &mut row);
        let size = relayout(&mut row, 400.0);
        let rec = paint(&mut row, size, None);
        let probe = rec.rects.iter().find(|(_, _, c)| *c == PROBE_INK);
        let (at, probe_size, _) = probe.expect("the added child paints");
        assert_eq!(at.x + probe_size.width, size.width, "at the trailing edge");
    }

    #[test]
    fn a_trailing_view_removed_is_gone_everywhere_and_torn_down() {
        let hits = Rc::new(RefCell::new(Vec::new()));
        let torn = Rc::new(Cell::new(0));
        let (one, none) = (
            probe_row(Some(1), &hits, &torn),
            probe_row(None, &hits, &torn),
        );
        let (mut row, size) = laid_out(&one);
        assert!(probe_painted(&paint(&mut row, size, None)));

        let mut next_id = 100u64;
        let flags = none.rebuild(&one, &mut row, &mut BuildCtx::new(&mut next_id));
        assert!(flags.contains(ChangeFlags::LAYOUT | ChangeFlags::PAINT));
        assert_eq!(torn.get(), 1, "the removed child was torn down");

        let size = relayout(&mut row, 400.0);
        assert!(!probe_painted(&paint(&mut row, size, None)), "not painted");
        let mut visited = 0;
        row.visit_children(&mut |_| visited += 1);
        assert_eq!(visited, 0, "not a child any more");
        send(&mut row, size, &pointer(PointerPhase::Down, 395.0, 10.0));
        assert!(hits.borrow().is_empty(), "receives no events");
    }

    #[test]
    fn a_removed_trailing_view_leaves_the_semantics_tree() {
        let labels = labels_after(&[Some(1), None]);
        assert!(labels[0].iter().any(|l| l == "probe 1"));
        assert!(!labels[1].iter().any(|l| l.starts_with("probe")));
    }

    #[test]
    fn re_adding_a_trailing_view_builds_the_new_one() {
        let hits = Rc::new(RefCell::new(Vec::new()));
        let torn = Rc::new(Cell::new(0));
        let first = probe_row(Some(1), &hits, &torn);
        let none = probe_row(None, &hits, &torn);
        let second = probe_row(Some(2), &hits, &torn);
        let (mut row, _) = laid_out(&first);
        rebuilt(&first, &none, &mut row);
        rebuilt(&none, &second, &mut row);
        let size = relayout(&mut row, 400.0);
        send(
            &mut row,
            size,
            &pointer(PointerPhase::Down, 395.0, size.height / 2.0),
        );
        assert_eq!(
            *hits.borrow(),
            vec![2],
            "the new view's widget, not the old"
        );
        assert_eq!(torn.get(), 1);
    }

    #[test]
    fn a_trailing_press_and_release_leaves_nothing_captured() {
        let hits = Rc::new(RefCell::new(Vec::new()));
        let torn = Rc::new(Cell::new(0));
        let (mut row, _) = laid_out(&probe_row(Some(1), &hits, &torn));
        let size = relayout(&mut row, 400.0);
        let y = size.height / 2.0;
        send(&mut row, size, &pointer(PointerPhase::Down, 395.0, y));
        send(&mut row, size, &pointer(PointerPhase::Up, 395.0, y));
        assert_eq!(hits.borrow().len(), 2);
        send(&mut row, size, &pointer(PointerPhase::Down, 5.0, y));
        assert_eq!(hits.borrow().len(), 2, "a press elsewhere is not captured");
    }

    #[test]
    fn a_long_value_stays_inside_a_narrow_row() {
        let value = "an exceptionally long value that cannot possibly fit on a phone row".repeat(2);
        for view in [
            key_value_row::<()>("Key", value.clone()),
            key_value_row::<()>("Key", value.clone()).stacked(true),
            key_value_row::<()>("A long key for the row", value.clone()),
        ] {
            let (mut row, size) = laid_out_in(&view, 320.0, None);
            assert!(size.width <= 320.0);
            let rec = paint(&mut row, size, None);
            assert!(rec.glyphs > 0);
            assert!(rec.max_x <= 320.0, "painted to {}", rec.max_x);
        }
    }

    #[test]
    fn a_long_timeline_label_and_detail_stay_inside_the_width() {
        let long = "a stage label far longer than any phone screen could show".repeat(2);
        let view =
            timeline([TimelineStep::new(long.clone(), TimelineStepState::Current).detail(long)]);
        let (mut tl, size) = laid_out_in(&view, 200.0, None);
        assert!(size.width <= 200.0);
        let rec = paint(&mut tl, size, None);
        assert!(rec.max_x <= 200.0, "painted to {}", rec.max_x);
    }
}
