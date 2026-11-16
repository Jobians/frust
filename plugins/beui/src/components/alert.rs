//! Alerts: an inline [`alert`] banner in an [`AlertTone`].
//!
//! The banner a phone app shows for a notice or error: a rounded surface tinted by
//! its tone, a leading icon, a title, an optional message, trailing actions and an
//! optional dismiss control. [`banner`](AlertView::banner) makes it full-bleed with
//! square corners, for the top of a page.
//!
//! # Tone, tint and icon
//!
//! | Tone | Tint role | Icon |
//! |------|-----------|------|
//! | [`Info`](AlertTone::Info) | the scheme's `primary_container`, which beUI fills with its cyan accent | `INFO` |
//! | [`Success`](AlertTone::Success) | [`BeuiTokens::success`] | `CHECK` |
//! | [`Warning`](AlertTone::Warning) | [`BeuiTokens::warning`] | `WARNING` |
//! | [`Danger`](AlertTone::Danger) | the scheme's `error` | `WARNING` |
//!
//! beUI authors no info hue, so Info reuses the accent role and the palette gains no
//! field. Danger takes `WARNING` rather than `SHIELD`: the sign reads as "something
//! went wrong" on a failed save as well as on a security notice, and it keeps Danger
//! visibly distinct from Info's circle. The icon is the baseline icon from the
//! facade, drawn in the theme's ink: a view has no theme at build time, so the tone
//! reads through the tinted surface and border, and
//! [`icon`](AlertView::icon) swaps in any view.
//!
//! # Layout
//!
//! The icon sits at the leading edge, the dismiss control at the trailing edge, and
//! the text between them wraps to what is left. Actions sit trailing beside the text
//! when they fit without wrapping it, and wrap onto rows below it otherwise. Under a
//! touch density the dismiss control's hit area grows to the minimum touch target.
//!
//! # Semantics
//!
//! The surface is a `Role::Alert` labelled with the title and message; the dismiss
//! control is a `Role::Button` labelled "Dismiss".

use std::rc::Rc;

use frust::Theme;
use frust::authoring::text::{TextContext, TextLayout, TextStyle};
use frust::authoring::{
    Action, AnyView, BezPath, BoxConstraints, Brush, BuildCtx, ChangeFlags, ChildPod, Color,
    ErasedCallback, EventCtx, EventResult, InputEvent, LayoutCtx, PaintCtx, PaintScene, Point,
    PointerPhase, Role, SemanticsCtx, Size, View, ViewSeq, Widget, any, build_child,
    erase_callback, rebuild_child, rebuild_children, route_event, teardown_child,
};
use frust::icons;

use crate::press::{inside, presses, stroke_outline};
use crate::style::{HOVER_WASH_ALPHA, with_alpha};
use crate::text::{ThemeTextType, label_style, themed_style};
use crate::tokens::{BeuiMetrics, BeuiTokens, color_scheme_light};

/// Horizontal padding of a surface.
const PAD_X: f64 = 14.0;
/// Vertical padding of a surface.
const PAD_Y: f64 = 12.0;
/// Gap between the icon and the text, and between the text and inline actions.
const GAP: f64 = 12.0;
/// Gap between actions.
const ACTION_GAP: f64 = 8.0;
/// Gap between the title and the message.
const LINE_GAP: f64 = 2.0;
/// Space kept between the dismiss control and the trailing edge.
const DISMISS_INSET: f64 = 4.0;
/// The dismiss control's side before the touch floor applies.
const DISMISS_SIDE: f64 = 32.0;
/// Edge of the dismiss cross.
const DISMISS_GLYPH: f64 = 12.0;
/// Stroke of the dismiss cross.
const DISMISS_STROKE: f64 = 1.75;
/// The icon edge the default icons are built at.
const ICON_EDGE: f64 = 20.0;
/// Width an unbounded parent gets.
const DEFAULT_WIDTH: f64 = 320.0;
/// Alpha of the tone wash over the parent surface.
const TINT_ALPHA: f32 = 0.12;
/// Alpha of the tone border.
const BORDER_ALPHA: f32 = 0.40;

/// How an [`alert`] reads at a glance.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AlertTone {
    /// Informational.
    #[default]
    Info,
    /// Completed or healthy.
    Success,
    /// Needs attention.
    Warning,
    /// Failed or destructive.
    Danger,
}

