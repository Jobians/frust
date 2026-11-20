//! CPU raster goldens for the chart families (bar, pie, gauge, scatter, radar, candlestick), run
//! through the repository's central golden corpus.
//!
//! ```text
//! cargo test -p frust-charts --test goldens_families
//! UPDATE_GOLDENS=1 cargo test -p frust-charts --test goldens_families
//! ```
//!
//! Baselines land in `testing/goldens/cpu/` under `charts-*` stems. Every case is text-free (the
//! runner rejects glyph runs from any face but its bundled ones): the bar charts hide their
//! category labels and draw no value axis, and the radar chart draws no titles. Data is
//! hard-coded (no randomness, no clock) and every colour is explicit on a dark base, so the
//! result does not depend on theme defaults.
//!
//! Writing a data handle requests a process-wide repaint, so the tests that record the scenes
//! hold one static mutex.

use std::f32::consts::PI;
use std::sync::{Mutex, MutexGuard, PoisonError};

use frust::authoring::Size;
use frust::{Color, FrameTime, Theme, View};
use frust_charts::{
    BarMode, Candle, Candles, Categories, GaugeCap, NeedleStyle, Points, RadarShape, RadarStyle,
    ScatterPoint, YDomain, bar_chart, candlestick_chart, gauge, pie_chart, radar_chart,
    scatter_chart,
};
use frust_testing::case::Tolerance;
use frust_testing::corpus::CorpusCase;
use frust_testing::frame::{
    FrameSpec, foreign_font_runs, physical_case, record_view, test_text_context,
};
use frust_testing::{Scene, run_cpu_goldens};

static STATICS: Mutex<()> = Mutex::new(());

fn hold() -> MutexGuard<'static, ()> {
    STATICS.lock().unwrap_or_else(PoisonError::into_inner)
}

const BASE: Color = Color::from_rgb8(0x12, 0x14, 0x1a);
const TRACK: Color = Color::from_rgb8(0x2a, 0x2f, 0x38);
const GRID: Color = Color::from_rgb8(0x2f, 0x35, 0x40);
const WEB: Color = Color::from_rgb8(0x44, 0x4b, 0x58);
const BLUE: Color = Color::from_rgb8(0x4a, 0x90, 0xe2);
const VIOLET: Color = Color::from_rgb8(0xb0, 0x7c, 0xe8);
const CALM: Color = Color::from_rgb8(0x2e, 0x9e, 0x5b);
const BUSY: Color = Color::from_rgb8(0xe0, 0xa3, 0x1a);
const CRITICAL: Color = Color::from_rgb8(0xd6, 0x3b, 0x3b);
const NEEDLE: Color = Color::from_rgb8(0xe6, 0xe8, 0xec);

/// The case shape every chart here takes: a `width` x `height` logical viewport at `scale`, on
/// [`BASE`], at the corpus' default tolerance (channel 2, alpha 2, no mismatched pixels).
fn case(name: &'static str, width: f64, height: f64, scale: f64) -> (&'static str, FrameSpec) {
    let mut frame = FrameSpec::phone().at(FrameTime::ZERO);
    frame.size = Size::new(width, height);
    frame.scale = scale;
    (name, frame)
}

