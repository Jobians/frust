//! [`usage_meter`]/[`UsageMeterView`]: a compact context-usage meter for an
//! app bar, a short determinate bar with an optional label.
//!
//! # Contract
//!
//! - **Controlled and non-reactive.** The caller passes the counts; the meter
//!   holds no signals and rebuilds only when one of its inputs changes.
//! - **Fixed width per label mode.** The tone mark and the label each reserve
//!   the width of a widest-case sample, so the header does not shift as the
//!   numbers change. Digits use tabular figures.
//! - **Tone is not colour alone.** From `warn_at` a `!` mark and the warning
//!   tone; from `critical_at` a `×` mark and the error tone.
//! - **True numbers past the limit.** A `used` above `size` clamps the bar,
//!   but the label and the semantics still report the real figures. A `size`
//!   of zero is an unknown state: an empty bar and "unknown", never a
//!   division by zero.

use std::marker::PhantomData;

use frust::Theme;
use frust::authoring::text::{TextContext, TextLayout, TextStyle};
use frust::authoring::{
    BoxConstraints, BuildCtx, ChangeFlags, LayoutCtx, PaintCtx, PaintScene, Role, SemanticsCtx,
    View, Widget,
};
use kurbo::{Point, Size};
use peniko::Color;

use super::style;

/// Bar length, in logical px.
const BAR_WIDTH: f64 = 64.0;
/// Bar thickness, in logical px.
const BAR_HEIGHT: f64 = 6.0;
/// Gap between the mark, the bar and the label, in logical px.
const GAP: f64 = style::PAD_SMALL;
/// Slack for threshold comparisons: `0.8` as an `f32` is not the `f64`
/// quotient `80 / 100`.
const THRESHOLD_EPSILON: f64 = 1e-6;

/// Mark drawn at the warning tone.
const WARNING_MARK: &str = "!";
/// Mark drawn at the critical tone.
const CRITICAL_MARK: &str = "\u{d7}"; // ×
/// Widest-case label samples the reserved label widths are measured from.
const PERCENT_SAMPLE: &str = "999%";
const COUNTS_SAMPLE: &str = "999.9M / 999.9M";
/// Label of a meter whose size is unknown.
const UNKNOWN_TEXT: &str = "unknown";

/// What the meter's label shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum UsageLabel {
    /// The share used, as a whole percentage (`21%`).
    #[default]
    Percent,
    /// The raw counts with k and M suffixes (`42k / 200k`).
    Counts,
    /// No label: the bar and the tone mark only.
    None,
}

/// The severity a meter shows, from its thresholds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MeterTone {
    Normal,
    Warning,
    Critical,
}

/// The plain data a meter is built from. Compared on rebuild to skip work.
#[derive(Clone, Debug, PartialEq)]
struct MeterModel {
    used: u64,
    size: u64,
    label: UsageLabel,
    warn_at: f32,
    critical_at: f32,
    compact: bool,
    semantics: Option<String>,
}

impl MeterModel {
    /// Used over size, unclamped; `None` when the size is unknown.
    fn ratio(&self) -> Option<f64> {
        (self.size > 0).then(|| self.used as f64 / self.size as f64)
    }

    /// The bar's fill, `0.0..=1.0`; `None` when the size is unknown.
    fn fraction(&self) -> Option<f64> {
        self.ratio().map(|r| r.min(1.0))
    }

    /// The whole percentage, unclamped; `None` when the size is unknown.
    fn percent(&self) -> Option<u64> {
        self.ratio().map(|r| (r * 100.0).round() as u64)
    }

    fn tone(&self) -> MeterTone {
        let Some(ratio) = self.ratio() else {
            return MeterTone::Normal;
        };
        if ratio + THRESHOLD_EPSILON >= f64::from(self.critical_at) {
            MeterTone::Critical
        } else if ratio + THRESHOLD_EPSILON >= f64::from(self.warn_at) {
            MeterTone::Warning
        } else {
            MeterTone::Normal
        }
    }

    /// The visible label text, empty for [`UsageLabel::None`].
    fn label_text(&self) -> String {
        match self.label {
            UsageLabel::None => String::new(),
            _ if self.size == 0 => UNKNOWN_TEXT.to_owned(),
            UsageLabel::Percent => format!("{}%", self.percent().unwrap_or(0)),
            UsageLabel::Counts => {
                format!("{} / {}", format_count(self.used), format_count(self.size))
            }
        }
    }

