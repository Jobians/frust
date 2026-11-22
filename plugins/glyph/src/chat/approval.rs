//! [`approval_card`]: the card through which a user answers an agent's
//! permission request, or sees how it was answered.
//!
//! # Contract
//!
//! - **Controlled and non-reactive.** The caller owns the [`ApprovalState`]
//!   and `.busy(..)`; a tap reports the option's id through `.on_select(..)`
//!   and then disables the options until the caller changes either, or the
//!   request itself (title, detail, options or deadline), so a double tap
//!   fires once and a new request in the same slot starts fresh. A press
//!   only fires against the request it began on.
//! - **Options**: allow kinds first (filled), then reject kinds (outlined),
//!   each group in the given order; full-width buttons at least 44 px
//!   high, label wrapping to two lines then eliding.
//! - **Countdown.** `.expires(..)` takes an [`ApprovalDeadline`] made once per
//!   request; at zero the options disable. Nothing requests frames once resolved.
//! - **Untrusted text.** Control, bidi and zero-width characters in the title,
//!   detail and labels are shown as `?`; option ids are never altered.
//! - **Long detail.** Past [`DETAIL_MAX_LINES`] the block scrolls and a fixed
//!   note says more exists. Known limit (owner decision): the options stay
//!   active whether or not the rest was scrolled into view.
//! - **Semantics.** A labelled group; each option a button; the countdown a
//!   live timer in ten-second steps; the outcome a status node.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use frust::authoring::text::{
    FontWeight, OverflowWrap, TextContext, TextLayout, TextOverflow, TextStyle,
};
use frust::authoring::{
    Action, BoxConstraints, BuildCtx, ChangeFlags, ChildPod, Color, ErasedArgCallback, EventCtx,
    EventResult, InputEvent, LayoutCtx, Live, PaintCtx, PaintScene, Point, PointerPhase, Rect,
    Role, SemanticsCtx, Size, TypedArgCallback, View, Widget, build_child, erase_callback_arg,
    rebuild_child, route_event_single, teardown_child,
};
use frust::{ScrollView, TextView, Theme, scroll_view, text};
use web_time::Instant;

use super::model::{ApprovalOption, ApprovalOptionKind, ApprovalState};
use super::style::{self, display_text};
use crate::dots::DotsLoaderView;
use crate::press::presses;

/// Minimum height of an option button (a touch target).
const MIN_TARGET: f64 = style::MIN_TARGET;
const TITLE_FONT_SIZE: f32 = 14.0;
const OPTION_FONT_SIZE: f32 = style::BODY_FONT_SIZE;
/// Lines of detail shown before the block scrolls.
const DETAIL_MAX_LINES: f64 = 6.0;
/// The always-present note under a detail that scrolls.
const OVERFLOW_NOTE: &str = "More lines below, scroll to read";
const LABEL_MAX_LINES: usize = 2;
const HINT_MAX_LINES: usize = 3;
const BUTTON_PAD_X: f64 = 12.0;
const BUTTON_PAD_Y: f64 = 8.0;
const BAR_HEIGHT: f64 = 3.0;
const LOADER_ROW: f64 = 20.0;
const DISABLED_ALPHA: f32 = 0.38;
/// The tag on the allow-always and reject-always options.
const ALWAYS_TAG: &str = "always";
/// Name of the countdown's accessibility node.
const TIMER_LABEL: &str = "Time left";
/// Seconds per step of the countdown value assistive technology reads.
const SPOKEN_STEP: u64 = 10;

/// When an approval request stops waiting: made once, from the time left,
/// when the request arrives, and stored in app state, so a rebuild with an
/// equal value keeps counting from the same instant. Monotonic
/// (`web_time::Instant`), never wall-clock.
///
/// Equality compares the creation instant: a deadline built inside a view
/// function differs on every rebuild, so the countdown restarts each time
/// and the card never expires.
#[derive(Clone, Debug, PartialEq)]
pub struct ApprovalDeadline {
    start: Instant,
    total: Duration,
    clock: Clock,
}

impl ApprovalDeadline {
    /// A deadline `remaining` from now. Call it once when the request
    /// arrives and keep the value in app state; calling it from a view
    /// function restarts the countdown on every rebuild.
    pub fn after(remaining: Duration) -> Self {
        Self::with_clock(remaining, Clock::default())
    }

    /// A deadline `remaining` from `clock`'s now, counted on that clock.
    pub(crate) fn with_clock(remaining: Duration, clock: Clock) -> Self {
        Self {
            start: clock.now(),
            total: remaining,
            clock,
        }
    }

    /// The time left at creation.
    pub fn total(&self) -> Duration {
        self.total
    }

    /// The time left now; zero once passed.
    pub fn remaining(&self) -> Duration {
        let elapsed = self.clock.now().saturating_duration_since(self.start);
        self.total.saturating_sub(elapsed)
    }

    /// The share of the total still left, `0.0..=1.0`.
    fn fraction(&self, remaining: Duration) -> f64 {
        if self.total.is_zero() {
            return 0.0;
        }
        (remaining.as_secs_f64() / self.total.as_secs_f64()).clamp(0.0, 1.0)
    }
}

/// The monotonic clock a deadline reads: `web_time::Instant::now` by default,
/// or a function a test moves by hand. Equal only to its own clones.
#[derive(Clone, Default)]
pub(crate) struct Clock(Option<Arc<dyn Fn() -> Instant + Send + Sync>>);

impl Clock {
    fn now(&self) -> Instant {
        self.0.as_ref().map_or_else(Instant::now, |now| now())
    }
}

impl PartialEq for Clock {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (None, None) => true,
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl std::fmt::Debug for Clock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "Clock(manual)"
        } else {
            "Clock(monotonic)"
        })
    }
}

/// Whole seconds shown for `remaining`, rounded up so zero means passed.
fn display_secs(remaining: Duration) -> u64 {
    (remaining.as_millis() as u64).div_ceil(1000)
}

/// The countdown text for `secs` seconds left.
fn countdown_text(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s left")
    } else {
        format!("{}m {:02}s left", secs / 60, secs % 60)
    }
}

/// The countdown value read to assistive technology, in [`SPOKEN_STEP`]s.
fn spoken_remaining(remaining: Duration) -> String {
    let step = display_secs(remaining).div_ceil(SPOKEN_STEP) * SPOKEN_STEP;
    format!("about {step} seconds left")
}

/// What a kind is called to assistive technology.
fn kind_words(kind: ApprovalOptionKind) -> &'static str {
    match kind {
        ApprovalOptionKind::AllowOnce => "allow once",
        ApprovalOptionKind::AllowAlways => "allow always",
        ApprovalOptionKind::RejectOnce => "reject once",
        ApprovalOptionKind::RejectAlways => "reject always",
    }
}

fn is_always(kind: ApprovalOptionKind) -> bool {
    matches!(
        kind,
        ApprovalOptionKind::AllowAlways | ApprovalOptionKind::RejectAlways
    )
}

/// The display order: allow options, then reject options, each in the
/// given order.
fn display_order(options: &[ApprovalOption]) -> Vec<usize> {
    let allow = (0..options.len()).filter(|&i| options[i].kind.is_allow());
    let reject = (0..options.len()).filter(|&i| !options[i].kind.is_allow());
    allow.chain(reject).collect()
}

/// The mark and word of a resolved state (vouched-for glyphs or ASCII).
fn outcome_parts(state: ApprovalState) -> (&'static str, &'static str) {
    match state {
        ApprovalState::Allowed => ("\u{2713}", "Allowed"),
        ApprovalState::Denied => ("\u{00D7}", "Denied"),
        ApprovalState::AnsweredElsewhere => ("\u{2192}", "Answered elsewhere"),
        _ => ("!", "Expired"),
    }
}

