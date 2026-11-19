//! The `heat_cells` widget: a grid of small rounded cells, one per
//! [`Samples`] value, coloured by a [`Tone`] and
//! [`Tone::intensity_at`].
//!
//! Cells are laid out row-major (index 0 top-left) at whole logical pixels. The widget reads
//! the shared [`Samples`] while painting, so a write shows with no rebuild: the write
//! requests a repaint and the next frame draws the new values. It takes no input and never
//! requests a frame.
//!
//! Colour precedence is explicit ([`tone`](HeatCellsView::tone) or
//! [`color`](HeatCellsView::color)), then the installed theme's primary colour, then
//! [`Tone::FALLBACK_CALM`]. A cell's alpha runs from
//! [`min_intensity`](HeatCellsView::min_intensity) at the range minimum up to 1.0 at the
//! maximum.

use std::marker::PhantomData;

use frust::authoring::{
    BoxConstraints, BuildCtx, ChangeFlags, LayoutCtx, PaintCtx, PaintScene, Point, Role,
    SemanticsCtx, Size, View, Widget,
};
use frust::{Color, Theme};

use crate::{Samples, Tone};

const DEFAULT_CELL: (f32, f32) = (8.0, 18.0);
const DEFAULT_GAP: f32 = 2.0;
const DEFAULT_RADIUS: f32 = 2.0;
const DEFAULT_RANGE: (f32, f32) = (0.0, 100.0);
const DEFAULT_MIN_INTENSITY: f32 = 0.25;
/// The colour of a gap cell when neither the caller nor the theme gives one.
const FALLBACK_EMPTY: Color = Color::from_rgb8(0x8a, 0x8f, 0x98);

/// A grid of small rounded cells, one per index of `samples`.
///
/// ```
/// use frust_charts::{Samples, Tone, heat_cells};
/// use frust::Color;
///
/// let cores = Samples::new(64);
/// let _view = heat_cells::<()>(cores.clone())
///     .columns(16)
///     .tone(Tone::load(Color::from_rgb8(0, 160, 0), Color::from_rgb8(220, 160, 0), Color::from_rgb8(200, 0, 0)))
///     .semantics("CPU cores");
/// ```
pub fn heat_cells<S: 'static>(samples: Samples) -> HeatCellsView<S> {
    HeatCellsView {
        samples,
        columns: None,
        cell: DEFAULT_CELL,
        gap: DEFAULT_GAP,
        radius: DEFAULT_RADIUS,
        range: DEFAULT_RANGE,
        tone: None,
        color: None,
        min_intensity: DEFAULT_MIN_INTENSITY,
        empty: None,
        label: None,
        _state: PhantomData,
    }
}

/// The declarative view built by [`heat_cells`].
pub struct HeatCellsView<S: 'static> {
    samples: Samples,
    columns: Option<usize>,
    cell: (f32, f32),
    gap: f32,
    radius: f32,
    range: (f32, f32),
    tone: Option<Tone>,
    color: Option<Color>,
    min_intensity: f32,
    empty: Option<Color>,
    label: Option<String>,
    _state: PhantomData<fn(&mut S)>,
}

impl<S: 'static> HeatCellsView<S> {
    /// A fixed column count (default: as many as fit at the cell size).
    pub fn columns(mut self, columns: usize) -> Self {
        self.columns = Some(columns);
        self
    }

    /// Cell width and height in logical pixels (default 8 x 18).
    pub fn cell(mut self, width: f32, height: f32) -> Self {
        self.cell = (width, height);
        self
    }

    /// The space between cells (default 2).
    pub fn gap(mut self, gap: f32) -> Self {
        self.gap = gap;
        self
    }

    /// The corner radius (default 2).
    pub fn radius(mut self, radius: f32) -> Self {
        self.radius = radius;
        self
    }

    /// The value range the intensity runs over (default 0..100).
    pub fn range(mut self, min: f32, max: f32) -> Self {
        self.range = (min, max);
        self
    }

    /// Colour each cell by its value through `tone`.
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = Some(tone);
        self
    }

    /// One colour for every cell, used when there is no tone.
    pub fn color(mut self, color: Color) -> Self {
        self.color = Some(color);
        self
    }

    /// The alpha a cell has at the range minimum, rising linearly to 1.0 at the maximum
    /// (default 0.25).
    pub fn min_intensity(mut self, min_intensity: f32) -> Self {
        self.min_intensity = min_intensity;
        self
    }

    /// The colour of a gap value, drawn at the minimum intensity.
    pub fn empty(mut self, color: Color) -> Self {
        self.empty = Some(color);
        self
    }

    /// An accessible name; the grid is exposed as one image node only when this is set.
    pub fn semantics(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    fn paint_changed(&self, prev: &Self) -> bool {
        !self.samples.ptr_eq(&prev.samples)
            || self.radius != prev.radius
            || self.range != prev.range
            || self.tone != prev.tone
            || self.color != prev.color
            || self.min_intensity != prev.min_intensity
            || self.empty != prev.empty
    }

    fn layout_changed(&self, prev: &Self) -> bool {
        self.cell != prev.cell || self.gap != prev.gap || self.columns != prev.columns
    }

    fn widget(&self) -> HeatCellsWidget {
        HeatCellsWidget {
            samples: self.samples.clone(),
            columns: self.columns,
            cell: self.cell,
            gap: self.gap,
            radius: self.radius,
            range: self.range,
            tone: self.tone.clone(),
            color: self.color,
            min_intensity: self.min_intensity,
            empty: self.empty,
            label: self.label.clone(),
            laid_out_columns: 1,
        }
    }
}

