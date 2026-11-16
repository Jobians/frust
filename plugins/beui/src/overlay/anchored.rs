//! The anchored (non-modal) overlay host: a full-area layer that places its
//! content against a trigger's rect and stages the panel's own entrance and
//! exit.
//!
//! # Where the placement rules come from
//!
//! Upstream has **no single placement module**. `components/motion/`'s
//! `popover-position.ts` is a *measurement* hook — it reads
//! `getBoundingClientRect()` on the trigger and `offsetWidth`/`offsetHeight` on
//! the portalled panel, re-measuring on resize and scroll — and carries no
//! side/align/offset math at all. The rules are spread across the three
//! consumers instead (beUI v2, rev `10c283e433a8f4f0ac0736684d4426ab612b9f55`,
//! retrieved 2026-09-01):
//!
//! * `popover.tsx`'s `buildGeo` — `side` (`top`/`bottom` only), `align`
//!   (`start`/`center`/`end`), `sideOffset` defaulting to `14`, plus the
//!   `ALIGN_ORIGIN` × side transform origin the panel scales about.
//! * `tooltip.tsx` — all four sides, a fixed `GAP` of `8`, always centred, and
//!   its own per-side `transformOrigin` table (`top` scales about
//!   `center bottom`, and so on).
//! * `context-menu.tsx` — the only viewport collision handling upstream ships:
//!   a clamp of the panel's own rect into the viewport inset by
//!   `VIEWPORT_PADDING = 8`.
//!
//! [`place`] is those three folded into one total function, which is what lets
//! a tooltip, a popover and a context menu share one host instead of each
//! re-deriving its own geometry.
//!
//! ## Flip-on-collision is an addition, not a port
//!
//! No upstream component flips to the opposite side when the preferred one does
//! not fit — a popover pinned to `bottom` near the viewport floor simply
//! overflows there, because the page can scroll and the panel is `position:
//! fixed` over it. A frust window does not scroll under an overlay, so the same
//! panel would be clipped by the window edge with no way to reach it. The flip
//! is therefore an explicit adaptation, defaulted **on** and switchable off with
//! [`OverlayPlacement::flip`] for a caller that wants the literal upstream
//! behaviour.
//!
//! # The three pieces
//!
//! 1. [`place`] — the placement math, pure and unit-tested on its own.
//! 2. [`OverlayAnchor`] + [`anchor`] — the trigger side. An anchor rect is in
//!    **window space**, and the only place a widget learns its window-space
//!    position is [`PaintCtx::origin`](frust::authoring::PaintCtx::origin), so
//!    [`anchor`] wraps the trigger in a transparent pod that writes
//!    `Rect::from_origin_size(ctx.origin(), ctx.size())` into a shared cell on
//!    every paint. The host reads the same cell.
//! 3. [`anchored`] — the host: a full-area widget holding one content child,
//!    positioned by [`place`], staged by [`Presence`], light-dismissing on a
//!    press outside the placed content.
//!
//! # Mounting, and what an exit costs
//!
//! The host fills whatever area it is given and treats that box as the window
//! its content is fitted into, so it expects **bounded constraints** — the top
//! child of a full-area [`frust::Stack`], or a transparent navigator page.
//! Neither may itself sit inside a [`frust::scroll_view`], whose child
//! constraints are unbounded on the scroll axis — the trap [`the seam's own
//! docs`](super) spell out.
//!
//! A host switched to [`AnchoredOverlayView::floating`] drops that contract:
//! see [*Floating in the root overlay layer*](#floating-in-the-root-overlay-layer).
//!
//! Mount it **unconditionally** and hand the open flag down through
//! [`AnchoredOverlayView::open`]: that is the kept-mounted shape
//! [`crate::motion::presence`] exists for, and the only one in which an exit
//! ramp is visible at all. A host that is unmounted the frame its flag clears
//! simply vanishes — nothing breaks, the exit is just truncated.
//!
//! A settled-closed host paints nothing, publishes no semantics, claims no
//! focus and dismisses on nothing; it still lays its content out (so
//! [`AnchoredOverlayWidget::content_rect`] stays truthful) and still swallows a
//! press that lands on content which has not finished fading.
//!
//! The entrance and exit are scene transforms, not layout: the content's pod
//! stays where layout put it, so a press mid-ramp is tested against the panel's
//! *resting* rect. The staging scales about an edge of that same rect, so the
//! two never differ by more than the ramp's own scale factor.
//!
//! # Coordinate spaces
//!
//! The anchor is captured in absolute window coordinates; the host places
//! content in its own local space. The host learns its own absolute origin from
//! its paint pass and subtracts it, so the two agree even when the host is not
//! at the window origin — with a one-frame lag on the pass where the host first
//! paints or moves, which the paint arm corrects by requesting a relayout. In
//! the supported mounts the host *is* at the window origin, so the correction is
//! the identity.
//!
//! # Dismissal
//!
//! - **Light dismiss** — a `Down` outside the placed content fires `on_dismiss`
//!   and is **consumed**. This is `lib/hooks/use-dismiss.ts`'s own shape: it
//!   listens on capture-phase `pointerdown`, not on a click. (Upstream's
//!   `"pass-through"` default lets the same gesture also activate what it landed
//!   on; frust has no post-hoc click suppression to build the `"consume"` arm
//!   out of, so the press is consumed here and a click-through variant is not
//!   offered.)
//! - **Escape** — dismisses once the host holds focus. The host claims focus on
//!   every `Down` it sees, so a caller completes one pointer interaction with the
//!   overlay before Escape does anything; there is no auto-focus-on-appear hook
//!   in the framework.
//! - **Focus hand-off** — a dismissal drops the focus path the host took, so the
//!   keyboard chain does not outlive the panel: Escape releases it explicitly,
//!   and a light-dismiss press claims nothing, which the root reads as a blur.
//!   Upstream additionally moves focus back onto the trigger element
//!   (`triggerRef.current?.focus()`); frust has no "focus that widget" call to
//!   aim at a trigger from here, so dropping the path is the whole of the
//!   hand-off and the next `Down` re-seats focus normally.
//! - **No pointer-leave arm.** `PointerPhase` is `Down`/`Move`/`Up`/`Cancel`
//!   with no `Leave`, so "the pointer left the panel" is not an event this host
//!   can answer. A hover-driven overlay (tooltip, hover card) closes on its
//!   *trigger's* own hover bookkeeping instead, and this host stays
//!   pass-through for `Move` so that trigger keeps seeing it.
//!
//! # Floating in the root overlay layer
//!
//! [`AnchoredOverlayView::floating`] moves the content out of the host's box
//! and into the render root's overlay layer through
//! [`frust::authoring::OverlaySlot`] — the mechanism `components::tooltip`
//! already floats its label through — so the host no longer needs a
//! full-area [`frust::Stack`] or any particular place in the tree. A floating
//! host lays itself out at its minimum size (nothing, under loose
//! constraints), which is what lets it sit inside a modal's content column
//! without growing the modal, and the root paints the panel above the whole
//! main tree — modal surface and clips included — and hit-tests it first. The
//! panel is placed in **window** space by the same [`place`], against the
//! anchor rect, at every paint.
//!
//! Dismissal keeps its shape, carried by the slot instead of by a full-area
//! barrier:
//!
//! - **Light dismiss** — the slot is registered `OutsideTap::Notify { consume:
//!   true }` while open, so a primary press outside every floated surface
//!   reaches nothing below (a modal's backdrop included) and fires
//!   `on_dismiss` once. That registration also makes the panel the root's
//!   modal surface for keys while open: a declined Escape is offered to it,
//!   and main-tree shortcuts are masked until it closes.
//! - **Escape** — a press on the panel claims focus for it, as the inline host
//!   claims focus for itself, so the next Escape reaches the host through the
//!   focus chain and closes only the panel; an Escape nothing on the focus path
//!   took reaches it as the declined key above. An Escape that lands on a
//!   focused *trigger* belongs to the trigger: each catalog trigger that opens
//!   a floating panel closes it on Escape itself, so the key never travels up
//!   to an enclosing modal host.
//! - **Exit** — while the exit ramp plays the panel stays registered with
//!   outside taps off, so it still swallows a press that lands on it and lets
//!   every other press through, exactly as the inline host does.
//!
//! What floating costs, all of it the framework portal's own v1 list:
//!
//! - **No hover inside the panel.** An overlay event is a broadcast and records
//!   no hover, so content that reads `PaintCtx::is_hovered` paints unhovered.
//! - **Semantics at the host's position.** The root never walks a floated pod
//!   for semantics, so the host publishes the content's nodes itself — under
//!   its own position and the panel's size, since a node's bounds come from
//!   the pod that publishes it. A host mounted right after its trigger lands
//!   them within the panel's offset of the panel itself.
//! - **No broadcast but the slot's own** reaches the content: a
//!   `Housekeeping` pass, a shortcut or a file-drop end stops at the host.

use std::any::Any;
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use frust::authoring::{
    Affine, AnyView, BoxConstraints, BuildCtx, ChangeFlags, ChildPod, ErasedCallback, EventCtx,
    EventResult, InputEvent, Key, LayoutCtx, NamedKey, OutsideTap, OverlayAnchor as PortalAnchor,
    OverlaySlot, PaintCtx, PaintScene, Point, PointerPhase, Rect, SemanticsCtx, Size, Vec2, View,
    Widget, any, build_child, erase_callback, mark_pending_result_flush, rebuild_child,
    route_event_single, teardown_child, visit_children,
};
use frust::{FrameTime, Theme};

use super::finite_or_zero;
use crate::motion::{Presence, PresencePhase, Ramp};
use crate::tokens::motion::{EASE_OUT, SPRING_PANEL};

/// The side of the anchor an overlay opens on.
///
/// Upstream's popover offers only `top`/`bottom`; its tooltip offers all four,
/// and this host is the union.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OverlaySide {
    /// Above the anchor.
    Top,
    /// To the trailing side of the anchor.
    Right,
    /// Below the anchor — the popover default.
    #[default]
    Bottom,
    /// To the leading side of the anchor.
    Left,
}

impl OverlaySide {
    /// The side a collision flip lands on.
    pub const fn opposite(self) -> Self {
        match self {
            OverlaySide::Top => OverlaySide::Bottom,
            OverlaySide::Bottom => OverlaySide::Top,
            OverlaySide::Left => OverlaySide::Right,
            OverlaySide::Right => OverlaySide::Left,
        }
    }

    /// Whether this side stacks the overlay vertically (`Top`/`Bottom`).
    pub const fn is_vertical(self) -> bool {
        matches!(self, OverlaySide::Top | OverlaySide::Bottom)
    }
}

/// How the overlay lines up with the anchor on the cross axis — `buildGeo`'s
/// `align`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OverlayAlign {
    /// Leading edges flush (`px = 0` in `buildGeo`).
    Start,
    /// Centres flush (`px = (tW - cW) / 2`) — the default.
    #[default]
    Center,
    /// Trailing edges flush (`px = tW - cW`).
    End,
}

/// A resolved placement request: which side, how it lines up, how far off the
/// anchor it sits, and how it may move to stay inside the area.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlayPlacement {
    /// The preferred side.
    pub side: OverlaySide,
    /// Cross-axis alignment.
    pub align: OverlayAlign,
    /// Gap between the anchor and the overlay, in logical px — `sideOffset`.
    pub offset: f64,
    /// Flip to [`OverlaySide::opposite`] when the preferred side does not fit
    /// and the opposite one does. An addition, not a port — see the [module
    /// docs](self).
    pub flip: bool,
    /// Shift the placed rect back inside the padded area when it overflows —
    /// `context-menu.tsx`'s clamp.
    pub clamp: bool,
    /// The margin the panel keeps from every edge of the area, in logical px —
    /// `context-menu.tsx`'s `VIEWPORT_PADDING`. Both the fit test and the clamp
    /// read the area inset by it.
    pub padding: f64,
}

/// The popover's `sideOffset` default: the length of the gooey neck between
/// trigger and panel, in logical px.
pub const SIDE_OFFSET: f64 = 14.0;

/// The tooltip's own gap (`GAP` in `tooltip.tsx`), in logical px — a tooltip
/// sits closer to its trigger than a popover does.
pub const TOOLTIP_OFFSET: f64 = 8.0;

/// The margin an anchored panel keeps from the area's edges, in logical px —
/// `context-menu.tsx`'s `VIEWPORT_PADDING`.
pub const VIEWPORT_PADDING: f64 = 8.0;

/// The scale an anchored panel enters from (`tooltip.tsx`'s `initial.scale`).
pub const ANCHORED_ENTER_SCALE: f64 = 0.9;

/// The scale an anchored panel leaves to (`tooltip.tsx`'s `exit.scale`) — a
/// shallower collapse than the entrance, so leaving reads quicker than
/// arriving.
pub const ANCHORED_EXIT_SCALE: f64 = 0.94;

/// How long an anchored panel's exit takes (`tooltip.tsx`'s
/// `exit.transition.duration`).
pub const ANCHORED_EXIT: Duration = Duration::from_millis(120);

