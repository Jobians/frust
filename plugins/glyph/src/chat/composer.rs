//! [`composer`]: the prompt input at the bottom of a chat screen.
//!
//! # Contract
//!
//! - **Controlled and non-reactive.** The caller owns the text: `.on_change`
//!   reports each edit, and the composer never clears itself after a submit;
//!   the caller sets the next value.
//! - **Field.** A multi-line `text_input` that is one line tall when empty,
//!   grows with its content up to `.max_visible_lines(n)` (default 6), then
//!   scrolls inside. The growth is ordinary layout, so a parent sees the new
//!   height on the same pass. The field's own Enter and IME handling is used
//!   as is: Enter never submits while a composition is active.
//! - **Submit.** The send control and, with `.submit_on_enter(true)`, Enter
//!   run `.on_submit(app state, text)` when the trimmed text is non-empty and
//!   the composer is not disabled. With `false` (default) Enter inserts a
//!   newline; Shift+Enter always does the opposite of the setting.
//! - **Running.** `.running(true)` turns the send control into a stop control
//!   that runs `.on_stop`; typing stays allowed.
//! - **Disabled.** `.disabled(reason)` shows the reason as a line above the
//!   field and disables the field and send. A running stop control stays live.
//! - **Trailing action.** `.trailing_action(view)` fills one icon-sized slot
//!   between the field and the send control; the send control never moves.
//! - **Semantics.** The field is labelled by the placeholder; send and stop
//!   are buttons with labels; the reason is plain text.

use std::rc::Rc;

use frust::authoring::text::OverflowWrap;
use frust::authoring::{
    Action, AnyView, BezPath, BoxConstraints, Brush, BuildCtx, ChangeFlags, ChildPod, Color,
    ErasedArgCallback, ErasedCallback, EventCtx, EventResult, InputEvent, LayoutCtx, PaintCtx,
    PaintScene, Point, PointerPhase, Rect, Role, SemanticsCtx, Size, ThemeTextColor, ThemeTextType,
    View, Widget, any, build_child, erase_callback, erase_callback_arg, rebuild_child,
    route_event_single, teardown_child,
};
use frust::{Theme, text, text_input};

use super::style;
use crate::press::presses;

/// Touch target of the send, stop and trailing slots.
const TARGET: f64 = style::MIN_TARGET;
/// Diameter of the painted disc inside a send or stop target.
const DISC: f64 = 36.0;
/// Edge of the square the send arrow is drawn in.
const ARROW_BOX: f64 = 18.0;
/// Edge of the stop square.
const STOP_BOX: f64 = 12.0;
/// Corner radius of the stop square.
const STOP_RADIUS: f64 = 2.0;
/// Field line cap when the caller names none.
const DEFAULT_MAX_LINES: usize = 6;
/// Placeholder when the caller sets none.
const DEFAULT_PLACEHOLDER: &str = "Message";
/// Accessible name of the send control.
const SEND_LABEL: &str = "Send";
/// Accessible name of the stop control.
const STOP_LABEL: &str = "Stop";

type OnChange<State> = Rc<dyn Fn(&mut State, String)>;
type OnStop<State> = Rc<dyn Fn(&mut State)>;

/// A declarative chat composer. See the [module docs](self).
pub struct ComposerView<State: 'static> {
    value: String,
    placeholder: String,
    max_lines: usize,
    submit_on_enter: bool,
    running: bool,
    disabled: Option<String>,
    on_change: Option<OnChange<State>>,
    on_submit: Option<OnChange<State>>,
    on_stop: Option<OnStop<State>>,
    trailing: Option<AnyView<State>>,
}

impl<State: 'static> std::fmt::Debug for ComposerView<State> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComposerView")
            .field("value", &self.value)
            .field("placeholder", &self.placeholder)
            .field("max_lines", &self.max_lines)
            .field("submit_on_enter", &self.submit_on_enter)
            .field("running", &self.running)
            .field("disabled", &self.disabled)
            .finish_non_exhaustive()
    }
}

