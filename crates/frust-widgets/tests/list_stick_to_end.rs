//! `ListView::stick_to_end` — a keyed variable-extent list that follows its
//! end while the last row grows and rows are appended, lets go when the user
//! scrolls away, comes back when the user returns, and reports each transition
//! through `on_stick_change`.
//!
//! Every frame here is a full rebuild → layout → paint, and "the end is in
//! view" is read off what paint actually drew: the bottom of the last row's
//! painted rect sits exactly on the viewport's bottom edge. The rows are
//! local fixtures (`frust_widgets::test_support` is behind a non-default
//! feature, so the integration tests keep their own, as the other files here
//! do): a row has an *area* and is as tall as that area wraps to at the
//! list's width, so a width change re-wraps it.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use frust_core::{
    BoxConstraints, BuildCtx, ChangeFlags, FrameTime, InputEvent, LayoutCtx, PaintCtx,
    PaintOutcome, PaintScene, PointerButton, PointerEvent, PointerPhase, RenderRoot, ScrollDelta,
    TOUCH_SLOP, View, Widget,
};
use frust_widgets::{ChildKey, ListView, ListViewWidget, ScrollController};
use kurbo::{Point, Size};

/// The list's estimate for an unmeasured row — deliberately unequal to the
/// rows' real heights, so the estimated and measured regions differ.
const ESTIMATE: f64 = 50.0;
const WIDTH: f64 = 320.0;
const WINDOW: Size = Size::new(WIDTH, 400.0);
const THRESHOLD: f64 = ListViewWidget::STICK_TO_END_THRESHOLD_PX;

/// A deterministic row height at [`WIDTH`]: 30..=90 px in 10 px steps.
fn height_of(id: u64) -> f64 {
    30.0 + ((id * 37) % 7) as f64 * 10.0
}

/// The app state: every `on_stick_change` delivery, in order.
#[derive(Default)]
struct Chat {
    changes: Vec<bool>,
}

/// Where each row was painted last frame: `id -> (top, height)`.
type PaintLog = Rc<RefCell<HashMap<u64, (f64, f64)>>>;

/// A row of a fixed *area*: as tall as the area wraps to at the width it is
/// laid out at, the way a text bubble re-wraps.
struct Bubble {
    id: u64,
    area: f64,
    painted: PaintLog,
}

struct BubbleWidget {
    id: u64,
    area: f64,
    painted: PaintLog,
}

impl View<Chat> for Bubble {
    type Element = BubbleWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> BubbleWidget {
        BubbleWidget {
            id: self.id,
            area: self.area,
            painted: self.painted.clone(),
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut BubbleWidget,
        _: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        element.id = self.id;
        if prev.area != self.area {
            element.area = self.area;
            return ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        ChangeFlags::NONE
    }
}

impl Widget for BubbleWidget {
    fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let width = bc.max().width;
        bc.constrain(Size::new(width, self.area / width))
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let (origin, size) = (ctx.origin(), ctx.size());
        self.painted
            .borrow_mut()
            .insert(self.id, (origin.y, size.height));
        scene.fill_rect(origin, size, peniko::Color::BLACK);
    }
}

struct NullScene;

impl PaintScene for NullScene {
    fn fill_rect(&mut self, _origin: Point, _size: Size, _color: peniko::Color) {}
    fn draw_text(&mut self, _origin: Point, _text: &str) {}
}

/// One test's list: the mutable transcript, the build/paint probes, the
/// render root and its clock.
struct Harness {
    /// `(id, area)` per row, in order.
    rows: Rc<RefCell<Vec<(u64, f64)>>>,
    painted: PaintLog,
    /// Every index the builder was asked for.
    built: Rc<RefCell<Vec<usize>>>,
    controller: ScrollController,
    stick: Rc<Cell<bool>>,
    root: RenderRoot<Chat, ListView<Chat>>,
    state: Chat,
    window: Size,
    clock_ms: f64,
}

impl Harness {
    fn new(ids: impl IntoIterator<Item = u64>) -> Self {
        let rows = ids
            .into_iter()
            .map(|id| (id, height_of(id) * WIDTH))
            .collect();
        Self {
            rows: Rc::new(RefCell::new(rows)),
            painted: Rc::new(RefCell::new(HashMap::new())),
            built: Rc::new(RefCell::new(Vec::new())),
            controller: ScrollController::new(),
            stick: Rc::new(Cell::new(true)),
            root: RenderRoot::new(),
            state: Chat::default(),
            window: WINDOW,
            clock_ms: 0.0,
        }
    }

