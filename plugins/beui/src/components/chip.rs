//! Chips: a compact [`chip`] and a toned [`status_badge`].
//!
//! The pill a phone app uses for filters, tags and state.
//!
//! - [`chip`] is a static or pressable pill: an optional [`leading`](ChipView::leading)
//!   view, a label, and an optional trailing close affordance
//!   ([`on_remove`](ChipView::on_remove)) drawn as a shape. A
//!   [`selected`](ChipView::selected) chip takes the accent. A pressable chip
//!   (one with `on_press` or `on_remove`) keeps its painted pill, but under a
//!   touch density the hit area grows around it to the platform minimum
//!   ([`BeuiMetrics::min_touch_target`]), the same way [`button`](super::button)
//!   does. `on_remove` fires only for a release in the trailing close zone and
//!   never also fires `on_press`.
//!
//!   A pressable chip is a Tab stop. While it holds keyboard focus it paints a
//!   ring around the painted pill (not the enlarged hit area); `Enter` and
//!   `Space` run `on_press`, and `Delete` or `Backspace` run `on_remove`. Tab
//!   itself is declined, so traversal moves on.
//! - [`status_badge`] is the static, non-animated sibling of
//!   [`animated_badge`](super::animated_badge): a soft-filled pill tinted by a
//!   [`StatusBadgeTone`], with an optional leading dot.
//!
//! # Colour roles
//!
//! The tone-to-role choice repeats `animated_badge`'s (its mapping is private,
//! so the roles, not literal colours, are repeated here): `Success` and
//! `Warning` read [`BeuiTokens`]' `success` / `warning`, `Danger` reads the
//! scheme's `error`, and `Info` reads the scheme's `primary` (the role
//! `animated_badge`'s `info` tone uses; the palette has no info role).
//! `Neutral` reads the muted roles (`surface_container` fill,
//! `on_surface_variant` ink, `outline_variant` border).
//!
//! The badge's label is the semantics label of a [`Role::Status`] node; a chip
//! with `on_press` is a [`Role::Button`] with a click action. A chip with
//! `on_remove` also publishes a child [`Role::Button`] labelled `Remove <label>`
//! with its own click action, so removal is reachable through the tree; a
//! remove-only chip's body is a plain label.

use std::rc::Rc;

use frust::Theme;
use frust::authoring::text::TextStyle;
use frust::authoring::{
    Action, AnyView, BezPath, BoxConstraints, BuildCtx, ChangeFlags, ChildPod, Color,
    ErasedCallback, EventCtx, EventResult, InputEvent, Key, LayoutCtx, NamedKey, PaintCtx,
    PaintScene, Point, PointerPhase, Role, SemanticsCtx, Size, Vec2, View, Widget, any,
    build_child, erase_callback, rebuild_child, teardown_child,
};
use peniko::Brush;

use crate::press::{draw_focus_ring, inside, is_activation_key, presses, stroke_outline};
use crate::style::{RADIUS_CONTROL, resolve_radius, spacing, with_alpha};
use crate::text::{LabelRun, ThemeTextType, label_style};
use crate::tokens::{BEUI_LIGHT, BeuiMetrics, BeuiTokens};

/// Alpha a tone's soft fill is painted at.
const TONE_FILL_ALPHA: f32 = 0.14;
/// Alpha a tone's border is painted at.
const TONE_BORDER_ALPHA: f32 = 0.30;
/// Alpha a selected chip's fill is painted at.
const SELECTED_FILL_ALPHA: f32 = 0.14;
/// Alpha of the ink overlay on a pressed chip.
const PRESS_OVERLAY_ALPHA: f32 = 0.08;

/// Gap between the painted pill and its keyboard focus ring.
const RING_OFFSET: f64 = 2.0;

/// Floor on the painted chip pill height.
fn chip_min_height() -> f64 {
    spacing(8.0)
}

/// The retained widget behind the chip entry points.
pub struct ChipWidget {
    label: LabelRun,
    leading: Option<ChildPod>,
    kind: Kind,
    selected: bool,
    show_dot: bool,
    on_press: Option<ErasedCallback>,
    on_remove: Option<ErasedCallback>,
    /// Which zone the live pointer capture started in.
    captured: Option<Zone>,
    pressed: bool,
    /// The painted pill, centred inside the laid-out hit area.
    pill: Size,
    metrics: BeuiMetrics,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Chip,
    Badge(StatusBadgeTone),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Zone {
    Body,
    Remove,
}

impl ChipWidget {
    fn new(label: String, kind: Kind) -> Self {
        Self {
            label: LabelRun::new(label),
            leading: None,
            kind,
            selected: false,
            show_dot: false,
            on_press: None,
            on_remove: None,
            captured: None,
            pressed: false,
            pill: Size::ZERO,
            metrics: BeuiMetrics::comfortable(),
        }
    }

