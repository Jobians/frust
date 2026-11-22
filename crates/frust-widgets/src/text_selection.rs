//! What the two text widgets that hold a selection share: the editable
//! [`TextInput`](crate::TextInputView) and a
//! [`selectable`](crate::RichTextView::selectable) `rich_text` block.
//!
//! - the selection highlight's colour ([`selection_color`]) and the stable
//!   accessibility ids of the clipboard verbs ([`A11Y_COPY_ID`] and its
//!   neighbours);
//! - the floated toolbar's slot configuration ([`new_toolbar_slot`]);
//! - the clipboard chord decoding ([`chord_verb`], and [`clipboard_verb`]
//!   for a block with no editing keys of its own);
//! - the drag handles at a selection's two ends: [`paint_handle`] and
//!   [`handle_hit`];
//! - the paint-clock long-press timer a block arms ([`HoldTimer`]).
//!
//! # Handles
//!
//! A handle is a stem along the caret at a selection end plus a round knob
//! hanging below the line, Android's shape: the knob is what a finger grabs,
//! and it sits below the glyphs so the finger does not cover the end it
//! positions. Its hit area is wider than what it paints ([`HANDLE_HIT`]
//! around the knob, and the stem's height), because a fingertip is. A knob
//! hangs below its line, so on the block's last line it lies outside the
//! block's box: the part of its hit area inside the box is what a press can
//! reach, since presses are hit-tested against the box.

use frust_core::{FrameTime, PaintCtx};
use frust_core::{
    Key, Modifiers, NamedKey, OutsideTap, OverlayBand, OverlayInput, PaintScene, TOUCH_SLOP,
};
use frust_theme::Theme;
use kurbo::{Point, Rect, Size};
use peniko::Color;

use crate::gesture::LONG_PRESS_MS;
use crate::overlay::{OverlayAlign, OverlayPlacement, OverlaySide, OverlaySlot};

/// Selection highlight color (unthemed fallback; themed from `primary` at alpha).
pub(crate) const SELECTION: Color = Color::from_rgb8(0xBF, 0xDB, 0xFE);
/// Alpha applied to `primary` for the themed selection highlight (a v1
/// simplification — a translucent primary stands in for a dedicated selection
/// role).
pub(crate) const SELECTION_ALPHA: f32 = 0.30;

/// The selection highlight of an enabled widget: the theme's `primary` at
/// [`SELECTION_ALPHA`], or [`SELECTION`] with no theme — `TextInput`'s
/// `Chrome::resolve` rule for its selection colour.
pub(crate) fn selection_color(theme: Option<&Theme>) -> Color {
    theme.map_or(SELECTION, |t| {
        t.scheme().primary.with_alpha(SELECTION_ALPHA)
    })
}

/// Gap between the selection's bounding box and the floated toolbar, in logical
/// px — wider than [`crate::DEFAULT_OFFSET`]'s neutral 4px because this anchor
/// is a run of text rather than a widget's own edge, and a bar sitting 4px off
/// a line of glyphs reads as touching it.
pub(crate) const TOOLBAR_GAP: f64 = 8.0;

/// A closed portal slot configured the way a selection toolbar wants it.
///
/// `Floating` (a toolbar is not a tooltip: it is the thing the user aims at),
/// `Interactive` (its whole purpose is being tapped), and
/// [`OutsideTap::Notify`] **without** consuming — a press elsewhere dismisses
/// the bar *and* still lands, so the tap that puts it away also moves the caret
/// where the user pointed. Placement is above the selection, centred, at
/// [`TOOLBAR_GAP`], flipping below and clamping inside the window when there is
/// no room (the [`OverlayPlacement`] defaults for both).
pub(crate) fn new_toolbar_slot() -> OverlaySlot<()> {
    let mut slot = OverlaySlot::new();
    slot.set_band(OverlayBand::Floating);
    slot.set_input(OverlayInput::Interactive);
    slot.set_outside_tap(OutsideTap::Notify { consume: false });
    slot.set_placement(
        OverlayPlacement::on(OverlaySide::Top)
            .align(OverlayAlign::Center)
            .offset(TOOLBAR_GAP),
    );
    slot
}

/// The stable accesskit custom-action id for **Cut** — and, with its three
/// siblings below, the whole id space a text widget's clipboard verbs are
/// published under (see `TextInput`'s module docs on the clipboard verbs and
/// assistive technology).
///
/// **Stable is the whole point.** An assistive-technology client holds on to
/// the id it was offered and sends that number back when the user picks the
/// action, so renumbering these would silently re-point a remembered "Copy" at
/// some other verb. They are ordinary small integers because that is what
/// [`accesskit::ActionData::CustomAction`] carries.
pub(crate) const A11Y_CUT_ID: i32 = 1;
/// See [`A11Y_CUT_ID`].
pub(crate) const A11Y_COPY_ID: i32 = 2;
/// See [`A11Y_CUT_ID`].
pub(crate) const A11Y_PASTE_ID: i32 = 3;
/// See [`A11Y_CUT_ID`].
pub(crate) const A11Y_SELECT_ALL_ID: i32 = 4;

