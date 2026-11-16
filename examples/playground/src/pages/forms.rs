//! Forms section: the fixtures for the safe-area / keyboard / back device gate.
//!
//! Top to bottom:
//!
//! 1. a status line: the field whose caret last reported, and the depth of the full-width
//!    navigator;
//! 2. two normal fields, A and B, stacked at the top (focus moves between them by tap, or by the
//!    keyboard's Return or Next key if the platform delivers one);
//! 3. an obscured field, C, classified as a password input;
//! 4. a "Push full-width route" button and a navigator that spans the page edge to edge, with no
//!    horizontal padding of its own (the pushed pages show their depth and their own Push);
//! 5. a "Search bar" section (an icon that expands into a narrow autofocused field, a clear
//!    button inside `keep_focus`, collapse on blur, and a status block of the field's live
//!    focus, composition, change and submit reports) and a "Long-press drag" section (a
//!    scrolling grid of 60 cells whose long press selects a cell and whose drag after the press
//!    extends the selection). A "Focus field" button under the page status line requests focus
//!    on the search field through a `FocusHandle`;
//! 6. numbered filler rows, so the last field sits near the bottom of the page, where the
//!    on-screen keyboard opens;
//! 7. field D at the very end of the page, just above the shell's bottom navigation bar once
//!    the page is scrolled to its end. At phone width the shell holds the page and that bar above
//!    the keyboard (a keyboard-aware safe area), so with the keyboard open the page body ends at
//!    the keyboard's top edge and the focused field scrolls into view above it.
//!
//! # What the framework cannot do here
//!
//! - **Fields A to D have no focus wiring.** They carry no `FocusHandle` and no
//!   `on_focus_change`, so A's Return cannot move focus to B and the top status line shows the
//!   field whose caret reported last, not a live "focused now". Return keeps its normal behaviour
//!   (it fires `on_submit`, which here only records a submit count); move focus by tapping B.
//!   The search field below uses both, and shows live focus.
//! - **No bottom anchoring.** The shell hosts every page in a vertical scroll view, so D is the
//!   last row of scrolling content rather than pinned to the bottom of the window.
//!
//! # Where the full-width navigator sits
//!
//! The shell hosts the page in a scroll view with no inset of its own, and this page adds no
//! page-level padding, so the navigator spans the scroll view's full width. It is at the
//! screen's left edge in portrait at phone width (the bottom navigation bar is in use). At the
//! wide breakpoint the shell puts a side rail to the left of the page, and the navigator then
//! starts at the rail's right edge. The shell's safe area also insets the page in landscape by
//! the display cutout.

use frust::{
    ButtonStyle, Color, EdgeInsets, FocusHandle, GestureDetector, Get, NavigatorController,
    NavigatorView, Padding, RouteObserver, RwSignal, Set, SizedBox, Theme, Update, View, button,
    column, container, either, icon_button, icons, keep_focus, navigator, row, scroll_view,
    semantics, text, text_input, use_context, views,
};

use crate::PlaygroundState;

/// Number of numbered filler rows between the password field and field D.
const FILLER_ROWS: usize = 18;

/// Height of the full-width navigator, in logical pixels: a navigator needs a bounded height
/// inside the shell's scroll view.
const NAV_H: f64 = 160.0;

/// Horizontal inset of ordinary page content (the navigator gets none).
const SIDE: f64 = 16.0;

/// Columns in the long-press grid.
const GRID_COLS: usize = 6;

/// Cells in the long-press grid (ten rows of [`GRID_COLS`]).
const GRID_CELLS: usize = 60;

/// One grid cell's width and height in logical pixels, padding included: the long-press
/// detector wraps exactly this box, so a press position inside it maps to a cell by division.
const CELL_W: f64 = 56.0;
const CELL_H: f64 = 80.0;

/// The grid viewport's height: the grid is ten rows of [`CELL_H`], about three viewports tall.
const GRID_VIEW_H: f64 = 260.0;

/// Width of the search field, narrow enough that its placeholder overflows.
const SEARCH_W: f64 = 150.0;