/// A prompt input showing `value`. The caller owns the text: it stores what
/// `.on_change(..)` reports, passes it back, and sets the next value after a
/// submit (the composer never clears itself).
pub fn composer<State: 'static>(value: impl Into<String>) -> ComposerView<State> {
    ComposerView {
        value: value.into(),
        placeholder: DEFAULT_PLACEHOLDER.to_owned(),
        max_lines: DEFAULT_MAX_LINES,
        submit_on_enter: false,
        running: false,
        disabled: None,
        on_change: None,
        on_submit: None,
        on_stop: None,
        trailing: None,
    }
}

impl<State: 'static> ComposerView<State> {
    /// Run `callback` with the requested text on every edit.
    pub fn on_change(mut self, callback: impl Fn(&mut State, String) + 'static) -> Self {
        self.on_change = Some(Rc::new(callback));
        self
    }

    /// Run `callback` with the text when the user submits it.
    pub fn on_submit(mut self, callback: impl Fn(&mut State, String) + 'static) -> Self {
        self.on_submit = Some(Rc::new(callback));
        self
    }

    /// Run `callback` when the stop control is released inside it.
    pub fn on_stop(mut self, callback: impl Fn(&mut State) + 'static) -> Self {
        self.on_stop = Some(Rc::new(callback));
        self
    }

    /// A turn is in progress: the send control becomes a stop control.
    pub fn running(mut self, running: bool) -> Self {
        self.running = running;
        self
    }

    /// Disable input and send, showing `reason` above the field.
    pub fn disabled(mut self, reason: impl Into<String>) -> Self {
        self.disabled = Some(reason.into());
        self
    }

    /// The field's placeholder, which also labels it for assistive tech.
    pub fn placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// Lines the field grows to before it scrolls (default 6, at least 1).
    pub fn max_visible_lines(mut self, lines: usize) -> Self {
        self.max_lines = lines.max(1);
        self
    }

    /// Whether Enter submits (`true`, desktop) or inserts a newline (`false`,
    /// the default). Shift+Enter does the opposite.
    pub fn submit_on_enter(mut self, submit_on_enter: bool) -> Self {
        self.submit_on_enter = submit_on_enter;
        self
    }

    /// One icon-sized action between the field and the send control.
    pub fn trailing_action(mut self, view: impl View<State> + 'static) -> Self {
        self.trailing = Some(any(view));
        self
    }

    fn can_send(&self) -> bool {
        self.disabled.is_none() && !self.value.trim().is_empty()
    }

    /// The field. Called for build, rebuild and teardown alike.
    fn field_view(&self) -> frust::TextInputView<State> {
        let on_change = self.on_change.clone();
        let on_submit = self.on_submit.clone();
        let locked = self.disabled.is_some();
        text_input(self.value.clone(), move |s: &mut State, t: String| {
            if let Some(f) = &on_change {
                f(s, t);
            }
        })
        .multiline(self.max_lines)
        .submit_on_enter(self.submit_on_enter)
        .on_submit(move |s: &mut State, t: String| {
            if !locked
                && !t.trim().is_empty()
                && let Some(f) = &on_submit
            {
                f(s, t);
            }
        })
        .placeholder(self.placeholder.clone())
        .label(self.placeholder.clone())
        .themed_family(ThemeTextType::BodyMedium)
        .enabled(!locked)
    }
}

fn reason_view<State: 'static>(reason: &str) -> impl View<State> + use<State> {
    text(reason)
        .size(style::LABEL_FONT_SIZE)
        .themed_role(ThemeTextColor::OnSurfaceVariant)
        .overflow_wrap(OverflowWrap::Anywhere)
}

/// Which control the right-hand slot shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Control {
    SendIdle,
    SendReady,
    Stop,
}

impl Control {
    fn of(running: bool, can_send: bool) -> Self {
        match (running, can_send) {
            (true, _) => Control::Stop,
            (false, true) => Control::SendReady,
            (false, false) => Control::SendIdle,
        }
    }

    fn live(self) -> bool {
        self != Control::SendIdle
    }

    fn label(self) -> &'static str {
        if self == Control::Stop {
            STOP_LABEL
        } else {
            SEND_LABEL
        }
    }
}

/// The retained widget for a [`ComposerView`].
pub struct ComposerWidget {
    field: ChildPod,
    reason: Option<ChildPod>,
    trailing: Option<ChildPod>,
    value: String,
    control: Control,
    on_submit: Option<ErasedArgCallback<String>>,
    on_stop: Option<ErasedCallback>,
    control_rect: Rect,
    pressed: bool,
    captured: bool,
}

