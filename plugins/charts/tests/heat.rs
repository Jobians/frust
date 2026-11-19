//! `heat_cells` headless: a real `RenderRoot` painting into a recording scene.
//!
//! Writing a `Samples` handle requests a process-wide repaint, so every test holds `STATICS`.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use frust::authoring::{
    ChangeFlags, EventOutcome, InputEvent, PaintScene, Point, PointerButton, PointerEvent,
    PointerPhase, Role, Size,
};
use frust::{Color, FrameTime};
use frust_charts::{Samples, Tone, heat_cells};
use frust_core::RenderRoot;
use frust_reactive::ReactiveRuntime;

static STATICS: Mutex<()> = Mutex::new(());

fn hold() -> MutexGuard<'static, ()> {
    STATICS.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone, Copy, Debug)]
struct Cell {
    origin: Point,
    size: Size,
    radius: f64,
    color: Color,
}

#[derive(Default)]
struct Rec {
    cells: Vec<Cell>,
}

impl PaintScene for Rec {
    fn fill_rect(&mut self, _origin: Point, _size: Size, _color: Color) {}
    fn fill_rounded_rect(&mut self, origin: Point, size: Size, radius: f64, color: Color) {
        self.cells.push(Cell {
            origin,
            size,
            radius,
            color,
        });
    }
    fn draw_text(&mut self, _origin: Point, _text: &str) {}
}

type View = frust_charts::HeatCellsView<()>;

struct Rig {
    rt: &'static ReactiveRuntime,
    root: RenderRoot<(), View>,
}

impl Rig {
    fn new() -> Self {
        Self {
            rt: ReactiveRuntime::init(Arc::new(|| {})),
            root: RenderRoot::new(),
        }
    }

    fn rebuild(&mut self, view: impl Fn() -> View) -> ChangeFlags {
        let mut logic = |_: &mut ()| view();
        self.rt
            .with_owner(|| self.root.rebuild(&mut logic, &mut ()))
    }

    fn paint(&mut self, width: f64, height: f64) -> Vec<Cell> {
        self.root.layout(Size::new(width, height));
        let mut rec = Rec::default();
        self.root
            .paint(&mut rec, FrameTime::from_nanos(1_000_000_000));
        rec.cells
    }
}

fn rig(view: impl Fn() -> View) -> Rig {
    let mut rig = Rig::new();
    rig.rebuild(view);
    rig
}

fn alpha(color: Color) -> f32 {
    color.components[3]
}

fn rgb(color: Color) -> [f32; 3] {
    [
        color.components[0],
        color.components[1],
        color.components[2],
    ]
}

const RED: Color = Color::from_rgb8(0xd0, 0x10, 0x10);

#[test]
fn defaults_lay_out_64_cells_on_whole_pixels() {
    let _g = hold();
    let samples = Samples::new(64);
    let mut rig = rig(|| heat_cells(samples.clone()));
    let cells = rig.paint(400.0, 300.0);
    assert_eq!(cells.len(), 64);

    // floor((400 + 2) / (8 + 2)) = 40 columns, 2 rows.
    let first_row: Vec<_> = cells.iter().take_while(|c| c.origin.y == 0.0).collect();
    assert_eq!(first_row.len(), 40);
    assert_eq!(cells[40].origin, Point::new(0.0, 20.0));
    assert_eq!(cells[63].origin, Point::new(23.0 * 10.0, 20.0));

    for cell in &cells {
        assert_eq!(cell.size, Size::new(8.0, 18.0));
        assert_eq!(cell.radius, 2.0);
        assert_eq!(cell.origin.x.fract(), 0.0);
        assert_eq!(cell.origin.y.fract(), 0.0);
        assert!(cell.origin.x + cell.size.width <= 400.0);
        assert!(cell.origin.y + cell.size.height <= 300.0);
    }
    // Uniform gaps: horizontal 2, vertical 2.
    for pair in cells[..40].windows(2) {
        assert_eq!(pair[1].origin.x - (pair[0].origin.x + 8.0), 2.0);
    }
    assert_eq!(cells[40].origin.y - (cells[0].origin.y + 18.0), 2.0);
    // The exact extent is 398 x 38.
    let right = cells
        .iter()
        .map(|c| c.origin.x + c.size.width)
        .fold(0.0, f64::max);
    let bottom = cells
        .iter()
        .map(|c| c.origin.y + c.size.height)
        .fold(0.0, f64::max);
    assert_eq!((right, bottom), (398.0, 38.0));
}