/// The everything-but-children part of a view, compared as one value.
#[derive(Clone, Debug, PartialEq)]
struct Spec {
    options: Vec<ApprovalOption>,
    state: ApprovalState,
    deadline: Option<ApprovalDeadline>,
    selected: Option<String>,
    busy: bool,
    expired_hint: Option<String>,
}

impl Spec {
    /// The outcome sentence without its mark, e.g. "Allowed: Allow once".
    fn outcome(&self, state: ApprovalState) -> String {
        let word = outcome_parts(state).1;
        let chosen = self
            .selected
            .as_deref()
            .and_then(|id| self.options.iter().find(|o| o.id == id));
        match chosen {
            Some(o) => format!("{word}: {}", display_text(&o.label)),
            None => word.to_owned(),
        }
    }
}

/// A declarative approval card. See the [module docs](self).
pub struct ApprovalCardView<State: 'static> {
    title: String,
    detail: Option<String>,
    spec: Spec,
    on_select: Option<TypedArgCallback<State, String>>,
}

impl<State: 'static> std::fmt::Debug for ApprovalCardView<State> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovalCardView")
            .field("title", &self.title)
            .field("detail", &self.detail)
            .field("options", &self.spec.options)
            .field("state", &self.spec.state)
            .field("deadline", &self.spec.deadline)
            .field("selected", &self.spec.selected)
            .field("busy", &self.spec.busy)
            .finish_non_exhaustive()
    }
}

/// A card asking `title`, answered by one of `options` (shown allow kinds
/// first, each group in the given order). The title, detail and option
/// labels are agent-supplied and untrusted. The caller owns the state, busy
/// flag and deadline and passes them on every build.
pub fn approval_card<State: 'static>(
    title: impl Into<String>,
    options: impl Into<Vec<ApprovalOption>>,
) -> ApprovalCardView<State> {
    ApprovalCardView {
        title: title.into(),
        detail: None,
        spec: Spec {
            options: options.into(),
            state: ApprovalState::Pending,
            deadline: None,
            selected: None,
            busy: false,
            expired_hint: None,
        },
        on_select: None,
    }
}

impl<State: 'static> ApprovalCardView<State> {
    /// The command or path asked about, in a wrapping monospace block that
    /// scrolls past a few lines.
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Where the request stands (default [`ApprovalState::Pending`]).
    pub fn state(mut self, state: ApprovalState) -> Self {
        self.spec.state = state;
        self
    }

    /// Count down to `deadline` while pending. Pass the same stored value
    /// on every rebuild; a deadline made afresh each time never expires.
    pub fn expires(mut self, deadline: ApprovalDeadline) -> Self {
        self.spec.deadline = Some(deadline);
        self
    }

    /// Run `callback` with the app state and the chosen option's id.
    pub fn on_select(mut self, callback: impl Fn(&mut State, &str) + 'static) -> Self {
        let typed: TypedArgCallback<State, String> =
            Rc::new(move |state: &mut State, id: String| callback(state, &id));
        self.on_select = Some(typed);
        self
    }

    /// The id that was chosen, named in the resolved outcome.
    pub fn selected(mut self, id: impl Into<String>) -> Self {
        self.spec.selected = Some(id.into());
        self
    }

    /// An answer is being sent: options disabled, a loader shown.
    pub fn busy(mut self, busy: bool) -> Self {
        self.spec.busy = busy;
        self
    }

    /// The line under the expired outcome.
    pub fn expired_hint(mut self, hint: impl Into<String>) -> Self {
        self.spec.expired_hint = Some(hint.into());
        self
    }

    fn title_view(&self) -> TextView {
        text(display_text(&self.title))
            .size(TITLE_FONT_SIZE)
            .weight(FontWeight::SEMI_BOLD)
            .overflow_wrap(OverflowWrap::Anywhere)
    }
}

fn detail_view<State: 'static>(detail: &str) -> ScrollView<State> {
    scroll_view(
        text(display_text(detail))
            .size(style::CODE_FONT_SIZE)
            .themed_mono_family()
            .overflow_wrap(OverflowWrap::Anywhere),
    )
}

/// One laid-out option button.
struct OptionButton {
    /// Index into the options.
    option: usize,
    rect: Rect,
    label: TextLayout,
    tag: Option<TextLayout>,
}

/// Colours resolved once per paint.
#[derive(Clone, Copy)]
struct Palette {
    surface: Color,
    border: Color,
    ink: Color,
    accent: Color,
    on_accent: Color,
    quiet: Color,
    detail_bg: Color,
    track: Color,
    bar: Color,
}

impl Palette {
    fn resolve(theme: Option<&Theme>) -> Self {
        let s = style::scheme(theme);
        Self {
            surface: style::surface(theme),
            border: style::border(theme),
            ink: style::ink(theme),
            accent: s.primary_container,
            on_accent: s.on_primary_container,
            quiet: style::surface_high(theme),
            detail_bg: style::surface_high(theme),
            track: style::track(theme),
            bar: style::accent(theme),
        }
    }
}

/// The retained widget for an [`ApprovalCardView`].
pub struct ApprovalCardWidget {
    spec: Spec,
    title: String,
    on_select: Option<ErasedArgCallback<String>>,
    title_pod: ChildPod,
    detail: Option<ChildPod>,
    loader: ChildPod,
    detail_rect: Rect,
    count_label: Option<TextLayout>,
    count_at: Point,
    bar_rect: Rect,
    buttons: Vec<OptionButton>,
    outcome: Option<(TextLayout, Point)>,
    hint: Option<(TextLayout, Point)>,
    /// The note shown while the detail scrolls, and its size.
    overflow: Option<(TextLayout, Point)>,
    shown_secs: Option<u64>,
    /// A select fired; options stay off until state or busy changes.
    answered: bool,
    /// The deadline passed while the state was still pending.
    timed_out: bool,
    pressed: Option<usize>,
    captured: Option<usize>,
}

impl ApprovalCardWidget {
    /// The state shown: the caller's, or expired once the deadline passed.
    fn shown_state(&self) -> ApprovalState {
        if self.spec.state.is_pending() && self.timed_out {
            ApprovalState::Expired
        } else {
            self.spec.state
        }
    }

    /// Whether the options are off (faded, inert).
    fn options_off(&self) -> bool {
        self.spec.busy || self.answered || self.timed_out || !self.spec.state.is_pending()
    }

    fn interactive(&self) -> bool {
        !self.options_off() && self.on_select.is_some()
    }

    fn hit(&self, p: Point) -> Option<usize> {
        self.buttons.iter().position(|b| b.rect.contains(p))
    }

    /// Mark the deadline passed if it has; `true` when that is news.
    fn check_deadline(&mut self) -> bool {
        let passed = self.spec.state.is_pending()
            && !self.timed_out
            && self
                .spec
                .deadline
                .as_ref()
                .is_some_and(|d| d.remaining().is_zero());
        if passed {
            self.timed_out = true;
            self.pressed = None;
        }
        passed
    }

    fn fire(&mut self, ctx: &mut EventCtx, button: usize) {
        let id = self.spec.options[self.buttons[button].option].id.clone();
        self.answered = true;
        if let Some(cb) = self.on_select.as_mut() {
            cb(ctx, id);
        }
    }

