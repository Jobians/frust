//! The process-wide repaint request (`frust_core::repaint`) against a real `RenderRoot`: a
//! request from any thread makes the root owe a paint, the paint repaints content behind a
//! retained boundary and republishes semantics with no rebuild, and the wake hook coalesces.
//!
//! The boundary here is a stand-in for `frust-widgets`' retained boundary, which this crate
//! cannot reach: it gates its body on `PaintCtx::subtree_clean` the same way and skips the body
//! when clean, where the real one would replay its recorded page.
//!
//! Every test touches the same process-wide statics, so each holds `STATICS` for its whole run.

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use accesskit::Role;
use frust_core::semantics::SemanticsCtx;
use frust_core::{
    AnyView, BoxConstraints, BuildCtx, ChangeFlags, ChildPod, DiscardScene, FrameTime, LayoutCtx,
    PaintCtx, PaintScene, RenderRoot, View, Widget, any, repaint_generation, request_repaint,
    set_repaint_hook, take_repaint_owed,
};
use kurbo::{Point, Size};

static STATICS: Mutex<()> = Mutex::new(());

/// Serialise on the statics and start from a drained, hook-free state.
fn hold() -> MutexGuard<'static, ()> {
    let guard = STATICS.lock().unwrap_or_else(PoisonError::into_inner);
    set_repaint_hook(None);
    take_repaint_owed();
    guard
}

thread_local! {
    /// How many times the leaf painted.
    static LEAF_PAINTS: Cell<u32> = const { Cell::new(0) };
    /// What `subtree_clean` answered for the leaf's pod at each boundary paint.
    static CLEAN: RefCell<Vec<bool>> = const { RefCell::new(Vec::new()) };
    /// The label the leaf reads from its own (non-view) state.
    static LABEL: RefCell<&'static str> = const { RefCell::new("first") };
}

struct Leaf;

impl Widget for Leaf {
    fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        bc.constrain(Size::new(40.0, 20.0))
    }
    fn paint(&mut self, _ctx: &mut PaintCtx, _scene: &mut dyn PaintScene) {
        LEAF_PAINTS.with(|n| n.set(n.get() + 1));
    }
    fn semantics(&self, ctx: &mut SemanticsCtx) {
        let label = LABEL.with(|l| *l.borrow());
        ctx.push_node(Role::Label, |node| node.set_label(label));
    }
}

struct LeafView;

impl<S: 'static> View<S> for LeafView {
    type Element = Leaf;
    fn build(&self, _ctx: &mut BuildCtx<'_>) -> Leaf {
        Leaf
    }
    fn rebuild(&self, _: &Self, _: &mut Leaf, _ctx: &mut BuildCtx<'_>) -> ChangeFlags {
        ChangeFlags::NONE
    }
}

/// Paints its body only when `subtree_clean` says the body changed.
struct Boundary {
    child: ChildPod,
}

impl Widget for Boundary {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let size = self
            .child
            .layout_child(ctx, &BoxConstraints::loose(bc.max()));
        self.child.set_origin(Point::ZERO);
        bc.constrain(size)
    }
    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let clean = ctx.subtree_clean(&self.child);
        CLEAN.with(|log| log.borrow_mut().push(clean));
        if !clean {
            self.child.paint_child(ctx, scene);
        }
    }
    fn semantics(&self, ctx: &mut SemanticsCtx) {
        self.child.semantics_child(ctx);
    }
    fn visit_children(&self, visitor: &mut dyn FnMut(&ChildPod)) {
        visitor(&self.child);
    }
}

struct BoundaryView;

impl<S: 'static> View<S> for BoundaryView {
    type Element = Boundary;
    fn build(&self, ctx: &mut BuildCtx<'_>) -> Boundary {
        let child: AnyView<S> = any(LeafView);
        Boundary {
            child: ChildPod::new(Box::new(child.build(ctx))),
        }
    }
    fn rebuild(&self, _: &Self, _: &mut Boundary, _ctx: &mut BuildCtx<'_>) -> ChangeFlags {
        ChangeFlags::NONE
    }
}

/// A built, laid-out root with fresh per-thread logs.
fn root() -> RenderRoot<(), BoundaryView> {
    LEAF_PAINTS.with(|n| n.set(0));
    CLEAN.with(|log| log.borrow_mut().clear());
    LABEL.with(|l| *l.borrow_mut() = "first");
    let mut root = RenderRoot::new();
    root.rebuild(&mut |_: &mut ()| BoundaryView, &mut ());
    root.layout(Size::new(200.0, 100.0));
    root
}

