//! Card surfaces: a plain [`card`], a [`hero_card`] with a title and an action row, and a
//! [`stat_tile`].
//!
//! Three themed surfaces for grouping content on a phone. All of them paint the same
//! chrome: a surface fill (`surface_container_low`, beUI's `--card`), a hairline
//! `outline_variant` border, and the metrics' [`radius_card`](BeuiMetrics::radius_card).
//! Padding defaults to [`CARD_PADDING`]; both it and the radius take a builder override.
//!
//! - [`card`] wraps one child and shrinks to it. `.on_press(..)` makes the whole card a
//!   button: a pressed wash, a focus ring, keyboard activation and a `Role::Button` node
//!   named by `.label(..)`. A pressable card never lays out below
//!   [`BeuiMetrics::min_touch_target`], so its hit area clears the touch minimum under a
//!   touch density. `.elevated(true)` adds a soft shadow and the next surface rung.
//! - [`hero_card`] stacks a title, an optional supporting line, the child and an
//!   `.actions(..)` row. The row is a [`frust::wrap`], so when the width cannot hold every
//!   action the rest flow onto further lines.
//! - [`stat_tile`] is a compact label-over-value card with an optional `.trailing(..)` view
//!   at the label's end. It fills a bounded width, so a 2-column grid cell sizes it.
//!
//! A hero card or stat tile fills a bounded maximum width; a plain card always shrinks to its
//! content. All text goes through the catalog's themed text helpers, so it follows the theme
//! family and the OS text scale. Nothing here animates: the pressed wash is a plain paint
//! state, so reduce-motion has nothing to honour.

use std::rc::Rc;

use frust::Theme;
use frust::authoring::text::{FontWeight, TextStyle};
use frust::authoring::{
    AnyView, BoxConstraints, BuildCtx, ChangeFlags, ChildPod, ErasedCallback, EventCtx,
    EventResult, InputEvent, LayoutCtx, PaintCtx, PaintScene, Point, PointerPhase, Role,
    SemanticsCtx, Size, View, Widget, any, build_child, erase_callback, rebuild_child,
    teardown_child,
};
use frust::{ViewSeq, wrap};
use kurbo::Vec2;
use peniko::Color;

use crate::press::{
    draw_focus_ring, inside, is_activation_key, lerp_color, presses, stroke_outline,
};
use crate::style::{ACTIVE_CURSOR, SPACING_UNIT, resolve_radius, with_alpha};
use crate::text::{LabelRun, ThemeTextType, label_style};
use crate::tokens::{BeuiMetrics, BeuiTokens, color_scheme_light};

/// The default padding around a card's content: `p-4`.
pub const CARD_PADDING: f64 = 16.0;

/// Gap between a card's stacked blocks.
const BLOCK_GAP: f64 = 3.0 * SPACING_UNIT;
/// Gap between a title and the line under it.
const LINE_GAP: f64 = SPACING_UNIT;
/// Gap between the actions in a hero card's row, and between wrapped rows.
const ACTION_GAP: f64 = 2.0 * SPACING_UNIT;
/// Gap between a stat tile's label and its trailing view.
const TRAILING_GAP: f64 = 2.0 * SPACING_UNIT;
/// Share of the ink mixed into a pressed card's fill.
const PRESSED_WASH: f64 = 0.08;
/// Share of the ink mixed into a hovered pressable card's fill.
const HOVER_WASH: f64 = 0.04;
/// Offset between the focus ring and a pressable card's edge.
const RING_OFFSET: f64 = 2.0;
/// Blur deviation of an elevated card's shadow.
const SHADOW_STD_DEV: f64 = 10.0;
/// Alpha of an elevated card's shadow.
const SHADOW_ALPHA: f32 = 0.14;
/// Vertical drop of an elevated card's shadow.
const SHADOW_DROP: f64 = 2.0;

/// Which entry point a [`CardWidget`] serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Plain,
    Hero,
    Stat,
}

/// One text line's look: size, weight, type role and whether it takes the muted ink.
#[derive(Clone, Copy)]
struct LineLook {
    size: f64,
    weight: FontWeight,
    role: ThemeTextType,
    muted: bool,
}

/// A view-held press handler, shared with the retained widget on build.
type PressHandler<State> = Rc<dyn Fn(&mut State)>;

/// Options every card entry point shares; the views carry one and the builders edit it.
struct Chrome<State: 'static> {
    padding: Option<f64>,
    radius: Option<f64>,
    elevated: bool,
    label: Option<String>,
    on_press: Option<PressHandler<State>>,
}