    /// Lay out the countdown, the options and the loader below `y`.
    fn layout_pending(&mut self, ctx: &mut LayoutCtx, mut y: f64, inner: f64, ts: &Styles) -> f64 {
        if let Some(deadline) = &self.spec.deadline {
            let secs = display_secs(deadline.remaining());
            let label =
                ctx.text_context::<TextContext>()
                    .layout(&countdown_text(secs), &ts.muted, None);
            y += style::BLOCK_GAP;
            self.count_at = Point::new(style::PAD, y);
            y += label.size().height + style::ROW_GAP;
            self.bar_rect = Rect::new(style::PAD, y, style::PAD + inner, y + BAR_HEIGHT);
            y += BAR_HEIGHT;
            self.count_label = Some(label);
            self.shown_secs = Some(secs);
        }
        let mut last_allow = None;
        for i in display_order(&self.spec.options) {
            let option = &self.spec.options[i];
            let allow = option.kind.is_allow();
            y += match last_allow {
                None => style::BLOCK_GAP,
                Some(prev) if prev != allow => style::BLOCK_GAP * 2.0,
                Some(_) => style::ROW_GAP,
            };
            last_allow = Some(allow);
            let (label_style, tag_style) = if allow {
                (&ts.on_accent, &ts.on_accent_small)
            } else {
                (&ts.ink, &ts.ink_small)
            };
            let text = ctx.text_context::<TextContext>();
            let tag = is_always(option.kind).then(|| text.layout(ALWAYS_TAG, tag_style, None));
            let tag_w = tag
                .as_ref()
                .map_or(0.0, |t| t.size().width + style::PAD_SMALL);
            let label_w = (inner - BUTTON_PAD_X * 2.0 - tag_w).max(1.0);
            let label = text.layout_bounded(
                &display_text(&option.label),
                label_style,
                Some(label_w as f32),
                Some(LABEL_MAX_LINES),
                TextOverflow::Ellipsis,
            );
            let h = (label.size().height + BUTTON_PAD_Y * 2.0).max(MIN_TARGET);
            self.buttons.push(OptionButton {
                option: i,
                rect: Rect::new(style::PAD, y, style::PAD + inner, y + h),
                label,
                tag,
            });
            y += h;
        }
        if self.spec.busy {
            y += style::BLOCK_GAP;
            let got = self.loader.layout_child(
                ctx,
                &BoxConstraints::new(Size::ZERO, Size::new(inner, LOADER_ROW)),
            );
            self.loader.set_origin(Point::new(
                style::PAD + (inner - got.width) / 2.0,
                y + (LOADER_ROW - got.height) / 2.0,
            ));
            y += LOADER_ROW;
        }
        y
    }

    /// Lay out the outcome line, and the hint when expired, below `y`.
    fn layout_resolved(&mut self, ctx: &mut LayoutCtx, mut y: f64, inner: f64, ts: &Styles) -> f64 {
        let state = self.shown_state();
        let line = format!("{} {}", outcome_parts(state).0, self.spec.outcome(state));
        let tone = match state {
            ApprovalState::Allowed => &ts.success,
            ApprovalState::Denied => &ts.error,
            _ => &ts.quiet,
        };
        let text = ctx.text_context::<TextContext>();
        let outcome = text.layout_bounded(
            &line,
            tone,
            Some(inner as f32),
            Some(1),
            TextOverflow::Ellipsis,
        );
        y += style::BLOCK_GAP;
        let at = Point::new(style::PAD, y);
        y += outcome.size().height;
        self.outcome = Some((outcome, at));
        if state == ApprovalState::Expired
            && let Some(hint) = &self.spec.expired_hint
        {
            let hint = text.layout_bounded(
                hint,
                &ts.muted,
                Some(inner as f32),
                Some(HINT_MAX_LINES),
                TextOverflow::Ellipsis,
            );
            y += style::ROW_GAP;
            let at = Point::new(style::PAD, y);
            y += hint.size().height;
            self.hint = Some((hint, at));
        }
        y
    }

    fn paint_buttons(&self, scene: &mut dyn PaintScene, o: Point, pal: Palette) {
        let alpha = if self.options_off() {
            DISABLED_ALPHA
        } else {
            1.0
        };
        for (k, b) in self.buttons.iter().enumerate() {
            let allow = self.spec.options[b.option].kind.is_allow();
            let at = Point::new(o.x + b.rect.x0, o.y + b.rect.y0);
            let size = b.rect.size();
            let mut fill = if allow { pal.accent } else { pal.quiet };
            if self.pressed == Some(k) {
                fill = style::mix(
                    fill,
                    if allow { pal.on_accent } else { pal.ink },
                    style::PRESSED_TINT,
                );
            }
            scene.fill_rounded_rect(at, size, style::RADIUS_SMALL, style::fade(fill, alpha));
            if !allow {
                style::stroke_outline(
                    scene,
                    at,
                    size,
                    style::RADIUS_SMALL,
                    style::fade(pal.border, alpha),
                );
            }
            if alpha < 1.0 {
                scene.push_layer(at, size, alpha);
            }
            let label_y = at.y + (size.height - b.label.size().height) / 2.0;
            style::paint_layout(&b.label, Point::new(at.x + BUTTON_PAD_X, label_y), scene);
            if let Some(tag) = &b.tag {
                let t = tag.size();
                let x = at.x + size.width - BUTTON_PAD_X - t.width;
                style::paint_layout(
                    tag,
                    Point::new(x, at.y + (size.height - t.height) / 2.0),
                    scene,
                );
            }
            if alpha < 1.0 {
                scene.pop_layer();
            }
        }
    }

    fn paint_countdown(&self, scene: &mut dyn PaintScene, o: Point, pal: Palette) {
        let (Some(label), Some(deadline)) = (&self.count_label, &self.spec.deadline) else {
            return;
        };
        style::paint_layout(
            label,
            Point::new(o.x + self.count_at.x, o.y + self.count_at.y),
            scene,
        );
        let r = self.bar_rect;
        let at = Point::new(o.x + r.x0, o.y + r.y0);
        let radius = BAR_HEIGHT / 2.0;
        scene.fill_rounded_rect(at, r.size(), radius, pal.track);
        let w = r.width() * deadline.fraction(deadline.remaining());
        if w > 0.0 {
            scene.fill_rounded_rect(at, Size::new(w, BAR_HEIGHT), radius, pal.bar);
        }
    }

    /// Route an event the options did not take to the children.
    fn route(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        let mut result = EventResult::Ignored;
        if let Some(detail) = self.detail.as_mut() {
            result = route_event_single(detail, ctx, event);
        }
        for pod in [&mut self.title_pod, &mut self.loader] {
            if matches!(result, EventResult::Handled) && !event.is_broadcast() {
                break;
            }
            if matches!(route_event_single(pod, ctx, event), EventResult::Handled) {
                result = EventResult::Handled;
            }
        }
        result
    }
}

/// Text styles shaped in layout.
struct Styles {
    ink: TextStyle,
    ink_small: TextStyle,
    on_accent: TextStyle,
    on_accent_small: TextStyle,
    muted: TextStyle,
    quiet: TextStyle,
    success: TextStyle,
    error: TextStyle,
}

impl Styles {
    fn resolve(theme: Option<&Theme>) -> Self {
        let family = style::mono_family(theme);
        let pal = Palette::resolve(theme);
        let mk = |size: f32, color| {
            TextStyle::new(size, color)
                .with_family(family.clone())
                .with_overflow_wrap(OverflowWrap::Anywhere)
        };
        let small = style::LABEL_FONT_SIZE;
        Self {
            ink: mk(OPTION_FONT_SIZE, pal.ink),
            ink_small: mk(small, pal.ink),
            on_accent: mk(OPTION_FONT_SIZE, pal.on_accent),
            on_accent_small: mk(small, pal.on_accent),
            muted: mk(small, style::muted(theme)),
            quiet: mk(OPTION_FONT_SIZE, style::muted(theme)),
            success: mk(OPTION_FONT_SIZE, style::success(theme)),
            error: mk(OPTION_FONT_SIZE, style::error(theme)),
        }
    }
}

