//! Avatars: a circular [`avatar`] showing a person's initials or an image.
//!
//! The initials are upper-cased and cut to two grapheme clusters. The circle's
//! fill is picked deterministically from the initials (so two people differ and
//! one person is stable across runs) or pinned to the muted neutral with
//! [`AvatarTone::Muted`]. An [`image`](AvatarView::image) replaces the initials
//! and is clipped to the circle. A [`label`](AvatarView::label) is published as
//! a [`Role::Image`] semantics node; without one the initials are the name.
//!
//! # Sizes and the text scale
//!
//! [`AvatarSize::Md`] is 28 px, the edge of the chat message row's avatar, and
//! its initials use the same `text-xs font-medium` style on the same type-scale
//! role. Under [`Density::Comfortable`](crate::tokens::Density::Comfortable)
//! with `.tone(AvatarTone::Muted)` the part therefore paints exactly what the
//! message row paints. Initials inside a fixed circle are fixed-geometry text;
//! this part does **not** take the opt-out. The initials follow the OS text
//! scale and the circle's edge is multiplied by the same factor, so the glyphs
//! never outgrow the circle and the proportions hold. At scale `1.0` (every
//! theme without a text scale) the edge is the plain base size. The edge does
//! not follow the density: an avatar is not a touch target.
//!
//! # Differences from the web part
//!
//! - **Two-cluster cut is an approximation.** The cut keeps a base character
//!   with its combining marks, variation selectors, skin-tone modifiers and
//!   zero-width-joiner sequences, and pairs regional indicators. It is not a
//!   full Unicode segmentation, which beUI has no dependency for.
//! - **The default fill is hashed**, not `bg-muted`; the muted look is an
//!   option so a row that must match the message avatar can ask for it.

use frust::Theme;
use frust::authoring::text::{FontWeight, TextStyle};
use frust::authoring::{AnyView, Color};
use frust::authoring::{
    BoxConstraints, BuildCtx, ChangeFlags, ChildPod, LayoutCtx, PaintCtx, PaintScene, Point, Role,
    SemanticsCtx, Size, View, Widget, any, build_child, rebuild_child, teardown_child,
};
use kurbo::Vec2;
use std::marker::PhantomData;

use crate::text::{LabelRun, ThemeTextType};
use crate::tokens::{BEUI_LIGHT, sans_family};

/// The type-scale role the initials take their family from at layout (the
/// message row's avatar uses the same one).
const AVATAR_ROLE: ThemeTextType = ThemeTextType::LabelMedium;

/// How many fill pairs [`AvatarTone::Auto`] picks between.
const TONE_SLOTS: usize = 3;

/// The avatar's edge, in logical px at text scale `1.0`.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AvatarSize {
    /// 24 px, `text-[10px]` initials.
    Sm,
    /// 28 px, `text-xs` initials: the chat message row's avatar.
    #[default]
    Md,
    /// 40 px, `text-base` initials.
    Lg,
}

impl AvatarSize {
    /// The edge in logical px at text scale `1.0`.
    pub const fn edge(self) -> f64 {
        match self {
            AvatarSize::Sm => 24.0,
            AvatarSize::Md => 28.0,
            AvatarSize::Lg => 40.0,
        }
    }

    /// The initials' font size in logical px at text scale `1.0`.
    pub const fn text_size(self) -> f64 {
        match self {
            AvatarSize::Sm => 10.0,
            AvatarSize::Md => crate::style::TEXT_XS,
            AvatarSize::Lg => crate::style::TEXT_BASE,
        }
    }
}

/// How the circle is filled.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AvatarTone {
    /// A container colour picked deterministically from the initials.
    #[default]
    Auto,
    /// The muted neutral (`bg-muted`, `text-muted-foreground`) — the look the
    /// message row's avatar has.
    Muted,
}

/// The initials style: `font-medium` at the size's text size. The family here is
/// the unthemed base; layout shapes in [`AVATAR_ROLE`]'s family.
fn initials_style(size: AvatarSize) -> TextStyle {
    TextStyle::default()
        .with_family(sans_family())
        .with_weight(FontWeight::MEDIUM)
        .with_size(size.text_size() as f32)
        .with_color(Color::BLACK)
}