/// A point in the grid's own coordinates (origin at the top-left of the first cell).
type GridPoint = (f64, f64);

/// The "Forms" section's state, owned by [`PlaygroundState`]. Every field is a
/// clone-shares-one-handle value.
#[derive(Clone)]
pub struct FormsState {
    /// Field A's contents.
    pub a: RwSignal<String>,
    /// Field B's contents.
    pub b: RwSignal<String>,
    /// Field C's contents (the obscured field).
    pub c: RwSignal<String>,
    /// Field D's contents (the bottom field).
    pub d: RwSignal<String>,
    /// The label of the field whose caret reported last.
    pub focus: RwSignal<&'static str>,
    /// How many times Return fired a field's `on_submit`.
    pub submits: RwSignal<u32>,
    /// The full-width navigator's controller.
    pub nav: NavigatorController<PlaygroundState>,
    /// Feeds the status line's depth.
    pub route: RouteObserver,
    /// Whether the search bar is expanded into its field.
    pub search_open: RwSignal<bool>,
    /// The search field's contents.
    pub search_text: RwSignal<String>,
    /// Whether the search field reported focus.
    pub search_focused: RwSignal<bool>,
    /// Whether the search field reported an IME composition in progress.
    pub search_composing: RwSignal<bool>,
    /// The value of the search field's last `on_change`.
    pub search_last_change: RwSignal<String>,
    /// How many times Return fired the search field's `on_submit`.
    pub search_submits: RwSignal<u32>,
    /// The text of the search field's last `on_submit`.
    pub search_last_submit: RwSignal<String>,
    /// The handle the "Focus field" button requests focus through.
    pub search_handle: FocusHandle,
    /// Which grid cells are selected.
    pub grid_selected: RwSignal<Vec<bool>>,
    /// Whether a long-press selection is in progress.
    pub grid_active: RwSignal<bool>,
    /// Where the selection drag last reported, in grid coordinates.
    pub grid_last: RwSignal<Option<GridPoint>>,
    /// The last `offset_from_origin` a long-press move reported.
    pub grid_offset: RwSignal<(f64, f64)>,
}

impl FormsState {
    /// Fresh state: empty fields, no focus report, an empty navigator.
    pub fn new() -> Self {
        FormsState {
            a: RwSignal::new(String::new()),
            b: RwSignal::new(String::new()),
            c: RwSignal::new(String::new()),
            d: RwSignal::new(String::new()),
            focus: RwSignal::new("none"),
            submits: RwSignal::new(0),
            nav: NavigatorController::new(),
            route: RouteObserver::new(),
            search_open: RwSignal::new(false),
            search_text: RwSignal::new(String::new()),
            search_focused: RwSignal::new(false),
            search_composing: RwSignal::new(false),
            search_last_change: RwSignal::new(String::new()),
            search_submits: RwSignal::new(0),
            search_last_submit: RwSignal::new(String::new()),
            search_handle: FocusHandle::new(),
            grid_selected: RwSignal::new(vec![false; GRID_CELLS]),
            grid_active: RwSignal::new(false),
            grid_last: RwSignal::new(None),
            grid_offset: RwSignal::new((0.0, 0.0)),
        }
    }
}

impl Default for FormsState {
    fn default() -> Self {
        Self::new()
    }
}

fn theme() -> Theme {
    use_context::<Theme>().unwrap_or_else(frust_material::baseline)
}

fn accent() -> Color {
    theme().scheme().primary
}

fn muted() -> Color {
    theme().scheme().on_surface_variant
}

/// Inset a content item from the screen edges.
fn inset<V: View<PlaygroundState>>(view: V) -> impl View<PlaygroundState> {
    Padding(EdgeInsets::symmetric(SIDE, 2.0), view)
}

/// A muted caption line.
fn caption(content: &str) -> impl View<PlaygroundState> + use<> {
    inset(text(content.to_string()).size(11.0).color(muted()))
}

/// A label line above a field.
fn label(content: &'static str) -> impl View<PlaygroundState> + use<> {
    inset(text(content).size(13.0).color(accent()))
}