/// A clipboard or selection verb a key decodes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClipboardVerb {
    Copy,
    Cut,
    Paste,
    SelectAll,
}

/// The verb a chorded character names — `c`/`x`/`v`/`a`, ASCII
/// case-insensitive — for a key already held with `ctrl` **or** `meta`.
///
/// Chord decoding is **platform-uniform**: ctrl OR meta arms the clipboard
/// verbs on every OS, so a Mac keyboard's Cmd+C works under Linux and a
/// terminal habit's Ctrl+C works on macOS. Flutter is platform-strict instead
/// (meta on macOS/iOS, ctrl elsewhere); a shell that wants exactly that decodes
/// the chord itself and dispatches an `EditCommand` — the route the platform
/// edit menus and the hardware clipboard keys already take, and the one that
/// always wins, since this decode only sees what a shell chose to forward as a
/// key. Alt is deliberately not a chord modifier: it composes characters.
/// Every other chorded character is `None`: never literal text, and left for
/// the root's shortcut registry.
pub(crate) fn chord_verb(character: &str) -> Option<ClipboardVerb> {
    [
        ("c", ClipboardVerb::Copy),
        ("x", ClipboardVerb::Cut),
        ("v", ClipboardVerb::Paste),
        ("a", ClipboardVerb::SelectAll),
    ]
    .into_iter()
    .find_map(|(letter, verb)| character.eq_ignore_ascii_case(letter).then_some(verb))
}

/// The verb `key` held with `modifiers` names, for a widget that takes no
/// other key: a [`chord_verb`] under `ctrl` or `meta`, the dedicated
/// [`NamedKey::Copy`]/[`Cut`](NamedKey::Cut)/[`Paste`](NamedKey::Paste) keys,
/// and the legacy spellings `TextInput` answers — `Ctrl+Insert` copies,
/// `Shift+Insert` pastes (shift wins when both are held), and `Shift+Delete`
/// alone cuts.
pub(crate) fn clipboard_verb(key: &Key, modifiers: Modifiers) -> Option<ClipboardVerb> {
    match key {
        Key::Character(s) if modifiers.ctrl || modifiers.meta => chord_verb(s),
        Key::Character(_) => None,
        Key::Named(NamedKey::Copy) => Some(ClipboardVerb::Copy),
        Key::Named(NamedKey::Cut) => Some(ClipboardVerb::Cut),
        Key::Named(NamedKey::Paste) => Some(ClipboardVerb::Paste),
        Key::Named(NamedKey::Insert) if modifiers.shift => Some(ClipboardVerb::Paste),
        Key::Named(NamedKey::Insert) if modifiers.ctrl => Some(ClipboardVerb::Copy),
        Key::Named(NamedKey::Delete) if modifiers.shift && !modifiers.ctrl && !modifiers.meta => {
            Some(ClipboardVerb::Cut)
        }
        Key::Named(_) => None,
    }
}

/// The knob's radius, in logical px.
pub(crate) const HANDLE_RADIUS: f64 = 6.0;
/// The stem's width, in logical px.
const HANDLE_STEM: f64 = 2.0;
/// How far from the knob's centre a press still grabs the handle: a
/// fingertip, wider than the knob it paints.
pub(crate) const HANDLE_HIT: f64 = TOUCH_SLOP + 4.0;
/// Handle colour with no theme: `TextInput`'s unthemed caret colour.
const HANDLE: Color = Color::from_rgb8(0x1D, 0x4E, 0xD8);

/// The knob's centre for the handle at a selection end whose caret rect is
/// `caret` (layout or widget space, as `caret` is).
pub(crate) fn handle_knob(caret: Rect) -> Point {
    Point::new(caret.x0, caret.y1 + HANDLE_RADIUS)
}

/// The handles' colour: the theme's `primary`, or [`HANDLE`] with no theme.
pub(crate) fn handle_color(theme: Option<&Theme>) -> Color {
    theme.map_or(HANDLE, |t| t.scheme().primary)
}

/// Paint the handle at the selection end whose caret rect is `caret`
/// (offset by `origin`) in `color` ([`handle_color`]).
pub(crate) fn paint_handle(scene: &mut dyn PaintScene, origin: Point, caret: Rect, color: Color) {
    let caret = caret + origin.to_vec2();
    scene.fill_rect(
        Point::new(caret.x0 - HANDLE_STEM / 2.0, caret.y0),
        Size::new(HANDLE_STEM, caret.height()),
        color,
    );
    let knob = handle_knob(caret);
    scene.fill_rounded_rect(
        Point::new(knob.x - HANDLE_RADIUS, knob.y - HANDLE_RADIUS),
        Size::new(2.0 * HANDLE_RADIUS, 2.0 * HANDLE_RADIUS),
        HANDLE_RADIUS,
        color,
    );
}