/// Standard deviations of Gaussian blur beyond which a drop shadow's own
/// visible contribution is negligible — three sigma covers ~99.7% of it.
///
/// Mirrors `overlay::modal`'s own `SHADOW_SPILL_NEAR`/`_FAR` derivation
/// (duplicated rather than shared through `overlay::mod`, which carries no
/// item either host reaches into): the content mounted here paints its own
/// shadow outside its layout rect exactly as a modal's panel does, from the
/// same `tokens::theme` glass chrome recipe (`y_offset 24, blur_std_dev 30`).
const SHADOW_SPILL_SIGMAS: f64 = 3.0;
const GLASS_SHADOW_Y_OFFSET: f64 = 24.0;
const GLASS_SHADOW_BLUR_STD_DEV: f64 = 30.0;

/// How far outside the content its own shadow may reach on the sides with no
/// directional offset (top, left, right).
const SHADOW_SPILL_NEAR: f64 = SHADOW_SPILL_SIGMAS * GLASS_SHADOW_BLUR_STD_DEV;

/// How far outside the content its shadow reaches on the offset (downward)
/// side.
const SHADOW_SPILL_FAR: f64 =
    GLASS_SHADOW_Y_OFFSET + SHADOW_SPILL_SIGMAS * GLASS_SHADOW_BLUR_STD_DEV;

impl Default for OverlayPlacement {
    fn default() -> Self {
        OverlayPlacement {
            side: OverlaySide::default(),
            align: OverlayAlign::default(),
            offset: SIDE_OFFSET,
            flip: true,
            clamp: true,
            padding: VIEWPORT_PADDING,
        }
    }
}

impl OverlayPlacement {
    /// A placement on `side`, everything else defaulted.
    pub fn on(side: OverlaySide) -> Self {
        OverlayPlacement {
            side,
            ..Self::default()
        }
    }

    /// Set the cross-axis alignment.
    pub const fn align(mut self, align: OverlayAlign) -> Self {
        self.align = align;
        self
    }

    /// Set the anchor gap, in logical px.
    pub const fn offset(mut self, offset: f64) -> Self {
        self.offset = offset;
        self
    }

    /// Enable or disable the collision flip.
    pub const fn flip(mut self, flip: bool) -> Self {
        self.flip = flip;
        self
    }

    /// Enable or disable the shift-back-inside clamp.
    pub const fn clamp(mut self, clamp: bool) -> Self {
        self.clamp = clamp;
        self
    }

    /// Set the margin kept from the area's edges, in logical px.
    pub const fn padding(mut self, padding: f64) -> Self {
        self.padding = padding;
        self
    }
}

/// The region a panel may occupy: `area` inset by `padding` on every side.
///
/// Falls back to `area` itself when the inset would invert it — a window
/// narrower than twice the padding still has to place its overlay somewhere,
/// and an inverted rect would make the clamp below meaningless.
fn field(area: Rect, padding: f64) -> Rect {
    let inset = area.inset(-padding);
    if inset.width() > 0.0 && inset.height() > 0.0 {
        inset
    } else {
        area
    }
}

/// Whether a `content`-sized overlay fits on `side` of `anchor` inside `field`.
fn fits(side: OverlaySide, anchor: Rect, content: Size, field: Rect, offset: f64) -> bool {
    match side {
        OverlaySide::Top => anchor.y0 - offset - content.height >= field.y0,
        OverlaySide::Bottom => anchor.y1 + offset + content.height <= field.y1,
        OverlaySide::Left => anchor.x0 - offset - content.width >= field.x0,
        OverlaySide::Right => anchor.x1 + offset + content.width <= field.x1,
    }
}

/// The cross-axis start coordinate for `align`, given the anchor's own span
/// `[a0, a1]` and the overlay's `extent` along that axis — `buildGeo`'s `px`.
fn align_start(align: OverlayAlign, a0: f64, a1: f64, extent: f64) -> f64 {
    match align {
        OverlayAlign::Start => a0,
        OverlayAlign::Center => (a0 + a1) / 2.0 - extent / 2.0,
        OverlayAlign::End => a1 - extent,
    }
}

/// Shift `rect` back inside `field`, keeping its size.
///
/// The start edge wins when the overlay is larger than the field (`min` before
/// `max`), which is `context-menu.tsx`'s own nesting: a too-wide panel hangs off
/// the trailing edge rather than the leading one, where its content starts.
fn clamp_into(rect: Rect, field: Rect) -> Rect {
    let x = rect.x0.min(field.x1 - rect.width()).max(field.x0);
    let y = rect.y0.min(field.y1 - rect.height()).max(field.y0);
    Rect::from_origin_size(Point::new(x, y), rect.size())
}

/// Place a `content`-sized overlay against `anchor` inside `area`.
///
/// All three rects are in one coordinate space (the host's own; see the [module
/// docs](self)). The returned rect is the overlay's placed bounds:
///
/// 1. inset `area` by `placement.padding` — every step below works in that
///    field;
/// 2. pick the side: the preferred one, or its opposite when `flip` is on, the
///    preferred one does not fit and the opposite one does;
/// 3. offset off that edge of the anchor by `offset`, lined up on the cross axis
///    per `align`;
/// 4. shift back inside the field when `clamp` is on.
///
/// Pure and total: it allocates nothing, reads no context, and is defined for a
/// degenerate anchor (a zero-size rect places against that point) and for
/// content larger than the field (which pins to the field's *start* edge).
pub fn place(anchor: Rect, content: Size, area: Rect, placement: OverlayPlacement) -> Rect {
    let field = field(area, placement.padding);
    let mut side = placement.side;
    if placement.flip
        && !fits(side, anchor, content, field, placement.offset)
        && fits(side.opposite(), anchor, content, field, placement.offset)
    {
        side = side.opposite();
    }
    let origin = match side {
        OverlaySide::Top => Point::new(
            align_start(placement.align, anchor.x0, anchor.x1, content.width),
            anchor.y0 - placement.offset - content.height,
        ),
        OverlaySide::Bottom => Point::new(
            align_start(placement.align, anchor.x0, anchor.x1, content.width),
            anchor.y1 + placement.offset,
        ),
        OverlaySide::Left => Point::new(
            anchor.x0 - placement.offset - content.width,
            align_start(placement.align, anchor.y0, anchor.y1, content.height),
        ),
        OverlaySide::Right => Point::new(
            anchor.x1 + placement.offset,
            align_start(placement.align, anchor.y0, anchor.y1, content.height),
        ),
    };
    let rect = Rect::from_origin_size(origin, content);
    if placement.clamp {
        clamp_into(rect, field)
    } else {
        rect
    }
}

/// The point a placed panel scales about — the corner or edge nearest its
/// anchor, so the panel appears to grow *out of* the trigger.
///
/// The fold of upstream's two origin tables: `tooltip.tsx`'s per-side
/// `transformOrigin` (`top` → `center bottom`, `bottom` → `center top`, `left` →
/// `right center`, `right` → `left center`) and `popover.tsx`'s `ALIGN_ORIGIN`
/// (`start` → `left`, `center` → `center`, `end` → `right`), which replaces the
/// tooltip's fixed `center` on whichever axis `align` governs.
///
/// Takes the **resolved** rect, so a panel the clamp moved still scales about
/// its own edge rather than a point outside it.
pub fn transform_origin(side: OverlaySide, align: OverlayAlign, rect: Rect) -> Point {
    let along = |a0: f64, a1: f64| match align {
        OverlayAlign::Start => a0,
        OverlayAlign::Center => (a0 + a1) / 2.0,
        OverlayAlign::End => a1,
    };
    match side {
        // The panel sits above the anchor, so it grows from its own bottom edge.
        OverlaySide::Top => Point::new(along(rect.x0, rect.x1), rect.y1),
        OverlaySide::Bottom => Point::new(along(rect.x0, rect.x1), rect.y0),
        OverlaySide::Left => Point::new(rect.x1, along(rect.y0, rect.y1)),
        OverlaySide::Right => Point::new(rect.x0, along(rect.y0, rect.y1)),
    }
}

/// A shared cell holding a trigger's window-space rect: written by [`anchor`] on
/// every paint of the wrapped trigger, read by [`anchored`] on every layout.
///
/// Clone it — the clone shares the same cell. An app keeps one per anchored
/// overlay in its `Component::State`, hands a clone to the trigger and another
/// to the host.
///
/// **Not reactive.** Writing a rect wakes no frame; it is read during the layout
/// pass the same frame anything that moved the trigger already scheduled, and
/// the host's paint arm requests a relayout when it sees the rect change under
/// it. An overlay whose trigger moves with nothing else repainting is therefore
/// one frame behind, never permanently stale. This is the frust equivalent of
/// `popover-position.ts`'s `ResizeObserver` + scroll/resize listeners, which
/// re-measure for the same reason.
#[derive(Clone, Debug, Default)]
pub struct OverlayAnchor(Rc<std::cell::Cell<Rect>>);

impl OverlayAnchor {
    /// A fresh anchor, holding the degenerate rect at the window origin until a
    /// trigger paints (see [`OverlayAnchor::rect`]).
    pub fn new() -> Self {
        Self::default()
    }

    /// The captured trigger rect, in window space.
    ///
    /// Before the wrapped trigger's first paint this is `Rect::ZERO` — a
    /// zero-size anchor at the window origin, which places an overlay at the
    /// top-left. In practice a trigger always paints before it can be pressed.
    pub fn rect(&self) -> Rect {
        self.0.get()
    }

    /// Overwrite the captured rect — for a caller anchoring against something it
    /// positions itself: a context menu's press point, a caret, a canvas hit.
    pub fn set(&self, rect: Rect) {
        self.0.set(rect);
    }
}

/// Wrap `child` so its window-space rect is captured into `anchor` on every
/// paint — the trigger half of an anchored overlay.
///
/// Transparent in every other respect: it lays out, paints, routes events to and
/// publishes the semantics of `child` unchanged, and paints nothing of its own.
pub fn anchor<State: 'static, V: View<State>>(
    anchor: &OverlayAnchor,
    child: V,
) -> OverlayAnchorView<State> {
    OverlayAnchorView {
        child: any(child),
        anchor: anchor.clone(),
    }
}

/// A declarative anchor-capturing wrapper. See [`anchor`].
pub struct OverlayAnchorView<State: 'static> {
    child: AnyView<State>,
    anchor: OverlayAnchor,
}

/// The retained widget for an [`OverlayAnchorView`].
pub struct OverlayAnchorWidget {
    child: ChildPod,
    anchor: OverlayAnchor,
}

impl<State: 'static> View<State> for OverlayAnchorView<State> {
    type Element = OverlayAnchorWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> OverlayAnchorWidget {
        OverlayAnchorWidget {
            child: build_child(&self.child, ctx),
            anchor: self.anchor.clone(),
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut OverlayAnchorWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        element.anchor = self.anchor.clone();
        rebuild_child(&prev.child, &self.child, &mut element.child, ctx)
    }

    fn teardown(&self, element: &mut OverlayAnchorWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.child, &mut element.child, ctx);
    }
}

impl Widget for OverlayAnchorWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let size = self.child.layout_child(ctx, bc);
        self.child.set_origin(Point::ORIGIN);
        bc.constrain(size)
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        // `PaintCtx::origin` is absolute window space — the one read that
        // answers "where is this trigger on screen", which is what an anchored
        // overlay needs and what `getBoundingClientRect()` answers upstream.
        self.anchor
            .set(Rect::from_origin_size(ctx.origin(), ctx.size()));
        self.child.paint_child(ctx, scene);
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        route_event_single(&mut self.child, ctx, event)
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        self.child.semantics_child(ctx);
    }

    visit_children!(child);
}

/// A view-held, typed callback (erased on build).
type OnState<State> = Rc<dyn Fn(&mut State)>;

/// Build an anchored-overlay host showing `content`. See the [module
/// docs](self) for the mounting, coordinate-space and dismissal contracts.
///
/// Chain [`AnchoredOverlayView::anchor`] to point it at a trigger,
/// [`AnchoredOverlayView::placement`] to pick the side/align/offset,
/// [`AnchoredOverlayView::open`] to drive it, and
/// [`AnchoredOverlayView::on_dismiss`] to answer a light dismiss or Escape.
pub fn anchored<State: 'static, V: View<State>>(content: V) -> AnchoredOverlayView<State> {
    AnchoredOverlayView {
        content: any(content),
        anchor: OverlayAnchor::new(),
        placement: OverlayPlacement::default(),
        open: true,
        enter: Ramp::spring(SPRING_PANEL),
        exit: Ramp::eased(ANCHORED_EXIT, EASE_OUT),
        on_dismiss: None,
        on_exited: None,
        floating: false,
        stage: StageCell::default(),
    }
}