    /// The text lines this widget carries, top to bottom.
    pub fn lines(&self) -> Vec<&str> {
        vec![self.label.content()]
    }

    /// Whether the widget reacts to a pointer.
    fn pressable(&self) -> bool {
        self.on_press.is_some() || self.on_remove.is_some()
    }

    /// The trailing close affordance's edge.
    fn remove_icon(&self) -> f64 {
        spacing(4.0)
    }

    /// Width the trailing close zone takes inside the pill (affordance plus the
    /// pill's trailing padding and half the gap before it).
    fn remove_zone_width(&self) -> f64 {
        self.remove_icon() + spacing(3.0) + spacing(1.0)
    }

    /// Which zone `pos` (widget-local) falls in, over the laid-out hit area.
    fn zone_at(&self, pos: Point, hit: Size) -> Zone {
        if self.on_remove.is_none() {
            return Zone::Body;
        }
        let pill_right = hit.width - (hit.width - self.pill.width).max(0.0) / 2.0;
        if pos.x >= pill_right - self.remove_zone_width() {
            Zone::Remove
        } else {
            Zone::Body
        }
    }
}

/// The resolved fill, border and ink for one [`StatusBadgeTone`].
#[derive(Clone, Copy, Debug, PartialEq)]
struct ToneColors {
    fill: Color,
    border: Color,
    ink: Color,
}

/// The fill, border and ink `tone` paints with; roles as the
/// [module docs](self) state.
fn tone_colors(tone: StatusBadgeTone, theme: Option<&Theme>) -> ToneColors {
    let tokens = BeuiTokens::resolve(theme);
    let scheme = theme.map(Theme::scheme);
    let hue =
        |pick: fn(&frust::ColorScheme) -> Color, fallback: Color| scheme.map_or(fallback, pick);
    let soft = |hue: Color| ToneColors {
        fill: with_alpha(hue, TONE_FILL_ALPHA),
        border: with_alpha(hue, TONE_BORDER_ALPHA),
        ink: hue,
    };
    match tone {
        StatusBadgeTone::Info => soft(hue(|s| s.primary, BEUI_LIGHT.primary)),
        StatusBadgeTone::Success => soft(tokens.success),
        StatusBadgeTone::Warning => soft(tokens.warning),
        StatusBadgeTone::Danger => soft(hue(|s| s.error, BEUI_LIGHT.danger)),
        StatusBadgeTone::Neutral => ToneColors {
            fill: hue(|s| s.surface_container, BEUI_LIGHT.card),
            border: hue(|s| s.outline_variant, BEUI_LIGHT.border),
            ink: hue(|s| s.on_surface_variant, BEUI_LIGHT.muted_foreground),
        },
    }
}

/// The colours a chip paints with.
fn chip_colors(selected: bool, theme: Option<&Theme>) -> ToneColors {
    let scheme = theme.map(Theme::scheme);
    if selected {
        let primary = scheme.map_or(BEUI_LIGHT.primary, |s| s.primary);
        ToneColors {
            fill: with_alpha(primary, SELECTED_FILL_ALPHA),
            border: primary,
            ink: primary,
        }
    } else {
        ToneColors {
            fill: scheme.map_or(BEUI_LIGHT.card, |s| s.surface_container),
            border: scheme.map_or(BEUI_LIGHT.border, |s| s.outline_variant),
            ink: scheme.map_or(BEUI_LIGHT.muted_foreground, |s| s.on_surface),
        }
    }
}

impl Widget for ChipWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let m = BeuiMetrics::resolve(Theme::from_layout_ctx(ctx));
        self.metrics = m;
        let (size, role) = match self.kind {
            Kind::Chip => (m.text_sm, ThemeTextType::LabelLarge),
            Kind::Badge(_) => (m.text_xs, ThemeTextType::LabelSmall),
        };
        let style: TextStyle = label_style(size);
        let text = self.label.layout_themed(ctx, &style, role);

        let (pad_x, gap, vpad) = match self.kind {
            Kind::Chip => (spacing(3.0), spacing(2.0), spacing(2.0)),
            Kind::Badge(_) => (spacing(2.0), spacing(1.5), spacing(1.0)),
        };
        let mut width = pad_x * 2.0 + text.width;
        let mut height = text.height + vpad * 2.0;

