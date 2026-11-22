//! [`notice_row`]/[`NoticeRowView`]: an inline transcript row for a notice
//! or a turn in progress ("waiting for approval", "context trimmed"). It is
//! a muted paragraph with a tone mark, and it sits between message rows.
//!
//! # Contract
//!
//! - **Leading, wrapping paragraph.** The mark and the text share the first
//!   line; the text wraps to the width it is given, and the row fills it.
//! - **Tone is not colour alone.** Each tone has its own mark (`i`, `✓`,
//!   `!`, `×`), and the accessible label names the tone.
//! - **Working state.** `.working(true)` puts the Glyph dots loader before
//!   the text, for a line that says the turn is still going.
//! - **Controlled and non-reactive.** The caller owns the text and the tone.
//!   Equal views skip the rebuild.

use std::marker::PhantomData;

use frust::Theme;
use frust::authoring::text::{TextContext, TextLayout, TextStyle};
use frust::authoring::{
    BoxConstraints, BuildCtx, ChangeFlags, ChildPod, LayoutCtx, PaintCtx, PaintScene, Role,
    SemanticsCtx, View, Widget, build_child, teardown_child,
};
use kurbo::{Point, Size};
use peniko::Color;

use super::style;
use crate::dots::dots_loader;

/// Font size of the notice text and mark, in logical px.
const NOTICE_FONT_SIZE: f32 = style::LABEL_FONT_SIZE;
/// Gap between the mark, the loader and the text, in logical px.
const GAP: f64 = style::BLOCK_GAP;

/// The severity of a notice row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum NoticeTone {
    /// Neutral information, in the info accent.
    #[default]
    Info,
    /// A thing that succeeded, in the success accent.
    Success,
    /// A thing to watch, in the warning accent.
    Warning,
    /// A failure, in the error accent.
    Error,
}

impl NoticeTone {
    /// The mark drawn before the text, one glyph per tone.
    fn mark(self) -> &'static str {
        match self {
            NoticeTone::Info => "i",
            NoticeTone::Success => "\u{2713}", // ✓
            NoticeTone::Warning => "!",
            NoticeTone::Error => "\u{d7}", // ×
        }
    }

    /// The tone's name, as the accessible label gives it.
    fn word(self) -> &'static str {
        match self {
            NoticeTone::Info => "Info",
            NoticeTone::Success => "Success",
            NoticeTone::Warning => "Warning",
            NoticeTone::Error => "Error",
        }
    }
}

/// Every tone, for measuring the widest mark.
const TONES: [NoticeTone; 4] = [
    NoticeTone::Info,
    NoticeTone::Success,
    NoticeTone::Warning,
    NoticeTone::Error,
];

/// The mark's colour: the tone's accent, themed or the Glyph-dark fallback.
fn tone_color(theme: Option<&Theme>, tone: NoticeTone) -> Color {
    match tone {
        NoticeTone::Info => style::info(theme),
        NoticeTone::Success => style::success(theme),
        NoticeTone::Warning => style::warning(theme),
        NoticeTone::Error => style::error(theme),
    }
}

/// A declarative notice row. See the [module docs](self).
pub struct NoticeRowView<State> {
    text: String,
    tone: NoticeTone,
    working: bool,
    state: PhantomData<fn() -> State>,
}

impl<State> std::fmt::Debug for NoticeRowView<State> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NoticeRowView")
            .field("text", &self.text)
            .field("tone", &self.tone)
            .field("working", &self.working)
            .finish()
    }
}

/// Create a notice row with `text`, at the default [`NoticeTone::Info`].
/// The caller owns the text and tone and passes them on every build.
pub fn notice_row<State: 'static>(text: impl Into<String>) -> NoticeRowView<State> {
    NoticeRowView {
        text: text.into(),
        tone: NoticeTone::Info,
        working: false,
        state: PhantomData,
    }
}