#[test]
fn explicit_columns_and_cell_geometry() {
    let _g = hold();
    let samples = Samples::new(10);
    let mut rig = rig(|| {
        heat_cells(samples.clone())
            .columns(4)
            .cell(10.0, 10.0)
            .gap(5.0)
            .radius(3.0)
    });
    let cells = rig.paint(400.0, 300.0);
    assert_eq!(cells.len(), 10);
    assert_eq!(cells[3].origin, Point::new(45.0, 0.0));
    assert_eq!(cells[4].origin, Point::new(0.0, 15.0));
    assert_eq!(cells[9].origin, Point::new(15.0, 30.0));
    assert!(cells.iter().all(|c| c.radius == 3.0));
}

#[test]
fn intensity_runs_from_min_to_full_across_the_range() {
    let _g = hold();
    let samples = Samples::new(5);
    for (i, v) in [0.0, 50.0, 100.0, -20.0, 400.0].into_iter().enumerate() {
        samples.set(i, v);
    }
    let mut rig = rig(|| heat_cells(samples.clone()).color(RED));
    let cells = rig.paint(400.0, 100.0);
    let a: Vec<f32> = cells.iter().map(|c| alpha(c.color)).collect();
    let near = |x: f32, y: f32| (x - y).abs() < 1e-5;
    assert!(near(a[0], 0.25), "{a:?}");
    assert!(near(a[1], 0.625), "{a:?}");
    assert!(near(a[2], 1.0), "{a:?}");
    assert!(near(a[3], 0.25) && near(a[4], 1.0), "{a:?}");
    assert_eq!(rgb(cells[0].color), rgb(RED));

    // A custom range and minimum intensity.
    let mut rig = self::rig(|| {
        heat_cells(samples.clone())
            .color(RED)
            .range(0.0, 200.0)
            .min_intensity(0.5)
    });
    let a: Vec<f32> = rig
        .paint(400.0, 100.0)
        .iter()
        .map(|c| alpha(c.color))
        .collect();
    assert!(
        near(a[0], 0.5) && near(a[1], 0.625) && near(a[2], 0.75),
        "{a:?}"
    );
}

#[test]
fn tone_picks_the_colour_at_each_boundary() {
    let _g = hold();
    let (calm, busy, critical) = (
        Color::from_rgb8(0, 200, 0),
        Color::from_rgb8(220, 160, 0),
        Color::from_rgb8(200, 0, 0),
    );
    let samples = Samples::new(4);
    for (i, v) in [59.9, 60.0, 84.9, 85.0].into_iter().enumerate() {
        samples.set(i, v);
    }
    let mut rig = rig(|| heat_cells(samples.clone()).tone(Tone::load(calm, busy, critical)));
    let cells = rig.paint(400.0, 100.0);
    let got: Vec<[f32; 3]> = cells.iter().map(|c| rgb(c.color)).collect();
    let want = [calm, busy, busy, critical].map(rgb);
    assert_eq!(got, want);
}

#[test]
fn a_gap_draws_the_empty_colour_at_minimum_intensity() {
    let _g = hold();
    let grey = Color::from_rgb8(0x40, 0x44, 0x50);
    let samples = Samples::new(2);
    samples.set(1, 100.0);
    let mut rig = rig(|| heat_cells(samples.clone()).color(RED).empty(grey));
    let cells = rig.paint(400.0, 100.0);
    assert_eq!(rgb(cells[0].color), rgb(grey));
    assert!((alpha(cells[0].color) - 0.25).abs() < 1e-5);
    assert_eq!(rgb(cells[1].color), rgb(RED));
}

