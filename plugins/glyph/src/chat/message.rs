//! [`message_row`]: one transcript row for a message, in three variants.
//!
//! # Contract
//!
//! - **Controlled and non-reactive.** The caller owns the text and the
//!   thought's open state (`.thought_open(..)` and `.on_thought_toggle(..)`
//!   work like `crate::accordion`: a press is reported, never self-applied).
//! - **User.** The text as plain selectable text, never parsed as markdown,
//!   wrapped anywhere, in a tinted bubble on the trailing side, at most
//!   [`WIDE_FRACTION`] of a viewport of [`WIDE_VIEWPORT`] or more.
//! - **Assistant.** [`markdown`] at full width with no bubble;
//!   `.streaming(..)` is forwarded to it.
//! - **Thought.** One muted line while closed (a label, then the first line
//!   ellipsized) with a chevron; open, the text as dimmed markdown. The
//!   closed height never depends on the text, and the header is a live
//!   toggle only with `.on_thought_toggle(..)`.
//! - **Copy.** With `.on_copy(..)` a user or assistant row shows an always
//!   visible copy control: a press writes the clipboard with the source
//!   text, then calls back; while `.streaming(true)` it stays put but inert.
//! - **Rebuilds.** An equal view changes nothing; an unchanged row costs no
//!   re-parse and no re-shape when a later row streams.
//! - **Width.** The row is as wide as it is given, never wider;
//!   `.max_width(..)` narrows the content on a tablet.

use std::rc::Rc;

use frust::FrameTime;
use frust::accesskit::Role;
use frust::authoring::Brush;
use frust::authoring::text::{OverflowWrap, TextContext, TextLayout, TextOverflow, TextStyle};
use frust::authoring::{
    Action, AnyView, BezPath, BoxConstraints, BuildCtx, ChangeFlags, ChildPod, Color,
    ErasedArgCallback, ErasedCallback, EventCtx, EventResult, InputEvent, LayoutCtx, PaintCtx,
    PaintScene, Point, PointerPhase, Rect, SemanticsCtx, Size, ThemeTextColor, ThemeTextType,
    TypedArgCallback, View, Widget, any, build_child, erase_callback, erase_callback_arg,
    rebuild_child, route_event_single, teardown_child,
};
use frust::{Theme, rich_text, span};

use super::markdown::markdown;
use super::model::ChatRole;
use super::style;
use crate::press::presses;

/// Viewport width from which a user bubble is capped to a fraction.
const WIDE_VIEWPORT: f64 = 600.0;
/// The widest share of a wide viewport a user bubble takes.
const WIDE_FRACTION: f64 = 0.8;
/// Horizontal padding inside a user bubble.
const BUBBLE_PAD_X: f64 = 12.0;
/// Vertical padding inside a user bubble.
const BUBBLE_PAD_Y: f64 = 8.0;
/// Height of the copy control: a comfortable touch target.
const COPY_H: f64 = 28.0;
/// Shortest thought header, for the same reason.
const HEADER_MIN_H: f64 = 28.0;
/// Body text size: the Glyph `body` token (as in the markdown renderer).
const BODY_SIZE: f32 = style::BODY_FONT_SIZE;
/// Width of the thought chevron.
const CHEVRON_W: f64 = 5.0;
/// Height of the thought chevron.
const CHEVRON_H: f64 = 9.0;
/// Space between the chevron, the label and the summary.
const HEADER_GAP: f64 = style::BLOCK_GAP;
/// Alpha an open thought's body is composited at, to read as dimmer.
const THOUGHT_ALPHA: f32 = 0.72;

type OnLink<State> = Rc<dyn Fn(&mut State, &str)>;
type OnToggle<State> = Rc<dyn Fn(&mut State)>;

/// A declarative transcript row. See the [module docs](self).
pub struct MessageRowView<State: 'static> {
    role: ChatRole,
    text: String,
    streaming: bool,
    selectable: bool,
    thought_open: bool,
    max_width: Option<f64>,
    on_link: Option<OnLink<State>>,
    on_copy: Option<TypedArgCallback<State, String>>,
    on_toggle: Option<OnToggle<State>>,
}

impl<State: 'static> std::fmt::Debug for MessageRowView<State> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MessageRowView")
            .field("role", &self.role)
            .field("text", &self.text)
            .field("streaming", &self.streaming)
            .field("selectable", &self.selectable)
            .field("thought_open", &self.thought_open)
            .field("max_width", &self.max_width)
            .finish_non_exhaustive()
    }
}

/// One transcript row: `text` authored by `role`. The caller owns the text
/// and the thought's open state and passes them on every build.
pub fn message_row<State: 'static>(
    role: ChatRole,
    text: impl Into<String>,
) -> MessageRowView<State> {
    MessageRowView {
        role,
        text: text.into(),
        streaming: false,
        selectable: true,
        thought_open: false,
        max_width: None,
        on_link: None,
        on_copy: None,
        on_toggle: None,
    }
}