impl AlertTone {
    /// The hue the surface tint and border derive from.
    fn hue(self, theme: Option<&Theme>) -> Color {
        let tokens = BeuiTokens::resolve(theme);
        let scheme = |pick: fn(&frust::ColorScheme) -> Color| {
            theme.map_or_else(|| pick(&color_scheme_light()), |t| pick(t.scheme()))
        };
        match self {
            AlertTone::Info => scheme(|s| s.primary_container),
            AlertTone::Success => tokens.success,
            AlertTone::Warning => tokens.warning,
            AlertTone::Danger => scheme(|s| s.error),
        }
    }

    /// The default leading icon.
    fn default_icon(self) -> frust::IconView {
        let source = match self {
            AlertTone::Info => icons::INFO,
            AlertTone::Success => icons::CHECK,
            AlertTone::Warning | AlertTone::Danger => icons::WARNING,
        };
        frust::icon(source).size(ICON_EDGE)
    }
}

// ---- Dismiss control -------------------------------------------------------

/// The close affordance: a cross in a square hit area.
struct DismissView<State: 'static> {
    on_dismiss: Rc<dyn Fn(&mut State)>,
}

/// The retained widget behind [`DismissView`].
pub struct DismissWidget {
    on_dismiss: ErasedCallback,
    pressed: bool,
    captured: bool,
    side: f64,
}

impl<State: 'static> View<State> for DismissView<State> {
    type Element = DismissWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> DismissWidget {
        DismissWidget {
            on_dismiss: erase_callback(&self.on_dismiss),
            pressed: false,
            captured: false,
            side: DISMISS_SIDE,
        }
    }

    fn rebuild(
        &self,
        _prev: &Self,
        element: &mut DismissWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        element.on_dismiss = erase_callback(&self.on_dismiss);
        ChangeFlags::NONE
    }
}

impl Widget for DismissWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let m = BeuiMetrics::resolve(Theme::from_layout_ctx(ctx));
        self.side = DISMISS_SIDE.max(m.min_touch_target);
        bc.constrain(Size::new(self.side, self.side))
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let scheme = Theme::from_paint_ctx(ctx).map(|t| *t.scheme());
        let (ink, strong) = scheme.map_or_else(
            || {
                let s = color_scheme_light();
                (s.on_surface_variant, s.on_surface)
            },
            |s| (s.on_surface_variant, s.on_surface),
        );
        let origin = ctx.origin();
        if self.pressed {
            scene.fill_rounded_rect(
                origin,
                Size::new(self.side, self.side),
                self.side / 2.0,
                with_alpha(strong, HOVER_WASH_ALPHA * 2.0),
            );
        }
        let centre = Point::new(origin.x + self.side / 2.0, origin.y + self.side / 2.0);
        let half = DISMISS_GLYPH / 2.0;
        let mut path = BezPath::new();
        path.move_to(Point::new(centre.x - half, centre.y - half));
        path.line_to(Point::new(centre.x + half, centre.y + half));
        path.move_to(Point::new(centre.x + half, centre.y - half));
        path.line_to(Point::new(centre.x - half, centre.y + half));
        let colour = if self.pressed { strong } else { ink };
        scene.stroke_path(Point::ORIGIN, &path, DISMISS_STROKE, &Brush::Solid(colour));
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        let InputEvent::Pointer(p) = event else {
            return EventResult::Ignored;
        };
        let size = ctx.size();
        match p.phase {
            PointerPhase::Down => {
                if !presses(p) || !inside(p.position, size) {
                    return EventResult::Ignored;
                }
                self.pressed = true;
                self.captured = true;
                ctx.capture_pointer();
                ctx.request_redraw();
                EventResult::Handled
            }
            PointerPhase::Move => {
                if !self.captured {
                    return EventResult::Ignored;
                }
                self.pressed = inside(p.position, size);
                ctx.request_redraw();
                EventResult::Handled
            }
            PointerPhase::Up => {
                if !self.captured {
                    return EventResult::Ignored;
                }
                if inside(p.position, size) {
                    (self.on_dismiss)(ctx);
                }
                self.pressed = false;
                self.captured = false;
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

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        ctx.push_node(Role::Button, |node| {
            node.set_label("Dismiss");
            node.add_action(Action::Click);
        });
    }
}

// ---- The alert -------------------------------------------------------------