impl<State: 'static> Chrome<State> {
    fn new() -> Self {
        Self {
            padding: None,
            radius: None,
            elevated: false,
            label: None,
            on_press: None,
        }
    }

    /// Copy the options onto `element`, answering what changed.
    fn apply(&self, element: &mut CardWidget) -> ChangeFlags {
        let mut flags = ChangeFlags::NONE;
        if element.padding != self.padding || element.radius != self.radius {
            element.padding = self.padding;
            element.radius = self.radius;
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        if element.elevated != self.elevated || element.label != self.label {
            element.elevated = self.elevated;
            element.label.clone_from(&self.label);
            flags |= ChangeFlags::PAINT;
        }
        if element.on_press.is_some() != self.on_press.is_some() {
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        element.on_press = self.on_press.as_ref().map(erase_callback);
        flags
    }
}

/// The builder methods every card view shares.
macro_rules! chrome_builders {
    ($view:ident) => {
        impl<State: 'static> $view<State> {
            /// Override the padding around the content (default [`CARD_PADDING`]).
            #[must_use]
            pub fn padding(mut self, padding: f64) -> Self {
                self.chrome.padding = Some(padding.max(0.0));
                self
            }

            /// Override the corner radius (default the metrics' `radius_card`).
            #[must_use]
            pub fn radius(mut self, radius: f64) -> Self {
                self.chrome.radius = Some(radius.max(0.0));
                self
            }

            /// Lift the card: a soft shadow and the next surface rung.
            #[must_use]
            pub fn elevated(mut self, elevated: bool) -> Self {
                self.chrome.elevated = elevated;
                self
            }

            /// The accessible name a pressable card publishes (default: its text lines).
            #[must_use]
            pub fn label(mut self, label: impl Into<String>) -> Self {
                self.chrome.label = Some(label.into());
                self
            }

            /// Make the whole card a button that runs `on_press` when released inside it.
            #[must_use]
            pub fn on_press(mut self, on_press: impl Fn(&mut State) + 'static) -> Self {
                self.chrome.on_press = Some(Rc::new(on_press));
                self
            }
        }
    };
}

/// Reconcile an optional child slot across a rebuild.
fn rebuild_slot<State: 'static>(
    prev: &Option<AnyView<State>>,
    next: &Option<AnyView<State>>,
    pod: &mut Option<ChildPod>,
    ctx: &mut BuildCtx<'_>,
) -> ChangeFlags {
    match (prev, next) {
        (Some(p), Some(n)) => match pod {
            Some(pod) => rebuild_child(p, n, pod, ctx),
            None => {
                *pod = Some(build_child(n, ctx));
                ChangeFlags::LAYOUT | ChangeFlags::PAINT
            }
        },
        (Some(p), None) => {
            if let Some(mut old) = pod.take() {
                teardown_child(p, &mut old, ctx);
            }
            ChangeFlags::LAYOUT | ChangeFlags::PAINT
        }
        (None, Some(n)) => {
            *pod = Some(build_child(n, ctx));
            ChangeFlags::LAYOUT | ChangeFlags::PAINT
        }
        (None, None) => ChangeFlags::NONE,
    }
}

/// The retained widget behind the card entry points: stacked text lines, an optional child
/// and an optional second slot (a hero card's action row, a stat tile's trailing view).
pub struct CardWidget {
    kind: Kind,
    lines: Vec<LabelRun>,
    line_pos: Vec<Point>,
    child: Option<ChildPod>,
    extra: Option<ChildPod>,
    padding: Option<f64>,
    radius: Option<f64>,
    elevated: bool,
    label: Option<String>,
    on_press: Option<ErasedCallback>,
    pressed: bool,
    captured: bool,
}

impl CardWidget {
    fn new(kind: Kind, lines: Vec<String>) -> Self {
        Self {
            kind,
            lines: lines.into_iter().map(LabelRun::new).collect(),
            line_pos: Vec::new(),
            child: None,
            extra: None,
            padding: None,
            radius: None,
            elevated: false,
            label: None,
            on_press: None,
            pressed: false,
            captured: false,
        }
    }

    /// The text lines this widget carries, top to bottom.
    pub fn lines(&self) -> Vec<&str> {
        self.lines.iter().map(LabelRun::content).collect()
    }