        if let Some(lead) = &mut self.leading {
            let loose =
                BoxConstraints::loose(Size::new(bc.max().width, text.height.max(m.icon_size)));
            let lead_size = lead.layout_child(ctx, &loose);
            width += lead_size.width + gap;
            height = height.max(lead_size.height + vpad * 2.0);
        } else if self.show_dot {
            width += self.dot_size() + gap;
        }
        if self.on_remove.is_some() {
            width += self.remove_icon() + gap;
        }
        if self.kind == Kind::Chip {
            height = height.max(chip_min_height());
        }

        // The painted pill keeps its own size; a pressable chip's hit area
        // grows around it to the platform minimum.
        let pill = bc.constrain(Size::new(width, height));
        self.pill = pill;
        if self.pressable() {
            bc.constrain(Size::new(
                pill.width.max(m.min_touch_target),
                pill.height.max(m.min_touch_target),
            ))
        } else {
            pill
        }
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let theme = Theme::from_paint_ctx(ctx);
        let colors = match self.kind {
            Kind::Chip => chip_colors(self.selected, theme),
            Kind::Badge(tone) => tone_colors(tone, theme),
        };
        let ring = BeuiTokens::resolve_ring(None, theme);
        let hit = ctx.size();
        let pill = if self.pill == Size::ZERO {
            hit
        } else {
            Size::new(
                self.pill.width.min(hit.width),
                self.pill.height.min(hit.height),
            )
        };
        let origin = ctx.origin()
            + Vec2::new(
                (hit.width - pill.width) / 2.0,
                (hit.height - pill.height) / 2.0,
            );
        let radius = resolve_radius(RADIUS_CONTROL, pill.width, pill.height);

        scene.fill_rounded_rect(origin, pill, radius, colors.fill);
        if self.pressed && self.captured == Some(Zone::Body) {
            scene.fill_rounded_rect(
                origin,
                pill,
                radius,
                with_alpha(colors.ink, PRESS_OVERLAY_ALPHA),
            );
        }
        stroke_outline(scene, origin, pill, radius, colors.border);

        let (pad_x, gap) = match self.kind {
            Kind::Chip => (spacing(3.0), spacing(2.0)),
            Kind::Badge(_) => (spacing(2.0), spacing(1.5)),
        };
        let mut x = pad_x;
        if let Some(lead) = &mut self.leading {
            let lead_h = lead.size().height;
            lead.set_origin(Point::new(
                (hit.width - pill.width) / 2.0 + x,
                (hit.height - lead_h) / 2.0,
            ));
            x += lead.size().width + gap;
            lead.paint_child(ctx, scene);
        } else if self.show_dot {
            let d = self.dot_size();
            scene.fill_rounded_rect(
                origin + Vec2::new(x, (pill.height - d) / 2.0),
                Size::new(d, d),
                d / 2.0,
                colors.ink,
            );
            x += d + gap;
        }
        let text_h = self.label.size().height;
        self.label.paint(
            origin + Vec2::new(x, (pill.height - text_h) / 2.0),
            colors.ink,
            scene,
        );

        if self.on_remove.is_some() {
            let edge = self.remove_icon();
            let inset = edge * 0.3;
            let mut cross = BezPath::new();
            cross.move_to((inset, inset));
            cross.line_to((edge - inset, edge - inset));
            cross.move_to((edge - inset, inset));
            cross.line_to((inset, edge - inset));
            let at = origin + Vec2::new(pill.width - pad_x - edge, (pill.height - edge) / 2.0);
            let pressed_remove = self.pressed && self.captured == Some(Zone::Remove);
            let ink = if pressed_remove {
                with_alpha(colors.ink, 0.6)
            } else {
                colors.ink
            };
            scene.stroke_path(at, &cross, 1.5, &Brush::Solid(ink));
        }