/// A field: `value` mirrored into `signal`, reporting its label on every caret report and counting
/// Return presses.
fn field(
    name: &'static str,
    value: RwSignal<String>,
    focus: RwSignal<&'static str>,
    submits: RwSignal<u32>,
) -> frust::TextInputView<PlaygroundState> {
    text_input(
        value.get(),
        move |_state: &mut PlaygroundState, next: String| value.set(next),
    )
    .placeholder(format!("Field {name}"))
    .on_caret_change(move |_state: &mut PlaygroundState, _info| focus.set(name))
    .on_submit(move |_state: &mut PlaygroundState, _text: String| {
        submits.update(|count| *count += 1);
    })
}

/// A pushed page showing its depth, with its own Push. The page pads itself, so the navigator
/// around it stays flush with the screen edges.
fn detail_page(
    nav: NavigatorController<PlaygroundState>,
    level: usize,
) -> impl View<PlaygroundState> {
    let host = nav.clone();
    Padding(
        EdgeInsets::all(12.0),
        column()
            .child(
                text(format!("Route depth {level}"))
                    .size(13.0)
                    .color(accent()),
            )
            .child(SizedBox(None, Some(8.0)))
            .child(
                button("Push", move |_state: &mut PlaygroundState| {
                    push_route(&host, level + 1);
                })
                .style(ButtonStyle::Primary)
                .small(),
            ),
    )
}

/// Push depth `level` onto `nav` with the default push options, as the Tabs page does.
fn push_route(nav: &NavigatorController<PlaygroundState>, level: usize) {
    let host = nav.clone();
    nav.push(move || detail_page(host.clone(), level));
}

/// The full-width navigator, observed for the status line's depth.
fn full_width_navigator(forms: &FormsState) -> NavigatorView<PlaygroundState> {
    let nav = forms.nav.clone();
    forms.route.observe(navigator(&nav, || {
        Padding(
            EdgeInsets::all(12.0),
            column()
                .child(text("Route depth 1 (root)").size(13.0).color(accent()))
                .child(
                    text("Press Push full-width route, then swipe back from the left edge.")
                        .size(11.0),
                ),
        )
    }))
}

/// "yes" or "no" for a status line.
fn yes_no(on: bool) -> &'static str {
    if on { "yes" } else { "no" }
}

/// The grid cell under `point` (grid coordinates), or `None` outside the grid.
fn cell_at(point: GridPoint) -> Option<usize> {
    let (x, y) = point;
    if x < 0.0 || y < 0.0 {
        return None;
    }
    let (col, row) = ((x / CELL_W) as usize, (y / CELL_H) as usize);
    let index = row * GRID_COLS + col;
    (col < GRID_COLS && index < GRID_CELLS).then_some(index)
}

/// Every cell the straight path `from` to `to` crosses, sampled finer than a cell so a fast
/// move between two frames does not skip one.
fn cells_crossed(from: GridPoint, to: GridPoint) -> Vec<usize> {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let steps = ((dx.hypot(dy) / (CELL_W.min(CELL_H) / 4.0)).ceil() as usize).max(1);
    let mut cells = Vec::new();
    for step in 0..=steps {
        let t = step as f64 / steps as f64;
        if let Some(cell) = cell_at((from.0 + dx * t, from.1 + dy * t))
            && !cells.contains(&cell)
        {
            cells.push(cell);
        }
    }
    cells
}