    fn set_lines(&mut self, lines: Vec<String>) -> ChangeFlags {
        let mut flags = ChangeFlags::NONE;
        if self.lines.len() != lines.len() {
            self.lines = lines.into_iter().map(LabelRun::new).collect();
            return ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        for (run, text) in self.lines.iter_mut().zip(lines) {
            if run.set_content(text) {
                flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
            }
        }
        flags
    }

    fn look(&self, index: usize, m: &BeuiMetrics) -> LineLook {
        match (self.kind, index) {
            (Kind::Hero, 0) => LineLook {
                size: m.text_base * 1.25,
                weight: FontWeight::SEMI_BOLD,
                role: ThemeTextType::TitleMedium,
                muted: false,
            },
            (Kind::Hero, _) => LineLook {
                size: m.text_sm,
                weight: FontWeight::REGULAR,
                role: ThemeTextType::BodyMedium,
                muted: true,
            },
            (_, 0) => LineLook {
                size: m.text_sm,
                weight: FontWeight::MEDIUM,
                role: ThemeTextType::LabelMedium,
                muted: true,
            },
            (_, _) => LineLook {
                size: m.text_base * 1.75,
                weight: FontWeight::SEMI_BOLD,
                role: ThemeTextType::HeadlineSmall,
                muted: false,
            },
        }
    }

    fn pressable(&self) -> bool {
        self.on_press.is_some()
    }

    /// The accessible name: the supplied label, else the text lines joined.
    fn name(&self) -> String {
        self.label
            .clone()
            .unwrap_or_else(|| self.lines().join(", "))
    }

    fn lay_line(&mut self, ctx: &mut LayoutCtx, index: usize, m: &BeuiMetrics) -> Size {
        let look = self.look(index, m);
        let style: TextStyle = label_style(look.size).with_weight(look.weight);
        self.lines[index].layout_themed(ctx, &style, look.role)
    }
}

impl Widget for CardWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let m = BeuiMetrics::resolve(Theme::from_layout_ctx(ctx));
        let pad = self.padding.unwrap_or(CARD_PADDING);
        let max_w = bc.max().width;
        let bounded = max_w.is_finite();
        let inner_w = if bounded {
            (max_w - 2.0 * pad).max(0.0)
        } else {
            f64::INFINITY
        };
        let inner = BoxConstraints::loose(Size::new(inner_w, f64::INFINITY));
        let mut content_w = 0.0_f64;
        let mut y = pad;
        self.line_pos.clear();
        // The stat tile's trailing view sits at the end of the label row; its x waits for
        // the final width.
        let mut trailing: Option<(f64, f64)> = None;

        match self.kind {
            Kind::Plain => {
                if let Some(child) = &mut self.child {
                    let size = child.layout_child(ctx, &inner);
                    child.set_origin(Point::new(pad, y));
                    content_w = size.width;
                    y += size.height;
                }
            }
            Kind::Hero => {
                for i in 0..self.lines.len() {
                    if i > 0 {
                        y += LINE_GAP;
                    }
                    let size = self.lay_line(ctx, i, &m);
                    self.line_pos.push(Point::new(pad, y));
                    content_w = content_w.max(size.width);
                    y += size.height;
                }
                for pod in [&mut self.child, &mut self.extra].into_iter().flatten() {
                    y += BLOCK_GAP;
                    let size = pod.layout_child(ctx, &inner);
                    pod.set_origin(Point::new(pad, y));
                    content_w = content_w.max(size.width);
                    y += size.height;
                }
            }
            Kind::Stat => {
                let label = self.lay_line(ctx, 0, &m);
                let mut row_h = label.height;
                let mut row_w = label.width;
                if let Some(pod) = &mut self.extra {
                    let size = pod.layout_child(ctx, &inner);
                    row_h = row_h.max(size.height);
                    row_w += TRAILING_GAP + size.width;
                    trailing = Some((size.width, size.height));
                    pod.set_origin(Point::new(pad, y + (row_h - size.height) / 2.0));
                }
                self.line_pos
                    .push(Point::new(pad, y + (row_h - label.height) / 2.0));
                y += row_h + LINE_GAP;
                let value = self.lay_line(ctx, 1, &m);
                self.line_pos.push(Point::new(pad, y));
                y += value.height;
                content_w = row_w.max(value.width);
            }
        }

        let fill = self.kind != Kind::Plain && bounded;
        let width = if fill { max_w } else { content_w + 2.0 * pad };
        let mut size = bc.constrain(Size::new(width, y + pad));
        // A pressable card is the hit area: never below the touch minimum.
        if self.pressable() {
            size = bc.constrain(Size::new(
                size.width.max(m.min_touch_target),
                size.height.max(m.min_touch_target),
            ));
        }
        if let (Some((w, _)), Some(pod)) = (trailing, &mut self.extra) {
            let origin = pod.origin();
            pod.set_origin(Point::new((size.width - pad - w).max(pad), origin.y));
        }
        size
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let theme = Theme::from_paint_ctx(ctx);
        let scheme = theme.map_or_else(color_scheme_light, |t| *t.scheme());
        let ring = BeuiTokens::resolve_ring(None, theme);
        let m = BeuiMetrics::resolve(theme);
        let origin = ctx.origin();
        let size = ctx.size();
        let radius = resolve_radius(
            self.radius.unwrap_or(m.radius_card),
            size.width,
            size.height,
        );