    /// The accessible text: the caller's override, else the real numbers.
    fn semantics_text(&self) -> String {
        if let Some(text) = &self.semantics {
            return text.clone();
        }
        match self.percent() {
            Some(percent) => format!(
                "context {} of {} tokens, {percent} percent",
                self.used, self.size
            ),
            None => "context usage unknown".to_owned(),
        }
    }
}

/// Formats a count with k and M suffixes: `999`, `42k`, `1.5M`.
fn format_count(n: u64) -> String {
    if n < 1_000 {
        return n.to_string();
    }
    let thousands = (n + 500) / 1_000;
    if thousands < 1_000 {
        return format!("{thousands}k");
    }
    // Tenths of a million, so 999,600 reads as `1M` and not `1000k`.
    let tenths = (n + 50_000) / 100_000;
    if tenths.is_multiple_of(10) {
        format!("{}M", tenths / 10)
    } else {
        format!("{}.{}M", tenths / 10, tenths % 10)
    }
}

/// The filled part of the bar, and the tone's text and mark colour.
fn fill_color(theme: Option<&Theme>, tone: MeterTone) -> Color {
    match tone {
        MeterTone::Normal => style::accent(theme),
        MeterTone::Warning => style::warning(theme),
        MeterTone::Critical => style::error(theme),
    }
}

/// A declarative usage meter. See the [module docs](self).
pub struct UsageMeterView<State> {
    model: MeterModel,
    state: PhantomData<fn() -> State>,
}

impl<State> std::fmt::Debug for UsageMeterView<State> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UsageMeterView")
            .field("used", &self.model.used)
            .field("size", &self.model.size)
            .field("label", &self.model.label)
            .field("warn_at", &self.model.warn_at)
            .field("critical_at", &self.model.critical_at)
            .field("compact", &self.model.compact)
            .finish_non_exhaustive()
    }
}

/// Create a usage meter for `used` of `size` (the unit is the caller's:
/// tokens, bytes, anything). It shows a percentage label, warns at 80 % and
/// turns critical at 95 %; see the builder methods to change any of that.
/// The caller owns the counts and passes the current ones on every build.
pub fn usage_meter<State: 'static>(used: u64, size: u64) -> UsageMeterView<State> {
    UsageMeterView {
        model: MeterModel {
            used,
            size,
            label: UsageLabel::Percent,
            warn_at: 0.8,
            critical_at: 0.95,
            compact: false,
            semantics: None,
        },
        state: PhantomData,
    }
}

impl<State> UsageMeterView<State> {
    /// Choose what the label shows.
    pub fn label(mut self, label: UsageLabel) -> Self {
        self.model.label = label;
        self
    }

    /// The fraction (`0.0..=1.0`) from which the warning tone shows.
    pub fn warn_at(mut self, fraction: f32) -> Self {
        self.model.warn_at = fraction;
        self
    }

    /// The fraction (`0.0..=1.0`) from which the critical tone shows.
    pub fn critical_at(mut self, fraction: f32) -> Self {
        self.model.critical_at = fraction;
        self
    }

    /// Show the bar and the tone mark only, without the label.
    pub fn compact(mut self, compact: bool) -> Self {
        self.model.compact = compact;
        self
    }

    /// Replace the accessible text the meter reports.
    pub fn semantics_label(mut self, label: impl Into<String>) -> Self {
        self.model.semantics = Some(label.into());
        self
    }
}

impl<State: 'static> View<State> for UsageMeterView<State> {
    type Element = UsageMeterWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> UsageMeterWidget {
        UsageMeterWidget {
            model: self.model.clone(),
            mark: None,
            mark_w: 0.0,
            label: None,
            label_w: 0.0,
            line_h: 0.0,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut UsageMeterWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        if prev.model == self.model {
            return ChangeFlags::NONE;
        }
        element.model = self.model.clone();
        ChangeFlags::LAYOUT | ChangeFlags::PAINT
    }
}

/// The retained widget for a [`UsageMeterView`]. See the [module docs](self).
pub struct UsageMeterWidget {
    model: MeterModel,
    mark: Option<TextLayout>,
    mark_w: f64,
    label: Option<TextLayout>,
    label_w: f64,
    line_h: f64,
}

impl UsageMeterWidget {
    /// The label text's reserved width for `label`, from its widest sample.
    fn sample(label: UsageLabel) -> Option<&'static str> {
        match label {
            UsageLabel::Percent => Some(PERCENT_SAMPLE),
            UsageLabel::Counts => Some(COUNTS_SAMPLE),
            UsageLabel::None => None,
        }
    }

    fn mark_text(tone: MeterTone) -> Option<&'static str> {
        match tone {
            MeterTone::Normal => None,
            MeterTone::Warning => Some(WARNING_MARK),
            MeterTone::Critical => Some(CRITICAL_MARK),
        }
    }
}