/// See the page-fn contract in [`crate::pages`].
pub fn page(state: &PlaygroundState) -> impl View<PlaygroundState> {
    let forms = &state.forms;
    let status = format!(
        "focus (last caret report): {}   route depth: {}   Return presses: {}",
        forms.focus.get(),
        forms.route.depth(),
        forms.submits.get(),
    );
    let (focus, submits) = (forms.focus, forms.submits);
    let push_nav = forms.nav.clone();

    let (search_open, search_text) = (forms.search_open, forms.search_text);
    let (search_focused, search_composing) = (forms.search_focused, forms.search_composing);
    let (search_last_change, search_submits) = (forms.search_last_change, forms.search_submits);
    let search_last_submit = forms.search_last_submit;
    let focus_button_handle = forms.search_handle.clone();
    let field_handle = forms.search_handle.clone();
    let search_status = format!(
        "focused: {}   composing: {}   last change: \"{}\"   submits: {}   last submit: \"{}\"",
        yes_no(search_focused.get()),
        yes_no(search_composing.get()),
        search_last_change.get(),
        search_submits.get(),
        search_last_submit.get(),
    );
    let search_bar = either(
        search_open.get(),
        move || {
            row()
                .child(
                    SizedBox::<PlaygroundState>(Some(SEARCH_W), None).child(
                        text_input(
                            search_text.get(),
                            move |_state: &mut PlaygroundState, next: String| {
                                search_last_change.set(next.clone());
                                search_text.set(next);
                            },
                        )
                        .placeholder(
                            "Search every widget, plugin, page and fixture in the catalogue",
                        )
                        .autofocus(true)
                        .focus_handle(field_handle)
                        .on_focus_change(move |_state: &mut PlaygroundState, focused| {
                            search_focused.set(focused);
                            if !focused {
                                search_open.set(false);
                            }
                        })
                        .on_composing_change(move |_state: &mut PlaygroundState, composing| {
                            search_composing.set(composing);
                        })
                        .on_submit(
                            move |_state: &mut PlaygroundState, submitted: String| {
                                search_submits.update(|count| *count += 1);
                                search_last_submit.set(submitted);
                            },
                        ),
                    ),
                )
                .child(keep_focus(icon_button(
                    icons::CLOSE,
                    "Clear search",
                    move |_state: &mut PlaygroundState| search_text.set(String::new()),
                )))
        },
        move || {
            row().child(icon_button(
                icons::SEARCH,
                "Open search",
                move |_state: &mut PlaygroundState| search_open.set(true),
            ))
        },
    );

    let (grid_selected, grid_active) = (forms.grid_selected, forms.grid_active);
    let (grid_last, grid_offset) = (forms.grid_last, forms.grid_offset);
    let selected_now = grid_selected.get();
    let selected_count = selected_now.iter().filter(|on| **on).count();
    let (offset_x, offset_y) = grid_offset.get();
    let grid_status = format!(
        "selected: {selected_count}   dragging: {}   last offset: ({offset_x:.0}, {offset_y:.0})",
        yes_no(grid_active.get()),
    );
    let scheme = *theme().scheme();
    let grid = column().children(views((0..GRID_CELLS / GRID_COLS).map(|grid_row| {
        row().children(views((0..GRID_COLS).map(|grid_col| {
            let index = grid_row * GRID_COLS + grid_col;
            let origin = (grid_col as f64 * CELL_W, grid_row as f64 * CELL_H);
            let on = selected_now[index];
            semantics(
                GestureDetector(Padding(
                    EdgeInsets::all(2.0),
                    container(text((index + 1).to_string()).size(13.0))
                        .fill(if on {
                            scheme.primary_container
                        } else {
                            scheme.secondary_container
                        })
                        .radius(6.0)
                        .size_centered(CELL_W - 4.0, CELL_H - 4.0),
                ))
                .on_long_press(move |_state: &mut PlaygroundState| {
                    grid_active.set(true);
                    grid_last.set(None);
                    grid_selected.update(|cells| {
                        cells.iter_mut().for_each(|cell| *cell = false);
                        cells[index] = true;
                    });
                })
                .on_long_press_move_update(
                    move |_state: &mut PlaygroundState, details: frust::LongPressMoveDetails| {
                        let here = (
                            origin.0 + details.local_position.x,
                            origin.1 + details.local_position.y,
                        );
                        let from = grid_last.get().unwrap_or(here);
                        grid_selected.update(|cells| {
                            for cell in cells_crossed(from, here) {
                                cells[cell] = true;
                            }
                        });
                        grid_last.set(Some(here));
                        grid_offset
                            .set((details.offset_from_origin.x, details.offset_from_origin.y));
                    },
                )
                .on_long_press_end(
                    move |_state: &mut PlaygroundState, _details: frust::LongPressEndDetails| {
                        grid_active.set(false);
                    },
                )
                .on_long_press_cancel(move |_state: &mut PlaygroundState| {
                    grid_active.set(false);
                }),
            )
            .label(format!("Cell {}", index + 1))
            .value(if on { "selected" } else { "not selected" })
        })))
    })));

    column()
        .child(SizedBox(None, Some(8.0)))
        .child(inset(text("Forms").size(13.0).color(accent())))
        .child(inset(text(status).size(12.0)))
        .child(inset(
            button("Focus field", move |_state: &mut PlaygroundState| {
                search_open.set(true);
                focus_button_handle.request();
            })
            .style(ButtonStyle::Secondary)
            .small(),
        ))
        .child(caption(
            "A downward drag on the page dismisses the keyboard; an upward drag \
             scrolls the page while you type.",
        ))
        .child(SizedBox(None, Some(8.0)))
        .child(label("A and B: two normal fields"))
        .child(inset(field("A", forms.a, focus, submits)))
        .child(inset(field("B", forms.b, focus, submits)))
        .child(caption(
            "Move focus from A to B by tapping B, then try the keyboard's Return or \
             Next key. The framework has no programmatic focus, so Return on A does \
             not move focus to B; it only counts a press.",
        ))
        .child(SizedBox(None, Some(8.0)))
        .child(label("C: the password field"))
        .child(inset(field("C", forms.c, focus, submits).obscured(true)))
        .child(caption(
            "C is obscured and reports itself as a password input to accessibility.",
        ))
        .child(SizedBox(None, Some(8.0)))
        .child(inset(
            button(
                "Push full-width route",
                move |_state: &mut PlaygroundState| {
                    let depth = push_nav.depth();
                    push_route(&push_nav, depth + 1);
                },
            )
            .style(ButtonStyle::Primary)
            .small(),
        ))
        .child(caption(
            "The navigator below has no horizontal padding or inset. It is at the \
             screen's left edge in portrait at phone width; with the wide-width side \
             rail it starts at the rail's right edge, and in landscape the safe area \
             insets it by the display cutout.",
        ))
        .child(SizedBox::<PlaygroundState>(None, Some(NAV_H)).child(full_width_navigator(forms)))
        .child(SizedBox(None, Some(8.0)))
        .child(label("Search bar"))
        .child(inset(search_bar))
        .child(inset(text(search_status).size(12.0)))
        .child(caption(
            "The search icon expands into a narrow single-line field that takes focus at \
             once, so its long placeholder overflows. The X button empties the field and \
             keeps focus and the keyboard. Tapping elsewhere collapses the bar. The Focus \
             field button at the top of the page expands the bar and focuses the field.",
        ))
        .child(SizedBox(None, Some(8.0)))
        .child(label("Long-press drag"))
        .child(inset(text(grid_status).size(12.0)))
        .child(inset(
            SizedBox::<PlaygroundState>(None, Some(GRID_VIEW_H)).child(scroll_view(grid)),
        ))
        .child(caption(
            "Drag inside the grid to scroll it. Press a cell and hold: it selects. Keep \
             dragging to select every cell the pointer crosses; the grid does not scroll \
             while the selection is in progress. Release ends the selection.",
        ))
        .child(SizedBox(None, Some(8.0)))
        .child(caption(
            "Filler rows: scroll down to field D, then focus it; the page should end at the \
             top of the keyboard and keep D in view.",
        ))
        .children(views(
            (1..=FILLER_ROWS).map(|n| inset(text(format!("Filler row {n}")).size(13.0))),
        ))
        .child(SizedBox(None, Some(8.0)))
        .child(label("D: the bottom field"))
        .child(inset(field("D", forms.d, focus, submits)))
        .child(caption(
            "D is the last row of the page: it sits just above the bottom navigation bar \
             when scrolled to the end. With the keyboard open the navigation bar sits \
             directly above the keyboard, the page ends at its top edge, and the content \
             does not move down.",
        ))
        .child(SizedBox(None, Some(8.0)))
}