fn corpus_case(
    (name, frame): (&'static str, FrameSpec),
    record: fn(&mut Scene),
    about: &'static str,
) -> CorpusCase {
    CorpusCase {
        spec: physical_case(name, &frame, Tolerance::new()).with_base_color(BASE),
        record,
        probes: &[],
        eroded_interior: false,
        about,
    }
}

fn record_chart<V: View<()>>(scene: &mut Scene, frame: &FrameSpec, build: impl Fn() -> V) {
    let (mut tcx, _family) = test_text_context();
    record_view(scene, frame, Theme::neutral(), &mut tcx, build);
}

fn spacer(width: f64, height: f64) -> impl View<()> {
    frust::SizedBox::<()>(Some(width), Some(height))
}

/// `view` in a fixed `width` x `height` box, for a chart that expands to the width it is given.
fn boxed(width: f64, height: f64, view: impl View<()>) -> impl View<()> {
    frust::SizedBox::<()>(Some(width), Some(height)).child(view)
}

fn bar_frame() -> (&'static str, FrameSpec) {
    case("charts-bar", 342.0, 120.0, 1.0)
}

fn bar() -> CorpusCase {
    fn record(scene: &mut Scene) {
        let quarters = Categories::from_rows([
            ("Q1", [42.0, 28.0, 15.0]),
            ("Q2", [55.0, 33.0, 22.0]),
            ("Q3", [38.0, 47.0, 30.0]),
            ("Q4", [64.0, 40.0, 26.0]),
        ]);
        record_chart(scene, &bar_frame().1, move || {
            let chart = |mode: BarMode, max: f32| {
                bar_chart(quarters.clone())
                    .mode(mode)
                    .colors([BLUE, VIOLET, CALM])
                    .domain(YDomain::fixed(0.0, max))
                    .grid([0.5, 1.0])
                    .grid_color(GRID)
                    .radius(2.0)
                    .category_labels(false)
                    .height(120.0)
            };
            frust::row::<()>()
                .child(boxed(165.0, 120.0, chart(BarMode::Grouped, 80.0)))
                .child(spacer(12.0, 120.0))
                .child(boxed(165.0, 120.0, chart(BarMode::Stacked, 160.0)))
        });
    }
    corpus_case(
        bar_frame(),
        record,
        "four categories of three values in three colours: grouped on the left, stacked on the right",
    )
}

fn pie_frame() -> (&'static str, FrameSpec) {
    case("charts-pie", 232.0, 112.0, 1.0)
}

fn pie() -> CorpusCase {
    fn record(scene: &mut Scene) {
        let shares =
            Categories::from_rows([("a", [40.0]), ("b", [25.0]), ("c", [20.0]), ("d", [15.0])]);
        record_chart(scene, &pie_frame().1, move || {
            let colors = [BLUE, VIOLET, CALM, BUSY];
            frust::row::<()>()
                .child(
                    pie_chart::<()>(shares.clone())
                        .colors(colors)
                        .track(TRACK)
                        .size(112.0),
                )
                .child(spacer(8.0, 112.0))
                .child(
                    pie_chart::<()>(shares.clone())
                        .colors(colors)
                        .track(TRACK)
                        .donut(0.6)
                        .gap(3.0)
                        .corner_radius(3.0)
                        .size(112.0),
                )
        });
    }
    corpus_case(
        pie_frame(),
        record,
        "a four-section pie beside the same data as a donut with 3 px gaps and rounded corners",
    )
}

fn gauge_frame() -> (&'static str, FrameSpec) {
    case("charts-gauge", 232.0, 112.0, 1.0)
}

fn gauge_case() -> CorpusCase {
    fn record(scene: &mut Scene) {
        record_chart(scene, &gauge_frame().1, move || {
            let zones = [
                (0.0, 60.0, CALM),
                (60.0, 85.0, BUSY),
                (85.0, 100.0, CRITICAL),
            ];
            frust::row::<()>()
                .child(
                    gauge::<()>(48.0)
                        .thickness(8.0)
                        .track(TRACK)
                        .color(BLUE)
                        .zones(zones)
                        .needle(NeedleStyle::new().color(NEEDLE))
                        .ticks(11, 5.0, WEB)
                        .size(112.0),
                )
                .child(spacer(8.0, 112.0))
                .child(
                    gauge::<()>(40.0)
                        .angles(PI, PI)
                        .thickness(12.0)
                        .cap(GaugeCap::Butt)
                        .track(TRACK)
                        .color(VIOLET)
                        .size(112.0),
                )
        });
    }
    corpus_case(
        gauge_frame(),
        record,
        "a default-arc gauge at 48 in blue over three zones with ticks and a needle, beside a \
         butt-capped half-circle gauge at 40 over the top",
    )
}

fn scatter_frame() -> (&'static str, FrameSpec) {
    case("charts-scatter", 342.0, 120.0, 1.0)
}

fn scatter() -> CorpusCase {
    fn record(scene: &mut Scene) {
        let points = Points::new(64);
        points.extend((0..48u32).map(|i| {
            let cluster = (i % 3) as usize;
            let step = (i / 3) as f32;
            // Three loose bands climbing at different rates, with a fixed wobble.
            let wobble = ((i * 7) % 11) as f32 - 5.0;
            let y = 10.0 + step * (2.0 + cluster as f32 * 1.5) + wobble;
            let radius = 2.0 + ((i * 5) % 4) as f32;
            ScatterPoint::new(step * 6.0 + cluster as f32 * 2.0, y)
                .with_radius(radius)
                .with_color(cluster)
        }));
        record_chart(scene, &scatter_frame().1, move || {
            scatter_chart::<()>(points.clone())
                .x_domain(YDomain::fixed(-4.0, 100.0))
                .y_domain(YDomain::fixed(0.0, 100.0))
                .colors([BLUE, VIOLET, BUSY])
                .height(120.0)
        });
    }
    corpus_case(
        scatter_frame(),
        record,
        "48 points in three colours on fixed domains, radii 2 to 5 px, three rising bands",
    )
}

fn radar_frame() -> (&'static str, FrameSpec) {
    case("charts-radar", 232.0, 112.0, 1.0)
}

fn radar() -> CorpusCase {
    fn record(scene: &mut Scene) {
        let skills = Categories::from_rows([
            ("a", [80.0, 55.0]),
            ("b", [65.0, 90.0]),
            ("c", [90.0, 40.0]),
            ("d", [45.0, 70.0]),
            ("e", [70.0, 85.0]),
            ("f", [55.0, 30.0]),
        ]);
        record_chart(scene, &radar_frame().1, move || {
            let chart = |shape: RadarShape| {
                radar_chart(skills.clone())
                    .shape(shape)
                    .domain(YDomain::fixed(0.0, 100.0))
                    .dataset_styles([
                        RadarStyle::color(CALM),
                        RadarStyle::color(BLUE).stroke_width(2.0).fill_alpha(0.35),
                    ])
                    .web(WEB, 1.0)
                    .titles(false)
                    .size(112.0)
            };
            frust::row::<()>()
                .child(chart(RadarShape::Polygon))
                .child(spacer(8.0, 112.0))
                .child(chart(RadarShape::Circle))
        });
    }
    corpus_case(
        radar_frame(),
        record,
        "two filled datasets over six axes, on a polygon web and on a circular one, no titles",
    )
}

fn candlestick_frame() -> (&'static str, FrameSpec) {
    case("charts-candlestick", 342.0, 96.0, 1.0)
}

/// Each candle's close minus its open; the zeros are dojis, and slots 9 and 10 are gaps.
const MOVES: [f32; 28] = [
    3.0, -2.0, 4.0, 2.0, -5.0, 0.0, 3.0, -1.0, 4.0, 0.0, 0.0, -3.0, -4.0, 2.0, -2.0, 5.0, 1.0, 0.0,
    -3.0, 2.0, 4.0, -1.0, -4.0, 3.0, -2.0, 1.0, 5.0, -3.0,
];

fn candlestick() -> CorpusCase {
    fn record(scene: &mut Scene) {
        let candles = Candles::new(28);
        let mut close = 50.0_f32;
        candles.extend((0..28u32).map(|i| {
            if i == 9 || i == 10 {
                return None;
            }
            let open = close;
            close = open + MOVES[i as usize];
            let high = open.max(close) + 1.0 + (i % 3) as f32;
            let low = open.min(close) - 1.0 - (i % 2) as f32;
            Some(Candle::new(open, high, low, close))
        }));
        record_chart(scene, &candlestick_frame().1, move || {
            candlestick_chart(candles.clone())
                .domain(YDomain::fixed(44.0, 68.0))
                .body_width(8.0)
                .gap(4.0)
                .wick_width(2.0)
                .up(CALM)
                .down(CRITICAL)
                .grid([0.25, 0.5, 0.75])
                .grid_color(GRID)
                .height(96.0)
        });
    }
    corpus_case(
        candlestick_frame(),
        record,
        "28 candle slots, up green and down red, two dojis and a two-slot gap, over three grid rules",
    )
}

fn family_cases() -> Vec<CorpusCase> {
    vec![
        bar(),
        pie(),
        gauge_case(),
        scatter(),
        radar(),
        candlestick(),
    ]
}

#[test]
fn the_chart_families_match_their_goldens() {
    let _guard = hold();
    let cases = family_cases();
    let report = run_cpu_goldens(&cases);
    println!(
        "goldens: frust-charts families - {} compared, {} promoted",
        report.compared, report.promoted
    );
    report.assert_passed("the frust-charts family golden corpus");
}

#[test]
fn no_family_shapes_against_a_host_font() {
    let _guard = hold();
    let mut failures = Vec::new();
    for case in family_cases() {
        for message in foreign_font_runs(&case.scene()) {
            failures.push(format!("[{}] {message}", case.spec.name));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
