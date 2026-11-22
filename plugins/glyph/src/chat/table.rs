//! The markdown table: a header row and body rows of cells, crate-private to
//! [`super::markdown`], which builds every cell as a `rich_text`.
//!
//! # Layout
//!
//! - **Measured columns.** Each cell is laid out once, wrapping at
//!   [`COL_MAX`] less its padding; a column is as wide as its widest cell,
//!   clamped to [`COL_MIN`]..[`COL_MAX`], so a long cell wraps inside its
//!   column instead of widening it. Rows are as tall as their tallest cell.
//! - **Alignment.** A column's `:-:` or `--:` places each cell's paragraph
//!   inside the column; the lines of a wrapped cell stay left-aligned.
//! - **Horizontal scroll.** The grid sits in a horizontal
//!   `custom_scroll_view` at its natural width, so a table wider than the
//!   available width scrolls sideways as a whole, and a vertical drag that
//!   starts on it scrolls the enclosing transcript.
//! - **Streaming.** A table still being written only gains text, so its
//!   measured columns only widen. While `.streaming(true)`, each column also
//!   keeps the widest width it has been laid out at, so a half-written last
//!   row never narrows an earlier column; widths settle to their measured
//!   values once streaming ends.

use std::cell::Cell;
use std::marker::PhantomData;
use std::rc::Rc;

use frust::accesskit::Role;
use frust::authoring::{
    BoxConstraints, BuildCtx, ChangeFlags, ChildPod, Color, EventCtx, EventResult, InputEvent,
    LayoutCtx, PaintCtx, PaintScene, Point, SemanticsCtx, Size, View, Widget, build_child,
    rebuild_child, route_event, route_event_single, teardown_child,
};
use frust::{ColorScheme, CustomScrollView, RichTextView, ScrollAxis, Theme, custom_scroll_view};

use super::markdown_parse::MdAlignment;
use super::style;

/// Narrowest column, padding included.
pub(crate) const COL_MIN: f64 = 48.0;
/// Widest column, padding included; longer cell text wraps.
pub(crate) const COL_MAX: f64 = 320.0;
/// Horizontal padding inside a cell.
const CELL_PAD_X: f64 = style::PAD_SMALL;
/// Vertical padding inside a cell.
const CELL_PAD_Y: f64 = style::ROW_GAP;
/// Width used when the incoming constraints are unbounded.
const FALLBACK_WIDTH: f64 = 480.0;

const DARK: ColorScheme = crate::tokens::color::dark();
/// Unthemed header-row fill.
const FALLBACK_HEADER_BG: Color = DARK.surface_container_high;

/// The cells of one table, shared by the outer view and its grid.
#[derive(Clone, Debug, PartialEq)]
struct Cells {
    alignments: Vec<MdAlignment>,
    columns: usize,
    /// Row-major, the header row first, every row padded to `columns`.
    cells: Vec<RichTextView>,
    streaming: bool,
}

impl Cells {
    fn rows(&self) -> usize {
        self.cells.len().checked_div(self.columns).unwrap_or(0)
    }

    fn alignment(&self, column: usize) -> MdAlignment {
        self.alignments
            .get(column)
            .copied()
            .unwrap_or(MdAlignment::None)
    }
}

/// A declarative table. See the [module docs](self).
pub(crate) struct TableView<State: 'static> {
    cells: Rc<Cells>,
    _state: PhantomData<fn(&mut State)>,
}

/// A table of `header` cells over `rows`, columns aligned by `alignments`.
/// Short rows are padded with empty cells.
pub(crate) fn table<State: 'static>(
    alignments: Vec<MdAlignment>,
    header: Vec<RichTextView>,
    rows: Vec<Vec<RichTextView>>,
) -> TableView<State> {
    let columns = rows
        .iter()
        .map(Vec::len)
        .chain([alignments.len(), header.len()])
        .max()
        .unwrap_or(0);
    let mut cells = Vec::with_capacity(columns * (rows.len() + 1));
    for row in std::iter::once(header).chain(rows) {
        let len = row.len();
        cells.extend(row);
        cells.extend((len..columns).map(|_| frust::rich_text(std::iter::empty())));
    }
    TableView {
        cells: Rc::new(Cells {
            alignments,
            columns,
            cells,
            streaming: false,
        }),
        _state: PhantomData,
    }
}