/// Headless drive of the page's two new sections through the real rebuild, layout, paint and
/// event pipeline, with no window and no GPU.
#[cfg(test)]
mod tests {
    use std::any::Any;

    use frust::authoring::{PaintScene, Size};
    use frust::{AnyView, GetUntracked};
    use frust_core::{
        FrameTime, InputEvent, PointerButton, PointerEvent, PointerPhase, RenderRoot,
    };
    use frust_reactive::ReactiveRuntime;
    use frust_text::TextContext;
    use kurbo::{Point, Rect};
    use peniko::Color;
    use reactive_graph::owner::Owner;

    use super::*;
    use crate::pages;

    /// A paint target that records nothing: the tests read state and layout, not pixels.
    struct Blank;

    impl PaintScene for Blank {
        fn fill_rect(&mut self, _: Point, _: Size, _: Color) {}
        fn draw_text(&mut self, _: Point, _: &str) {}
    }

    /// The page mounted alone in a window tall enough to show all of it.
    struct Harness {
        root: RenderRoot<PlaygroundState, AnyView<PlaygroundState>>,
        state: PlaygroundState,
        tcx: TextContext,
        t_ms: u64,
        _owner: Owner,
    }

    impl Harness {
        fn new() -> Self {
            let _ = ReactiveRuntime::init(std::sync::Arc::new(|| {}));
            let owner = Owner::new();
            owner.set();
            let mut harness = Harness {
                root: RenderRoot::new(),
                state: PlaygroundState::new(),
                tcx: TextContext::new(),
                t_ms: 0,
                _owner: owner,
            };
            harness.frames(3);
            harness
        }