impl<State: 'static> View<State> for ApprovalCardView<State> {
    type Element = ApprovalCardWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> ApprovalCardWidget {
        ApprovalCardWidget {
            spec: self.spec.clone(),
            title: display_text(&self.title),
            on_select: self.on_select.as_ref().map(erase_callback_arg),
            title_pod: build_child::<State, _>(&self.title_view(), ctx),
            detail: self
                .detail
                .as_deref()
                .map(|d| build_child(&detail_view::<State>(d), ctx)),
            loader: build_child::<State, _>(&DotsLoaderView, ctx),
            detail_rect: Rect::ZERO,
            count_label: None,
            count_at: Point::ORIGIN,
            bar_rect: Rect::ZERO,
            buttons: Vec::new(),
            outcome: None,
            hint: None,
            overflow: None,
            shown_secs: None,
            answered: false,
            timed_out: false,
            pressed: None,
            captured: None,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut ApprovalCardWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = ChangeFlags::NONE;
        element.on_select = self.on_select.as_ref().map(erase_callback_arg);
        if prev.title != self.title {
            element.title = display_text(&self.title);
            flags |= rebuild_child::<State, _>(
                &prev.title_view(),
                &self.title_view(),
                &mut element.title_pod,
                ctx,
            );
        }
        match (prev.detail.as_deref(), self.detail.as_deref()) {
            (Some(old), Some(new)) if old != new => {
                if let Some(pod) = element.detail.as_mut() {
                    flags |= rebuild_child(
                        &detail_view::<State>(old),
                        &detail_view::<State>(new),
                        pod,
                        ctx,
                    );
                }
            }
            (Some(old), None) => {
                if let Some(mut pod) = element.detail.take() {
                    teardown_child(&detail_view::<State>(old), &mut pod, ctx);
                }
                flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
            }
            (None, Some(new)) => {
                element.detail = Some(build_child(&detail_view::<State>(new), ctx));
                flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
            }
            _ => {}
        }
        let (old, new) = (&prev.spec, &self.spec);
        let new_request = prev.title != self.title
            || prev.detail != self.detail
            || old.options != new.options
            || old.deadline != new.deadline;
        if new_request {
            element.answered = false;
            element.timed_out = false;
            element.pressed = None;
            element.captured = None;
            flags |= ChangeFlags::PAINT;
        }
        if old != new || prev.on_select.is_some() != self.on_select.is_some() {
            if old.state != new.state || old.busy != new.busy {
                element.answered = false;
            }
            if old.state != new.state {
                element.timed_out = false;
            }
            element.spec = new.clone();
            element.pressed = None;
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        flags
    }

    fn teardown(&self, element: &mut ApprovalCardWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child::<State, _>(&self.title_view(), &mut element.title_pod, ctx);
        if let (Some(d), Some(pod)) = (self.detail.as_deref(), element.detail.as_mut()) {
            teardown_child(&detail_view::<State>(d), pod, ctx);
        }
        teardown_child::<State, _>(&DotsLoaderView, &mut element.loader, ctx);
    }
}

impl Widget for ApprovalCardWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let width = style::width_of(bc);
        let inner = (width - style::PAD * 2.0).max(0.0);
        let (ts, line_h) = {
            let theme = Theme::from_layout_ctx(ctx);
            let family = style::mono_family(theme);
            let probe = TextStyle::new(style::CODE_FONT_SIZE, Palette::resolve(theme).ink)
                .with_family(family);
            (Styles::resolve(theme), probe)
        };
        let line_h = ctx
            .text_context::<TextContext>()
            .layout("0", &line_h, None)
            .size()
            .height;

        let mut y = style::PAD;
        let loose = |w: f64| BoxConstraints::new(Size::new(w, 0.0), Size::new(w, f64::INFINITY));
        let title = self.title_pod.layout_child(ctx, &loose(inner));
        self.title_pod.set_origin(Point::new(style::PAD, y));
        y += title.height;

        self.overflow = None;
        if let Some(detail) = self.detail.as_mut() {
            y += style::BLOCK_GAP;
            let text_w = (inner - style::PAD_SMALL * 2.0).max(0.0);
            let cap = line_h * DETAIL_MAX_LINES;
            let mut h = detail.layout_child(ctx, &loose(text_w)).height;
            let overflows = h > cap + 0.5;
            if overflows {
                h = detail
                    .layout_child(ctx, &BoxConstraints::tight(Size::new(text_w, cap)))
                    .height;
            }
            detail.set_origin(Point::new(
                style::PAD + style::PAD_SMALL,
                y + style::PAD_SMALL,
            ));
            self.detail_rect = Rect::new(
                style::PAD,
                y,
                style::PAD + inner,
                y + h + style::PAD_SMALL * 2.0,
            );
            y = self.detail_rect.y1;
            if overflows {
                let note = ctx.text_context::<TextContext>().layout_bounded(
                    OVERFLOW_NOTE,
                    &ts.muted,
                    Some(inner as f32),
                    Some(2),
                    TextOverflow::Ellipsis,
                );
                y += style::ROW_GAP;
                let at = Point::new(style::PAD, y);
                y += note.size().height;
                self.overflow = Some((note, at));
            }
        }

        self.check_deadline();
        self.buttons.clear();
        self.count_label = None;
        self.shown_secs = None;
        self.outcome = None;
        self.hint = None;
        y = if self.shown_state().is_pending() {
            self.layout_pending(ctx, y, inner, &ts)
        } else {
            self.layout_resolved(ctx, y, inner, &ts)
        };
        bc.constrain(Size::new(width, y + style::PAD))
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        if self.check_deadline() {
            ctx.request_layout();
        } else if self.shown_state().is_pending()
            && let Some(deadline) = &self.spec.deadline
        {
            if Some(display_secs(deadline.remaining())) != self.shown_secs {
                ctx.request_layout();
            } else {
                ctx.request_frame_paced();
            }
        }
        let pal = Palette::resolve(Theme::from_paint_ctx(ctx));
        let o = ctx.origin();
        let size = ctx.size();
        scene.fill_rounded_rect(o, size, style::RADIUS, pal.surface);
        self.title_pod.paint_child(ctx, scene);
        if let Some(detail) = self.detail.as_mut() {
            let r = self.detail_rect;
            let at = Point::new(o.x + r.x0, o.y + r.y0);
            scene.fill_rounded_rect(at, r.size(), style::RADIUS_SMALL, pal.detail_bg);
            detail.paint_child(ctx, scene);
        }
        self.paint_countdown(scene, o, pal);
        self.paint_buttons(scene, o, pal);
        if self.spec.busy && self.shown_state().is_pending() {
            self.loader.paint_child(ctx, scene);
        }
        for (layout, at) in [&self.outcome, &self.hint, &self.overflow]
            .into_iter()
            .flatten()
        {
            style::paint_layout(layout, Point::new(o.x + at.x, o.y + at.y), scene);
        }
        style::stroke_outline(scene, o, size, style::RADIUS, pal.border);
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        if self.check_deadline() {
            ctx.request_layout();
        }
        let InputEvent::Pointer(p) = event else {
            return self.route(ctx, event);
        };
        match p.phase {
            PointerPhase::Down if presses(p) && self.interactive() => {
                let Some(k) = self.hit(p.position) else {
                    return self.route(ctx, event);
                };
                self.captured = Some(k);
                self.pressed = Some(k);
                ctx.capture_pointer();
                ctx.request_redraw();
                EventResult::Handled
            }
            PointerPhase::Move if self.captured.is_some() => {
                let inside = self.hit(p.position).filter(|k| Some(*k) == self.captured);
                if inside != self.pressed {
                    self.pressed = inside;
                    ctx.request_redraw();
                }
                EventResult::Handled
            }
            PointerPhase::Up if self.captured.is_some() => {
                let captured = self.captured.take();
                self.pressed = None;
                if let Some(k) = captured
                    && self.hit(p.position) == Some(k)
                    && self.interactive()
                {
                    self.fire(ctx, k);
                }
                ctx.request_redraw();
                EventResult::Handled
            }
            PointerPhase::Cancel if self.captured.is_some() => {
                self.captured = None;
                self.pressed = None;
                ctx.request_redraw();
                EventResult::Handled
            }
            _ => self.route(ctx, event),
        }
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        let busy = self.spec.busy && self.shown_state().is_pending();
        ctx.push_container(
            Role::Group,
            |node| {
                node.set_label(self.title.as_str());
                if busy {
                    node.set_busy();
                }
            },
            |ctx| {
                self.title_pod.semantics_child(ctx);
                if let Some(detail) = &self.detail {
                    detail.semantics_child(ctx);
                }
                let origin = ctx.origin();
                let bounds = |r: Rect| style::node_bounds(origin, r);
                if let Some((note, at)) = &self.overflow {
                    let r = Rect::from_origin_size(*at, note.size());
                    ctx.push_node(Role::Label, |node| {
                        node.set_label(OVERFLOW_NOTE);
                        node.set_bounds(bounds(r));
                    });
                }
                let state = self.shown_state();
                if !state.is_pending() {
                    let mut text = self.spec.outcome(state);
                    if self.hint.is_some()
                        && let Some(hint) = &self.spec.expired_hint
                    {
                        text = format!("{text}. {hint}");
                    }
                    ctx.push_node(Role::Status, |node| {
                        node.set_label(text.as_str());
                        node.set_live(Live::Polite);
                    });
                    return;
                }
                if let Some(deadline) = &self.spec.deadline {
                    let value = spoken_remaining(deadline.remaining());
                    let r = Rect::new(
                        self.count_at.x,
                        self.count_at.y,
                        self.bar_rect.x1,
                        self.bar_rect.y1,
                    );
                    ctx.push_node(Role::Timer, |node| {
                        node.set_label(TIMER_LABEL);
                        node.set_value(value.as_str());
                        node.set_live(Live::Polite);
                        node.set_bounds(bounds(r));
                    });
                }
                let live = self.interactive();
                for b in &self.buttons {
                    let option = &self.spec.options[b.option];
                    ctx.push_node(Role::Button, |node| {
                        node.set_label(display_text(&option.label));
                        node.set_description(kind_words(option.kind));
                        node.set_bounds(bounds(b.rect));
                        if live {
                            node.add_action(Action::Click);
                        } else {
                            node.set_disabled();
                        }
                    });
                }
                self.loader.semantics_child(ctx);
            },
        );
    }

    frust::authoring::visit_children!(title_pod, detail, loader);
}