#[test]
fn a_write_changes_exactly_that_cell_without_a_rebuild() {
    let _g = hold();
    let samples = Samples::new(64);
    for i in 0..64 {
        samples.set(i, 10.0);
    }
    let mut rig = rig(|| heat_cells(samples.clone()).color(RED));
    let before = rig.paint(400.0, 300.0);
    samples.set(17, 90.0);
    let after = rig.paint(400.0, 300.0);
    assert_eq!(before.len(), after.len());
    let changed: Vec<usize> = (0..64)
        .filter(|&i| before[i].color != after[i].color)
        .collect();
    assert_eq!(changed, [17]);
    assert!(alpha(after[17].color) > alpha(before[17].color));
    assert!((0..64).all(|i| before[i].origin == after[i].origin));
}

#[test]
fn rebuild_flags_follow_what_changed() {
    let _g = hold();
    let samples = Samples::new(8);
    let base = || heat_cells(samples.clone()).color(RED);
    let mut rig = rig(base);

    assert_eq!(rig.rebuild(base), ChangeFlags::NONE);

    let paint_only: [&dyn Fn() -> View; 5] = [
        &|| heat_cells(samples.clone()).color(Color::BLACK),
        &|| heat_cells(samples.clone()).color(RED).range(0.0, 10.0),
        &|| heat_cells(samples.clone()).color(RED).min_intensity(0.5),
        &|| heat_cells(samples.clone()).color(RED).tone(Tone::new(RED)),
        &|| heat_cells(Samples::new(8)).color(RED),
    ];
    for next in paint_only {
        let flags = rig.rebuild(next);
        assert!(flags.contains(ChangeFlags::PAINT), "{flags:?}");
        assert!(!flags.contains(ChangeFlags::LAYOUT), "{flags:?}");
        rig.rebuild(base);
    }

    let layout: [&dyn Fn() -> View; 3] = [
        &|| heat_cells(samples.clone()).color(RED).cell(9.0, 18.0),
        &|| heat_cells(samples.clone()).color(RED).gap(3.0),
        &|| heat_cells(samples.clone()).color(RED).columns(2),
    ];
    for next in layout {
        let flags = rig.rebuild(next);
        assert!(
            flags.contains(ChangeFlags::LAYOUT | ChangeFlags::PAINT),
            "{flags:?}"
        );
        rig.rebuild(base);
    }
}

#[test]
fn input_is_ignored() {
    let _g = hold();
    let samples = Samples::new(8);
    let mut rig = rig(|| heat_cells(samples.clone()));
    rig.paint(400.0, 100.0);
    let down = InputEvent::Pointer(PointerEvent {
        phase: PointerPhase::Down,
        position: Point::new(4.0, 4.0),
        button: PointerButton::Primary,
    });
    let outcome: EventOutcome = rig.root.event(&mut (), &down);
    assert!(!outcome.handled);
}

#[test]
fn semantics_is_one_image_node_only_with_a_label() {
    let _g = hold();
    let samples = Samples::new(8);
    let images = |rig: &Rig| {
        rig.root
            .semantics()
            .nodes
            .iter()
            .filter(|(_, n)| n.role() == Role::Image)
            .count()
    };
    let mut plain = rig(|| heat_cells(samples.clone()));
    plain.paint(400.0, 100.0);
    assert_eq!(images(&plain), 0);

    let mut labelled = rig(|| heat_cells(samples.clone()).semantics("CPU cores"));
    labelled.paint(400.0, 100.0);
    assert_eq!(images(&labelled), 1);
    let update = labelled.root.semantics();
    let node = update
        .nodes
        .iter()
        .find(|(_, n)| n.role() == Role::Image)
        .unwrap();
    assert_eq!(node.1.label(), Some("CPU cores"));
}

#[test]
fn degenerate_inputs_do_not_panic() {
    let _g = hold();
    let mut empty = rig(|| heat_cells(Samples::new(0)));
    assert!(empty.paint(400.0, 100.0).is_empty());

    let samples = Samples::new(64);
    let mut rig = rig(|| heat_cells(samples.clone()));
    let cells = rig.paint(0.0, 0.0);
    assert!(cells.len() <= 64);
    assert!(
        cells
            .iter()
            .all(|c| c.origin.x + c.size.width <= 0.0 + 1e-6)
    );

    let mut zero_cell = self::rig(|| {
        heat_cells(samples.clone())
            .cell(0.0, 0.0)
            .gap(0.0)
            .columns(0)
    });
    zero_cell.paint(100.0, 100.0);
}