        /// `count` frames of 16 ms each: rebuild, layout, paint.
        fn frames(&mut self, count: usize) {
            let index = pages::section_index_for("forms").expect("forms is a known section");
            let mut logic = |s: &mut PlaygroundState| pages::current(index, s);
            for _ in 0..count {
                self.t_ms += 16;
                self.root.rebuild(&mut logic, &mut self.state);
                let tcx_any: &mut dyn Any = &mut self.tcx;
                self.root
                    .layout_with_text(Size::new(390.0, 4000.0), tcx_any);
                self.root
                    .paint(&mut Blank, FrameTime::from_nanos(self.t_ms * 1_000_000));
            }
        }

        /// Frames until `ms` of paint time has passed.
        fn hold(&mut self, ms: u64) {
            self.frames(ms.div_ceil(16) as usize);
        }

        fn pointer(&mut self, phase: PointerPhase, at: Point) {
            self.root.event(
                &mut self.state,
                &InputEvent::Pointer(PointerEvent {
                    phase,
                    position: at,
                    button: PointerButton::Primary,
                }),
            );
        }

        fn tap(&mut self, at: Point) {
            self.pointer(PointerPhase::Down, at);
            self.pointer(PointerPhase::Up, at);
            self.frames(3);
        }

        /// The window bounds of the `n`-th laid-out widget whose type name contains `name`.
        fn bounds(&self, name: &str, n: usize) -> Rect {
            self.root
                .inspect()
                .iter()
                .filter(|node| node.type_name.contains(name))
                .nth(n)
                .map(|node| node.bounds)
                .unwrap_or_else(|| panic!("no laid-out {name} number {n}"))
        }

        fn count(&self, name: &str) -> usize {
            self.root
                .inspect()
                .iter()
                .filter(|node| node.type_name.contains(name))
                .count()
        }

        /// The center of grid cell `index`, read from the layout of the first cell.
        fn cell_center(&self, index: usize) -> Point {
            let first = self.bounds("GestureDetector", 0);
            Point::new(
                first.x0 + (index % GRID_COLS) as f64 * CELL_W + CELL_W / 2.0,
                first.y0 + (index / GRID_COLS) as f64 * CELL_H + CELL_H / 2.0,
            )
        }

        fn selected(&self) -> Vec<usize> {
            let cells = self.state.forms.grid_selected.get_untracked();
            (0..GRID_CELLS).filter(|i| cells[*i]).collect()
        }
    }

