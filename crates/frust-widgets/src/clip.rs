//! Clip view: [`clip(child)`](clip) contains a child's overflow to the view's
//! own bounds, optionally with rounded corners.
//!
//! # Layout
//!
//! [`ClipView`] is a transparent pass-through: the child receives the incoming
//! constraints and the view takes the child's size (clamped to those
//! constraints, so a child that returns more than it was offered overflows the
//! clip rather than growing it).
//!
//! # Paint
//!
//! Paint pushes a clip of the view's own bounds, paints the child, and pops the
//! clip. With [`radius`](ClipView::radius) the clip is rounded; the radii are
//! resolved against the view's size the way a container's corner radii are
//! (NaN or negative is square, anything above half the shorter side is half the
//! shorter side), and because the scene clip takes one radius the largest
//! resolved corner is used for all four.
//!
//! # Events, semantics, focus
//!
//! These pass through untouched, with one addition: a pointer press outside
//! the view's bounds does not reach the child, even where the child overflows.
//! A child already holding a pointer capture keeps receiving its events.
//!
//! # Example
//!
//! ```
//! use frust_core::{Component, View};
//! use frust_widgets::{clip, text};
//!
//! struct Banner;
//!
//! impl Component for Banner {
//!     type State = ();
//!
//!     fn init(&self) {}
//!
//!     fn build(&self, _state: &mut ()) -> impl View<()> {
//!         clip(text("a long headline that must not spill out of its box")).radius(12.0)
//!     }
//! }
//! # let _ = Banner;
//! ```

use std::marker::PhantomData;

use frust_core::{
    BoxConstraints, BuildCtx, ChangeFlags, ChildPod, CornerRadii, EventCtx, EventResult,
    InputEvent, InspectDetail, LayoutCtx, PaintCtx, PaintScene, SemanticsCtx, View, Widget,
};
use kurbo::{Point, Rect, Size};

use crate::container::resolve_corner_radii;

/// A declarative single-child clip. See the [module docs](self).
pub struct ClipView<State, V> {
    child: V,
    radius: CornerRadii,
    _state: PhantomData<fn(&mut State)>,
}

/// Clip `child` to the view's own bounds — see the [module docs](self).
pub fn clip<State: 'static, V: View<State>>(child: V) -> ClipView<State, V> {
    ClipView {
        child,
        radius: CornerRadii::uniform(0.0),
        _state: PhantomData,
    }
}

impl<State, V> ClipView<State, V> {
    /// Round the clip's corners. A single `f64` applies to all four; NaN or a
    /// negative value is square, and `f64::INFINITY` (or any value above half
    /// the shorter side) is half the shorter side, a capsule or circle.
    pub fn radius(mut self, radius: impl Into<CornerRadii>) -> Self {
        self.radius = radius.into();
        self
    }
}

/// The retained widget for a [`ClipView`].
pub struct ClipWidget {
    child: ChildPod,
    radius: CornerRadii,
    size: Size,
}

impl<State: 'static, V: View<State>> View<State> for ClipView<State, V> {
    type Element = ClipWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> ClipWidget {
        ClipWidget {
            child: crate::authoring::build_child(&self.child, ctx),
            radius: self.radius,
            size: Size::ZERO,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut ClipWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = ChangeFlags::NONE;
        if prev.radius != self.radius {
            element.radius = self.radius;
            flags |= ChangeFlags::PAINT;
        }
        flags |= crate::authoring::rebuild_child(&prev.child, &self.child, &mut element.child, ctx);
        flags
    }

    fn teardown(&self, element: &mut ClipWidget, ctx: &mut BuildCtx<'_>) {
        crate::authoring::teardown_child(&self.child, &mut element.child, ctx);
    }
}

impl Widget for ClipWidget {
    /// [`InspectDetail::Clip`]: the view clips its content to its box.
    fn inspect_detail(&self) -> Option<InspectDetail> {
        Some(InspectDetail::Clip)
    }

    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let child = self.child.layout_child(ctx, bc);
        self.child.set_origin(Point::ZERO);
        self.size = bc.constrain(child);
        self.size
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let origin = ctx.origin();
        let size = ctx.size();
        let radius = resolve_corner_radii(self.radius, size).largest();
        if radius > 0.0 {
            scene.push_clip_rounded(origin, size, radius);
        } else {
            scene.push_clip(origin, size);
        }
        self.child.paint_child(ctx, scene);
        scene.pop_clip();
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        // Only a fresh pointer contact is bounded by the clip: broadcasts,
        // focus-routed events and a captured child's own stream pass through.
        if !event.is_broadcast()
            && !event.is_focus_routed()
            && !self.child.is_active()
            && !Rect::from_origin_size(Point::ZERO, self.size).contains(event.position())
        {
            return EventResult::Ignored;
        }
        crate::authoring::route_event_single(&mut self.child, ctx, event)
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        self.child.semantics_child(ctx);
    }

    // Paints no content of its own, only a clip around the child's.
    fn hit_test_self(&self) -> bool {
        false
    }

    crate::authoring::visit_children!(child);
}