impl<State: 'static> TableView<State> {
    /// Keep every column at least as wide as it has been laid out, while the
    /// table is still being written.
    pub(crate) fn streaming(mut self, streaming: bool) -> Self {
        Rc::make_mut(&mut self.cells).streaming = streaming;
        self
    }

    fn scroller(&self, height: &Rc<Cell<f64>>) -> CustomScrollView<State> {
        let grid = GridView::<State> {
            cells: self.cells.clone(),
            height: height.clone(),
            _state: PhantomData,
        };
        custom_scroll_view(grid).axis(ScrollAxis::Horizontal)
    }
}

/// The retained widget for a [`TableView`]: the horizontal scroll view,
/// sized to the height its grid last measured.
pub(crate) struct TableWidget {
    viewport: ChildPod,
    /// The grid's natural height, written by the grid at every layout.
    height: Rc<Cell<f64>>,
}

impl<State: 'static> View<State> for TableView<State> {
    type Element = TableWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> TableWidget {
        let height = Rc::new(Cell::new(0.0));
        TableWidget {
            viewport: build_child(&self.scroller(&height), ctx),
            height,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut TableWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let height = &element.height;
        rebuild_child(
            &prev.scroller(height),
            &self.scroller(height),
            &mut element.viewport,
            ctx,
        )
    }

    fn teardown(&self, element: &mut TableWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.scroller(&element.height), &mut element.viewport, ctx);
    }
}

impl Widget for TableWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let width = if bc.max().width.is_finite() {
            bc.max().width
        } else {
            FALLBACK_WIDTH
        };
        // The viewport takes its height from its constraints; the grid inside
        // reports the height it needs, and a change is laid out again at once.
        let before = self.height.get();
        self.viewport
            .layout_child(ctx, &BoxConstraints::tight(Size::new(width, before)));
        let after = self.height.get();
        if (after - before).abs() > f64::EPSILON {
            self.viewport
                .layout_child(ctx, &BoxConstraints::tight(Size::new(width, after)));
        }
        self.viewport.set_origin(Point::ORIGIN);
        bc.constrain(Size::new(width, after))
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        self.viewport.paint_child(ctx, scene);
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        route_event_single(&mut self.viewport, ctx, event)
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        self.viewport.semantics_child(ctx);
    }

    frust::authoring::visit_children!(viewport);
}

/// The grid of cells inside the scroll view.
struct GridView<State: 'static> {
    cells: Rc<Cells>,
    height: Rc<Cell<f64>>,
    _state: PhantomData<fn(&mut State)>,
}

/// The retained widget for a [`GridView`].
pub(crate) struct GridWidget {
    cells: Rc<Cells>,
    pods: Vec<ChildPod>,
    height: Rc<Cell<f64>>,
    /// Left edge of every column, then the grid's right edge.
    col_x: Vec<f64>,
    /// Top of every row, then the grid's bottom edge.
    row_y: Vec<f64>,
    /// Column widths kept while streaming (see the module docs).
    floor: Vec<f64>,
}