impl<State: 'static> MessageRowView<State> {
    /// Mark the text as still arriving (see the module docs).
    pub fn streaming(mut self, streaming: bool) -> Self {
        self.streaming = streaming;
        self
    }

    /// Run `on_link` with the app state and the exact URL of a safe link the
    /// user taps in assistant or thought markdown. Nothing is opened here; the
    /// URL is attacker-controlled and may differ from the visible text, so
    /// show or confirm the destination before opening it.
    pub fn on_link(mut self, on_link: impl Fn(&mut State, &str) + 'static) -> Self {
        self.on_link = Some(Rc::new(on_link));
        self
    }

    /// Show a copy control; a press writes the clipboard, then runs
    /// `on_copy` with the app state and the message's source text.
    pub fn on_copy(mut self, on_copy: impl Fn(&mut State, &str) + 'static) -> Self {
        let typed: TypedArgCallback<State, String> =
            Rc::new(move |state: &mut State, text: String| on_copy(state, &text));
        self.on_copy = Some(typed);
        self
    }

    /// Whether a thought row is open (default closed). Ignored by the other
    /// roles.
    pub fn thought_open(mut self, open: bool) -> Self {
        self.thought_open = open;
        self
    }

    /// Run `on_toggle` when the thought disclosure is pressed. The row never
    /// flips the open state itself.
    pub fn on_thought_toggle(mut self, on_toggle: impl Fn(&mut State) + 'static) -> Self {
        self.on_toggle = Some(Rc::new(on_toggle));
        self
    }

    /// Cap the content width, for tablet layouts.
    pub fn max_width(mut self, width: f64) -> Self {
        self.max_width = Some(width);
        self
    }

    /// Let the user select text (default `true`).
    pub fn selectable(mut self, selectable: bool) -> Self {
        self.selectable = selectable;
        self
    }

    /// The body view, if the role and state show one.
    fn body_view(&self) -> Option<AnyView<State>> {
        match self.role {
            ChatRole::User => Some(any(rich_text([span(self.text.as_str())
                .size(BODY_SIZE)
                .themed_family(ThemeTextType::BodyLarge)
                .themed_role(ThemeTextColor::OnSurface)])
            .overflow_wrap(OverflowWrap::Anywhere)
            .selectable(self.selectable))),
            ChatRole::Thought if !self.thought_open => None,
            _ => {
                let mut view = markdown::<State>(self.text.as_str())
                    .streaming(self.streaming)
                    .selectable(self.selectable);
                if let Some(on_link) = self.on_link.clone() {
                    view = view.on_link(move |state: &mut State, url: &str| on_link(state, url));
                }
                Some(any(view))
            }
        }
    }

    fn shows_copy(&self) -> bool {
        self.on_copy.is_some() && self.role != ChatRole::Thought
    }

    /// Everything but the callbacks, for the equal-view check.
    fn same_data(&self, other: &Self) -> bool {
        self.role == other.role
            && self.text == other.text
            && self.streaming == other.streaming
            && self.selectable == other.selectable
            && self.thought_open == other.thought_open
            && self.max_width == other.max_width
            && self.on_link.is_some() == other.on_link.is_some()
            && self.on_copy.is_some() == other.on_copy.is_some()
            && self.on_toggle.is_some() == other.on_toggle.is_some()
    }
}

/// What a press can land on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Copy,
    Toggle,
}

/// Shaped label text.
#[derive(Default)]
struct Labels {
    thought: Option<TextLayout>,
    summary: Option<TextLayout>,
    copy: Option<TextLayout>,
    copied: Option<TextLayout>,
    copy_off: Option<TextLayout>,
}

/// The retained widget for a [`MessageRowView`].
pub struct MessageRowWidget {
    role: ChatRole,
    text: String,
    streaming: bool,
    thought_open: bool,
    max_width: Option<f64>,
    shows_copy: bool,
    on_copy: Option<ErasedArgCallback<String>>,
    on_toggle: Option<ErasedCallback>,
    body: Option<ChildPod>,
    labels: Labels,
    label_h: f64,
    width: f64,
    bubble: Rect,
    header_h: f64,
    copy_rect: Rect,
    pressed: Option<Target>,
    captured: Option<Target>,
    copied: bool,
    copied_at: Option<FrameTime>,
}

impl MessageRowWidget {
    fn copy_live(&self) -> bool {
        self.shows_copy && self.on_copy.is_some() && !self.streaming
    }

    fn toggle_rect(&self) -> Rect {
        Rect::new(0.0, 0.0, self.content_w(), self.header_h)
    }

    fn content_w(&self) -> f64 {
        self.max_width
            .map_or(self.width, |m| m.min(self.width))
            .max(0.0)
    }