        if self.pressable() && ctx.is_focused_for_ring() {
            draw_focus_ring(scene, origin, pill, radius, RING_OFFSET, ring);
        }
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        if !self.pressable() {
            return EventResult::Ignored;
        }
        let p = match event {
            InputEvent::Pointer(p) => p,
            InputEvent::Key(key) => {
                // Tab and every other key are declined so traversal moves on.
                let cb = if is_activation_key(key) {
                    self.on_press.as_mut()
                } else if matches!(key.key, Key::Named(NamedKey::Delete | NamedKey::Backspace)) {
                    self.on_remove.as_mut()
                } else {
                    None
                };
                return match cb {
                    Some(cb) => {
                        cb(ctx);
                        EventResult::Handled
                    }
                    None => EventResult::Ignored,
                };
            }
            _ => return EventResult::Ignored,
        };
        let size = ctx.size();
        match p.phase {
            PointerPhase::Down => {
                if !presses(p) || !inside(p.position, size) {
                    return EventResult::Ignored;
                }
                let zone = self.zone_at(p.position, size);
                // A body press with no `on_press` is not ours.
                if zone == Zone::Body && self.on_press.is_none() {
                    return EventResult::Ignored;
                }
                self.captured = Some(zone);
                self.pressed = true;
                ctx.capture_pointer();
                ctx.request_focus();
                ctx.request_redraw();
                EventResult::Handled
            }
            PointerPhase::Move => {
                let Some(zone) = self.captured else {
                    return EventResult::Ignored;
                };
                self.pressed = inside(p.position, size) && self.zone_at(p.position, size) == zone;
                ctx.request_redraw();
                EventResult::Handled
            }
            PointerPhase::Up => {
                let Some(zone) = self.captured.take() else {
                    return EventResult::Ignored;
                };
                if inside(p.position, size) && self.zone_at(p.position, size) == zone {
                    let cb = match zone {
                        Zone::Body => self.on_press.as_mut(),
                        Zone::Remove => self.on_remove.as_mut(),
                    };
                    if let Some(cb) = cb {
                        cb(ctx);
                    }
                }
                self.pressed = false;
                ctx.request_redraw();
                EventResult::Handled
            }
            PointerPhase::Cancel => {
                if self.captured.take().is_none() {
                    return EventResult::Ignored;
                }
                self.pressed = false;
                ctx.request_redraw();
                EventResult::Handled
            }
        }
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        let label = self.label.content();
        if self.kind != Kind::Chip || !self.pressable() {
            let role = match self.kind {
                Kind::Badge(_) => Role::Status,
                Kind::Chip => Role::Label,
            };
            ctx.push_node(role, |node| node.set_label(label));
            return;
        }
        let has_focus = ctx.has_focus();
        let body_role = if self.on_press.is_some() {
            Role::Button
        } else {
            Role::Label
        };
        let mut remove_id = None;
        let id = ctx.push_container(
            body_role,
            |node| {
                node.set_label(label);
                if self.on_press.is_some() {
                    node.add_action(Action::Click);
                }
            },
            |ctx| {
                if self.on_remove.is_some() {
                    remove_id = Some(ctx.push_node(Role::Button, |node| {
                        node.set_label(format!("Remove {label}"));
                        node.add_action(Action::Click);
                    }));
                }
            },
        );
        if has_focus {
            // A remove-only chip has no body action, so the removable node
            // carries the focus.
            ctx.set_focused(if self.on_press.is_some() {
                id
            } else {
                remove_id.unwrap_or(id)
            });
        }
    }

    fn is_focusable(&self) -> bool {
        self.pressable()
    }

    fn visit_children(&self, visitor: &mut dyn FnMut(&ChildPod)) {
        if let Some(child) = &self.leading {
            visitor(child);
        }
    }
}

impl ChipWidget {
    /// The leading dot's diameter.
    fn dot_size(&self) -> f64 {
        spacing(1.5)
    }
}

/// How a [`status_badge`] reads at a glance.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StatusBadgeTone {
    /// No particular meaning.
    #[default]
    Neutral,
    /// Informational.
    Info,
    /// Completed or healthy.
    Success,
    /// Needs attention.
    Warning,
    /// Failed or destructive.
    Danger,
}

/// A stored `Fn(&mut State)` callback.
type Handler<State> = Rc<dyn Fn(&mut State)>;

/// A compact label chip. See the [module docs](self).
pub struct ChipView<State: 'static = ()> {
    label: String,
    leading: Option<AnyView<State>>,
    selected: bool,
    on_press: Option<Handler<State>>,
    on_remove: Option<Handler<State>>,
}

/// A chip reading `label`.
pub fn chip<State: 'static>(label: impl Into<String>) -> ChipView<State> {
    ChipView {
        label: label.into(),
        leading: None,
        selected: false,
        on_press: None,
        on_remove: None,
    }
}

