//! [`keep_focus(child)`](keep_focus): a wrapper whose pointer presses leave the focused control
//! focused.
//!
//! A press on an ordinary control ends the focus session of a text field elsewhere on the page,
//! and the soft keyboard goes with it. Wrapping the control keeps the session: the child still
//! receives the press and runs its tap handler, and the field keeps its caret, selection and IME
//! state.
//!
//! ```
//! use frust_core::{Component, View};
//! use frust_widgets::{button, keep_focus, row, text_input};
//!
//! struct SearchBar;
//!
//! impl Component for SearchBar {
//!     type State = String;
//!
//!     fn init(&self) -> String {
//!         String::new()
//!     }
//!
//!     fn build(&self, query: &mut String) -> impl View<String> {
//!         row()
//!             .child(text_input(query.clone(), |query: &mut String, value: String| {
//!                 *query = value;
//!             }))
//!             .child(keep_focus(button("Clear", |query: &mut String| query.clear())))
//!     }
//! }
//! # let _ = SearchBar;
//! ```
//!
//! Unlike [`overlay_portal`](crate::overlay_portal)`(..).preserve_focus(..)`, which decides
//! whether a floated surface's own presses move focus, this wraps any in-tree control.

use frust_core::{
    BoxConstraints, BuildCtx, ChangeFlags, ChildPod, EventCtx, EventResult, InputEvent, LayoutCtx,
    PaintCtx, PaintScene, PointerPhase, SemanticsCtx, View, Widget,
};
use std::marker::PhantomData;

use kurbo::{Point, Size};

/// A view that lets its child handle a press without ending the focus session. See the
/// [module docs](self).
pub struct KeepFocusView<State, V> {
    child: V,
    _state: PhantomData<fn(&mut State)>,
}

/// Wrap `child` so a pointer press landing inside it keeps the live focus session: the child still
/// receives the press, but the focused field keeps its session and the keyboard stays up. Layout,
/// paint and semantics are the child's own.
pub fn keep_focus<State: 'static, V: View<State>>(child: V) -> KeepFocusView<State, V> {
    KeepFocusView {
        child,
        _state: PhantomData,
    }
}

/// The retained widget for a [`KeepFocusView`].
pub struct KeepFocusWidget {
    child: ChildPod,
}

impl<State: 'static, V: View<State>> View<State> for KeepFocusView<State, V> {
    type Element = KeepFocusWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> KeepFocusWidget {
        KeepFocusWidget {
            child: crate::authoring::build_child(&self.child, ctx),
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut KeepFocusWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        crate::authoring::rebuild_child(&prev.child, &self.child, &mut element.child, ctx)
    }

    fn teardown(&self, element: &mut KeepFocusWidget, ctx: &mut BuildCtx<'_>) {
        crate::authoring::teardown_child(&self.child, &mut element.child, ctx);
    }
}

impl Widget for KeepFocusWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let size = self.child.layout_child(ctx, bc);
        self.child.set_origin(Point::ZERO);
        size
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        self.child.paint_child(ctx, scene);
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        if let InputEvent::Pointer(p) = event
            && p.phase == PointerPhase::Down
            && ctx.size().to_rect().contains(p.position)
        {
            ctx.keep_focus();
        }
        crate::authoring::route_event_single(&mut self.child, ctx, event)
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        self.child.semantics_child(ctx);
    }

    // Draws nothing of its own.
    fn hit_test_self(&self) -> bool {
        false
    }

    crate::authoring::visit_children!(child);
}