impl<State> NoticeRowView<State> {
    /// Choose the row's tone.
    pub fn tone(mut self, tone: NoticeTone) -> Self {
        self.tone = tone;
        self
    }

    /// Show the dots loader before the text, for a turn still in progress.
    pub fn working(mut self, working: bool) -> Self {
        self.working = working;
        self
    }
}

impl<State: 'static> View<State> for NoticeRowView<State> {
    type Element = NoticeRowWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> NoticeRowWidget {
        NoticeRowWidget {
            text: self.text.clone(),
            tone: self.tone,
            working: self.working,
            loader: self
                .working
                .then(|| build_child::<State, _>(&dots_loader(), ctx)),
            mark_layout: None,
            mark_slot_w: 0.0,
            text_layout: None,
            text_x: 0.0,
            line_h: 0.0,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut NoticeRowWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        if prev.text == self.text && prev.tone == self.tone && prev.working == self.working {
            return ChangeFlags::NONE;
        }
        if prev.working != self.working {
            if self.working {
                element.loader = Some(build_child::<State, _>(&dots_loader(), ctx));
            } else if let Some(mut pod) = element.loader.take() {
                teardown_child::<State, _>(&dots_loader(), &mut pod, ctx);
            }
        }
        element.text = self.text.clone();
        element.tone = self.tone;
        element.working = self.working;
        ChangeFlags::LAYOUT | ChangeFlags::PAINT
    }

    fn teardown(&self, element: &mut NoticeRowWidget, ctx: &mut BuildCtx<'_>) {
        if let Some(pod) = element.loader.as_mut() {
            teardown_child::<State, _>(&dots_loader(), pod, ctx);
        }
    }
}

/// The retained widget for a [`NoticeRowView`]. See the [module docs](self).
pub struct NoticeRowWidget {
    text: String,
    tone: NoticeTone,
    working: bool,
    loader: Option<ChildPod>,
    mark_layout: Option<TextLayout>,
    /// Width reserved for the mark: the widest tone's mark.
    mark_slot_w: f64,
    text_layout: Option<TextLayout>,
    /// Where the text starts, past the mark and the loader.
    text_x: f64,
    /// Height of one line of the notice text.
    line_h: f64,
}

impl NoticeRowWidget {
    /// The label a screen reader hears: the tone, the text, then the state.
    fn label(&self) -> String {
        let working = if self.working { " (working)" } else { "" };
        format!("{}: {}{}", self.tone.word(), self.text, working)
    }
}