#[cfg(test)]
mod tests {
    use super::*;
    use frust::FrameTime;
    use frust::authoring::{AnyView, DiscardScene, PaintOutcome, PointerButton, PointerEvent, any};
    use frust_core::RenderRoot;
    use std::any::Any;

    const SECS_30: Duration = Duration::from_secs(30);

    /// A clock the tests move by hand; deadlines it makes count on it.
    #[derive(Clone)]
    struct TestClock(Arc<std::sync::Mutex<Instant>>);

    impl TestClock {
        fn new() -> Self {
            Self(Arc::new(std::sync::Mutex::new(Instant::now())))
        }

        fn advance(&self, by: Duration) {
            let mut now = self.0.lock().expect("test clock");
            *now += by;
        }

        fn deadline(&self, remaining: Duration) -> ApprovalDeadline {
            let cell = Arc::clone(&self.0);
            let clock = Clock(Some(Arc::new(move || *cell.lock().expect("test clock"))));
            ApprovalDeadline::with_clock(remaining, clock)
        }
    }

    #[test]
    fn the_real_clock_counts_down_and_a_zero_deadline_expires_the_card() {
        fn shared<T: Send + Sync>(_: &T) {}
        let d = ApprovalDeadline::after(SECS_30);
        shared(&d);
        let left = d.remaining();
        assert!(
            left <= SECS_30 && left > SECS_30 - Duration::from_secs(5),
            "{left:?}"
        );
        assert_eq!(d.clone(), d);
        assert_eq!(
            ApprovalDeadline::after(Duration::ZERO).remaining(),
            Duration::ZERO
        );
        let mut rig = Rig::new(true, 360.0);
        rig.show(card(options(2)).expires(ApprovalDeadline::after(Duration::ZERO)));
        rig.frame();
        let (label, _) = rig.node(Role::Status).expect("expired shown");
        assert_eq!(label, "Expired");
        assert!(rig.buttons().is_empty());
        assert!(!rig.frame().needs_frame, "no frames once expired");
    }

    #[derive(Default)]
    struct App {
        picked: Vec<String>,
    }

    /// Records every rounded-rect fill, to read the countdown bar.
    #[derive(Default)]
    struct Fills(Vec<(Size, Color)>);

    impl PaintScene for Fills {
        fn fill_rect(&mut self, _o: Point, _s: Size, _c: Color) {}
        fn draw_text(&mut self, _o: Point, _t: &str) {}
        fn fill_rounded_rect(&mut self, _o: Point, s: Size, _r: f64, c: Color) {
            self.0.push((s, c));
        }
    }

    struct Rig {
        root: RenderRoot<App, AnyView<App>>,
        app: App,
        text: TextContext,
        window: Size,
    }

    impl Rig {
        fn new(themed: bool, width: f64) -> Self {
            let mut root = RenderRoot::new();
            if themed {
                root.set_theme(Box::new(crate::baseline()));
            }
            Rig {
                root,
                app: App::default(),
                text: TextContext::new(),
                window: Size::new(width, 4000.0),
            }
        }

        fn show(&mut self, view: ApprovalCardView<App>) -> ChangeFlags {
            let mut view = Some(view);
            let flags = self.root.rebuild(
                &mut |_: &mut App| any(view.take().expect("built once per show")),
                &mut self.app,
            );
            self.layout();
            flags
        }

        fn layout(&mut self) {
            self.root
                .layout_with_text(self.window, &mut self.text as &mut dyn Any);
        }

        /// Paint, then lay out what the paint asked for.
        fn frame(&mut self) -> PaintOutcome {
            let out = self.root.paint(&mut DiscardScene, FrameTime::ZERO);
            self.layout();
            out
        }

        fn fills(&mut self) -> Vec<(Size, Color)> {
            let mut scene = Fills::default();
            self.root.paint(&mut scene, FrameTime::ZERO);
            scene.0
        }

        fn card(&self) -> Rect {
            self.root.inspect()[0].bounds
        }

        fn shaped(&self) -> u64 {
            let s = self.text.shape_cache_stats();
            s.shapes + s.line_breaks
        }

        /// The option buttons: `(label, kind words, bounds, disabled)`.
        fn buttons(&self) -> Vec<(String, String, Rect, bool)> {
            let update = self.root.semantics();
            update
                .nodes
                .iter()
                .filter(|(_, n)| n.role() == Role::Button)
                .map(|(_, n)| {
                    let b = n.bounds().expect("buttons have bounds");
                    (
                        n.label().unwrap_or_default().to_owned(),
                        n.description().unwrap_or_default().to_owned(),
                        Rect::new(b.x0, b.y0, b.x1, b.y1),
                        n.is_disabled(),
                    )
                })
                .collect()
        }

        fn node(&self, role: Role) -> Option<(String, String)> {
            let update = self.root.semantics();
            update
                .nodes
                .iter()
                .find(|(_, n)| n.role() == role)
                .map(|(_, n)| {
                    (
                        n.label().unwrap_or_default().to_owned(),
                        n.value().unwrap_or_default().to_owned(),
                    )
                })
        }

        fn pointer(&mut self, phase: PointerPhase, at: Point) {
            let e = InputEvent::Pointer(PointerEvent {
                phase,
                position: at,
                button: PointerButton::Primary,
            });
            self.root.event(&mut self.app, &e);
        }

        /// Every label and value the semantics tree carries, joined.
        fn all_text(&self) -> String {
            let update = self.root.semantics();
            let mut out = String::new();
            for (_, n) in &update.nodes {
                out += n.label().unwrap_or_default();
                out += "|";
                out += n.value().unwrap_or_default();
                out += "|";
            }
            out
        }

        fn has_label(&self, label: &str) -> bool {
            let update = self.root.semantics();
            update.nodes.iter().any(|(_, n)| n.label() == Some(label))
        }

