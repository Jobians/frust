//! `validate_scene` runs the up-front refusals `compile` runs, with no strips
//! and no GPU, and names the command it refuses.

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
            command: 1,
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
            command: 0,
            kind: "Path"
        }
    ));
    assert_eq!(validated.to_string(), compiled.to_string());
}

#[test]
fn the_display_names_command_and_kind() {
    let error = EngineError::InvalidGeometry {
        command: 41,
        kind: "RoundedRect",
    };
    assert_eq!(
        error.to_string(),
        "non-finite geometry refused (command 41, RoundedRect)"
    );
}