impl<State: 'static> ChipView<State> {
    /// Place `view` (an icon or avatar) before the label.
    pub fn leading<V: View<State>>(mut self, view: V) -> Self {
        self.leading = Some(any(view));
        self
    }

    /// Show the chip as selected (accent fill and border).
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Run `f` when the chip body is released inside it. Makes the chip
    /// pressable.
    pub fn on_press(mut self, f: impl Fn(&mut State) + 'static) -> Self {
        self.on_press = Some(Rc::new(f));
        self
    }

    /// Show a trailing close affordance and run `f` when it is released. Fires
    /// instead of, never alongside, `on_press`.
    pub fn on_remove(mut self, f: impl Fn(&mut State) + 'static) -> Self {
        self.on_remove = Some(Rc::new(f));
        self
    }

    fn sync(&self, element: &mut ChipWidget) {
        element.selected = self.selected;
        element.on_press = self.on_press.as_ref().map(erase_callback);
        element.on_remove = self.on_remove.as_ref().map(erase_callback);
        // A press on a zone whose handler is gone can never complete.
        let stale = match element.captured {
            Some(Zone::Body) => element.on_press.is_none(),
            Some(Zone::Remove) => element.on_remove.is_none(),
            None => false,
        };
        if stale {
            element.captured = None;
            element.pressed = false;
        }
    }
}

