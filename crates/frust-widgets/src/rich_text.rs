//! The `rich_text` widget: one paragraph of mixed-style text.
//!
//! [`span`] describes a styled piece of text and [`rich_text`] joins pieces
//! into a [`RichTextView`], which materialises into a retained
//! [`RichTextWidget`]. The widget resolves every span's effective style at
//! layout exactly as [`crate::TextView`] does for a single run (themed colour
//! unless explicit, themed family roles, the theme's text scale for spans
//! without an explicit size), shapes the spans as one paragraph, and paints
//! span backgrounds, underline rects, glyph runs, then line-through rects.
//!
//! [`SpanView::background`] fills behind each line fragment of a span, padded
//! sideways without touching layout. [`RichTextView::on_link`] receives a
//! [`SpanView::link`] id on a primary tap that starts and ends on that link
//! within [`TOUCH_SLOP`]; a press elsewhere is not consumed, and a scroll
//! taking it over cancels it. Each link is a `Role::Link` child of the
//! paragraph node, with no focus stop of its own.
//!
//! `frust-text` publishes no per-range geometry, so fragments come from a
//! probe layout striking each marked group through in a colour encoding its
//! index (decoration never changes shaping or breaking), on line boxes
//! measured as the layout's height capped at each line count.

use std::any::TypeId;
use std::ops::Range;
use std::rc::Rc;

use frust_core::accesskit::{Action, Rect as AccessRect, Role};
use frust_core::{
    BoxConstraints, BuildCtx, ChangeFlags, CursorIcon, EventCtx, EventResult, InputEvent,
    InspectDetail, LayoutCtx, PaintCtx, PaintScene, PointerPhase, SemanticsCtx, TOUCH_SLOP,
    TextInspect, View, Widget,
};
use frust_text::{
    DecorationKind, FontFamily, FontStyle, FontWeight, LocaleStamp, OverflowWrap, TextAlign,
    TextContext, TextDecoration, TextLayout, TextOverflow, TextSpan, TextStyle,
};
use frust_theme::{ColorScheme, ShapeScale, SurfaceRole, Theme};
use kurbo::{Point, Rect, Size};
use peniko::Color;

use crate::authoring::{ErasedArgCallback, TypedArgCallback, erase_callback_arg, presses};
use crate::text::{
    FamilySource, ThemeTextColor, ThemeTextType, paint_layout, resolve_style, text_inspect,
};

/// How far a span background reaches past its glyphs on each side, in
/// logical px. Paint-only, so layout and line height are unchanged;
/// `frust-theme` has no spacing token to resolve it from.
const BG_PAD_X: f64 = 2.0;
/// Corner radius of a span background without a theme; a theme resolves it
/// from `shape.extra_small`.
const BG_RADIUS: f64 = 3.0;
/// Fill of a [`SpanView::themed_background`] span when no theme is threaded.
const BG_FALLBACK: Color = Color::from_rgb8(0xE5, 0xE7, 0xEB);

/// A styled piece of text inside a [`RichTextView`].
///
/// Carries the per-span subset of the [`crate::TextView`] builders. Colour,
/// family and size resolve per span with the same precedence as a plain text:
/// an explicit choice wins over the theme, and a span with no explicit size
/// follows the theme's text scale.
#[derive(Clone, Debug, PartialEq)]
pub struct SpanView {
    content: String,
    style: TextStyle,
    color_explicit: bool,
    role: ThemeTextColor,
    family_explicit: bool,
    size_explicit: bool,
    family_source: Option<FamilySource>,
    /// The explicit background fill; wins over `background_role`.
    background: Option<Color>,
    background_role: Option<SurfaceRole>,
    link: Option<String>,
}

/// Create a span rendering `content` with default styling.
pub fn span(content: impl Into<String>) -> SpanView {
    SpanView {
        content: content.into(),
        style: TextStyle::default(),
        color_explicit: false,
        role: ThemeTextColor::OnSurface,
        family_explicit: false,
        size_explicit: false,
        family_source: None,
        background: None,
        background_role: None,
        link: None,
    }
}

impl SpanView {
    /// Set the font size, in logical pixels. Marks the size as explicit, so
    /// the theme's text scale no longer applies to this span.
    pub fn size(mut self, size: f32) -> Self {
        self.style.size = size;
        self.size_explicit = true;
        self
    }

    /// Set the glyph fill color. Marks the color as explicit, so it wins over
    /// any themed default.
    pub fn color(mut self, color: Color) -> Self {
        self.style.color = color;
        self.color_explicit = true;
        self
    }

    /// Set the font weight.
    pub fn weight(mut self, weight: FontWeight) -> Self {
        self.style.weight = weight;
        self
    }

    /// Set the font family (or fallback stack). Marks the family as explicit,
    /// so it wins over any [`Self::themed_family`] role.
    pub fn family(mut self, family: FontFamily) -> Self {
        self.style.family = family;
        self.family_explicit = true;
        self
    }

    /// Set the font style to italic.
    pub fn italic(mut self) -> Self {
        self.style.style = FontStyle::Italic;
        self
    }

    /// Set the extra spacing between letters, in logical pixels.
    pub fn letter_spacing(mut self, letter_spacing: f32) -> Self {
        self.style.letter_spacing = letter_spacing;
        self
    }

    /// Tag the span with a BCP 47 locale (`"ja"`, `"th"`, `"zh-Hant"`). Touches
    /// only the locale; color, family and size keep resolving as before.
    pub fn locale(mut self, locale: impl Into<String>) -> Self {
        self.style.locale = Some(locale.into());
        self
    }

    /// Set one OpenType feature, appended after any already set; a later
    /// setting of the same tag wins.
    pub fn font_feature(mut self, tag: [u8; 4], value: u16) -> Self {
        self.style.features.push((tag, value));
        self
    }

    /// Replace the OpenType feature settings with `features`.
    pub fn font_features(mut self, features: impl IntoIterator<Item = ([u8; 4], u16)>) -> Self {
        self.style.features = features.into_iter().collect();
        self
    }

    /// Use tabular (fixed-width) figures, the `tnum` feature.
    pub fn tabular_figures(self) -> Self {
        self.font_feature(*b"tnum", 1)
    }

    /// Underline the span, keeping any other decoration already set.
    pub fn underline(mut self) -> Self {
        self.style.decoration.underline = true;
        self
    }

    /// Strike a line through the span, keeping any other decoration already
    /// set.
    pub fn line_through(mut self) -> Self {
        self.style.decoration.line_through = true;
        self
    }

    /// Replace the span's decoration wholesale.
    pub fn decoration(mut self, decoration: TextDecoration) -> Self {
        self.style.decoration = decoration;
        self
    }

    /// Resolve the span's family from the active theme's type-scale slot at
    /// layout, as [`crate::TextView::themed_family`] does.
    pub fn themed_family(mut self, role: ThemeTextType) -> Self {
        self.family_source = Some(FamilySource::Role(role));
        self
    }

    /// Resolve the span's family from the active theme's monospace family at
    /// layout, as [`crate::TextView::themed_mono_family`] does.
    pub fn themed_mono_family(mut self) -> Self {
        self.family_source = Some(FamilySource::Monospace);
        self
    }

    /// Set the themed default color role used when no explicit color was set.
    pub fn themed_role(mut self, role: ThemeTextColor) -> Self {
        self.role = role;
        self
    }