/// A declarative anchored-overlay host. See [`anchored`].
pub struct AnchoredOverlayView<State: 'static> {
    /// The content — wrapped in the floated pod's own staging view once
    /// [`floating`](Self::floating) is set.
    content: AnyView<State>,
    anchor: OverlayAnchor,
    placement: OverlayPlacement,
    open: bool,
    enter: Ramp,
    exit: Ramp,
    on_dismiss: Option<OnState<State>>,
    on_exited: Option<OnState<State>>,
    floating: bool,
    /// What the host's paint hands a floated pod; shared with the wrapper the
    /// content is moved into.
    stage: StageCell,
}

impl<State: 'static> AnchoredOverlayView<State> {
    /// Float the content in the render root's overlay layer instead of placing
    /// it inside this host's own box — see the [module
    /// docs](self#floating-in-the-root-overlay-layer). A floating host needs no
    /// full-area mount: it can sit anywhere under its trigger's ancestors, a
    /// modal's content included.
    pub fn floating(self) -> Self {
        if self.floating {
            return self;
        }
        let AnchoredOverlayView {
            content,
            anchor,
            placement,
            open,
            enter,
            exit,
            on_dismiss,
            on_exited,
            stage,
            ..
        } = self;
        AnchoredOverlayView {
            content: any(FloatedView {
                content,
                stage: stage.clone(),
            }),
            anchor,
            placement,
            open,
            enter,
            exit,
            on_dismiss,
            on_exited,
            floating: true,
            stage,
        }
    }

    /// Anchor the overlay to the rect `anchor` carries (see [`OverlayAnchor`]).
    pub fn anchor(mut self, anchor: &OverlayAnchor) -> Self {
        self.anchor = anchor.clone();
        self
    }

    /// Set the placement (side, align, offset, flip/clamp/padding).
    pub fn placement(mut self, placement: OverlayPlacement) -> Self {
        self.placement = placement;
        self
    }

    /// Tell a kept-mounted host whether it is open. The default is `true`, which
    /// is the mount-on-open contract: a mounted host is an open one, and its
    /// exit is never seen.
    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    /// Replace the entrance ramp (default: a [`SPRING_PANEL`] spring).
    pub fn enter(mut self, enter: Ramp) -> Self {
        self.enter = enter;
        self
    }

    /// Replace the exit ramp (default: [`ANCHORED_EXIT`] on [`EASE_OUT`]).
    pub fn exit(mut self, exit: Ramp) -> Self {
        self.exit = exit;
        self
    }

    /// Set the dismiss callback: a press outside the placed content, or Escape
    /// once the host holds focus.
    pub fn on_dismiss<F: Fn(&mut State) + 'static>(mut self, on_dismiss: F) -> Self {
        self.on_dismiss = Some(Rc::new(on_dismiss));
        self
    }

    /// Set the exit-finished callback: the host is settled closed and may be
    /// dropped entirely.
    ///
    /// Fired from the **event** pass following the paint that settled the exit
    /// — the deferral [`crate::motion::presence`] documents, since a paint
    /// carries no `EventCtx` to reach app state through. A host nobody sends
    /// another event to keeps laying its content out at zero presence until one
    /// arrives, which is the same inert cost a settled-closed host already pays.
    /// A [floating](Self::floating) host, which sits on no pointer's path, asks
    /// for that pass itself: the next rebuild's `Housekeeping` broadcast.
    pub fn on_exited<F: Fn(&mut State) + 'static>(mut self, on_exited: F) -> Self {
        self.on_exited = Some(Rc::new(on_exited));
        self
    }
}

/// The retained widget for an [`AnchoredOverlayView`].
pub struct AnchoredOverlayWidget {
    /// The content, or — for a floating host, whose content is the floated
    /// pod — a vacant placeholder.
    content: ChildPod,
    /// The floating host's slot; `None` for an inline host.
    floated: Option<Box<dyn FloatSlot>>,
    /// What the floated pod stages its content with (see [`Stage`]).
    stage: StageCell,
    /// The area a floated pod is placed in: the window, or the host's own
    /// area under a bare layout context.
    window: Size,
    anchor: OverlayAnchor,
    placement: OverlayPlacement,
    /// Whether the overlay is open. `false` makes the host inert without
    /// unmounting it — the kept-mounted pattern the module docs describe.
    open: bool,
    /// The ramps [`presence`](Self::presence) is rebuilt from when the theme's
    /// `reduce_motion` flips (see `sync_motion`).
    enter: Ramp,
    exit: Ramp,
    presence: Presence,
    /// The `reduce_motion` value `presence` was last built for; `None` until the
    /// first paint resolves a theme.
    reduced: Option<bool>,
    on_dismiss: Option<ErasedCallback>,
    on_exited: Option<ErasedCallback>,
    /// The placed content rect, in this host's own coordinate space.
    rect: Rect,
    /// The anchor rect the last layout placed against (window space) — compared
    /// at paint so a trigger that moved forces a relayout.
    anchor_used: Rect,
    /// This host's own absolute origin, learned at paint.
    host_origin: Point,
}

impl AnchoredOverlayWidget {
    /// The placed content rect in the host's own coordinate space — what the
    /// light-dismiss hit test compares against.
    ///
    /// Placement runs whether the host is open or not, so this stays truthful
    /// while a kept-mounted overlay plays its exit.
    pub fn content_rect(&self) -> Rect {
        self.rect
    }

    /// Whether the host is open (see [`AnchoredOverlayView::open`]).
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Whether the content floats in the root overlay layer (see
    /// [`AnchoredOverlayView::floating`]).
    pub fn is_floating(&self) -> bool {
        self.floated.is_some()
    }

    /// The presence driver's current phase — visible through the whole exit,
    /// [`PresencePhase::Absent`] once it settles.
    pub fn phase(&self) -> PresencePhase {
        self.presence.phase()
    }

    /// Rebuild the presence driver when `reduce_motion` flips, preserving what
    /// the old one was doing.
    ///
    /// [`Presence::collapsed`] is a constructor, not a switch, so a live toggle
    /// has to replace the driver. A driver replaced mid-*exit* is re-opened and
    /// re-closed on the spot: that keeps the pending exit real (and so still
    /// reported through [`Presence::take_exited`]) instead of silently dropping
    /// an owner's unmount bookkeeping on the floor. One replaced while fully
    /// `Present` is parked back there directly rather than restarted from
    /// `Absent`: `Presence::set_open` only ever *opens into* an entrance, so a
    /// naive rebuild here would replay the whole thing over a panel the user
    /// is already looking at, every time a ramp/config/`reduce_motion` change
    /// lands on an overlay that was simply sitting open.
    fn sync_motion(&mut self, reduce: bool, now: FrameTime) {
        if self.reduced == Some(reduce) {
            return;
        }
        let was_exiting = self.presence.phase() == PresencePhase::Exiting;
        let was_present = self.presence.phase() == PresencePhase::Present;
        self.reduced = Some(reduce);
        let base = Presence::new(self.enter, self.exit);
        let mut next = if reduce { base.collapsed() } else { base };
        if was_exiting && !self.open {
            next.set_open(true);
        }
        next.set_open(self.open);
        if was_present && self.open {
            // Two `advance` calls — one to latch the fresh driver's own
            // clock, one at (or past) its settle time — land it on `Present`
            // without a frame of visible motion.
            let settle = next.active_ramp().settle();
            let settled_at =
                FrameTime::from_nanos(now.as_nanos().saturating_add(settle.as_nanos() as u64));
            next.advance(now);
            next.advance(settled_at);
        }
        self.presence = next;
    }

    /// Fire the dismiss callback and hand the keyboard chain back (see the
    /// module docs' focus note).
    ///
    /// The release is what the *Escape* path needs — a key dispatch that
    /// releases drops the root's focus session. The light-dismiss path reaches
    /// the same end differently: its `Down` claims no focus, and the root reads
    /// an unclaimed `Down` as a blur.
    fn dismiss(&mut self, ctx: &mut EventCtx) {
        if let Some(on_dismiss) = self.on_dismiss.as_mut() {
            on_dismiss(ctx);
        }
        if ctx.has_focus() {
            ctx.release_focus();
        }
    }
}

impl<State: 'static> View<State> for AnchoredOverlayView<State> {
    type Element = AnchoredOverlayWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> AnchoredOverlayWidget {
        let mut presence = Presence::new(self.enter, self.exit);
        presence.set_open(self.open);
        let (content, floated) = if self.floating {
            (
                build_child::<State, _>(&Vacant, ctx),
                Some(self.mount_floated(ctx)),
            )
        } else {
            (build_child(&self.content, ctx), None)
        };
        AnchoredOverlayWidget {
            content,
            floated,
            stage: self.stage.clone(),
            window: Size::ZERO,
            anchor: self.anchor.clone(),
            placement: self.placement,
            open: self.open,
            enter: self.enter,
            exit: self.exit,
            presence,
            reduced: None,
            on_dismiss: self.on_dismiss.as_ref().map(erase_callback),
            on_exited: self.on_exited.as_ref().map(erase_callback),
            rect: Rect::ZERO,
            anchor_used: Rect::ZERO,
            host_origin: Point::ORIGIN,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut AnchoredOverlayWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        element.stage = self.stage.clone();
        let mut flags = self.rebuild_content(prev, element, ctx);
        if element.placement != self.placement {
            element.placement = self.placement;
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        if element.enter != self.enter || element.exit != self.exit {
            element.enter = self.enter;
            element.exit = self.exit;
            // Force the next paint to rebuild the driver around the new ramps,
            // the same path a `reduce_motion` flip takes.
            element.reduced = None;
            flags |= ChangeFlags::PAINT;
        }
        if element.open != self.open {
            element.open = self.open;
            if element.presence.set_open(self.open) {
                // The paint is what starts the ramp; the layout underneath it is
                // unchanged either way (placement runs open or closed).
                flags |= ChangeFlags::PAINT;
            }
        }
        element.anchor = self.anchor.clone();
        // A trigger that moved since the last layout must re-place the content;
        // the anchor cell is external, so no prop above reports it. Signal LAYOUT
        // so per-pod layout skipping cannot keep the stale placement.
        if element.anchor.rect() != element.anchor_used {
            flags |= ChangeFlags::LAYOUT;
        }
        // Closures aren't comparable, so the adapters are reinstalled
        // unconditionally — cheap, and what every interactive widget does.
        element.on_dismiss = self.on_dismiss.as_ref().map(erase_callback);
        element.on_exited = self.on_exited.as_ref().map(erase_callback);
        flags
    }

    fn teardown(&self, element: &mut AnchoredOverlayWidget, ctx: &mut BuildCtx<'_>) {
        if self.floating {
            if let Some(slot) = floated_slot::<State>(&mut element.floated) {
                slot.rebuild(Some(&self.content), None, ctx);
            }
            element.floated = None;
            teardown_child::<State, _>(&Vacant, &mut element.content, ctx);
        } else {
            teardown_child(&self.content, &mut element.content, ctx);
        }
    }
}

impl<State: 'static> AnchoredOverlayView<State> {
    /// A fresh slot with this view's content mounted in it.
    fn mount_floated(&self, ctx: &mut BuildCtx<'_>) -> Box<dyn FloatSlot> {
        let mut slot = OverlaySlot::<State>::new();
        slot.rebuild(None, Some(&self.content), ctx);
        Box::new(slot)
    }

    /// Reconcile the content in whichever home it has — the inline pod or the
    /// floated slot — moving it across when [`floating`](Self::floating)
    /// changed between two frames.
    fn rebuild_content(
        &self,
        prev: &Self,
        element: &mut AnchoredOverlayWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        match (prev.floating, self.floating) {
            (false, false) => {
                rebuild_child(&prev.content, &self.content, &mut element.content, ctx)
            }
            (true, true) => match floated_slot::<State>(&mut element.floated) {
                Some(slot) => slot.rebuild(Some(&prev.content), Some(&self.content), ctx),
                None => {
                    element.floated = Some(self.mount_floated(ctx));
                    ChangeFlags::LAYOUT | ChangeFlags::PAINT
                }
            },
            (false, true) => {
                teardown_child(&prev.content, &mut element.content, ctx);
                element.content = build_child::<State, _>(&Vacant, ctx);
                element.floated = Some(self.mount_floated(ctx));
                ChangeFlags::LAYOUT | ChangeFlags::PAINT
            }
            (true, false) => {
                if let Some(slot) = floated_slot::<State>(&mut element.floated) {
                    slot.rebuild(Some(&prev.content), None, ctx);
                }
                element.floated = None;
                teardown_child::<State, _>(&Vacant, &mut element.content, ctx);
                element.content = build_child(&self.content, ctx);
                ChangeFlags::LAYOUT | ChangeFlags::PAINT
            }
        }
    }
}

/// The typed slot behind a floating host's erased one — `None` for an inline
/// host. The widget is not generic over the app state, so the slot is held
/// erased and recovered here, in the view's typed `build`/`rebuild`/
/// `teardown`, the only places that hold a typed view to reconcile it with.
fn floated_slot<State: 'static>(
    floated: &mut Option<Box<dyn FloatSlot>>,
) -> Option<&mut OverlaySlot<State>> {
    floated
        .as_mut()
        .and_then(|slot| slot.as_any_mut().downcast_mut::<OverlaySlot<State>>())
}