/// The retained widget behind the alert.
///
/// `pods` holds the leading icon, then the dismiss control when there is one,
/// then the actions.
pub struct AlertWidget {
    tone: AlertTone,
    banner: bool,
    title: String,
    message: Option<String>,
    pods: Vec<ChildPod>,
    dismissible: bool,

    title_layout: Option<TextLayout>,
    message_layout: Option<TextLayout>,
    title_at: Point,
    message_at: Point,
    size: Size,
}

impl AlertWidget {
    /// The text lines this widget carries, top to bottom.
    pub fn lines(&self) -> Vec<&str> {
        let mut lines = vec![self.title.as_str()];
        lines.extend(self.message.as_deref());
        lines
    }

    /// The accessible label: the title, then the message.
    pub fn label(&self) -> String {
        match &self.message {
            Some(message) => format!("{}. {message}", self.title),
            None => self.title.clone(),
        }
    }

    /// How many pods precede the actions.
    fn head(&self) -> usize {
        1 + usize::from(self.dismissible)
    }
}

/// An inline alert banner. See the [module docs](self).
pub struct AlertView<State: 'static = ()> {
    tone: AlertTone,
    title: String,
    message: Option<String>,
    banner: bool,
    icon: AnyView<State>,
    dismiss: Option<AnyView<State>>,
    actions: Vec<AnyView<State>>,
}

/// An alert in `tone` headed by `title`.
pub fn alert<State: 'static>(tone: AlertTone, title: impl Into<String>) -> AlertView<State> {
    AlertView {
        tone,
        title: title.into(),
        message: None,
        banner: false,
        icon: any(tone.default_icon()),
        dismiss: None,
        actions: Vec::new(),
    }
}

impl<State: 'static> AlertView<State> {
    /// The tone this alert carries.
    pub fn tone(&self) -> AlertTone {
        self.tone
    }

    /// Body text under the title.
    pub fn message(mut self, text: impl Into<String>) -> Self {
        self.message = Some(text.into());
        self
    }

    /// Trailing actions: beside the text when they fit, on rows below it otherwise.
    pub fn actions<M>(mut self, actions: impl ViewSeq<State, M>) -> Self {
        self.actions.clear();
        actions.extend_views(&mut self.actions);
        self
    }

    /// Show a close control that runs `on_dismiss`.
    pub fn on_dismiss(mut self, on_dismiss: impl Fn(&mut State) + 'static) -> Self {
        let on_dismiss: Rc<dyn Fn(&mut State)> = Rc::new(on_dismiss);
        self.dismiss = Some(any(DismissView { on_dismiss }));
        self
    }

    /// Replace the tone's default leading icon.
    pub fn icon(mut self, icon: impl View<State>) -> Self {
        self.icon = any(icon);
        self
    }

    /// Full-bleed with square corners and no side borders, for the top of a page.
    pub fn banner(mut self, banner: bool) -> Self {
        self.banner = banner;
        self
    }
}

