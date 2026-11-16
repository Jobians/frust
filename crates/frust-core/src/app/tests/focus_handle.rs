//! Programmatic focus at the root (`crate::focus_handle`): handle requests and releases, the
//! `is_focused` mirror, presses that keep the live session, and the deferred focus-change
//! notification. The harness drives the root the way an on-demand shell does: a frame (rebuild,
//! layout, paint) runs only when an input outcome, a paint, a pending change flag or the deferred
//! flush asked for one, so a missing frame request leaves a test failing instead of hidden.

use std::collections::{HashMap, HashSet};

use super::*;
use crate::focus_handle::FocusHandle;
use crate::overlay::{OverlayBand, OverlayKeys, OverlayPod};
use crate::widget::{ChildPod, PaintOutcome, Widget};

#[derive(Default)]
struct Log {
    handles: HashMap<&'static str, FocusHandle>,
    /// The fields that painted focused on the latest paint, in paint order.
    focused: Vec<&'static str>,
    open: HashSet<&'static str>,
    /// Holders whose inner field the next rebuild unmounts.
    gone: HashSet<&'static str>,
    /// Holders (by label) the next rebuild unmounts whole.
    removed: HashSet<&'static str>,
    /// Pressed widgets that call `keep_focus`.
    keep: HashSet<&'static str>,
    /// Pressed widgets that call `request_focus`.
    claim: HashSet<&'static str>,
    /// Button name to the handle it requests on a press.
    asks: HashMap<&'static str, &'static str>,
    /// `(widget, has_focus)` for each deferred focus-change delivery.
    notices: Vec<(&'static str, bool)>,
}

type Shared = Rc<RefCell<Log>>;

const MODAL_RECT: Rect = Rect::new(100.0, 100.0, 200.0, 200.0);

fn caret_of(name: &str) -> Rect {
    let x = f64::from(name.as_bytes()[0]);
    Rect::new(x, 0.0, x + 1.0, 10.0)
}

/// A focusable field: claims on a press, releases on Escape, publishes a caret unique to its
/// name while focused, binds its handle every paint.
struct Field {
    name: &'static str,
    log: Shared,
}

impl Widget for Field {
    fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        bc.constrain(Size::new(40.0, 20.0))
    }
    fn paint(&mut self, ctx: &mut PaintCtx, _scene: &mut dyn PaintScene) {
        if let Some(handle) = self.log.borrow().handles.get(self.name) {
            ctx.bind_focus_handle(handle);
        }
        if ctx.has_focus() {
            self.log.borrow_mut().focused.push(self.name);
            ctx.publish_ime_state(ImeState {
                active: true,
                caret: Some(caret_of(self.name)),
                ..ImeState::default()
            });
        }
    }
    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        match event {
            InputEvent::Pointer(p) if p.phase == PointerPhase::Down => {
                ctx.request_focus();
                EventResult::Handled
            }
            InputEvent::Key(key) if key.key == Key::Named(NamedKey::Escape) => {
                ctx.release_focus();
                EventResult::Handled
            }
            InputEvent::Housekeeping if ctx.focus_changed() => {
                let entry = (self.name, ctx.has_focus());
                self.log.borrow_mut().notices.push(entry);
                EventResult::Ignored
            }
            _ => EventResult::Ignored,
        }
    }
    fn is_focusable(&self) -> bool {
        true
    }
}

/// A focusable wrapper around an optional inner field; it records focus-change deliveries.
struct Holder {
    name: &'static str,
    inner: Option<ChildPod>,
    log: Shared,
}

impl Widget for Holder {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        if let Some(inner) = &mut self.inner {
            inner.layout_child(ctx, &BoxConstraints::loose(Size::new(40.0, 20.0)));
        }
        bc.constrain(Size::new(40.0, 20.0))
    }
    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        if let Some(handle) = self.log.borrow().handles.get(self.name) {
            ctx.bind_focus_handle(handle);
        }
        if let Some(inner) = &mut self.inner {
            inner.paint_child(ctx, scene);
        }
    }
    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        if matches!(event, InputEvent::Housekeeping) && ctx.focus_changed() {
            let entry = (self.name, ctx.has_focus());
            self.log.borrow_mut().notices.push(entry);
        }
        match &mut self.inner {
            Some(inner) => inner.event_child(ctx, event),
            None => EventResult::Ignored,
        }
    }
    fn is_focusable(&self) -> bool {
        true
    }
    fn visit_children(&self, visitor: &mut dyn FnMut(&ChildPod)) {
        self.inner.iter().for_each(visitor);
    }
}