impl AnchoredOverlayWidget {
    /// The floating host's layout: the pod against the window, the host
    /// itself at its minimum size.
    fn layout_floated(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let window = ctx.window_size();
        self.window = if window.width > 0.0 && window.height > 0.0 {
            window
        } else {
            // A bare layout context has no window; the host's own offered
            // area stands in for it (see `Stage::area`).
            Size::new(
                finite_or_zero(bc.max().width),
                finite_or_zero(bc.max().height),
            )
        };
        let mut stage = self.stage.get();
        stage.area = self.window;
        self.stage.set(stage);
        self.content
            .layout_child(ctx, &BoxConstraints::loose(Size::ZERO));
        self.content.set_origin(Point::ORIGIN);
        let Some(slot) = self.floated.as_mut() else {
            return bc.constrain(Size::ZERO);
        };
        slot.layout(ctx);
        let content = slot.pod_size();
        self.anchor_used = self.anchor.rect();
        self.rect = place(
            self.anchor_used,
            content,
            Rect::from_origin_size(Point::ORIGIN, self.window),
            self.placement,
        ) - self.host_origin.to_vec2();
        bc.constrain(Size::ZERO)
    }

    /// The floating host's paint: advance the ramp, then place, stage and
    /// register the pod. The root paints it, after the whole main tree.
    fn paint_floated(&mut self, ctx: &mut PaintCtx) {
        self.host_origin = ctx.origin();
        let reduce = Theme::from_paint_ctx(ctx).is_some_and(|theme| theme.motion.reduce_motion);
        self.sync_motion(reduce, ctx.frame_time());
        let was_visible = self.presence.is_visible();
        let progress = self.presence.advance(ctx.frame_time());
        if self.presence.is_animating() {
            ctx.request_frame();
        }
        if !self.presence.is_visible() {
            // Settled closed: not registered, so nothing hit-tests it and no
            // outside press is reported to it. A floating host sits on no
            // pointer's path, so the exit report is not left waiting for an
            // event that may never come: the next rebuild owes a
            // `Housekeeping` broadcast, which drains it.
            if was_visible && self.on_exited.is_some() {
                mark_pending_result_flush();
                ctx.request_frame();
            }
            return;
        }
        let (scale, alpha) = self.staging(progress);
        let Some(slot) = self.floated.as_mut() else {
            return;
        };
        let content = slot.pod_size();
        self.anchor_used = self.anchor.rect();
        let placed = place(
            self.anchor_used,
            content,
            Rect::from_origin_size(Point::ORIGIN, self.window),
            self.placement,
        );
        self.rect = placed - self.host_origin.to_vec2();
        self.stage.set(Stage {
            scale,
            alpha,
            side: self.placement.side,
            align: self.placement.align,
            open: self.open,
            area: self.window,
        });
        slot.set_outside_tap(if self.open {
            OutsideTap::Notify { consume: true }
        } else {
            OutsideTap::Ignore
        });
        slot.place_at(placed.origin());
        let size = ctx.size();
        slot.paint(ctx, size);
    }

    /// The floating host's event pass: the slot first, then what it reports.
    fn event_floated(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        let Some(slot) = self.floated.as_mut() else {
            return EventResult::Ignored;
        };
        if let Some(result) = slot.route(ctx, event) {
            // Drained unconditionally, so a notification that arrives while
            // closing is spent rather than left standing for a later pass.
            let outside = slot.take_outside_down();
            let declined_escape = slot.take_escape();
            if !self.open {
                return result;
            }
            if outside || declined_escape {
                if declined_escape {
                    ctx.consume_overlay_key();
                }
                self.dismiss(ctx);
                ctx.request_redraw();
                return result;
            }
            // A key the focused panel declined: Escape closes the panel, and
            // every other key falls through, as on the inline host.
            if result == EventResult::Ignored
                && let InputEvent::Key(key) = event
                && key.key == Key::Named(NamedKey::Escape)
            {
                self.dismiss(ctx);
                return EventResult::Handled;
            }
            return result;
        }
        // Not the slot's: only a raw press reaches a floating host on its own —
        // before its first registration, or under a bare test context — and it
        // is a light dismiss whenever it misses the placed panel.
        if self.open
            && let InputEvent::Pointer(p) = event
            && p.phase == PointerPhase::Down
        {
            if !self.rect.contains(p.position) {
                self.dismiss(ctx);
            }
            return EventResult::Handled;
        }
        EventResult::Ignored
    }

    /// The staging scale and alpha at ramp `progress` — the scale-and-fade
    /// both modes play.
    fn staging(&self, progress: f64) -> (f64, f32) {
        let from = if self.presence.phase() == PresencePhase::Exiting {
            ANCHORED_EXIT_SCALE
        } else {
            ANCHORED_ENTER_SCALE
        };
        // Raw progress for the scale (a spring's overshoot is what makes it
        // spring), clamped for the alpha, which is only meaningful in `[0, 1]`.
        (
            from + (1.0 - from) * progress,
            progress.clamp(0.0, 1.0) as f32,
        )
    }
}

/// Push the staging around a panel resting at `rect` (absolute): a fade layer
/// wide enough for the panel's own shadow while `alpha` is under `1.0`, then
/// the scale about the edge facing the anchor. Returns whether a layer was
/// pushed, for [`pop_stage`].
///
/// A pushed layer clips to its own rectangle, and the content paints its own
/// shadow outside its layout rect — the same discipline `overlay::modal`'s
/// panel keeps, and for the same reason: a layer sized to the resting `rect`
/// alone would cut the shadow at every partial alpha. `scale` never grows past
/// `1.0` while `alpha` is under it (both [`ANCHORED_ENTER_SCALE`] and
/// [`ANCHORED_EXIT_SCALE`] are under `1.0`), and the transform's pivot sits on
/// `rect`'s own edge, so a contraction toward it never moves the transformed
/// content outside `rect` — only the shadow needs the extra room, and only
/// while still fading; a fully open panel needs no layer at all.
fn push_stage(
    scene: &mut dyn PaintScene,
    rect: Rect,
    side: OverlaySide,
    align: OverlayAlign,
    scale: f64,
    alpha: f32,
) -> bool {
    let pivot = transform_origin(side, align, rect);
    let layer_pushed = alpha < 1.0;
    if layer_pushed {
        let layer = Rect::new(
            rect.x0 - SHADOW_SPILL_NEAR,
            rect.y0 - SHADOW_SPILL_NEAR,
            rect.x1 + SHADOW_SPILL_NEAR,
            rect.y1 + SHADOW_SPILL_FAR,
        );
        scene.push_layer(layer.origin(), layer.size(), alpha);
    }
    scene.push_transform(
        Affine::translate(pivot.to_vec2())
            * Affine::scale(scale)
            * Affine::translate(-pivot.to_vec2()),
    );
    layer_pushed
}

/// Pop what [`push_stage`] pushed.
fn pop_stage(scene: &mut dyn PaintScene, layer_pushed: bool) {
    scene.pop_transform();
    if layer_pushed {
        scene.pop_layer();
    }
}

impl Widget for AnchoredOverlayWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        if self.floated.is_some() {
            return self.layout_floated(ctx, bc);
        }
        let area = Size::new(
            finite_or_zero(bc.max().width),
            finite_or_zero(bc.max().height),
        );
        let content = self.content.layout_child(ctx, &BoxConstraints::loose(area));
        self.anchor_used = self.anchor.rect();
        // Window space → this host's own space (the identity in the supported
        // mounts; see the module docs).
        let anchor_local = self.anchor_used - self.host_origin.to_vec2();
        self.rect = place(
            anchor_local,
            content,
            Rect::from_origin_size(Point::ORIGIN, area),
            self.placement,
        );
        self.content.set_origin(self.rect.origin());
        bc.constrain(area)
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        if self.floated.is_some() {
            // Nothing of the content paints here: the root paints the pod.
            self.paint_floated(ctx);
            return;
        }
        // Self-correction, the paint-time mirror of the layout above: this is
        // the only pass that knows the host's absolute origin, and the anchor
        // cell can move under a layout that already ran.
        if ctx.origin() != self.host_origin || self.anchor.rect() != self.anchor_used {
            self.host_origin = ctx.origin();
            ctx.request_layout();
        }

        let reduce = Theme::from_paint_ctx(ctx).is_some_and(|theme| theme.motion.reduce_motion);
        self.sync_motion(reduce, ctx.frame_time());

        let progress = self.presence.advance(ctx.frame_time());
        // A ramp with a visible endpoint: unpaced frames while it plays, none
        // once it settles.
        if self.presence.is_animating() {
            ctx.request_frame();
        }
        if !self.presence.is_visible() {
            // Settled closed: no scrim, no panel, nothing of the content either.
            // The exit-completion latch is drained from the event pass (see
            // `AnchoredOverlayView::on_exited`).
            return;
        }

        // The host paints no chrome of its own — no scrim, no panel surface.
        // Everything visible is the content's; the host only stages it.
        let (scale, alpha) = self.staging(progress);
        let rect = self.rect + ctx.origin().to_vec2();
        let layer_pushed = push_stage(
            scene,
            rect,
            self.placement.side,
            self.placement.align,
            scale,
            alpha,
        );
        self.content.paint_child(ctx, scene);
        pop_stage(scene, layer_pushed);
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        // The exit-completion latch, drained before anything else so a settled
        // exit is reported on the first pass that can carry it.
        if self.presence.take_exited()
            && let Some(on_exited) = self.on_exited.as_mut()
        {
            on_exited(ctx);
        }
        if self.floated.is_some() {
            return self.event_floated(ctx, event);
        }

        // A closed (or closing) kept-mounted host claims no focus and dismisses
        // on nothing: light dismiss and Escape are off the moment `open` flips.
        // A broadcast still reaches the content, which is what keeps its pods
        // live. A `Down` still reaches the content while the panel is visibly
        // present, so it still gets first refusal — but the panel itself must
        // swallow whatever lands on it regardless of what the content answers,
        // matching the open path below (a press on the panel's own background,
        // not just a control inside it, is still the panel's): a press that
        // fell through here would reach the page underneath a panel the user
        // can still see. Once the exit settles the host is fully transparent
        // to input again: the press that closed the overlay already reached
        // the page, and every later one does too.
        if !self.open {
            if event.is_broadcast() {
                self.content.event_child(ctx, event);
                return EventResult::Ignored;
            }
            if self.presence.is_visible()
                && let InputEvent::Pointer(p) = event
            {
                match p.phase {
                    PointerPhase::Down => {
                        // A press inside `rect` belongs to the panel regardless of
                        // what the content answers (the barrier's widening), but a
                        // press outside `rect` that the content itself consumed is
                        // still `Handled` — its own answer isn't discarded just
                        // because it fell outside the panel's own bounds.
                        let content_handled = route_event_single(&mut self.content, ctx, event)
                            == EventResult::Handled;
                        return if self.rect.contains(p.position) || content_handled {
                            EventResult::Handled
                        } else {
                            EventResult::Ignored
                        };
                    }
                    PointerPhase::Up | PointerPhase::Cancel => {
                        // Forward Up/Cancel to allow captures from Down to be released.
                        route_event_single(&mut self.content, ctx, event);
                        return EventResult::Ignored;
                    }
                    _ => {}
                }
            }
            return EventResult::Ignored;
        }

        // Claim focus on a `Down` that lands on the panel, before the routing
        // below — the opt-in that makes Escape reachable at all, and claimed
        // even when the content consumes the press (a control inside the panel
        // claiming focus of its own simply wins the descent; both pods sit on
        // the same chain). Re-claiming while already focused is a no-op.
        //
        // A press *outside* the panel deliberately claims nothing: the root
        // reads a `Down` that bubbled no claim as a blur, which is what hands
        // the keyboard chain back on a light dismiss (a `Down` arm consults no
        // release flag, so an explicit release there would be ignored).
        if matches!(event, InputEvent::Pointer(p)
            if p.phase == PointerPhase::Down && self.rect.contains(p.position))
        {
            ctx.request_focus();
        }
        // Content first, always: a broadcast reaches it unconsumed, a focused
        // child takes the key events, and a press inside it is its own. The host
        // claims nothing before this (the claim-ordering rule).
        if route_event_single(&mut self.content, ctx, event) == EventResult::Handled {
            return EventResult::Handled;
        }
        if let InputEvent::Key(key) = event {
            if key.key == Key::Named(NamedKey::Escape) {
                self.dismiss(ctx);
                return EventResult::Handled;
            }
            // Every other key falls through: an anchored overlay is not a
            // barrier, and the page below keeps its own shortcuts.
            return EventResult::Ignored;
        }
        let InputEvent::Pointer(p) = event else {
            return EventResult::Ignored;
        };
        match p.phase {
            PointerPhase::Down => {
                // Deliberately button-agnostic, unlike every press machine in
                // the catalog: a light dismiss is not a press, and the browser
                // this ports from closes an open popover on a right-click
                // outside it exactly as on a left one. The host captures
                // nothing here — it only fires the app's callback — so the
                // primary-only press rule has nothing to protect.
                if !self.rect.contains(p.position) {
                    self.dismiss(ctx);
                }
                // Consumed either way: dismissing swallows the press, and a
                // press on the panel's own background is the panel's.
                EventResult::Handled
            }
            // Everything else passes through, so a trigger under the host keeps
            // seeing its own hover and scroll (the module docs' no-leave note).
            _ => EventResult::Ignored,
        }
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        // The host contributes no node of its own — the content (a menu, a
        // tooltip) carries the role worth reporting. A closed host reports
        // nothing: a panel on its way out is not there to be read.
        if !self.open {
            return;
        }
        match &self.floated {
            // The root walks no floated pod for semantics, so the host
            // publishes it — at its own position (see the module docs).
            Some(slot) => slot.visit_pod(&mut |pod| pod.semantics_child(ctx)),
            None => self.content.semantics_child(ctx),
        }
    }

    visit_children!(content);

    // A floated pod's stops follow the host's own, as on the framework portal.
    fn visit_focus_children(&self, visitor: &mut dyn FnMut(&ChildPod)) {
        visitor(&self.content);
        if let Some(slot) = &self.floated {
            slot.visit_pod(visitor);
        }
    }
}