    /// Fill a small rounded rectangle of `color` behind the span, one per
    /// line fragment it occupies. Marks the background explicit, so it wins
    /// over [`Self::themed_background`].
    pub fn background(mut self, color: Color) -> Self {
        self.background = Some(color);
        self
    }

    /// Fill the span's background from the active theme's surface `role`,
    /// resolved at paint (a neutral grey with no theme). An explicit
    /// [`Self::background`] wins.
    pub fn themed_background(mut self, role: SurfaceRole) -> Self {
        self.background_role = Some(role);
        self
    }

    /// Mark the span as a link carrying the opaque `id` that
    /// [`RichTextView::on_link`] receives. Adds no styling: colour and
    /// underline stay the caller's. Adjacent spans with the same id form one
    /// link.
    pub fn link(mut self, id: impl Into<String>) -> Self {
        self.link = Some(id.into());
        self
    }

    /// Whether the span needs fragment geometry (a background or a link).
    fn is_marked(&self) -> bool {
        self.background.is_some() || self.background_role.is_some() || self.link.is_some()
    }

    /// Whether `other` paints the same background and belongs to the same
    /// link, so the two may share a shaping group.
    fn same_marks(&self, other: &SpanView) -> bool {
        self.background == other.background
            && self.background_role == other.background_role
            && self.link == other.link
    }

    /// The background fill: explicit, else the themed role, else none.
    fn background_color(&self, theme: Option<&Theme>) -> Option<Color> {
        if let Some(color) = self.background {
            return Some(color);
        }
        let role = self.background_role?;
        Some(theme.map_or(BG_FALLBACK, |t| surface_color(t.scheme(), role)))
    }

    fn effective_style(&self, theme: Option<&Theme>) -> TextStyle {
        resolve_style(
            &self.style,
            self.color_explicit,
            self.role,
            self.family_explicit,
            self.family_source,
            self.size_explicit,
            theme,
        )
    }
}

/// The colour a surface `role` names in `scheme`.
fn surface_color(scheme: &ColorScheme, role: SurfaceRole) -> Color {
    match role {
        SurfaceRole::Surface => scheme.surface,
        SurfaceRole::SurfaceContainerLowest => scheme.surface_container_lowest,
        SurfaceRole::SurfaceContainerLow => scheme.surface_container_low,
        SurfaceRole::SurfaceContainer => scheme.surface_container,
        SurfaceRole::SurfaceContainerHigh => scheme.surface_container_high,
        SurfaceRole::SurfaceContainerHighest => scheme.surface_container_highest,
    }
}

/// Create a rich text from `spans`, laid out as one paragraph.
pub fn rich_text(spans: impl IntoIterator<Item = SpanView>) -> RichTextView {
    RichTextView {
        spans: spans.into_iter().collect(),
        align: None,
        overflow_wrap: None,
        max_lines: None,
        overflow: TextOverflow::default(),
        on_link: LinkHandler::default(),
    }
}

/// A declarative description of a paragraph of styled [`SpanView`]s.
///
/// The paragraph knobs ([`Self::align`], [`Self::overflow_wrap`],
/// [`Self::max_lines`], [`Self::overflow`]) apply to the whole paragraph. A
/// truncation ellipsis takes the style of the span it cuts.
#[derive(Clone, Debug, PartialEq)]
pub struct RichTextView {
    spans: Vec<SpanView>,
    align: Option<TextAlign>,
    overflow_wrap: Option<OverflowWrap>,
    max_lines: Option<usize>,
    overflow: TextOverflow,
    on_link: LinkHandler,
}

/// The view-held [`RichTextView::on_link`] callback, erased at the call
/// because the view is not generic over the app state. Equality sees only
/// whether one is set, so a fresh closure each build keeps `prev == self`.
#[derive(Clone, Default)]
struct LinkHandler(Option<Rc<LinkCallback>>);

struct LinkCallback {
    /// The app-state type the callback was written against.
    state: TypeId,
    /// Makes the widget-held adapter; reinstalled on every build and rebuild.
    erase: Box<dyn Fn() -> ErasedArgCallback<String>>,
}

impl PartialEq for LinkHandler {
    fn eq(&self, other: &Self) -> bool {
        self.0.is_some() == other.0.is_some()
    }
}

impl std::fmt::Debug for LinkHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() { "Some(..)" } else { "None" })
    }
}

impl LinkHandler {
    /// The adapter for a view built under `State`, or `None` when no callback
    /// is set or it names another state type (a wiring bug: debug builds
    /// panic, release builds leave the links inert).
    fn install<State: 'static>(&self) -> Option<ErasedArgCallback<String>> {
        let callback = self.0.as_ref()?;
        debug_assert_eq!(
            callback.state,
            TypeId::of::<State>(),
            "RichTextView::on_link names a different state type than the view is built under"
        );
        (callback.state == TypeId::of::<State>()).then(|| (callback.erase)())
    }
}

impl RichTextView {
    /// Set the paragraph alignment; visible once the text wraps to more than
    /// one line.
    pub fn align(mut self, align: TextAlign) -> Self {
        self.align = Some(align);
        self
    }

    /// Set what a word wider than the line does (CSS `overflow-wrap`).
    pub fn overflow_wrap(mut self, overflow_wrap: OverflowWrap) -> Self {
        self.overflow_wrap = Some(overflow_wrap);
        self
    }

    /// Cap the rendered line count. Content past the limit is handled per
    /// [`Self::overflow`]. `0` renders nothing.
    pub fn max_lines(mut self, max_lines: usize) -> Self {
        self.max_lines = Some(max_lines);
        self
    }

    /// Set how content past [`Self::max_lines`] is handled.
    pub fn overflow(mut self, overflow: TextOverflow) -> Self {
        self.overflow = overflow;
        self
    }

    /// Run `on_link` against the app state with the id of a
    /// [`SpanView::link`] the user activates: a tap that starts and ends on
    /// that link, or the link node's accessibility Click. `State` is the
    /// state this view is built under; type it in the closure
    /// (`|app: &mut App, id| ..`).
    pub fn on_link<State: 'static>(mut self, on_link: impl Fn(&mut State, &str) + 'static) -> Self {
        let typed: TypedArgCallback<State, String> =
            Rc::new(move |state: &mut State, id: String| on_link(state, &id));
        self.on_link = LinkHandler(Some(Rc::new(LinkCallback {
            state: TypeId::of::<State>(),
            erase: Box::new(move || erase_callback_arg(&typed)),
        })));
        self
    }
}

impl<State: 'static> View<State> for RichTextView {
    type Element = RichTextWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> RichTextWidget {
        RichTextWidget {
            view: self.clone(),
            layout: None,
            laid_out_max_width: None,
            inspect: None,
            laid_out_span_styles: Vec::new(),
            laid_out_locale_stamp: LocaleStamp::default(),
            fragments: Vec::new(),
            links: link_runs(&self.spans),
            on_link: self.on_link.install::<State>(),
            press: None,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut RichTextWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        // Closures are not comparable: reinstall the adapter every time.
        element.on_link = self.on_link.install::<State>();
        if prev == self {
            return ChangeFlags::NONE;
        }
        element.view = self.clone();
        element.layout = None;
        element.fragments.clear();
        element.links = link_runs(&self.spans);
        element.press = None;
        ChangeFlags::LAYOUT
    }
}

