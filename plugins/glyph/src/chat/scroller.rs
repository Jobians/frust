//! [`message_scroller`]: the transcript container. A virtualized list of rows
//! of varying, changing height that stays on the live edge, lets go when the
//! user scrolls away, and offers a jump-to-latest control to come back.
//!
//! # Contract
//!
//! - **Built on the keyed list.** The rows are a keyed, variable-extent
//!   `ListView` with `stick_to_end(true)`: only the materialized window is
//!   ever built, and every offset, drag, wheel and fling is the list's. This
//!   module adds no scrolling of its own.
//! - **Controlled and non-reactive.** `.on_at_end_change(..)` reports the
//!   list's stuck flag once per transition (never the initial state); the
//!   caller stores it and passes it back through `.at_end(..)`, which is what
//!   shows the jump control. `.unread(n)` puts a count on the control.
//! - **Jump control.** A floating Glyph pill above the bottom padding, shown
//!   while not at the end. A tap runs `animate_to_end` on the controller
//!   (`.controller(..)`, else one made here), which re-sticks the list. It
//!   enters and leaves with the Glyph slide; hidden, it takes no input.
//! - **Spacing.** The gap is the top inset of every row but the first, and the
//!   padding the first row's top and the last row's bottom inset, so neither
//!   changes with a row's height and a row's rebuild touches no other row.
//! - **Semantics.** A `Log` region over the list; the control is a button
//!   whose label carries the unread count.

use std::rc::Rc;

use frust::FrameTime;
use frust::authoring::text::{TextContext, TextLayout, TextStyle};
use frust::authoring::{
    Action, AnyView, BoxConstraints, BuildCtx, ChangeFlags, ChildPod, Color, EventCtx, EventResult,
    InputEvent, LayoutCtx, Live, PaintCtx, PaintScene, Point, PointerPhase, Rect, Role,
    SemanticsCtx, Size, View, Widget, any, build_child, rebuild_child, route_event_single,
    teardown_child,
};
use frust::motion::patterns::{PatternLayer, TransitionPattern};
use frust::{
    AnimateTo, ChildKey, EdgeInsets, ListView, MotionScheme, Padding, PaddingView,
    ScrollController, Theme,
};

use super::style;
use crate::motion::{GLYPH_SLIDE_ENTER, GLYPH_SLIDE_EXIT, GlyphSlide, SlideDirection};
use crate::press::presses;

/// The assumed height of a row not yet laid out, until
/// [`MessageScrollerView::estimated_row_extent`] names another.
pub const DEFAULT_ESTIMATED_ROW_EXTENT: f64 = 72.0;

/// Height of the jump control.
const JUMP_HEIGHT: f64 = 32.0;
/// The arrow on the jump control (vouched for by `style::kind_glyph`).
const JUMP_ARROW: char = '\u{2193}';
/// Accessible name of the transcript region.
const LOG_LABEL: &str = "Messages";
/// Accessible name of the jump control, before any unread count.
const JUMP_LABEL: &str = "Jump to latest";

type OnAtEndChange<State> = Rc<dyn Fn(&mut State, bool)>;
type RowBuilder<State> = Rc<dyn Fn(usize, EdgeInsets) -> PaddingView<State>>;

/// A declarative transcript scroller. See the [module docs](self).
pub struct MessageScrollerView<State: 'static> {
    len: usize,
    key_of: Rc<dyn Fn(usize) -> ChildKey>,
    rows: RowBuilder<State>,
    estimate: f64,
    controller: Option<ScrollController>,
    at_end: bool,
    on_at_end_change: Option<OnAtEndChange<State>>,
    unread: usize,
    row_gap: f64,
    padding: EdgeInsets,
    empty: Option<AnyView<State>>,
}

impl<State: 'static> std::fmt::Debug for MessageScrollerView<State> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MessageScrollerView")
            .field("len", &self.len)
            .field("estimate", &self.estimate)
            .field("at_end", &self.at_end)
            .field("unread", &self.unread)
            .field("row_gap", &self.row_gap)
            .field("padding", &self.padding)
            .finish_non_exhaustive()
    }
}