// ---- The floated pod -------------------------------------------------------

/// What a floating host's paint hands its pod each frame: the staging the pod
/// applies around the content, and the area the pod lays out against when the
/// layout context names no window.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Stage {
    scale: f64,
    alpha: f32,
    side: OverlaySide,
    align: OverlayAlign,
    /// Whether the host is open — a closing panel claims no focus.
    open: bool,
    /// The host's area — the pod's layout fallback under a bare layout
    /// context, whose window size is zero.
    area: Size,
}

impl Default for Stage {
    fn default() -> Self {
        Stage {
            scale: 1.0,
            alpha: 1.0,
            side: OverlaySide::default(),
            align: OverlayAlign::default(),
            open: true,
            area: Size::ZERO,
        }
    }
}

/// The host-to-pod staging handle. Each frame's view carries a fresh one; the
/// host and the pod both adopt it on rebuild.
type StageCell = Rc<Cell<Stage>>;

/// A floating host's pod: the content, staged with the host's ramp. The root
/// paints this pod in its own overlay pass, which is why the staging travels
/// to it through [`Stage`] rather than being pushed around it by the host.
struct FloatedView<State: 'static> {
    content: AnyView<State>,
    stage: StageCell,
}

/// The retained widget for a [`FloatedView`].
struct FloatedWidget {
    content: ChildPod,
    stage: StageCell,
}

impl<State: 'static> View<State> for FloatedView<State> {
    type Element = FloatedWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> FloatedWidget {
        FloatedWidget {
            content: build_child(&self.content, ctx),
            stage: self.stage.clone(),
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut FloatedWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        element.stage = self.stage.clone();
        rebuild_child(&prev.content, &self.content, &mut element.content, ctx)
    }

    fn teardown(&self, element: &mut FloatedWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.content, &mut element.content, ctx);
    }
}

impl Widget for FloatedWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let max = bc.max();
        let fallback = BoxConstraints::loose(self.stage.get().area);
        let bc = if max.width > 0.0 && max.height > 0.0 {
            bc
        } else {
            &fallback
        };
        let size = self.content.layout_child(ctx, bc);
        self.content.set_origin(Point::ORIGIN);
        bc.constrain(size)
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let stage = self.stage.get();
        let rect = Rect::from_origin_size(ctx.origin(), ctx.size());
        let layer_pushed = push_stage(
            scene,
            rect,
            stage.side,
            stage.align,
            stage.scale,
            stage.alpha,
        );
        self.content.paint_child(ctx, scene);
        pop_stage(scene, layer_pushed);
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        let down = matches!(event, InputEvent::Pointer(p) if p.phase == PointerPhase::Down);
        // The inline host's focus opt-in, made by the panel itself: a press on
        // an open panel claims focus, so the next Escape reaches the host
        // through the focus chain. Claimed before the routing, so a control
        // inside that claims focus of its own still wins the descent.
        if down && self.stage.get().open {
            ctx.request_focus();
        }
        if route_event_single(&mut self.content, ctx, event) == EventResult::Handled {
            return EventResult::Handled;
        }
        // A press on the panel's own background is the panel's.
        if down {
            EventResult::Handled
        } else {
            EventResult::Ignored
        }
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        self.content.semantics_child(ctx);
    }

    visit_children!(content);
}

/// A floating host's inline content: nothing, since its content is the pod.
struct Vacant;

/// The retained widget for [`Vacant`].
struct VacantWidget;

impl<State: 'static> View<State> for Vacant {
    type Element = VacantWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> VacantWidget {
        VacantWidget
    }

    fn rebuild(
        &self,
        _prev: &Self,
        _element: &mut VacantWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        ChangeFlags::NONE
    }
}

impl Widget for VacantWidget {
    fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        bc.constrain(Size::ZERO)
    }

    fn paint(&mut self, _ctx: &mut PaintCtx, _scene: &mut dyn PaintScene) {}
}

/// A floating host's [`OverlaySlot`], erased so [`AnchoredOverlayWidget`]
/// stays non-generic over the app state (see [`floated_slot`]).
trait FloatSlot {
    fn layout(&mut self, ctx: &mut LayoutCtx);
    fn paint(&mut self, ctx: &mut PaintCtx, owner: Size);
    fn route(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> Option<EventResult>;
    fn take_outside_down(&mut self) -> bool;
    fn take_escape(&mut self) -> bool;
    fn visit_pod(&self, visitor: &mut dyn FnMut(&ChildPod));
    fn set_outside_tap(&mut self, outside_tap: OutsideTap);
    /// Pin the pod's window-space top-left (the host places it itself).
    fn place_at(&mut self, origin: Point);
    fn as_any_mut(&mut self) -> &mut dyn Any;

    /// The pod's laid-out size, `Size::ZERO` while none is mounted.
    fn pod_size(&self) -> Size {
        let mut size = Size::ZERO;
        self.visit_pod(&mut |pod| size = pod.size());
        size
    }
}

impl<S: 'static> FloatSlot for OverlaySlot<S> {
    fn layout(&mut self, ctx: &mut LayoutCtx) {
        OverlaySlot::layout(self, ctx);
    }

    fn paint(&mut self, ctx: &mut PaintCtx, owner: Size) {
        OverlaySlot::paint(self, ctx, owner);
    }