/// One link: consecutive spans sharing a [`SpanView::link`] id.
struct LinkRun {
    id: String,
    /// The link's text, its accessible label.
    label: String,
    spans: Range<usize>,
}

/// The links of `spans`, in order.
fn link_runs(spans: &[SpanView]) -> Vec<LinkRun> {
    let mut runs: Vec<LinkRun> = Vec::new();
    for (index, span) in spans.iter().enumerate() {
        let Some(id) = &span.link else { continue };
        match runs.last_mut() {
            Some(last) if last.spans.end == index && last.id == *id => {
                last.label.push_str(&span.content);
                last.spans.end = index + 1;
            }
            _ => runs.push(LinkRun {
                id: id.clone(),
                label: span.content.clone(),
                spans: index..index + 1,
            }),
        }
    }
    runs
}

/// One line's share of a marked span group, in layout coordinates: the
/// glyphs' pen extent across, the line box down.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Fragment {
    /// The first span of the group, whose marks every span in it shares.
    span: usize,
    rect: Rect,
}

/// A press that began on link `link` at `start`.
struct LinkPress {
    link: usize,
    start: Point,
}

/// Retained widget for [`RichTextView`]: caches the shaped [`TextLayout`]
/// produced during layout for reuse in paint.
pub struct RichTextWidget {
    view: RichTextView,
    /// `None` until the first layout pass, or after a view change.
    layout: Option<TextLayout>,
    /// The width the cached layout was broken at.
    laid_out_max_width: Option<f32>,
    /// Layout report from the last real layout, served by `inspect_detail`.
    inspect: Option<TextInspect>,
    /// Every span's effective style, so a bare theme swap or text-scale
    /// change re-shapes without a view change. Kept before grouping:
    /// grouping depends on the theme-resolved styles, so two layouts with
    /// equal group styles can still split the spans differently.
    laid_out_span_styles: Vec<TextStyle>,
    /// The locale stamp read just before shaping.
    laid_out_locale_stamp: LocaleStamp,
    /// Every marked group's line fragments from the last layout, in line
    /// order; empty when no span has a background or link.
    fragments: Vec<Fragment>,
    links: Vec<LinkRun>,
    on_link: Option<ErasedArgCallback<String>>,
    press: Option<LinkPress>,
}

impl RichTextWidget {
    /// The link whose glyphs lie under the local point `at`, if any.
    fn link_at(&self, at: Point) -> Option<usize> {
        self.fragments
            .iter()
            .filter(|f| f.rect.contains(at))
            .find_map(|f| self.link_of(f.span))
    }

    /// The link span `span` belongs to.
    fn link_of(&self, span: usize) -> Option<usize> {
        self.links.iter().position(|l| l.spans.contains(&span))
    }

    /// Every span's effective style, with the paragraph knobs applied.
    fn effective_styles(&self, theme: Option<&Theme>) -> Vec<TextStyle> {
        self.view
            .spans
            .iter()
            .map(|span| {
                let mut style = span.effective_style(theme);
                if let Some(align) = self.view.align {
                    style.align = align;
                }
                if let Some(wrap) = self.view.overflow_wrap {
                    style.overflow_wrap = wrap;
                }
                style
            })
            .collect()
    }
}

/// A run of adjacent spans shaped as one: the index of the span whose style
/// they all share, and the spans' range.
struct SpanGroup {
    style: usize,
    spans: std::ops::Range<usize>,
}

/// What decides whether two adjacent spans may be shaped as one group: equal
/// effective style, background and link.
struct GroupKey<'a> {
    style: &'a TextStyle,
    span: &'a SpanView,
}

impl PartialEq for GroupKey<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.style == other.style && self.span.same_marks(other.span)
    }
}

/// Groups the non-empty spans (`contents`) for shaping: empty spans are
/// dropped, adjacent spans with equal `keys` merge (the widget keys on
/// effective style, background and link), and past `limit` groups
/// the excess folds into the last kept one (its text takes that group's
/// style) instead of overflowing the shaper's span index.
///
/// Text with no non-empty span keeps its first span so the empty layout is
/// still sized by that span's style.
fn group_spans<'a, K: PartialEq>(
    contents: impl ExactSizeIterator<Item = &'a str>,
    keys: &[K],
    limit: usize,
) -> Vec<SpanGroup> {
    let mut groups: Vec<SpanGroup> = Vec::new();
    let count = contents.len();
    for (index, content) in contents.enumerate() {
        if content.is_empty() {
            continue;
        }
        let full = groups.len() >= limit;
        match groups.last_mut() {
            Some(last) if keys[last.style] == keys[index] => last.spans.end = index + 1,
            Some(last) if full => last.spans.end = index + 1,
            _ => groups.push(SpanGroup {
                style: index,
                spans: index..index + 1,
            }),
        }
    }
    if groups.is_empty() && count > 0 {
        groups.push(SpanGroup {
            style: 0,
            spans: 0..1,
        });
    }
    groups
}