/// A control that is no traversal stop: a press may keep focus, claim it, or request a handle.
struct Button {
    name: &'static str,
    log: Shared,
}

impl Widget for Button {
    fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        bc.constrain(Size::new(40.0, 20.0))
    }
    fn paint(&mut self, _ctx: &mut PaintCtx, _scene: &mut dyn PaintScene) {}
    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        match event {
            InputEvent::Pointer(p) if p.phase == PointerPhase::Down => {
                let log = self.log.borrow();
                if log.keep.contains(self.name) {
                    ctx.keep_focus();
                }
                if log.claim.contains(self.name) {
                    ctx.request_focus();
                }
                if let Some(handle) = log.asks.get(self.name).and_then(|n| log.handles.get(n)) {
                    handle.request();
                }
                EventResult::Handled
            }
            _ => EventResult::Ignored,
        }
    }
}

/// Keeps focus for the press, then forwards it (or forwards it first) to a child that claims.
struct Keeper {
    child: ChildPod,
    forward_first: bool,
}

impl Widget for Keeper {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        self.child
            .layout_child(ctx, &BoxConstraints::loose(Size::new(40.0, 20.0)));
        bc.constrain(Size::new(40.0, 20.0))
    }
    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        self.child.paint_child(ctx, scene);
    }
    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        if !matches!(event, InputEvent::Pointer(p) if p.phase == PointerPhase::Down) {
            return self.child.event_child(ctx, event);
        }
        if self.forward_first {
            let result = self.child.event_child(ctx, event);
            ctx.keep_focus();
            return result;
        }
        ctx.keep_focus();
        self.child.event_child(ctx, event)
    }
    fn visit_children(&self, visitor: &mut dyn FnMut(&ChildPod)) {
        visitor(&self.child);
    }
}

struct Column {
    children: Vec<ChildPod>,
}

impl Widget for Column {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let mut y = 0.0;
        for pod in &mut self.children {
            let size = pod.layout_child(ctx, &BoxConstraints::loose(Size::new(200.0, 200.0)));
            pod.set_origin(Point::new(0.0, y));
            y += size.height;
        }
        bc.constrain(Size::new(200.0, y))
    }
    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        for pod in &mut self.children {
            pod.paint_child(ctx, scene);
        }
    }
    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        if event.is_broadcast() {
            for pod in &mut self.children {
                pod.event_child(ctx, event);
            }
            return EventResult::Ignored;
        }
        let focus_routed = event.is_focus_routed();
        let reached = self.children.iter_mut().find(|pod| {
            if focus_routed {
                pod.holds_live_focus()
            } else {
                pod.contains(event.position())
            }
        });
        reached.map_or(EventResult::Ignored, |pod| pod.event_child(ctx, event))
    }
    fn visit_children(&self, visitor: &mut dyn FnMut(&ChildPod)) {
        self.children.iter().for_each(visitor);
    }
}

fn field(name: &'static str, log: &Shared) -> ChildPod {
    let mut pod = ChildPod::new(Box::new(Field {
        name,
        log: Rc::clone(log),
    }));
    pod.set_debug_label(name);
    pod
}

fn holder(name: &'static str, inner: &'static str, log: &Shared) -> ChildPod {
    let mut pod = ChildPod::new(Box::new(Holder {
        name,
        inner: Some(field(inner, log)),
        log: Rc::clone(log),
    }));
    pod.set_debug_label(name);
    pod
}

fn button(name: &'static str, log: &Shared) -> ChildPod {
    ChildPod::new(Box::new(Button {
        name,
        log: Rc::clone(log),
    }))
}

fn keeper(inner: &'static str, forward_first: bool, log: &Shared) -> ChildPod {
    ChildPod::new(Box::new(Keeper {
        child: button(inner, log),
        forward_first,
    }))
}

/// Floats a column of fields at [`MODAL_RECT`] while its name is open, as a modal.
struct ModalHost {
    name: &'static str,
    key: OverlayKey,
    pod: OverlayPod,
    log: Shared,
}