/// Whether `c` joins the cluster before it instead of starting a new one.
fn extends(c: char) -> bool {
    matches!(c as u32,
        0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF
        | 0xFE00..=0xFE0F | 0xFE20..=0xFE2F | 0x200D | 0x1F3FB..=0x1F3FF)
}

fn is_regional_indicator(c: char) -> bool {
    matches!(c as u32, 0x1F1E6..=0x1F1FF)
}

/// Split `text` into approximate grapheme clusters (see the module docs).
fn clusters(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut prev_joiner = false;
    let mut open_flag = false;
    for c in text.chars() {
        let joins = !out.is_empty()
            && (extends(c) || prev_joiner || (open_flag && is_regional_indicator(c)));
        if joins {
            out.last_mut().expect("joined onto a cluster").push(c);
            open_flag = false;
        } else {
            out.push(c.to_string());
            open_flag = is_regional_indicator(c);
        }
        prev_joiner = c == '\u{200D}';
    }
    out
}

/// `raw` trimmed, upper-cased and cut to two grapheme clusters.
fn normalize_initials(raw: &str) -> String {
    clusters(&raw.trim().to_uppercase())
        .into_iter()
        .take(2)
        .collect()
}

/// The deterministic fill slot for `initials` (FNV-1a, so it is stable across
/// runs and platforms).
fn tone_slot(initials: &str) -> usize {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in initials.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    (hash % TONE_SLOTS as u64) as usize
}

/// The circle fill and the initials ink one avatar resolves.
#[derive(Clone, Copy, Debug, PartialEq)]
struct AvatarPaint {
    fill: Color,
    ink: Color,
}

impl AvatarPaint {
    /// Resolve against `theme`, falling back to beUI light's muted pair.
    fn resolve(theme: Option<&Theme>, tone: AvatarTone, initials: &str) -> Self {
        let Some(theme) = theme else {
            return AvatarPaint {
                fill: BEUI_LIGHT.muted,
                ink: BEUI_LIGHT.muted_foreground,
            };
        };
        let scheme = theme.scheme();
        match tone {
            AvatarTone::Muted => AvatarPaint {
                fill: scheme.surface_container_highest,
                ink: scheme.on_surface_variant,
            },
            _ => match tone_slot(initials) {
                0 => AvatarPaint {
                    fill: scheme.primary_container,
                    ink: scheme.on_primary_container,
                },
                1 => AvatarPaint {
                    fill: scheme.secondary_container,
                    ink: scheme.on_secondary_container,
                },
                _ => AvatarPaint {
                    fill: scheme.tertiary_container,
                    ink: scheme.on_tertiary_container,
                },
            },
        }
    }
}

/// The retained widget behind the avatar.
pub struct AvatarWidget {
    initials: LabelRun,
    size: AvatarSize,
    tone: AvatarTone,
    label: Option<String>,
    child: Option<ChildPod>,
    /// The edge resolved by the last layout (base edge times the text scale).
    edge: f64,
}

impl AvatarWidget {
    /// The initials this widget shows (already upper-cased and cut).
    pub fn initials(&self) -> &str {
        self.initials.content()
    }

    /// The text lines this widget carries: the initials.
    pub fn lines(&self) -> Vec<&str> {
        vec![self.initials.content()]
    }

    /// The edge resolved by the last layout.
    pub fn edge(&self) -> f64 {
        self.edge
    }

    /// The fill colour this widget paints under `theme`.
    pub fn fill_in(&self, theme: Option<&Theme>) -> Color {
        AvatarPaint::resolve(theme, self.tone, self.initials.content()).fill
    }
}