impl Widget for NoticeRowWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        // Resolve every colour and family before `ctx` is borrowed again
        // by the loader and the text context.
        let theme = Theme::from_layout_ctx(ctx);
        let mark_color = tone_color(theme, self.tone);
        let text_color = style::muted(theme);
        let family = style::mono_family(theme);
        let mark_style = TextStyle::new(NOTICE_FONT_SIZE, mark_color).with_family(family.clone());
        let text_style = TextStyle::new(NOTICE_FONT_SIZE, text_color).with_family(family);

        let mut loader_w = 0.0;
        let mut loader_h = 0.0;
        if self.working
            && let Some(pod) = self.loader.as_mut()
        {
            let size = pod.layout_child(
                ctx,
                &BoxConstraints::loose(Size::new(f64::INFINITY, f64::INFINITY)),
            );
            loader_w = size.width + GAP;
            loader_h = size.height;
        }

        let text = ctx.text_context::<TextContext>();
        self.mark_slot_w = TONES
            .iter()
            .map(|t| text.layout(t.mark(), &mark_style, None).size().width)
            .fold(0.0_f64, f64::max);
        self.mark_layout = Some(text.layout(self.tone.mark(), &mark_style, None));
        self.line_h = text.layout("0", &text_style, None).size().height;

        self.text_x = self.mark_slot_w + GAP + loader_w;
        let max_text_w = bc
            .max()
            .width
            .is_finite()
            .then(|| (bc.max().width - self.text_x).max(0.0) as f32);
        let text_layout = text.layout(&self.text, &text_style, max_text_w);
        let text_size = text_layout.size();
        self.text_layout = Some(text_layout);

        if self.working
            && let Some(pod) = self.loader.as_mut()
        {
            pod.set_origin(Point::new(
                self.mark_slot_w + GAP,
                style::PAD_SMALL + (self.line_h - loader_h) / 2.0,
            ));
        }

        let content_h = self.line_h.max(text_size.height);
        let height = content_h + style::PAD_SMALL * 2.0;
        let width = if bc.max().width.is_finite() {
            bc.max().width
        } else {
            self.text_x + text_size.width
        };
        bc.constrain(Size::new(width, height))
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let origin = ctx.origin();
        if let Some(mark) = &self.mark_layout {
            let y = origin.y + style::PAD_SMALL + (self.line_h - mark.size().height) / 2.0;
            style::paint_layout(mark, Point::new(origin.x, y), scene);
        }
        if self.working
            && let Some(pod) = self.loader.as_mut()
        {
            pod.paint_child(ctx, scene);
        }
        if let Some(text) = &self.text_layout {
            let x = origin.x + self.text_x;
            style::paint_layout(text, Point::new(x, origin.y + style::PAD_SMALL), scene);
        }
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        let label = self.label();
        ctx.push_node(Role::Status, |node| {
            node.set_label(label.as_str());
        });
    }

    frust::authoring::visit_children!(loader);
}

#[cfg(test)]
mod tests {
    use super::*;
    use frust::authoring::{AnyView, BuildCtx, any};
    use frust_core::RenderRoot;
    use std::any::Any;

    const LONG: &str = "Waiting for approval on the file change before the next tool call can run, \
        and the transcript will continue once the reviewer has answered.";

    fn build(text: &str, tone: NoticeTone, working: bool) -> NoticeRowWidget {
        let view = notice_row::<()>(text).tone(tone).working(working);
        let mut counter = 0u64;
        View::<()>::build(&view, &mut BuildCtx::new(&mut counter))
    }

    fn layout(widget: &mut NoticeRowWidget, width: f64, theme: Option<&Theme>) -> Size {
        let mut tcx = TextContext::new();
        let mut lctx = match theme {
            Some(t) => {
                LayoutCtx::with_resources(Some(&mut tcx as &mut dyn Any), Some(t as &dyn Any))
            }
            None => LayoutCtx::with_text_context(&mut tcx as &mut dyn Any),
        };
        widget.layout(&mut lctx, &BoxConstraints::loose(Size::new(width, 1000.0)))
    }

    fn paint(widget: &mut NoticeRowWidget, size: Size, theme: Option<&Theme>) {
        struct Discard;
        impl PaintScene for Discard {
            fn fill_rect(&mut self, _o: Point, _s: Size, _c: Color) {}
            fn draw_text(&mut self, _o: Point, _t: &str) {}
        }
        let mut pctx = match theme {
            Some(t) => PaintCtx::new(Point::ZERO, size).with_theme(t),
            None => PaintCtx::new(Point::ZERO, size),
        };
        widget.paint(&mut pctx, &mut Discard);
    }

    #[test]
    fn every_tone_has_a_distinct_mark_and_word() {
        for (i, a) in TONES.iter().enumerate() {
            for b in &TONES[i + 1..] {
                assert_ne!(a.mark(), b.mark(), "{a:?} vs {b:?}");
                assert_ne!(a.word(), b.word());
            }
        }
    }

    #[test]
    fn the_label_names_the_tone_and_the_working_state() {
        assert_eq!(
            build("Context trimmed", NoticeTone::Warning, false).label(),
            "Warning: Context trimmed"
        );
        assert_eq!(
            build("Thinking", NoticeTone::Info, true).label(),
            "Info: Thinking (working)"
        );
    }

