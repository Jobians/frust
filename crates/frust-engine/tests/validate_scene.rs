//! `validate_scene` runs the up-front refusals `compile` runs, with no strips
//! and no GPU, and names the command it refuses.

use frust_engine::filters::{MAX_BACKDROPS_PER_FRAME, max_backdrops_per_frame};
use frust_engine::{EngineError, SceneCompiler, validate_scene};
use frust_scene::{Scene, SceneBuilder};
use kurbo::{Affine, BezPath, Point, Rect};
use peniko::Brush;
use peniko::color::palette::css::RED;

const VIEWPORT: (u16, u16) = (200, 200);

fn scene_of(record: impl FnOnce(&mut SceneBuilder<'_>)) -> Scene {
    let mut scene = Scene::new();
    let mut builder = SceneBuilder::new(&mut scene);
    record(&mut builder);
    scene
}

fn red() -> Brush {
    Brush::Solid(RED)
}

fn compile_error(scene: &Scene, root: Affine) -> Option<EngineError> {
    SceneCompiler::new(VIEWPORT.0, VIEWPORT.1)
        .compile(scene, root, VIEWPORT)
        .err()
}

#[test]
fn an_infinite_corner_radius_names_the_command_and_its_kind() {
    let scene = scene_of(|b| {
        b.fill_rect(Rect::new(0.0, 0.0, 10.0, 10.0), red());
        b.fill_rounded_rect(Rect::new(0.0, 0.0, 40.0, 40.0), f64::INFINITY, red());
    });

    assert!(matches!(
        validate_scene(&scene, Affine::IDENTITY, VIEWPORT),
        Err(EngineError::InvalidGeometry {
            command: Some(1),
            kind: "RoundedRect"
        })
    ));
}

#[test]
fn a_finite_scene_validates() {
    let scene = scene_of(|b| b.fill_rect(Rect::new(0.0, 0.0, 40.0, 40.0), red()));

    assert!(validate_scene(&scene, Affine::IDENTITY, VIEWPORT).is_ok());
    assert!(compile_error(&scene, Affine::IDENTITY).is_none());
}

#[test]
fn validation_and_compile_agree_on_a_nan_transform() {
    let scene = scene_of(|b| {
        b.push_transform(Affine::scale(f64::NAN));
        b.fill_rect(Rect::new(0.0, 0.0, 40.0, 40.0), red());
        b.pop_transform();
    });

    let validated = validate_scene(&scene, Affine::IDENTITY, VIEWPORT).unwrap_err();
    let compiled = compile_error(&scene, Affine::IDENTITY).expect("compile refuses");
    assert!(matches!(
        validated,
        EngineError::InvalidTransform {
            command: Some(_),
            kind: "FillRect"
        }
    ));
    assert_eq!(validated.to_string(), compiled.to_string());
}

#[test]
fn validation_and_compile_agree_on_a_non_finite_path_point() {
    let mut path = BezPath::new();
    path.move_to(Point::new(0.0, 0.0));
    path.line_to(Point::new(f64::NAN, 10.0));
    path.line_to(Point::new(10.0, 10.0));
    path.close_path();
    let scene = scene_of(|b| b.fill_path(path, red()));

    let validated = validate_scene(&scene, Affine::IDENTITY, VIEWPORT).unwrap_err();
    let compiled = compile_error(&scene, Affine::IDENTITY).expect("compile refuses");
    assert!(matches!(
        validated,
        EngineError::InvalidGeometry {
            command: Some(0),
            kind: "Path"
        }
    ));
    assert_eq!(validated.to_string(), compiled.to_string());
}

#[test]
fn the_display_names_command_and_kind() {
    let error = EngineError::InvalidGeometry {
        command: Some(41),
        kind: "RoundedRect",
    };
    assert_eq!(
        error.to_string(),
        "non-finite geometry refused (command 41, RoundedRect)"
    );
}

#[test]
fn an_unknown_command_index_is_not_printed_as_zero() {
    let error = EngineError::InvalidGeometry {
        command: None,
        kind: "PushLayer",
    };
    assert_eq!(error.to_string(), "non-finite geometry refused (PushLayer)");
}

/// `count` backdrops over `rect` at one sigma, each on its own row.
fn backdrops(count: usize, x_offset: f64) -> Scene {
    scene_of(|b| {
        for i in 0..count {
            let y = 10.0 + 20.0 * i as f64;
            b.backdrop_blur(
                Rect::new(x_offset + 10.0, y, x_offset + 50.0, y + 15.0),
                2.0,
            );
        }
    })
}

/// These tests rely on the default cap; nothing in this binary changes the
/// process-wide cap, so it cannot race another test.
fn default_cap() -> usize {
    assert_eq!(max_backdrops_per_frame(), MAX_BACKDROPS_PER_FRAME);
    MAX_BACKDROPS_PER_FRAME
}

#[test]
fn more_on_target_backdrops_than_the_cap_are_refused_by_both() {
    let scene = backdrops(default_cap() + 1, 0.0);

    let validated = validate_scene(&scene, Affine::IDENTITY, VIEWPORT).unwrap_err();
    let compiled = compile_error(&scene, Affine::IDENTITY).expect("compile refuses");
    assert!(matches!(
        validated,
        EngineError::TooManyBackdrops { recorded, max }
            if recorded == MAX_BACKDROPS_PER_FRAME + 1 && max == MAX_BACKDROPS_PER_FRAME
    ));
    assert_eq!(validated.to_string(), compiled.to_string());
}

#[test]
fn the_same_backdrops_moved_off_the_target_are_accepted_by_both() {
    let scene = backdrops(default_cap() + 1, 1000.0);

    assert!(validate_scene(&scene, Affine::IDENTITY, VIEWPORT).is_ok());
    assert!(compile_error(&scene, Affine::IDENTITY).is_none());
}

#[test]
fn backdrops_at_the_cap_are_accepted_by_both() {
    let scene = backdrops(default_cap(), 0.0);

    assert!(validate_scene(&scene, Affine::IDENTITY, VIEWPORT).is_ok());
    assert!(compile_error(&scene, Affine::IDENTITY).is_none());
}

#[test]
fn a_draw_retained_is_not_refused_by_validation() {
    let scene = scene_of(|b| {
        b.draw_retained(7, Rect::new(0.0, 0.0, 40.0, 40.0), 1.0, 1.0);
    });

    assert!(validate_scene(&scene, Affine::IDENTITY, VIEWPORT).is_ok());
}

#[test]
fn validation_and_compile_agree_over_every_refusal_case() {
    let mut nan_path = BezPath::new();
    nan_path.move_to(Point::new(0.0, 0.0));
    nan_path.line_to(Point::new(f64::NAN, 10.0));
    nan_path.close_path();

    let nan_transform = scene_of(|b| {
        b.push_transform(Affine::scale(f64::NAN));
        b.fill_rect(Rect::new(0.0, 0.0, 40.0, 40.0), red());
        b.pop_transform();
    });
    let infinite_radius = scene_of(|b| {
        b.fill_rounded_rect(Rect::new(0.0, 0.0, 40.0, 40.0), f64::INFINITY, red());
    });
    let non_finite_path = scene_of(|b| b.fill_path(nan_path, red()));
    let finite = scene_of(|b| b.fill_rect(Rect::new(0.0, 0.0, 40.0, 40.0), red()));

    let cases: Vec<(&str, Scene, Affine)> = vec![
        ("finite", finite, Affine::IDENTITY),
        ("nan transform", nan_transform, Affine::IDENTITY),
        ("infinite radius", infinite_radius, Affine::IDENTITY),
        ("non-finite path", non_finite_path, Affine::IDENTITY),
        (
            "non-finite root",
            scene_of(|b| b.fill_rect(Rect::new(0.0, 0.0, 40.0, 40.0), red())),
            Affine::scale(f64::NAN),
        ),
        (
            "too many backdrops",
            backdrops(default_cap() + 1, 0.0),
            Affine::IDENTITY,
        ),
        (
            "backdrops off target",
            backdrops(default_cap() + 1, 1000.0),
            Affine::IDENTITY,
        ),
    ];

    for (name, scene, root) in cases {
        let validated = validate_scene(&scene, root, VIEWPORT).err();
        let compiled = compile_error(&scene, root);
        assert_eq!(
            validated.as_ref().map(ToString::to_string),
            compiled.as_ref().map(ToString::to_string),
            "{name}: validate_scene and compile disagree"
        );
    }

    let too_large = (u16::MAX, u16::MAX);
    let scene = scene_of(|b| b.fill_rect(Rect::new(0.0, 0.0, 4.0, 4.0), red()));
    let validated = validate_scene(&scene, Affine::IDENTITY, too_large).unwrap_err();
    let compiled = SceneCompiler::new(64, 64)
        .compile(&scene, Affine::IDENTITY, too_large)
        .unwrap_err();
    assert_eq!(validated.to_string(), compiled.to_string());
}
