//! Focus-return scopes at the root: a closed modal overlay or widget scope gives focus back to the
//! stop that held it as the scope opened, with its keyboard origin, exactly once, and only when
//! the user did not blur, the stop is still reachable and no other session is live.

use std::collections::HashSet;

use super::*;
use crate::focus_scope::FocusScopeKey;
use crate::overlay::{OverlayBand, OverlayKeys, OverlayPod};
use crate::widget::{ChildPod, PaintOutcome, Widget};

#[derive(Default)]
struct Log {
    /// `(name, is_focused_for_ring)` for every field that painted focused on the latest paint.
    focused: Vec<(&'static str, bool)>,
    open: HashSet<&'static str>,
    /// Main-tree pods the next rebuild unmounts, by debug label.
    gone: HashSet<&'static str>,
    activated: Vec<&'static str>,
}

type Shared = Rc<RefCell<Log>>;

const MODAL_RECT: Rect = Rect::new(100.0, 100.0, 200.0, 200.0);

/// A focusable field: claims on `Down`, releases on Escape, activates on Enter, publishes an IME
/// surface while focused.
struct Field {
    name: &'static str,
    log: Shared,
}

impl Widget for Field {
    fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        bc.constrain(Size::new(40.0, 20.0))
    }
    fn paint(&mut self, ctx: &mut PaintCtx, _scene: &mut dyn PaintScene) {
        if ctx.has_focus() {
            let entry = (self.name, ctx.is_focused_for_ring());
            self.log.borrow_mut().focused.push(entry);
            ctx.publish_ime_state(ImeState {
                active: true,
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
            InputEvent::Key(key) if key.key == Key::Named(NamedKey::Enter) => {
                self.log.borrow_mut().activated.push(self.name);
                EventResult::Handled
            }
            _ => EventResult::Ignored,
        }
    }
    fn is_focusable(&self) -> bool {
        true
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

/// A vertical stack routing like `frust-widgets`' containers: broadcasts to every child,
/// focus-routed input to the live holder, pointers by hit test.
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

fn column(children: Vec<ChildPod>) -> Column {
    Column { children }
}

/// Floats a column of fields at [`MODAL_RECT`] while its name is open, forwarding its overlay
/// events and, while the pod holds focus, focus-routed input.
struct OverlayHost {
    name: &'static str,
    key: OverlayKey,
    keys: OverlayKeys,
    outside_tap: OutsideTap,
    pod: OverlayPod,
    log: Shared,
}

impl Widget for OverlayHost {
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
            outside_tap: self.outside_tap,
            keys: self.keys,
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

fn overlay_host(
    name: &'static str,
    keys: OverlayKeys,
    outside_tap: OutsideTap,
    fields: &[&'static str],
    log: &Shared,
) -> ChildPod {
    let pod = column(fields.iter().map(|name| field(name, log)).collect());
    ChildPod::new(Box::new(OverlayHost {
        name,
        key: OverlayKey::next(),
        keys,
        outside_tap,
        pod: Rc::new(RefCell::new(ChildPod::new(Box::new(pod)))),
        log: Rc::clone(log),
    }))
}

fn modal(name: &'static str, fields: &[&'static str], log: &Shared) -> ChildPod {
    overlay_host(name, OverlayKeys::Modal, OutsideTap::Ignore, fields, log)
}

#[derive(Clone, Copy, Default)]
struct ScopeSpec {
    /// Claims the session on a `Down` while open, as the beUI and shadcn hosts do.
    claims: bool,
    /// Releases the session when Escape closes it.
    releases: bool,
    /// Publishes an inactive IME surface the paint it opens, as a navigator push does.
    clears_ime: bool,
    /// Registers once more in the paint that closes it, as a host a control inside can close does.
    lingers: bool,
}

/// A main-tree scope host: registers its [`FocusScopeKey`] every paint its name is open; a
/// focused Escape closes it.
struct ScopeHost {
    name: &'static str,
    key: FocusScopeKey,
    spec: ScopeSpec,
    was_open: bool,
    log: Shared,
}

impl Widget for ScopeHost {
    fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        bc.constrain(Size::new(40.0, 20.0))
    }
    fn paint(&mut self, ctx: &mut PaintCtx, _scene: &mut dyn PaintScene) {
        let open = self.log.borrow().open.contains(self.name);
        if open || (self.spec.lingers && self.was_open) {
            ctx.register_focus_scope(self.key);
        }
        if open && self.spec.clears_ime && !self.was_open {
            ctx.publish_ime_state(ImeState::default());
        }
        self.was_open = open;
    }
    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        let open = self.log.borrow().open.contains(self.name);
        match event {
            InputEvent::Pointer(p) if p.phase == PointerPhase::Down && open => {
                if self.spec.claims {
                    ctx.request_focus();
                }
                EventResult::Handled
            }
            InputEvent::Key(key) if key.key == Key::Named(NamedKey::Escape) && open => {
                if self.spec.releases {
                    ctx.release_focus();
                }
                self.log.borrow_mut().open.remove(self.name);
                EventResult::Handled
            }
            _ => EventResult::Ignored,
        }
    }
}

fn scope_host(name: &'static str, spec: ScopeSpec, log: &Shared) -> ChildPod {
    ChildPod::new(Box::new(ScopeHost {
        name,
        key: FocusScopeKey::new(),
        spec,
        was_open: false,
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
        let before = element.children.len();
        let kept = |pod: &ChildPod| {
            pod.debug_label()
                .is_none_or(|name| !log.gone.contains(name))
        };
        element.children.retain(kept);
        if element.children.len() == before {
            ChangeFlags::NONE
        } else {
            ChangeFlags::LAYOUT | ChangeFlags::PAINT
        }
    }
}

struct Harness {
    root: RenderRoot<(), TreeView>,
    make: fn(&Shared) -> Column,
    log: Shared,
}

impl Harness {
    fn new(make: fn(&Shared) -> Column) -> Self {
        let mut harness = Self {
            root: RenderRoot::new(),
            make,
            log: Shared::default(),
        };
        harness.frame();
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
        self.root.layout(Size::new(200.0, 200.0));
        self.root
            .paint(&mut RecordingScene::default(), FrameTime::ZERO)
    }

    fn frame(&mut self) -> PaintOutcome {
        self.rebuild();
        self.paint()
    }

    fn send(&mut self, event: &InputEvent) -> EventOutcome {
        let outcome = self.root.event(&mut (), event);
        self.frame();
        outcome
    }

    fn tap(&mut self, x: f64, y: f64) {
        self.root.event(&mut (), &pointer(PointerPhase::Down, x, y));
        self.send(&pointer(PointerPhase::Up, x, y));
    }

    fn tab(&mut self) {
        self.send(&key(NamedKey::Tab));
    }

    fn escape(&mut self) {
        self.send(&key(NamedKey::Escape));
    }

    fn open(&mut self, name: &'static str) {
        self.log.borrow_mut().open.insert(name);
        self.frame();
    }

    /// Close `name` and run the frame whose paint sees it gone: the return is owed, not run.
    fn close(&mut self, name: &'static str) -> PaintOutcome {
        self.log.borrow_mut().open.remove(name);
        self.frame()
    }

    /// The deepest field that painted focused (paint order is pre-order, floated pods last).
    fn focused(&self) -> Option<&'static str> {
        self.log.borrow().focused.last().map(|entry| entry.0)
    }

    fn ringed(&self) -> Vec<&'static str> {
        let log = self.log.borrow();
        log.focused.iter().filter(|e| e.1).map(|e| e.0).collect()
    }
}

fn key(named: NamedKey) -> InputEvent {
    InputEvent::Key(KeyEvent {
        key: Key::Named(named),
        modifiers: Modifiers::default(),
        repeat: false,
    })
}

fn field_and_modal(log: &Shared) -> Column {
    column(vec![field("field", log), modal("modal", &["m1"], log)])
}

#[test]
fn a_closed_modal_overlay_returns_focus_to_the_field_it_covered() {
    let mut h = Harness::new(field_and_modal);
    h.tap(5.0, 5.0);
    assert_eq!(h.focused(), Some("field"));
    h.open("modal");
    assert!(
        !h.root.is_focus_active(),
        "the trap ended the field's session"
    );

    let outcome = h.close("modal");
    assert!(
        outcome.needs_frame,
        "the close wakes the frame that returns"
    );
    assert!(!outcome.needs_frame_paced_only);
    assert!(!h.root.is_focus_active(), "owed to the next rebuild");
    h.rebuild();
    assert!(h.root.is_focus_active(), "one rebuild returned the session");
    h.paint();
    assert_eq!(h.focused(), Some("field"));
    assert!(h.root.ime_state().is_some(), "the field publishes again");
    assert!(
        h.ringed().is_empty(),
        "a pointer-origin focus shows no ring"
    );
}

#[test]
fn a_widget_registered_scope_returns_focus_like_a_modal_overlay() {
    let mut h = Harness::new(|log| {
        let spec = ScopeSpec {
            clears_ime: true,
            ..ScopeSpec::default()
        };
        column(vec![field("field", log), scope_host("dialog", spec, log)])
    });
    h.tap(5.0, 5.0);
    h.open("dialog");
    assert!(!h.root.is_focus_active(), "the cleared surface ended it");
    assert_eq!(h.root.focus_returns.stack().len(), 1);

    assert!(h.close("dialog").needs_frame);
    h.rebuild();
    assert!(h.root.is_focus_active());
    h.paint();
    assert_eq!(h.focused(), Some("field"));
    assert!(h.root.ime_state().is_some());
}

#[test]
fn a_scope_host_that_takes_the_session_returns_it_to_the_field_it_had() {
    let mut h = Harness::new(|log| {
        let spec = ScopeSpec {
            claims: true,
            releases: true,
            ..ScopeSpec::default()
        };
        column(vec![field("field", log), scope_host("dialog", spec, log)])
    });
    h.tap(5.0, 5.0);
    h.open("dialog");
    h.tap(5.0, 25.0);
    assert!(h.root.is_focus_active(), "the host holds the session");
    assert_eq!(h.focused(), None, "and the field lost it");

    h.escape();
    assert!(
        !h.root.is_focus_active(),
        "the host released it as it closed"
    );
    h.rebuild();
    h.paint();
    assert_eq!(h.focused(), Some("field"));
}

#[test]
fn focus_comes_back_with_the_keyboard_origin_it_was_committed_with() {
    let mut h = Harness::new(|log| {
        let children = vec![
            field("field", log),
            field("other", log),
            modal("modal", &["m1"], log),
        ];
        column(children)
    });
    h.tab();
    assert_eq!(h.ringed(), ["field"]);
    h.open("modal");
    h.tap(110.0, 105.0);
    assert_eq!(h.focused(), Some("m1"), "a press inside the modal");
    h.escape();
    h.close("modal");
    h.rebuild();
    h.paint();
    assert_eq!(h.focused(), Some("field"));
    assert_eq!(h.ringed(), ["field"], "Tab-origin focus returns ringed");

    h.tap(5.0, 25.0);
    assert_eq!(h.focused(), Some("other"));
    h.open("modal");
    h.close("modal");
    h.rebuild();
    h.paint();
    assert_eq!(h.focused(), Some("other"));
    assert!(
        h.ringed().is_empty(),
        "pointer-origin focus returns unringed"
    );
}

#[test]
fn a_blur_before_the_scope_opened_leaves_nothing_to_return() {
    let mut h = Harness::new(field_and_modal);
    h.tap(5.0, 5.0);
    h.tap(150.0, 5.0);
    assert!(!h.root.is_focus_active(), "a tap on nothing blurred");
    h.open("modal");
    h.close("modal");
    h.rebuild();
    h.paint();
    assert!(!h.root.is_focus_active());
    assert_eq!(h.focused(), None);
}

#[test]
fn a_field_unmounted_while_the_scope_was_up_is_not_returned_to() {
    let mut h = Harness::new(|log| {
        let children = vec![
            field("other", log),
            field("field", log),
            modal("modal", &["m1"], log),
        ];
        column(children)
    });
    h.tap(5.0, 25.0);
    assert_eq!(h.focused(), Some("field"));
    h.open("modal");
    h.log.borrow_mut().gone.insert("field");
    h.frame();
    h.close("modal");
    h.rebuild();
    h.paint();
    assert!(!h.root.is_focus_active(), "the stop is gone from the walk");
    assert_eq!(h.focused(), None);
}

#[test]
fn a_live_session_at_restore_time_is_never_stolen() {
    let mut h = Harness::new(|log| {
        let spec = ScopeSpec {
            claims: true,
            ..ScopeSpec::default()
        };
        let dialog = scope_host("dialog", spec, log);
        column(vec![field("field", log), dialog, field("other", log)])
    });
    h.tap(5.0, 5.0);
    h.open("dialog");
    h.tap(5.0, 25.0);
    h.escape();
    assert!(
        h.root.is_focus_active(),
        "the host closed holding the session"
    );
    h.root
        .event(&mut (), &pointer(PointerPhase::Down, 5.0, 45.0));
    assert!(h.root.is_focus_active(), "the user took another control");
    h.send(&pointer(PointerPhase::Up, 5.0, 45.0));
    assert_eq!(h.focused(), Some("other"), "the field took nothing");
    assert!(h.root.focus_returns.take_pending().is_none(), "consumed");
}

#[test]
fn an_event_before_the_rebuild_runs_the_return_and_the_rebuild_does_not_repeat_it() {
    let mut h = Harness::new(field_and_modal);
    h.tap(5.0, 5.0);
    h.open("modal");
    h.close("modal");
    let outcome = h.root.event(&mut (), &key(NamedKey::ArrowLeft));
    assert!(h.root.is_focus_active(), "returned by the end of the pass");
    assert!(outcome.needs_redraw);

    h.tap(150.0, 5.0);
    assert!(
        !h.root.is_focus_active(),
        "the blur stands through the rebuild"
    );
    assert_eq!(h.focused(), None);
}

#[test]
fn nested_scopes_return_inner_to_the_outer_stop_then_outer_to_the_main_tree() {
    let mut h = Harness::new(|log| {
        let children = vec![
            field("field", log),
            modal("outer", &["o1", "o2"], log),
            modal("inner", &["i1"], log),
        ];
        column(children)
    });
    h.tab();
    h.open("outer");
    h.tab();
    h.tab();
    assert_eq!(h.focused(), Some("o2"));
    h.open("inner");
    h.tab();
    assert_eq!(
        h.focused(),
        Some("i1"),
        "traversal moved into the inner modal"
    );

    h.escape();
    h.close("inner");
    h.rebuild();
    h.paint();
    assert_eq!(
        h.focused(),
        Some("o2"),
        "the inner scope returns to the outer stop"
    );
    assert_eq!(h.ringed(), ["o2"]);

    h.escape();
    h.close("outer");
    h.rebuild();
    h.paint();
    assert_eq!(
        h.focused(),
        Some("field"),
        "the outer scope returns to the main tree"
    );
}

#[test]
fn a_light_dismiss_menu_neither_captures_nor_restores() {
    let mut h = Harness::new(|log| {
        let consume = OutsideTap::Notify { consume: true };
        let menu = overlay_host("menu", OverlayKeys::Pass, consume, &["m1"], log);
        column(vec![field("field", log), menu])
    });
    h.tap(5.0, 5.0);
    h.open("menu");
    assert!(h.root.is_focus_active(), "a menu traps nothing");
    assert!(
        h.root.focus_returns.stack().is_empty(),
        "and opens no scope"
    );
    h.tap(110.0, 105.0);
    assert_eq!(h.focused(), Some("m1"));
    h.escape();

    assert!(!h.close("menu").needs_frame, "its close owes nothing");
    h.rebuild();
    h.paint();
    assert!(!h.root.is_focus_active());
    assert_eq!(h.focused(), None);
}

fn claiming_dialog(log: &Shared) -> Column {
    let spec = ScopeSpec {
        claims: true,
        ..ScopeSpec::default()
    };
    column(vec![field("field", log), scope_host("dialog", spec, log)])
}

#[test]
fn a_modal_closed_while_a_stop_inside_holds_focus_returns_it() {
    let mut h = Harness::new(field_and_modal);
    h.tap(5.0, 5.0);
    h.open("modal");
    h.tab();
    assert_eq!(h.focused(), Some("m1"), "Tab moved into the modal");

    h.close("modal");
    assert!(h.root.is_focus_active(), "m1 still holds the session");
    h.rebuild();
    assert!(h.root.is_focus_active(), "one rebuild returned the session");
    h.paint();
    assert_eq!(h.focused(), Some("field"));
    assert!(h.root.ime_state().is_some(), "the field publishes again");
    assert!(h.ringed().is_empty(), "with the pointer origin it had");
}

#[test]
fn a_scope_host_that_closes_holding_the_session_gives_it_back() {
    let mut h = Harness::new(claiming_dialog);
    h.tap(5.0, 5.0);
    h.open("dialog");
    h.tap(5.0, 25.0);
    assert_eq!(h.focused(), None, "the host holds the session");

    h.escape();
    assert!(h.root.is_focus_active(), "no release as it closed");
    h.rebuild();
    assert!(h.root.is_focus_active());
    h.paint();
    assert_eq!(h.focused(), Some("field"));
    assert!(h.root.ime_state().is_some());
}

#[test]
fn a_modal_closed_by_a_claim_made_with_no_paint_between_still_returns_focus() {
    let mut h = Harness::new(field_and_modal);
    h.tap(5.0, 5.0);
    h.open("modal");
    // The claim and the close share a frame, as an accessibility click's Down and Up do.
    h.root.event(&mut (), &key(NamedKey::Tab));
    assert!(h.root.is_focus_active(), "m1 took the session unpainted");

    h.close("modal");
    assert!(h.root.is_focus_active(), "m1 still holds the session");
    h.rebuild();
    h.paint();
    assert_eq!(h.focused(), Some("field"));
    assert!(h.root.ime_state().is_some(), "the field publishes again");
}

#[test]
fn a_scope_host_closed_in_the_frame_it_claimed_returns_focus_if_it_lingers_a_paint() {
    let mut h = Harness::new(|log| {
        let spec = ScopeSpec {
            claims: true,
            lingers: true,
            ..ScopeSpec::default()
        };
        column(vec![field("field", log), scope_host("dialog", spec, log)])
    });
    h.tap(5.0, 5.0);
    h.open("dialog");
    // Claim and close with no paint between: the host's last open paint saw no focus path.
    h.root
        .event(&mut (), &pointer(PointerPhase::Down, 5.0, 25.0));
    h.root.event(&mut (), &pointer(PointerPhase::Up, 5.0, 25.0));
    h.root.event(&mut (), &key(NamedKey::Escape));
    assert!(h.root.is_focus_active(), "no release as it closed");

    h.frame();
    assert!(
        h.root.is_focus_active(),
        "the lingering paint still registers"
    );
    h.frame();
    h.rebuild();
    h.paint();
    assert_eq!(h.focused(), Some("field"));
}

#[test]
fn a_session_started_after_a_modal_closed_holding_focus_is_kept() {
    let mut h = Harness::new(|log| {
        let children = vec![
            field("field", log),
            field("other", log),
            modal("modal", &["m1"], log),
        ];
        column(children)
    });
    h.tap(5.0, 5.0);
    h.open("modal");
    h.tab();
    h.close("modal");

    h.root
        .event(&mut (), &pointer(PointerPhase::Down, 5.0, 25.0));
    h.send(&pointer(PointerPhase::Up, 5.0, 25.0));
    assert_eq!(h.focused(), Some("other"));
    h.frame();
    assert_eq!(h.focused(), Some("other"), "nothing returns later either");
}

#[test]
fn a_key_that_beats_the_rebuild_never_reaches_the_restored_control() {
    let mut h = Harness::new(field_and_modal);
    h.tab();
    h.open("modal");
    h.tab();
    h.escape();
    h.close("modal");

    let enter = InputEvent::Key(KeyEvent {
        key: Key::Named(NamedKey::Enter),
        modifiers: Modifiers::default(),
        repeat: true,
    });
    let outcome = h.root.event(&mut (), &enter);
    assert!(
        h.log.borrow().activated.is_empty(),
        "the repeat found no session"
    );
    assert!(h.root.is_focus_active(), "and focus returned after it");
    assert!(outcome.needs_redraw);
    h.paint();
    assert_eq!(h.focused(), Some("field"));
    assert_eq!(h.ringed(), ["field"]);
}
