//! `Tester` runs the engine's own scene validation on every frame it paints: a view whose paint
//! the engine would refuse fails the test at the call that produced the frame, unless the options
//! opt in to collecting refused frames, and an ordinary view never produces one.
//!
//! Needs the facade's `test-support` feature (`cargo test -p frust-ui --features test-support`);
//! without it the file compiles to nothing.

#![cfg(feature = "test-support")]

use std::time::Duration;

use frust::testing::{Tester, TesterOptions};
use frust::{Component, View, component, text};
use frust_core::{BoxConstraints, BuildCtx, ChangeFlags, LayoutCtx, PaintCtx, PaintScene, Widget};
use kurbo::{Point, Size};
use peniko::Color;

const SIZE: Size = Size::new(200.0, 100.0);

/// A leaf that fills a rounded rectangle whose radius is infinite.
struct InfiniteRadius;

impl<S: 'static> View<S> for InfiniteRadius {
    type Element = InfiniteRadius;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> InfiniteRadius {
        InfiniteRadius
    }

    fn rebuild(
        &self,
        _prev: &Self,
        _element: &mut InfiniteRadius,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        ChangeFlags::NONE
    }
}

impl Widget for InfiniteRadius {
    fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        bc.constrain(Size::new(40.0, 40.0))
    }

    fn paint(&mut self, _ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        scene.fill_rounded_rect(
            Point::ZERO,
            Size::new(40.0, 40.0),
            f64::INFINITY,
            Color::from_rgb8(200, 30, 30),
        );
    }
}

struct Refused;

impl Component for Refused {
    type State = ();

    fn init(&self) {}

    fn build(&self, _state: &mut ()) -> impl View<()> {
        InfiniteRadius
    }
}

struct Ordinary;

impl Component for Ordinary {
    type State = ();

    fn init(&self) {}

    fn build(&self, _state: &mut ()) -> impl View<()> {
        text("hello")
    }
}

#[test]
#[should_panic(expected = "RoundedRect")]
fn a_refused_frame_fails_the_test_by_default() {
    let _tester = Tester::new((), |_: &mut ()| component(Refused), SIZE);
}

#[test]
fn allowing_refused_frames_collects_them_instead_of_panicking() {
    let options = TesterOptions::default().allow_refused_frames(true);
    let mut tester = Tester::with_options((), |_: &mut ()| component(Refused), SIZE, &options);
    assert_eq!(tester.refused_frames().len(), 1);
    tester.pump(Duration::from_millis(16));
    let frames = tester.refused_frames();
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].frame, 0);
    assert_eq!(frames[1].frame, 1);
    assert!(frames[0].error.to_string().contains("RoundedRect"));
}

#[test]
fn an_ordinary_view_has_no_refused_frames() {
    let options = TesterOptions::default().allow_refused_frames(true);
    let mut tester = Tester::with_options((), |_: &mut ()| component(Ordinary), SIZE, &options);
    tester.pump_and_settle();
    assert!(tester.refused_frames().is_empty());
}