impl<State: 'static> View<State> for AlertView<State> {
    type Element = AlertWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> AlertWidget {
        let mut pods = vec![build_child(&self.icon, ctx)];
        if let Some(dismiss) = &self.dismiss {
            pods.push(build_child(dismiss, ctx));
        }
        pods.extend(self.actions.iter().map(|view| build_child(view, ctx)));
        AlertWidget {
            tone: self.tone,
            banner: self.banner,
            title: self.title.clone(),
            message: self.message.clone(),
            pods,
            dismissible: self.dismiss.is_some(),
            title_layout: None,
            message_layout: None,
            title_at: Point::ORIGIN,
            message_at: Point::ORIGIN,
            size: Size::ZERO,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut AlertWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = ChangeFlags::NONE;
        if element.tone != self.tone {
            element.tone = self.tone;
            flags |= ChangeFlags::PAINT;
        }
        if element.banner != self.banner {
            element.banner = self.banner;
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        if element.title != self.title {
            element.title.clone_from(&self.title);
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        if element.message != self.message {
            element.message.clone_from(&self.message);
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }

        flags |= rebuild_child(&prev.icon, &self.icon, &mut element.pods[0], ctx);
        match (prev.dismiss.as_ref(), self.dismiss.as_ref()) {
            (Some(was), Some(now)) => {
                flags |= rebuild_child(was, now, &mut element.pods[1], ctx);
            }
            (None, Some(now)) => {
                element.pods.insert(1, build_child(now, ctx));
                flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
            }
            (Some(was), None) => {
                let mut pod = element.pods.remove(1);
                teardown_child(was, &mut pod, ctx);
                flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
            }
            (None, None) => {}
        }
        element.dismissible = self.dismiss.is_some();

        let head = element.head();
        let mut tail = element.pods.split_off(head);
        flags |= rebuild_children(
            &prev.actions,
            &self.actions,
            &mut tail,
            ctx,
            |view| view,
            |_| None,
        );
        element.pods.extend(tail);
        flags
    }

    fn teardown(&self, element: &mut AlertWidget, ctx: &mut BuildCtx<'_>) {
        let mut pods = element.pods.iter_mut();
        if let Some(pod) = pods.next() {
            teardown_child(&self.icon, pod, ctx);
        }
        if let Some(dismiss) = &self.dismiss
            && let Some(pod) = pods.next()
        {
            teardown_child(dismiss, pod, ctx);
        }
        for (view, pod) in self.actions.iter().zip(pods) {
            teardown_child(view, pod, ctx);
        }
    }
}

/// Shape `text` in `style`, wrapped to `width` when given.
fn shape(ctx: &mut LayoutCtx, text: &str, style: &TextStyle, width: Option<f64>) -> TextLayout {
    ctx.text_context::<TextContext>()
        .layout(text, style, width.map(|w| w.max(1.0) as f32))
}

impl Widget for AlertWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let (title_style, message_style) = {
            let theme = Theme::from_layout_ctx(ctx);
            let m = BeuiMetrics::resolve(theme);
            let title = themed_style(
                label_style(m.text_base).with_weight(frust::authoring::text::FontWeight::SEMI_BOLD),
                ThemeTextType::BodyMedium,
                theme,
            );
            let message = themed_style(
                label_style(m.text_sm).with_weight(frust::authoring::text::FontWeight::REGULAR),
                ThemeTextType::BodyMedium,
                theme,
            );
            (title, message)
        };

        let width = if bc.max().width.is_finite() {
            bc.max().width
        } else {
            DEFAULT_WIDTH
        };
        let pad_x = if self.banner { PAD_X + 2.0 } else { PAD_X };

        // The leading icon, the trailing dismiss control and the actions.
        let loose = BoxConstraints::loose(Size::new(width, f64::INFINITY));
        let icon = self.pods[0].layout_child(ctx, &loose);
        let dismiss_size = if self.dismissible {
            Some(self.pods[1].layout_child(ctx, &loose))
        } else {
            None
        };
        let head = self.head();
        let action_sizes: Vec<Size> = self.pods[head..]
            .iter_mut()
            .map(|pod| pod.layout_child(ctx, &loose))
            .collect();

        let text_x = pad_x + icon.width + GAP;
        let trailing = dismiss_size.map_or(pad_x, |d| d.width + DISMISS_INSET);
        let text_full = (width - text_x - trailing).max(1.0);

        // Natural text width decides whether actions fit beside it.
        let message = self.message.clone();
        let title_natural = shape(ctx, &self.title, &title_style, None).size().width;
        let message_natural = message
            .as_deref()
            .map_or(0.0, |t| shape(ctx, t, &message_style, None).size().width);

        let actions_total = action_sizes.iter().map(|s| s.width).sum::<f64>()
            + ACTION_GAP * action_sizes.len().saturating_sub(1) as f64;
        let inline = !action_sizes.is_empty()
            && title_natural.max(message_natural) + GAP + actions_total <= text_full;
        let text_w = if inline {
            text_full - actions_total - GAP
        } else {
            text_full
        };

        let title_layout = shape(ctx, &self.title, &title_style, Some(text_w));
        let title_h = title_layout.size().height;
        let message_layout = message
            .as_deref()
            .map(|t| shape(ctx, t, &message_style, Some(text_w)));
        let message_h = message_layout.as_ref().map_or(0.0, |l| l.size().height);
        let text_h = title_h
            + if message_layout.is_some() {
                LINE_GAP + message_h
            } else {
                0.0
            };

        self.title_at = Point::new(text_x, PAD_Y);
        self.message_at = Point::new(text_x, PAD_Y + title_h + LINE_GAP);
        self.title_layout = Some(title_layout);
        self.message_layout = message_layout;

        let mut body_bottom = PAD_Y + text_h;
        let first_line = title_h;
        // The icon rides the first text line.
        self.pods[0].set_origin(Point::new(
            pad_x,
            PAD_Y + ((first_line - icon.height) / 2.0).max(0.0),
        ));

        if !action_sizes.is_empty() {
            if inline {
                let mut x = text_x + text_w + GAP;
                for (pod, size) in self.pods[head..].iter_mut().zip(&action_sizes) {
                    pod.set_origin(Point::new(x, PAD_Y));
                    x += size.width + ACTION_GAP;
                }
                let tallest = action_sizes.iter().map(|s| s.height).fold(0.0, f64::max);
                body_bottom = body_bottom.max(PAD_Y + tallest);
            } else {
                // Wrap onto rows below the text, each row trailing.
                let edge = text_x + text_full;
                let mut rows: Vec<(usize, usize, f64, f64)> = Vec::new();
                let mut start = 0;
                let mut row_w = 0.0;
                let mut row_h = 0.0_f64;
                for (i, size) in action_sizes.iter().enumerate() {
                    let gap = if i > start { ACTION_GAP } else { 0.0 };
                    if i > start && row_w + gap + size.width > text_full {
                        rows.push((start, i, row_w, row_h));
                        start = i;
                        row_w = 0.0;
                        row_h = 0.0;
                    }
                    let gap = if i > start { ACTION_GAP } else { 0.0 };
                    row_w += gap + size.width;
                    row_h = row_h.max(size.height);
                }
                rows.push((start, action_sizes.len(), row_w, row_h));
                let mut y = body_bottom + ACTION_GAP;
                for (from, to, w, h) in rows {
                    let mut x = edge - w;
                    for (pod, size) in self.pods[head + from..head + to]
                        .iter_mut()
                        .zip(&action_sizes[from..to])
                    {
                        pod.set_origin(Point::new(x, y));
                        x += size.width + ACTION_GAP;
                    }
                    y += h + ACTION_GAP;
                }
                body_bottom = y - ACTION_GAP;
            }
        }

        let mut height = body_bottom + PAD_Y;
        if let Some(d) = dismiss_size {
            // Centred on the first line's band, the control may stand taller
            // than the text.
            let band = first_line + 2.0 * PAD_Y;
            let y = ((band - d.height) / 2.0).max(0.0);
            self.pods[1].set_origin(Point::new(width - d.width - DISMISS_INSET, y));
            height = height.max(y + d.height);
        }

        self.size = bc.constrain(Size::new(width, height));
        self.size
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let theme = Theme::from_paint_ctx(ctx);
        let hue = self.tone.hue(theme);
        let (ink, soft) = theme.map_or_else(
            || {
                let s = color_scheme_light();
                (s.on_surface, s.on_surface_variant)
            },
            |t| (t.scheme().on_surface, t.scheme().on_surface_variant),
        );
        let m = BeuiMetrics::resolve(theme);
        let origin = ctx.origin();
        let size = self.size;
        let border = with_alpha(hue, BORDER_ALPHA);
        if self.banner {
            scene.fill_rounded_rect(origin, size, 0.0, with_alpha(hue, TINT_ALPHA));
            scene.fill_rounded_rect(
                Point::new(origin.x, origin.y + size.height - 1.0),
                Size::new(size.width, 1.0),
                0.0,
                border,
            );
        } else {
            scene.fill_rounded_rect(origin, size, m.radius_list, with_alpha(hue, TINT_ALPHA));
            stroke_outline(scene, origin, size, m.radius_list, border);
        }

        for pod in &mut self.pods {
            pod.paint_child(ctx, scene);
        }

        for (layout, at, colour) in [
            (&self.title_layout, self.title_at, ink),
            (&self.message_layout, self.message_at, soft),
        ] {
            if let Some(layout) = layout {
                for mut run in layout.to_scene_runs(Point::new(origin.x + at.x, origin.y + at.y)) {
                    run.brush = Brush::Solid(colour);
                    scene.draw_glyph_run(run);
                }
            }
        }
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        // The icon is decorative; the dismiss control and actions take events.
        route_event(&mut self.pods[1..], ctx, event)
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        let label = self.label();
        ctx.push_container(
            Role::Alert,
            |node| node.set_label(label),
            |ctx| {
                for pod in &self.pods {
                    pod.semantics_child(ctx);
                }
            },
        );
    }

    frust::authoring::visit_children!(pods);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::{Density, theme_with_density};
    use frust::FrameTime;
    use frust::authoring::scene::GlyphRun;
    use frust::authoring::{PointerButton, PointerEvent};
    use std::any::Any;

    #[derive(Default)]
    struct Counter {
        dismissed: u32,
    }

    fn build(view: &AlertView<Counter>) -> AlertWidget {
        let mut next_id = 0u64;
        View::<Counter>::build(view, &mut BuildCtx::new(&mut next_id))
    }

    fn lay(w: &mut AlertWidget, theme: Option<&Theme>, width: f64) -> Size {
        let mut text_ctx = TextContext::new();
        let mut ctx = LayoutCtx::with_resources(
            Some(&mut text_ctx as &mut dyn Any),
            theme.map(|t| t as &dyn Any),
        );
        w.layout(&mut ctx, &BoxConstraints::loose(Size::new(width, 1000.0)))
    }

    #[derive(Default)]
    struct Recorder {
        fills: Vec<(Point, Size, f64, Color)>,
        glyphs: usize,
    }

    impl PaintScene for Recorder {
        fn fill_rect(&mut self, _o: Point, _s: Size, _c: Color) {}
        fn draw_text(&mut self, _o: Point, _t: &str) {}
        fn fill_rounded_rect(&mut self, o: Point, s: Size, radius: f64, color: Color) {
            self.fills.push((o, s, radius, color));
        }
        fn draw_glyph_run(&mut self, _run: GlyphRun) {
            self.glyphs += 1;
        }
    }

    fn paint(w: &mut AlertWidget, theme: &Theme) -> Recorder {
        let mut ctx =
            PaintCtx::for_test(Point::ORIGIN, w.size, FrameTime::from_nanos(0)).with_theme(theme);
        let mut rec = Recorder::default();
        w.paint(&mut ctx, &mut rec);
        rec
    }

    #[test]
    fn the_alert_builds_in_every_tone() {
        for tone in [
            AlertTone::Info,
            AlertTone::Success,
            AlertTone::Warning,
            AlertTone::Danger,
        ] {
            let view = alert::<Counter>(tone, "Heads up");
            assert_eq!(view.tone(), tone);
            let mut w = build(&view);
            let size = lay(&mut w, None, 400.0);
            assert_eq!(w.lines(), vec!["Heads up"]);
            assert!(size.width > 0.0 && size.height > 0.0);
        }
    }

    #[test]
    fn each_tone_tints_distinctly_in_light_and_dark() {
        for brightness in [frust::Brightness::Light, frust::Brightness::Dark] {
            let theme = Theme::builder(crate::theme())
                .brightness(brightness)
                .build();
            let fills: Vec<Color> = [
                AlertTone::Info,
                AlertTone::Success,
                AlertTone::Warning,
                AlertTone::Danger,
            ]
            .into_iter()
            .map(|tone| {
                let mut w = build(&alert::<Counter>(tone, "Heads up").message("Body"));
                lay(&mut w, Some(&theme), 320.0);
                let rec = paint(&mut w, &theme);
                assert!(rec.glyphs > 0, "{tone:?} paints its text");
                rec.fills[0].3
            })
            .collect();
            for (i, a) in fills.iter().enumerate() {
                for b in &fills[i + 1..] {
                    assert_ne!(a, b, "{brightness:?}");
                }
            }
        }
    }

    #[test]
    fn a_banner_is_square_and_a_card_is_rounded() {
        let theme = crate::theme();
        let mut card = build(&alert::<Counter>(AlertTone::Info, "Hi"));
        lay(&mut card, Some(&theme), 300.0);
        assert!(paint(&mut card, &theme).fills[0].2 > 0.0);
        let mut banner = build(&alert::<Counter>(AlertTone::Info, "Hi").banner(true));
        lay(&mut banner, Some(&theme), 300.0);
        assert_eq!(paint(&mut banner, &theme).fills[0].2, 0.0);
    }

    fn click(w: &mut AlertWidget, state: &mut Counter, size: Size, at: Point) {
        for phase in [PointerPhase::Down, PointerPhase::Up] {
            let event = InputEvent::Pointer(PointerEvent {
                phase,
                position: at,
                button: PointerButton::Primary,
            });
            let any_state: &mut dyn Any = state;
            let mut ctx = EventCtx::new(any_state, Point::ZERO, size);
            w.event(&mut ctx, &event);
        }
    }

    #[test]
    fn dismiss_fires_its_handler_and_clears_the_touch_minimum() {
        let theme = theme_with_density(Density::Touch);
        let floor = BeuiMetrics::resolve(Some(&theme)).min_touch_target;
        let mut w =
            build(&alert::<Counter>(AlertTone::Danger, "Failed").on_dismiss(|c| c.dismissed += 1));
        let size = lay(&mut w, Some(&theme), 320.0);
        let pod = &w.pods[1];
        assert!(pod.size().width >= floor && pod.size().height >= floor);
        let at = pod.origin() + frust::authoring::Vec2::new(3.0, 3.0);
        let mut state = Counter::default();
        click(&mut w, &mut state, size, at);
        assert_eq!(state.dismissed, 1);
        // A press away from the control does nothing.
        click(&mut w, &mut state, size, Point::new(40.0, 20.0));
        assert_eq!(state.dismissed, 1);
    }

    #[test]
    fn the_message_wraps_inside_the_surface_at_a_narrow_width() {
        let long = "This message is long enough that it cannot stay on one line at this width.";
        let mut wide = build(&alert::<Counter>(AlertTone::Warning, "Careful").message(long));
        let wide_size = lay(&mut wide, None, 600.0);
        let mut narrow = build(
            &alert::<Counter>(AlertTone::Warning, "Careful")
                .message(long)
                .on_dismiss(|_| {}),
        );
        let narrow_size = lay(&mut narrow, None, 200.0);
        assert!(narrow_size.width <= 200.0);
        assert!(
            narrow_size.height > wide_size.height,
            "wrapped onto more lines"
        );
        let message = narrow.message_layout.as_ref().unwrap();
        let right = narrow.message_at.x + message.size().width;
        assert!(right <= narrow_size.width, "text stays inside the surface");
        assert!(message.size().width < 200.0);
    }

    #[test]
    fn actions_sit_beside_wide_text_and_wrap_below_on_narrow_widths() {
        let build_with = || {
            build(&alert::<Counter>(AlertTone::Info, "Update ready").actions((
                crate::components::button::button::<Counter>("Later", |_| {}),
                crate::components::button::button::<Counter>("Install now", |_| {}),
            )))
        };
        let mut wide = build_with();
        lay(&mut wide, None, 600.0);
        let beside = wide.pods[1].origin();
        assert!(beside.x > wide.title_at.x, "inline, trailing the text");
        assert_eq!(beside.y, wide.title_at.y);

        let mut narrow = build_with();
        let size = lay(&mut narrow, None, 200.0);
        let below = narrow.pods[1].origin();
        let title_h = narrow.title_layout.as_ref().unwrap().size().height;
        assert!(
            below.y >= narrow.title_at.y + title_h,
            "wrapped below the text"
        );
        for pod in &narrow.pods[1..] {
            assert!(pod.origin().x + pod.size().width <= size.width);
        }
    }

    #[test]
    fn the_semantics_label_contains_the_title_and_message() {
        let w = build(&alert::<Counter>(AlertTone::Success, "Saved").message("All changes synced"));
        assert!(w.label().contains("Saved"));
        assert!(w.label().contains("All changes synced"));
        let bare = build(&alert::<Counter>(AlertTone::Success, "Saved"));
        assert_eq!(bare.label(), "Saved");
    }

    #[test]
    fn rebuilding_adds_and_removes_the_dismiss_control() {
        let plain = alert::<Counter>(AlertTone::Info, "Hi");
        let dismissible = alert::<Counter>(AlertTone::Info, "Hi").on_dismiss(|_| {});
        let mut w = build(&plain);
        assert_eq!(w.pods.len(), 1);
        let mut next_id = 0u64;
        View::<Counter>::rebuild(
            &dismissible,
            &plain,
            &mut w,
            &mut BuildCtx::new(&mut next_id),
        );
        assert_eq!(w.pods.len(), 2);
        assert!(w.dismissible);
        View::<Counter>::rebuild(
            &plain,
            &dismissible,
            &mut w,
            &mut BuildCtx::new(&mut next_id),
        );
        assert_eq!(w.pods.len(), 1);
        assert!(!w.dismissible);
    }
}