impl Widget for AvatarWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let scale = Theme::from_layout_ctx(ctx).map_or(1.0, |t| f64::from(t.text_scale()));
        let edge = self.size.edge() * scale;
        let size = bc.constrain(Size::new(edge, edge));
        self.edge = size.width.min(size.height);
        match &mut self.child {
            Some(child) => {
                child.layout_child(ctx, &BoxConstraints::tight(Size::new(self.edge, self.edge)));
                child.set_origin(Point::ORIGIN);
            }
            None => {
                self.initials
                    .layout_themed(ctx, &initials_style(self.size), AVATAR_ROLE);
            }
        }
        size
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let theme = Theme::from_paint_ctx(ctx);
        let paint = AvatarPaint::resolve(theme, self.tone, self.initials.content());
        let origin = ctx.origin();
        let edge = Size::new(self.edge, self.edge);
        scene.fill_rounded_rect(origin, edge, self.edge / 2.0, paint.fill);
        if let Some(child) = &mut self.child {
            scene.push_clip_rounded(origin, edge, self.edge / 2.0);
            child.paint_child(ctx, scene);
            scene.pop_clip();
        } else {
            let text = self.initials.size();
            self.initials.paint(
                origin
                    + Vec2::new(
                        (self.edge - text.width) / 2.0,
                        (self.edge - text.height) / 2.0,
                    ),
                paint.ink,
                scene,
            );
        }
    }

    fn visit_children(&self, visitor: &mut dyn FnMut(&ChildPod)) {
        if let Some(child) = &self.child {
            visitor(child);
        }
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        let name = self
            .label
            .as_deref()
            .unwrap_or_else(|| self.initials.content());
        if name.is_empty() {
            return;
        }
        ctx.push_node(Role::Image, |node| {
            node.set_label(name);
        });
    }
}

/// A circular initials avatar. See the [module docs](self).
pub struct AvatarView<State: 'static = ()> {
    initials: String,
    size: AvatarSize,
    tone: AvatarTone,
    label: Option<String>,
    image: Option<AnyView<State>>,
    _state: PhantomData<fn(&mut State)>,
}

/// An avatar showing `initials`, upper-cased and cut to two grapheme clusters.
pub fn avatar<State: 'static>(initials: impl Into<String>) -> AvatarView<State> {
    AvatarView {
        initials: normalize_initials(&initials.into()),
        size: AvatarSize::default(),
        tone: AvatarTone::default(),
        label: None,
        image: None,
        _state: PhantomData,
    }
}

impl<State: 'static> AvatarView<State> {
    /// Pick the edge ([`AvatarSize::Md`] by default).
    pub fn size(mut self, size: AvatarSize) -> Self {
        self.size = size;
        self
    }

    /// Pick how the circle is filled ([`AvatarTone::Auto`] by default).
    pub fn tone(mut self, tone: AvatarTone) -> Self {
        self.tone = tone;
        self
    }

    /// Show `image` instead of the initials, clipped to the circle. The view is
    /// laid out tight at the circle's edge.
    pub fn image<V: View<State>>(mut self, image: V) -> Self {
        self.image = Some(any(image));
        self
    }

    /// The accessible name, published on a [`Role::Image`] node. Without it the
    /// initials are the name.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