impl Widget for ModalHost {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let window = BoxConstraints::loose(ctx.window_size());
        self.pod.borrow_mut().layout_child(ctx, &window);
        bc.constrain(Size::new(40.0, 20.0))
    }
    fn paint(&mut self, ctx: &mut PaintCtx, _scene: &mut dyn PaintScene) {
        if !self.log.borrow().open.contains(self.name) {
            return;
        }
        ctx.register_overlay(OverlayEntry {
            key: self.key,
            band: OverlayBand::Floating,
            input: OverlayInput::Interactive,
            outside_tap: OutsideTap::Ignore,
            keys: OverlayKeys::Modal,
            window_rect: MODAL_RECT,
            pod: Rc::clone(&self.pod),
            insets: ctx.window_insets(),
        });
    }
    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        let mut pod = self.pod.borrow_mut();
        match event {
            InputEvent::Overlay(overlay) if overlay.key == self.key => {
                let local = match &overlay.kind {
                    OverlayEventKind::Shortcut(id) => InputEvent::Shortcut(*id),
                    OverlayEventKind::Pointer(p) => InputEvent::Pointer(PointerEvent {
                        position: p.position - MODAL_RECT.origin().to_vec2(),
                        ..*p
                    }),
                    _ => return EventResult::Ignored,
                };
                pod.event_child(ctx, &local);
                EventResult::Ignored
            }
            event if event.is_focus_routed() && pod.holds_live_focus() => {
                pod.event_child(ctx, event)
            }
            _ => EventResult::Ignored,
        }
    }
}

fn modal(name: &'static str, fields: &[&'static str], log: &Shared) -> ChildPod {
    let column = Column {
        children: fields.iter().map(|name| field(name, log)).collect(),
    };
    ChildPod::new(Box::new(ModalHost {
        name,
        key: OverlayKey::next(),
        pod: Rc::new(RefCell::new(ChildPod::new(Box::new(column)))),
        log: Rc::clone(log),
    }))
}

struct TreeView {
    make: fn(&Shared) -> Column,
    log: Shared,
}

impl View<()> for TreeView {
    type Element = Column;
    fn build(&self, _ctx: &mut BuildCtx<'_>) -> Column {
        (self.make)(&self.log)
    }
    fn rebuild(&self, _prev: &Self, element: &mut Column, _ctx: &mut BuildCtx<'_>) -> ChangeFlags {
        let log = self.log.borrow();
        let mut flags = ChangeFlags::NONE;
        let before = element.children.len();
        element.children.retain(|pod| {
            !pod.debug_label()
                .is_some_and(|name| log.removed.contains(name))
        });
        if element.children.len() != before {
            flags = ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        for pod in &mut element.children {
            if !pod
                .debug_label()
                .is_some_and(|name| log.gone.contains(name))
            {
                continue;
            }
            if let Some(holder) = pod.widget_mut().downcast_mut::<Holder>()
                && holder.inner.take().is_some()
            {
                flags = ChangeFlags::LAYOUT | ChangeFlags::PAINT;
            }
        }
        flags
    }
}

/// The on-demand driver: a frame runs only when something asked for one.
struct Harness {
    root: RenderRoot<(), TreeView>,
    make: fn(&Shared) -> Column,
    log: Shared,
    asked: bool,
    paints: usize,
}

impl Harness {
    fn with_log(log: Shared, make: fn(&Shared) -> Column) -> Self {
        let mut harness = Self {
            root: RenderRoot::new(),
            make,
            log,
            asked: true,
            paints: 0,
        };
        harness.pump();
        harness
    }

    fn rebuild(&mut self) {
        let (make, log) = (self.make, Rc::clone(&self.log));
        let mut logic = move |_: &mut ()| TreeView {
            make,
            log: Rc::clone(&log),
        };
        self.root.rebuild(&mut logic, &mut ());
    }

    fn paint(&mut self) -> PaintOutcome {
        self.log.borrow_mut().focused.clear();
        self.paints += 1;
        // A shell drains the flags it paints for.
        self.root.take_change_flags();
        self.root.layout(Size::new(200.0, 200.0));
        self.root
            .paint(&mut RecordingScene::default(), FrameTime::ZERO)
    }