impl Widget for RichTextWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let max_width = {
            let w = bc.max().width;
            if w.is_finite() { Some(w as f32) } else { None }
        };
        let all_styles = self.effective_styles(Theme::from_layout_ctx(ctx));
        let locale_stamp = LocaleStamp::for_style(None);
        if let Some(cached) = &self.layout
            && self.laid_out_max_width == max_width
            && self.laid_out_span_styles == all_styles
            && self.laid_out_locale_stamp == locale_stamp
        {
            return bc.constrain(cached.size());
        }

        let view_spans = &self.view.spans;
        let keys: Vec<GroupKey<'_>> = view_spans
            .iter()
            .zip(&all_styles)
            .map(|(span, style)| GroupKey { style, span })
            .collect();
        let groups = group_spans(
            view_spans.iter().map(|s| s.content.as_str()),
            &keys,
            usize::from(u16::MAX),
        );
        let styles: Vec<TextStyle> = groups.iter().map(|g| all_styles[g.style].clone()).collect();
        let texts: Vec<String> = groups
            .iter()
            .map(|g| {
                self.view.spans[g.spans.clone()]
                    .iter()
                    .map(|s| s.content.as_str())
                    .collect()
            })
            .collect();
        let spans: Vec<TextSpan<'_>> = texts
            .iter()
            .zip(&styles)
            .map(|(text, style)| TextSpan::new(text, style))
            .collect();
        let text_ctx = ctx.text_context::<TextContext>();
        let layout = text_ctx.layout_rich_bounded(
            &spans,
            max_width,
            self.view.max_lines,
            self.view.overflow,
        );
        let tags: Vec<Option<usize>> = groups
            .iter()
            .map(|g| self.view.spans[g.style].is_marked().then_some(g.style))
            .collect();
        let fragments = marked_fragments(
            text_ctx,
            &spans,
            &tags,
            &layout,
            max_width,
            self.view.max_lines,
            self.view.overflow,
        );
        let size = bc.constrain(layout.size());
        let joined: String = self.view.spans.iter().map(|s| s.content.as_str()).collect();
        self.inspect = Some(text_inspect(&joined, &layout, self.view.max_lines));
        self.layout = Some(layout);
        self.fragments = fragments;
        self.laid_out_max_width = max_width;
        self.laid_out_span_styles = all_styles;
        self.laid_out_locale_stamp = locale_stamp;
        size
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let Some(layout) = &self.layout else {
            return;
        };
        let origin = ctx.origin();
        let theme = Theme::from_paint_ctx(ctx);
        for fragment in &self.fragments {
            let Some(color) = self.view.spans[fragment.span].background_color(theme) else {
                continue;
            };
            let rect = fragment.rect.inflate(BG_PAD_X, 0.0);
            let radius = theme.map_or(BG_RADIUS, |t| t.shape.extra_small);
            let radius = ShapeScale::resolve(radius, rect.width(), rect.height());
            scene.fill_rounded_rect(origin + rect.origin().to_vec2(), rect.size(), radius, color);
        }
        paint_layout(layout, origin, scene);
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        if self.on_link.is_none() || self.links.is_empty() {
            return EventResult::Ignored;
        }
        let InputEvent::Pointer(p) = event else {
            return EventResult::Ignored;
        };
        let within_slop = |press: &LinkPress| (p.position - press.start).hypot() <= TOUCH_SLOP;
        match p.phase {
            PointerPhase::Down => {
                self.press = None;
                if !presses(p) {
                    return EventResult::Ignored;
                }
                let Some(link) = self.link_at(p.position) else {
                    return EventResult::Ignored;
                };
                self.press = Some(LinkPress {
                    link,
                    start: p.position,
                });
                ctx.capture_pointer();
                EventResult::Handled
            }
            PointerPhase::Move => {
                if self.press.as_ref().is_some_and(|press| !within_slop(press)) {
                    // It became a drag: let it go without firing.
                    self.press = None;
                }
                if self.link_at(p.position).is_some() {
                    ctx.set_cursor(CursorIcon::Pointer);
                }
                if self.press.is_some() {
                    EventResult::Handled
                } else {
                    EventResult::Ignored
                }
            }
            PointerPhase::Up => {
                let Some(press) = self.press.take() else {
                    return EventResult::Ignored;
                };
                if presses(p)
                    && within_slop(&press)
                    && self.link_at(p.position) == Some(press.link)
                    && let Some(on_link) = self.on_link.as_mut()
                {
                    on_link(ctx, self.links[press.link].id.clone());
                    ctx.request_redraw();
                }
                EventResult::Handled
            }
            PointerPhase::Cancel => {
                if self.press.take().is_some() {
                    EventResult::Handled
                } else {
                    EventResult::Ignored
                }
            }
        }
    }

    fn inspect_detail(&self) -> Option<InspectDetail> {
        self.inspect.clone().map(InspectDetail::Text)
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        let value: String = self.view.spans.iter().map(|s| s.content.as_str()).collect();
        if self.links.is_empty() {
            ctx.push_node(Role::Label, |node| {
                node.set_value(value);
            });
            return;
        }
        let origin = ctx.origin();
        ctx.push_container(
            Role::Label,
            |node| node.set_value(value),
            |ctx| {
                for (index, link) in self.links.iter().enumerate() {
                    // A link cut off by `max_lines` has no glyphs to activate.
                    let Some(first) = self
                        .fragments
                        .iter()
                        .find(|f| self.link_of(f.span) == Some(index))
                    else {
                        continue;
                    };
                    // The first fragment's bounds, so a Click synthesized at
                    // their centre lands on this link's glyphs.
                    let bounds = first.rect + origin.to_vec2();
                    ctx.push_node(Role::Link, |node| {
                        node.set_label(link.label.as_str());
                        node.set_bounds(AccessRect {
                            x0: bounds.x0,
                            y0: bounds.y0,
                            x1: bounds.x1,
                            y1: bounds.y1,
                        });
                        if self.on_link.is_some() {
                            node.add_action(Action::Click);
                        }
                    });
                }
            },
        );
    }
}

/// Encodes group index `group` as a decoration colour the probe layout
/// hands back unchanged on its line-through rects. Zero marks a group that
/// is not tagged (it still gets a rect, so every line with text has one).
fn group_tag(group: usize) -> Color {
    Color::new([(group + 1) as f32, 0.0, 0.0, 1.0])
}