impl ComposerWidget {
    fn hits_control(&self, p: Point) -> bool {
        self.control.live() && self.control_rect.contains(p)
    }

    fn fire(&mut self, ctx: &mut EventCtx) {
        match self.control {
            Control::Stop => {
                if let Some(f) = self.on_stop.as_mut() {
                    f(ctx);
                }
            }
            Control::SendReady => {
                let value = self.value.clone();
                if let Some(f) = self.on_submit.as_mut() {
                    f(ctx, value);
                }
            }
            Control::SendIdle => {}
        }
    }

    fn route(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        if let Some(t) = self.trailing.as_mut()
            && matches!(route_event_single(t, ctx, event), EventResult::Handled)
        {
            return EventResult::Handled;
        }
        route_event_single(&mut self.field, ctx, event)
    }

    fn paint_control(&self, scene: &mut dyn PaintScene, origin: Point, colors: ControlColors) {
        let r = self.control_rect;
        let at = Point::new(origin.x + r.x0, origin.y + r.y0);
        let disc_at = Point::new(at.x + (TARGET - DISC) / 2.0, at.y + (TARGET - DISC) / 2.0);
        let (fill, ink) = match self.control {
            Control::SendIdle => (colors.idle_fill, colors.idle_ink),
            Control::SendReady => (colors.send_fill, colors.send_ink),
            Control::Stop => (colors.stop_fill, colors.stop_ink),
        };
        let fill = if self.pressed {
            style::mix(fill, ink, style::PRESSED_TINT)
        } else {
            fill
        };
        scene.fill_rounded_rect(disc_at, Size::new(DISC, DISC), DISC / 2.0, fill);
        let centre = Point::new(at.x + TARGET / 2.0, at.y + TARGET / 2.0);
        match self.control {
            Control::Stop => {
                let corner = Point::new(centre.x - STOP_BOX / 2.0, centre.y - STOP_BOX / 2.0);
                scene.fill_rounded_rect(corner, Size::new(STOP_BOX, STOP_BOX), STOP_RADIUS, ink);
            }
            _ => {
                let corner = Point::new(centre.x - ARROW_BOX / 2.0, centre.y - ARROW_BOX / 2.0);
                scene.fill_path(corner, &arrow_path(), &Brush::Solid(ink));
            }
        }
    }
}

/// An up arrow filling `ARROW_BOX`.
fn arrow_path() -> BezPath {
    let s = ARROW_BOX / 18.0;
    let mut p = BezPath::new();
    for (i, (x, y)) in [
        (9.0, 1.0),
        (16.5, 8.5),
        (11.5, 8.5),
        (11.5, 17.0),
        (6.5, 17.0),
        (6.5, 8.5),
        (1.5, 8.5),
    ]
    .iter()
    .enumerate()
    {
        let pt = Point::new(x * s, y * s);
        if i == 0 {
            p.move_to(pt);
        } else {
            p.line_to(pt);
        }
    }
    p.close_path();
    p
}

/// The control's resolved colours.
#[derive(Clone, Copy)]
struct ControlColors {
    send_fill: Color,
    send_ink: Color,
    stop_fill: Color,
    stop_ink: Color,
    idle_fill: Color,
    idle_ink: Color,
}

impl ControlColors {
    fn resolve(theme: Option<&Theme>) -> Self {
        let scheme = style::scheme(theme);
        Self {
            send_fill: scheme.primary,
            send_ink: scheme.on_primary,
            stop_fill: scheme.on_surface,
            stop_ink: scheme.surface,
            idle_fill: style::surface_high(theme),
            idle_ink: style::muted(theme),
        }
    }
}