impl Widget for UsageMeterWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        // Resolve every colour and family first, so the theme borrow ends
        // before the text context takes `ctx`.
        let theme = Theme::from_layout_ctx(ctx);
        let tone = self.model.tone();
        let fill = fill_color(theme, tone);
        let muted = style::muted(theme);
        let family = style::mono_family(theme);
        let base = |color: Color| {
            TextStyle::new(style::LABEL_FONT_SIZE, color)
                .with_family(family.clone())
                .with_feature(*b"tnum", 1)
        };
        let mark_style = base(fill);
        let label_style = base(if tone == MeterTone::Normal {
            muted
        } else {
            fill
        });
        let label_text = self.model.label_text();
        let show_label = !self.model.compact && Self::sample(self.model.label).is_some();

        let text = ctx.text_context::<TextContext>();
        self.mark_w = text.layout(CRITICAL_MARK, &mark_style, None).size().width;
        self.mark = Self::mark_text(tone).map(|m| text.layout(m, &mark_style, None));
        self.label_w = match Self::sample(self.model.label) {
            Some(sample) if show_label => text.layout(sample, &label_style, None).size().width,
            _ => 0.0,
        };
        self.label = show_label.then(|| text.layout(&label_text, &label_style, None));
        self.line_h = text.layout("0", &label_style, None).size().height;

        let mut width = self.mark_w + GAP + BAR_WIDTH;
        if show_label {
            width += GAP + self.label_w;
        }
        let height = self.line_h.max(BAR_HEIGHT);
        bc.constrain(Size::new(width, height))
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let theme = Theme::from_paint_ctx(ctx);
        let track = style::track(theme);
        let fill = fill_color(theme, self.model.tone());
        let origin = ctx.origin();
        let size = ctx.size();
        let radius = BAR_HEIGHT / 2.0;

        let bar_origin = Point::new(
            origin.x + self.mark_w + GAP,
            origin.y + (size.height - BAR_HEIGHT) / 2.0,
        );
        scene.fill_rounded_rect(bar_origin, Size::new(BAR_WIDTH, BAR_HEIGHT), radius, track);
        if let Some(fraction) = self.model.fraction() {
            let filled = BAR_WIDTH * fraction;
            if filled > 0.0 {
                scene.fill_rounded_rect(bar_origin, Size::new(filled, BAR_HEIGHT), radius, fill);
            }
        }

        if let Some(mark) = &self.mark {
            let y = origin.y + (size.height - mark.size().height) / 2.0;
            style::paint_layout(mark, Point::new(origin.x, y), scene);
        }
        if let Some(label) = &self.label {
            let x = bar_origin.x + BAR_WIDTH + GAP;
            let y = origin.y + (size.height - label.size().height) / 2.0;
            style::paint_layout(label, Point::new(x, y), scene);
        }
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        let text = self.model.semantics_text();
        let fraction = self.model.fraction();
        ctx.push_node(Role::Meter, |node| {
            node.set_label(text.as_str());
            if let Some(fraction) = fraction {
                node.set_numeric_value(fraction);
                node.set_min_numeric_value(0.0);
                node.set_max_numeric_value(1.0);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use frust::authoring::{AnyView, BuildCtx, any};
    use frust_core::RenderRoot;
    use std::any::Any;

    fn model(used: u64, size: u64) -> MeterModel {
        usage_meter::<()>(used, size).model
    }

    fn with_label(used: u64, size: u64, label: UsageLabel) -> MeterModel {
        usage_meter::<()>(used, size).label(label).model
    }

    #[derive(Default)]
    struct Recorder {
        rrects: Vec<(Point, Size, f64, Color)>,
        glyph_colors: Vec<Color>,
        glyph_runs: usize,
    }

    impl PaintScene for Recorder {
        fn fill_rect(&mut self, _o: Point, _s: Size, _c: Color) {}
        fn draw_text(&mut self, _o: Point, _t: &str) {}
        fn fill_rounded_rect(&mut self, o: Point, s: Size, radius: f64, color: Color) {
            self.rrects.push((o, s, radius, color));
        }
        fn draw_glyph_run(&mut self, run: frust::authoring::scene::GlyphRun) {
            self.glyph_runs += 1;
            if let frust::authoring::Brush::Solid(c) = run.brush {
                self.glyph_colors.push(c);
            }
        }
    }

    fn build(used: u64, size: u64) -> UsageMeterWidget {
        let view = usage_meter::<()>(used, size);
        let mut counter = 0u64;
        View::<()>::build(&view, &mut BuildCtx::new(&mut counter))
    }

    fn layout(widget: &mut UsageMeterWidget, theme: Option<&Theme>) -> Size {
        let mut tcx = TextContext::new();
        let mut lctx = match theme {
            Some(t) => {
                LayoutCtx::with_resources(Some(&mut tcx as &mut dyn Any), Some(t as &dyn Any))
            }
            None => LayoutCtx::with_text_context(&mut tcx as &mut dyn Any),
        };
        widget.layout(&mut lctx, &BoxConstraints::loose(Size::new(400.0, 200.0)))
    }

    fn paint(widget: &mut UsageMeterWidget, size: Size, theme: Option<&Theme>) -> Recorder {
        let mut rec = Recorder::default();
        let mut pctx = match theme {
            Some(t) => PaintCtx::new(Point::ZERO, size).with_theme(t),
            None => PaintCtx::new(Point::ZERO, size),
        };
        widget.paint(&mut pctx, &mut rec);
        rec
    }

    #[test]
    fn percent_label_at_each_sampled_share() {
        for (used, expected) in [
            (0, "0%"),
            (100, "50%"),
            (160, "80%"),
            (190, "95%"),
            (200, "100%"),
            (300, "150%"),
        ] {
            assert_eq!(model(used, 200).label_text(), expected, "used {used}");
        }
    }

    #[test]
    fn over_full_clamps_the_bar_but_keeps_the_true_numbers() {
        let m = model(300, 200);
        assert_eq!(m.fraction(), Some(1.0));
        assert_eq!(m.percent(), Some(150));
        assert!(m.semantics_text().contains("150 percent"));
        assert!(m.semantics_text().contains("300 of 200"));
    }

    #[test]
    fn zero_size_is_unknown_without_dividing() {
        let m = model(42, 0);
        assert_eq!(m.ratio(), None);
        assert_eq!(m.fraction(), None);
        assert_eq!(m.tone(), MeterTone::Normal);
        assert_eq!(m.label_text(), UNKNOWN_TEXT);
        assert_eq!(m.semantics_text(), "context usage unknown");
    }

    #[test]
    fn tones_switch_at_the_default_thresholds() {
        assert_eq!(model(79, 100).tone(), MeterTone::Normal);
        assert_eq!(model(80, 100).tone(), MeterTone::Warning);
        assert_eq!(model(94, 100).tone(), MeterTone::Warning);
        assert_eq!(model(95, 100).tone(), MeterTone::Critical);
        assert_eq!(model(150, 100).tone(), MeterTone::Critical);
    }

    #[test]
    fn custom_thresholds_are_honoured() {
        let m = usage_meter::<()>(50, 100)
            .warn_at(0.4)
            .critical_at(0.5)
            .model;
        assert_eq!(m.tone(), MeterTone::Critical);
        let m = usage_meter::<()>(45, 100)
            .warn_at(0.4)
            .critical_at(0.5)
            .model;
        assert_eq!(m.tone(), MeterTone::Warning);
    }

    #[test]
    fn counts_format_with_k_and_m_suffixes() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1_000), "1k");
        assert_eq!(format_count(42_000), "42k");
        assert_eq!(format_count(999_499), "999k");
        assert_eq!(format_count(999_600), "1M");
        assert_eq!(format_count(1_500_000), "1.5M");
        assert_eq!(format_count(2_000_000), "2M");
        assert_eq!(
            with_label(42_000, 200_000, UsageLabel::Counts).label_text(),
            "42k / 200k"
        );
    }

    #[test]
    fn no_label_has_no_text() {
        assert_eq!(with_label(10, 20, UsageLabel::None).label_text(), "");
    }

    #[test]
    fn the_mark_appears_only_at_a_raised_tone() {
        let mut calm = build(10, 100);
        layout(&mut calm, None);
        assert!(calm.mark.is_none());
        let mut warn = build(80, 100);
        layout(&mut warn, None);
        assert!(warn.mark.is_some());
        let mut crit = build(95, 100);
        layout(&mut crit, None);
        assert!(crit.mark.is_some());
    }

    #[test]
    fn width_is_constant_across_values_in_each_label_mode() {
        for label in [UsageLabel::Percent, UsageLabel::Counts, UsageLabel::None] {
            let mut widths = Vec::new();
            for (used, size) in [
                (0, 200_000),
                (42_000, 200_000),
                (160_000, 200_000),
                (190_000, 200_000),
                (300_000, 200_000),
                (42, 0),
            ] {
                let view = usage_meter::<()>(used, size).label(label);
                let mut counter = 0u64;
                let mut w = View::<()>::build(&view, &mut BuildCtx::new(&mut counter));
                widths.push(layout(&mut w, None).width);
            }
            assert!(
                widths.windows(2).all(|p| (p[0] - p[1]).abs() < 1e-6),
                "{label:?}: {widths:?}"
            );
        }
    }

    #[test]
    fn compact_meter_drops_the_label() {
        let view = usage_meter::<()>(42, 100).compact(true);
        let mut counter = 0u64;
        let mut w = View::<()>::build(&view, &mut BuildCtx::new(&mut counter));
        layout(&mut w, None);
        assert!(w.label.is_none());
    }

    #[test]
    fn semantics_uses_the_override_when_given() {
        let view = usage_meter::<()>(42, 100).semantics_label("usage");
        assert_eq!(view.model.semantics_text(), "usage");
    }

    #[test]
    fn paint_draws_track_fill_and_label_in_the_tone_colour() {
        let theme = crate::baseline();
        let mut w = build(160, 200);
        layout(&mut w, Some(&theme));
        let size = Size::new(200.0, 20.0);
        let rec = paint(&mut w, size, Some(&theme));
        assert_eq!(rec.rrects.len(), 2, "track and fill");
        assert_eq!(rec.rrects[0].3, style::track(Some(&theme)));
        assert!(rec.glyph_runs > 0);
    }

    #[test]
    fn themed_and_unthemed_paint_do_not_panic() {
        let theme = crate::baseline();
        for theme in [None, Some(&theme)] {
            for (used, size) in [(0, 0), (0, 200), (80, 100), (95, 100), (150, 100)] {
                let mut w = build(used, size);
                layout(&mut w, theme);
                let _ = paint(&mut w, Size::new(200.0, 20.0), theme);
            }
        }
    }

    // ---- Tree-level: text work and the rebuild skip ------------------------

    struct Rig {
        root: RenderRoot<(), AnyView<()>>,
        text: TextContext,
    }

    impl Rig {
        fn new() -> Self {
            let mut root: RenderRoot<(), AnyView<()>> = RenderRoot::new();
            root.set_theme(Box::new(crate::baseline()));
            Rig {
                root,
                text: TextContext::new(),
            }
        }

        fn show(&mut self, mut view: impl FnMut() -> UsageMeterView<()>) {
            self.root.rebuild(&mut |_: &mut ()| any(view()), &mut ());
            self.root
                .layout_with_text(Size::new(360.0, 200.0), &mut self.text as &mut dyn Any);
        }

        fn events(&self) -> u64 {
            let s = self.text.shape_cache_stats();
            s.shapes + s.line_breaks
        }
    }

    #[test]
    fn equal_views_do_no_text_work() {
        let mut rig = Rig::new();
        let make = || usage_meter::<()>(42_000, 200_000).label(UsageLabel::Counts);
        rig.show(make);
        let before = rig.events();
        rig.show(make);
        assert_eq!(rig.events(), before);
    }

    #[test]
    fn a_changed_count_relays_out_the_meter() {
        let mut rig = Rig::new();
        rig.show(|| usage_meter::<()>(42_000, 200_000));
        let before = rig.events();
        rig.show(|| usage_meter::<()>(43_000, 200_000));
        assert!(rig.events() > before);
    }

    #[test]
    fn semantics_reports_meter_role_and_text() {
        fn logic(_s: &mut ()) -> UsageMeterView<()> {
            usage_meter::<()>(42, 200)
        }
        let mut root: RenderRoot<(), UsageMeterView<()>> = RenderRoot::new();
        let mut state = ();
        root.rebuild(&mut logic, &mut state);
        let mut tcx = TextContext::new();
        root.layout_with_text(Size::new(300.0, 300.0), &mut tcx as &mut dyn Any);
        let update = root.semantics();
        let (_, node) = update
            .nodes
            .iter()
            .find(|(_, n)| n.role() == Role::Meter)
            .expect("meter contributes a Role::Meter node");
        assert_eq!(node.label(), Some("context 42 of 200 tokens, 21 percent"));
    }
}