    /// One frame, whether or not one was asked for: tests that stage a state between a paint and
    /// the next rebuild.
    fn frame(&mut self) {
        self.rebuild();
        self.asked = self.paint().needs_frame;
    }

    /// Run frames while the root asks for one, as a frame-gated shell would.
    fn pump(&mut self) {
        for _ in 0..8 {
            let asked = self.asked
                || crate::event::has_pending_result_flush()
                || self.root.has_pending_change_flags();
            if !asked {
                return;
            }
            self.asked = false;
            self.frame();
        }
        panic!("the root kept asking for frames");
    }

    /// Deliver `event`, then run the frames the outcome and the root ask for.
    fn send(&mut self, event: &InputEvent) -> EventOutcome {
        let outcome = self.root.event(&mut (), event);
        self.asked |= outcome.needs_redraw;
        self.pump();
        outcome
    }

    fn tap(&mut self, x: f64, y: f64) {
        self.send(&pointer(PointerPhase::Down, x, y));
        self.send(&pointer(PointerPhase::Up, x, y));
    }

    fn touch_tap(&mut self, x: f64, y: f64) {
        self.send(&touch(PointerPhase::Down, x, y));
        self.send(&touch(PointerPhase::Up, x, y));
    }

    fn handle(&self, name: &'static str) -> FocusHandle {
        self.log.borrow().handles[name].clone()
    }

    fn focused(&self) -> Option<&'static str> {
        self.log.borrow().focused.last().copied()
    }

    fn open(&mut self, name: &'static str) {
        self.log.borrow_mut().open.insert(name);
        self.asked = true;
        self.pump();
    }

    fn notices(&self) -> Vec<(&'static str, bool)> {
        std::mem::take(&mut self.log.borrow_mut().notices)
    }
}

fn touch(phase: PointerPhase, x: f64, y: f64) -> InputEvent {
    InputEvent::PointerContact {
        pointer_id: PointerId::touch(0),
        event: PointerEvent {
            phase,
            position: Point::new(x, y),
            button: PointerButton::Primary,
        },
    }
}