impl<State: 'static> View<State> for ComposerView<State> {
    type Element = ComposerWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> ComposerWidget {
        ComposerWidget {
            field: build_child(&self.field_view(), ctx),
            reason: self
                .disabled
                .as_deref()
                .map(|r| build_child(&reason_view::<State>(r), ctx)),
            trailing: self.trailing.as_ref().map(|v| build_child(v, ctx)),
            value: self.value.clone(),
            control: Control::of(self.running, self.can_send()),
            on_submit: self.on_submit.as_ref().map(erase_callback_arg),
            on_stop: self.on_stop.as_ref().map(erase_callback),
            control_rect: Rect::ZERO,
            pressed: false,
            captured: false,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut ComposerWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = rebuild_child(
            &prev.field_view(),
            &self.field_view(),
            &mut element.field,
            ctx,
        );
        match (&prev.disabled, &self.disabled) {
            (Some(old), Some(new)) => {
                if let Some(pod) = element.reason.as_mut() {
                    flags |= rebuild_child(
                        &reason_view::<State>(old),
                        &reason_view::<State>(new),
                        pod,
                        ctx,
                    );
                }
            }
            (Some(old), None) => {
                if let Some(mut pod) = element.reason.take() {
                    teardown_child(&reason_view::<State>(old), &mut pod, ctx);
                }
                flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
            }
            (None, Some(new)) => {
                element.reason = Some(build_child(&reason_view::<State>(new), ctx));
                flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
            }
            (None, None) => {}
        }
        match (&prev.trailing, &self.trailing) {
            (Some(old), Some(new)) => {
                if let Some(pod) = element.trailing.as_mut() {
                    flags |= rebuild_child(old, new, pod, ctx);
                }
            }
            (Some(old), None) => {
                if let Some(mut pod) = element.trailing.take() {
                    teardown_child(old, &mut pod, ctx);
                }
                flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
            }
            (None, Some(new)) => {
                element.trailing = Some(build_child(new, ctx));
                flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
            }
            (None, None) => {}
        }
        element.on_submit = self.on_submit.as_ref().map(erase_callback_arg);
        element.on_stop = self.on_stop.as_ref().map(erase_callback);
        element.value.clone_from(&self.value);
        let control = Control::of(self.running, self.can_send());
        if element.control != control {
            element.control = control;
            if !control.live() {
                element.pressed = false;
                element.captured = false;
            }
            // Semantics-only too, but `PAINT` is what reaches the semantics gate.
            flags |= ChangeFlags::PAINT;
        }
        flags
    }

    fn teardown(&self, element: &mut ComposerWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.field_view(), &mut element.field, ctx);
        if let (Some(r), Some(pod)) = (self.disabled.as_deref(), element.reason.as_mut()) {
            teardown_child(&reason_view::<State>(r), pod, ctx);
        }
        if let (Some(v), Some(pod)) = (self.trailing.as_ref(), element.trailing.as_mut()) {
            teardown_child(v, pod, ctx);
        }
    }
}