impl<S: 'static> View<S> for HeatCellsView<S> {
    type Element = HeatCellsWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> HeatCellsWidget {
        self.widget()
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut HeatCellsWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = ChangeFlags::NONE;
        if self.layout_changed(prev) {
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        if self.paint_changed(prev) {
            flags |= ChangeFlags::PAINT;
        }
        if self.label != prev.label {
            flags |= ChangeFlags::PAINT;
        }
        if flags != ChangeFlags::NONE {
            let laid_out_columns = element.laid_out_columns;
            *element = self.widget();
            element.laid_out_columns = laid_out_columns;
        }
        flags
    }
}

/// The retained widget behind [`HeatCellsView`].
pub struct HeatCellsWidget {
    samples: Samples,
    columns: Option<usize>,
    cell: (f32, f32),
    gap: f32,
    radius: f32,
    range: (f32, f32),
    tone: Option<Tone>,
    color: Option<Color>,
    min_intensity: f32,
    empty: Option<Color>,
    label: Option<String>,
    /// The column count of the last layout, read by paint.
    laid_out_columns: usize,
}

/// A finite, non-negative logical-pixel length.
fn length(value: f32) -> f64 {
    if value.is_finite() {
        f64::from(value).max(0.0)
    } else {
        0.0
    }
}

impl HeatCellsWidget {
    fn pitch(&self) -> (f64, f64) {
        let gap = length(self.gap);
        (length(self.cell.0) + gap, length(self.cell.1) + gap)
    }

    /// The column count for `count` cells under `max_width`.
    fn resolve_columns(&self, count: usize, max_width: f64) -> usize {
        let columns = match self.columns {
            Some(columns) => columns,
            None => {
                let (pitch_x, _) = self.pitch();
                if !max_width.is_finite() {
                    count
                } else if pitch_x > 0.0 {
                    ((max_width + length(self.gap)) / pitch_x).floor() as usize
                } else {
                    count
                }
            }
        };
        columns.clamp(1, count.max(1))
    }

    /// The fill colour of a value.
    fn value_color(&self, theme: Option<&Theme>, value: f32) -> Color {
        let base = match (&self.tone, self.color) {
            (Some(tone), _) => tone.color_at(value),
            (None, Some(color)) => color,
            (None, None) => theme.map_or(Tone::FALLBACK_CALM, |t| t.scheme().primary),
        };
        base.multiply_alpha(self.alpha(Tone::intensity_at(value, self.range)))
    }

    fn alpha(&self, intensity: f32) -> f32 {
        let min = self.min_intensity.clamp(0.0, 1.0);
        min + (1.0 - min) * intensity
    }

    fn empty_color(&self, theme: Option<&Theme>) -> Color {
        let base = self.empty.unwrap_or_else(|| {
            theme.map_or(FALLBACK_EMPTY, |t| t.scheme().surface_container_highest)
        });
        base.multiply_alpha(self.alpha(0.0))
    }
}

impl Widget for HeatCellsWidget {
    fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let count = self.samples.len();
        let columns = self.resolve_columns(count, bc.max().width);
        self.laid_out_columns = columns;
        if count == 0 {
            return bc.constrain(Size::ZERO);
        }
        let rows = count.div_ceil(columns);
        let gap = length(self.gap);
        let width = columns as f64 * length(self.cell.0) + (columns - 1) as f64 * gap;
        let height = rows as f64 * length(self.cell.1) + (rows - 1) as f64 * gap;
        bc.constrain(Size::new(width, height))
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let theme = Theme::from_paint_ctx(ctx);
        let (origin, size) = (ctx.origin(), ctx.size());
        let (cell_w, cell_h) = (length(self.cell.0), length(self.cell.1));
        let (pitch_x, pitch_y) = self.pitch();
        let columns = self.laid_out_columns.max(1);
        let radius = length(self.radius);
        let empty = self.empty_color(theme);
        self.samples.with_values(|values| {
            for (index, value) in values.iter().enumerate() {
                let (col, row) = (index % columns, index / columns);
                let x = (col as f64 * pitch_x).round();
                let y = (row as f64 * pitch_y).round();
                if x + cell_w > size.width + 1e-6 || y + cell_h > size.height + 1e-6 {
                    continue;
                }
                let color = match value {
                    Some(value) => self.value_color(theme, *value),
                    None => empty,
                };
                scene.fill_rounded_rect(
                    Point::new(origin.x + x, origin.y + y),
                    Size::new(cell_w, cell_h),
                    radius,
                    color,
                );
            }
        });
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        if let Some(label) = &self.label {
            ctx.push_node(Role::Image, |node| node.set_label(label.as_str()));
        }
    }
}