        fn tap(&mut self, at: Point) {
            for phase in [PointerPhase::Down, PointerPhase::Up] {
                let e = InputEvent::Pointer(PointerEvent {
                    phase,
                    position: at,
                    button: PointerButton::Primary,
                });
                self.root.event(&mut self.app, &e);
            }
        }
    }

    fn opt(id: &str, kind: ApprovalOptionKind) -> ApprovalOption {
        ApprovalOption {
            id: id.into(),
            label: format!("Label {id}"),
            kind,
        }
    }

    fn options(n: usize) -> Vec<ApprovalOption> {
        use ApprovalOptionKind::*;
        let kinds = [AllowOnce, RejectOnce, AllowAlways, RejectAlways];
        (0..n)
            .map(|i| opt(&format!("o{i}"), kinds[i % 4]))
            .collect()
    }

    fn card(opts: Vec<ApprovalOption>) -> ApprovalCardView<App> {
        approval_card("Run this command?", opts)
            .on_select(|app: &mut App, id| app.picked.push(id.to_owned()))
    }

    /// The bar fill's share of the track, from one paint.
    fn bar_share(rig: &mut Rig) -> f64 {
        let bars: Vec<_> = rig
            .fills()
            .into_iter()
            .filter(|(s, _)| (s.height - BAR_HEIGHT).abs() < 1e-9)
            .collect();
        match bars.as_slice() {
            [_] => 0.0,
            [track, fill] => fill.0.width / track.0.width,
            other => panic!("{} bar fills", other.len()),
        }
    }

    #[test]
    fn one_two_and_six_options_fit_both_widths() {
        for width in [360.0, 1024.0] {
            for n in [1, 2, 6] {
                let mut rig = Rig::new(true, width);
                rig.show(card(options(n)).detail("rm -rf target"));
                assert!(rig.card().width() <= width + 0.01);
                let buttons = rig.buttons();
                assert_eq!(buttons.len(), n, "{n} options at {width}");
                for (label, _, r, disabled) in &buttons {
                    assert!(r.height() >= MIN_TARGET - 0.01, "{label} at {width}");
                    assert!(r.x0 >= 0.0 && r.x1 <= width + 0.01);
                    assert!((r.width() - (width - style::PAD * 2.0)).abs() < 0.5);
                    assert!(!disabled);
                }
            }
        }
    }

    #[test]
    fn allow_options_come_first_each_group_in_the_given_order() {
        use ApprovalOptionKind::*;
        let given = vec![
            opt("r1", RejectOnce),
            opt("a1", AllowOnce),
            opt("r2", RejectAlways),
            opt("a2", AllowAlways),
            opt("a3", AllowOnce),
        ];
        assert_eq!(display_order(&given), [1, 3, 4, 0, 2]);
        let mut rig = Rig::new(true, 360.0);
        rig.show(card(given));
        let buttons = rig.buttons();
        let labels: Vec<_> = buttons.iter().map(|b| b.0.as_str()).collect();
        assert_eq!(
            labels,
            ["Label a1", "Label a2", "Label a3", "Label r1", "Label r2"]
        );
        let kinds: Vec<_> = buttons.iter().map(|b| b.1.as_str()).collect();
        assert_eq!(
            kinds,
            [
                "allow once",
                "allow always",
                "allow once",
                "reject once",
                "reject always"
            ]
        );
        let gap = |a: usize, b: usize| buttons[b].2.y0 - buttons[a].2.y1;
        assert!(gap(0, 1) > 0.0);
        assert!(gap(2, 3) > gap(0, 1), "the groups sit apart");
    }

    #[test]
    fn long_labels_wrap_to_two_lines_then_elide() {
        let mut rig = Rig::new(true, 360.0);
        let with = |label: String| ApprovalOption {
            id: "x".into(),
            label,
            kind: ApprovalOptionKind::AllowOnce,
        };
        let height = |rig: &mut Rig, label: String| {
            rig.show(card(vec![with(label)]));
            rig.buttons()[0].2.height()
        };
        let short = height(&mut rig, "Allow".into());
        let medium = height(&mut rig, "allow ".repeat(40));
        let long = height(&mut rig, "allow ".repeat(400));
        assert!(medium > short);
        assert!((long - medium).abs() < 0.5, "capped at two lines");
        let unbroken = height(&mut rig, "x".repeat(800));
        assert!((unbroken - medium).abs() < 0.5);
    }

    #[test]
    fn always_kinds_carry_a_tag() {
        use ApprovalOptionKind::*;
        let mut counter = 0u64;
        let view = card(vec![
            opt("a", AllowOnce),
            opt("b", AllowAlways),
            opt("c", RejectOnce),
            opt("d", RejectAlways),
        ]);
        let mut w = View::<App>::build(&view, &mut BuildCtx::new(&mut counter));
        let mut text = TextContext::new();
        let mut ctx = LayoutCtx::with_text_context(&mut text);
        w.layout(&mut ctx, &BoxConstraints::loose(Size::new(360.0, 4000.0)));
        let tagged: Vec<_> = w
            .buttons
            .iter()
            .map(|b| (w.spec.options[b.option].id.as_str(), b.tag.is_some()))
            .collect();
        assert_eq!(
            tagged,
            [("a", false), ("b", true), ("c", false), ("d", true)]
        );
    }

    #[test]
    fn resolved_states_show_an_outcome_and_no_options() {
        let cases = [
            (ApprovalState::Allowed, Some("o0"), "Allowed: Label o0"),
            (ApprovalState::Denied, Some("o1"), "Denied: Label o1"),
            (ApprovalState::Denied, Some("unknown"), "Denied"),
            (ApprovalState::AnsweredElsewhere, None, "Answered elsewhere"),
            (ApprovalState::Expired, None, "Expired. Ask the agent again"),
        ];
        for (state, selected, want) in cases {
            let mut rig = Rig::new(true, 360.0);
            let mut view = card(options(4))
                .state(state)
                .expired_hint("Ask the agent again");
            if let Some(id) = selected {
                view = view.selected(id);
            }
            rig.show(view);
            assert!(rig.buttons().is_empty(), "{state:?}");
            assert!(rig.node(Role::Timer).is_none());
            let (label, _) = rig.node(Role::Status).expect("an outcome");
            assert_eq!(label, want);
            assert_eq!(outcome_parts(state).0.chars().count(), 1);
        }
        let marks: Vec<_> = [
            ApprovalState::Allowed,
            ApprovalState::Denied,
            ApprovalState::Expired,
            ApprovalState::AnsweredElsewhere,
        ]
        .map(|s| outcome_parts(s).0)
        .to_vec();
        for (i, a) in marks.iter().enumerate() {
            assert!(!marks[i + 1..].contains(a), "distinct marks");
        }
    }

    #[test]
    fn the_hint_shows_only_when_expired() {
        let mut a = Rig::new(true, 360.0);
        a.show(card(options(2)).state(ApprovalState::Expired));
        let mut b = Rig::new(true, 360.0);
        b.show(
            card(options(2))
                .state(ApprovalState::Expired)
                .expired_hint("Ask again"),
        );
        let mut c = Rig::new(true, 360.0);
        c.show(
            card(options(2))
                .state(ApprovalState::Denied)
                .expired_hint("Ask again"),
        );
        assert!(b.card().height() > a.card().height());
        assert!((c.card().height() - a.card().height()).abs() < 0.5);
    }

    #[test]
    fn countdown_text_and_spoken_value() {
        assert_eq!(display_secs(Duration::ZERO), 0);
        assert_eq!(display_secs(Duration::from_millis(1)), 1);
        assert_eq!(display_secs(Duration::from_millis(29_500)), 30);
        assert_eq!(countdown_text(30), "30s left");
        assert_eq!(countdown_text(125), "2m 05s left");
        let spoken = |ms| spoken_remaining(Duration::from_millis(ms));
        assert_eq!(spoken(30_000), "about 30 seconds left");
        assert_eq!(spoken(20_500), "about 30 seconds left");
        assert_eq!(spoken(19_900), "about 20 seconds left");
        assert_eq!(spoken(1), "about 10 seconds left");
    }

