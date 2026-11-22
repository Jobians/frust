//! Read-only geometry of a laid-out [`TextLayout`]: the rectangles a byte
//! range covers, each visual line's box, hit testing, word lookup and caret
//! rectangles.
//!
//! Every answer comes from the editor's own caret model (the editor module's
//! `EditLayout`, `Cursor` and `Selection`), built once per layout on the
//! first geometry call and kept with it, so a read-only paragraph and a
//! [`crate::TextEditor`] over the same text agree to the pixel: the same
//! grapheme caret stops, the same visual left/right across bidi runs, the
//! same UAX #29 word boundaries (dictionary segmented where the
//! `complex-scripts` feature is on), and the same selection rectangles.
//!
//! # Coordinates and indices
//!
//! Geometry is Y-down logical pixels in layout coordinates: `(0, 0)` is the
//! layout's top-left, as [`TextLayout::to_scene_runs`] places it at
//! `Point::ORIGIN`. Byte indices are into the layout's source text — for a
//! rich layout, the concatenated span text, so span `n` starts at the sum of
//! the earlier spans' lengths. A layout cut by `max_lines` or an ellipsis has
//! geometry only for the text it kept; a range past the cut covers nothing.
//!
//! # Two vertical extents
//!
//! [`TextLayout::line_bounds`] is the line's box as the line layout stacks
//! it: each line starts where the previous one ends, the first at `0` and
//! the last ending at [`TextLayout::size`]'s height, so line boxes tile the
//! layout with no gap. [`TextLayout::range_rects`] and
//! [`TextLayout::caret_rect`] use the editor's caret box instead: the line's
//! ascent and descent, each rounded, plus the leading's halves around the
//! baseline (Chrome's quantization, the box [`crate::TextEditor`] draws its
//! caret and selection in). The two differ by under a pixel; draw a selection
//! with the caret box and fill a whole line with the line box.

use std::ops::Range;

use kurbo::{Point, Rect};

use super::TextLayout;
use crate::editor::{Affinity, Cursor, EditLayout, Selection};

/// Which side of a boundary a [`TextPosition`] belongs to, where one byte
/// index has two places on screen: the end of a soft-wrapped line and the
/// start of the next, or the two edges of a bidi run boundary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TextAffinity {
    /// With the character after the index, in logical order.
    #[default]
    Downstream,
    /// With the character before the index.
    Upstream,
}

/// A caret position in a [`TextLayout`]: a byte index into its source text
/// and the [`TextAffinity`] that settles which side of a line or run boundary
/// it draws on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct TextPosition {
    /// The byte index, on a char boundary; at or past the text's end it is
    /// the end.
    pub index: usize,
    pub affinity: TextAffinity,
}

impl TextPosition {
    /// The position at byte `index` with `affinity`.
    pub const fn new(index: usize, affinity: TextAffinity) -> Self {
        Self { index, affinity }
    }
}

impl From<TextAffinity> for Affinity {
    fn from(affinity: TextAffinity) -> Self {
        match affinity {
            TextAffinity::Downstream => Affinity::Downstream,
            TextAffinity::Upstream => Affinity::Upstream,
        }
    }
}

impl From<Affinity> for TextAffinity {
    fn from(affinity: Affinity) -> Self {
        match affinity {
            Affinity::Downstream => TextAffinity::Downstream,
            Affinity::Upstream => TextAffinity::Upstream,
        }
    }
}

impl From<Cursor> for TextPosition {
    fn from(cursor: Cursor) -> Self {
        Self::new(cursor.index, cursor.affinity.into())
    }
}

/// A layout's caret model and line tops, built on the first geometry call.
#[derive(Debug)]
pub(crate) struct Geometry {
    edit: EditLayout,
    /// Each line's top, then the layout's bottom: `lines + 1` entries.
    tops: Vec<f64>,
}

impl Geometry {
    fn new(layout: &TextLayout) -> Self {
        let ellipsis = layout.ellipsis.as_ref().map(|e| &*e.shaped);
        let edit =
            EditLayout::with_ellipsis(layout.text.len(), &layout.shaped, ellipsis, &layout.lines);
        // The line layout's own stacking: `y` summed in `f64` exactly as
        // `lay_out` sums it, and narrowed through `f32` as the layout's
        // height is, so the last bottom is the layout's height.
        let mut tops = Vec::with_capacity(layout.lines.lines.len() + 1);
        let mut y = 0.0f64;
        tops.push(0.0);
        for line in &layout.lines.lines {
            y += f64::from(line.metrics.line_height);
            tops.push(f64::from(y as f32));
        }
        Self { edit, tops }
    }
}