fn paint(root: &mut RenderRoot<(), BoundaryView>) {
    root.paint(&mut DiscardScene, FrameTime::ZERO);
}

fn leaf_paints() -> u32 {
    LEAF_PAINTS.with(Cell::get)
}

#[test]
fn a_request_from_another_thread_is_pending_until_the_next_paint() {
    let _statics = hold();
    let mut root = root();
    paint(&mut root);
    assert!(!root.repaint_pending());

    let before = repaint_generation();
    std::thread::spawn(request_repaint)
        .join()
        .expect("the requesting thread ran");
    assert_ne!(
        repaint_generation(),
        before,
        "the request advanced the generation"
    );
    assert!(root.repaint_pending());
    assert!(root.repaint_pending(), "reading pending does not clear it");

    paint(&mut root);
    assert!(!root.repaint_pending(), "the paint honoured the request");
    take_repaint_owed();
}

#[test]
fn a_requested_paint_repaints_behind_a_boundary_with_no_rebuild() {
    let _statics = hold();
    let mut root = root();
    paint(&mut root);
    assert_eq!(leaf_paints(), 1, "the first paint paints the body");
    paint(&mut root);
    assert_eq!(leaf_paints(), 1, "an unchanged body is skipped");

    request_repaint();
    paint(&mut root);
    assert_eq!(
        leaf_paints(),
        2,
        "the request reached the body with no rebuild"
    );
    paint(&mut root);
    assert_eq!(leaf_paints(), 2, "and only for one paint");
    assert_eq!(
        CLEAN.with(|log| log.borrow().clone()),
        vec![false, true, false, true],
        "subtree_clean flipped to not-clean for exactly the requested paint"
    );
    take_repaint_owed();
}

#[test]
fn the_hook_runs_once_per_debt_however_many_requests_land() {
    let _statics = hold();
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let replaced = set_repaint_hook(Some(Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    })));
    assert!(replaced.is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 0, "nothing owed, no wake");

    let threads: Vec<_> = (0..4)
        .map(|_| std::thread::spawn(request_repaint))
        .collect();
    for thread in threads {
        thread.join().expect("a requesting thread ran");
    }
    request_repaint();
    assert_eq!(calls.load(Ordering::SeqCst), 1, "five requests, one wake");

    assert!(take_repaint_owed());
    assert!(!take_repaint_owed(), "one take per debt");
    request_repaint();
    request_repaint();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "the take re-armed the edge"
    );

    assert!(
        set_repaint_hook(None).is_some(),
        "the installed hook is handed back"
    );
    take_repaint_owed();
}

#[test]
fn installing_a_hook_over_an_owed_debt_wakes_at_once() {
    let _statics = hold();
    request_repaint();
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    set_repaint_hook(Some(Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    })));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    set_repaint_hook(None);
    take_repaint_owed();
}

#[test]
fn a_hook_that_requests_again_does_not_deadlock() {
    let _statics = hold();
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    set_repaint_hook(Some(Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
        request_repaint();
        set_repaint_hook(None);
    })));
    request_repaint();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    take_repaint_owed();
}

#[test]
fn a_requested_paint_republishes_semantics_with_no_rebuild() {
    let _statics = hold();
    let mut root = root();
    paint(&mut root);
    let seen = root.semantics_generation();
    assert!(root.semantics_if_changed(seen).is_none());

    LABEL.with(|l| *l.borrow_mut() = "second");
    paint(&mut root);
    assert!(
        root.semantics_if_changed(seen).is_none(),
        "a plain paint does not move the gate"
    );

    request_repaint();
    paint(&mut root);
    let update = root
        .semantics_if_changed(seen)
        .expect("the requested paint moved the semantics gate");
    let labels: Vec<_> = update
        .nodes
        .iter()
        .filter_map(|(_, node)| node.label().map(str::to_owned))
        .collect();
    assert!(labels.iter().any(|label| label == "second"), "{labels:?}");
    take_repaint_owed();
}

#[test]
fn a_root_built_after_earlier_requests_owes_nothing() {
    let _statics = hold();
    request_repaint();
    request_repaint();
    let mut root = root();
    assert!(!root.repaint_pending());
    paint(&mut root);
    let seen = root.semantics_generation();
    paint(&mut root);
    assert!(root.semantics_if_changed(seen).is_none());
    take_repaint_owed();
}