    #[test]
    fn the_countdown_counts_from_one_instant() {
        let clock = TestClock::new();
        let d = clock.deadline(SECS_30);
        assert_eq!(d.remaining(), SECS_30);
        clock.advance(Duration::from_millis(10_250));
        assert_eq!(d.remaining(), Duration::from_millis(19_750));
        assert_eq!(d.clone(), d, "a clone is equal");
        assert_ne!(clock.deadline(SECS_30), d, "a later deadline is not");
        clock.advance(SECS_30);
        assert_eq!(d.remaining(), Duration::ZERO);
        assert_eq!(d.fraction(d.remaining()), 0.0);
        let real = ApprovalDeadline::after(SECS_30);
        assert!(real.remaining() <= SECS_30 && real.total() == SECS_30);
    }

    #[test]
    fn countdown_text_and_bar_at_several_points() {
        let clock = TestClock::new();
        let d = clock.deadline(SECS_30);
        let mut counter = 0u64;
        let view = card(options(2)).expires(d.clone());
        let mut w = View::<App>::build(&view, &mut BuildCtx::new(&mut counter));
        let mut text = TextContext::new();
        let mut rig = Rig::new(true, 360.0);
        rig.show(card(options(2)).expires(d.clone()));
        let mut at = Duration::ZERO;
        for (to_ms, secs, share) in [
            (0, 30, 1.0),
            (500, 30, 29.5 / 30.0),
            (1_200, 29, 28.8 / 30.0),
            (15_000, 15, 0.5),
            (29_900, 1, 0.1 / 30.0),
        ] {
            let to = Duration::from_millis(to_ms);
            clock.advance(to - at);
            at = to;
            let mut ctx = LayoutCtx::with_text_context(&mut text);
            w.layout(&mut ctx, &BoxConstraints::loose(Size::new(360.0, 4000.0)));
            assert_eq!(w.shown_secs, Some(secs), "at {to_ms} ms");
            let got = bar_share(&mut rig);
            assert!((got - share).abs() < 1e-3, "bar {got} at {to_ms} ms");
        }
    }

    #[test]
    fn ticks_keep_the_height_and_shape_text_once_a_second() {
        let clock = TestClock::new();
        let mut rig = Rig::new(true, 360.0);
        rig.show(card(options(3)).expires(clock.deadline(SECS_30)));
        let height = rig.card().height();
        rig.frame();
        let mut last = rig.shaped();
        for ms in [100, 200, 300] {
            clock.advance(Duration::from_millis(ms));
            assert!(rig.frame().needs_frame);
            assert_eq!(rig.shaped(), last, "no text work inside a second");
        }
        for _ in 0..12 {
            clock.advance(Duration::from_millis(1_000));
            rig.frame();
            assert!((rig.card().height() - height).abs() < 0.01, "height kept");
            assert!(rig.shaped() > last, "a new second reshapes its label");
            last = rig.shaped();
        }
    }

    #[test]
    fn zero_disables_the_options_and_stops_frames() {
        let clock = TestClock::new();
        let mut rig = Rig::new(true, 360.0);
        rig.show(
            card(options(2))
                .expires(clock.deadline(SECS_30))
                .expired_hint("Too late"),
        );
        let target = rig.buttons()[0].2.center();
        clock.advance(SECS_30);
        rig.tap(target);
        assert!(rig.app.picked.is_empty(), "a passed deadline takes no tap");
        rig.frame();
        assert!(rig.buttons().is_empty());
        let (label, _) = rig.node(Role::Status).expect("expired shown");
        assert_eq!(label, "Expired. Too late");
        assert!(!rig.frame().needs_frame, "no frames after zero");
        rig.tap(target);
        assert!(rig.app.picked.is_empty());
    }

    #[test]
    fn a_double_tap_fires_once_until_state_or_busy_changes() {
        let mut rig = Rig::new(true, 360.0);
        rig.show(card(options(2)));
        let target = rig.buttons()[0].2.center();
        rig.tap(target);
        rig.tap(target);
        assert_eq!(rig.app.picked, ["o0"]);
        assert!(rig.buttons().iter().all(|b| b.3), "disabled after a tap");
        rig.show(card(options(2)));
        rig.tap(target);
        assert_eq!(rig.app.picked, ["o0"], "an equal view keeps them off");
        rig.show(card(options(2)).busy(true));
        rig.tap(target);
        assert_eq!(rig.app.picked, ["o0"], "busy keeps them off");
        rig.show(card(options(2)));
        rig.tap(target);
        assert_eq!(rig.app.picked, ["o0", "o0"]);
    }

    #[test]
    fn busy_disables_options_and_marks_the_group() {
        let mut rig = Rig::new(true, 360.0);
        rig.show(card(options(2)));
        let idle = rig.card().height();
        rig.show(card(options(2)).busy(true));
        assert!(rig.card().height() > idle, "a loader row shows");
        assert!(rig.buttons().iter().all(|b| b.3));
        let update = rig.root.semantics();
        let group = update
            .nodes
            .iter()
            .find(|(_, n)| n.role() == Role::Group)
            .map(|(_, n)| n)
            .expect("a group");
        assert_eq!(group.label(), Some("Run this command?"));
        assert!(group.is_busy());
        let target = rig.buttons()[0].2.center();
        rig.tap(target);
        assert!(rig.app.picked.is_empty());
    }

    #[test]
    fn the_timer_is_a_live_value_in_ten_second_steps() {
        let clock = TestClock::new();
        let mut rig = Rig::new(true, 360.0);
        rig.show(card(options(1)).expires(clock.deadline(SECS_30)));
        let value = |rig: &Rig| rig.node(Role::Timer).expect("a timer").1;
        assert_eq!(value(&rig), "about 30 seconds left");
        clock.advance(Duration::from_secs(9));
        rig.frame();
        assert_eq!(value(&rig), "about 30 seconds left");
        clock.advance(Duration::from_secs(2));
        rig.frame();
        assert_eq!(value(&rig), "about 20 seconds left");
        let update = rig.root.semantics();
        let timer = update
            .nodes
            .iter()
            .find(|(_, n)| n.role() == Role::Timer)
            .map(|(_, n)| n)
            .expect("a timer");
        assert_eq!(timer.label(), Some(TIMER_LABEL));
        assert_eq!(timer.live(), Some(Live::Polite));
    }

    #[test]
    fn equal_views_skip_the_rebuild_and_keep_the_deadline() {
        let clock = TestClock::new();
        let d = clock.deadline(SECS_30);
        let view = || card(options(6)).detail("ls -la").expires(d.clone());
        let mut rig = Rig::new(true, 360.0);
        rig.show(view());
        let before = rig.shaped();
        assert_eq!(rig.show(view()), ChangeFlags::NONE);
        assert_eq!(rig.shaped(), before);
        clock.advance(Duration::from_secs(10));
        assert_eq!(rig.show(view()), ChangeFlags::NONE, "an equal deadline");
        let share = bar_share(&mut rig);
        assert!((share - 2.0 / 3.0).abs() < 1e-3, "did not restart: {share}");
        let fresh = card(options(6))
            .detail("ls -la")
            .expires(clock.deadline(SECS_30));
        assert!(rig.show(fresh).contains(ChangeFlags::LAYOUT));
        assert!((bar_share(&mut rig) - 1.0).abs() < 1e-3, "a new deadline");
    }

    #[test]
    fn frames_are_requested_only_while_pending_with_a_deadline() {
        let clock = TestClock::new();
        let mut rig = Rig::new(true, 360.0);
        rig.show(card(options(2)));
        assert!(!rig.frame().needs_frame, "no deadline, no frames");
        rig.show(card(options(2)).expires(clock.deadline(SECS_30)));
        assert!(rig.frame().needs_frame);
        for state in [
            ApprovalState::Allowed,
            ApprovalState::Denied,
            ApprovalState::Expired,
            ApprovalState::AnsweredElsewhere,
        ] {
            let mut rig = Rig::new(true, 360.0);
            rig.show(
                card(options(2))
                    .state(state)
                    .expires(clock.deadline(SECS_30)),
            );
            assert!(!rig.frame().needs_frame, "{state:?}");
        }
    }