    fn route(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> Option<EventResult> {
        // The pod is built over the ambient application state.
        self.event_ambient(ctx, event)
    }

    fn take_outside_down(&mut self) -> bool {
        OverlaySlot::take_outside_down(self)
    }

    fn take_escape(&mut self) -> bool {
        OverlaySlot::take_escape(self)
    }

    fn visit_pod(&self, visitor: &mut dyn FnMut(&ChildPod)) {
        self.visit_focus_pod(visitor);
    }

    fn set_outside_tap(&mut self, outside_tap: OutsideTap) {
        OverlaySlot::set_outside_tap(self, outside_tap);
    }

    fn place_at(&mut self, origin: Point) {
        // A window anchor is placed as stated: the host already ran `place`.
        self.set_anchor(PortalAnchor::Window {
            point: origin,
            offset: Vec2::ZERO,
        });
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use frust::FrameTime;
    use frust::authoring::{
        Color, KeyEvent, Modifiers, PointerButton, PointerEvent, text::TextContext,
    };
    use frust_core::RenderRoot;
    use std::any::Any;

    const AREA: Rect = Rect::new(0.0, 0.0, 400.0, 600.0);
    const WINDOW: Size = Size::new(400.0, 600.0);
    /// Placement with the padding switched off, so a geometry assertion reads
    /// against the raw area rather than the inset one.
    fn bare() -> OverlayPlacement {
        OverlayPlacement::default().padding(0.0)
    }

    fn anchor_rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect::from_origin_size(Point::new(x, y), Size::new(w, h))
    }

    // ---- placement math ---------------------------------------------------

    #[test]
    fn the_default_placement_is_the_popovers_bottom_center_at_its_own_offset() {
        let p = OverlayPlacement::default();
        assert_eq!(p.side, OverlaySide::Bottom);
        assert_eq!(p.align, OverlayAlign::Center);
        assert_eq!(p.offset, SIDE_OFFSET);
        assert_eq!(p.padding, VIEWPORT_PADDING);
        assert!(p.flip && p.clamp);
        // The tooltip sits closer than the popover does.
        const { assert!(TOOLTIP_OFFSET < SIDE_OFFSET) };
    }

    #[test]
    fn each_side_offsets_off_its_own_edge() {
        let a = anchor_rect(100.0, 200.0, 80.0, 40.0);
        let content = Size::new(120.0, 60.0);
        // The raw side placement, with the collision passes off: `left` would
        // otherwise flip (a 120px panel does not fit in the 100px to the
        // anchor's left), which the flip test below covers on its own.
        let raw = |side| OverlayPlacement::on(side).flip(false).clamp(false);
        let bottom = place(a, content, AREA, raw(OverlaySide::Bottom));
        assert_eq!(bottom.y0, a.y1 + SIDE_OFFSET);
        let top = place(a, content, AREA, raw(OverlaySide::Top));
        assert_eq!(top.y1, a.y0 - SIDE_OFFSET);
        let right = place(a, content, AREA, raw(OverlaySide::Right));
        assert_eq!(right.x0, a.x1 + SIDE_OFFSET);
        let left = place(a, content, AREA, raw(OverlaySide::Left));
        assert_eq!(left.x1, a.x0 - SIDE_OFFSET);
        // Sizes are never altered by placement.
        for r in [bottom, top, right, left] {
            assert_eq!(r.size(), content);
        }
        assert!(OverlaySide::Top.is_vertical() && !OverlaySide::Left.is_vertical());
        assert_eq!(OverlaySide::Top.opposite(), OverlaySide::Bottom);
        assert_eq!(OverlaySide::Left.opposite(), OverlaySide::Right);
    }

    #[test]
    fn align_lines_up_leading_center_or_trailing_edges() {
        let a = anchor_rect(100.0, 200.0, 80.0, 40.0);
        let content = Size::new(120.0, 60.0);
        let at = |side, align| place(a, content, AREA, OverlayPlacement::on(side).align(align));
        assert_eq!(at(OverlaySide::Bottom, OverlayAlign::Start).x0, a.x0);
        assert_eq!(at(OverlaySide::Bottom, OverlayAlign::End).x1, a.x1);
        assert_eq!(
            at(OverlaySide::Bottom, OverlayAlign::Center).center().x,
            a.center().x
        );

        // The same three on a horizontal side act on the vertical axis.
        assert_eq!(at(OverlaySide::Right, OverlayAlign::Start).y0, a.y0);
        assert_eq!(at(OverlaySide::Right, OverlayAlign::End).y1, a.y1);
        assert_eq!(
            at(OverlaySide::Right, OverlayAlign::Center).center().y,
            a.center().y
        );
    }

    #[test]
    fn a_side_that_does_not_fit_flips_to_the_opposite_one() {
        // An anchor near the bottom edge: `bottom` overflows, `top` fits.
        let a = anchor_rect(100.0, 560.0, 80.0, 20.0);
        let content = Size::new(120.0, 100.0);
        let flipped = place(a, content, AREA, bare());
        assert_eq!(flipped.y1, a.y0 - SIDE_OFFSET, "flipped above the anchor");

        // With the flip disabled it stays below and only the clamp moves it —
        // the literal upstream behaviour, which never flips.
        let pinned = place(a, content, AREA, bare().flip(false));
        assert_eq!(pinned.y1, AREA.y1, "clamped, not flipped");

        // Neither side fits: the preferred one is kept (and clamped).
        let tall = Size::new(120.0, 590.0);
        let kept = place(a, tall, AREA, bare());
        assert_eq!(kept.y1, AREA.y1);
    }

    #[test]
    fn each_side_flips_at_the_edge_it_would_overflow() {
        let content = Size::new(120.0, 100.0);
        // Top edge: a `top` placement flips down.
        let high = anchor_rect(100.0, 10.0, 80.0, 20.0);
        let down = place(high, content, AREA, OverlayPlacement::on(OverlaySide::Top));
        assert_eq!(down.y0, high.y1 + SIDE_OFFSET);
        // Left edge: a `left` placement flips right.
        let leading = anchor_rect(10.0, 200.0, 20.0, 20.0);
        let right = place(
            leading,
            content,
            AREA,
            OverlayPlacement::on(OverlaySide::Left),
        );
        assert_eq!(right.x0, leading.x1 + SIDE_OFFSET);
        // Right edge: a `right` placement flips left.
        let trailing = anchor_rect(370.0, 200.0, 20.0, 20.0);
        let left = place(
            trailing,
            content,
            AREA,
            OverlayPlacement::on(OverlaySide::Right),
        );
        assert_eq!(left.x1, trailing.x0 - SIDE_OFFSET);
    }

    #[test]
    fn clamping_shifts_the_rect_back_inside_and_can_be_turned_off() {
        // An anchor at the right edge, centre-aligned: the panel overflows.
        let a = anchor_rect(380.0, 100.0, 20.0, 20.0);
        let content = Size::new(200.0, 50.0);
        let clamped = place(a, content, AREA, bare());
        assert_eq!(clamped.x1, AREA.x1);
        assert_eq!(clamped.y0, a.y1 + SIDE_OFFSET, "only the cross axis moved");

        let free = place(a, content, AREA, bare().clamp(false));
        assert!(free.x1 > AREA.x1, "unclamped placement may overflow");

        // Content wider than the area pins to the leading edge (the start-edge
        // rule), not the trailing one.
        let huge = Size::new(600.0, 50.0);
        let pinned = place(a, huge, AREA, bare());
        assert_eq!(pinned.x0, AREA.x0);
    }

    #[test]
    fn the_clamp_keeps_the_viewport_padding_off_every_edge() {
        let content = Size::new(200.0, 50.0);
        // Trailing overflow lands `VIEWPORT_PADDING` short of the area edge.
        let trailing = place(
            anchor_rect(380.0, 100.0, 20.0, 20.0),
            content,
            AREA,
            OverlayPlacement::default(),
        );
        assert_eq!(trailing.x1, AREA.x1 - VIEWPORT_PADDING);
        // …and so does a leading one.
        let leading = place(
            anchor_rect(0.0, 100.0, 20.0, 20.0),
            content,
            AREA,
            OverlayPlacement::default(),
        );
        assert_eq!(leading.x0, AREA.x0 + VIEWPORT_PADDING);
        // The bottom edge is the same rule on the other axis.
        let low = place(
            anchor_rect(100.0, 560.0, 20.0, 20.0),
            Size::new(100.0, 300.0),
            AREA,
            OverlayPlacement::default().flip(false),
        );
        assert_eq!(low.y1, AREA.y1 - VIEWPORT_PADDING);
    }

    #[test]
    fn a_padding_larger_than_the_area_falls_back_to_the_area_itself() {
        // A window narrower than twice the padding still has to place its
        // overlay somewhere; the inset would invert, so it is dropped.
        let tiny = Rect::new(0.0, 0.0, 10.0, 10.0);
        let rect = place(
            Rect::ZERO,
            Size::new(4.0, 4.0),
            tiny,
            OverlayPlacement::default().flip(false),
        );
        assert!(tiny.contains(rect.origin()));
        assert_eq!(rect.x0, 0.0, "clamped to the un-inset area's own edge");
    }

    #[test]
    fn an_uncaptured_anchor_places_at_the_padded_area_origin() {
        let content = Size::new(80.0, 40.0);
        let rect = place(Rect::ZERO, content, AREA, OverlayPlacement::default());
        assert_eq!(
            rect.x0,
            AREA.x0 + VIEWPORT_PADDING,
            "clamped in from the centred overhang"
        );
        assert_eq!(rect.y0, SIDE_OFFSET);
    }

    #[test]
    fn the_transform_origin_is_the_edge_facing_the_anchor() {
        let rect = Rect::new(100.0, 200.0, 220.0, 260.0);
        // A panel below its anchor grows down from its own top edge.
        let below = transform_origin(OverlaySide::Bottom, OverlayAlign::Center, rect);
        assert_eq!(below, Point::new(rect.center().x, rect.y0));
        // A panel above it grows up from its own bottom edge.
        let above = transform_origin(OverlaySide::Top, OverlayAlign::Center, rect);
        assert_eq!(above, Point::new(rect.center().x, rect.y1));
        // A panel to the anchor's right grows out of its own left edge.
        let right = transform_origin(OverlaySide::Right, OverlayAlign::Center, rect);
        assert_eq!(right, Point::new(rect.x0, rect.center().y));
        let left = transform_origin(OverlaySide::Left, OverlayAlign::Center, rect);
        assert_eq!(left, Point::new(rect.x1, rect.center().y));
        // `align` replaces the centre on whichever axis it governs.
        assert_eq!(
            transform_origin(OverlaySide::Bottom, OverlayAlign::Start, rect),
            Point::new(rect.x0, rect.y0)
        );
        assert_eq!(
            transform_origin(OverlaySide::Bottom, OverlayAlign::End, rect),
            Point::new(rect.x1, rect.y0)
        );
        assert_eq!(
            transform_origin(OverlaySide::Left, OverlayAlign::End, rect),
            Point::new(rect.x1, rect.y1)
        );
    }

    // ---- the host ---------------------------------------------------------

    #[derive(Default)]
    struct AppState {
        dismissed: u32,
        exited: u32,
        pressed: u32,
        open: bool,
        /// Presses the floating harness's full-window cover received.
        covered: u32,
    }

    /// A fixed-size leaf that reports every press into the app state — enough to
    /// prove a press inside the content reached it rather than the host.
    struct Panel(Size);

    /// The retained half of [`Panel`].
    struct PanelWidget(Size);

    impl View<AppState> for Panel {
        type Element = PanelWidget;
        fn build(&self, _ctx: &mut BuildCtx<'_>) -> PanelWidget {
            PanelWidget(self.0)
        }
        fn rebuild(
            &self,
            _prev: &Self,
            element: &mut PanelWidget,
            _ctx: &mut BuildCtx<'_>,
        ) -> ChangeFlags {
            element.0 = self.0;
            ChangeFlags::NONE
        }
    }

    impl Widget for PanelWidget {
        fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
            bc.constrain(self.0)
        }
        fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
            scene.fill_rect(ctx.origin(), ctx.size(), Color::BLACK);
        }
        fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
            let InputEvent::Pointer(p) = event else {
                return EventResult::Ignored;
            };
            if p.phase == PointerPhase::Down {
                ctx.state_mut::<AppState>().pressed += 1;
                return EventResult::Handled;
            }
            EventResult::Ignored
        }
    }

    /// A fixed-size leaf that paints its own drop shadow outside its layout
    /// rect — the geometry a fade layer must stay wide enough not to clip.
    struct ShadowedPanel(Size);

    /// The retained half of [`ShadowedPanel`].
    struct ShadowedPanelWidget(Size);

    impl View<AppState> for ShadowedPanel {
        type Element = ShadowedPanelWidget;
        fn build(&self, _ctx: &mut BuildCtx<'_>) -> ShadowedPanelWidget {
            ShadowedPanelWidget(self.0)
        }
        fn rebuild(
            &self,
            _prev: &Self,
            element: &mut ShadowedPanelWidget,
            _ctx: &mut BuildCtx<'_>,
        ) -> ChangeFlags {
            element.0 = self.0;
            ChangeFlags::NONE
        }
    }

    impl Widget for ShadowedPanelWidget {
        fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
            bc.constrain(self.0)
        }
        fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
            let origin = ctx.origin();
            let size = ctx.size();
            scene.fill_rect(origin, size, Color::BLACK);
            // The shadow's own visual reach: `SHADOW_SPILL_NEAR` on the sides
            // with no offset, `SHADOW_SPILL_FAR` on the downward one.
            let shadow_origin =
                Point::new(origin.x - SHADOW_SPILL_NEAR, origin.y - SHADOW_SPILL_NEAR);
            let shadow_size = Size::new(
                size.width + SHADOW_SPILL_NEAR * 2.0,
                size.height + SHADOW_SPILL_NEAR + SHADOW_SPILL_FAR,
            );
            scene.fill_rect(shadow_origin, shadow_size, Color::BLACK);
        }
    }

    /// A fixed-size leaf that never claims a press — for proving the panel's
    /// own barrier does not depend on what the content answers.
    struct DecliningPanel(Size);

    /// The retained half of [`DecliningPanel`].
    struct DecliningPanelWidget(Size);

    impl View<AppState> for DecliningPanel {
        type Element = DecliningPanelWidget;
        fn build(&self, _ctx: &mut BuildCtx<'_>) -> DecliningPanelWidget {
            DecliningPanelWidget(self.0)
        }
        fn rebuild(
            &self,
            _prev: &Self,
            element: &mut DecliningPanelWidget,
            _ctx: &mut BuildCtx<'_>,
        ) -> ChangeFlags {
            element.0 = self.0;
            ChangeFlags::NONE
        }
    }

    impl Widget for DecliningPanelWidget {
        fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
            bc.constrain(self.0)
        }
        fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
            scene.fill_rect(ctx.origin(), ctx.size(), Color::BLACK);
        }
        // `event` deliberately left at the `Widget` default (`Ignored`).
    }

    /// A fixed-size leaf that captures the pointer on `Down` and answers every
    /// event `Handled` for as long as it holds the capture — even once the
    /// pointer has moved outside its own bounds — for proving the closed
    /// host's barrier doesn't discard the content's own `Handled` for an
    /// event that lands outside `rect` but that the content still consumed.
    struct CapturingPanel(Size);

    /// The retained half of [`CapturingPanel`].
    struct CapturingPanelWidget(Size);

    impl View<AppState> for CapturingPanel {
        type Element = CapturingPanelWidget;
        fn build(&self, _ctx: &mut BuildCtx<'_>) -> CapturingPanelWidget {
            CapturingPanelWidget(self.0)
        }
        fn rebuild(
            &self,
            _prev: &Self,
            element: &mut CapturingPanelWidget,
            _ctx: &mut BuildCtx<'_>,
        ) -> ChangeFlags {
            element.0 = self.0;
            ChangeFlags::NONE
        }
    }

    impl Widget for CapturingPanelWidget {
        fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
            bc.constrain(self.0)
        }
        fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
            scene.fill_rect(ctx.origin(), ctx.size(), Color::BLACK);
        }
        fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
            let InputEvent::Pointer(p) = event else {
                return EventResult::Ignored;
            };
            if p.phase == PointerPhase::Down {
                ctx.capture_pointer();
            }
            EventResult::Handled
        }
    }

    const CONTENT: Size = Size::new(120.0, 60.0);
    const ANCHOR: Rect = Rect::new(100.0, 200.0, 180.0, 240.0);

    /// A recording scene: the filled rects in paint order, the alpha of each
    /// composited layer, and each layer's own recorded rectangle (origin,
    /// size) alongside it.
    #[derive(Default)]
    struct Recorder {
        rects: Vec<(Point, Size)>,
        alphas: Vec<f32>,
        layers: Vec<(Point, Size)>,
    }

    impl PaintScene for Recorder {
        fn push_layer(&mut self, origin: Point, size: Size, alpha: f32) {
            self.alphas.push(alpha);
            self.layers.push((origin, size));
        }
        fn fill_rect(&mut self, origin: Point, size: Size, _color: Color) {
            self.rects.push((origin, size));
        }
        fn draw_text(&mut self, _origin: Point, _text: &str) {}
    }

    /// The host mounted the way an app mounts it: as the top child of a
    /// full-area [`frust::Stack`]. The `Stack` is load-bearing for the Escape
    /// tests — it is the container that focus-routes a key event to whichever
    /// child holds the focus path, which is what the host's own focus claim
    /// buys.
    struct Harness {
        root: RenderRoot<AppState, frust::StackView<AppState>>,
        state: AppState,
        tcx: TextContext,
        anchor: OverlayAnchor,
        shadowed: bool,
    }

    impl Harness {
        fn new() -> Self {
            Self::with_theme(None)
        }

        /// A host whose content paints its own shadow outside its layout
        /// rect (see [`ShadowedPanel`]) — for a layer-containment check that
        /// needs a real shadow to contain.
        fn shadowed() -> Self {
            let anchor = OverlayAnchor::new();
            anchor.set(ANCHOR);
            let mut h = Harness {
                root: RenderRoot::new(),
                state: AppState {
                    open: true,
                    ..AppState::default()
                },
                tcx: TextContext::new(),
                anchor,
                shadowed: true,
            };
            h.pass();
            h
        }

        fn with_theme(theme: Option<Theme>) -> Self {
            let anchor = OverlayAnchor::new();
            anchor.set(ANCHOR);
            let mut root = RenderRoot::new();
            if let Some(theme) = theme {
                root.set_theme(Box::new(theme));
            }
            let mut h = Harness {
                root,
                state: AppState {
                    open: true,
                    ..AppState::default()
                },
                tcx: TextContext::new(),
                anchor,
                shadowed: false,
            };
            h.pass();
            h
        }

        fn pass(&mut self) {
            let anchor = self.anchor.clone();
            let shadowed = self.shadowed;
            let mut logic = move |s: &mut AppState| {
                let on_dismiss = |s: &mut AppState| {
                    s.dismissed += 1;
                    s.open = false;
                };
                let on_exited = |s: &mut AppState| s.exited += 1;
                let view: AnyView<AppState> = if shadowed {
                    any(anchored(ShadowedPanel(CONTENT))
                        .anchor(&anchor)
                        .open(s.open)
                        .on_dismiss(on_dismiss)
                        .on_exited(on_exited))
                } else {
                    any(anchored(Panel(CONTENT))
                        .anchor(&anchor)
                        .open(s.open)
                        .on_dismiss(on_dismiss)
                        .on_exited(on_exited))
                };
                frust::stack().child(view)
            };
            self.root.rebuild(&mut logic, &mut self.state);
            self.root
                .layout_with_text(WINDOW, &mut self.tcx as &mut dyn Any);
        }

        fn paint(&mut self, ms: u64) -> Recorder {
            let mut rec = Recorder::default();
            self.root
                .paint(&mut rec, FrameTime::from_nanos(ms * 1_000_000));
            rec
        }

        fn pointer(&mut self, phase: PointerPhase, x: f64, y: f64) -> EventResult {
            let outcome = self.root.event(
                &mut self.state,
                &InputEvent::Pointer(PointerEvent {
                    phase,
                    position: Point::new(x, y),
                    button: PointerButton::Primary,
                }),
            );
            if outcome.handled {
                EventResult::Handled
            } else {
                EventResult::Ignored
            }
        }

        fn escape(&mut self) {
            self.root.event(
                &mut self.state,
                &InputEvent::Key(KeyEvent {
                    key: Key::Named(NamedKey::Escape),
                    modifiers: Modifiers::default(),
                    repeat: false,
                }),
            );
        }
    }

    fn placed() -> Rect {
        place(ANCHOR, CONTENT, AREA, OverlayPlacement::default())
    }

    #[test]
    fn the_host_fills_its_area_and_places_the_content_against_the_anchor() {
        let mut h = Harness::new();
        let rect = placed();
        // Bottom/centre of the anchor, `SIDE_OFFSET` below it.
        assert_eq!(rect.y0, ANCHOR.y1 + SIDE_OFFSET);
        assert_eq!(rect.center().x, ANCHOR.center().x);
        // Nothing painted by the host itself; the content lands at the
        // placement.
        let rec = h.paint(0);
        assert_eq!(rec.rects, vec![(rect.origin(), CONTENT)]);
    }

    #[test]
    fn an_unbounded_height_collapses_the_host_area_and_pins_the_content_to_its_top() {
        // The documented mounts (a navigator page, a full-area `Stack`) are
        // bounded; a host put inside a scroll view is not, and this is what that
        // costs — pinned, not fixed here: the coercion cannot invent an extent
        // nobody offered, so the fix belongs at the mount site.
        let cell = OverlayAnchor::new();
        cell.set(ANCHOR);
        let view = anchored(Panel(CONTENT)).anchor(&cell);
        let mut counter = 0u64;
        let mut w = View::<AppState>::build(&view, &mut BuildCtx::new(&mut counter));
        let mut tcx = TextContext::new();
        let mut lctx = LayoutCtx::with_text_context(&mut tcx as &mut dyn Any);
        // The constraints `frust::scroll_view` hands its child: the viewport
        // width, an infinite max height.
        let scrolled = BoxConstraints::new(
            Size::new(WINDOW.width, 0.0),
            Size::new(WINDOW.width, f64::INFINITY),
        );
        let size = w.layout(&mut lctx, &scrolled);
        assert_eq!(size.height, 0.0, "no vertical area to fill");
        assert_eq!(
            w.content_rect().y0,
            0.0,
            "the panel clamps to the top of a zero-height area, wherever its \
             anchor sits"
        );

        // The same host under the contract's own constraints places normally.
        let bounded = w.layout(&mut lctx, &BoxConstraints::tight(WINDOW));
        assert_eq!(bounded, WINDOW);
        assert_eq!(w.content_rect().y0, ANCHOR.y1 + SIDE_OFFSET);
    }

    #[test]
    fn a_press_outside_the_content_dismisses_once_and_is_swallowed() {
        let mut h = Harness::new();
        assert_eq!(
            h.pointer(PointerPhase::Down, 5.0, 5.0),
            EventResult::Handled,
            "the light dismiss consumes the press"
        );
        assert_eq!(h.state.dismissed, 1);
        assert_eq!(h.state.pressed, 0, "the content never saw it");
        // The app's flag is now false, so the next pass closes the host — and a
        // second press outside it dismisses nothing.
        h.pass();
        h.pointer(PointerPhase::Down, 5.0, 5.0);
        assert_eq!(h.state.dismissed, 1, "dismissed exactly once");
    }

    #[test]
    fn a_press_inside_the_content_routes_to_it_and_never_dismisses() {
        let mut h = Harness::new();
        let c = placed().center();
        assert_eq!(
            h.pointer(PointerPhase::Down, c.x, c.y),
            EventResult::Handled
        );
        assert_eq!(h.state.pressed, 1);
        assert_eq!(h.state.dismissed, 0);
    }

    #[test]
    fn moves_outside_the_content_pass_through_untouched() {
        let mut h = Harness::new();
        assert_eq!(
            h.pointer(PointerPhase::Move, 5.0, 5.0),
            EventResult::Ignored,
            "a hover-driven trigger under the host keeps its own moves"
        );
        assert_eq!(h.state.dismissed, 0);
    }

    #[test]
    fn escape_dismisses_exactly_once_after_a_press_has_focused_the_host() {
        let mut h = Harness::new();
        h.escape();
        assert_eq!(
            h.state.dismissed, 0,
            "no focus yet, so Escape has no chain to travel"
        );
        // A press on the content focuses the host without dismissing it.
        let c = placed().center();
        h.pointer(PointerPhase::Down, c.x, c.y);
        h.pointer(PointerPhase::Up, c.x, c.y);
        h.escape();
        assert_eq!(h.state.dismissed, 1);
        // The dismissal handed the keyboard chain back, so a repeated Escape
        // reaches nothing — and the closed host would refuse it anyway.
        h.escape();
        assert_eq!(h.state.dismissed, 1, "dismissed exactly once");
    }

    #[test]
    fn a_light_dismiss_press_claims_no_focus_so_the_chain_is_handed_back() {
        let mut h = Harness::new();
        // Seat the focus on the panel first.
        let c = placed().center();
        h.pointer(PointerPhase::Down, c.x, c.y);
        h.pointer(PointerPhase::Up, c.x, c.y);
        // A press outside dismisses without re-claiming, so the root blurs and
        // a following Escape reaches nothing.
        h.pointer(PointerPhase::Down, 5.0, 5.0);
        assert_eq!(h.state.dismissed, 1);
        h.escape();
        assert_eq!(h.state.dismissed, 1, "the keyboard chain went with it");
    }

    #[test]
    fn a_moved_anchor_is_re_placed_on_the_next_layout() {
        let mut h = Harness::new();
        h.anchor.set(anchor_rect(20.0, 20.0, 40.0, 20.0));
        h.pass();
        let rec = h.paint(0);
        let expected = place(h.anchor.rect(), CONTENT, AREA, OverlayPlacement::default());
        assert_eq!(rec.rects[0].0, expected.origin());
    }

    // ---- presence staging -------------------------------------------------

    #[test]
    fn the_entrance_ramps_the_content_in_and_asks_for_frames_until_it_settles() {
        let mut h = Harness::new();
        let first = h.paint(0);
        assert_eq!(first.alphas.len(), 1, "one staged layer, the content's");
        assert!(first.alphas[0] < 1.0, "the entrance starts transparent");
        // Well past the spring's settle time it is fully present and asks for
        // nothing more — no layer either, since nothing is left to fade.
        let settled = h.paint(4_000);
        assert!(
            settled.alphas.is_empty(),
            "a fully open panel needs no fade layer"
        );
    }

    #[test]
    fn a_closed_host_stays_visible_for_its_whole_exit_and_only_then_reports_itself() {
        let mut h = Harness::new();
        h.paint(0);
        h.paint(4_000);
        // Close it, keeping it mounted: the panel is still painted while the
        // exit runs.
        h.state.open = false;
        h.pass();
        let mid = h.paint(4_000);
        assert_eq!(mid.rects.len(), 1, "still painted at the start of the exit");
        assert!(
            mid.alphas.is_empty(),
            "the exit clock just latched at full opacity, so nothing is fading yet"
        );
        assert_eq!(h.state.exited, 0);

        // Half-way through the exit it is dimmer but still there.
        let half = h.paint(4_000 + ANCHORED_EXIT.as_millis() as u64 / 2);
        assert_eq!(half.rects.len(), 1);
        assert_eq!(half.alphas.len(), 1, "now mid-fade, so layered");
        assert!(half.alphas[0] < 1.0);

        // Past the exit ramp it paints nothing at all…
        let after = h.paint(4_000 + ANCHORED_EXIT.as_millis() as u64 + 1);
        assert!(after.rects.is_empty(), "settled closed paints nothing");
        // …and the completion is reported on the next event pass, once, which
        // is the deferral the presence driver documents.
        assert_eq!(h.state.exited, 0, "not reportable from a paint");
        h.pointer(PointerPhase::Move, 5.0, 5.0);
        assert_eq!(h.state.exited, 1);
        h.pointer(PointerPhase::Move, 6.0, 6.0);
        assert_eq!(h.state.exited, 1, "the latch drains exactly once");
    }

    #[test]
    fn a_press_on_a_still_fading_panel_is_swallowed_and_one_past_it_is_not() {
        let mut h = Harness::new();
        h.paint(0);
        h.paint(4_000);
        h.state.open = false;
        h.pass();
        h.paint(4_000);
        let c = placed().center();
        assert_eq!(
            h.pointer(PointerPhase::Down, c.x, c.y),
            EventResult::Handled,
            "the fading panel absorbs the press rather than letting it fall through"
        );
        assert_eq!(h.state.dismissed, 0, "a closed host dismisses on nothing");

        // Once the exit has settled the host is input-transparent again.
        h.paint(4_000 + ANCHORED_EXIT.as_millis() as u64 + 1);
        assert_eq!(
            h.pointer(PointerPhase::Down, c.x, c.y),
            EventResult::Ignored
        );
    }

    #[test]
    fn reduce_motion_collapses_both_ramps_without_changing_the_bookkeeping() {
        let mut theme = crate::theme();
        theme.motion.reduce_motion = true;
        let mut h = Harness::with_theme(Some(theme));
        // The entrance is over on the frame it starts — fully open immediately,
        // so no fade layer either.
        let first = h.paint(0);
        assert!(first.alphas.is_empty());
        // …and so is the exit, which still reports itself exactly once.
        h.state.open = false;
        h.pass();
        let after = h.paint(1);
        assert!(after.rects.is_empty());
        h.pointer(PointerPhase::Move, 5.0, 5.0);
        assert_eq!(h.state.exited, 1);
    }

    #[test]
    fn the_layer_contains_a_content_painted_shadow_through_entry_and_exit() {
        let assert_shadow_contained = |rec: &Recorder| {
            assert_eq!(rec.layers.len(), 1, "still fading, so still layered");
            let layer = Rect::from_origin_size(rec.layers[0].0, rec.layers[0].1);
            // The host paints no chrome of its own: `rects[0]` is the
            // content's own fill, `rects[1]` the shadow it paints outside it.
            assert_eq!(rec.rects.len(), 2);
            let shadow = Rect::from_origin_size(rec.rects[1].0, rec.rects[1].1);
            assert!(
                layer.x0 - 1e-6 <= shadow.x0
                    && layer.y0 - 1e-6 <= shadow.y0
                    && layer.x1 + 1e-6 >= shadow.x1
                    && layer.y1 + 1e-6 >= shadow.y1,
                "layer {layer:?} clips the shadow {shadow:?}"
            );
        };

        // Entrance: progress 0 and mid-ramp.
        let mut h = Harness::shadowed();
        assert_shadow_contained(&h.paint(0));
        assert_shadow_contained(&h.paint(20));

        // Exit: latch the exit's own clock at rest, then read mid-ramp.
        h.paint(4_000);
        h.state.open = false;
        h.pass();
        h.paint(4_000);
        assert_shadow_contained(&h.paint(4_000 + ANCHORED_EXIT.as_millis() as u64 / 2));
    }

    #[test]
    fn a_mid_exit_press_on_the_panel_is_swallowed_even_when_the_content_declines_it() {
        let anchor = OverlayAnchor::new();
        anchor.set(ANCHOR);
        let mut root: RenderRoot<AppState, frust::StackView<AppState>> = RenderRoot::new();
        let mut state = AppState {
            open: true,
            ..AppState::default()
        };
        let mut tcx = TextContext::new();
        let mut pass = |root: &mut RenderRoot<AppState, frust::StackView<AppState>>,
                        state: &mut AppState| {
            let anchor = anchor.clone();
            let mut logic = move |s: &mut AppState| {
                frust::stack().child(
                    anchored(DecliningPanel(CONTENT))
                        .anchor(&anchor)
                        .open(s.open),
                )
            };
            root.rebuild(&mut logic, state);
            root.layout_with_text(WINDOW, &mut tcx as &mut dyn Any);
        };
        pass(&mut root, &mut state);
        root.paint(&mut Recorder::default(), FrameTime::from_nanos(0));
        root.paint(
            &mut Recorder::default(),
            FrameTime::from_nanos(4_000 * 1_000_000),
        );

        state.open = false;
        pass(&mut root, &mut state);
        root.paint(
            &mut Recorder::default(),
            FrameTime::from_nanos(4_000 * 1_000_000),
        );

        let c = placed().center();
        let down = |root: &mut RenderRoot<AppState, frust::StackView<AppState>>,
                    state: &mut AppState| {
            root.event(
                state,
                &InputEvent::Pointer(PointerEvent {
                    phase: PointerPhase::Down,
                    position: c,
                    button: PointerButton::Primary,
                }),
            )
            .handled
        };
        assert!(
            down(&mut root, &mut state),
            "the still-visible panel swallows the press, whatever the \
             declining content answers"
        );
    }

    #[test]
    fn a_closed_panels_content_capture_answer_survives_outside_its_rect() {
        let anchor = OverlayAnchor::new();
        anchor.set(ANCHOR);
        let mut root: RenderRoot<AppState, frust::StackView<AppState>> = RenderRoot::new();
        let mut state = AppState {
            open: true,
            ..AppState::default()
        };
        let mut tcx = TextContext::new();
        let mut pass = |root: &mut RenderRoot<AppState, frust::StackView<AppState>>,
                        state: &mut AppState| {
            let anchor = anchor.clone();
            let mut logic = move |s: &mut AppState| {
                frust::stack().child(
                    anchored(CapturingPanel(CONTENT))
                        .anchor(&anchor)
                        .open(s.open),
                )
            };
            root.rebuild(&mut logic, state);
            root.layout_with_text(WINDOW, &mut tcx as &mut dyn Any);
        };
        pass(&mut root, &mut state);
        root.paint(&mut Recorder::default(), FrameTime::from_nanos(0));
        root.paint(
            &mut Recorder::default(),
            FrameTime::from_nanos(4_000 * 1_000_000),
        );

        let down = |root: &mut RenderRoot<AppState, frust::StackView<AppState>>,
                    state: &mut AppState,
                    position: Point| {
            root.event(
                state,
                &InputEvent::Pointer(PointerEvent {
                    phase: PointerPhase::Down,
                    position,
                    button: PointerButton::Primary,
                }),
            )
            .handled
        };

        // A press inside the content while it is still open lets it capture
        // the pointer.
        let c = placed().center();
        assert!(down(&mut root, &mut state, c), "the open press is claimed");

        // Close the host, then press again while it is still visibly fading.
        state.open = false;
        pass(&mut root, &mut state);
        root.paint(
            &mut Recorder::default(),
            FrameTime::from_nanos(4_000 * 1_000_000),
        );

        // A press far outside `rect` — but the content still holds the
        // capture from the earlier press, so `route_event_single` still
        // forwards it and the content still consumes it. The closed-but-
        // visible host must not discard that answer just because the
        // position missed `rect`.
        let outside = Point::new(2.0, 2.0);
        assert!(
            !placed().contains(outside),
            "the probe position must actually be outside `rect`"
        );
        assert!(
            down(&mut root, &mut state, outside),
            "the content's own capture-held answer must survive outside `rect`"
        );
    }

    #[test]
    fn a_config_change_while_open_and_settled_does_not_restart_the_entrance() {
        let cell = OverlayAnchor::new();
        cell.set(ANCHOR);
        let view = anchored(Panel(CONTENT)).anchor(&cell);
        let mut counter = 0u64;
        let mut w = View::<AppState>::build(&view, &mut BuildCtx::new(&mut counter));
        let mut tcx = TextContext::new();
        let mut lctx = LayoutCtx::with_text_context(&mut tcx as &mut dyn Any);
        w.layout(&mut lctx, &BoxConstraints::tight(WINDOW));

        let paint = |w: &mut AnchoredOverlayWidget, ms: u64| {
            let mut ctx =
                PaintCtx::for_test(Point::ORIGIN, WINDOW, FrameTime::from_nanos(ms * 1_000_000));
            let mut rec = Recorder::default();
            w.paint(&mut ctx, &mut rec);
            rec
        };
        paint(&mut w, 0);
        let settled = paint(&mut w, 4_000);
        assert!(settled.alphas.is_empty(), "settled: no fade layer yet");
        assert_eq!(w.phase(), PresencePhase::Present);

        // A rebuild that forces the driver to be rebuilt (any ramp change
        // does — reduce_motion is only one trigger among several) while the
        // overlay is open and already settled.
        let changed = anchored(Panel(CONTENT))
            .anchor(&cell)
            .exit(Ramp::eased(Duration::from_millis(999), EASE_OUT));
        let mut counter2 = 1u64;
        View::<AppState>::rebuild(&changed, &view, &mut w, &mut BuildCtx::new(&mut counter2));
        w.layout(&mut lctx, &BoxConstraints::tight(WINDOW));

        let after = paint(&mut w, 4_100);
        assert_eq!(
            w.phase(),
            PresencePhase::Present,
            "landed back on `Present`, not replaying the entrance"
        );
        assert!(
            after.alphas.is_empty(),
            "still fully open, so still no layer"
        );
    }

    // ---- the trigger-side capture -----------------------------------------

    #[test]
    fn the_anchor_wrapper_captures_the_triggers_window_rect_at_paint() {
        let cell = OverlayAnchor::new();
        assert_eq!(cell.rect(), Rect::ZERO, "nothing captured before a paint");

        let mut root: RenderRoot<AppState, frust::PaddingView<AppState>> = RenderRoot::new();
        let mut state = AppState::default();
        let captured = cell.clone();
        // Offset by a padding so the captured rect proves it is absolute, not
        // parent-relative.
        let mut logic = move |_s: &mut AppState| {
            frust::Padding(
                frust::EdgeInsets::symmetric(24.0, 12.0),
                anchor(&captured, Panel(CONTENT)),
            )
        };
        root.rebuild(&mut logic, &mut state);
        let mut tcx = TextContext::new();
        root.layout_with_text(WINDOW, &mut tcx as &mut dyn Any);
        root.paint(&mut Recorder::default(), FrameTime::ZERO);

        assert_eq!(
            cell.rect(),
            Rect::from_origin_size(Point::new(24.0, 12.0), CONTENT)
        );
    }

    // ---- the floating host ------------------------------------------------

    /// A full-window leaf painted after a floating host, counting every press
    /// that reaches it — the main tree a floated panel must sit above.
    struct Cover;

    /// The retained half of [`Cover`].
    struct CoverWidget;

    impl View<AppState> for Cover {
        type Element = CoverWidget;
        fn build(&self, _ctx: &mut BuildCtx<'_>) -> CoverWidget {
            CoverWidget
        }
        fn rebuild(
            &self,
            _prev: &Self,
            _element: &mut CoverWidget,
            _ctx: &mut BuildCtx<'_>,
        ) -> ChangeFlags {
            ChangeFlags::NONE
        }
    }

    impl Widget for CoverWidget {
        fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
            bc.constrain(WINDOW)
        }
        fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
            scene.fill_rect(ctx.origin(), ctx.size(), Color::WHITE);
        }
        fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
            if matches!(event, InputEvent::Pointer(p) if p.phase == PointerPhase::Down) {
                ctx.state_mut::<AppState>().covered += 1;
                return EventResult::Handled;
            }
            EventResult::Ignored
        }
    }

    /// A floating host as the first child of a `Stack` whose later child is a
    /// full-window [`Cover`]: inline, the content would paint under the cover
    /// and the cover would take every press.
    struct FloatHarness {
        root: RenderRoot<AppState, frust::StackView<AppState>>,
        state: AppState,
        tcx: TextContext,
        anchor: OverlayAnchor,
    }

    impl FloatHarness {
        fn new() -> Self {
            let anchor = OverlayAnchor::new();
            anchor.set(ANCHOR);
            let mut h = FloatHarness {
                root: RenderRoot::new(),
                state: AppState {
                    open: true,
                    ..AppState::default()
                },
                tcx: TextContext::new(),
                anchor,
            };
            h.pass();
            h.paint(0);
            h
        }

        fn pass(&mut self) {
            let anchor = self.anchor.clone();
            let mut logic = move |s: &mut AppState| {
                frust::stack()
                    .child(
                        anchored(Panel(CONTENT))
                            .floating()
                            .anchor(&anchor)
                            .open(s.open)
                            .on_dismiss(|s: &mut AppState| {
                                s.dismissed += 1;
                                s.open = false;
                            })
                            .on_exited(|s: &mut AppState| s.exited += 1),
                    )
                    .child(Cover)
            };
            self.root.rebuild(&mut logic, &mut self.state);
            self.root
                .layout_with_text(WINDOW, &mut self.tcx as &mut dyn Any);
        }

        fn paint(&mut self, ms: u64) -> Recorder {
            let mut rec = Recorder::default();
            self.root
                .paint(&mut rec, FrameTime::from_nanos(ms * 1_000_000));
            rec
        }

        fn press(&mut self, at: Point) {
            for phase in [PointerPhase::Down, PointerPhase::Up] {
                self.root.event(
                    &mut self.state,
                    &InputEvent::Pointer(PointerEvent {
                        phase,
                        position: at,
                        button: PointerButton::Primary,
                    }),
                );
            }
        }

        fn escape(&mut self) {
            self.root.event(
                &mut self.state,
                &InputEvent::Key(KeyEvent {
                    key: Key::Named(NamedKey::Escape),
                    modifiers: Modifiers::default(),
                    repeat: false,
                }),
            );
        }

        /// The host's own laid-out bounds.
        fn host_bounds(&self) -> Rect {
            self.root
                .inspect()
                .into_iter()
                .find(|n| n.type_name.ends_with("AnchoredOverlayWidget"))
                .map(|n| n.bounds)
                .expect("the host is mounted")
        }
    }

    #[test]
    fn a_floating_host_takes_no_room_and_paints_its_content_above_the_main_tree() {
        let mut h = FloatHarness::new();
        assert_eq!(h.host_bounds().size(), Size::ZERO, "no room of its own");
        let rec = h.paint(5_000);
        assert_eq!(
            rec.rects,
            vec![(Point::ORIGIN, WINDOW), (placed().origin(), CONTENT)],
            "the cover first, then the floated content, placed against the anchor"
        );
    }

    #[test]
    fn a_floating_panel_takes_its_own_presses_ahead_of_the_main_tree() {
        let mut h = FloatHarness::new();
        h.press(placed().center());
        assert_eq!(h.state.pressed, 1, "the content saw the press");
        assert_eq!(h.state.covered, 0, "the cover under it did not");
        assert_eq!(h.state.dismissed, 0);
    }

    #[test]
    fn an_outside_press_dismisses_a_floating_panel_once_and_reaches_nothing_below() {
        let mut h = FloatHarness::new();
        h.press(Point::new(5.0, 5.0));
        assert_eq!(h.state.dismissed, 1);
        assert_eq!(h.state.covered, 0, "the light dismiss consumed the press");

        // Closed and settled, the panel is no longer registered: presses
        // reach the main tree again, and dismiss nothing.
        h.pass();
        h.paint(0);
        h.paint(5_000);
        // The next frame's rebuild carries the exit report: no pointer ever
        // reaches a host with no room of its own.
        h.pass();
        assert_eq!(h.state.exited, 1, "the settled exit was reported");
        h.press(Point::new(5.0, 5.0));
        assert_eq!(h.state.dismissed, 1);
        assert_eq!(h.state.covered, 1);
    }

    #[test]
    fn a_closing_floating_panel_still_swallows_a_press_on_itself() {
        let mut h = FloatHarness::new();
        h.paint(5_000);
        h.state.open = false;
        h.pass();
        // Mid-exit: still registered, outside taps off.
        h.paint(5_016);
        h.press(placed().center());
        assert_eq!(
            h.state.covered, 0,
            "a press on the fading panel is the panel's"
        );
        h.press(Point::new(5.0, 5.0));
        assert_eq!(h.state.covered, 1, "every other press reaches the page");
        assert_eq!(h.state.dismissed, 0, "a closing panel dismisses on nothing");
    }

    #[test]
    fn escape_after_a_press_on_a_floating_panel_dismisses_it() {
        let mut h = FloatHarness::new();
        h.press(placed().center());
        h.escape();
        assert_eq!(h.state.dismissed, 1, "the press focused the panel");
    }

    #[test]
    fn an_escape_nothing_focused_took_reaches_an_open_floating_panel() {
        let mut h = FloatHarness::new();
        h.escape();
        assert_eq!(h.state.dismissed, 1, "offered the declined Escape");
    }

    #[test]
    fn the_widget_reports_which_home_its_content_has() {
        let cell = OverlayAnchor::new();
        let mut counter = 0u64;
        let floating = anchored(Panel(CONTENT)).floating().floating().anchor(&cell);
        let w = View::<AppState>::build(&floating, &mut BuildCtx::new(&mut counter));
        assert!(w.is_floating() && w.is_open(), "floating is idempotent");
        let inline = anchored(Panel(CONTENT)).anchor(&cell);
        let w = View::<AppState>::build(&inline, &mut BuildCtx::new(&mut counter));
        assert!(!w.is_floating());
    }
}