impl<State: 'static> View<State> for ChipView<State> {
    type Element = ChipWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> ChipWidget {
        let mut element = ChipWidget::new(self.label.clone(), Kind::Chip);
        self.sync(&mut element);
        element.leading = self.leading.as_ref().map(|v| build_child(v, ctx));
        element
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut ChipWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = ChangeFlags::NONE;
        if element.label.set_content(self.label.clone()) {
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        if prev.selected != self.selected {
            flags |= ChangeFlags::PAINT;
        }
        if prev.on_remove.is_some() != self.on_remove.is_some()
            || prev.on_press.is_some() != self.on_press.is_some()
        {
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        self.sync(element);
        match (&prev.leading, &self.leading, &mut element.leading) {
            (Some(p), Some(n), Some(pod)) => flags |= rebuild_child(p, n, pod, ctx),
            (Some(p), None, slot @ Some(_)) => {
                if let Some(pod) = slot {
                    teardown_child(p, pod, ctx);
                }
                *slot = None;
                flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
            }
            (None, Some(n), slot) => {
                *slot = Some(build_child(n, ctx));
                flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
            }
            _ => {}
        }
        flags
    }

    fn teardown(&self, element: &mut ChipWidget, ctx: &mut BuildCtx<'_>) {
        if let (Some(view), Some(pod)) = (&self.leading, &mut element.leading) {
            teardown_child(view, pod, ctx);
        }
    }
}

/// A chip carrying a [`StatusBadgeTone`]. See the [module docs](self).
pub struct StatusBadgeView {
    label: String,
    tone: StatusBadgeTone,
    dot: bool,
}

/// A status badge reading `label` in `tone`.
pub fn status_badge(label: impl Into<String>, tone: StatusBadgeTone) -> StatusBadgeView {
    StatusBadgeView {
        label: label.into(),
        tone,
        dot: false,
    }
}

impl StatusBadgeView {
    /// The tone this badge carries.
    pub fn tone(&self) -> StatusBadgeTone {
        self.tone
    }

    /// Show or hide a leading dot in the tone's ink (default hidden).
    pub fn dot(mut self, dot: bool) -> Self {
        self.dot = dot;
        self
    }
}

impl<State: 'static> View<State> for StatusBadgeView {
    type Element = ChipWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> ChipWidget {
        let mut element = ChipWidget::new(self.label.clone(), Kind::Badge(self.tone));
        element.show_dot = self.dot;
        element
    }

    fn rebuild(
        &self,
        _prev: &Self,
        element: &mut ChipWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = ChangeFlags::NONE;
        if element.label.set_content(self.label.clone()) {
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        if element.kind != Kind::Badge(self.tone) {
            element.kind = Kind::Badge(self.tone);
            flags |= ChangeFlags::PAINT;
        }
        if element.show_dot != self.dot {
            element.show_dot = self.dot;
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        flags
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::{Density, theme_with_density};
    use frust::Brightness;
    use frust::authoring::text::TextContext;
    use frust::authoring::{KeyEvent, Modifiers, PointerButton, PointerEvent};
    use std::any::Any;

    const TONES: [StatusBadgeTone; 5] = [
        StatusBadgeTone::Neutral,
        StatusBadgeTone::Info,
        StatusBadgeTone::Success,
        StatusBadgeTone::Warning,
        StatusBadgeTone::Danger,
    ];

    #[derive(Default)]
    struct Count {
        press: u32,
        remove: u32,
    }

    /// Build `view` headlessly and lay it out, returning the widget and its size.
    fn laid_out<V: View<()>>(view: &V) -> (V::Element, Size) {
        let mut next_id = 0u64;
        let mut widget = view.build(&mut BuildCtx::new(&mut next_id));
        let mut text_ctx = TextContext::new();
        let mut layout = LayoutCtx::with_text_context(&mut text_ctx as &mut dyn Any);
        let size = widget.layout(&mut layout, &BoxConstraints::loose(Size::new(400.0, 400.0)));
        (widget, size)
    }

    fn laid_out_in(view: &ChipView<Count>, theme: &Theme) -> (ChipWidget, Size) {
        let mut next_id = 0u64;
        let mut widget = View::<Count>::build(view, &mut BuildCtx::new(&mut next_id));
        let mut text_ctx = TextContext::new();
        let mut layout =
            LayoutCtx::with_resources(Some(&mut text_ctx as &mut dyn Any), Some(theme as &dyn Any));
        let size = widget.layout(&mut layout, &BoxConstraints::loose(Size::new(400.0, 400.0)));
        (widget, size)
    }

    fn ev(phase: PointerPhase, x: f64, y: f64) -> InputEvent {
        InputEvent::Pointer(PointerEvent {
            phase,
            position: Point::new(x, y),
            button: PointerButton::Primary,
        })
    }

    fn dispatch(w: &mut ChipWidget, state: &mut Count, size: Size, event: &InputEvent) {
        let any_state: &mut dyn Any = state;
        let mut ctx = EventCtx::new(any_state, Point::ZERO, size);
        w.event(&mut ctx, event);
    }

    fn tap(w: &mut ChipWidget, state: &mut Count, size: Size, x: f64, y: f64) {
        dispatch(w, state, size, &ev(PointerPhase::Down, x, y));
        dispatch(w, state, size, &ev(PointerPhase::Up, x, y));
    }

    #[test]
    fn the_chip_entry_points_build_and_lay_out() {
        let (c, size) = laid_out(&chip("Draft"));
        assert_eq!(c.lines(), vec!["Draft"]);
        assert!(size.width > 0.0);
        for tone in TONES {
            let badge = status_badge("Live", tone);
            assert_eq!(badge.tone(), tone);
            let (w, _) = laid_out(&badge);
            assert_eq!(w.lines(), vec!["Live"]);
        }
    }

    #[test]
    fn every_tone_builds_in_light_and_dark_with_a_fill_distinct_from_neutral() {
        for brightness in [Brightness::Light, Brightness::Dark] {
            let theme = Theme::builder(crate::theme())
                .brightness(brightness)
                .build();
            let neutral = tone_colors(StatusBadgeTone::Neutral, Some(&theme));
            for tone in TONES {
                let (w, size) = laid_out_in_badge(tone, &theme);
                assert!(size.width > 0.0 && size.height > 0.0, "{tone:?}");
                assert_eq!(w.lines(), vec!["Live"]);
                if tone != StatusBadgeTone::Neutral {
                    let c = tone_colors(tone, Some(&theme));
                    assert_ne!(c.fill, neutral.fill, "{tone:?} {brightness:?}");
                    assert_ne!(c.ink, neutral.ink, "{tone:?} {brightness:?}");
                }
            }
        }
    }

    fn laid_out_in_badge(tone: StatusBadgeTone, theme: &Theme) -> (ChipWidget, Size) {
        let view = status_badge("Live", tone).dot(true);
        let mut next_id = 0u64;
        let mut widget = View::<()>::build(&view, &mut BuildCtx::new(&mut next_id));
        let mut text_ctx = TextContext::new();
        let mut layout =
            LayoutCtx::with_resources(Some(&mut text_ctx as &mut dyn Any), Some(theme as &dyn Any));
        let size = widget.layout(&mut layout, &BoxConstraints::loose(Size::new(400.0, 400.0)));
        (widget, size)
    }

    #[test]
    fn a_pressable_chip_has_a_touch_sized_hit_area_around_a_smaller_pill() {
        let theme = theme_with_density(Density::Touch);
        let floor = BeuiMetrics::resolve(Some(&theme)).min_touch_target;
        assert!(floor >= 44.0);
        let (w, size) = laid_out_in(&chip::<Count>("Tag").on_press(|_| {}), &theme);
        assert!(size.width >= floor && size.height >= floor, "{size:?}");
        assert!(w.pill.height < floor);
        // An inert chip does not grow.
        let (_, inert) = laid_out_in(&chip::<Count>("Tag"), &theme);
        assert!(inert.height < floor);
    }

    #[test]
    fn a_press_in_the_enlarged_margin_fires_on_press() {
        let theme = theme_with_density(Density::Touch);
        let (mut w, size) = laid_out_in(&chip::<Count>("Tag").on_press(|c| c.press += 1), &theme);
        let mut state = Count::default();
        tap(&mut w, &mut state, size, size.width / 2.0, 1.0);
        assert_eq!(state.press, 1);
    }

    #[test]
    fn on_remove_fires_separately_from_on_press() {
        let view = chip::<Count>("Tag")
            .on_press(|c| c.press += 1)
            .on_remove(|c| c.remove += 1);
        let theme = theme_with_density(Density::Touch);
        let (mut w, size) = laid_out_in(&view, &theme);
        let mut state = Count::default();
        let y = size.height / 2.0;

        tap(&mut w, &mut state, size, size.width - 4.0, y);
        assert_eq!((state.press, state.remove), (0, 1));

        tap(&mut w, &mut state, size, size.width / 2.0 - 10.0, y);
        assert_eq!((state.press, state.remove), (1, 1));

        // Pressed in one zone and released in the other fires neither.
        dispatch(
            &mut w,
            &mut state,
            size,
            &ev(PointerPhase::Down, size.width - 4.0, y),
        );
        dispatch(&mut w, &mut state, size, &ev(PointerPhase::Up, 6.0, y));
        assert_eq!((state.press, state.remove), (1, 1));
    }

    #[test]
    fn a_remove_only_chip_ignores_a_body_press() {
        let (mut w, size) = laid_out_in(
            &chip::<Count>("Tag").on_remove(|c| c.remove += 1),
            &theme_with_density(Density::Comfortable),
        );
        let mut state = Count::default();
        tap(&mut w, &mut state, size, 8.0, size.height / 2.0);
        assert_eq!((state.press, state.remove), (0, 0));
        tap(
            &mut w,
            &mut state,
            size,
            size.width - 4.0,
            size.height / 2.0,
        );
        assert_eq!(state.remove, 1);
    }

    #[test]
    fn selection_recolours_the_chip() {
        let theme = crate::theme();
        assert_ne!(
            chip_colors(true, Some(&theme)),
            chip_colors(false, Some(&theme))
        );
    }

    // ---- Keyboard, semantics and rebuild ------------------------------------

    fn key(key: Key) -> InputEvent {
        InputEvent::Key(KeyEvent {
            key,
            modifiers: Modifiers::default(),
            repeat: false,
        })
    }

    /// A column of three chips under a real `RenderRoot`: pressable,
    /// handler-less, pressable.
    struct Harness {
        root: frust_core::RenderRoot<Count, frust::FlexView<Count>>,
        state: Count,
    }

    impl Harness {
        fn new() -> Self {
            let mut h = Harness {
                root: frust_core::RenderRoot::new(),
                state: Count::default(),
            };
            h.root.set_theme(Box::new(crate::theme()));
            let mut logic = |_s: &mut Count| {
                frust::column::<Count>()
                    .child(chip::<Count>("One").on_press(|c| c.press += 1))
                    .child(chip::<Count>("Inert"))
                    .child(
                        chip::<Count>("Two")
                            .on_press(|c| c.press += 10)
                            .on_remove(|c| c.remove += 1),
                    )
            };
            h.root.rebuild(&mut logic, &mut h.state);
            h.root.layout_with_text(
                Size::new(300.0, 300.0),
                &mut TextContext::new() as &mut dyn Any,
            );
            h
        }

        fn send(&mut self, k: Key) {
            self.root.event(&mut self.state, &key(k));
        }

        /// The label of the node semantics reports focused, if any.
        fn focused_label(&self) -> Option<String> {
            let update = self.root.semantics();
            let focus = update.focus?;
            update
                .nodes
                .iter()
                .find(|(id, _)| *id == focus)
                .and_then(|(_, n)| n.label().map(str::to_owned))
        }
    }

    #[test]
    fn tab_lands_on_a_pressable_chip_skips_inert_ones_and_moves_on() {
        let mut h = Harness::new();
        assert_eq!(h.focused_label(), None);
        h.send(Key::Named(NamedKey::Tab));
        assert_eq!(h.focused_label().as_deref(), Some("One"));
        h.send(Key::Named(NamedKey::Tab));
        assert_eq!(h.focused_label().as_deref(), Some("Two"));
    }

    #[test]
    fn enter_and_space_each_fire_on_press_once() {
        for k in [Key::Named(NamedKey::Enter), Key::Character(" ".into())] {
            let mut h = Harness::new();
            h.send(Key::Named(NamedKey::Tab));
            h.send(k);
            assert_eq!((h.state.press, h.state.remove), (1, 0));
        }
    }

    #[test]
    fn delete_and_backspace_fire_on_remove_and_not_on_press() {
        for k in [NamedKey::Delete, NamedKey::Backspace] {
            let mut h = Harness::new();
            h.send(Key::Named(NamedKey::Tab));
            h.send(Key::Named(NamedKey::Tab));
            h.send(Key::Named(k));
            assert_eq!((h.state.press, h.state.remove), (0, 1));
        }
    }

    #[test]
    fn a_chip_with_a_handler_is_focusable_and_one_without_is_not() {
        let (w, _) = laid_out(&chip::<()>("A"));
        assert!(!w.is_focusable());
        let (w, _) = laid_out(&chip::<()>("A").on_press(|_| {}));
        assert!(w.is_focusable());
        let (w, _) = laid_out(&chip::<()>("A").on_remove(|_| {}));
        assert!(w.is_focusable());
    }

    #[test]
    fn a_remove_only_chip_is_a_stop_and_focuses_its_remove_node() {
        let mut root = frust_core::RenderRoot::new();
        root.set_theme(Box::new(crate::theme()));
        let mut state = Count::default();
        let mut logic = |_s: &mut Count| {
            frust::column::<Count>().child(chip::<Count>("Tag").on_remove(|c| c.remove += 1))
        };
        root.rebuild(&mut logic, &mut state);
        root.layout_with_text(
            Size::new(300.0, 300.0),
            &mut TextContext::new() as &mut dyn Any,
        );
        root.event(&mut state, &key(Key::Named(NamedKey::Tab)));
        let update = root.semantics();
        let focus = update.focus.expect("focused");
        let (_, node) = update.nodes.iter().find(|(id, _)| *id == focus).unwrap();
        assert_eq!(node.label(), Some("Remove Tag"));
        // Enter has no body action on a remove-only chip.
        root.event(&mut state, &key(Key::Named(NamedKey::Enter)));
        assert_eq!(state.remove, 0);
        root.event(&mut state, &key(Key::Named(NamedKey::Delete)));
        assert_eq!(state.remove, 1);
    }

    fn actionable_buttons(view: ChipView<Count>) -> Vec<(String, bool)> {
        let mut root = frust_core::RenderRoot::new();
        root.set_theme(Box::new(crate::theme()));
        let mut state = Count::default();
        let mut logic = move |_s: &mut Count| ChipView {
            label: view.label.clone(),
            leading: None,
            selected: false,
            on_press: view.on_press.clone(),
            on_remove: view.on_remove.clone(),
        };
        root.rebuild(&mut logic, &mut state);
        root.layout_with_text(
            Size::new(300.0, 300.0),
            &mut TextContext::new() as &mut dyn Any,
        );
        root.semantics()
            .nodes
            .iter()
            .filter(|(_, n)| n.role() == Role::Button)
            .map(|(_, n)| {
                (
                    n.label().unwrap_or_default().to_owned(),
                    n.supports_action(Action::Click),
                )
            })
            .collect()
    }

    #[test]
    fn semantics_publishes_the_remove_node_and_the_body_action_separately() {
        let got = actionable_buttons(chip("Tag").on_remove(|_| {}));
        assert_eq!(got, vec![("Remove Tag".into(), true)]);
        let mut got = actionable_buttons(chip("Tag").on_press(|_| {}).on_remove(|_| {}));
        got.sort();
        assert_eq!(got, vec![("Remove Tag".into(), true), ("Tag".into(), true)]);
        let got = actionable_buttons(chip("Tag").on_press(|_| {}));
        assert_eq!(got, vec![("Tag".into(), true)]);
    }

    #[test]
    fn removing_on_press_during_a_hold_clears_the_pressed_state() {
        let theme = theme_with_density(Density::Comfortable);
        let held = chip::<Count>("Tag").on_press(|c| c.press += 1);
        let (mut w, size) = laid_out_in(&held, &theme);
        let mut state = Count::default();
        dispatch(&mut w, &mut state, size, &ev(PointerPhase::Down, 8.0, 8.0));
        assert!(w.pressed && w.captured.is_some());
        let next = chip::<Count>("Tag");
        let mut next_id = 0u64;
        View::<Count>::rebuild(&next, &held, &mut w, &mut BuildCtx::new(&mut next_id));
        assert!(!w.pressed && w.captured.is_none());
        dispatch(&mut w, &mut state, size, &ev(PointerPhase::Up, 8.0, 8.0));
        assert_eq!(state.press, 0);
    }
}