/// Whether a press at `at` grabs the handle whose caret rect is `caret`
/// (both in one space): within [`HANDLE_HIT`] of the knob, or beside the
/// stem by as much. The distance to the knob ranks two candidate handles.
pub(crate) fn handle_hit(caret: Rect, at: Point) -> Option<f64> {
    let knob = (at - handle_knob(caret)).hypot();
    let stem = Rect::new(
        caret.x0 - HANDLE_HIT / 2.0,
        caret.y0,
        caret.x0 + HANDLE_HIT / 2.0,
        caret.y1,
    );
    (knob <= HANDLE_HIT || stem.contains(at)).then_some(knob)
}

/// A long-press timer measured across paints, as `TextInput`'s and
/// [`crate::gesture`]'s are: only the paint pass carries a clock. The first
/// paint after the press seeds the epoch; the paint that sees
/// [`LONG_PRESS_MS`] elapse marks it and latches the deferred-callback flush,
/// so the next pass carrying an `EventCtx` (the `Housekeeping` broadcast, or
/// an in-slop `Move`/`Up`) fires it.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct HoldTimer {
    started_at: Option<FrameTime>,
    elapsed: bool,
}

impl HoldTimer {
    /// Advance the timer on a paint at `now`. Requests a plain continuation
    /// frame until the threshold is crossed (on the crossing frame too, which
    /// is what reaches the flush on a dirty-driven shell): a long-press fires
    /// at its threshold in wall time, under a frame gate or `reduce_motion`
    /// alike.
    pub(crate) fn tick(&mut self, ctx: &mut PaintCtx, now: FrameTime) {
        let start = *self.started_at.get_or_insert(now);
        if !self.elapsed {
            if now.saturating_sub(start).as_secs_f64() * 1000.0 >= LONG_PRESS_MS {
                self.elapsed = true;
                frust_core::mark_pending_result_flush();
            }
            ctx.request_frame();
        }
    }

    /// Whether a paint saw the threshold crossed.
    pub(crate) fn elapsed(&self) -> bool {
        self.elapsed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(ctrl: bool, shift: bool, meta: bool) -> Modifiers {
        Modifiers {
            ctrl,
            shift,
            meta,
            ..Modifiers::default()
        }
    }

    #[test]
    fn chords_decode_case_insensitively_under_ctrl_or_meta_only() {
        let c = Key::Character("C".into());
        assert_eq!(chord_verb("c"), Some(ClipboardVerb::Copy));
        assert_eq!(chord_verb("A"), Some(ClipboardVerb::SelectAll));
        assert_eq!(chord_verb("z"), None);
        assert_eq!(
            clipboard_verb(&c, mods(true, false, false)),
            Some(ClipboardVerb::Copy)
        );
        assert_eq!(
            clipboard_verb(&c, mods(false, false, true)),
            Some(ClipboardVerb::Copy)
        );
        assert_eq!(clipboard_verb(&c, mods(false, true, false)), None);
    }

    #[test]
    fn the_legacy_keys_decode_like_the_field_answers_them() {
        let insert = Key::Named(NamedKey::Insert);
        let delete = Key::Named(NamedKey::Delete);
        assert_eq!(
            clipboard_verb(&insert, mods(true, false, false)),
            Some(ClipboardVerb::Copy)
        );
        assert_eq!(
            clipboard_verb(&insert, mods(true, true, false)),
            Some(ClipboardVerb::Paste)
        );
        assert_eq!(clipboard_verb(&insert, Modifiers::default()), None);
        assert_eq!(
            clipboard_verb(&delete, mods(false, true, false)),
            Some(ClipboardVerb::Cut)
        );
        assert_eq!(clipboard_verb(&delete, mods(true, true, false)), None);
        assert_eq!(
            clipboard_verb(&Key::Named(NamedKey::Copy), Modifiers::default()),
            Some(ClipboardVerb::Copy)
        );
    }

    #[test]
    fn a_handle_grabs_around_its_knob_and_beside_its_stem() {
        let caret = Rect::new(50.0, 10.0, 50.0, 30.0);
        let knob = handle_knob(caret);
        assert_eq!(knob, Point::new(50.0, 30.0 + HANDLE_RADIUS));
        assert_eq!(handle_hit(caret, knob), Some(0.0));
        assert!(handle_hit(caret, Point::new(50.0 + HANDLE_HIT - 1.0, knob.y)).is_some());
        assert!(
            handle_hit(caret, Point::new(52.0, 12.0)).is_some(),
            "the stem"
        );
        assert!(handle_hit(caret, Point::new(50.0 + HANDLE_HIT + 1.0, knob.y)).is_none());
        assert!(handle_hit(caret, Point::new(80.0, 12.0)).is_none());
    }

    #[test]
    fn the_selection_colour_follows_the_theme() {
        assert_eq!(selection_color(None), SELECTION);
        let theme = Theme::neutral();
        assert_eq!(
            selection_color(Some(&theme)),
            theme.scheme().primary.with_alpha(SELECTION_ALPHA)
        );
    }
}