impl Widget for ComposerWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let width = style::width_of(bc);
        let pad = style::PAD_SMALL;
        let inner = (width - pad * 2.0).max(0.0);
        let loose_w = |w: f64| BoxConstraints::new(Size::new(w, 0.0), Size::new(w, f64::INFINITY));

        let mut y = pad;
        if let Some(reason) = self.reason.as_mut() {
            let got = reason.layout_child(ctx, &loose_w(inner));
            reason.set_origin(Point::new(pad, y));
            y += got.height + pad;
        }

        let slot = TARGET + pad;
        let trailing_w = if self.trailing.is_some() { slot } else { 0.0 };
        let field_w = (inner - slot - trailing_w).max(0.0);
        let field = self.field.layout_child(ctx, &loose_w(field_w));
        let row_h = field.height.max(TARGET);
        self.field
            .set_origin(Point::new(pad, y + (row_h - field.height) / 2.0));

        let control_x = pad + (inner - TARGET).max(0.0);
        let control_y = y + row_h - TARGET;
        self.control_rect = Rect::new(control_x, control_y, control_x + TARGET, control_y + TARGET);
        if let Some(t) = self.trailing.as_mut() {
            let tight = BoxConstraints::new(Size::ZERO, Size::new(TARGET, TARGET));
            let got = t.layout_child(ctx, &tight);
            let x = control_x - slot + (TARGET - got.width) / 2.0;
            t.set_origin(Point::new(x, control_y + (TARGET - got.height) / 2.0));
        }
        bc.constrain(Size::new(width, y + row_h + pad))
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let colors = ControlColors::resolve(Theme::from_paint_ctx(ctx));
        self.field.paint_child(ctx, scene);
        if let Some(reason) = self.reason.as_mut() {
            reason.paint_child(ctx, scene);
        }
        if let Some(t) = self.trailing.as_mut() {
            t.paint_child(ctx, scene);
        }
        self.paint_control(scene, ctx.origin(), colors);
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        let InputEvent::Pointer(p) = event else {
            return self.route(ctx, event);
        };
        match p.phase {
            PointerPhase::Down if presses(p) && self.hits_control(p.position) => {
                self.pressed = true;
                self.captured = true;
                ctx.capture_pointer();
                ctx.request_redraw();
                EventResult::Handled
            }
            PointerPhase::Move if self.captured => {
                let inside = self.hits_control(p.position);
                if inside != self.pressed {
                    self.pressed = inside;
                    ctx.request_redraw();
                }
                EventResult::Handled
            }
            PointerPhase::Up if self.captured => {
                self.captured = false;
                self.pressed = false;
                if self.hits_control(p.position) {
                    self.fire(ctx);
                }
                ctx.request_redraw();
                EventResult::Handled
            }
            PointerPhase::Cancel if self.captured => {
                self.captured = false;
                self.pressed = false;
                ctx.request_redraw();
                EventResult::Handled
            }
            _ => self.route(ctx, event),
        }
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        if let Some(reason) = &self.reason {
            reason.semantics_child(ctx);
        }
        self.field.semantics_child(ctx);
        if let Some(t) = &self.trailing {
            t.semantics_child(ctx);
        }
        let origin = ctx.origin();
        let r = self.control_rect;
        let (label, live) = (self.control.label(), self.control.live());
        ctx.push_node(Role::Button, |node| {
            node.set_label(label);
            if live {
                node.add_action(Action::Click);
            } else {
                node.set_disabled();
            }
            node.set_bounds(style::node_bounds(origin, r));
        });
    }

    frust::authoring::visit_children!(field, reason, trailing);
}

#[cfg(test)]
mod tests {
    use super::*;
    use frust::FrameTime;
    use frust::authoring::text::TextContext;
    use frust::authoring::{
        DiscardScene, ImeEvent, Key, KeyEvent, Modifiers, NamedKey, PointerButton, PointerEvent,
    };
    use frust_core::RenderRoot;
    use std::any::Any;

    #[derive(Default)]
    struct App {
        value: String,
        submits: Vec<String>,
        stops: usize,
        running: bool,
        disabled: Option<&'static str>,
        enter_submits: bool,
        trailing: bool,
        cap: Option<usize>,
    }

    fn view(app: &App) -> ComposerView<App> {
        let mut c = composer::<App>(app.value.clone())
            .on_change(|s, v| s.value = v)
            .on_submit(|s, v| s.submits.push(v))
            .on_stop(|s| s.stops += 1)
            .running(app.running)
            .submit_on_enter(app.enter_submits)
            .placeholder("Ask anything");
        if let Some(cap) = app.cap {
            c = c.max_visible_lines(cap);
        }
        if let Some(r) = app.disabled {
            c = c.disabled(r);
        }
        if app.trailing {
            c = c.trailing_action(text("mic"));
        }
        c
    }

    struct Rig {
        root: RenderRoot<App, AnyView<App>>,
        app: App,
        text: TextContext,
        width: f64,
    }

    impl Rig {
        fn new(app: App, themed: bool, width: f64) -> Self {
            let mut root = RenderRoot::new();
            if themed {
                root.set_theme(Box::new(crate::baseline()));
            }
            let mut rig = Rig {
                root,
                app,
                text: TextContext::new(),
                width,
            };
            rig.refresh();
            rig
        }

        fn refresh(&mut self) {
            self.root
                .rebuild(&mut |a: &mut App| any(view(a)), &mut self.app);
            self.root.layout_with_text(
                Size::new(self.width, 4000.0),
                &mut self.text as &mut dyn Any,
            );
        }

        fn send(&mut self, e: InputEvent) {
            self.root.event(&mut self.app, &e);
            self.refresh();
        }

        fn height(&self) -> f64 {
            self.root.inspect()[0].bounds.height()
        }

        fn click_field(&mut self) {
            for phase in [PointerPhase::Down, PointerPhase::Up] {
                self.send(pointer(phase, Point::new(20.0, 20.0)));
            }
        }

