//! `clip`: the clip commands recorded around an overflowing child, the radius
//! resolution, pointer containment to the view's bounds, and the inspect
//! report — all through a recording scene and a render root, no GPU.

use frust_core::{
    BoxConstraints, BuildCtx, ChangeFlags, EventCtx, EventResult, FrameTime, InputEvent,
    InspectDetail, LayoutCtx, PaintCtx, PaintScene, PointerButton, PointerEvent, PointerPhase,
    RenderRoot, View, Widget,
};
use std::any::Any;

use frust_widgets::{Align, Alignment, SizedBox, clip};
use kurbo::{Point, Rect, Size};
use peniko::Color;

/// The clip view's box: smaller than the child it clips.
const BOX: Size = Size::new(50.0, 40.0);
/// The size the overflowing child insists on, whatever it is offered.
const CHILD: Size = Size::new(200.0, 200.0);

#[derive(Debug, PartialEq)]
enum Op {
    Clip(Point, Size),
    ClipRounded(Point, Size, f64),
    PopClip,
    Fill(Point, Size),
}

#[derive(Default)]
struct Rec(Vec<Op>);

impl PaintScene for Rec {
    fn fill_rect(&mut self, origin: Point, size: Size, _color: Color) {
        self.0.push(Op::Fill(origin, size));
    }
    fn draw_text(&mut self, _origin: Point, _text: &str) {}
    fn push_clip(&mut self, origin: Point, size: Size) {
        self.0.push(Op::Clip(origin, size));
    }
    fn push_clip_rounded(&mut self, origin: Point, size: Size, radius: f64) {
        self.0.push(Op::ClipRounded(origin, size, radius));
    }
    fn pop_clip(&mut self) {
        self.0.push(Op::PopClip);
    }
}

/// State counting the presses that reached the child.
#[derive(Default)]
struct Taps(u32);

/// A child that keeps its own size whatever it is offered and counts presses.
struct Overflowing;

struct OverflowingWidget;

impl View<Taps> for Overflowing {
    type Element = OverflowingWidget;
    fn build(&self, _ctx: &mut BuildCtx<'_>) -> OverflowingWidget {
        OverflowingWidget
    }
    fn rebuild(
        &self,
        _prev: &Self,
        _element: &mut OverflowingWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        ChangeFlags::NONE
    }
}

impl Widget for OverflowingWidget {
    fn layout(&mut self, _ctx: &mut LayoutCtx, _bc: &BoxConstraints) -> Size {
        CHILD
    }
    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        scene.fill_rect(ctx.origin(), ctx.size(), Color::BLACK);
    }
    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        if let InputEvent::Pointer(p) = event
            && p.phase == PointerPhase::Down
        {
            ctx.state_mut::<Taps>().0 += 1;
            return EventResult::Handled;
        }
        EventResult::Ignored
    }
}

/// The clip view pinned to `BOX` at the window's top-left, over the overflowing child.
fn view(radius: Option<f64>) -> impl View<Taps> {
    let clipped = clip(Overflowing);
    let clipped = match radius {
        Some(r) => clipped.radius(r),
        None => clipped,
    };
    Align(
        Alignment::TOP_LEFT,
        SizedBox(Some(BOX.width), Some(BOX.height)).child(clipped),
    )
}

fn root_for(radius: Option<f64>) -> RenderRoot<Taps, impl View<Taps>> {
    let mut root = RenderRoot::new();
    root.rebuild(&mut |_: &mut Taps| view(radius), &mut Taps::default());
    root.layout(Size::new(300.0, 300.0));
    root
}

fn paint<V: View<Taps>>(root: &mut RenderRoot<Taps, V>) -> Vec<Op> {
    let mut scene = Rec::default();
    root.paint(&mut scene, FrameTime::default());
    scene.0
}

fn press(x: f64, y: f64) -> InputEvent {
    InputEvent::Pointer(PointerEvent {
        phase: PointerPhase::Down,
        position: Point::new(x, y),
        button: PointerButton::Primary,
    })
}

#[test]
fn an_overflowing_child_is_painted_between_a_push_and_pop_of_the_views_bounds() {
    let mut root = root_for(None);
    assert_eq!(
        paint(&mut root),
        vec![
            Op::Clip(Point::ZERO, BOX),
            Op::Fill(Point::ZERO, CHILD),
            Op::PopClip,
        ]
    );
}

#[test]
fn an_infinite_radius_records_a_finite_rounded_clip_of_half_the_shorter_side() {
    let mut root = root_for(Some(f64::INFINITY));
    assert_eq!(
        paint(&mut root),
        vec![
            Op::ClipRounded(Point::ZERO, BOX, 20.0),
            Op::Fill(Point::ZERO, CHILD),
            Op::PopClip,
        ]
    );
}

#[test]
fn a_nan_or_negative_radius_clips_square() {
    for radius in [f64::NAN, -4.0] {
        let mut root = root_for(Some(radius));
        assert_eq!(paint(&mut root)[0], Op::Clip(Point::ZERO, BOX));
    }
}

#[test]
fn a_press_outside_the_bounds_does_not_reach_the_child_and_one_inside_does() {
    // Driven on the widget itself: a parent's own hit test would already
    // bound the press to the clip, so this isolates the clip's own rule.
    let view = clip(Overflowing);
    let mut counter = 0u64;
    let mut widget = View::<Taps>::build(&view, &mut BuildCtx::new(&mut counter));
    let size = widget.layout(&mut LayoutCtx::new(), &BoxConstraints::tight(BOX));
    assert_eq!(size, BOX);

    let mut taps = Taps::default();
    let mut dispatch = |x: f64, y: f64| {
        let state: &mut dyn Any = &mut taps;
        let mut ctx = EventCtx::new(state, Point::ZERO, BOX);
        widget.event(&mut ctx, &press(x, y))
    };

    // Inside the child's 200x200 box but outside the clip's 50x40 bounds.
    for (x, y) in [(100.0, 100.0), (60.0, 10.0), (10.0, 45.0)] {
        assert_eq!(dispatch(x, y), EventResult::Ignored, "({x}, {y})");
    }
    assert_eq!(dispatch(10.0, 10.0), EventResult::Handled);
    assert_eq!(taps.0, 1, "only the press inside reached the child");
}

#[test]
fn the_inspect_report_marks_the_clip_with_the_views_bounds() {
    let root = root_for(None);
    let nodes = root.inspect();
    let clips: Vec<_> = nodes
        .iter()
        .filter(|n| n.detail == Some(InspectDetail::Clip))
        .collect();
    assert_eq!(clips.len(), 1, "exactly the clip view reports a clip");
    assert_eq!(
        clips[0].bounds,
        Rect::from_origin_size(Point::ZERO, BOX),
        "the reported clip equals the view's bounds"
    );
}