    /// Press at `from`, hold past the long-press threshold, move through `path` in order and
    /// release at its last point.
    fn long_press_drag(h: &mut Harness, from: Point, path: &[Point]) {
        h.pointer(PointerPhase::Down, from);
        h.hold(600);
        h.frames(2);
        for at in path {
            h.pointer(PointerPhase::Move, *at);
            h.frames(1);
        }
        h.pointer(PointerPhase::Up, *path.last().unwrap_or(&from));
        h.frames(3);
    }

    #[test]
    fn search_bar_opens_focused_clears_without_blur_and_collapses_on_blur() {
        let mut h = Harness::new();
        let forms = h.state.forms.clone();
        let fields_closed = h.count("TextInputWidget");
        assert!(!forms.search_open.get_untracked());

        h.tap(h.bounds("IconButtonWidget", 0).center());
        assert!(
            forms.search_open.get_untracked(),
            "the icon expands the bar"
        );
        assert_eq!(h.count("TextInputWidget"), fields_closed + 1);
        assert!(
            forms.search_focused.get_untracked() && forms.search_handle.is_focused(),
            "the expanded field autofocuses"
        );

        forms.search_text.set("hello".to_string());
        h.frames(2);
        h.tap(h.bounds("IconButtonWidget", 0).center());
        assert_eq!(forms.search_text.get_untracked(), "", "X empties the field");
        assert!(
            forms.search_focused.get_untracked() && forms.search_open.get_untracked(),
            "the clear button inside keep_focus leaves the field focused"
        );

        forms.search_handle.release();
        h.frames(3);
        assert!(!forms.search_focused.get_untracked());
        assert!(
            !forms.search_open.get_untracked(),
            "losing focus collapses the bar"
        );
        assert_eq!(h.count("TextInputWidget"), fields_closed);

        h.tap(h.bounds("ButtonWidget", 0).center());
        assert!(
            forms.search_open.get_untracked() && forms.search_focused.get_untracked(),
            "the Focus field button expands the bar and focuses the field"
        );
    }

    #[test]
    fn long_press_selects_and_the_drag_extends_it_without_scrolling() {
        let mut h = Harness::new();
        let forms = h.state.forms.clone();
        let top_before = h.bounds("GestureDetectorWidget", 0).y0;
        assert_eq!(
            h.count("GestureDetectorWidget"),
            GRID_CELLS,
            "one detector per cell"
        );

        let start = h.cell_center(0);
        let end = h.cell_center(8);
        let path = [h.cell_center(1), h.cell_center(2), end];
        long_press_drag(&mut h, start, &path);

        assert_eq!(
            h.selected(),
            vec![0, 1, 2, 8],
            "start cell plus every cell crossed"
        );
        assert!(
            !forms.grid_active.get_untracked(),
            "release ends the selection"
        );
        let (dx, dy) = forms.grid_offset.get_untracked();
        assert!(
            (dx - (end.x - start.x)).abs() < 0.5 && (dy - (end.y - start.y)).abs() < 0.5,
            "the last offset is the distance from the press: ({dx}, {dy})"
        );
        assert_eq!(
            h.bounds("GestureDetectorWidget", 0).y0,
            top_before,
            "the grid did not scroll under the selection drag"
        );

        let thirteen = h.cell_center(13);
        long_press_drag(&mut h, thirteen, &[thirteen]);
        assert_eq!(
            h.selected(),
            vec![13],
            "a new long press starts a new selection"
        );
    }

    #[test]
    fn a_drag_before_the_threshold_scrolls_the_grid_and_selects_nothing() {
        let mut h = Harness::new();
        let top_before = h.bounds("GestureDetectorWidget", 0).y0;
        let from = h.cell_center(7);
        h.pointer(PointerPhase::Down, from);
        h.frames(1);
        for step in 1..=8 {
            h.pointer(
                PointerPhase::Move,
                Point::new(from.x, from.y - 12.0 * step as f64),
            );
            h.frames(1);
        }
        h.pointer(PointerPhase::Up, Point::new(from.x, from.y - 96.0));
        h.frames(3);
        assert!(h.selected().is_empty(), "no long press fired");
        assert!(
            h.bounds("GestureDetectorWidget", 0).y0 < top_before,
            "the grid scrolled"
        );
    }
}