        fn key(&mut self, key: NamedKey, shift: bool) {
            self.send(InputEvent::Key(KeyEvent {
                key: Key::Named(key),
                modifiers: Modifiers {
                    shift,
                    ..Modifiers::default()
                },
                repeat: false,
            }));
        }

        fn type_text(&mut self, s: &str) {
            self.send(InputEvent::Ime(ImeEvent::Commit(s.into())));
        }

        fn button(&self) -> (String, Rect, bool) {
            let update = self.root.semantics();
            let (_, n) = update
                .nodes
                .iter()
                .find(|(_, n)| n.role() == Role::Button)
                .expect("a button");
            let b = n.bounds().expect("bounds");
            (
                n.label().unwrap_or_default().to_owned(),
                Rect::new(b.x0, b.y0, b.x1, b.y1),
                n.is_disabled(),
            )
        }

        fn tap_button(&mut self) {
            let c = self.button().1.center();
            self.send(pointer(PointerPhase::Down, c));
            self.send(pointer(PointerPhase::Up, c));
        }
    }

    fn pointer(phase: PointerPhase, position: Point) -> InputEvent {
        InputEvent::Pointer(PointerEvent {
            phase,
            position,
            button: PointerButton::Primary,
        })
    }

    fn with_value(v: &str) -> App {
        App {
            value: v.into(),
            ..App::default()
        }
    }

    #[test]
    fn height_grows_per_line_then_stops_at_the_cap() {
        let mut rig = Rig::new(
            App {
                cap: Some(3),
                ..App::default()
            },
            true,
            320.0,
        );
        let empty = rig.height();
        let mut heights = vec![empty];
        for _ in 0..5 {
            rig.app.value.push_str("line\n");
            rig.refresh();
            heights.push(rig.height());
        }
        assert!(heights[1] > heights[0], "{heights:?}");
        assert!(heights[2] > heights[1], "{heights:?}");
        assert!(
            (heights[3] - heights[2]).abs() < 0.01,
            "capped at 3 lines: {heights:?}"
        );
        assert!((heights[5] - heights[2]).abs() < 0.01, "{heights:?}");
        assert!(empty >= TARGET, "an empty composer fits the control");
    }

    #[test]
    fn send_is_disabled_for_empty_and_whitespace_text() {
        for v in ["", "   ", "\n \t"] {
            let mut rig = Rig::new(with_value(v), true, 320.0);
            let (label, _, disabled) = rig.button();
            assert_eq!((label.as_str(), disabled), ("Send", true), "{v:?}");
            rig.tap_button();
            assert!(rig.app.submits.is_empty(), "{v:?}");
        }
        let mut rig = Rig::new(with_value("hi"), true, 320.0);
        assert!(!rig.button().2);
        rig.tap_button();
        assert_eq!(rig.app.submits, ["hi"]);
    }

    #[test]
    fn running_shows_stop_which_fires_on_stop() {
        let mut rig = Rig::new(
            App {
                running: true,
                value: "typing is allowed".into(),
                ..App::default()
            },
            true,
            320.0,
        );
        let (label, _, disabled) = rig.button();
        assert_eq!((label.as_str(), disabled), ("Stop", false));
        rig.tap_button();
        assert_eq!((rig.app.stops, rig.app.submits.len()), (1, 0));
        rig.click_field();
        rig.type_text("!");
        assert!(rig.app.value.contains('!'), "typing stays allowed");
    }

    #[test]
    fn disabled_shows_its_reason_and_blocks_submit() {
        let mut rig = Rig::new(
            App {
                disabled: Some("Sign in to chat"),
                value: "hello".into(),
                enter_submits: true,
                ..App::default()
            },
            true,
            320.0,
        );
        let update = rig.root.semantics();
        assert!(
            update
                .nodes
                .iter()
                .any(|(_, n)| n.label() == Some("Sign in to chat")
                    || n.value() == Some("Sign in to chat")),
            "the reason is exposed as text"
        );
        assert!(rig.button().2);
        rig.tap_button();
        rig.click_field();
        rig.key(NamedKey::Enter, false);
        assert!(rig.app.submits.is_empty());
        assert_eq!(rig.app.value, "hello");
        let with_reason = rig.height();
        rig.app.disabled = None;
        rig.refresh();
        assert!(with_reason > rig.height(), "the reason line takes height");
    }