/// A transcript of `len` rows: `key_fn(i)` is row `i`'s stable identity and
/// `row_builder(i)` its view, the same pair `ListView::builder_keyed` takes.
/// Both are pure functions of the index, called only for materialized rows.
/// The caller owns the transcript and the at-end flag (see the module docs).
pub fn message_scroller<State: 'static, V: View<State>>(
    len: usize,
    key_fn: impl Fn(usize) -> ChildKey + 'static,
    row_builder: impl Fn(usize) -> V + 'static,
) -> MessageScrollerView<State> {
    MessageScrollerView {
        len,
        key_of: Rc::new(key_fn),
        rows: Rc::new(move |index, insets| Padding(insets, row_builder(index))),
        estimate: DEFAULT_ESTIMATED_ROW_EXTENT,
        controller: None,
        at_end: true,
        on_at_end_change: None,
        unread: 0,
        row_gap: style::BLOCK_GAP,
        padding: EdgeInsets::all(0.0),
        empty: None,
    }
}

impl<State: 'static> MessageScrollerView<State> {
    /// Drive (and read) the list through `controller`; without one the
    /// scroller keeps its own.
    pub fn controller(mut self, controller: ScrollController) -> Self {
        self.controller = Some(controller);
        self
    }

    /// The caller's copy of the list's at-end flag (default `true`); `false`
    /// shows the jump control.
    pub fn at_end(mut self, at_end: bool) -> Self {
        self.at_end = at_end;
        self
    }

    /// Run `callback` with the new flag each time the list sticks to its end
    /// (`true`) or the user scrolls it away (`false`).
    pub fn on_at_end_change(mut self, callback: impl Fn(&mut State, bool) + 'static) -> Self {
        self.on_at_end_change = Some(Rc::new(callback));
        self
    }

    /// The count shown on the jump control; zero shows the arrow alone.
    pub fn unread(mut self, unread: usize) -> Self {
        self.unread = unread;
        self
    }

    /// Space between consecutive rows, in logical px.
    pub fn row_gap(mut self, gap: f64) -> Self {
        self.row_gap = gap.max(0.0);
        self
    }

    /// Space around the rows inside the scrolled content; the bottom inset is
    /// what lets the last row clear a composer laid over the scroller.
    pub fn padding(mut self, padding: EdgeInsets) -> Self {
        self.padding = padding;
        self
    }

    /// Shown in place of the rows while `len` is zero.
    pub fn empty(mut self, view: impl View<State>) -> Self {
        self.empty = Some(any(view));
        self
    }

    /// The assumed height of a row not yet laid out (default
    /// [`DEFAULT_ESTIMATED_ROW_EXTENT`]). Panics at build if not positive,
    /// as `ListView::estimated_item_extent` does.
    pub fn estimated_row_extent(mut self, px: f64) -> Self {
        self.estimate = px;
        self
    }

    fn shows_empty(&self) -> bool {
        self.len == 0 && self.empty.is_some()
    }

    /// The list this view stands for, driven through `controller`.
    fn list(&self, controller: &ScrollController) -> ListView<State> {
        let (len, gap, pad) = (self.len, self.row_gap, self.padding);
        let key_of = Rc::clone(&self.key_of);
        let rows = Rc::clone(&self.rows);
        let list = ListView::builder_keyed(
            len,
            self.estimate,
            move |index| key_of(index),
            move |index| rows(index, row_insets(index, len, gap, pad)),
        )
        .estimated_item_extent(self.estimate)
        .stick_to_end(true)
        .controller(controller.clone());
        match &self.on_at_end_change {
            Some(callback) => {
                let callback = Rc::clone(callback);
                list.on_stick_change(move |state: &mut State, at_end| callback(state, at_end))
            }
            None => list,
        }
    }
}

/// Row `index`'s insets: the gap above every row but the first, the padding
/// above the first and below the last, the side padding on all.
fn row_insets(index: usize, len: usize, gap: f64, pad: EdgeInsets) -> EdgeInsets {
    EdgeInsets {
        left: pad.left,
        right: pad.right,
        top: if index == 0 { pad.top } else { gap },
        bottom: if index + 1 == len { pad.bottom } else { 0.0 },
    }
}

/// The text on the jump control.
fn jump_text(unread: usize) -> String {
    match unread {
        0 => JUMP_ARROW.to_string(),
        n => format!("{JUMP_ARROW} {n} new"),
    }
}

/// The jump control's accessible name.
fn jump_label(unread: usize) -> String {
    match unread {
        0 => JUMP_LABEL.to_owned(),
        n => format!("{JUMP_LABEL}, {n} unread"),
    }
}

/// Whether the jump control shows, and where its enter or exit slide is.
struct Presence {
    shown: bool,
    moving: bool,
    since: Option<FrameTime>,
}