impl<State: 'static> View<State> for GridView<State> {
    type Element = GridWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> GridWidget {
        GridWidget {
            cells: self.cells.clone(),
            pods: self
                .cells
                .cells
                .iter()
                .map(|cell| build_child::<State, _>(cell, ctx))
                .collect(),
            height: self.height.clone(),
            col_x: Vec::new(),
            row_y: Vec::new(),
            floor: Vec::new(),
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut GridWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        element.height = self.height.clone();
        let (old, new) = (&prev.cells.cells, &self.cells.cells);
        let mut flags = ChangeFlags::NONE;
        if prev.cells.columns != self.cells.columns
            || prev.cells.alignments != self.cells.alignments
            || prev.cells.streaming != self.cells.streaming
            || old.len() != new.len()
        {
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        if prev.cells.columns != self.cells.columns {
            element.floor.clear();
        }
        let common = old.len().min(new.len());
        for (index, pod) in element.pods.iter_mut().enumerate().take(common) {
            flags |= rebuild_child::<State, _>(&old[index], &new[index], pod, ctx);
        }
        for (index, cell) in old.iter().enumerate().skip(common) {
            teardown_child::<State, _>(cell, &mut element.pods[index], ctx);
        }
        element.pods.truncate(common);
        for cell in &new[common..] {
            element.pods.push(build_child::<State, _>(cell, ctx));
        }
        element.cells = self.cells.clone();
        flags
    }

    fn teardown(&self, element: &mut GridWidget, ctx: &mut BuildCtx<'_>) {
        for (cell, pod) in self.cells.cells.iter().zip(&mut element.pods) {
            teardown_child::<State, _>(cell, pod, ctx);
        }
    }
}

impl GridWidget {
    fn columns(&self) -> usize {
        self.cells.columns
    }

    /// Column widths from the cell sizes, ratcheted while streaming.
    fn widths(&mut self, sizes: &[Size]) -> Vec<f64> {
        let columns = self.columns();
        let mut widths = vec![COL_MIN; columns];
        for (index, size) in sizes.iter().enumerate() {
            let column = index % columns;
            let wanted = (size.width + CELL_PAD_X * 2.0).clamp(COL_MIN, COL_MAX);
            widths[column] = widths[column].max(wanted);
        }
        if self.cells.streaming {
            for (width, floor) in widths.iter_mut().zip(&self.floor) {
                *width = width.max(*floor);
            }
            self.floor.clone_from(&widths);
        } else {
            self.floor.clear();
        }
        widths
    }
}

impl Widget for GridWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let columns = self.columns();
        if columns == 0 {
            self.col_x = vec![0.0];
            self.row_y = vec![0.0];
            self.height.set(0.0);
            return bc.constrain(Size::ZERO);
        }
        let inner = COL_MAX - CELL_PAD_X * 2.0;
        let cell_bc = BoxConstraints::loose(Size::new(inner, f64::INFINITY));
        let sizes: Vec<Size> = self
            .pods
            .iter_mut()
            .map(|pod| pod.layout_child(ctx, &cell_bc))
            .collect();
        let widths = self.widths(&sizes);

        self.col_x = std::iter::once(0.0)
            .chain(widths.iter().scan(0.0, |x, w| {
                *x += w;
                Some(*x)
            }))
            .collect();
        self.row_y = vec![0.0];
        for row in sizes.chunks(columns) {
            let tallest = row.iter().map(|s| s.height).fold(0.0, f64::max);
            let top = self.row_y.last().copied().unwrap_or(0.0);
            self.row_y.push(top + tallest + CELL_PAD_Y * 2.0);
        }

        for (index, pod) in self.pods.iter_mut().enumerate() {
            let (row, column) = (index / columns, index % columns);
            let free = (widths[column] - CELL_PAD_X * 2.0 - sizes[index].width).max(0.0);
            let shift = match self.cells.alignment(column) {
                MdAlignment::Center => free / 2.0,
                MdAlignment::Right => free,
                MdAlignment::None | MdAlignment::Left => 0.0,
            };
            pod.set_origin(Point::new(
                self.col_x[column] + CELL_PAD_X + shift,
                self.row_y[row] + CELL_PAD_Y,
            ));
        }

        let natural = Size::new(
            self.col_x.last().copied().unwrap_or(0.0),
            self.row_y.last().copied().unwrap_or(0.0),
        );
        self.height.set(natural.height);
        bc.constrain(natural)
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let theme = Theme::from_paint_ctx(ctx);
        let border = style::border(theme);
        let header_bg = theme.map_or(FALLBACK_HEADER_BG, |t| t.scheme().surface_container_high);
        let o = ctx.origin();
        let width = self.col_x.last().copied().unwrap_or(0.0);
        if let [top, below, ..] = self.row_y[..] {
            scene.fill_rect(o, Size::new(width, below - top), header_bg);
        }
        for pod in &mut self.pods {
            pod.paint_child(ctx, scene);
        }
        let rows = self.row_y.len();
        for (index, y) in self.row_y.iter().enumerate() {
            // Separators between rows, plus the table's top and bottom edges.
            let edge = index == 0 || index + 1 == rows;
            let line = if edge || index == 1 {
                style::BORDER_WIDTH
            } else {
                style::BORDER_WIDTH * 0.5
            };
            scene.stroke_line(
                Point::new(o.x, o.y + y),
                Point::new(o.x + width, o.y + y),
                line,
                border,
            );
        }
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        route_event(&mut self.pods, ctx, event)
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        let columns = self.columns();
        let rows = self.cells.rows();
        ctx.push_container(
            Role::Table,
            |node| {
                node.set_row_count(rows);
                node.set_column_count(columns);
            },
            |ctx| {
                for (row, pods) in self.pods.chunks(columns.max(1)).enumerate() {
                    ctx.push_container(
                        Role::Row,
                        |node| node.set_row_index(row),
                        |ctx| {
                            for (column, pod) in pods.iter().enumerate() {
                                let role = if row == 0 {
                                    Role::ColumnHeader
                                } else {
                                    Role::Cell
                                };
                                ctx.push_container(
                                    role,
                                    |node| {
                                        node.set_row_index(row);
                                        node.set_column_index(column);
                                    },
                                    |ctx| pod.semantics_child(ctx),
                                );
                            }
                        },
                    );
                }
            },
        );
    }

    frust::authoring::visit_children!(pods);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(columns: usize, streaming: bool) -> GridWidget {
        GridWidget {
            cells: Rc::new(Cells {
                alignments: Vec::new(),
                columns,
                cells: Vec::new(),
                streaming,
            }),
            pods: Vec::new(),
            height: Rc::new(Cell::new(0.0)),
            col_x: Vec::new(),
            row_y: Vec::new(),
            floor: Vec::new(),
        }
    }

    fn sized(widths: &[f64]) -> Vec<Size> {
        widths.iter().map(|w| Size::new(*w, 10.0)).collect()
    }

    #[test]
    fn columns_clamp_between_the_minimum_and_the_maximum() {
        let mut g = grid(3, false);
        let w = g.widths(&sized(&[1.0, 100.0, 5000.0, 2.0, 20.0, 3.0]));
        assert_eq!(w, [COL_MIN, 100.0 + CELL_PAD_X * 2.0, COL_MAX]);
    }

    #[test]
    fn a_streaming_table_never_narrows_a_column() {
        let mut g = grid(2, true);
        let wide = g.widths(&sized(&[100.0, 10.0]));
        let after = g.widths(&sized(&[60.0, 80.0]));
        assert_eq!(after[0], wide[0], "kept while streaming");
        assert_eq!(after[1], 80.0 + CELL_PAD_X * 2.0, "still widens");
        let mut done = grid(2, false);
        done.floor = g.floor.clone();
        let settled = done.widths(&sized(&[60.0, 80.0]));
        assert_eq!(settled[0], 60.0 + CELL_PAD_X * 2.0, "settles once done");
    }

    #[test]
    fn short_rows_are_padded_to_the_column_count() {
        let t = table::<()>(
            vec![MdAlignment::Left, MdAlignment::Right, MdAlignment::None],
            vec![frust::rich_text([frust::span("h")])],
            vec![vec![], vec![frust::rich_text([frust::span("a")]); 2]],
        );
        assert_eq!(t.cells.columns, 3);
        assert_eq!(t.cells.cells.len(), 9);
        assert_eq!(t.cells.rows(), 3);
        assert_eq!(t.cells.alignment(1), MdAlignment::Right);
        assert_eq!(t.cells.alignment(7), MdAlignment::None);
    }
}