    fn hit(&self, p: Point) -> Option<Target> {
        if self.role == ChatRole::Thought {
            let live = self.on_toggle.is_some() && self.toggle_rect().contains(p);
            return live.then_some(Target::Toggle);
        }
        (self.copy_live() && self.copy_rect.contains(p)).then_some(Target::Copy)
    }

    fn fire(&mut self, ctx: &mut EventCtx, target: Target) {
        match target {
            Target::Copy => {
                ctx.write_clipboard(self.text.clone());
                if let Some(cb) = self.on_copy.as_mut() {
                    cb(ctx, self.text.clone());
                }
                self.copied = true;
                self.copied_at = None;
            }
            Target::Toggle => {
                if let Some(cb) = self.on_toggle.as_mut() {
                    cb(ctx);
                }
            }
        }
    }

    fn shape(&mut self, ctx: &mut LayoutCtx, content_w: f64) {
        let theme = Theme::from_layout_ctx(ctx);
        let family = style::mono_family(theme);
        let active = style::ink(theme);
        let done = style::success(theme);
        let muted = style::muted(theme);
        let mono =
            |color| TextStyle::new(style::LABEL_FONT_SIZE, color).with_family(family.clone());
        let text = ctx.text_context::<TextContext>();
        let mut labels = Labels::default();
        if self.role == ChatRole::Thought {
            let label = text.layout("Thought", &mono(muted), None);
            self.label_h = label.size().height;
            let used = Self::chevron_area() + label.size().width + HEADER_GAP;
            let room = content_w - used - style::PAD_SMALL;
            let first = first_line(&self.text);
            if !self.thought_open && !first.is_empty() && room > 1.0 {
                let summary_style = TextStyle::new(style::LABEL_FONT_SIZE, muted);
                labels.summary = Some(text.layout_bounded(
                    first,
                    &summary_style,
                    Some(room as f32),
                    Some(1),
                    TextOverflow::Ellipsis,
                ));
            }
            labels.thought = Some(label);
        }
        if self.shows_copy {
            let copy = text.layout(style::COPY_TEXT, &mono(active), None);
            self.label_h = copy.size().height;
            labels.copy = Some(copy);
            labels.copied = Some(text.layout(style::COPIED_TEXT, &mono(done), None));
            labels.copy_off = Some(text.layout(style::COPY_TEXT, &mono(muted), None));
        }
        self.labels = labels;
    }

    fn chevron_area() -> f64 {
        style::PAD_SMALL + CHEVRON_W + HEADER_GAP
    }

    /// The widest copy-control label, so "Copy" and "Copied" never move
    /// anything.
    fn copy_w(&self) -> f64 {
        [&self.labels.copy, &self.labels.copied]
            .into_iter()
            .flatten()
            .map(|l| l.size().width)
            .fold(0.0, f64::max)
    }

    /// The copy control, `top` below the row's top, `left` or trailing.
    fn place_copy(&mut self, top: f64, trailing: bool) -> f64 {
        if !self.shows_copy {
            self.copy_rect = Rect::ZERO;
            return 0.0;
        }
        let w = (self.copy_w() + style::PAD_SMALL * 2.0).min(self.width);
        let x = if trailing { self.width - w } else { 0.0 };
        let y = top + style::ROW_GAP;
        self.copy_rect = Rect::new(x, y, x + w, y + COPY_H);
        style::ROW_GAP + COPY_H
    }
}

impl<State: 'static> View<State> for MessageRowView<State> {
    type Element = MessageRowWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> MessageRowWidget {
        MessageRowWidget {
            role: self.role,
            text: self.text.clone(),
            streaming: self.streaming,
            thought_open: self.thought_open,
            max_width: self.max_width,
            shows_copy: self.shows_copy(),
            on_copy: self.on_copy.as_ref().map(erase_callback_arg),
            on_toggle: self.on_toggle.as_ref().map(erase_callback),
            body: self.body_view().map(|view| build_child(&view, ctx)),
            labels: Labels::default(),
            label_h: 0.0,
            width: 0.0,
            bubble: Rect::ZERO,
            header_h: 0.0,
            copy_rect: Rect::ZERO,
            pressed: None,
            captured: None,
            copied: false,
            copied_at: None,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut MessageRowWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        element.on_copy = self.on_copy.as_ref().map(erase_callback_arg);
        element.on_toggle = self.on_toggle.as_ref().map(erase_callback);
        let mut flags = ChangeFlags::NONE;
        let (before, after) = (prev.body_view(), self.body_view());
        let in_place = prev.role == self.role && before.is_some() && after.is_some();
        match (&before, &after, element.body.as_mut()) {
            (Some(old), Some(new), Some(pod)) if in_place => {
                flags |= rebuild_child(old, new, pod, ctx);
            }
            (None, None, _) => {}
            (old, _, pod) => {
                if let (Some(old), Some(pod)) = (old, pod) {
                    teardown_child(old, pod, ctx);
                }
                element.body = after.as_ref().map(|view| build_child(view, ctx));
                flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
            }
        }
        if !prev.same_data(self) {
            element.role = self.role;
            element.text.clone_from(&self.text);
            element.streaming = self.streaming;
            element.thought_open = self.thought_open;
            element.max_width = self.max_width;
            element.shows_copy = self.shows_copy();
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        flags
    }

    fn teardown(&self, element: &mut MessageRowWidget, ctx: &mut BuildCtx<'_>) {
        if let (Some(view), Some(pod)) = (self.body_view(), element.body.as_mut()) {
            teardown_child(&view, pod, ctx);
        }
    }
}

/// The first non-blank line of `text`, trimmed.
fn first_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
}

