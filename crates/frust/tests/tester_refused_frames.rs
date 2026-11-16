//! `Tester` runs the engine's up-front scene validation on every frame it paints: a view whose paint
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
use kurbo::{Affine, Point, Size};
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

/// A leaf that records `glass` backdrop blurs over the window, and a non-finite transform around
/// a fill when `broken_transform` is set.
struct Probe {
    glass: usize,
    broken_transform: bool,
}

struct ProbeView {
    glass: usize,
    broken_transform: bool,
}

impl<S: 'static> View<S> for ProbeView {
    type Element = Probe;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> Probe {
        Probe {
            glass: self.glass,
            broken_transform: self.broken_transform,
        }
    }

    fn rebuild(&self, _prev: &Self, element: &mut Probe, _ctx: &mut BuildCtx<'_>) -> ChangeFlags {
        element.glass = self.glass;
        element.broken_transform = self.broken_transform;
        ChangeFlags::PAINT
    }
}

impl Widget for Probe {
    fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        bc.constrain(SIZE)
    }

    fn paint(&mut self, _ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        for _ in 0..self.glass {
            scene.backdrop_blur(Point::new(10.0, 10.0), Size::new(80.0, 40.0), 4.0);
        }
        if self.broken_transform {
            scene.push_transform(Affine::scale(f64::NAN));
            scene.fill_rect(
                Point::ZERO,
                Size::new(10.0, 10.0),
                Color::from_rgb8(30, 30, 200),
            );
            scene.pop_transform();
        }
    }
}

struct ProbeScreen {
    glass: usize,
    broken_transform: bool,
}

impl Component for ProbeScreen {
    type State = ();

    fn init(&self) {}

    fn build(&self, _state: &mut ()) -> impl View<()> {
        ProbeView {
            glass: self.glass,
            broken_transform: self.broken_transform,
        }
    }
}

/// A Tester whose view records `*state` glass surfaces.
fn glass_tester(glass: usize, options: &TesterOptions) -> Tester<usize, impl View<usize>> {
    Tester::with_options(
        glass,
        |count: &mut usize| {
            component(ProbeScreen {
                glass: *count,
                broken_transform: false,
            })
        },
        SIZE,
        options,
    )
}

#[test]
#[should_panic(expected = "backdrop")]
fn a_view_that_becomes_refused_after_a_state_change_fails_at_pump() {
    let mut tester = glass_tester(1, &TesterOptions::default());
    tester.pump(Duration::from_millis(16));
    *tester.state_mut() = 9;
    tester.pump(Duration::from_millis(16));
}

#[test]
#[should_panic(expected = "backdrop")]
fn a_view_that_becomes_refused_after_a_state_change_fails_at_scene() {
    let mut tester = glass_tester(1, &TesterOptions::default());
    *tester.state_mut() = 9;
    let _scene = tester.scene();
}

#[test]
fn more_glass_surfaces_than_the_default_cap_are_refused_with_the_backdrop_error() {
    let options = TesterOptions::default().allow_refused_frames(true);
    let tester = glass_tester(5, &options);
    let frames = tester.refused_frames();
    assert_eq!(frames.len(), 1);
    assert!(
        matches!(
            frames[0].error,
            frust_render::EngineError::TooManyBackdrops { recorded: 5, .. }
        ),
        "{:?}",
        frames[0].error
    );
}

#[test]
fn glass_surfaces_within_the_default_cap_are_accepted() {
    let options = TesterOptions::default().allow_refused_frames(true);
    let tester = glass_tester(4, &options);
    assert!(tester.refused_frames().is_empty());
}

#[test]
fn glass_is_judged_at_the_physical_extent_too() {
    let options = TesterOptions::default()
        .scale(2.0)
        .allow_refused_frames(true);
    let tester = glass_tester(5, &options);
    assert_eq!(tester.refused_frames().len(), 1);
}

#[test]
fn a_widget_that_pushes_a_non_finite_transform_is_refused() {
    let options = TesterOptions::default().allow_refused_frames(true);
    let tester = Tester::with_options(
        (),
        |_: &mut ()| {
            component(ProbeScreen {
                glass: 0,
                broken_transform: true,
            })
        },
        SIZE,
        &options,
    );
    let frames = tester.refused_frames();
    assert_eq!(frames.len(), 1);
    assert!(
        frames[0].error.to_string().contains("transform"),
        "{}",
        frames[0].error
    );
}