    #[test]
    fn tones_lay_out_and_paint_at_320_and_1024() {
        let theme = crate::baseline();
        for width in [320.0, 1024.0] {
            for tone in TONES {
                for theme in [None, Some(&theme)] {
                    let mut w = build("Turn complete", tone, false);
                    let size = layout(&mut w, width, theme);
                    assert!((size.width - width).abs() < 1e-6, "fills the width");
                    assert!(size.height > 0.0);
                    paint(&mut w, size, theme);
                }
            }
        }
    }

    #[test]
    fn long_text_wraps_inside_a_narrow_width() {
        let mut short = build("Turn complete", NoticeTone::Info, false);
        let short_h = layout(&mut short, 320.0, None).height;
        let mut long = build(LONG, NoticeTone::Info, false);
        let long_h = layout(&mut long, 320.0, None).height;
        assert!(long_h > short_h, "a long notice takes more lines");
    }

    #[test]
    fn working_state_places_the_loader_before_the_text() {
        let mut idle = build("Waiting", NoticeTone::Info, false);
        let idle_x = {
            layout(&mut idle, 320.0, None);
            idle.text_x
        };
        let mut busy = build("Waiting", NoticeTone::Info, true);
        layout(&mut busy, 320.0, None);
        assert!(busy.loader.is_some());
        assert!(busy.text_x > idle_x, "the loader pushes the text right");
    }

    #[test]
    fn mark_slot_is_constant_across_tones() {
        let mut widths = Vec::new();
        for tone in TONES {
            let mut w = build("x", tone, false);
            layout(&mut w, 320.0, None);
            widths.push(w.text_x);
        }
        assert!(widths.windows(2).all(|p| (p[0] - p[1]).abs() < 1e-6));
    }

    // ---- Tree-level: the loader follows the working flag, and equal views skip work ----

    struct Rig {
        root: RenderRoot<(), AnyView<()>>,
        text: TextContext,
    }

    impl Rig {
        fn new() -> Self {
            let mut root: RenderRoot<(), AnyView<()>> = RenderRoot::new();
            root.set_theme(Box::new(crate::baseline()));
            Rig {
                root,
                text: TextContext::new(),
            }
        }

        fn show(&mut self, mut view: impl FnMut() -> NoticeRowView<()>) {
            self.root.rebuild(&mut |_: &mut ()| any(view()), &mut ());
            self.root
                .layout_with_text(Size::new(320.0, 200.0), &mut self.text as &mut dyn Any);
        }

        fn has_node(&self, ty: &str) -> bool {
            self.root
                .inspect()
                .iter()
                .any(|n| n.type_name.ends_with(ty))
        }

        fn events(&self) -> u64 {
            let s = self.text.shape_cache_stats();
            s.shapes + s.line_breaks
        }
    }

    #[test]
    fn the_loader_follows_the_working_flag() {
        let mut rig = Rig::new();
        rig.show(|| notice_row(LONG).working(true));
        assert!(rig.has_node("DotsLoaderWidget"));
        rig.show(|| notice_row(LONG).working(false));
        assert!(!rig.has_node("DotsLoaderWidget"));
        rig.show(|| notice_row(LONG).working(true));
        assert!(rig.has_node("DotsLoaderWidget"));
    }

    #[test]
    fn equal_views_do_no_text_work() {
        let mut rig = Rig::new();
        let make = || notice_row(LONG).tone(NoticeTone::Warning).working(true);
        rig.show(make);
        let before = rig.events();
        rig.show(make);
        assert_eq!(rig.events(), before);
    }

    #[test]
    fn themed_and_unthemed_paint_do_not_panic() {
        let theme = crate::baseline();
        for theme in [None, Some(&theme)] {
            for working in [false, true] {
                let mut w = build(LONG, NoticeTone::Error, working);
                let size = layout(&mut w, 320.0, theme);
                paint(&mut w, size, theme);
            }
        }
    }
}