fn log_with_handles(names: &[&'static str]) -> Shared {
    let log = Shared::default();
    for name in names {
        log.borrow_mut().handles.insert(name, FocusHandle::new());
    }
    log
}

/// Fields `a` and `b`, then buttons.
fn two_fields(log: &Shared) -> Column {
    column(vec![
        field("a", log),
        field("b", log),
        button("plain", log),
        button("asker", log),
        button("keeper", log),
        keeper("claimer", false, log),
        keeper("late_claimer", true, log),
    ])
}

fn column(children: Vec<ChildPod>) -> Column {
    Column { children }
}

/// The y of the middle of the `index`th child (all 20 high).
fn row(index: usize) -> f64 {
    20.0 * index as f64 + 10.0
}

fn field_and_modal(log: &Shared) -> Column {
    column(vec![
        field("a", log),
        field("b", log),
        modal("m", &["m1"], log),
    ])
}

fn nested(log: &Shared) -> Column {
    column(vec![holder("outer", "inner", log), field("a", log)])
}

#[test]
fn a_request_before_mount_focuses_once_the_widget_binds() {
    let log = log_with_handles(&["a", "b"]);
    log.borrow().handles["a"].request();
    let h = Harness::with_log(log, two_fields);
    assert_eq!(h.focused(), Some("a"));
    assert!(h.handle("a").is_focused());
    assert!(!h.handle("b").is_focused());
    assert!(h.root.is_focus_active());
    assert!(!h.root.keyboard_focus, "a programmatic grant shows no ring");
    assert!(h.root.ime_state().is_some());
}

#[test]
fn a_request_from_another_controls_handler_moves_focus_at_the_end_of_that_pass() {
    let log = log_with_handles(&["a", "b"]);
    log.borrow_mut().asks.insert("asker", "b");
    log.borrow_mut().keep.insert("asker");
    let mut h = Harness::with_log(log, two_fields);
    h.tap(5.0, row(0));
    assert_eq!(h.focused(), Some("a"));

    let epoch = h.root.focus_epoch();
    let outcome = h
        .root
        .event(&mut (), &pointer(PointerPhase::Down, 5.0, row(3)));
    assert!(outcome.needs_redraw);
    assert!(
        h.handle("b").is_focused(),
        "applied before the pass returned"
    );
    assert!(!h.handle("a").is_focused());
    assert_ne!(h.root.focus_epoch(), epoch);
    h.asked = true;
    h.pump();
    assert_eq!(h.focused(), Some("b"));
}

#[test]
fn a_request_on_a_stop_outside_an_open_modal_is_dropped_and_never_applied_later() {
    let log = log_with_handles(&["a"]);
    let mut h = Harness::with_log(log, field_and_modal);
    h.open("m");
    h.handle("a").request();
    h.pump();
    assert!(
        !h.root.is_focus_active(),
        "the trap refuses a field behind it"
    );
    assert!(!h.handle("a").is_focused());

    h.log.borrow_mut().open.remove("m");
    h.asked = true;
    h.pump();
    assert!(
        !h.root.is_focus_active(),
        "the dropped request did not wait"
    );
    assert!(!h.handle("a").is_focused());
}

#[test]
fn a_request_while_another_stop_holds_a_session_moves_it_and_flips_both_handles() {
    let log = log_with_handles(&["a", "b"]);
    let mut h = Harness::with_log(log, two_fields);
    h.tap(5.0, row(0));
    assert!(h.handle("a").is_focused());
    let epoch = h.root.focus_epoch();

    h.handle("b").request();
    h.pump();
    assert_eq!(h.focused(), Some("b"));
    assert!(h.handle("b").is_focused());
    assert!(!h.handle("a").is_focused());
    assert_ne!(h.root.focus_epoch(), epoch);
    assert_eq!(
        h.root.ime_state().and_then(|ime| ime.caret),
        Some(caret_of("b"))
    );
    assert!(!h.root.keyboard_focus);
}

#[test]
fn a_release_ends_only_its_own_session() {
    let log = log_with_handles(&["a", "b"]);
    let mut h = Harness::with_log(log, two_fields);
    h.tap(5.0, row(0));

    h.handle("b").release();
    h.pump();
    assert!(h.root.is_focus_active(), "b held nothing");
    assert!(h.handle("a").is_focused());

    h.handle("a").release();
    h.pump();
    assert!(!h.root.is_focus_active());
    assert!(!h.handle("a").is_focused());
    assert!(h.root.ime_state().is_none());
}

#[test]
fn a_request_for_the_stop_already_holding_the_session_leaves_it_untouched() {
    let log = log_with_handles(&["a"]);
    let mut h = Harness::with_log(log, two_fields);
    h.tap(5.0, row(0));
    let (epoch, ime) = (h.root.focus_epoch(), h.root.ime_state());
    h.handle("a").request();
    h.pump();
    assert_eq!(h.root.focus_epoch(), epoch);
    assert_eq!(h.root.ime_state(), ime);
}

/// A press on `button_row` that keeps focus leaves the session as it was through Down and Up.
fn assert_press_keeps_session(h: &mut Harness, touch_input: bool, button_row: usize) {
    h.tap(5.0, row(0));
    assert!(h.root.is_focus_active());
    let (epoch, ime) = (h.root.focus_epoch(), h.root.ime_state());
    let (down, up) = if touch_input {
        (
            touch(PointerPhase::Down, 5.0, row(button_row)),
            touch(PointerPhase::Up, 5.0, row(button_row)),
        )
    } else {
        (
            pointer(PointerPhase::Down, 5.0, row(button_row)),
            pointer(PointerPhase::Up, 5.0, row(button_row)),
        )
    };
    h.send(&down);
    assert_eq!(
        h.root.focus_epoch(),
        epoch,
        "the holder is the same session"
    );
    assert_eq!(h.root.ime_state(), ime, "and its IME state untouched");
    h.send(&up);
    assert!(h.root.is_focus_active(), "no blur at Up");
    assert_eq!(h.root.focus_epoch(), epoch);
    assert_eq!(h.root.ime_state(), ime);
    assert_eq!(h.focused(), Some("a"));
}

#[test]
fn a_press_that_keeps_focus_leaves_the_session_for_a_mouse() {
    let log = log_with_handles(&[]);
    log.borrow_mut().keep.insert("keeper");
    let mut h = Harness::with_log(log, two_fields);
    assert_press_keeps_session(&mut h, false, 4);
}

#[test]
fn a_press_that_keeps_focus_leaves_the_session_for_a_touch() {
    let log = log_with_handles(&[]);
    log.borrow_mut().keep.insert("keeper");
    let mut h = Harness::with_log(log, two_fields);
    assert_press_keeps_session(&mut h, true, 4);
}

#[test]
fn a_kept_press_ignores_a_descendants_request_in_either_order() {
    for (touch_input, button_row) in [(false, 5), (true, 5), (false, 6), (true, 6)] {
        let log = log_with_handles(&[]);
        log.borrow_mut().claim.insert("claimer");
        log.borrow_mut().claim.insert("late_claimer");
        let mut h = Harness::with_log(log, two_fields);
        assert_press_keeps_session(&mut h, touch_input, button_row);
    }
}

#[test]
fn a_kept_press_does_not_stamp_a_later_claim() {
    let log = log_with_handles(&[]);
    log.borrow_mut().claim.insert("late_claimer");
    let mut h = Harness::with_log(log, two_fields);
    assert_press_keeps_session(&mut h, false, 6);
    h.tap(5.0, row(1));
    assert_eq!(
        h.focused(),
        Some("b"),
        "a later claim moves the session normally"
    );
    h.send(&key(NamedKey::Escape));
    assert!(!h.root.is_focus_active());
    assert_eq!(h.focused(), None);
}

#[test]
fn a_kept_press_with_no_session_still_claims() {
    let log = log_with_handles(&[]);
    log.borrow_mut().claim.insert("claimer");
    let mut h = Harness::with_log(log, two_fields);
    h.send(&pointer(PointerPhase::Down, 5.0, row(5)));
    assert!(h.root.is_focus_active(), "no live session: nothing to keep");
}

#[test]
fn a_press_that_does_not_keep_focus_still_blurs_at_up() {
    for touch_input in [false, true] {
        let log = log_with_handles(&[]);
        let mut h = Harness::with_log(log, two_fields);
        h.tap(5.0, row(0));
        assert!(h.root.is_focus_active());
        if touch_input {
            h.touch_tap(5.0, row(2));
        } else {
            h.tap(5.0, row(2));
        }
        assert!(
            !h.root.is_focus_active(),
            "the plain button blurred the field"
        );
    }
}

#[test]
fn an_explicit_request_wins_over_an_owed_focus_return() {
    let log = log_with_handles(&["b"]);
    let mut h = Harness::with_log(log, field_and_modal);
    h.tap(5.0, row(0));
    assert_eq!(h.focused(), Some("a"));
    h.open("m");
    assert!(!h.root.is_focus_active(), "the trap ended the session");

    h.log.borrow_mut().open.remove("m");
    h.frame();
    assert!(
        !h.root.is_focus_active(),
        "the return to a is owed, not run"
    );
    h.handle("b").request();
    h.rebuild();
    assert!(h.handle("b").is_focused());
    h.asked = true;
    h.pump();
    assert_eq!(h.focused(), Some("b"), "the return to a never ran");
}

#[test]
fn a_notification_reaches_the_widget_whose_focus_a_modal_trap_ended() {
    let log = log_with_handles(&["a"]);
    let mut h = Harness::with_log(log, field_and_modal);
    h.tap(5.0, row(0));
    assert_eq!(h.notices(), vec![("a", true)], "a claim is announced too");

    h.open("m");
    assert!(!h.root.is_focus_active());
    assert_eq!(h.notices(), vec![("a", false)]);
    assert!(!h.handle("a").is_focused());
}

#[test]
fn an_ancestors_handle_reads_unfocused_while_a_descendant_holds() {
    let log = log_with_handles(&["outer", "inner"]);
    let mut h = Harness::with_log(log, nested);
    h.tap(5.0, row(0));
    assert!(h.handle("inner").is_focused());
    assert!(
        !h.handle("outer").is_focused(),
        "an ancestor is not the holder"
    );
    assert_eq!(h.notices(), vec![("inner", true)]);

    h.log.borrow_mut().gone.insert("outer");
    h.asked = true;
    h.pump();
    assert!(
        !h.root.is_focus_active(),
        "the orphan release ended the session"
    );
    assert!(!h.handle("outer").is_focused());
    assert_eq!(
        h.notices(),
        vec![],
        "the ancestor never held, so none is owed"
    );
}

#[test]
fn a_request_on_an_ancestor_moves_the_session_to_it() {
    let log = log_with_handles(&["outer", "inner"]);
    let mut h = Harness::with_log(log, nested);
    h.tap(5.0, row(0));
    let epoch = h.root.focus_epoch();

    h.handle("outer").request();
    h.pump();
    assert!(h.root.is_focus_active());
    assert_ne!(
        h.root.focus_epoch(),
        epoch,
        "the request was not dropped as held"
    );
    assert!(h.handle("outer").is_focused());
    assert!(!h.handle("inner").is_focused());
}

#[test]
fn a_release_on_an_ancestor_leaves_the_descendants_session_alone() {
    let log = log_with_handles(&["outer", "inner"]);
    let mut h = Harness::with_log(log, nested);
    h.tap(5.0, row(0));
    let epoch = h.root.focus_epoch();

    h.handle("outer").release();
    h.pump();
    assert!(h.root.is_focus_active());
    assert_eq!(h.root.focus_epoch(), epoch);
    assert!(h.handle("inner").is_focused());
    assert_eq!(h.focused(), Some("inner"));
}

#[test]
fn unmounting_the_holders_ancestor_ends_the_session_and_notifies_the_survivors_only() {
    let log = log_with_handles(&["a", "inner"]);
    let mut h = Harness::with_log(log, nested);
    h.tap(5.0, row(0));
    assert_eq!(h.notices(), vec![("inner", true)]);

    h.log.borrow_mut().removed.insert("outer");
    h.asked = true;
    h.pump();
    assert!(!h.root.is_focus_active());
    assert!(
        !h.handle("inner").is_focused(),
        "the unbound handle reads unfocused"
    );
    assert!(!h.handle("a").is_focused());
    assert_eq!(h.notices(), vec![], "the survivor never held focus");
    assert_eq!(h.focused(), None);
}

#[test]
fn a_kept_press_stamp_never_comes_back_as_a_later_claim_epoch() {
    let log = log_with_handles(&[]);
    log.borrow_mut().claim.insert("late_claimer");
    log.borrow_mut().keep.insert("keeper");
    let mut h = Harness::with_log(log, two_fields);
    h.tap(5.0, row(0));
    // The first kept press has a claimer that ran before the keep, so its stamp stands (a keep
    // made first suppresses the claim outright); the second claims nothing. No Up and no paint
    // runs between them (an Up clears a pod's link), so the stamp is still there when the claim
    // lands.
    for y in [row(6), row(4), row(1)] {
        h.root.event(&mut (), &pointer(PointerPhase::Down, 5.0, y));
    }
    h.asked = true;
    h.pump();
    assert_eq!(h.focused(), Some("b"));
    let mut holders = 0;
    let root = h.root.tree.pod(h.root.root_id.unwrap()).unwrap();
    root.widget()
        .visit_children(&mut |child| count_live_links(child, &mut holders));
    assert_eq!(holders, 1, "only b holds a link on the live session");
}

/// The pods under `pod` (itself included) whose link names the live session.
fn count_live_links(pod: &ChildPod, count: &mut usize) {
    *count += usize::from(pod.holds_live_focus());
    pod.widget()
        .visit_children(&mut |child| count_live_links(child, count));
}

#[test]
fn a_notification_for_a_handle_request_lands_on_an_on_demand_driver() {
    let log = log_with_handles(&["a", "b"]);
    let mut h = Harness::with_log(log, two_fields);
    h.tap(5.0, row(0));
    h.notices();
    let paints = h.paints;

    // Nothing asked for a frame: an idle driver runs none.
    h.pump();
    assert_eq!(h.paints, paints);

    h.handle("b").request();
    h.pump();
    assert!(h.paints > paints, "the request reached a frame by itself");
    let mut seen = h.notices();
    seen.sort_by_key(|entry| entry.0);
    assert_eq!(seen, vec![("a", false), ("b", true)]);
}

fn key(named: NamedKey) -> InputEvent {
    InputEvent::Key(KeyEvent {
        key: Key::Named(named),
        modifiers: Modifiers::default(),
        repeat: false,
    })
}