        let mut fill = if self.elevated {
            scheme.surface_container_high
        } else {
            scheme.surface_container_low
        };
        if self.pressable() {
            let wash = if self.pressed {
                PRESSED_WASH
            } else if ctx.is_hovered() && !self.captured {
                HOVER_WASH
            } else {
                0.0
            };
            if wash > 0.0 {
                fill = lerp_color(fill, scheme.on_surface, wash);
            }
        }
        if self.elevated {
            scene.draw_shadow(
                origin + Vec2::new(0.0, SHADOW_DROP),
                size,
                radius,
                SHADOW_STD_DEV,
                with_alpha(scheme.shadow, SHADOW_ALPHA),
            );
        }
        scene.fill_rounded_rect(origin, size, radius, fill);
        stroke_outline(scene, origin, size, radius, scheme.outline_variant);

        for i in 0..self.lines.len() {
            let ink: Color = if self.look(i, &m).muted {
                scheme.on_surface_variant
            } else {
                scheme.on_surface
            };
            let at = self.line_pos.get(i).copied().unwrap_or(Point::ZERO);
            self.lines[i].paint(origin + at.to_vec2(), ink, scene);
        }
        if let Some(child) = &mut self.child {
            child.paint_child(ctx, scene);
        }
        if let Some(extra) = &mut self.extra {
            extra.paint_child(ctx, scene);
        }
        if self.pressable() && ctx.is_focused_for_ring() {
            draw_focus_ring(scene, origin, size, radius, RING_OFFSET, ring);
        }
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        if self.on_press.is_none() {
            return EventResult::Ignored;
        }
        match event {
            InputEvent::Key(key) => {
                if !is_activation_key(key) {
                    return EventResult::Ignored;
                }
                if let Some(on_press) = &mut self.on_press {
                    on_press(ctx);
                }
                EventResult::Handled
            }
            InputEvent::Pointer(p) => {
                let size = ctx.size();
                match p.phase {
                    PointerPhase::Down => {
                        if !presses(p) || !inside(p.position, size) {
                            return EventResult::Ignored;
                        }
                        self.pressed = true;
                        self.captured = true;
                        ctx.capture_pointer();
                        ctx.request_focus();
                        ctx.request_redraw();
                        EventResult::Handled
                    }
                    PointerPhase::Move => {
                        let over = inside(p.position, size);
                        if self.captured {
                            if self.pressed != over {
                                self.pressed = over;
                                ctx.request_redraw();
                            }
                            ctx.set_cursor(ACTIVE_CURSOR);
                            return EventResult::Handled;
                        }
                        if over {
                            ctx.claim_hover();
                            ctx.set_cursor(ACTIVE_CURSOR);
                        }
                        EventResult::Ignored
                    }
                    PointerPhase::Up => {
                        if !self.captured {
                            return EventResult::Ignored;
                        }
                        let fire = inside(p.position, size);
                        self.pressed = false;
                        self.captured = false;
                        if fire && let Some(on_press) = &mut self.on_press {
                            on_press(ctx);
                        }
                        ctx.request_redraw();
                        EventResult::Handled
                    }
                    PointerPhase::Cancel => {
                        if !self.captured {
                            return EventResult::Ignored;
                        }
                        self.pressed = false;
                        self.captured = false;
                        ctx.request_redraw();
                        EventResult::Handled
                    }
                }
            }
            _ => EventResult::Ignored,
        }
    }

    fn is_focusable(&self) -> bool {
        self.pressable()
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        let visit = |ctx: &mut SemanticsCtx| {
            for pod in [&self.child, &self.extra].into_iter().flatten() {
                pod.semantics_child(ctx);
            }
        };
        if self.pressable() {
            let name = self.name();
            ctx.push_container(
                Role::Button,
                |node| {
                    node.set_label(name);
                    node.add_action(frust::authoring::Action::Click);
                },
                visit,
            );
        } else {
            visit(ctx);
        }
    }

    fn visit_children(&self, visitor: &mut dyn FnMut(&ChildPod)) {
        for pod in [&self.child, &self.extra].into_iter().flatten() {
            visitor(pod);
        }
    }
}

/// A plain card around one child. See the [module docs](self).
pub struct CardView<State: 'static> {
    child: AnyView<State>,
    chrome: Chrome<State>,
}