    fn logic(&self) -> impl FnMut(&mut Chat) -> ListView<Chat> + use<> {
        let rows = self.rows.clone();
        let painted = self.painted.clone();
        let built = self.built.clone();
        let controller = self.controller.clone();
        let stick = self.stick.clone();
        move |_: &mut Chat| {
            let snapshot: Rc<Vec<(u64, f64)>> = Rc::new(rows.borrow().clone());
            let (keys, items) = (snapshot.clone(), snapshot.clone());
            let (painted, built) = (painted.clone(), built.clone());
            ListView::builder_keyed(
                snapshot.len(),
                ESTIMATE,
                move |i| ChildKey::new(keys[i].0),
                move |i| {
                    built.borrow_mut().push(i);
                    Bubble {
                        id: items[i].0,
                        area: items[i].1,
                        painted: painted.clone(),
                    }
                },
            )
            .estimated_item_extent(ESTIMATE)
            .stick_to_end(stick.get())
            .on_stick_change(|chat: &mut Chat, stuck| chat.changes.push(stuck))
            .controller(controller.clone())
        }
    }

    /// One full rebuild → layout → paint frame.
    fn frame(&mut self) -> PaintOutcome {
        self.painted.borrow_mut().clear();
        let mut logic = self.logic();
        self.root.rebuild(&mut logic, &mut self.state);
        self.root.layout(self.window);
        let mut sink = NullScene;
        let outcome = self.root.paint(
            &mut sink,
            FrameTime::from_nanos((self.clock_ms * 1_000_000.0) as u64),
        );
        self.clock_ms += 16.0;
        outcome
    }

    /// Frames until paint stops asking for one, returning how many it took.
    fn settle(&mut self, limit: usize) -> usize {
        for n in 1..=limit {
            if !self.frame().needs_frame {
                return n;
            }
        }
        panic!("the list did not settle within {limit} frames");
    }

    fn event(&mut self, event: InputEvent) {
        self.root.event(&mut self.state, &event);
    }

    /// The `Housekeeping` flush a frame-gated shell runs when a pass owed one.
    fn housekeeping(&mut self) {
        self.event(InputEvent::Housekeeping);
    }

    fn wheel(&mut self, px: f64) {
        self.event(InputEvent::Scroll {
            position: Point::new(10.0, 100.0),
            delta: ScrollDelta::Pixels(0.0, px),
        });
    }

    fn pointer(&mut self, phase: PointerPhase, y: f64) {
        self.event(InputEvent::Pointer(PointerEvent {
            phase,
            position: Point::new(10.0, y),
            button: PointerButton::Primary,
        }));
    }

    /// A slow drag moving the finger by `dy` (positive = finger down = away
    /// from the end), held still before release so no fling starts.
    fn drag(&mut self, dy: f64) {
        let start = 200.0;
        let slop = if dy >= 0.0 {
            TOUCH_SLOP + 1.0
        } else {
            -(TOUCH_SLOP + 1.0)
        };
        self.pointer(PointerPhase::Down, start);
        self.frame();
        self.pointer(PointerPhase::Move, start + slop);
        self.frame();
        self.pointer(PointerPhase::Move, start + slop + dy);
        self.frame();
        for _ in 0..12 {
            self.pointer(PointerPhase::Move, start + slop + dy);
            self.frame();
        }
        self.pointer(PointerPhase::Up, start + slop + dy);
    }

    /// A quick flick moving the finger by `dy` and releasing into a fling.
    fn flick(&mut self, dy: f64) {
        let start = 200.0;
        let slop = if dy >= 0.0 {
            TOUCH_SLOP + 1.0
        } else {
            -(TOUCH_SLOP + 1.0)
        };
        self.pointer(PointerPhase::Down, start);
        self.frame();
        self.pointer(PointerPhase::Move, start + slop);
        self.frame();
        self.pointer(PointerPhase::Move, start + slop + dy);
        self.pointer(PointerPhase::Up, start + slop + dy);
        assert!(self.list().is_flinging(), "the flick started a fling");
    }

    fn list(&self) -> &ListViewWidget {
        let id = self.root.root_id().expect("root built");
        (self.root.tree().pod(id).expect("root pod").widget() as &dyn Any)
            .downcast_ref::<ListViewWidget>()
            .expect("root is a ListViewWidget")
    }

    /// The last row of the data, as painted this frame: `(top, height)`.
    fn last_painted(&self) -> (f64, f64) {
        let last = self.rows.borrow().last().expect("rows").0;
        *self
            .painted
            .borrow()
            .get(&last)
            .expect("the last row was painted this frame")
    }