impl Presence {
    /// Show or hide; `true` when that is a change.
    fn set(&mut self, shown: bool) -> bool {
        if self.shown == shown {
            return false;
        }
        self.shown = shown;
        self.moving = true;
        self.since = None;
        true
    }

    /// The layer to paint the control with at `now`, or `None` when hidden.
    fn layer(&mut self, now: FrameTime, reduce_motion: bool) -> Option<PatternLayer> {
        if self.moving && !reduce_motion {
            let since = *self.since.get_or_insert(now);
            let elapsed = now.saturating_sub(since);
            let span = if self.shown {
                GLYPH_SLIDE_ENTER
            } else {
                GLYPH_SLIDE_EXIT
            };
            if elapsed < span {
                // GlyphSlide maps both layers onto the enter timeline; its exit ends at 150 of 340 ms.
                let p = elapsed.as_secs_f64() / GLYPH_SLIDE_ENTER.as_secs_f64();
                return Some(if self.shown {
                    GlyphSlide::new(SlideDirection::Up)
                        .resolve(p, false, Size::ZERO)
                        .0
                } else {
                    GlyphSlide::new(SlideDirection::Down)
                        .resolve(p, false, Size::ZERO)
                        .1
                });
            }
        }
        self.moving = false;
        self.since = None;
        self.shown.then_some(PatternLayer::IDENTITY)
    }
}

/// The retained widget for a [`MessageScrollerView`].
pub struct MessageScrollerWidget {
    list: ChildPod,
    empty: Option<ChildPod>,
    /// The controller made here, used while the view names none.
    own_controller: ScrollController,
    /// The controller the list is attached to now.
    controller: ScrollController,
    unread: usize,
    padding: EdgeInsets,
    label: Option<TextLayout>,
    jump_rect: Rect,
    presence: Presence,
    pressed: bool,
    captured: bool,
    motion: MotionScheme,
}

impl MessageScrollerWidget {
    fn hits_jump(&self, p: Point) -> bool {
        self.presence.shown && self.jump_rect.contains(p)
    }

    fn jump(&self) {
        self.controller.animate_to_end(AnimateTo {
            duration_ms: self.motion.durations.slow,
            curve: self.motion.easing.spatial,
        });
    }

    /// Route a non-control event to the empty view, then the list.
    fn route(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        if let Some(empty) = self.empty.as_mut() {
            let result = route_event_single(empty, ctx, event);
            if matches!(result, EventResult::Handled) {
                return result;
            }
        }
        route_event_single(&mut self.list, ctx, event)
    }

    fn paint_jump(
        &self,
        scene: &mut dyn PaintScene,
        origin: Point,
        layer: PatternLayer,
        colors: JumpColors,
    ) {
        let size = self.jump_rect.size();
        let at = Point::new(
            origin.x + self.jump_rect.x0 + layer.dx,
            origin.y + self.jump_rect.y0 + layer.dy,
        );
        let radius = size.height / 2.0;
        let alpha = layer.alpha.clamp(0.0, 1.0);
        let (y, blur, shadow) = colors.shadow;
        scene.draw_shadow(
            Point::new(at.x, at.y + y),
            size,
            radius,
            blur,
            style::fade(shadow, alpha),
        );
        let fill = if self.pressed {
            style::mix(colors.fill, colors.ink, style::PRESSED_TINT)
        } else {
            colors.fill
        };
        scene.fill_rounded_rect(at, size, radius, style::fade(fill, alpha));
        style::stroke_outline(scene, at, size, radius, style::fade(colors.border, alpha));
        if let Some(label) = &self.label {
            let text = label.size();
            let text_at = Point::new(
                at.x + (size.width - text.width) / 2.0,
                at.y + (size.height - text.height) / 2.0,
            );
            let faded = alpha < 1.0;
            if faded {
                scene.push_layer(at, size, alpha);
            }
            for run in label.to_scene_runs(text_at) {
                scene.draw_glyph_run(run);
            }
            if faded {
                scene.pop_layer();
            }
        }
    }
}

/// The jump control's resolved colours.
#[derive(Clone, Copy)]
struct JumpColors {
    fill: Color,
    border: Color,
    ink: Color,
    /// `(y offset, blur, colour)` of the floating shadow.
    shadow: (f64, f64, Color),
}

impl JumpColors {
    fn resolve(theme: Option<&Theme>) -> Self {
        let level = crate::tokens::scales::floating();
        let spec = theme.map_or(level.shadow_dark, |t| *level.shadow(t.brightness));
        Self {
            fill: style::surface_high(theme),
            border: style::border(theme),
            ink: style::ink(theme),
            shadow: (
                spec.y_offset,
                spec.blur_std_dev,
                style::fade(style::scheme(theme).shadow, spec.color_alpha),
            ),
        }
    }
}