impl<State: 'static> View<State> for AvatarView<State> {
    type Element = AvatarWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> AvatarWidget {
        AvatarWidget {
            initials: LabelRun::new(self.initials.clone()),
            size: self.size,
            tone: self.tone,
            label: self.label.clone(),
            child: self.image.as_ref().map(|view| build_child(view, ctx)),
            edge: self.size.edge(),
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut AvatarWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = ChangeFlags::NONE;
        match (&prev.image, &self.image, &mut element.child) {
            (Some(old), Some(new), Some(pod)) => flags |= rebuild_child(old, new, pod, ctx),
            (None, Some(new), slot @ None) => {
                *slot = Some(build_child(new, ctx));
                flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
            }
            (Some(old), None, slot) => {
                if let Some(pod) = slot {
                    teardown_child(old, pod, ctx);
                }
                *slot = None;
                flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
            }
            _ => {}
        }
        if element.initials.set_content(self.initials.clone()) {
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        if element.size != self.size {
            element.size = self.size;
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        if element.tone != self.tone {
            element.tone = self.tone;
            flags |= ChangeFlags::PAINT;
        }
        if element.label != self.label {
            element.label.clone_from(&self.label);
            flags |= ChangeFlags::PAINT;
        }
        flags
    }

    fn teardown(&self, element: &mut AvatarWidget, ctx: &mut BuildCtx<'_>) {
        if let (Some(view), Some(pod)) = (&self.image, &mut element.child) {
            teardown_child(view, pod, ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use frust::Brightness;
    use frust::authoring::text::TextContext;
    use std::any::Any;

    /// Build `view` headlessly and lay it out, returning the widget and its size.
    fn laid_out<V: View<()>>(view: &V) -> (V::Element, Size) {
        let mut next_id = 0u64;
        let mut widget = view.build(&mut BuildCtx::new(&mut next_id));
        let mut text_ctx = TextContext::new();
        let mut layout = LayoutCtx::with_text_context(&mut text_ctx as &mut dyn Any);
        let size = widget.layout(&mut layout, &BoxConstraints::loose(Size::new(400.0, 400.0)));
        (widget, size)
    }

    fn avatar_of(initials: &str) -> AvatarView<()> {
        avatar(initials)
    }

    #[test]
    fn lower_case_initials_are_upper_cased() {
        let (a, _) = laid_out(&avatar_of("ed"));
        assert_eq!(a.initials(), "ED");
        assert_eq!(a.lines(), vec!["ED"]);
    }

    #[test]
    fn three_or_more_characters_are_cut_to_two() {
        assert_eq!(normalize_initials("abc"), "AB");
        assert_eq!(normalize_initials("  ed frust"), "ED");
        let (a, _) = laid_out(&avatar_of("xyz"));
        assert_eq!(a.initials(), "XY");
    }

    #[test]
    fn the_cut_keeps_whole_grapheme_clusters() {
        // `e` + combining acute is one cluster; a flag is one cluster.
        assert_eq!(normalize_initials("e\u{301}bc"), "E\u{301}B");
        assert_eq!(
            normalize_initials("\u{1F1E9}\u{1F1EA}\u{1F1EB}\u{1F1F7}x"),
            "\u{1F1E9}\u{1F1EA}\u{1F1EB}\u{1F1F7}"
        );
        // A ZWJ sequence stays together.
        assert_eq!(
            normalize_initials("\u{1F469}\u{200D}\u{1F4BB}ab"),
            "\u{1F469}\u{200D}\u{1F4BB}A"
        );
    }

    #[test]
    fn each_size_lays_out_square_at_its_edge() {
        for size in [AvatarSize::Sm, AvatarSize::Md, AvatarSize::Lg] {
            let (_, laid) = laid_out(&avatar_of("ab").size(size));
            assert_eq!(laid, Size::new(size.edge(), size.edge()), "{size:?}");
        }
        assert_eq!(
            AvatarSize::Md.edge(),
            crate::agents::message::MESSAGE_AVATAR_SIZE
        );
    }

    #[test]
    fn an_image_avatar_lays_out_square_too() {
        let (_, laid) = laid_out(&avatar_of("ab").size(AvatarSize::Lg).image(frust::text("x")));
        assert_eq!(laid, Size::new(40.0, 40.0));
    }

    #[test]
    fn the_same_initials_pick_the_same_tone_and_others_can_differ() {
        assert_eq!(tone_slot("ED"), tone_slot("ED"));
        let slots: std::collections::HashSet<usize> = ["AB", "CD", "ED", "FG", "HI", "JK"]
            .iter()
            .map(|i| tone_slot(i))
            .collect();
        assert!(slots.len() > 1, "six users all landed on one tone");
        assert!(slots.iter().all(|s| *s < TONE_SLOTS));
    }

    #[test]
    fn both_brightnesses_resolve_a_fill_and_the_muted_tone_matches_the_message_row() {
        for brightness in [Brightness::Light, Brightness::Dark] {
            let theme = crate::theme().with_brightness(brightness);
            let (auto, _) = laid_out(&avatar_of("ed"));
            let (muted, _) = laid_out(&avatar_of("ed").tone(AvatarTone::Muted));
            let scheme = theme.scheme();
            assert_eq!(
                muted.fill_in(Some(&theme)),
                scheme.surface_container_highest
            );
            let picked = auto.fill_in(Some(&theme));
            assert!(
                [
                    scheme.primary_container,
                    scheme.secondary_container,
                    scheme.tertiary_container
                ]
                .contains(&picked)
            );
        }
    }

    #[test]
    fn rebuild_reports_changes() {
        let mut next_id = 0u64;
        let old = avatar_of("ab");
        let mut widget = old.build(&mut BuildCtx::new(&mut next_id));
        let same = avatar_of("ab");
        let flags = same.rebuild(&old, &mut widget, &mut BuildCtx::new(&mut next_id));
        assert_eq!(flags, ChangeFlags::NONE);
        let bigger = avatar_of("cd").size(AvatarSize::Lg);
        let flags = bigger.rebuild(&same, &mut widget, &mut BuildCtx::new(&mut next_id));
        assert!(flags.contains(ChangeFlags::LAYOUT));
        assert_eq!(widget.initials(), "CD");
    }
}