impl Widget for MessageRowWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        self.width = style::width_of(bc);
        let content_w = self.content_w();
        self.shape(ctx, content_w);
        self.header_h = 0.0;
        self.bubble = Rect::ZERO;
        let loose = |w: f64| BoxConstraints::loose(Size::new(w.max(0.0), f64::INFINITY));
        let mut height;
        match self.role {
            ChatRole::User => {
                let cap = if self.max_width.is_none() && self.width >= WIDE_VIEWPORT {
                    self.width * WIDE_FRACTION
                } else {
                    content_w
                };
                let inner = loose(cap - BUBBLE_PAD_X * 2.0);
                let body = self.body.as_mut().map(|pod| pod.layout_child(ctx, &inner));
                let body = body.unwrap_or(Size::ZERO);
                let w = (body.width + BUBBLE_PAD_X * 2.0).min(self.width);
                let h = body.height + BUBBLE_PAD_Y * 2.0;
                let x = self.width - w;
                self.bubble = Rect::new(x, 0.0, x + w, h);
                if let Some(pod) = self.body.as_mut() {
                    pod.set_origin(Point::new(x + BUBBLE_PAD_X, BUBBLE_PAD_Y));
                }
                height = h;
                height += self.place_copy(h, true);
            }
            ChatRole::Assistant => {
                let body = self
                    .body
                    .as_mut()
                    .map(|pod| pod.layout_child(ctx, &loose(content_w)));
                if let Some(pod) = self.body.as_mut() {
                    pod.set_origin(Point::ORIGIN);
                }
                height = body.map_or(0.0, |s| s.height);
                height += self.place_copy(height, false);
            }
            _ => {
                self.header_h = (self.label_h + style::PAD_SMALL * 2.0).max(HEADER_MIN_H);
                height = self.header_h;
                if let Some(pod) = self.body.as_mut() {
                    let size = pod.layout_child(ctx, &loose(content_w));
                    pod.set_origin(Point::new(0.0, height + style::ROW_GAP));
                    height += style::ROW_GAP + size.height;
                }
            }
        }
        bc.constrain(Size::new(self.width, height))
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        if self.copied {
            let now = ctx.frame_time();
            let since = *self.copied_at.get_or_insert(now);
            if now.saturating_sub(since) >= style::COPIED_FOR {
                self.copied = false;
                self.copied_at = None;
            } else {
                ctx.request_frame();
            }
        }
        let theme = Theme::from_paint_ctx(ctx);
        let o = ctx.origin();
        let centered = |layout: &TextLayout, rect: Rect| {
            let size = layout.size();
            Point::new(
                o.x + rect.x0 + (rect.width() - size.width) / 2.0,
                o.y + rect.y0 + (rect.height() - size.height) / 2.0,
            )
        };
        match self.role {
            ChatRole::User => {
                let tint = style::surface_high(theme);
                let b = self.bubble;
                scene.fill_rounded_rect(
                    Point::new(o.x + b.x0, o.y + b.y0),
                    Size::new(b.width(), b.height()),
                    style::RADIUS,
                    tint,
                );
                if let Some(pod) = self.body.as_mut() {
                    pod.paint_child(ctx, scene);
                }
            }
            ChatRole::Assistant => {
                if let Some(pod) = self.body.as_mut() {
                    pod.paint_child(ctx, scene);
                }
            }
            _ => {
                let muted = style::muted(theme);
                self.paint_thought(ctx, scene, muted);
            }
        }
        if self.shows_copy {
            let label = if !self.copy_live() && !self.copied {
                &self.labels.copy_off
            } else if self.copied {
                &self.labels.copied
            } else {
                &self.labels.copy
            };
            if let Some(layout) = label {
                for run in layout.to_scene_runs(centered(layout, self.copy_rect)) {
                    scene.draw_glyph_run(run);
                }
            }
        }
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        let to_body = |body: &mut Option<ChildPod>, ctx: &mut EventCtx| match body {
            Some(pod) => route_event_single(pod, ctx, event),
            None => EventResult::Ignored,
        };
        let InputEvent::Pointer(p) = event else {
            return to_body(&mut self.body, ctx);
        };
        match p.phase {
            PointerPhase::Down => {
                if presses(p)
                    && let Some(target) = self.hit(p.position)
                {
                    self.captured = Some(target);
                    self.pressed = Some(target);
                    ctx.capture_pointer();
                    ctx.request_redraw();
                    return EventResult::Handled;
                }
                to_body(&mut self.body, ctx)
            }
            PointerPhase::Move => {
                let Some(target) = self.captured else {
                    return to_body(&mut self.body, ctx);
                };
                let inside = (self.hit(p.position) == Some(target)).then_some(target);
                if inside != self.pressed {
                    self.pressed = inside;
                    ctx.request_redraw();
                }
                EventResult::Handled
            }
            PointerPhase::Up => {
                let Some(target) = self.captured.take() else {
                    return to_body(&mut self.body, ctx);
                };
                self.pressed = None;
                if self.hit(p.position) == Some(target) {
                    self.fire(ctx, target);
                }
                ctx.request_redraw();
                EventResult::Handled
            }
            PointerPhase::Cancel => {
                if self.captured.take().is_some() {
                    self.pressed = None;
                    ctx.request_redraw();
                    return EventResult::Handled;
                }
                to_body(&mut self.body, ctx)
            }
        }
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        let label = match self.role {
            ChatRole::User => "User message",
            ChatRole::Assistant => "Assistant message",
            _ => "Thought",
        };
        ctx.push_container(
            Role::Group,
            |node| node.set_label(label),
            |ctx| {
                let origin = ctx.origin();
                let bounds = |r: Rect| style::node_bounds(origin, r);
                if self.role == ChatRole::Thought {
                    ctx.push_node(Role::Button, |node| {
                        node.set_label("Thought");
                        node.set_expanded(self.thought_open);
                        if self.on_toggle.is_some() {
                            node.add_action(Action::Click);
                        } else {
                            node.set_disabled();
                        }
                        node.set_bounds(bounds(self.toggle_rect()));
                    });
                }
                if let Some(pod) = &self.body {
                    pod.semantics_child(ctx);
                }
                if self.shows_copy {
                    ctx.push_node(Role::Button, |node| {
                        node.set_label("Copy message");
                        if self.copy_live() {
                            node.add_action(Action::Click);
                        } else {
                            node.set_disabled();
                        }
                        node.set_bounds(bounds(self.copy_rect));
                    });
                }
            },
        );
    }

    frust::authoring::visit_children!(body);
}