impl<State: 'static> View<State> for MessageScrollerView<State> {
    type Element = MessageScrollerWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> MessageScrollerWidget {
        let own_controller = ScrollController::new();
        let controller = self
            .controller
            .clone()
            .unwrap_or_else(|| own_controller.clone());
        let list = build_child(&self.list(&controller), ctx);
        let empty = if self.shows_empty() {
            self.empty.as_ref().map(|view| build_child(view, ctx))
        } else {
            None
        };
        MessageScrollerWidget {
            list,
            empty,
            own_controller,
            controller,
            unread: self.unread,
            padding: self.padding,
            label: None,
            jump_rect: Rect::ZERO,
            presence: Presence {
                shown: !self.at_end,
                moving: false,
                since: None,
            },
            pressed: false,
            captured: false,
            motion: MotionScheme::neutral(),
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut MessageScrollerWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let own = &element.own_controller;
        let prev_controller = prev.controller.clone().unwrap_or_else(|| own.clone());
        let controller = self.controller.clone().unwrap_or_else(|| own.clone());
        let mut flags = rebuild_child(
            &prev.list(&prev_controller),
            &self.list(&controller),
            &mut element.list,
            ctx,
        );
        element.controller = controller;

        match (
            prev.shows_empty(),
            self.shows_empty(),
            &prev.empty,
            &self.empty,
        ) {
            (true, true, Some(old), Some(new)) => {
                if let Some(pod) = element.empty.as_mut() {
                    flags |= rebuild_child(old, new, pod, ctx);
                }
            }
            (true, false, Some(old), _) => {
                if let Some(mut pod) = element.empty.take() {
                    teardown_child(old, &mut pod, ctx);
                }
                flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
            }
            (false, true, _, Some(new)) => {
                element.empty = Some(build_child(new, ctx));
                flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
            }
            _ => {}
        }

        if element.presence.set(!self.at_end) {
            if self.at_end {
                element.pressed = false;
            }
            flags |= ChangeFlags::PAINT;
        }
        if element.unread != self.unread || element.padding != self.padding {
            element.unread = self.unread;
            element.padding = self.padding;
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        flags
    }

    fn teardown(&self, element: &mut MessageScrollerWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.list(&element.controller), &mut element.list, ctx);
        if let (Some(view), Some(pod)) = (self.empty.as_ref(), element.empty.as_mut()) {
            teardown_child(view, pod, ctx);
        }
    }
}