impl TextLayout {
    /// The caret model, built on first use.
    fn geometry(&self) -> &Geometry {
        self.geometry.get_or_init(|| Geometry::new(self))
    }

    fn cursor(&self, position: TextPosition) -> Cursor {
        Cursor::from_byte_index(
            &self.geometry().edit,
            position.index,
            position.affinity.into(),
        )
    }

    /// The rectangles covering the source bytes `range`, in layout
    /// coordinates: one per contiguous visual span per line, so a range
    /// crossing a soft wrap or a newline yields one per line and one that
    /// crosses a bidi run boundary can yield two on a line. Exactly the
    /// rectangles [`crate::TextEditor::selection_rects`] draws for a
    /// selection of the same range of the same text.
    ///
    /// A range that ends past a newline covers it with a stub a quarter of
    /// the line's ascent plus descent wide at its line's end; a line wholly
    /// inside the range is covered across its full advance, trailing
    /// whitespace included. Each rectangle spans its line's caret box (see
    /// the module docs). An empty range, or one wholly past a `max_lines`
    /// cut, yields none.
    pub fn range_rects(&self, range: Range<usize>) -> Vec<Rect> {
        let mut rects = Vec::new();
        if range.is_empty() {
            return rects;
        }
        let edit = &self.geometry().edit;
        let selection = Selection::new(
            Cursor::from_byte_index(edit, range.start, Affinity::Downstream),
            Cursor::from_byte_index(edit, range.end, Affinity::Downstream),
        );
        selection.geometry_with(edit, |rect| rects.push(rect));
        rects
    }

    /// The top and bottom of visual line `line`, in layout coordinates, or
    /// `None` past [`Self::line_count`]. Line boxes stack with no gap from
    /// `0` to the layout's height (see the module docs for how they differ
    /// from the caret box).
    pub fn line_bounds(&self, line: usize) -> Option<(f64, f64)> {
        let tops = &self.geometry().tops;
        Some((*tops.get(line)?, *tops.get(line + 1)?))
    }

    /// The caret position nearest the layout-coordinate `point`: the line by
    /// `y` (the first above the layout, the last below it), then the
    /// grapheme edge nearest `x` on it, clamped to the line's ends. A point
    /// on a line's last grapheme past its middle lands after it, upstream —
    /// except before a newline, which never takes the caret onto the next
    /// line.
    pub fn position_at_point(&self, point: Point) -> TextPosition {
        Cursor::from_point(&self.geometry().edit, point.x as f32, point.y as f32).into()
    }

    /// The byte range of the word around `position`, by the same word
    /// boundaries [`crate::TextEditor`]'s word motion uses: from the
    /// boundary at or before the grapheme the position attaches to (the one
    /// after it downstream, the one before it upstream) to the next
    /// boundary. A space between two words is a word of its own. Empty, at
    /// the position, where no grapheme is laid out there (an empty text, or
    /// past a `max_lines` cut).
    ///
    /// `word_range_at(position_at_point(p))` is the word under `p`, as a
    /// double-click or a long-press at `p` selects it.
    pub fn word_range_at(&self, position: TextPosition) -> Range<usize> {
        let edit = &self.geometry().edit;
        let cursor = self.cursor(position);
        let before = cursor.index.checked_sub(1).and_then(|i| edit.unit_at(i));
        let after = edit.unit_at(cursor.index);
        let unit = match cursor.affinity {
            Affinity::Upstream => before.or(after),
            Affinity::Downstream => after.or(before),
        };
        let Some(unit) = unit else {
            return cursor.index..cursor.index;
        };
        Selection::word_at_unit(edit, unit).text_range()
    }

    /// The caret rectangle at `position`, `width` wide, in layout
    /// coordinates: at the grapheme edge the position draws on, spanning its
    /// line's caret box — the rectangle [`crate::TextEditor::cursor_rect`]
    /// draws for a caret at the same position.
    pub fn caret_rect(&self, position: TextPosition, width: f32) -> Rect {
        self.cursor(position).geometry(&self.geometry().edit, width)
    }
}