/// The line fragments of every tagged group (`tags[g]` names its first
/// span), in line order, for `layout`: the layout of `spans` at
/// `max_width`, capped by `max_lines` and `overflow`.
///
/// A probe re-lays `spans` with every group struck through in its
/// [`group_tag`], so the line-through rects give each group's pen extent
/// per line. The rects arrive line by line, so when they fall into exactly
/// `layout.line_count()` bands (every line has text) a rect's band is its
/// line with no further layout. Otherwise (blank lines, whitespace-only
/// lines) each rect's line is bisected over line tops. Either way the line
/// boxes are the heights of `spans` capped at `k` lines (breaking is greedy,
/// so the first `k` lines are unchanged), measured only at the boundaries a
/// marked fragment needs, never for unmarked lines above it. Touching pieces
/// of one group on a line (a face change inside it) merge.
fn marked_fragments(
    text_ctx: &mut TextContext,
    spans: &[TextSpan<'_>],
    tags: &[Option<usize>],
    layout: &TextLayout,
    max_width: Option<f32>,
    max_lines: Option<usize>,
    overflow: TextOverflow,
) -> Vec<Fragment> {
    if tags.iter().all(Option::is_none) {
        return Vec::new();
    }
    let probe_styles: Vec<TextStyle> = spans
        .iter()
        .enumerate()
        .map(|(group, span)| {
            let mut style = span.style.clone();
            style.decoration = TextDecoration::line_through().with_color(group_tag(group));
            style
        })
        .collect();
    let probe_spans: Vec<TextSpan<'_>> = spans
        .iter()
        .zip(&probe_styles)
        .map(|(span, style)| TextSpan::new(span.text, style))
        .collect();
    let probe = text_ctx.layout_rich_bounded(&probe_spans, max_width, max_lines, overflow);

    let lines = layout.line_count();
    let height = layout.size().height;
    let mut tops: std::collections::BTreeMap<usize, f64> = std::collections::BTreeMap::new();
    let mut top_of = |line: usize| -> f64 {
        if line == 0 {
            return 0.0;
        }
        if line >= lines {
            return height;
        }
        *tops.entry(line).or_insert_with(|| {
            text_ctx
                .layout_rich_bounded(spans, max_width, Some(line), TextOverflow::Clip)
                .size()
                .height
        })
    };

    // (group, rect) of every line-through rect, in line order.
    let rects: Vec<(usize, Rect)> = probe
        .decoration_rects(Point::ZERO)
        .into_iter()
        .filter(|d| d.kind == DecorationKind::LineThrough)
        .filter_map(|d| {
            let tag = d.color.components[0];
            (tag >= 1.0).then(|| ((tag as usize) - 1, d.rect))
        })
        .collect();

    // Band the rects: a new band starts when a rect sits more than half an
    // average line below everything in the current one.
    let tolerance = height / lines.max(1) as f64 / 2.0;
    let mut bands: Vec<usize> = Vec::with_capacity(rects.len());
    let mut band_max = f64::NEG_INFINITY;
    let mut band = 0usize;
    for (index, (_, rect)) in rects.iter().enumerate() {
        let mid = (rect.y0 + rect.y1) / 2.0;
        if index > 0 && mid > band_max + tolerance {
            band += 1;
            band_max = mid;
        } else {
            band_max = band_max.max(mid);
        }
        bands.push(band);
    }
    let banded = !rects.is_empty() && band + 1 == lines;

    let mut out: Vec<(usize, Fragment)> = Vec::new();
    let mut line = 0;
    for (index, (group, rect)) in rects.iter().enumerate() {
        let Some(span) = tags.get(*group).copied().flatten() else {
            continue;
        };
        if banded {
            line = bands[index];
        } else {
            let mid = (rect.y0 + rect.y1) / 2.0;
            // The last line in `line..lines` whose top is at or above `mid`.
            let (mut lo, mut hi) = (line, lines - 1);
            while lo < hi {
                let m = (lo + hi).div_ceil(2);
                if top_of(m) <= mid {
                    lo = m;
                } else {
                    hi = m - 1;
                }
            }
            line = lo;
        }
        let touches =
            |f: &Fragment| (rect.x0 - f.rect.x1).abs() < 0.5 || (f.rect.x0 - rect.x1).abs() < 0.5;
        match out.last_mut() {
            Some((on, f)) if *on == line && f.span == span && touches(f) => {
                f.rect.x0 = f.rect.x0.min(rect.x0);
                f.rect.x1 = f.rect.x1.max(rect.x1);
            }
            _ => {
                let (y0, y1) = (top_of(line), top_of(line + 1));
                out.push((
                    line,
                    Fragment {
                        span,
                        rect: Rect::new(rect.x0, y0, rect.x1, y1),
                    },
                ));
            }
        }
    }
    out.into_iter().map(|(_, f)| f).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use frust_scene::GlyphRun;
    use kurbo::Point;
    use peniko::Brush;
    use std::any::Any;

    #[derive(Debug, PartialEq)]
    enum Op {
        Rect(Point, Size, Color),
        Glyphs(Brush, f32),
    }

    #[derive(Default)]
    struct Recorder(Vec<Op>);

    impl PaintScene for Recorder {
        fn fill_rect(&mut self, o: Point, s: Size, c: Color) {
            self.0.push(Op::Rect(o, s, c));
        }
        fn draw_text(&mut self, _o: Point, _t: &str) {}
        fn draw_glyph_run(&mut self, run: GlyphRun) {
            self.0.push(Op::Glyphs(run.brush, run.font_size));
        }
    }

    fn lay_out(
        view: &RichTextView,
        theme: Option<&Theme>,
        width: f64,
        tcx: &mut TextContext,
    ) -> (RichTextWidget, Size) {
        let mut widget = View::<()>::build(view, &mut BuildCtx::new(&mut 0u64));
        let size = layout_widget(&mut widget, theme, width, tcx);
        (widget, size)
    }

    fn layout_widget(
        widget: &mut RichTextWidget,
        theme: Option<&Theme>,
        width: f64,
        tcx: &mut TextContext,
    ) -> Size {
        let mut lctx =
            LayoutCtx::with_resources(Some(tcx as &mut dyn Any), theme.map(|t| t as &dyn Any));
        widget.layout(&mut lctx, &BoxConstraints::loose(Size::new(width, 400.0)))
    }

    fn paint(widget: &mut RichTextWidget) -> Vec<Op> {
        let mut rec = Recorder::default();
        let mut pctx = PaintCtx::new(Point::ZERO, Size::new(300.0, 400.0));
        widget.paint(&mut pctx, &mut rec);
        rec.0
    }

    fn painted(view: &RichTextView, theme: Option<&Theme>) -> Vec<Op> {
        let _slot = crate::test_support::LocaleSlotGuard::hold();
        let mut tcx = TextContext::new();
        let (mut widget, _) = lay_out(view, theme, 300.0, &mut tcx);
        paint(&mut widget)
    }

    /// The style of each span group the widget last laid out, regrouped
    /// from the per-span styles it keys reuse on.
    fn group_styles(widget: &RichTextWidget) -> Vec<TextStyle> {
        let styles = &widget.laid_out_span_styles;
        group_spans(
            widget.view.spans.iter().map(|s| s.content.as_str()),
            styles,
            usize::from(u16::MAX),
        )
        .iter()
        .map(|g| styles[g.style].clone())
        .collect()
    }

    fn glyph_colors(ops: &[Op]) -> Vec<Color> {
        ops.iter()
            .filter_map(|op| match op {
                Op::Glyphs(Brush::Solid(c), _) => Some(*c),
                _ => None,
            })
            .collect()
    }

    fn glyph_sizes(ops: &[Op]) -> Vec<f32> {
        ops.iter()
            .filter_map(|op| match op {
                Op::Glyphs(_, s) => Some(*s),
                _ => None,
            })
            .collect()
    }

    fn red() -> Color {
        Color::from_rgb8(200, 20, 20)
    }
    fn green() -> Color {
        Color::from_rgb8(20, 160, 20)
    }
    fn blue() -> Color {
        Color::from_rgb8(20, 20, 200)
    }

    #[test]
    fn three_spans_paint_their_own_colours() {
        let view = rich_text([
            span("one ").color(red()),
            span("two ").color(green()),
            span("three").color(blue()),
        ]);
        let colors = glyph_colors(&painted(&view, None));
        assert_eq!(colors, vec![red(), green(), blue()]);
    }

    #[test]
    fn a_bold_span_shapes_with_its_own_weight() {
        let _slot = crate::test_support::LocaleSlotGuard::hold();
        let view = rich_text([span("plain "), span("bold").weight(FontWeight::BOLD)]);
        let mut tcx = TextContext::new();
        let (mut widget, _) = lay_out(&view, None, 300.0, &mut tcx);
        // The weight is part of the style each span is shaped with; the
        // neighbouring span keeps the default.
        let groups = group_styles(&widget);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].weight, FontWeight::default());
        assert_eq!(groups[1].weight, FontWeight::BOLD);
        assert_eq!(paint(&mut widget).len(), 2, "one run per span");
    }

    #[test]
    fn only_the_underlined_span_gets_an_underline_rect() {
        let plain = painted(&rich_text([span("aaaa "), span("bbbb")]), None);
        assert!(plain.iter().all(|op| !matches!(op, Op::Rect(..))));

        let ops = painted(&rich_text([span("aaaa "), span("bbbb").underline()]), None);
        let rects: Vec<_> = ops
            .iter()
            .filter_map(|op| match op {
                Op::Rect(o, s, _) => Some((*o, *s)),
                _ => None,
            })
            .collect();
        assert_eq!(rects.len(), 1, "{ops:?}");
        let (origin, size) = rects[0];
        // The first span's text occupies the left of the line; the underline
        // starts after it and does not cover the whole paragraph width.
        assert!(origin.x > 0.0, "starts under the second span: {origin:?}");
        assert!(size.width > 0.0 && size.height > 0.0);
        assert!(matches!(ops[0], Op::Rect(..)), "underline paints first");
    }

    #[test]
    fn line_through_paints_after_the_glyph_runs() {
        let ops = painted(
            &rich_text([span("aaaa "), span("bbbb").line_through()]),
            None,
        );
        assert!(matches!(ops.last(), Some(Op::Rect(..))), "{ops:?}");
    }

    fn themed(brightness: frust_theme::Brightness) -> Theme {
        Theme::neutral().with_brightness(brightness)
    }

    #[test]
    fn themed_colour_follows_a_brightness_flip_but_explicit_colour_does_not() {
        let _slot = crate::test_support::LocaleSlotGuard::hold();
        let view = rich_text([span("themed "), span("fixed").color(red())]);
        let light = themed(frust_theme::Brightness::Light);
        let dark = themed(frust_theme::Brightness::Dark);
        assert_ne!(light.scheme().on_surface, dark.scheme().on_surface);

        let mut tcx = TextContext::new();
        let (mut widget, _) = lay_out(&view, Some(&light), 300.0, &mut tcx);
        let before = glyph_colors(&paint(&mut widget));
        assert_eq!(before, vec![light.scheme().on_surface, red()]);

        layout_widget(&mut widget, Some(&dark), 300.0, &mut tcx);
        let after = glyph_colors(&paint(&mut widget));
        assert_eq!(after, vec![dark.scheme().on_surface, red()]);
    }

    #[test]
    fn semantics_value_is_the_concatenated_text() {
        let _slot = crate::test_support::LocaleSlotGuard::hold();
        let mut root: frust_core::RenderRoot<(), RichTextView> = frust_core::RenderRoot::new();
        let mut logic = |_: &mut ()| rich_text([span("Hello, "), span("rich "), span("world")]);
        root.rebuild(&mut logic, &mut ());
        let mut tcx = TextContext::new();
        root.layout_with_text(Size::new(400.0, 200.0), &mut tcx as &mut dyn std::any::Any);
        let update = root.semantics();
        let window = update
            .nodes
            .iter()
            .find(|(id, _)| *id == update.root)
            .expect("window node")
            .1
            .clone();
        let label = update
            .nodes
            .iter()
            .find(|(id, _)| *id == window.children()[0])
            .expect("rich text node")
            .1
            .clone();
        assert_eq!(label.role(), frust_core::accesskit::Role::Label);
        assert_eq!(label.value(), Some("Hello, rich world"));
    }

    #[test]
    fn max_lines_with_ellipsis_truncates() {
        let _slot = crate::test_support::LocaleSlotGuard::hold();
        let words = "word ".repeat(40);
        let open = rich_text([span(words.clone()), span("tail").weight(FontWeight::BOLD)]);
        let capped = open.clone().max_lines(1).overflow(TextOverflow::Ellipsis);
        let mut tcx = TextContext::new();
        let (_, full) = lay_out(&open, None, 120.0, &mut tcx);
        let (_, cut) = lay_out(&capped, None, 120.0, &mut tcx);
        assert!(full.height > cut.height * 1.5, "{full:?} vs {cut:?}");
    }

    #[test]
    fn rich_text_reports_flags_and_concatenated_text() {
        let _slot = crate::test_support::LocaleSlotGuard::hold();
        let words = "word ".repeat(40);
        let mut root: frust_core::RenderRoot<(), RichTextView> = frust_core::RenderRoot::new();
        root.rebuild(
            &mut |_: &mut ()| {
                rich_text([span(words.clone()), span("tail").weight(FontWeight::BOLD)])
                    .max_lines(1)
                    .overflow(TextOverflow::Ellipsis)
            },
            &mut (),
        );
        let mut tcx = TextContext::new();
        root.layout_with_text(Size::new(120.0, 200.0), &mut tcx as &mut dyn std::any::Any);
        let Some(Some(InspectDetail::Text(r))) =
            root.inspect().into_iter().map(|n| n.detail).next()
        else {
            panic!("no text detail");
        };
        assert_eq!(r.text, format!("{words}tail"));
        assert_eq!(r.lines, 1);
        assert!(r.did_exceed_max_lines && r.ellipsized, "{r:?}");
    }

    #[test]
    fn text_scale_enlarges_only_default_sized_spans() {
        // A pinned size differing from the default, so the two spans stay
        // separate groups.
        let view = rich_text([span("auto "), span("pinned").size(17.0)]);
        let base = Theme::neutral();
        let authored = base.type_scale.clone();
        let scaled = base.clone().with_text_scale(&authored, 1.5);

        let plain = glyph_sizes(&painted(&view, Some(&base)));
        let big = glyph_sizes(&painted(&view, Some(&scaled)));
        assert_eq!(plain.len(), 2);
        assert_eq!(big.len(), 2);
        assert!((big[0] / plain[0] - 1.5).abs() < 0.1, "{plain:?} {big:?}");
        assert!((big[1] - plain[1]).abs() < 0.01, "explicit size is pinned");
    }

    #[test]
    fn a_text_scale_change_that_regroups_spans_relays_out() {
        let _slot = crate::test_support::LocaleSlotGuard::hold();
        // At scale 1.0 the default size equals the pinned 16, so the groups
        // are [ab:16, c:24]; at scale 1.5 it is 24 and they are [a:16, bc:24].
        // Both group-style vectors are [S16, S24].
        let view = rich_text([span("a").size(16.0), span("b"), span("c").size(24.0)]);
        let base = Theme::neutral();
        let authored = base.type_scale.clone();
        let scaled = base.clone().with_text_scale(&authored, 1.5);

        let mut tcx = TextContext::new();
        let (mut reused, _) = lay_out(&view, Some(&base), 300.0, &mut tcx);
        let reused_size = layout_widget(&mut reused, Some(&scaled), 300.0, &mut tcx);
        let (mut fresh, fresh_size) = lay_out(&view, Some(&scaled), 300.0, &mut tcx);
        assert_eq!(
            glyph_sizes(&paint(&mut reused)),
            glyph_sizes(&paint(&mut fresh))
        );
        assert_eq!(reused_size, fresh_size, "\"b\" lays out at 24px");
    }

    #[test]
    fn an_unchanged_span_list_reuses_the_layout() {
        let _slot = crate::test_support::LocaleSlotGuard::hold();
        let view = rich_text([span("alpha ").color(red()), span("beta").underline()]);
        let mut tcx = TextContext::new();
        let (mut widget, _) = lay_out(&view, None, 300.0, &mut tcx);
        let shapes = tcx.shape_cache_stats().shapes;
        assert_eq!(shapes, 1);

        // An identical rebuild changes nothing and an extra pass skips the
        // text context entirely.
        let again = view.clone();
        let flags = View::<()>::rebuild(&again, &view, &mut widget, &mut BuildCtx::new(&mut 0u64));
        assert_eq!(flags, ChangeFlags::NONE);
        layout_widget(&mut widget, None, 300.0, &mut tcx);
        let stats = tcx.shape_cache_stats();
        assert_eq!(stats.shapes, 1, "no re-shape");
        assert_eq!(stats.hits, 0, "the widget skipped the text context");
    }

    #[test]
    fn a_changed_span_list_requests_layout_and_reshapes() {
        let _slot = crate::test_support::LocaleSlotGuard::hold();
        let view = rich_text([span("alpha "), span("beta")]);
        let next = rich_text([span("alpha "), span("gamma")]);
        let mut tcx = TextContext::new();
        let (mut widget, _) = lay_out(&view, None, 300.0, &mut tcx);
        let flags = View::<()>::rebuild(&next, &view, &mut widget, &mut BuildCtx::new(&mut 0u64));
        assert!(flags.contains(ChangeFlags::LAYOUT));
        layout_widget(&mut widget, None, 300.0, &mut tcx);
        assert_eq!(tcx.shape_cache_stats().shapes, 2);
    }

    #[test]
    fn empty_spans_drop_and_equal_neighbours_merge() {
        let view = rich_text([
            span("one "),
            span(""),
            span("two ").color(red()),
            span("three").color(red()),
            span("").color(green()),
            span("four"),
        ]);
        {
            let _slot = crate::test_support::LocaleSlotGuard::hold();
            let mut tcx = TextContext::new();
            let (widget, _) = lay_out(&view, None, 300.0, &mut tcx);
            assert_eq!(group_styles(&widget).len(), 3);
        }
        let colors = glyph_colors(&painted(&view, None));
        assert_eq!(colors.len(), 3);
        assert_eq!(colors[1], red());
    }

    #[test]
    fn grouping_folds_the_excess_into_the_last_kept_group() {
        let styles: Vec<TextStyle> = (0..6)
            .map(|i| TextStyle::new(10.0 + (i % 2) as f32, Color::BLACK))
            .collect();
        let texts = ["a"; 6];
        let groups = group_spans(texts.iter().copied(), &styles, 4);
        assert_eq!(groups.len(), 4);
        assert_eq!(groups[3].style, 3);
        assert_eq!(
            groups[3].spans,
            3..6,
            "the excess joins the last kept group"
        );
        let none = group_spans(["", ""].into_iter(), &styles, 4);
        assert_eq!(none.len(), 1, "all-empty text keeps its first span");
    }

    #[test]
    fn more_spans_than_the_shaper_indexes_degrade_instead_of_panicking() {
        let _slot = crate::test_support::LocaleSlotGuard::hold();
        // Alternating explicit sizes, so no neighbours merge.
        let count = usize::from(u16::MAX) + 4_000;
        let view =
            rich_text((0..count).map(|i| span("a").size(if i % 2 == 0 { 12.0 } else { 13.0 })));
        let mut tcx = TextContext::new();
        let (widget, size) = lay_out(&view, None, 300.0, &mut tcx);
        assert_eq!(group_styles(&widget).len(), usize::from(u16::MAX));
        assert!(size.width > 0.0);
    }

    /// Paint ops for the background and link tests: rounded fills, plain
    /// fills and every glyph's position.
    #[derive(Debug, PartialEq)]
    enum Mark {
        Round(Rect, f64, Color),
        Rect,
        Glyphs(Vec<(f32, f32)>),
    }

    #[derive(Default)]
    struct MarkRecorder(Vec<Mark>);

    impl PaintScene for MarkRecorder {
        fn fill_rect(&mut self, _o: Point, _s: Size, _c: Color) {
            self.0.push(Mark::Rect);
        }
        fn fill_rounded_rect(&mut self, o: Point, s: Size, radius: f64, c: Color) {
            self.0
                .push(Mark::Round(Rect::from_origin_size(o, s), radius, c));
        }
        fn draw_text(&mut self, _o: Point, _t: &str) {}
        fn draw_glyph_run(&mut self, run: GlyphRun) {
            self.0.push(Mark::Glyphs(
                run.glyphs.iter().map(|g| (g.x, g.y)).collect(),
            ));
        }
    }

    fn marks(widget: &mut RichTextWidget) -> Vec<Mark> {
        let mut rec = MarkRecorder::default();
        widget.paint(
            &mut PaintCtx::new(Point::ZERO, Size::new(300.0, 400.0)),
            &mut rec,
        );
        rec.0
    }

    fn grey() -> Color {
        Color::from_rgb8(220, 220, 220)
    }

    /// A paragraph whose middle span wraps across lines at a 120 px width.
    fn wrapped(middle: SpanView) -> RichTextView {
        rich_text([span("see "), middle, span(" after")])
    }

    const CODE: &str = "alpha beta gamma delta epsilon";

    #[test]
    fn a_wrapped_background_paints_one_padded_rect_per_line_fragment() {
        let _slot = crate::test_support::LocaleSlotGuard::hold();
        let mut tcx = TextContext::new();
        let (mut widget, size) = lay_out(
            &wrapped(span(CODE).background(grey())),
            None,
            120.0,
            &mut tcx,
        );
        let fragments = widget.fragments.clone();
        assert!(fragments.len() >= 2, "the span wraps: {fragments:?}");
        let first = fragments[0].rect;
        assert_eq!(first.y0, 0.0, "it starts on the first line");
        assert!(first.x0 > 0.0, "it starts mid-line, after \"see \"");
        for pair in fragments.windows(2) {
            assert_eq!(pair[0].rect.y1, pair[1].rect.y0, "line boxes stack");
            assert!(pair[0].rect.height() > 0.0);
        }
        assert!(fragments.last().unwrap().rect.y1 <= size.height + 0.01);

        let ops = marks(&mut widget);
        let rounds: Vec<(Rect, f64, Color)> = ops
            .iter()
            .filter_map(|op| match op {
                Mark::Round(r, radius, c) => Some((*r, *radius, *c)),
                _ => None,
            })
            .collect();
        assert_eq!(rounds.len(), fragments.len());
        for ((rect, radius, color), fragment) in rounds.iter().zip(&fragments) {
            assert_eq!(*rect, fragment.rect.inflate(BG_PAD_X, 0.0));
            assert_eq!(*radius, BG_RADIUS);
            assert_eq!(*color, grey());
        }
        let first_glyphs = ops.iter().position(|op| matches!(op, Mark::Glyphs(_)));
        assert!(
            first_glyphs.is_some_and(|g| g >= rounds.len()),
            "backgrounds paint under the glyphs: {ops:?}"
        );
    }

    fn glyph_positions(ops: &[Mark]) -> Vec<(f32, f32)> {
        ops.iter()
            .flat_map(|op| match op {
                Mark::Glyphs(g) => g.clone(),
                _ => Vec::new(),
            })
            .collect()
    }

    #[test]
    fn a_background_or_link_changes_neither_size_nor_glyph_positions() {
        let _slot = crate::test_support::LocaleSlotGuard::hold();
        let mut tcx = TextContext::new();
        let (mut plain, plain_size) = lay_out(&wrapped(span(CODE)), None, 120.0, &mut tcx);
        let reference = glyph_positions(&marks(&mut plain));
        for marked in [
            span(CODE).background(grey()),
            span(CODE).link("x"),
            span(CODE).themed_background(SurfaceRole::SurfaceContainerHigh),
        ] {
            let (mut widget, size) = lay_out(&wrapped(marked), None, 120.0, &mut tcx);
            assert_eq!(size, plain_size);
            assert_eq!(glyph_positions(&marks(&mut widget)), reference);
            assert_eq!(
                widget.layout.as_ref().map(TextLayout::line_count),
                plain.layout.as_ref().map(TextLayout::line_count)
            );
        }
    }

    #[test]
    fn a_background_resolves_explicit_then_themed_then_fallback() {
        let theme = Theme::neutral();
        let high = theme.scheme().surface_container_high;
        let themed = span("x").themed_background(SurfaceRole::SurfaceContainerHigh);
        assert_eq!(themed.background_color(Some(&theme)), Some(high));
        assert_eq!(themed.background_color(None), Some(BG_FALLBACK));
        let explicit_last = themed.clone().background(red());
        let explicit_first = span("x")
            .background(red())
            .themed_background(SurfaceRole::SurfaceContainerHigh);
        for s in [explicit_last, explicit_first] {
            assert_eq!(s.background_color(Some(&theme)), Some(red()));
        }
        assert_eq!(span("x").background_color(Some(&theme)), None);
    }

    #[test]
    fn a_link_hits_on_every_fragment_and_misses_elsewhere() {
        let _slot = crate::test_support::LocaleSlotGuard::hold();
        let mut tcx = TextContext::new();
        let (widget, size) = lay_out(&wrapped(span(CODE).link("docs")), None, 120.0, &mut tcx);
        assert_eq!(widget.links.len(), 1);
        assert!(widget.fragments.len() >= 2, "{:?}", widget.fragments);
        for fragment in &widget.fragments {
            assert_eq!(widget.link_at(fragment.rect.center()), Some(0));
        }
        let first = widget.fragments[0].rect;
        assert_eq!(widget.link_at(Point::new(1.0, first.center().y)), None);
        assert_eq!(
            widget.link_at(Point::new(first.x0 - BG_PAD_X / 2.0, 1.0)),
            None
        );
        assert_eq!(widget.link_at(Point::new(5.0, size.height + 5.0)), None);
    }

    #[test]
    fn link_spans_group_apart_from_equal_style_neighbours() {
        let _slot = crate::test_support::LocaleSlotGuard::hold();
        let view = rich_text([
            span("a "),
            span("b").link("one"),
            span("c").link("one").weight(FontWeight::BOLD),
            span(" d "),
            span("e").link("two"),
        ]);
        let runs = link_runs(&view.spans);
        let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["one", "two"]);
        assert_eq!(runs[0].label, "bc");
        assert_eq!(runs[0].spans, 1..3);

        let mut tcx = TextContext::new();
        let (widget, _) = lay_out(&view, None, 300.0, &mut tcx);
        let spans: Vec<usize> = widget.fragments.iter().map(|f| f.span).collect();
        assert_eq!(spans, [1, 2, 4], "one fragment per marked group");
        assert_eq!(widget.link_of(2), Some(0));
        assert_eq!(widget.link_of(4), Some(1));
    }

    #[test]
    fn a_mid_line_span_in_right_to_left_text_gets_its_visual_extent() {
        let _slot = crate::test_support::LocaleSlotGuard::hold();
        let view = rich_text([
            span("\u{5d0}\u{5d1}\u{5d2} "),
            span("\u{5d3}\u{5d4}").background(grey()).link("he"),
            span(" \u{5d5}\u{5d6}\u{5d7}"),
        ]);
        let mut tcx = TextContext::new();
        let (mut widget, _) = lay_out(&view, None, 300.0, &mut tcx);
        assert_eq!(widget.fragments.len(), 1, "{:?}", widget.fragments);
        let rect = widget.fragments[0].rect;
        // The line is right-aligned; the span sits between the other two.
        let xs: Vec<f64> = glyph_positions(&marks(&mut widget))
            .iter()
            .map(|&(x, _)| f64::from(x))
            .collect();
        let (left, right) = xs
            .iter()
            .fold((f64::MAX, f64::MIN), |(l, r), &x| (l.min(x), r.max(x)));
        assert!(
            rect.x0 > left && rect.x1 < right,
            "{rect:?} in {left}..{right}"
        );
        assert_eq!(widget.link_at(rect.center()), Some(0));
        assert_eq!(
            widget.link_at(Point::new(rect.x0 - 3.0, rect.center().y)),
            None
        );
        assert_eq!(
            widget.link_at(Point::new(rect.x1 + 3.0, rect.center().y)),
            None
        );
    }

    #[test]
    fn a_link_cut_by_max_lines_has_no_fragment() {
        let _slot = crate::test_support::LocaleSlotGuard::hold();
        let words = "word ".repeat(40);
        let view = rich_text([span(words), span("tail").link("gone")]).max_lines(1);
        let mut tcx = TextContext::new();
        let (widget, _) = lay_out(&view, None, 120.0, &mut tcx);
        assert!(widget.fragments.is_empty(), "{:?}", widget.fragments);
    }

    #[test]
    fn spans_and_views_compare_by_content_not_by_callback() {
        let make = |id: &str| {
            rich_text([span("x").background(grey()).link(id)]).on_link(|_: &mut (), _| {})
        };
        assert_eq!(make("a"), make("a"));
        assert_ne!(make("a"), make("b"));
        assert_ne!(
            make("a"),
            rich_text([span("x").background(grey()).link("a")]),
            "having a callback is part of the view"
        );
        assert_ne!(span("x").background(grey()), span("x"));
    }

    /// The shape cache's events (shapes, re-breaks and hits): one per
    /// layout of any cap, so its delta counts the layouts a call made.
    fn layout_events(tcx: &TextContext) -> u64 {
        let stats = tcx.shape_cache_stats();
        stats.shapes + stats.line_breaks + stats.hits
    }

    /// `lines` one-word lines at 40 px wide, with `marked` giving the line
    /// indices whose word is a link. Returns the widget, its height and the
    /// layouts the widget's first layout made.
    fn tall_paragraph(lines: usize, marked: &[usize]) -> (RichTextWidget, f64, u64) {
        let _slot = crate::test_support::LocaleSlotGuard::hold();
        let mut tcx = TextContext::new();
        let spans: Vec<SpanView> = (0..lines)
            .map(|i| {
                let word = span(if i == 0 {
                    "word000".to_owned()
                } else {
                    format!("\nword{i:03}")
                });
                if marked.contains(&i) {
                    word.link("x")
                } else {
                    word
                }
            })
            .collect();
        let view = rich_text(spans);
        let mut widget = View::<()>::build(&view, &mut BuildCtx::new(&mut 0u64));
        let before = layout_events(&tcx);
        let size = layout_widget(&mut widget, None, 300.0, &mut tcx);
        (widget, size.height, layout_events(&tcx) - before)
    }

    #[test]
    fn measuring_a_link_on_the_last_line_does_not_scale_with_the_lines_above() {
        let (small, _, small_layouts) = tall_paragraph(40, &[39]);
        let (large, _, large_layouts) = tall_paragraph(90, &[89]);
        assert_eq!(small.fragments.len(), 1);
        assert_eq!(large.fragments.len(), 1);
        // The fragment is the last of at least 40 line boxes.
        let f = small.fragments[0].rect;
        assert!(f.y1 / f.height() >= 39.5, "{f:?}");
        let f = large.fragments[0].rect;
        assert!(f.y1 / f.height() >= 89.5, "{f:?}");
        assert_eq!(
            small_layouts, large_layouts,
            "extra layouts must not grow with the unmarked lines above"
        );
        assert!(small_layouts <= 6, "{small_layouts} layouts");
    }

    #[test]
    fn measuring_marks_on_two_adjacent_lines_is_bounded() {
        let (small, _, small_layouts) = tall_paragraph(40, &[20, 21]);
        let (large, _, large_layouts) = tall_paragraph(90, &[60, 61]);
        assert_eq!(small.fragments.len(), 2);
        assert_eq!(large.fragments.len(), 2);
        let (a, b) = (small.fragments[0].rect, small.fragments[1].rect);
        assert!(
            (a.y1 - b.y0).abs() < 0.01,
            "adjacent line boxes share an edge"
        );
        assert!(a.y0 > 0.0 && b.y1 > b.y0);
        assert_eq!(small_layouts, large_layouts);
        assert!(small_layouts <= 7, "{small_layouts} layouts");
    }
}