impl Widget for MessageScrollerWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let (motion, ink, family) = {
            let theme = Theme::from_layout_ctx(ctx);
            (
                theme.map_or(MotionScheme::neutral(), |t| t.motion),
                JumpColors::resolve(theme).ink,
                style::mono_family(theme),
            )
        };
        self.motion = motion;
        let text_style = TextStyle::new(style::LABEL_FONT_SIZE, ink).with_family(family);
        let label =
            ctx.text_context::<TextContext>()
                .layout(&jump_text(self.unread), &text_style, None);
        let label_w = label.size().width;
        self.label = Some(label);

        let size = self.list.layout_child(ctx, bc);
        self.list.set_origin(Point::ZERO);

        let pad = self.padding;
        if let Some(empty) = self.empty.as_mut() {
            let inner = Size::new(
                (size.width - pad.left - pad.right).max(0.0),
                (size.height - pad.top - pad.bottom).max(0.0),
            );
            let got = empty.layout_child(ctx, &BoxConstraints::new(Size::ZERO, inner));
            empty.set_origin(Point::new(
                pad.left + (inner.width - got.width) / 2.0,
                pad.top + (inner.height - got.height) / 2.0,
            ));
        }

        let w = (label_w + style::PAD * 2.0).max(JUMP_HEIGHT);
        let x = (size.width - w) / 2.0;
        let y = size.height - pad.bottom - style::PAD - JUMP_HEIGHT;
        self.jump_rect = Rect::new(x, y, x + w, y + JUMP_HEIGHT);
        size
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let now = ctx.frame_time();
        let (reduce_motion, colors) = {
            let theme = Theme::from_paint_ctx(ctx);
            (
                theme.is_some_and(|t| t.motion.reduce_motion),
                JumpColors::resolve(theme),
            )
        };
        self.list.paint_child(ctx, scene);
        if let Some(empty) = self.empty.as_mut() {
            empty.paint_child(ctx, scene);
        }
        if let Some(layer) = self.presence.layer(now, reduce_motion) {
            self.paint_jump(scene, ctx.origin(), layer, colors);
        }
        if self.presence.moving {
            ctx.request_frame();
        }
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        let InputEvent::Pointer(p) = event else {
            return self.route(ctx, event);
        };
        match p.phase {
            PointerPhase::Down if presses(p) && self.hits_jump(p.position) => {
                self.pressed = true;
                self.captured = true;
                ctx.capture_pointer();
                ctx.request_redraw();
                EventResult::Handled
            }
            PointerPhase::Move if self.captured => {
                let inside = self.hits_jump(p.position);
                if inside != self.pressed {
                    self.pressed = inside;
                    ctx.request_redraw();
                }
                EventResult::Handled
            }
            PointerPhase::Up if self.captured => {
                self.captured = false;
                self.pressed = false;
                if self.hits_jump(p.position) {
                    self.jump();
                }
                ctx.request_redraw();
                EventResult::Handled
            }
            PointerPhase::Cancel if self.captured => {
                self.captured = false;
                self.pressed = false;
                ctx.request_redraw();
                EventResult::Handled
            }
            _ => self.route(ctx, event),
        }
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        ctx.push_container(
            Role::Log,
            |node| {
                node.set_label(LOG_LABEL);
                node.set_live(Live::Polite);
            },
            |ctx| {
                self.list.semantics_child(ctx);
                if let Some(empty) = &self.empty {
                    empty.semantics_child(ctx);
                }
                if self.presence.shown {
                    let origin = ctx.origin();
                    let r = self.jump_rect;
                    let label = jump_label(self.unread);
                    ctx.push_node(Role::Button, |node| {
                        node.set_label(label.as_str());
                        node.add_action(Action::Click);
                        node.set_bounds(style::node_bounds(origin, r));
                    });
                }
            },
        );
    }

    frust::authoring::visit_children!(list, empty);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::any::Any;
    use std::cell::RefCell;
    use std::collections::{BTreeSet, HashMap};

    use frust::authoring::{DiscardScene, PaintOutcome, PointerButton, PointerEvent};
    use frust_core::{RenderRoot, TOUCH_SLOP};

    const WIDTH: f64 = 320.0;
    const HEIGHT: f64 = 600.0;
    /// Deliberately unlike the rows' real heights.
    const ESTIMATE: f64 = 60.0;

    /// Mixed heights: 30..=90 px in 10 px steps, every 13th row 240 px.
    fn height_of(id: u64) -> f64 {
        if id.is_multiple_of(13) {
            240.0
        } else {
            30.0 + ((id * 37) % 7) as f64 * 10.0
        }
    }

    /// What the rows report: builder calls, changed rebuilds, layouts and
    /// where each row was painted last frame.
    #[derive(Default)]
    struct Probe {
        built: RefCell<Vec<usize>>,
        changed: RefCell<Vec<u64>>,
        laid: RefCell<HashMap<u64, usize>>,
        painted: RefCell<HashMap<u64, Rect>>,
    }

    struct App {
        rows: Vec<(u64, f64)>,
        at_end: bool,
        changes: Vec<bool>,
        unread: usize,
    }

    struct Bubble {
        id: u64,
        h: f64,
        probe: Rc<Probe>,
    }

    struct BubbleWidget {
        id: u64,
        h: f64,
        probe: Rc<Probe>,
    }

    impl<S: 'static> View<S> for Bubble {
        type Element = BubbleWidget;

        fn build(&self, _ctx: &mut BuildCtx<'_>) -> BubbleWidget {
            BubbleWidget {
                id: self.id,
                h: self.h,
                probe: self.probe.clone(),
            }
        }

        fn rebuild(&self, prev: &Self, el: &mut BubbleWidget, _: &mut BuildCtx<'_>) -> ChangeFlags {
            el.id = self.id;
            if prev.h == self.h {
                return ChangeFlags::NONE;
            }
            el.h = self.h;
            self.probe.changed.borrow_mut().push(self.id);
            ChangeFlags::LAYOUT | ChangeFlags::PAINT
        }
    }

    impl Widget for BubbleWidget {
        fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
            *self.probe.laid.borrow_mut().entry(self.id).or_default() += 1;
            bc.constrain(Size::new(bc.max().width, self.h))
        }

        fn paint(&mut self, ctx: &mut PaintCtx, _scene: &mut dyn PaintScene) {
            let rect = Rect::from_origin_size(ctx.origin(), ctx.size());
            self.probe.painted.borrow_mut().insert(self.id, rect);
        }
    }

    struct Rig {
        root: RenderRoot<App, AnyView<App>>,
        app: App,
        probe: Rc<Probe>,
        controller: ScrollController,
        text: TextContext,
        window: Size,
        pad: EdgeInsets,
        gap: f64,
        clock_ms: f64,
    }

    impl Rig {
        fn new(ids: impl IntoIterator<Item = u64>) -> Self {
            let mut root = RenderRoot::new();
            root.set_theme(Box::new(crate::baseline()));
            Rig {
                root,
                app: App {
                    rows: ids.into_iter().map(|id| (id, height_of(id))).collect(),
                    at_end: true,
                    changes: Vec::new(),
                    unread: 0,
                },
                probe: Rc::new(Probe::default()),
                controller: ScrollController::new(),
                text: TextContext::new(),
                window: Size::new(WIDTH, HEIGHT),
                pad: EdgeInsets::all(0.0),
                gap: 0.0,
                clock_ms: 0.0,
            }
        }

        fn logic(&self) -> impl FnMut(&mut App) -> AnyView<App> + use<> {
            let (probe, controller) = (self.probe.clone(), self.controller.clone());
            let (pad, gap) = (self.pad, self.gap);
            move |app: &mut App| {
                let rows = Rc::new(app.rows.clone());
                let (keys, probe) = (rows.clone(), probe.clone());
                any(message_scroller(
                    rows.len(),
                    move |i| ChildKey::new(keys[i].0),
                    move |i| {
                        probe.built.borrow_mut().push(i);
                        Bubble {
                            id: rows[i].0,
                            h: rows[i].1,
                            probe: probe.clone(),
                        }
                    },
                )
                .controller(controller.clone())
                .at_end(app.at_end)
                .unread(app.unread)
                .on_at_end_change(|app: &mut App, at_end| {
                    app.at_end = at_end;
                    app.changes.push(at_end);
                })
                .row_gap(gap)
                .padding(pad)
                .estimated_row_extent(ESTIMATE)
                .empty(frust::text("No messages yet")))
            }
        }

        fn frame(&mut self) -> PaintOutcome {
            self.probe.painted.borrow_mut().clear();
            let mut logic = self.logic();
            self.root.rebuild(&mut logic, &mut self.app);
            self.root
                .layout_with_text(self.window, &mut self.text as &mut dyn Any);
            let t = FrameTime::from_nanos((self.clock_ms * 1e6) as u64);
            self.clock_ms += 16.0;
            self.root.paint(&mut DiscardScene, t)
        }

        fn settle(&mut self, limit: usize) {
            for _ in 0..limit {
                if !self.frame().needs_frame {
                    return;
                }
            }
            panic!("did not settle within {limit} frames");
        }

        fn pointer(&mut self, phase: PointerPhase, at: Point) {
            let e = InputEvent::Pointer(PointerEvent {
                phase,
                position: at,
                button: PointerButton::Primary,
            });
            self.root.event(&mut self.app, &e);
        }

        /// A slow drag moving the pointer down by `dy` (away from the end),
        /// held still before release so no fling starts.
        fn drag_from(&mut self, start: Point, dy: f64) {
            let at = |y: f64| Point::new(start.x, start.y + y);
            self.pointer(PointerPhase::Down, start);
            self.frame();
            self.pointer(PointerPhase::Move, at(TOUCH_SLOP + 1.0));
            self.frame();
            for _ in 0..12 {
                self.pointer(PointerPhase::Move, at(TOUCH_SLOP + 1.0 + dy));
                self.frame();
            }
            self.pointer(PointerPhase::Up, at(TOUCH_SLOP + 1.0 + dy));
        }

        fn drag(&mut self, dy: f64) {
            self.drag_from(Point::new(10.0, 150.0), dy);
        }

        fn painted(&self, id: u64) -> Rect {
            *self
                .probe
                .painted
                .borrow()
                .get(&id)
                .unwrap_or_else(|| panic!("row {id} was painted"))
        }

        fn last_id(&self) -> u64 {
            self.app.rows.last().expect("rows").0
        }

        fn assert_end_in_view(&self, what: &str) {
            let bottom = self.painted(self.last_id()).y1;
            let want = self.window.height - self.pad.bottom;
            assert!(
                (bottom - want).abs() < 0.5,
                "{what}: the last row ends at {bottom}, not {want}"
            );
        }

        fn grow_last(&mut self, px: f64) {
            self.app.rows.last_mut().expect("rows").1 += px;
        }

        /// The jump control's semantics node: `(label, bounds)`.
        fn jump_button(&self) -> Option<(String, Rect)> {
            let update = self.root.semantics();
            update
                .nodes
                .iter()
                .find(|(_, n)| n.role() == Role::Button)
                .map(|(_, n)| {
                    let b = n.bounds().expect("the control has bounds");
                    let label = n.label().unwrap_or_default().to_owned();
                    (label, Rect::new(b.x0, b.y0, b.x1, b.y1))
                })
        }

        fn has_text(&self) -> bool {
            self.root
                .inspect()
                .iter()
                .any(|n| n.type_name.ends_with("TextWidget"))
        }
    }

    #[test]
    fn five_thousand_rows_open_at_the_end_and_build_only_the_window() {
        let mut rig = Rig::new(0..5_000);
        rig.settle(8);
        rig.assert_end_in_view("opened");
        assert!(
            rig.app.changes.is_empty(),
            "the initial state is not reported"
        );
        let built = rig.probe.built.borrow();
        let distinct: BTreeSet<usize> = built.iter().copied().collect();
        assert!(distinct.len() <= 40, "built {} rows", distinct.len());
        let lowest = *distinct.first().expect("rows were built");
        assert!(lowest >= 5_000 - 40, "row {lowest} is far from the end");
        assert!(built.len() < 400, "{} builder calls", built.len());
    }

    #[test]
    fn growing_the_last_row_keeps_its_bottom_on_the_viewport_bottom() {
        let mut rig = Rig::new(0..30);
        rig.pad = EdgeInsets {
            left: 8.0,
            top: 12.0,
            right: 8.0,
            bottom: 56.0,
        };
        rig.gap = 8.0;
        rig.settle(8);
        rig.assert_end_in_view("opened");
        let last = rig.last_id();
        let laid_before = rig.probe.laid.borrow().clone();
        rig.probe.changed.borrow_mut().clear();
        for step in 0..200 {
            rig.grow_last(3.0);
            rig.frame();
            rig.assert_end_in_view(&format!("growth step {step}"));
            let (prev, row) = (rig.painted(last - 1), rig.painted(last));
            assert!((row.y0 - prev.y1 - 8.0).abs() < 0.5, "gap at step {step}");
            assert!((row.x0 - 8.0).abs() < 0.5 && (row.x1 - (WIDTH - 8.0)).abs() < 0.5);
        }
        assert!(rig.painted(last).height() > HEIGHT, "outgrew the viewport");
        assert!(rig.probe.changed.borrow().iter().all(|id| *id == last));
        let laid = rig.probe.laid.borrow();
        for (id, n) in laid_before.iter().filter(|(id, _)| **id != last) {
            assert_eq!(laid.get(id), Some(n), "row {id} was laid out again");
        }
        assert!(rig.app.changes.is_empty(), "growth never releases the list");
    }

    #[test]
    fn a_drag_away_releases_once_shows_the_control_and_growth_keeps_the_offset() {
        let mut rig = Rig::new(0..60);
        rig.settle(8);
        assert!(rig.jump_button().is_none(), "hidden at the end");
        rig.drag(300.0);
        rig.settle(60);
        assert_eq!(rig.app.changes, [false]);
        let (label, _) = rig.jump_button().expect("the control shows");
        assert_eq!(label, JUMP_LABEL);
        let offset = rig.controller.offset();
        for _ in 0..20 {
            rig.grow_last(10.0);
            rig.frame();
        }
        rig.settle(8);
        assert!((rig.controller.offset() - offset).abs() < 0.5);
        assert_eq!(rig.app.changes, [false], "growth did not re-stick");
    }

    #[test]
    fn tapping_the_control_returns_to_the_end() {
        let mut rig = Rig::new(0..60);
        rig.settle(8);
        rig.drag(300.0);
        rig.settle(60);
        rig.app.unread = 3;
        rig.settle(60);
        let (label, rect) = rig.jump_button().expect("the control shows");
        assert_eq!(label, "Jump to latest, 3 unread");
        rig.pointer(PointerPhase::Down, rect.center());
        rig.pointer(PointerPhase::Up, rect.center());
        rig.settle(240);
        assert_eq!(rig.app.changes, [false, true]);
        rig.assert_end_in_view("after the jump");
        assert!(rig.jump_button().is_none(), "hidden again");
        rig.grow_last(40.0);
        rig.frame();
        rig.assert_end_in_view("growth after the jump is followed");
    }

    #[test]
    fn appending_while_away_does_not_move_what_the_user_reads() {
        let mut rig = Rig::new(0..60);
        rig.settle(8);
        rig.drag(300.0);
        rig.settle(60);
        let (id, top) = rig
            .probe
            .painted
            .borrow()
            .iter()
            .find(|(_, r)| r.y0 > 100.0 && r.y1 < HEIGHT - 100.0)
            .map(|(id, r)| (*id, r.y0))
            .expect("a row sits mid-viewport");
        rig.app
            .rows
            .extend((100..110).map(|id| (id, height_of(id))));
        rig.settle(8);
        assert!((rig.painted(id).y0 - top).abs() < 0.5, "row {id} moved");
        assert_eq!(rig.app.changes, [false]);
    }

    #[test]
    fn a_shorter_viewport_at_the_end_keeps_the_end_visible() {
        let mut rig = Rig::new(0..60);
        rig.pad.bottom = 40.0;
        rig.settle(8);
        rig.window = Size::new(WIDTH, 320.0);
        rig.frame();
        rig.assert_end_in_view("keyboard up");
        rig.settle(8);
        rig.assert_end_in_view("settled");
        assert!(rig.app.changes.is_empty());
    }

    #[test]
    fn the_empty_view_shows_until_the_first_row() {
        let mut rig = Rig::new([]);
        rig.settle(8);
        assert!(rig.has_text(), "the empty view is mounted");
        rig.app.rows.push((7, 50.0));
        rig.settle(8);
        assert!(!rig.has_text(), "the empty view is gone");
        let row = rig.painted(7);
        assert!(row.y0.abs() < 0.5 && (row.height() - 50.0).abs() < 0.5);
    }

    #[test]
    fn a_hidden_control_never_takes_a_press() {
        let mut rig = Rig::new(0..60);
        rig.settle(8);
        // Where the control would sit: the bottom centre, above the padding.
        let at = Point::new(WIDTH / 2.0, HEIGHT - style::PAD - JUMP_HEIGHT / 2.0);
        rig.drag_from(at, 300.0);
        rig.settle(60);
        assert_eq!(rig.app.changes, [false], "the list took the drag");
    }

    #[test]
    fn the_transcript_is_a_log_region() {
        let mut rig = Rig::new(0..5);
        rig.settle(8);
        let update = rig.root.semantics();
        let (_, log) = update
            .nodes
            .iter()
            .find(|(_, n)| n.role() == Role::Log)
            .expect("a log node");
        assert_eq!(log.label(), Some(LOG_LABEL));
        assert_eq!(log.live(), Some(Live::Polite));
        assert!(update.nodes.iter().any(|(_, n)| n.role() == Role::List));
    }

    #[test]
    fn the_control_slides_in_then_settles() {
        let mut p = Presence {
            shown: false,
            moving: false,
            since: None,
        };
        assert!(p.layer(FrameTime::from_nanos(0), false).is_none());
        assert!(p.set(true));
        let first = p.layer(FrameTime::from_nanos(0), false).expect("entering");
        assert!(first.alpha < 0.01 && first.dy > 0.0, "starts low and clear");
        let end = FrameTime::from_nanos(GLYPH_SLIDE_ENTER.as_nanos() as u64);
        assert_eq!(p.layer(end, false), Some(PatternLayer::IDENTITY));
        assert!(!p.moving);
        assert!(p.set(false));
        assert!(p.layer(end, true).is_none(), "reduced motion hides at once");
    }

    #[test]
    fn equal_views_skip_the_rebuild() {
        let mut rig = Rig::new(0..30);
        rig.gap = 4.0;
        rig.settle(8);
        let mut logic = rig.logic();
        let flags = rig.root.rebuild(&mut logic, &mut rig.app);
        assert_eq!(flags, ChangeFlags::NONE, "an equal view changed nothing");
        assert!(rig.probe.changed.borrow().is_empty());
        rig.app.unread = 2;
        let flags = rig.root.rebuild(&mut logic, &mut rig.app);
        assert!(flags.contains(ChangeFlags::LAYOUT), "a new count reshapes");
        rig.app.at_end = false;
        let flags = rig.root.rebuild(&mut logic, &mut rig.app);
        assert_eq!(flags, ChangeFlags::PAINT, "showing the control only paints");
    }
}