    #[test]
    fn the_detail_wraps_then_scrolls_past_its_line_cap() {
        let mut rig = Rig::new(true, 360.0);
        let height = |rig: &mut Rig, detail: String| {
            rig.show(card(options(1)).detail(detail));
            rig.card().height()
        };
        let none = {
            rig.show(card(options(1)));
            rig.card().height()
        };
        let one = height(&mut rig, "cargo test".into());
        let three = height(&mut rig, "line\nline\nline".into());
        let twenty = height(&mut rig, "line\n".repeat(20));
        let hundred = height(&mut rig, "line\n".repeat(100));
        assert!(none < one && one < three && three < twenty);
        assert!((twenty - hundred).abs() < 0.5, "capped");
        height(&mut rig, "/".repeat(2000));
        for n in rig.root.inspect() {
            assert!(n.bounds.x1 <= 360.0 + 0.01, "{}", n.type_name);
        }
    }

    #[test]
    fn themed_and_unthemed_paint_do_not_panic() {
        let clock = TestClock::new();
        for themed in [true, false] {
            for busy in [false, true] {
                let mut rig = Rig::new(themed, 360.0);
                rig.show(
                    card(options(6))
                        .detail("git push --force")
                        .busy(busy)
                        .expires(clock.deadline(SECS_30)),
                );
                rig.frame();
                rig.fills();
            }
            let mut rig = Rig::new(themed, 360.0);
            rig.show(
                card(options(2))
                    .state(ApprovalState::Expired)
                    .expired_hint("x"),
            );
            rig.frame();
        }
    }

    fn lines(n: usize) -> String {
        vec!["line"; n].join("\n")
    }

    /// Answer request A, then show `next`; the new request's first option
    /// must be live and a tap must fire its id.
    fn answered_then(next: ApprovalCardView<App>, want: &str) {
        let mut rig = Rig::new(true, 360.0);
        rig.show(card(options(2)).detail("a"));
        rig.tap(rig.buttons()[0].2.center());
        assert_eq!(rig.app.picked, ["o0"]);
        rig.show(next);
        let buttons = rig.buttons();
        assert!(buttons.iter().all(|b| !b.3), "the new request is live");
        rig.tap(buttons[0].2.center());
        assert_eq!(rig.app.picked, ["o0", want]);
    }

    fn fresh_options() -> Vec<ApprovalOption> {
        vec![
            opt("n0", ApprovalOptionKind::AllowOnce),
            opt("n1", ApprovalOptionKind::RejectOnce),
        ]
    }

    #[test]
    fn a_new_request_after_an_answered_one_is_live() {
        answered_then(card(fresh_options()).detail("a"), "n0");
        answered_then(
            approval_card("Another question?", options(2))
                .detail("a")
                .on_select(|app: &mut App, id| app.picked.push(id.to_owned())),
            "o0",
        );
        answered_then(card(options(2)).detail("b"), "o0");
        let clock = TestClock::new();
        answered_then(
            card(options(2))
                .detail("a")
                .expires(clock.deadline(SECS_30)),
            "o0",
        );
    }

    #[test]
    fn a_new_request_after_an_answered_one_offers_click() {
        let mut rig = Rig::new(true, 360.0);
        rig.show(card(options(2)));
        rig.tap(rig.buttons()[0].2.center());
        rig.show(card(fresh_options()));
        let update = rig.root.semantics();
        let clicks = update
            .nodes
            .iter()
            .filter(|(_, n)| n.role() == Role::Button)
            .filter(|(_, n)| n.supports_action(Action::Click))
            .count();
        assert_eq!(clicks, 2);
    }

    #[test]
    fn a_press_does_not_survive_a_different_request() {
        let mut rig = Rig::new(true, 360.0);
        rig.show(card(options(2)));
        let at = rig.buttons()[0].2.center();
        rig.pointer(PointerPhase::Down, at);
        rig.show(card(fresh_options()));
        rig.pointer(PointerPhase::Up, rig.buttons()[0].2.center());
        assert!(rig.app.picked.is_empty(), "nothing fires");
    }

    #[test]
    fn a_press_survives_a_rebuild_with_an_equal_request() {
        let mut rig = Rig::new(true, 360.0);
        rig.show(card(options(2)));
        let at = rig.buttons()[0].2.center();
        rig.pointer(PointerPhase::Down, at);
        rig.show(card(options(2)));
        rig.pointer(PointerPhase::Up, at);
        assert_eq!(rig.app.picked, ["o0"]);
    }

    #[test]
    fn a_detail_past_the_cap_shows_the_overflow_note() {
        let mut heights = Vec::new();
        for (n, expect) in [(1, false), (6, false), (7, true), (40, true)] {
            let mut rig = Rig::new(true, 360.0);
            rig.show(card(options(1)).detail(lines(n)));
            assert_eq!(rig.has_label(OVERFLOW_NOTE), expect, "{n} lines");
            heights.push(rig.card().height());
        }
        assert!(heights[2] > heights[1], "the note takes room");
        assert!((heights[2] - heights[3]).abs() < 0.5, "a constant note");
        let mut rig = Rig::new(true, 360.0);
        rig.show(card(options(1)));
        assert!(!rig.has_label(OVERFLOW_NOTE));
    }

    #[test]
    fn displayed_text_replaces_hidden_characters_with_a_question_mark() {
        let raw = "x\u{202E}a\u{2066}b\u{200B}c\u{0007}d\u{0085}e\u{FEFF}f";
        assert_eq!(display_text(raw), "x?a?b?c?d?e?f");
        assert_eq!(display_text("a\tb\nc"), "a\tb\nc");
        let id = "id\u{202E}1";
        let mut rig = Rig::new(true, 360.0);
        rig.show(
            approval_card(
                format!("T{raw}"),
                vec![ApprovalOption {
                    id: id.into(),
                    label: format!("L{raw}"),
                    kind: ApprovalOptionKind::AllowOnce,
                }],
            )
            .detail(format!("D\ta\n{raw}"))
            .on_select(|app: &mut App, id| app.picked.push(id.to_owned())),
        );
        let text = rig.all_text();
        for c in ['\u{202E}', '\u{2066}', '\u{200B}', '\u{0007}', '\u{0085}'] {
            assert!(!text.contains(c), "{c:?} shown");
        }
        assert!(rig.has_label("Tx?a?b?c?d?e?f"));
        assert!(rig.has_label("Lx?a?b?c?d?e?f"));
        rig.tap(rig.buttons()[0].2.center());
        assert_eq!(rig.app.picked, [id]);
    }

    #[test]
    fn a_deadline_made_afresh_on_every_rebuild_restarts_the_countdown() {
        let clock = TestClock::new();
        let mut rig = Rig::new(true, 360.0);
        rig.show(card(options(2)).expires(clock.deadline(SECS_30)));
        clock.advance(Duration::from_secs(10));
        let stored = clock.deadline(SECS_30);
        clock.advance(Duration::from_secs(10));
        rig.show(card(options(2)).expires(stored));
        let kept = bar_share(&mut rig);
        clock.advance(Duration::from_secs(5));
        rig.show(card(options(2)).expires(clock.deadline(SECS_30)));
        assert!(kept < 1.0, "a stored deadline keeps counting");
        assert!((bar_share(&mut rig) - 1.0).abs() < 1e-3, "restarted");
        let first = clock.deadline(SECS_30);
        clock.advance(Duration::from_millis(1));
        assert_ne!(
            clock.deadline(SECS_30),
            first,
            "a fresh deadline is unequal"
        );
    }
}
