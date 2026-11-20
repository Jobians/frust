//! CPU raster goldens for the chart set, run through the repository's central golden corpus.
//!
//! ```text
//! cargo test -p frust-charts --test goldens
//! UPDATE_GOLDENS=1 cargo test -p frust-charts --test goldens
//! ```
//!
//! Baselines land in `testing/goldens/cpu/` under `charts-*` stems. Every case is text-free (the
//! runner rejects glyph runs from any face but its bundled ones), uses hard-coded data (no
//! randomness, no clock) and explicit colours on a dark base, so the result does not depend on
//! theme defaults.
//!
//! Writing a data handle requests a process-wide repaint, so the test that records the scenes
//! holds one static mutex.

use std::sync::{Mutex, MutexGuard, PoisonError};

use frust::authoring::Size;
use frust::{Color, CrossAxisAlignment, EdgeInsets, FrameTime, Theme, View};
use frust_charts::{
    PlotStyle, SampleSlot, Samples, Segment, Series, SeriesStyle, SparkStyle, SparklineView, Tone,
    YDomain, heat_cells, meter, meter_segments, meter_strip, sparkline, time_series,
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
const BLUE: Color = Color::from_rgb8(0x4a, 0x90, 0xe2);
const VIOLET: Color = Color::from_rgb8(0xb0, 0x7c, 0xe8);
const CALM: Color = Color::from_rgb8(0x2e, 0x9e, 0x5b);
const BUSY: Color = Color::from_rgb8(0xe0, 0xa3, 0x1a);
const CRITICAL: Color = Color::from_rgb8(0xd6, 0x3b, 0x3b);

fn load_tone() -> Tone {
    Tone::load(CALM, BUSY, CRITICAL)
}

/// A triangle wave between `lo` and `hi` with the given period, in integer steps.
fn triangle(i: usize, period: usize, lo: f32, hi: f32) -> f32 {
    let half = period / 2;
    let phase = i % period;
    let up = if phase <= half { phase } else { period - phase };
    lo + (hi - lo) * up as f32 / half as f32
}

/// A fixed, lumpy waveform: two triangle waves of different periods added together.
fn waveform(i: usize, lo: f32, hi: f32) -> f32 {
    let slow = triangle(i, 40, 0.0, 1.0);
    let fast = triangle(i + 3, 12, 0.0, 1.0);
    lo + (hi - lo) * (0.7 * slow + 0.3 * fast)
}

fn series_of(capacity: usize, lo: f32, hi: f32, shift: usize) -> Series {
    let series = Series::new(capacity);
    series.extend((0..capacity).map(|i| Some(waveform(i + shift, lo, hi))));
    series
}

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

fn area_frame() -> (&'static str, FrameSpec) {
    case("charts-area", 342.0, 56.0, 1.0)
}

fn area() -> CorpusCase {
    fn record(scene: &mut Scene) {
        let series = series_of(60, 8.0, 92.0, 0);
        record_chart(scene, &area_frame().1, move || {
            time_series::<()>()
                .series(series.clone(), SeriesStyle::color(BLUE))
                .grid([0.5])
                .grid_color(GRID)
                .height(56.0)
        });
    }
    corpus_case(
        area_frame(),
        record,
        "a 342x56 area chart of 60 samples of a fixed waveform with one mid grid rule",
    )
}

fn gaps_frame() -> (&'static str, FrameSpec) {
    case("charts-area-gaps", 342.0, 56.0, 1.0)
}

fn area_gaps() -> CorpusCase {
    fn record(scene: &mut Scene) {
        let series = Series::new(60);
        series.extend((0..60).map(|i| match i {
            10..=14 | 16..=20 => None,
            _ => Some(waveform(i, 8.0, 92.0)),
        }));
        record_chart(scene, &gaps_frame().1, move || {
            time_series::<()>()
                .series(series.clone(), SeriesStyle::color(BLUE))
                .height(56.0)
        });
    }
    corpus_case(
        gaps_frame(),
        record,
        "the area chart with two gap runs and one isolated sample between them",
    )
}

fn mirrored_frame() -> (&'static str, FrameSpec) {
    case("charts-mirrored", 342.0, 72.0, 1.0)
}

fn mirrored() -> CorpusCase {
    fn record(scene: &mut Scene) {
        let upper = series_of(60, 10.0, 80.0, 0);
        let lower = series_of(60, 5.0, 40.0, 17);
        record_chart(scene, &mirrored_frame().1, move || {
            time_series::<()>()
                .mirrored(
                    (upper.clone(), SeriesStyle::color(BLUE)),
                    (lower.clone(), SeriesStyle::color(VIOLET)),
                )
                .domain(YDomain::auto_from_zero())
                .height(72.0)
        });
    }
    corpus_case(
        mirrored_frame(),
        record,
        "a 342x72 mirrored pair in two colours around a middle axis, auto domain per half",
    )
}

fn blocks_frame() -> (&'static str, FrameSpec) {
    case("charts-blocks", 342.0, 56.0, 1.0)
}

fn blocks() -> CorpusCase {
    fn record(scene: &mut Scene) {
        let series = Series::new(85);
        series.extend((0..85).map(|i| Some(triangle(i, 85, 8.0, 98.0))));
        record_chart(scene, &blocks_frame().1, move || {
            time_series::<()>()
                .series(series.clone(), SeriesStyle::new().tone(load_tone()))
                .style(PlotStyle::Blocks {
                    column_width: 3.0,
                    gap: 1.0,
                })
                .height(56.0)
        });
    }
    corpus_case(
        blocks_frame(),
        record,
        "a 342x56 block-column chart, 3 px columns with a 1 px gap, tone-coloured across 60 and 85",
    )
}

fn sparklines_frame() -> (&'static str, FrameSpec) {
    case("charts-sparklines", 328.0, 56.0, 2.0)
}

fn sparklines() -> CorpusCase {
    fn record(scene: &mut Scene) {
        let blocks_a = series_of(53, 10.0, 95.0, 0);
        let blocks_b = series_of(53, 20.0, 70.0, 9);
        let line_a = series_of(53, 10.0, 95.0, 5);
        let line_b = series_of(53, 15.0, 80.0, 21);
        record_chart(scene, &sparklines_frame().1, move || {
            let spark = |series: &Series, line: bool, color: Color| -> SparklineView {
                let view = sparkline(series.clone()).size(160.0, 24.0).color(color);
                if line {
                    view.style(SparkStyle::line())
                } else {
                    view
                }
            };
            let pair = |left: SparklineView, right: SparklineView| {
                frust::row::<()>()
                    .child(left)
                    .child(spacer(8.0, 24.0))
                    .child(right)
            };
            frust::column::<()>()
                .cross_axis(CrossAxisAlignment::Start)
                .child(pair(
                    spark(&blocks_a, false, BLUE),
                    spark(&blocks_b, false, CALM),
                ))
                .child(spacer(8.0, 8.0))
                .child(pair(
                    spark(&line_a, true, BUSY),
                    spark(&line_b, true, VIOLET),
                ))
        });
    }
    corpus_case(
        sparklines_frame(),
        record,
        "a 2x2 grid of 160x24 sparklines at 2x: block-column on top, line below",
    )
}

fn meters_frame() -> (&'static str, FrameSpec) {
    case("charts-meters", 200.0, 74.0, 1.0)
}

fn meters() -> CorpusCase {
    fn record(scene: &mut Scene) {
        record_chart(scene, &meters_frame().1, move || {
            frust::Padding(
                EdgeInsets::all(8.0),
                frust::column::<()>()
                    .cross_axis(CrossAxisAlignment::Stretch)
                    .child(meter(12.0).thickness(4.0).tone(load_tone()).track(TRACK))
                    .child(spacer(10.0, 10.0))
                    .child(meter(63.0).thickness(6.0).tone(load_tone()).track(TRACK))
                    .child(spacer(10.0, 10.0))
                    .child(meter(97.0).thickness(8.0).tone(load_tone()).track(TRACK))
                    .child(spacer(10.0, 10.0))
                    .child(
                        meter_segments([
                            Segment::new(30.0, BLUE),
                            Segment::new(20.0, VIOLET),
                            Segment::new(15.0, BUSY),
                        ])
                        .thickness(8.0)
                        .track(TRACK),
                    ),
            )
        });
    }
    corpus_case(
        meters_frame(),
        record,
        "horizontal meters 4, 6 and 8 px thick at 12, 63 and 97 under a load tone, plus a segmented one",
    )
}

fn strip_frame() -> (&'static str, FrameSpec) {
    case("charts-meter-strip", 318.0, 48.0, 1.0)
}

fn meter_strip_case() -> CorpusCase {
    fn record(scene: &mut Scene) {
        let samples = Samples::new(32);
        samples.set_all((0..32).map(|i| Some(waveform(i * 3, 4.0, 100.0))));
        record_chart(scene, &strip_frame().1, move || {
            meter_strip(samples.clone())
                .gap(2.0)
                .tone(load_tone())
                .track(TRACK)
                .height(48.0)
        });
    }
    corpus_case(
        strip_frame(),
        record,
        "32 vertical meter bars of mixed values, tone-coloured, on dark tracks",
    )
}

fn heat_frame() -> (&'static str, FrameSpec) {
    case("charts-heat", 222.0, 54.0, 1.0)
}

fn heat() -> CorpusCase {
    fn record(scene: &mut Scene) {
        let samples = Samples::new(64);
        samples.set_all((0..64).map(|i| match i {
            19 | 44 => None,
            _ => Some(i as f32 * 100.0 / 63.0),
        }));
        // A handle's slot reads back what the whole-set write stored.
        debug_assert_eq!(SampleSlot::get(&samples.slot(63)), Some(100.0));
        record_chart(scene, &heat_frame().1, move || {
            heat_cells::<()>(samples.clone())
                .columns(16)
                .cell(12.0, 12.0)
                .gap(2.0)
                .tone(load_tone())
                .empty(TRACK)
        });
    }
    corpus_case(
        heat_frame(),
        record,
        "64 rounded heat cells in 16 columns, values ramping 0..100, two gap cells",
    )
}

fn smooth_tone_frame() -> (&'static str, FrameSpec) {
    case("charts-line-smooth-tone", 342.0, 56.0, 1.0)
}

fn line_smooth_tone() -> CorpusCase {
    fn record(scene: &mut Scene) {
        let series = Series::new(60);
        series.extend((0..60).map(|i| match i {
            28..=31 => None,
            _ => Some(waveform(i, 10.0, 125.0)),
        }));
        record_chart(scene, &smooth_tone_frame().1, move || {
            time_series::<()>()
                .series(
                    series.clone(),
                    SeriesStyle::color(BLUE).tone_along_value(load_tone()),
                )
                .smooth(true)
                .height(56.0)
        });
    }
    corpus_case(
        smooth_tone_frame(),
        record,
        "a smoothed area chart coloured along its value by the load tone, with one gap",
    )
}

fn chart_cases() -> Vec<CorpusCase> {
    vec![
        area(),
        area_gaps(),
        mirrored(),
        blocks(),
        sparklines(),
        meters(),
        meter_strip_case(),
        heat(),
        line_smooth_tone(),
    ]
}

#[test]
fn the_chart_set_matches_its_goldens() {
    let _guard = hold();
    let cases = chart_cases();
    let report = run_cpu_goldens(&cases);
    println!(
        "goldens: frust-charts - {} compared, {} promoted",
        report.compared, report.promoted
    );
    report.assert_passed("the frust-charts golden corpus");
}

#[test]
fn no_chart_shapes_against_a_host_font() {
    let _guard = hold();
    let mut failures = Vec::new();
    for case in chart_cases() {
        for message in foreign_font_runs(&case.scene()) {
            failures.push(format!("[{}] {message}", case.spec.name));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