impl MessageRowWidget {
    fn paint_thought(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene, muted: Color) {
        let o = ctx.origin();
        // A chevron drawn as a path: right when closed, down when open.
        let (cx, cy) = (style::PAD_SMALL, self.header_h / 2.0);
        let mut chevron = BezPath::new();
        if self.thought_open {
            chevron.move_to(Point::new(0.0, -CHEVRON_W / 2.0));
            chevron.line_to(Point::new(CHEVRON_H / 2.0, CHEVRON_W / 2.0));
            chevron.line_to(Point::new(CHEVRON_H, -CHEVRON_W / 2.0));
        } else {
            chevron.move_to(Point::new(0.0, -CHEVRON_H / 2.0));
            chevron.line_to(Point::new(CHEVRON_W, 0.0));
            chevron.line_to(Point::new(0.0, CHEVRON_H / 2.0));
        }
        let at = Point::new(o.x + cx, o.y + cy);
        scene.stroke_path(
            at,
            &chevron,
            style::BORDER_WIDTH * 1.5,
            &Brush::Solid(muted),
        );
        let mut x = o.x + Self::chevron_area();
        let put = |layout: &TextLayout, x: f64, scene: &mut dyn PaintScene| {
            let y = o.y + (self.header_h - layout.size().height) / 2.0;
            for run in layout.to_scene_runs(Point::new(x, y)) {
                scene.draw_glyph_run(run);
            }
        };
        if let Some(label) = &self.labels.thought {
            put(label, x, scene);
            x += label.size().width + HEADER_GAP;
        }
        if let Some(summary) = &self.labels.summary {
            put(summary, x, scene);
        }
        if let Some(pod) = self.body.as_mut() {
            let top = o.y + self.header_h;
            let size = Size::new(
                ctx.size().width,
                (ctx.size().height - self.header_h).max(0.0),
            );
            scene.push_layer(Point::new(o.x, top), size, THOUGHT_ALPHA);
            pod.paint_child(ctx, scene);
            scene.pop_layer();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use frust::authoring::scene::GlyphRun;
    use frust::authoring::{DiscardScene, PointerButton, PointerEvent};
    use frust_core::{InspectDetail, RenderRoot};
    use std::any::Any;

    #[derive(Default)]
    struct App {
        copied: Vec<String>,
        toggles: usize,
        links: Vec<String>,
    }

    type Root = RenderRoot<App, AnyView<App>>;

    struct Rig {
        root: Root,
        app: App,
        text: TextContext,
        window: Size,
    }

    impl Rig {
        fn new(themed: bool, width: f64) -> Self {
            let mut root: Root = RenderRoot::new();
            if themed {
                root.set_theme(Box::new(crate::baseline()));
            }
            Rig {
                root,
                app: App::default(),
                text: TextContext::new(),
                window: Size::new(width, 4000.0),
            }
        }

        fn show(&mut self, mut view: impl FnMut(&mut App) -> MessageRowView<App>) {
            self.root
                .rebuild(&mut |s: &mut App| any(view(s)), &mut self.app);
            self.root
                .layout_with_text(self.window, &mut self.text as &mut dyn Any);
        }

        fn row(&self) -> Rect {
            self.root.inspect()[0].bounds
        }

        fn rects(&self) -> Vec<(String, Rect)> {
            self.root
                .inspect()
                .into_iter()
                .filter_map(|n| match n.detail {
                    Some(InspectDetail::Text(t)) if n.type_name.ends_with("RichTextWidget") => {
                        Some((t.text, n.bounds))
                    }
                    _ => None,
                })
                .collect()
        }

        fn click(&mut self, at: Point) {
            for phase in [PointerPhase::Down, PointerPhase::Up] {
                let e = InputEvent::Pointer(PointerEvent {
                    phase,
                    position: at,
                    button: PointerButton::Primary,
                });
                self.root.event(&mut self.app, &e);
            }
        }

        fn stats(&self) -> u64 {
            let s = self.text.shape_cache_stats();
            s.shapes + s.line_breaks
        }
    }

    const ROLES: [ChatRole; 3] = [ChatRole::User, ChatRole::Assistant, ChatRole::Thought];

    fn long_text() -> String {
        format!(
            "first line of the message\n\nA second paragraph with **bold** text.\n\n{}",
            "x".repeat(400)
        )
    }

    #[test]
    fn every_variant_lays_out_within_phone_and_tablet_widths() {
        let text = long_text();
        for themed in [true, false] {
            for width in [360.0, 1024.0] {
                for role in ROLES {
                    for open in [false, true] {
                        let mut rig = Rig::new(themed, width);
                        rig.show(|_| {
                            message_row(role, text.as_str())
                                .thought_open(open)
                                .on_copy(|_: &mut App, _| {})
                        });
                        let row = rig.row();
                        assert!(
                            row.x1 <= width + 0.01 && row.height() > 0.0,
                            "{role:?} {width} {row:?}"
                        );
                        for (t, r) in rig.rects() {
                            assert!(
                                r.x0 >= -0.01 && r.x1 <= width + 0.01,
                                "{role:?} {width} {t:?} at {r:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_long_unbroken_string_wraps_in_a_user_bubble() {
        let mut rig = Rig::new(true, 360.0);
        rig.show(|_| message_row(ChatRole::User, "y".repeat(500)));
        let (_, rect) = rig.rects().remove(0);
        assert!(rect.width() <= 360.0 - BUBBLE_PAD_X * 2.0 + 0.01);
        assert!(rect.height() > 60.0, "wrapped: {}", rect.height());
    }

    #[test]
    fn user_text_is_plain_and_the_bubble_sits_on_the_trailing_side() {
        let mut rig = Rig::new(true, 1024.0);
        rig.show(|_| message_row(ChatRole::User, "a **not bold** line"));
        let texts = rig.rects();
        assert_eq!(texts[0].0, "a **not bold** line");
        assert!(
            texts[0].1.x1 > 1024.0 - BUBBLE_PAD_X - 1.0,
            "{:?}",
            texts[0].1
        );
        let long = "word ".repeat(200);
        rig.show(|_| message_row(ChatRole::User, long.as_str()));
        let body = rig.rects()[0].1;
        assert!(
            body.width() <= 1024.0 * WIDE_FRACTION - BUBBLE_PAD_X * 2.0 + 0.01,
            "{}",
            body.width()
        );
        rig.show(|_| message_row(ChatRole::User, long.as_str()).max_width(400.0));
        assert!(rig.rects()[0].1.width() <= 400.0 - BUBBLE_PAD_X * 2.0 + 0.01);
    }

    #[test]
    fn an_assistant_row_is_markdown_without_a_bubble() {
        let mut rig = Rig::new(true, 400.0);
        rig.show(|_| message_row(ChatRole::Assistant, "# Title\n\nbody"));
        let texts = rig.rects();
        assert_eq!(texts.len(), 2);
        assert!(texts[0].1.x0 < 0.01);
    }

    #[test]
    fn a_closed_thought_is_one_short_line_and_open_is_taller() {
        let text = long_text();
        let height = |open: bool| {
            let mut rig = Rig::new(true, 360.0);
            rig.show(|_| message_row(ChatRole::Thought, text.as_str()).thought_open(open));
            rig.row().height()
        };
        let (closed, open) = (height(false), height(true));
        assert!(closed <= HEADER_MIN_H + 4.0, "closed {closed}");
        assert!(open > closed + 40.0, "open {open}");
        let mut rig = Rig::new(true, 360.0);
        rig.show(|_| message_row(ChatRole::Thought, text.as_str()));
        assert!(rig.rects().is_empty(), "a closed thought builds no body");
    }

    #[test]
    fn a_streaming_collapsed_thought_keeps_its_height() {
        let source = long_text();
        let mut rig = Rig::new(true, 360.0);
        let mut heights = Vec::new();
        let mut at = 0;
        while at < source.len() {
            at = (at + 23).min(source.len());
            let prefix = &source[..at];
            rig.show(|_| message_row(ChatRole::Thought, prefix).streaming(true));
            heights.push(rig.row().height());
        }
        rig.show(|_| message_row(ChatRole::Thought, ""));
        heights.push(rig.row().height());
        assert!(heights.windows(2).all(|w| w[0] == w[1]), "{heights:?}");
    }

    #[test]
    fn the_thought_disclosure_reports_and_exposes_its_state() {
        let mut rig = Rig::new(true, 360.0);
        for open in [false, true] {
            rig.show(|_| {
                message_row(ChatRole::Thought, "idea")
                    .thought_open(open)
                    .on_thought_toggle(|app: &mut App| app.toggles += 1)
            });
            let row = rig.row();
            rig.click(Point::new(row.x0 + 30.0, row.y0 + 10.0));
            let update = rig.root.semantics();
            assert!(
                update
                    .nodes
                    .iter()
                    .any(|(_, n)| { n.role() == Role::Button && n.is_expanded() == Some(open) })
            );
        }
        assert_eq!(rig.app.toggles, 2);
        let update = rig.root.semantics();
        assert!(
            update
                .nodes
                .iter()
                .any(|(_, n)| { n.role() == Role::Group && n.label() == Some("Thought") })
        );
    }

    #[test]
    fn copy_writes_the_clipboard_and_reports_the_source_text() {
        for role in [ChatRole::User, ChatRole::Assistant] {
            let source = "some *source* text";
            let mut rig = Rig::new(true, 400.0);
            rig.show(|_| {
                message_row(role, source).on_copy(|app: &mut App, t| app.copied.push(t.into()))
            });
            let row = rig.row();
            let (x, y) = if role == ChatRole::User {
                (row.x1 - 20.0, row.y1 - 8.0)
            } else {
                (row.x0 + 20.0, row.y1 - 8.0)
            };
            rig.click(Point::new(x, y));
            assert_eq!(rig.app.copied, [source.to_owned()], "{role:?}");
            assert_eq!(rig.root.take_clipboard_write().as_deref(), Some(source));
            let update = rig.root.semantics();
            assert!(
                update
                    .nodes
                    .iter()
                    .any(|(_, n)| n.label() == Some("Copy message"))
            );
        }
    }

    #[test]
    fn inert_controls_take_no_press_and_report_no_click() {
        let button = |rig: &Rig, label: &str| {
            let update = rig.root.semantics();
            let (_, n) = update
                .nodes
                .iter()
                .find(|(_, n)| n.role() == Role::Button && n.label() == Some(label))
                .unwrap_or_else(|| panic!("no {label} button"));
            (
                n.supports_action(Action::Click),
                n.is_disabled(),
                n.bounds(),
            )
        };
        let mut rig = Rig::new(true, 400.0);
        rig.show(|_| message_row(ChatRole::Thought, "thinking"));
        let (click, disabled, bounds) = button(&rig, "Thought");
        assert_eq!((click, disabled), (false, true), "no toggle, no click");
        let b = bounds.expect("bounds");
        let header = Point::new((b.x0 + b.x1) / 2.0, (b.y0 + b.y1) / 2.0);
        rig.click(header);
        rig.show(|_| {
            message_row(ChatRole::Thought, "thinking")
                .on_thought_toggle(|app: &mut App| app.toggles += 1)
        });
        assert!(button(&rig, "Thought").0, "a toggle makes the header live");
        rig.click(header);
        assert_eq!(rig.app.toggles, 1, "the header fires once it is live");

        for (streaming, live) in [(true, false), (false, true)] {
            rig.show(|_| {
                message_row(ChatRole::Assistant, "hi")
                    .streaming(streaming)
                    .on_copy(|_: &mut App, _| {})
            });
            let (click, disabled, _) = button(&rig, "Copy message");
            assert_eq!((click, disabled), (live, !live), "streaming {streaming}");
        }
    }

    #[test]
    fn no_copy_control_without_a_callback_and_none_on_a_thought() {
        let mut rig = Rig::new(true, 400.0);
        rig.show(|_| message_row(ChatRole::Assistant, "x"));
        let bare = rig.row().height();
        let update = rig.root.semantics();
        assert!(
            !update
                .nodes
                .iter()
                .any(|(_, n)| n.label() == Some("Copy message"))
        );
        rig.show(|_| message_row(ChatRole::Assistant, "x").on_copy(|_: &mut App, _| {}));
        assert!(rig.row().height() > bare);
        rig.show(|_| message_row(ChatRole::Thought, "x").on_copy(|_: &mut App, _| {}));
        let update = rig.root.semantics();
        assert!(
            !update
                .nodes
                .iter()
                .any(|(_, n)| n.label() == Some("Copy message"))
        );
    }

    #[test]
    fn a_streaming_row_ignores_the_copy_control_without_moving() {
        let mut rig = Rig::new(true, 400.0);
        let view = |streaming: bool| {
            move |_: &mut App| {
                message_row(ChatRole::Assistant, "x")
                    .streaming(streaming)
                    .on_copy(|app: &mut App, t| app.copied.push(t.into()))
            }
        };
        rig.show(view(true));
        let row = rig.row();
        rig.click(Point::new(row.x0 + 20.0, row.y1 - 8.0));
        assert!(rig.app.copied.is_empty());
        assert_eq!(rig.root.take_clipboard_write(), None);
    }

    #[test]
    fn a_link_in_assistant_markdown_reaches_on_link() {
        let mut rig = Rig::new(true, 400.0);
        rig.show(|_| {
            message_row(ChatRole::Assistant, "[docs](https://x.example/) tail")
                .on_link(|app: &mut App, url| app.links.push(url.into()))
        });
        let rect = rig.rects()[0].1;
        rig.click(Point::new(rect.x0 + 6.0, rect.y0 + 6.0));
        assert_eq!(rig.app.links, ["https://x.example/".to_owned()]);
    }

    #[test]
    fn equal_views_skip_the_rebuild() {
        let make = |role| message_row::<()>(role, long_text()).on_copy(|_: &mut (), _| {});
        for role in ROLES {
            let (a, b) = (make(role), make(role));
            let mut counter = 0u64;
            let mut ctx = BuildCtx::new(&mut counter);
            let mut widget = View::<()>::build(&a, &mut ctx);
            let flags = View::<()>::rebuild(&b, &a, &mut widget, &mut ctx);
            assert_eq!(flags, ChangeFlags::NONE, "{role:?}");
            let edited = message_row::<()>(role, "other").on_copy(|_: &mut (), _| {});
            let flags = View::<()>::rebuild(&edited, &b, &mut widget, &mut ctx);
            assert!(flags.contains(ChangeFlags::LAYOUT), "{role:?}");
        }
    }

    #[test]
    fn an_unchanged_row_is_not_reshaped_when_another_streams() {
        let mut rig = Rig::new(true, 360.0);
        let first = long_text();
        let show = |rig: &mut Rig, tail: &str| {
            rig.show(|_| message_row(ChatRole::Assistant, first.as_str()));
            let _ = tail;
        };
        show(&mut rig, "a");
        let before = rig.stats();
        show(&mut rig, "ab");
        assert_eq!(rig.stats(), before);
    }

    #[test]
    fn opening_and_closing_a_thought_rebuilds_the_body() {
        let mut rig = Rig::new(true, 360.0);
        for open in [true, false, true] {
            rig.show(|_| message_row(ChatRole::Thought, "a\n\nb").thought_open(open));
            assert_eq!(rig.rects().len(), if open { 2 } else { 0 }, "{open}");
        }
    }

    #[test]
    fn a_role_change_replaces_the_body() {
        let mut rig = Rig::new(true, 360.0);
        rig.show(|_| message_row(ChatRole::User, "**x**"));
        assert_eq!(rig.rects()[0].0, "**x**");
        rig.show(|_| message_row(ChatRole::Assistant, "**x**"));
        assert_eq!(rig.rects()[0].0, "x");
    }

    #[derive(Default)]
    struct Count(usize);

    impl PaintScene for Count {
        fn fill_rect(&mut self, _o: Point, _s: Size, _c: Color) {}
        fn draw_text(&mut self, _o: Point, _t: &str) {}
        fn draw_glyph_run(&mut self, _run: GlyphRun) {
            self.0 += 1;
        }
    }

    #[test]
    fn themed_and_unthemed_paint_draw_without_panicking() {
        let text = long_text();
        for themed in [true, false] {
            for role in ROLES {
                for open in [false, true] {
                    let mut rig = Rig::new(themed, 360.0);
                    rig.show(|_| {
                        message_row(role, text.as_str())
                            .thought_open(open)
                            .streaming(open)
                            .on_copy(|_: &mut App, _| {})
                    });
                    let mut scene = Count::default();
                    rig.root.paint(&mut scene, FrameTime::ZERO);
                    assert!(scene.0 > 0, "{role:?} themed {themed}");
                    rig.root.paint(&mut DiscardScene, FrameTime::ZERO);
                }
            }
        }
    }
}