    #[test]
    fn enter_and_shift_enter_follow_submit_on_enter() {
        for (enter_submits, shift, submits) in [
            (true, false, true),
            (true, true, false),
            (false, false, false),
            (false, true, true),
        ] {
            let mut rig = Rig::new(
                App {
                    enter_submits,
                    ..App::default()
                },
                true,
                320.0,
            );
            rig.click_field();
            rig.type_text("go");
            rig.key(NamedKey::Enter, shift);
            let case = (enter_submits, shift);
            assert_eq!(rig.app.submits.len(), usize::from(submits), "{case:?}");
            if submits {
                assert_eq!(rig.app.submits[0], "go", "{case:?}");
            } else {
                assert_eq!(rig.app.value, "go\n", "{case:?}");
            }
        }
    }

    #[test]
    fn enter_with_blank_text_never_submits() {
        let mut rig = Rig::new(
            App {
                enter_submits: true,
                ..App::default()
            },
            true,
            320.0,
        );
        rig.click_field();
        rig.type_text("  ");
        rig.key(NamedKey::Enter, false);
        assert!(rig.app.submits.is_empty());
    }

    #[test]
    fn enter_during_composition_does_not_submit() {
        let mut rig = Rig::new(
            App {
                enter_submits: true,
                ..App::default()
            },
            true,
            320.0,
        );
        rig.click_field();
        rig.send(InputEvent::Ime(ImeEvent::Compose {
            text: "ni".into(),
            cursor: Some((2, 2)),
        }));
        rig.key(NamedKey::Enter, false);
        assert!(rig.app.submits.is_empty(), "Enter confirms the candidate");
        assert_eq!(rig.app.value, "ni");
    }

    #[test]
    fn trailing_action_does_not_move_the_send_control() {
        for width in [320.0, 1024.0] {
            let plain = Rig::new(with_value("x"), true, width);
            let with = Rig::new(
                App {
                    trailing: true,
                    value: "x".into(),
                    ..App::default()
                },
                true,
                width,
            );
            assert_eq!(plain.button().1, with.button().1, "{width}");
            let nodes = with.root.inspect();
            let slot = nodes
                .iter()
                .find(|n| n.type_name.ends_with("TextWidget"))
                .expect("the trailing text")
                .bounds;
            assert!(slot.x1 <= with.button().1.x0 + 0.01, "left of send");
            assert!(slot.width() <= TARGET + 0.01);
        }
    }

    #[test]
    fn controls_are_touch_sized_and_glyphs_differ() {
        let send = Rig::new(with_value("x"), true, 320.0);
        let stop = Rig::new(
            App {
                running: true,
                ..App::default()
            },
            true,
            320.0,
        );
        for rig in [&send, &stop] {
            let r = rig.button().1;
            assert!(r.width() >= 44.0 && r.height() >= 44.0, "{r:?}");
        }
        assert_ne!(send.button().0, stop.button().0);
        assert_ne!(Control::SendReady, Control::Stop);
    }

    #[test]
    fn lays_out_inside_320_and_1024_widths() {
        for width in [320.0, 1024.0] {
            let rig = Rig::new(with_value("a few words of text"), true, width);
            for n in rig.root.inspect() {
                assert!(n.bounds.x1 <= width + 0.01, "{} at {width}", n.type_name);
            }
            assert!((rig.root.inspect()[0].bounds.width() - width).abs() < 0.01);
        }
    }

    #[test]
    fn the_field_is_labelled_by_the_placeholder() {
        let rig = Rig::new(App::default(), true, 320.0);
        let update = rig.root.semantics();
        assert!(
            update
                .nodes
                .iter()
                .any(|(_, n)| n.label() == Some("Ask anything"))
        );
    }

    #[test]
    fn themed_and_unthemed_paint_do_not_panic() {
        for themed in [true, false] {
            for app in [
                with_value("hello"),
                App {
                    running: true,
                    trailing: true,
                    ..App::default()
                },
                App {
                    disabled: Some("busy"),
                    ..App::default()
                },
            ] {
                let mut rig = Rig::new(app, themed, 360.0);
                rig.root
                    .paint(&mut DiscardScene, FrameTime::from_nanos(1_000_000));
            }
        }
    }
}