    /// Assert the end is on screen as painted: the last row's bottom on the
    /// viewport's bottom edge.
    fn assert_end_in_view(&self, what: &str) {
        let (top, height) = self.last_painted();
        let bottom = top + height;
        assert!(
            (bottom - self.window.height).abs() < 1e-6,
            "{what}: the last row's painted bottom is {bottom}, not the viewport bottom {}",
            self.window.height
        );
    }

    /// The topmost row on screen as painted: `(id, top)`.
    fn top_row(&self) -> (u64, f64) {
        let painted = self.painted.borrow();
        painted
            .iter()
            .filter(|(_, (top, height))| top + height > 0.0)
            .map(|(&id, &(top, _))| (id, top))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .expect("a row is on screen")
    }

    fn grow_last(&mut self, px: f64) {
        let mut rows = self.rows.borrow_mut();
        let last = rows.last_mut().expect("rows");
        last.1 += px * WIDTH;
    }

    fn distance_to_end(&self) -> f64 {
        self.list().max_offset() - self.list().offset()
    }
}

#[test]
fn five_thousand_rows_open_at_the_true_end_and_build_only_the_window() {
    let mut h = Harness::new(0..5_000);
    let mut frames = 0;
    loop {
        frames += 1;
        let outcome = h.frame();
        h.assert_end_in_view(&format!("opening frame {frames}"));
        if !outcome.needs_frame {
            break;
        }
        assert!(frames < 6, "opening at the end did not settle");
    }
    assert!(
        frames <= 3,
        "opening converges within three frames (took {frames})"
    );
    let (_, top) = h.top_row();
    assert!(top <= 0.0, "the settled window covers the viewport's top");
    assert!(h.list().is_stuck_to_end(), "a fresh list is stuck");
    assert!(
        h.state.changes.is_empty(),
        "the initial stuck state is not a transition"
    );

    // The builder runs for every materialized slot on every rebuild (a
    // survivor is rebuilt against its previous view), so count the distinct
    // rows it was ever asked for.
    let built: std::collections::BTreeSet<usize> = h.built.borrow().iter().copied().collect();
    assert!(
        built.len() <= 16,
        "only the window was built ({} distinct rows: {built:?})",
        built.len()
    );
    let lowest = built.iter().copied().min().expect("something was built");
    assert!(
        lowest >= 5_000 - 16,
        "no row far from the end was built (lowest index {lowest})"
    );
}

#[test]
fn last_row_growth_keeps_the_end_with_no_frame_short_of_it() {
    let mut h = Harness::new(0..60);
    h.settle(6);
    h.assert_end_in_view("settled");
    // A streaming reply: the last row grows a few pixels every frame.
    for step in 0..40 {
        h.grow_last(10.0);
        h.frame();
        h.assert_end_in_view(&format!("growth step {step}"));
    }
    assert!(
        h.last_painted().1 > WINDOW.height,
        "the last row outgrew the viewport and its bottom still sits on the edge"
    );
    assert!(h.list().is_stuck_to_end());
    assert!(h.state.changes.is_empty(), "growth never releases the list");
}

#[test]
fn appended_rows_keep_the_end() {
    let mut h = Harness::new(0..60);
    h.settle(6);
    for id in 1_000..1_005 {
        h.rows.borrow_mut().push((id, height_of(id) * WIDTH));
        h.frame();
        h.assert_end_in_view(&format!("append {id}"));
        assert_eq!(h.last_painted().1, height_of(id), "the new row is the last");
    }
    // A burst larger than the window, appended in one frame.
    let burst: Vec<(u64, f64)> = (2_000..2_030)
        .map(|id| (id, height_of(id) * WIDTH))
        .collect();
    h.rows.borrow_mut().extend(burst);
    h.frame();
    h.assert_end_in_view("thirty rows appended at once");
    h.settle(6);
    h.assert_end_in_view("after the burst settles");
    assert!(h.list().is_stuck_to_end());
}

#[test]
fn a_viewport_height_or_width_change_keeps_the_end() {
    let mut h = Harness::new(0..60);
    h.settle(6);

    // A keyboard rising: the viewport shrinks.
    h.window = Size::new(WIDTH, 250.0);
    h.frame();
    h.assert_end_in_view("viewport shrunk to 250");
    h.settle(6);

    // ...and falling again.
    h.window = WINDOW;
    h.frame();
    h.assert_end_in_view("viewport grown back to 400");
    h.settle(6);

    // A rotation narrowing the list: every row re-wraps taller.
    h.window = Size::new(200.0, 400.0);
    h.frame();
    h.assert_end_in_view("narrower width, rows re-wrapped");
    assert!(
        h.last_painted().1 > height_of(59),
        "the last row really re-wrapped taller"
    );
    h.settle(6);
    h.assert_end_in_view("after the re-wrap settles");
    assert!(h.list().is_stuck_to_end());
}

#[test]
fn a_user_drag_up_releases_and_later_growth_does_not_move_the_offset() {
    let mut h = Harness::new(0..60);
    h.settle(6);
    h.drag(150.0);
    assert!(
        !h.list().is_stuck_to_end(),
        "a drag past the threshold releases"
    );
    assert_eq!(
        h.state.changes,
        vec![false],
        "the release is delivered with the drag"
    );
    h.settle(6);
    let offset = h.list().offset();
    let top = h.top_row();
    for _ in 0..5 {
        h.grow_last(20.0);
        h.frame();
        assert_eq!(h.list().offset(), offset, "growth below no longer moves it");
        assert_eq!(h.top_row(), top, "and the rows on screen stay put");
    }
    assert!(h.distance_to_end() > THRESHOLD);
    assert_eq!(h.state.changes, vec![false]);
}

#[test]
fn a_small_wheel_step_inside_the_threshold_is_left_alone_until_the_end_moves() {
    let mut h = Harness::new(0..60);
    h.settle(6);
    h.wheel(-10.0);
    h.settle(6);
    assert!(h.list().is_stuck_to_end(), "10 px is inside the threshold");
    assert!(
        (h.distance_to_end() - 10.0).abs() < 1e-9,
        "a sub-threshold step is not snapped back while nothing moves"
    );
    // A trackpad's run of small steps adds up and escapes.
    for _ in 0..4 {
        h.wheel(-5.0);
        h.frame();
    }
    assert!(!h.list().is_stuck_to_end(), "30 px out releases the list");
    assert_eq!(h.state.changes, vec![false]);
}

#[test]
fn scrolling_back_to_the_end_re_sticks_and_the_callback_fires_once_each_way() {
    let mut h = Harness::new(0..60);
    h.settle(6);
    h.wheel(-200.0);
    assert_eq!(h.state.changes, vec![false], "released once");
    h.wheel(-50.0);
    h.settle(6);
    assert_eq!(h.state.changes, vec![false], "and not again while away");

    // Back within the threshold, not all the way: it re-sticks and lands on
    // the true end.
    let back = h.distance_to_end() - (THRESHOLD - 4.0);
    h.wheel(back);
    assert_eq!(h.state.changes, vec![false, true], "re-stuck once");
    assert!(h.list().is_stuck_to_end());
    h.frame();
    h.assert_end_in_view("a re-stick lands on the end");
    h.settle(6);

    // More growth, more scrolling at the end: no further transitions.
    h.grow_last(30.0);
    h.frame();
    h.assert_end_in_view("growth after re-sticking");
    h.wheel(40.0);
    h.settle(6);
    assert_eq!(h.state.changes, vec![false, true], "exactly once each way");
}

#[test]
fn a_programmatic_jump_away_never_releases_the_list() {
    let mut h = Harness::new(0..60);
    h.settle(6);
    h.controller.jump_to(0.0);
    h.frame();
    assert_eq!(h.list().offset(), 0.0, "the jump lands");
    assert!(
        h.list().is_stuck_to_end(),
        "programmatic writes never release"
    );
    // The rows it landed among measure, the end moves, and a stuck list
    // follows its end: it is pinned back.
    h.settle(6);
    h.assert_end_in_view("the stuck list followed its end back");
    h.grow_last(10.0);
    h.frame();
    h.assert_end_in_view("and keeps following it");
    h.housekeeping();
    assert!(h.list().is_stuck_to_end());
    assert!(h.state.changes.is_empty(), "no transition either way");
}

#[test]
fn jump_to_end_during_a_fling_sticks_and_lands_on_the_end() {
    let mut h = Harness::new(0..200);
    h.settle(6);
    h.drag(300.0);
    h.settle(6);
    assert_eq!(h.state.changes, vec![false]);

    // Fling further up, away from the end, and jump mid-flight.
    h.flick(120.0);
    h.frame();
    h.frame();
    assert!(h.list().is_flinging(), "still flinging");
    h.controller.jump_to_end();
    h.frame();
    assert!(!h.list().is_flinging(), "the jump ended the fling");
    assert!(h.list().is_stuck_to_end(), "jump_to_end sticks the list");
    h.assert_end_in_view("the jump's frame");
    h.housekeeping();
    assert_eq!(
        h.state.changes,
        vec![false, true],
        "the re-stick reaches the callback on the next event"
    );
    h.grow_last(40.0);
    h.frame();
    h.assert_end_in_view("growth after the jump");
}

#[test]
fn growth_during_a_fling_toward_the_end_lands_stuck_on_the_new_end() {
    let mut h = Harness::new(0..200);
    h.settle(6);
    h.drag(250.0);
    h.settle(6);
    assert!(!h.list().is_stuck_to_end());

    // Fling down toward the end while the last row keeps growing.
    h.flick(-160.0);
    for _ in 0..200 {
        h.grow_last(3.0);
        h.frame();
        if !h.list().is_flinging() {
            break;
        }
    }
    assert!(!h.list().is_flinging(), "the fling came to rest");
    // The overshoot past the end eases back on the ordinary release settle.
    h.settle(40);
    assert!(
        h.list().is_stuck_to_end(),
        "the fling brought the list back"
    );
    h.assert_end_in_view("after the fling, on the grown end");
    h.housekeeping();
    assert_eq!(h.state.changes, vec![false, true]);
}

#[test]
fn growth_during_a_fling_away_from_the_end_never_pulls_the_list_back() {
    let mut h = Harness::new(0..200);
    h.settle(6);
    h.flick(160.0);
    let mut trace = vec![h.list().offset()];
    for _ in 0..200 {
        h.grow_last(5.0);
        h.frame();
        trace.push(h.list().offset());
        if !h.list().is_flinging() {
            break;
        }
    }
    for pair in trace.windows(2) {
        assert!(
            pair[1] <= pair[0],
            "the offset stepped toward the end during a fling away from it ({} -> {})",
            pair[0],
            pair[1]
        );
    }
    h.housekeeping();
    assert!(!h.list().is_stuck_to_end(), "the fling released the list");
    assert_eq!(h.state.changes, vec![false]);
    // The correction the fling withheld commits once it stops; let it.
    h.settle(6);
    let (offset, top) = (h.list().offset(), h.top_row());
    h.grow_last(50.0);
    h.settle(6);
    assert_eq!(h.list().offset(), offset, "nothing pins it after the fling");
    assert_eq!(h.top_row(), top);
}

#[test]
fn the_keyed_prepend_anchor_holds_when_released_and_when_stuck() {
    // Released: a history load above the screen keeps the rows on screen.
    let mut h = Harness::new(1_000..1_200);
    h.settle(6);
    h.drag(600.0);
    h.settle(6);
    assert!(!h.list().is_stuck_to_end());
    let before = h.top_row();
    h.rows
        .borrow_mut()
        .splice(0..0, (0..20).map(|id| (id, height_of(id) * WIDTH)));
    h.frame();
    assert_eq!(h.top_row(), before, "the prepend did not move the content");
    h.settle(6);
    assert_eq!(h.top_row(), before, "nor did settling it");

    // Stuck: the same prepend leaves the end, and every row on it, in place.
    let mut h = Harness::new(1_000..1_200);
    h.settle(6);
    let before = h.top_row();
    h.rows
        .borrow_mut()
        .splice(0..0, (0..20).map(|id| (id, height_of(id) * WIDTH)));
    h.frame();
    h.assert_end_in_view("prepend while stuck");
    assert_eq!(h.top_row(), before, "the rows on screen stay put");
    h.settle(6);
    assert_eq!(h.top_row(), before);
    assert!(h.list().is_stuck_to_end());
}

#[test]
fn stick_to_end_off_leaves_the_list_at_the_top() {
    let mut h = Harness::new(0..60);
    h.stick.set(false);
    h.settle(6);
    assert_eq!(h.list().offset(), 0.0, "an ordinary list opens at the top");
    assert!(!h.list().is_stuck_to_end());
    h.grow_last(100.0);
    h.settle(6);
    assert_eq!(h.list().offset(), 0.0, "and growth below never moves it");
    assert!(h.state.changes.is_empty());

    // jump_to_end still lands on the measured end, then lets go.
    h.controller.jump_to_end();
    h.settle(8);
    h.frame();
    h.assert_end_in_view("jump_to_end on a list that does not stick");
    let offset = h.list().offset();
    h.grow_last(40.0);
    h.settle(6);
    assert_eq!(
        h.list().offset(),
        offset,
        "nothing follows the end afterwards"
    );
    assert!(h.state.changes.is_empty());
}