/// Wrap `child` in a plain card.
pub fn card<State: 'static, V: View<State>>(child: V) -> CardView<State> {
    CardView {
        child: any(child),
        chrome: Chrome::new(),
    }
}

chrome_builders!(CardView);

impl<State: 'static> View<State> for CardView<State> {
    type Element = CardWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> CardWidget {
        let mut widget = CardWidget::new(Kind::Plain, Vec::new());
        self.chrome.apply(&mut widget);
        widget.child = Some(build_child(&self.child, ctx));
        widget
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut CardWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = self.chrome.apply(element);
        if let Some(pod) = &mut element.child {
            flags |= rebuild_child(&prev.child, &self.child, pod, ctx);
        }
        flags
    }

    fn teardown(&self, element: &mut CardWidget, ctx: &mut BuildCtx<'_>) {
        if let Some(pod) = &mut element.child {
            teardown_child(&self.child, pod, ctx);
        }
    }
}

/// A card with a title above its content. See the [module docs](self).
pub struct HeroCardView<State: 'static> {
    title: String,
    subtitle: Option<String>,
    child: AnyView<State>,
    actions: Option<AnyView<State>>,
    chrome: Chrome<State>,
}

/// Wrap `child` in a hero card headed by `title`.
pub fn hero_card<State: 'static, V: View<State>>(
    title: impl Into<String>,
    child: V,
) -> HeroCardView<State> {
    HeroCardView {
        title: title.into(),
        subtitle: None,
        child: any(child),
        actions: None,
        chrome: Chrome::new(),
    }
}

chrome_builders!(HeroCardView);

impl<State: 'static> HeroCardView<State> {
    /// A supporting line under the title.
    #[must_use]
    pub fn subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    /// The action row under the content. The views flow left to right and wrap onto
    /// further lines when the card is too narrow to hold them on one.
    #[must_use]
    pub fn actions<M>(mut self, actions: impl ViewSeq<State, M>) -> Self {
        self.actions = Some(any(wrap()
            .spacing(ACTION_GAP)
            .run_spacing(ACTION_GAP)
            .children(actions)));
        self
    }

    fn lines(&self) -> Vec<String> {
        let mut lines = vec![self.title.clone()];
        lines.extend(self.subtitle.clone());
        lines
    }
}

impl<State: 'static> View<State> for HeroCardView<State> {
    type Element = CardWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> CardWidget {
        let mut widget = CardWidget::new(Kind::Hero, self.lines());
        self.chrome.apply(&mut widget);
        widget.child = Some(build_child(&self.child, ctx));
        widget.extra = self.actions.as_ref().map(|a| build_child(a, ctx));
        widget
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut CardWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = self.chrome.apply(element) | element.set_lines(self.lines());
        if let Some(pod) = &mut element.child {
            flags |= rebuild_child(&prev.child, &self.child, pod, ctx);
        }
        flags |= rebuild_slot(&prev.actions, &self.actions, &mut element.extra, ctx);
        flags
    }

    fn teardown(&self, element: &mut CardWidget, ctx: &mut BuildCtx<'_>) {
        if let Some(pod) = &mut element.child {
            teardown_child(&self.child, pod, ctx);
        }
        if let (Some(view), Some(pod)) = (&self.actions, &mut element.extra) {
            teardown_child(view, pod, ctx);
        }
    }
}

/// A tile showing one labelled figure. See the [module docs](self).
pub struct StatTileView<State: 'static = ()> {
    label: String,
    value: String,
    trailing: Option<AnyView<State>>,
    chrome: Chrome<State>,
}

/// A stat tile pairing a `label` with its `value`.
pub fn stat_tile<State: 'static>(
    label: impl Into<String>,
    value: impl Into<String>,
) -> StatTileView<State> {
    StatTileView {
        label: label.into(),
        value: value.into(),
        trailing: None,
        chrome: Chrome::new(),
    }
}

chrome_builders!(StatTileView);

impl<State: 'static> StatTileView<State> {
    /// A view at the end of the label row (an icon, a delta chip).
    #[must_use]
    pub fn trailing<V: View<State>>(mut self, trailing: V) -> Self {
        self.trailing = Some(any(trailing));
        self
    }

    fn lines(&self) -> Vec<String> {
        vec![self.label.clone(), self.value.clone()]
    }
}

impl<State: 'static> View<State> for StatTileView<State> {
    type Element = CardWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> CardWidget {
        let mut widget = CardWidget::new(Kind::Stat, self.lines());
        self.chrome.apply(&mut widget);
        widget.extra = self.trailing.as_ref().map(|t| build_child(t, ctx));
        widget
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut CardWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = self.chrome.apply(element) | element.set_lines(self.lines());
        flags |= rebuild_slot(&prev.trailing, &self.trailing, &mut element.extra, ctx);
        flags
    }

    fn teardown(&self, element: &mut CardWidget, ctx: &mut BuildCtx<'_>) {
        if let (Some(view), Some(pod)) = (&self.trailing, &mut element.extra) {
            teardown_child(view, pod, ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::button::button;
    use crate::tokens::{Density, theme_with_density};
    use frust::Brightness;
    use frust::FrameTime;
    use frust::authoring::text::TextContext;
    use frust::authoring::{PointerButton, PointerEvent};
    use std::any::Any;
    use std::cell::Cell;

    #[derive(Default)]
    struct Counter {
        presses: u32,
    }

    /// Every theme the entry points must work under.
    fn themes() -> Vec<(String, Theme)> {
        let mut out = Vec::new();
        for density in [Density::Comfortable, Density::Touch] {
            for brightness in [Brightness::Light, Brightness::Dark] {
                out.push((
                    format!("{density:?}/{brightness:?}"),
                    theme_with_density(density).with_brightness(brightness),
                ));
            }
        }
        out
    }

    fn build<S: 'static, V: View<S>>(view: &V) -> V::Element {
        let mut next_id = 0u64;
        view.build(&mut BuildCtx::new(&mut next_id))
    }

    fn layout_at(w: &mut CardWidget, theme: &Theme, width: f64) -> Size {
        let mut text_ctx = TextContext::new();
        let mut ctx =
            LayoutCtx::with_resources(Some(&mut text_ctx as &mut dyn Any), Some(theme as &dyn Any));
        w.layout(&mut ctx, &BoxConstraints::loose(Size::new(width, 600.0)))
    }

    /// Fills recorded for one paint.
    #[derive(Default)]
    struct Fills(Vec<(Size, Color)>);

    impl PaintScene for Fills {
        fn fill_rect(&mut self, _o: Point, _s: Size, _c: Color) {}
        fn draw_text(&mut self, _o: Point, _t: &str) {}
        fn fill_rounded_rect(&mut self, _o: Point, s: Size, _r: f64, c: Color) {
            self.0.push((s, c));
        }
    }

    fn paint(w: &mut CardWidget, size: Size, theme: &Theme) -> Fills {
        let mut ctx =
            PaintCtx::for_test(Point::ORIGIN, size, FrameTime::from_nanos(0)).with_theme(theme);
        let mut fills = Fills::default();
        w.paint(&mut ctx, &mut fills);
        fills
    }

    fn ev(phase: PointerPhase, x: f64, y: f64) -> InputEvent {
        InputEvent::Pointer(PointerEvent {
            phase,
            position: Point::new(x, y),
            button: PointerButton::Primary,
        })
    }

    fn dispatch(w: &mut CardWidget, state: &mut Counter, size: Size, event: &InputEvent) {
        let any_state: &mut dyn Any = state;
        let mut ctx = EventCtx::new(any_state, Point::ZERO, size);
        w.event(&mut ctx, event);
    }

    #[test]
    fn the_card_entry_points_build_and_lay_out_under_every_theme() {
        for (name, theme) in themes() {
            let mut plain = build(&card::<(), _>(stat_tile::<()>("Open", "12")));
            let plain_size = layout_at(&mut plain, &theme, 400.0);
            assert!(plain_size.width > 0.0 && plain_size.height > 0.0, "{name}");

            let mut hero = build(
                &hero_card::<(), _>("Title", stat_tile::<()>("Open", "12"))
                    .subtitle("Supporting")
                    .actions((button::<()>("A", |_| {}), button::<()>("B", |_| {}))),
            );
            assert_eq!(hero.lines(), vec!["Title", "Supporting"]);
            let hero_size = layout_at(&mut hero, &theme, 400.0);
            assert_eq!(
                hero_size.width, 400.0,
                "{name}: a hero card fills the width"
            );
            assert!(hero_size.height > plain_size.height, "{name}");

            let mut tile =
                build(&stat_tile::<()>("Open", "12").trailing(button::<()>("+", |_| {})));
            assert_eq!(tile.lines(), vec!["Open", "12"]);
            let tile_size = layout_at(&mut tile, &theme, 180.0);
            assert_eq!(tile_size.width, 180.0, "{name}: a tile fills its grid cell");

            let fills = paint(&mut tile, tile_size, &theme);
            assert_eq!(
                fills.0[0].0, tile_size,
                "{name}: the surface fills the tile"
            );
            assert!(fills.0[0].1.components[3] > 0.0, "{name}");
        }
    }

    #[test]
    fn the_surface_follows_the_brightness() {
        let mut light = build(&card::<(), _>(stat_tile::<()>("a", "1")));
        let mut dark = build(&card::<(), _>(stat_tile::<()>("a", "1")));
        let l = theme_with_density(Density::Comfortable);
        let d = l.clone().with_brightness(Brightness::Dark);
        let size = layout_at(&mut light, &l, 400.0);
        layout_at(&mut dark, &d, 400.0);
        assert_ne!(
            paint(&mut light, size, &l).0[0].1,
            paint(&mut dark, size, &d).0[0].1
        );
    }

    #[test]
    fn padding_and_radius_overrides_apply() {
        let theme = theme_with_density(Density::Comfortable);
        let mut base = build(&card::<(), _>(SizedChip));
        let mut roomy = build(
            &card::<(), _>(SizedChip)
                .padding(CARD_PADDING + 10.0)
                .radius(3.0),
        );
        let a = layout_at(&mut base, &theme, 400.0);
        let b = layout_at(&mut roomy, &theme, 400.0);
        assert!((b.width - a.width - 20.0).abs() < 1e-6);
        assert_eq!(roomy.radius, Some(3.0));
    }

    #[test]
    fn a_pressable_card_has_a_touch_sized_hit_area_and_fires_once_per_press() {
        let theme = theme_with_density(Density::Touch);
        let floor = BeuiMetrics::resolve(Some(&theme)).min_touch_target;
        assert!(floor >= 44.0);
        // A child far smaller than the floor, with no padding.
        let view = card::<Counter, _>(SizedChip)
            .padding(0.0)
            .on_press(|c: &mut Counter| c.presses += 1);
        let mut w = build(&view);
        let size = layout_at(&mut w, &theme, 400.0);
        assert!(size.width >= floor && size.height >= floor, "{size:?}");

        let mut state = Counter::default();
        dispatch(
            &mut w,
            &mut state,
            size,
            &ev(PointerPhase::Down, 1.0, size.height - 1.0),
        );
        assert!(w.pressed);
        dispatch(
            &mut w,
            &mut state,
            size,
            &ev(PointerPhase::Up, 1.0, size.height - 1.0),
        );
        assert_eq!(state.presses, 1);
        // A press released outside does not fire; a stray Up fires nothing.
        dispatch(&mut w, &mut state, size, &ev(PointerPhase::Down, 5.0, 5.0));
        dispatch(&mut w, &mut state, size, &ev(PointerPhase::Up, -20.0, 5.0));
        dispatch(&mut w, &mut state, size, &ev(PointerPhase::Up, 5.0, 5.0));
        assert_eq!(state.presses, 1);
        // A plain card is not pressable.
        let mut plain = build(&card::<Counter, _>(SizedChip));
        layout_at(&mut plain, &theme, 400.0);
        dispatch(
            &mut plain,
            &mut state,
            size,
            &ev(PointerPhase::Down, 5.0, 5.0),
        );
        assert!(!plain.pressed && !plain.is_focusable());
    }

    #[test]
    fn a_pressed_card_washes_its_fill_and_publishes_a_button() {
        let theme = theme_with_density(Density::Comfortable);
        let view = card::<Counter, _>(SizedChip)
            .label("Open item")
            .on_press(|_| {});
        let mut w = build(&view);
        let size = layout_at(&mut w, &theme, 400.0);
        let rest = paint(&mut w, size, &theme).0[0].1;
        let mut state = Counter::default();
        dispatch(&mut w, &mut state, size, &ev(PointerPhase::Down, 5.0, 5.0));
        assert_ne!(paint(&mut w, size, &theme).0[0].1, rest);
        assert!(w.is_focusable());
        assert_eq!(w.name(), "Open item");
    }

    /// A fixed-size leaf, 20 x 10.
    struct SizedChip;
    struct SizedChipWidget;
    impl Widget for SizedChipWidget {
        fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
            bc.constrain(Size::new(20.0, 10.0))
        }
        fn paint(&mut self, _ctx: &mut PaintCtx, _scene: &mut dyn PaintScene) {}
    }
    impl<S: 'static> View<S> for SizedChip {
        type Element = SizedChipWidget;
        fn build(&self, _ctx: &mut BuildCtx<'_>) -> SizedChipWidget {
            SizedChipWidget
        }
        fn rebuild(
            &self,
            _p: &Self,
            _e: &mut SizedChipWidget,
            _c: &mut BuildCtx<'_>,
        ) -> ChangeFlags {
            ChangeFlags::NONE
        }
    }

    /// Counts how often its layout runs.
    struct Probe(Rc<Cell<u32>>);
    struct ProbeWidget(Rc<Cell<u32>>);
    impl Widget for ProbeWidget {
        fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
            self.0.set(self.0.get() + 1);
            bc.constrain(Size::new(30.0, 20.0))
        }
        fn paint(&mut self, _ctx: &mut PaintCtx, _scene: &mut dyn PaintScene) {}
    }
    impl View<()> for Probe {
        type Element = ProbeWidget;
        fn build(&self, _ctx: &mut BuildCtx<'_>) -> ProbeWidget {
            ProbeWidget(self.0.clone())
        }
        fn rebuild(&self, _p: &Self, _e: &mut ProbeWidget, _c: &mut BuildCtx<'_>) -> ChangeFlags {
            ChangeFlags::NONE
        }
    }

    /// Mount `view`, then run `layout` twice more with nothing changed.
    fn skip_check<V: View<()> + 'static>(view: impl Fn() -> V + 'static, probe: &Rc<Cell<u32>>) {
        let mut root = frust_core::RenderRoot::new();
        root.set_theme(Box::new(theme_with_density(Density::Touch)));
        let mut state = ();
        let mut tcx = TextContext::new();
        let mut logic = move |_s: &mut ()| view();
        root.rebuild(&mut logic, &mut state);
        root.layout_with_text(Size::new(300.0, 600.0), &mut tcx as &mut dyn Any);
        let first = probe.get();
        assert!(first >= 1, "the probe was laid out");
        // A rebuild that changes nothing, then a second pass.
        root.rebuild(&mut logic, &mut state);
        root.layout_with_text(Size::new(300.0, 600.0), &mut tcx as &mut dyn Any);
        root.layout_with_text(Size::new(300.0, 600.0), &mut tcx as &mut dyn Any);
        assert_eq!(probe.get(), first, "children re-laid out");
        // The audit's forced pass finds nothing stale either.
        root.set_layout_audit(true);
        root.layout_with_text(Size::new(300.0, 600.0), &mut tcx as &mut dyn Any);
        assert_eq!(root.layout_audit_mismatches(), 0);
    }

    #[test]
    fn a_second_layout_pass_with_nothing_changed_skips_the_children() {
        let probe = Rc::new(Cell::new(0));
        let p = probe.clone();
        skip_check(move || card::<(), _>(Probe(p.clone())), &probe);
        let probe = Rc::new(Cell::new(0));
        let p = probe.clone();
        skip_check(
            move || hero_card::<(), _>("T", Probe(p.clone())).actions(button::<()>("A", |_| {})),
            &probe,
        );
        let probe = Rc::new(Cell::new(0));
        let p = probe.clone();
        skip_check(
            move || stat_tile::<()>("a", "1").trailing(Probe(p.clone())),
            &probe,
        );
    }

    #[test]
    fn the_hero_actions_wrap_at_a_narrow_width() {
        let mut root = frust_core::RenderRoot::new();
        root.set_theme(Box::new(theme_with_density(Density::Touch)));
        let mut state = ();
        let mut tcx = TextContext::new();
        let mut logic = |_s: &mut ()| {
            hero_card::<(), _>("Plan", SizedChip).actions((
                button::<()>("Approve all", |_| {}),
                button::<()>("Decline all", |_| {}),
                button::<()>("Later", |_| {}),
            ))
        };
        let buttons = |root: &frust_core::RenderRoot<(), _>| {
            let mut found: Vec<_> = root
                .inspect()
                .into_iter()
                .filter(|n| n.type_name.ends_with("ButtonWidget"))
                .map(|n| n.bounds)
                .collect();
            found.sort_by(|a, b| a.y0.total_cmp(&b.y0).then(a.x0.total_cmp(&b.x0)));
            found
        };
        root.rebuild(&mut logic, &mut state);
        root.layout_with_text(Size::new(600.0, 800.0), &mut tcx as &mut dyn Any);
        let wide = buttons(&root);
        assert_eq!(wide.len(), 3);
        assert!(
            wide.iter().all(|r| r.y0 == wide[0].y0),
            "one line when wide: {wide:?}"
        );

        root.layout_with_text(Size::new(200.0, 800.0), &mut tcx as &mut dyn Any);
        let narrow = buttons(&root);
        assert_eq!(narrow.len(), 3);
        assert!(
            narrow[2].y0 > narrow[0].y0,
            "the last action wraps: {narrow:?}"
        );
        assert!(
            narrow.iter().all(|r| r.x1 <= 200.0 - CARD_PADDING + 1e-6),
            "{narrow:?}"
        );
    }
}
