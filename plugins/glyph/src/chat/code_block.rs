//! [`code_block`]: agent-output code in a monospace, never-wrapping, selectable
//! body under a header with a language label and a copy control.
//!
//! # Contract
//!
//! - **Controlled and non-reactive.** The caller owns the text and re-builds
//!   the view as it grows; `.expanded(bool)` and `.on_toggle(..)` work like
//!   `crate::accordion` (a press is reported, never self-applied).
//! - **One horizontal scroll.** The body is a horizontal `custom_scroll_view`
//!   over the lines, so a long line scrolls sideways instead of wrapping or
//!   overflowing the block. The scroll view takes no vertical drag: one that
//!   starts on the body scrolls the enclosing transcript.
//! - **Cheap appends.** Lines sit in chunks of [`CHUNK_LINES`], one selectable
//!   `rich_text` each; an append changes only the last chunk's view, so
//!   earlier chunks are not re-shaped. A selection stays inside one chunk;
//!   the copy control copies the whole text.
//! - **Max visible lines.** `.max_visible_lines(n)` cuts a longer block to
//!   its first `n` lines (only those are built, hit and exposed) under a
//!   show-all row, while `.on_toggle(..)` is set (else it is never cut).
//! - **Copy.** A press writes the clipboard, then calls `.on_copy(..)` with
//!   the exact text; the label reads "Copied" for a moment. While
//!   `.streaming(true)` the control stays in place but inert.
//! - **Tones.** `.spans(..)` colours byte ranges by [`CodeTone`]; a span can
//!   name only a theme colour role, so a tone adds weight, style or strike.

use std::ops::Range;
use std::rc::Rc;

use frust::FrameTime;
use frust::authoring::ThemeTextColor;
use frust::authoring::text::{FontWeight, TextContext, TextLayout, TextStyle};
use frust::authoring::{
    Action, BoxConstraints, BuildCtx, ChangeFlags, ChildPod, ErasedArgCallback, ErasedCallback,
    EventCtx, EventResult, InputEvent, LayoutCtx, PaintCtx, PaintScene, Point, PointerPhase, Rect,
    Role, SemanticsCtx, Size, TypedArgCallback, View, Widget, build_child, erase_callback,
    erase_callback_arg, rebuild_child, route_event_single, teardown_child,
};
use frust::{RichTextView, ScrollAxis, Theme, column, custom_scroll_view, rich_text, span};

use super::style;
use crate::press::presses;

/// Lines per body chunk (one `rich_text` each).
pub(crate) const CHUNK_LINES: usize = 16;

/// What a run of code is, for colouring. A seam for a highlighter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CodeTone {
    /// Ordinary text.
    #[default]
    Plain,
    /// A language keyword.
    Keyword,
    /// A string or character literal.
    String,
    /// A comment.
    Comment,
    /// A numeric literal.
    Number,
    /// A function or method name.
    Function,
    /// A type name.
    Type,
    /// Text a diff added.
    Added,
    /// Text a diff removed.
    Removed,
}

impl CodeTone {
    /// The role, weight, italic and strike-through a tone is drawn with.
    fn look(self) -> (ThemeTextColor, FontWeight, bool, bool) {
        use ThemeTextColor::{Error, OnPrimaryContainer, OnSurface, OnSurfaceVariant};
        let w = FontWeight::REGULAR;
        match self {
            Self::Plain => (OnSurface, w, false, false),
            Self::Keyword => (OnPrimaryContainer, FontWeight::SEMI_BOLD, false, false),
            Self::String => (OnPrimaryContainer, w, false, false),
            Self::Comment => (OnSurfaceVariant, w, true, false),
            Self::Number => (OnSurfaceVariant, w, false, false),
            Self::Function => (OnSurface, FontWeight::SEMI_BOLD, false, false),
            Self::Type => (OnSurface, w, true, false),
            Self::Added => (OnPrimaryContainer, FontWeight::BOLD, false, false),
            Self::Removed => (Error, w, false, true),
        }
    }
}

/// A byte range of the code and the tone it is drawn in. A later span wins
/// where two overlap; a range that is empty, past the end or not on a
/// character boundary is ignored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeSpan {
    /// Byte range into the code text.
    pub range: Range<usize>,
    /// The tone of the range.
    pub style: CodeTone,
}

type OnToggle<State> = Rc<dyn Fn(&mut State)>;

/// A declarative code block. See the [module docs](self).
pub struct CodeBlockView<State: 'static> {
    code: String,
    language: Option<String>,
    spans: Vec<CodeSpan>,
    on_copy: Option<TypedArgCallback<State, String>>,
    max_visible_lines: Option<usize>,
    expanded: bool,
    on_toggle: Option<OnToggle<State>>,
    streaming: bool,
}

impl<State: 'static> std::fmt::Debug for CodeBlockView<State> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeBlockView")
            .field("code", &self.code)
            .field("language", &self.language)
            .field("spans", &self.spans)
            .field("max_visible_lines", &self.max_visible_lines)
            .field("expanded", &self.expanded)
            .field("streaming", &self.streaming)
            .finish_non_exhaustive()
    }
}

/// Show `code` in a monospace block that scrolls sideways and can be copied.
/// The caller owns the text and the expanded state and passes them on every
/// build.
pub fn code_block<State: 'static>(code: impl Into<String>) -> CodeBlockView<State> {
    CodeBlockView {
        code: code.into(),
        language: None,
        spans: Vec::new(),
        on_copy: None,
        max_visible_lines: None,
        expanded: false,
        on_toggle: None,
        streaming: false,
    }
}

impl<State: 'static> CodeBlockView<State> {
    /// The language label shown in the header (a label only).
    pub fn language(mut self, language: impl Into<String>) -> Self {
        self.language = Some(language.into());
        self
    }

    /// Colour byte ranges of the code by tone.
    pub fn spans(mut self, spans: Vec<CodeSpan>) -> Self {
        self.spans = spans;
        self
    }

    /// Run `on_copy` with the app state and the exact code text when the copy
    /// control is pressed (the clipboard is written first).
    pub fn on_copy(mut self, on_copy: impl Fn(&mut State, &str) + 'static) -> Self {
        let typed: TypedArgCallback<State, String> =
            Rc::new(move |state: &mut State, text: String| on_copy(state, &text));
        self.on_copy = Some(typed);
        self
    }

    /// Collapse a block of more than `lines` lines to its first `lines`, with
    /// a show-all row. Needs [`Self::on_toggle`]; see the module docs.
    pub fn max_visible_lines(mut self, lines: usize) -> Self {
        self.max_visible_lines = Some(lines);
        self
    }

    /// Set the (app-owned) expanded state of a collapsible block.
    pub fn expanded(mut self, expanded: bool) -> Self {
        self.expanded = expanded;
        self
    }

    /// Fire on a press of the show-all row. The widget never flips
    /// `expanded` itself.
    pub fn on_toggle(mut self, on_toggle: impl Fn(&mut State) + 'static) -> Self {
        self.on_toggle = Some(Rc::new(on_toggle));
        self
    }

    /// Mark the block unterminated: its copy control is inert until the
    /// caller clears the flag.
    pub fn streaming(mut self, streaming: bool) -> Self {
        self.streaming = streaming;
        self
    }

    /// The line count a collapsed block is cut to, or `None` when every
    /// line shows; mirrors [`CodeBlockWidget::collapsed`].
    fn shown_lines(&self) -> Option<usize> {
        let lines = line_ranges(&self.code).len();
        self.max_visible_lines
            .filter(|n| *n > 0 && lines > *n && self.on_toggle.is_some() && !self.expanded)
    }

    /// The body: while collapsed it holds only the visible lines, so its
    /// hit testing and semantics match what is painted.
    fn body_view(&self) -> impl View<State> + use<State> {
        let chunks = chunk_views(&self.code, &self.spans, self.shown_lines());
        custom_scroll_view(column().children(chunks)).axis(ScrollAxis::Horizontal)
    }
}

/// The byte range of every line, without its line ending. A trailing newline
/// ends the last line; empty text is one empty line.
fn line_ranges(code: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, b) in code.bytes().enumerate() {
        if b == b'\n' {
            let end = if i > start && code.as_bytes()[i - 1] == b'\r' {
                i - 1
            } else {
                i
            };
            out.push(start..end);
            start = i + 1;
        }
    }
    if start < code.len() || out.is_empty() {
        out.push(start..code.len());
    }
    out
}

/// The spans that apply to `code`, clipped to its end.
fn clean_spans(code: &str, spans: &[CodeSpan]) -> Vec<(Range<usize>, CodeTone)> {
    spans
        .iter()
        .filter_map(|s| {
            let end = s.range.end.min(code.len());
            let ok = s.range.start < end
                && code.is_char_boundary(s.range.start)
                && code.is_char_boundary(end);
            ok.then_some((s.range.start..end, s.style))
        })
        .collect()
}

fn push_piece(pieces: &mut Vec<(String, CodeTone)>, text: &str, tone: CodeTone) {
    match pieces.last_mut() {
        Some((last, t)) if *t == tone => last.push_str(text),
        _ => pieces.push((text.to_owned(), tone)),
    }
}

fn line_pieces(
    code: &str,
    line: &Range<usize>,
    spans: &[(Range<usize>, CodeTone)],
    pieces: &mut Vec<(String, CodeTone)>,
) {
    let mut cuts = vec![line.start, line.end];
    for (r, _) in spans {
        let (a, b) = (r.start.max(line.start), r.end.min(line.end));
        if a < b {
            cuts.extend([a, b]);
        }
    }
    cuts.sort_unstable();
    cuts.dedup();
    for w in cuts.windows(2) {
        let tone = spans
            .iter()
            .rev()
            .find(|(r, _)| r.start <= w[0] && w[1] <= r.end)
            .map_or(CodeTone::Plain, |(_, t)| *t);
        push_piece(pieces, &style::expand_tabs(&code[w[0]..w[1]]), tone);
    }
}

fn tone_span(text: &str, tone: CodeTone) -> frust::SpanView {
    let (role, weight, italic, strike) = tone.look();
    let mut s = span(text)
        .size(style::CODE_FONT_SIZE)
        .themed_mono_family()
        .themed_role(role)
        .weight(weight);
    if italic {
        s = s.italic();
    }
    if strike {
        s = s.line_through();
    }
    s
}

/// The body's views: one selectable paragraph per [`CHUNK_LINES`] lines.
/// With `max_lines`, only the first that many lines.
fn chunk_views(code: &str, spans: &[CodeSpan], max_lines: Option<usize>) -> Vec<RichTextView> {
    let spans = clean_spans(code, spans);
    let mut lines = line_ranges(code);
    lines.truncate(max_lines.unwrap_or(usize::MAX));
    lines
        .chunks(CHUNK_LINES)
        .map(|lines| {
            let mut pieces = Vec::new();
            for (i, line) in lines.iter().enumerate() {
                if i > 0 {
                    push_piece(&mut pieces, "\n", CodeTone::Plain);
                }
                line_pieces(code, line, &spans, &mut pieces);
            }
            if pieces.is_empty() {
                // An empty paragraph has no height; a space gives it a line.
                pieces.push((" ".to_owned(), CodeTone::Plain));
            }
            rich_text(pieces.iter().map(|(t, tone)| tone_span(t, *tone))).selectable(true)
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Copy,
    Toggle,
}

/// Shaped header and footer text.
#[derive(Default)]
struct Labels {
    language: Option<TextLayout>,
    copy: Option<TextLayout>,
    copied: Option<TextLayout>,
    copy_off: Option<TextLayout>,
    show_all: Option<TextLayout>,
}

/// The retained widget for a [`CodeBlockView`].
pub struct CodeBlockWidget {
    code: String,
    language: Option<String>,
    lines: usize,
    max_visible_lines: Option<usize>,
    expanded: bool,
    streaming: bool,
    on_copy: Option<ErasedArgCallback<String>>,
    on_toggle: Option<ErasedCallback>,
    body: ChildPod,
    labels: Labels,
    width: f64,
    line_h: f64,
    label_h: f64,
    header_h: f64,
    footer_h: f64,
    body_y: f64,
    visible_h: f64,
    copy_rect: Rect,
    pressed: Option<Target>,
    captured: Option<Target>,
    copied: bool,
    copied_at: Option<FrameTime>,
}

impl CodeBlockWidget {
    /// The line limit in force: set, exceeded and expandable.
    fn limit(&self) -> Option<usize> {
        self.max_visible_lines
            .filter(|n| *n > 0 && self.lines > *n && self.on_toggle.is_some())
    }

    fn collapsed(&self) -> bool {
        self.limit().is_some() && !self.expanded
    }

    fn footer_text(&self) -> String {
        if self.expanded {
            "show fewer lines".to_owned()
        } else {
            format!("show all {} lines", self.lines)
        }
    }

    fn hit(&self, p: Point) -> Option<Target> {
        if p.y < self.header_h {
            let live = !self.streaming && self.copy_rect.contains(p);
            return live.then_some(Target::Copy);
        }
        let footer_top = self.body_y + self.visible_h + style::PAD_SMALL;
        (self.limit().is_some() && p.y >= footer_top).then_some(Target::Toggle)
    }

    fn fire(&mut self, ctx: &mut EventCtx, target: Target) {
        match target {
            Target::Copy => {
                ctx.write_clipboard(self.code.clone());
                if let Some(cb) = self.on_copy.as_mut() {
                    cb(ctx, self.code.clone());
                }
                self.copied = true;
                self.copied_at = None;
            }
            Target::Toggle => {
                if let Some(cb) = self.on_toggle.as_mut() {
                    cb(ctx);
                }
            }
        }
    }

    fn shape(&mut self, ctx: &mut LayoutCtx) {
        let theme = Theme::from_layout_ctx(ctx);
        let family = style::mono_family(theme);
        let active = style::ink(theme);
        let done = style::success(theme);
        let muted = style::muted(theme);
        let mk = |color| TextStyle::new(style::LABEL_FONT_SIZE, color).with_family(family.clone());
        let code_style = TextStyle::new(style::CODE_FONT_SIZE, active).with_family(family.clone());
        let text = ctx.text_context::<TextContext>();
        self.line_h = text.layout("0", &code_style, None).size().height;
        self.labels = Labels {
            language: self
                .language
                .as_deref()
                .map(|l| text.layout(l, &mk(muted), None)),
            copy: Some(text.layout(style::COPY_TEXT, &mk(active), None)),
            copied: Some(text.layout(style::COPIED_TEXT, &mk(done), None)),
            copy_off: Some(text.layout(style::COPY_TEXT, &mk(muted), None)),
            show_all: self
                .limit()
                .map(|_| text.layout(&self.footer_text(), &mk(muted), None)),
        };
        self.label_h = self.labels.copy.as_ref().map_or(0.0, |l| l.size().height);
    }

    /// The widest copy-control label, so toggling "Copy" and "Copied" never
    /// moves anything.
    fn copy_w(&self) -> f64 {
        [&self.labels.copy, &self.labels.copied]
            .into_iter()
            .flatten()
            .map(|l| l.size().width)
            .fold(0.0, f64::max)
    }
}

impl<State: 'static> View<State> for CodeBlockView<State> {
    type Element = CodeBlockWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> CodeBlockWidget {
        CodeBlockWidget {
            code: self.code.clone(),
            language: self.language.clone(),
            lines: line_ranges(&self.code).len(),
            max_visible_lines: self.max_visible_lines,
            expanded: self.expanded,
            streaming: self.streaming,
            on_copy: self.on_copy.as_ref().map(erase_callback_arg),
            on_toggle: self.on_toggle.as_ref().map(erase_callback),
            body: build_child(&self.body_view(), ctx),
            labels: Labels::default(),
            width: 0.0,
            line_h: 0.0,
            label_h: 0.0,
            header_h: 0.0,
            footer_h: 0.0,
            body_y: 0.0,
            visible_h: 0.0,
            copy_rect: Rect::ZERO,
            pressed: None,
            captured: None,
            copied: false,
            copied_at: None,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut CodeBlockWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = ChangeFlags::NONE;
        element.on_copy = self.on_copy.as_ref().map(erase_callback_arg);
        element.on_toggle = self.on_toggle.as_ref().map(erase_callback);
        if prev.code != self.code
            || prev.spans != self.spans
            || prev.shown_lines() != self.shown_lines()
        {
            element.code = self.code.clone();
            element.lines = line_ranges(&self.code).len();
            flags |= rebuild_child(&prev.body_view(), &self.body_view(), &mut element.body, ctx);
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        if prev.language != self.language
            || prev.max_visible_lines != self.max_visible_lines
            || prev.expanded != self.expanded
            || prev.on_toggle.is_some() != self.on_toggle.is_some()
        {
            element.language = self.language.clone();
            element.max_visible_lines = self.max_visible_lines;
            element.expanded = self.expanded;
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        if prev.streaming != self.streaming {
            element.streaming = self.streaming;
            flags |= ChangeFlags::PAINT;
        }
        flags
    }

    fn teardown(&self, element: &mut CodeBlockWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.body_view(), &mut element.body, ctx);
    }
}

impl Widget for CodeBlockWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        self.width = style::width_of(bc);
        self.shape(ctx);

        self.header_h = self.label_h + style::PAD_SMALL * 2.0;
        let btn_w = self.copy_w() + style::PAD_SMALL * 2.0;
        self.copy_rect = Rect::new(
            (self.width - btn_w - style::PAD_SMALL).max(0.0),
            0.0,
            self.width,
            self.header_h,
        );
        self.body_y = self.header_h + style::PAD_SMALL;

        let body_w = (self.width - style::PAD * 2.0).max(0.0);
        let shown = match (self.limit(), self.collapsed()) {
            (Some(n), true) => n,
            _ => self.lines,
        };
        self.visible_h = shown as f64 * self.line_h;
        let body_bc = BoxConstraints::tight(Size::new(body_w, self.visible_h));
        self.body.layout_child(ctx, &body_bc);
        self.body.set_origin(Point::new(style::PAD, self.body_y));

        self.footer_h = if self.limit().is_some() {
            self.label_h + style::PAD_SMALL * 2.0
        } else {
            0.0
        };
        let height = self.body_y + self.visible_h + style::PAD_SMALL + self.footer_h;
        bc.constrain(Size::new(self.width, height))
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        if self.copied {
            let now = ctx.frame_time();
            let since = *self.copied_at.get_or_insert(now);
            if now.saturating_sub(since) >= style::COPIED_FOR {
                self.copied = false;
                self.copied_at = None;
            } else {
                ctx.request_frame();
            }
        }
        let theme = Theme::from_paint_ctx(ctx);
        let (surface, border) = (style::surface(theme), style::border(theme));
        let o = ctx.origin();
        let size = ctx.size();
        scene.fill_rounded_rect(o, size, style::RADIUS, surface);

        let text_y = |top: f64, h: f64| top + (h - self.label_h) / 2.0;
        if let Some(l) = &self.labels.language {
            style::paint_layout(
                l,
                Point::new(o.x + style::PAD, text_y(o.y, self.header_h)),
                scene,
            );
        }
        let copy = if self.streaming {
            &self.labels.copy_off
        } else if self.copied {
            &self.labels.copied
        } else {
            &self.labels.copy
        };
        if let Some(l) = copy {
            let at = Point::new(
                o.x + self.width - style::PAD - self.copy_w(),
                text_y(o.y, self.header_h),
            );
            style::paint_layout(l, at, scene);
        }
        let divider = |y: f64, scene: &mut dyn PaintScene| {
            scene.stroke_line(
                Point::new(o.x, o.y + y),
                Point::new(o.x + size.width, o.y + y),
                style::BORDER_WIDTH,
                border,
            );
        };
        divider(self.header_h, scene);

        scene.push_clip(
            Point::new(o.x + style::PAD, o.y + self.body_y),
            Size::new((size.width - style::PAD * 2.0).max(0.0), self.visible_h),
        );
        self.body.paint_child(ctx, scene);
        scene.pop_clip();

        if let Some(l) = &self.labels.show_all {
            let top = self.body_y + self.visible_h + style::PAD_SMALL;
            divider(top, scene);
            let at = Point::new(o.x + style::PAD, text_y(o.y + top, self.footer_h));
            style::paint_layout(l, at, scene);
        }

        style::stroke_outline(scene, o, size, style::RADIUS, border);
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        let InputEvent::Pointer(p) = event else {
            return route_event_single(&mut self.body, ctx, event);
        };
        match p.phase {
            PointerPhase::Down => {
                if presses(p)
                    && let Some(target) = self.hit(p.position)
                {
                    self.captured = Some(target);
                    self.pressed = Some(target);
                    ctx.capture_pointer();
                    ctx.request_redraw();
                    return EventResult::Handled;
                }
                route_event_single(&mut self.body, ctx, event)
            }
            PointerPhase::Move => {
                let Some(target) = self.captured else {
                    return route_event_single(&mut self.body, ctx, event);
                };
                let inside = (self.hit(p.position) == Some(target)).then_some(target);
                if inside != self.pressed {
                    self.pressed = inside;
                    ctx.request_redraw();
                }
                EventResult::Handled
            }
            PointerPhase::Up => {
                let Some(target) = self.captured.take() else {
                    return route_event_single(&mut self.body, ctx, event);
                };
                self.pressed = None;
                if self.hit(p.position) == Some(target) {
                    self.fire(ctx, target);
                }
                ctx.request_redraw();
                EventResult::Handled
            }
            PointerPhase::Cancel => {
                if self.captured.take().is_some() {
                    self.pressed = None;
                    ctx.request_redraw();
                    return EventResult::Handled;
                }
                route_event_single(&mut self.body, ctx, event)
            }
        }
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        let label = match &self.language {
            Some(l) => format!("code block, {l}"),
            None => "code block".to_owned(),
        };
        ctx.push_container(
            Role::Group,
            |node| node.set_label(label.as_str()),
            |ctx| {
                let origin = ctx.origin();
                let bounds = |r: Rect| style::node_bounds(origin, r);
                ctx.push_node(Role::Button, |node| {
                    node.set_label("Copy code");
                    if !self.streaming {
                        node.add_action(Action::Click);
                    }
                    node.set_bounds(bounds(self.copy_rect));
                });
                self.body.semantics_child(ctx);
                if self.limit().is_some() {
                    let top = self.body_y + self.visible_h + style::PAD_SMALL;
                    let text = self.footer_text();
                    ctx.push_node(Role::Button, |node| {
                        node.set_label(text.as_str());
                        node.set_expanded(self.expanded);
                        node.add_action(Action::Click);
                        node.set_bounds(bounds(Rect::new(
                            0.0,
                            top,
                            self.width,
                            top + self.footer_h,
                        )));
                    });
                }
            },
        );
    }

    frust::authoring::visit_children!(body);
}

#[cfg(test)]
mod tests {
    use super::*;
    use frust::authoring::scene::GlyphRun;
    use frust::authoring::{AnyView, Color, DiscardScene, PointerButton, PointerEvent, any};
    use frust_core::RenderRoot;
    use std::any::Any;

    #[derive(Default)]
    struct App {
        copied: Vec<String>,
        toggles: usize,
    }

    type Root = RenderRoot<App, AnyView<App>>;

    struct Rig {
        root: Root,
        app: App,
        text: TextContext,
        window: Size,
        now: u64,
    }

    impl Rig {
        fn new(themed: bool, window_w: f64) -> Self {
            let mut root: Root = RenderRoot::new();
            if themed {
                root.set_theme(Box::new(crate::baseline()));
            }
            Rig {
                root,
                app: App::default(),
                text: TextContext::new(),
                window: Size::new(window_w, 800.0),
                now: 0,
            }
        }

        fn show(&mut self, mut view: impl FnMut(&mut App) -> CodeBlockView<App>) {
            self.root
                .rebuild(&mut |s: &mut App| any(view(s)), &mut self.app);
            self.root
                .layout_with_text(self.window, &mut self.text as &mut dyn Any);
        }

        fn paint(&mut self, secs: f64) -> frust::authoring::PaintOutcome {
            let t = FrameTime::from_nanos((secs * 1e9) as u64);
            self.now = t.as_nanos();
            self.root.paint(&mut DiscardScene, t)
        }

        fn node(&self, ty: &str) -> Vec<Rect> {
            self.root
                .inspect()
                .into_iter()
                .filter(|n| n.type_name.ends_with(ty))
                .map(|n| n.bounds)
                .collect()
        }

        /// The block itself: the root pod, whose widget type erases to a box.
        fn block(&self) -> Rect {
            self.root.inspect()[0].bounds
        }

        fn click(&mut self, at: Point) {
            for phase in [PointerPhase::Down, PointerPhase::Up] {
                let e = InputEvent::Pointer(PointerEvent {
                    phase,
                    position: at,
                    button: PointerButton::Primary,
                });
                self.root.event(&mut self.app, &e);
            }
        }

        fn events(&self) -> u64 {
            let s = self.text.shape_cache_stats();
            s.shapes + s.line_breaks + s.hits
        }
    }

    fn numbered(n: usize) -> String {
        (1..=n).map(|i| format!("line {i}\n")).collect()
    }

    fn copy_point(block: Rect) -> Point {
        Point::new(block.x1 - style::PAD, block.y0 + 8.0)
    }

    #[test]
    fn lines_split_without_endings() {
        assert_eq!(line_ranges("").len(), 1);
        assert_eq!(line_ranges("a\nb\n").len(), 2);
        assert_eq!(line_ranges("a\nb").len(), 2);
        assert_eq!(line_ranges("a\n\n").len(), 2);
        let crlf = "a\r\nb";
        let r = line_ranges(crlf);
        assert_eq!(&crlf[r[0].clone()], "a");
    }

    #[test]
    fn spans_cut_a_line_and_bad_ranges_are_ignored() {
        let code = "let é = 1;";
        let spans = vec![
            CodeSpan {
                range: 0..3,
                style: CodeTone::Keyword,
            },
            CodeSpan {
                range: 5..6, // inside the two-byte `é`
                style: CodeTone::String,
            },
            CodeSpan {
                range: 8..99, // clipped to the end
                style: CodeTone::Number,
            },
            CodeSpan {
                range: 4..4,
                style: CodeTone::Type,
            },
        ];
        let clean = clean_spans(code, &spans);
        assert_eq!(clean.len(), 2);
        let mut pieces = Vec::new();
        line_pieces(code, &(0..code.len()), &clean, &mut pieces);
        let joined: String = pieces.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(joined, code);
        assert_eq!(pieces[0], ("let".to_owned(), CodeTone::Keyword));
        assert_eq!(pieces.last().map(|p| p.1), Some(CodeTone::Number));
    }

    #[test]
    fn later_spans_win_where_they_overlap() {
        let code = "abcdef";
        let spans = clean_spans(
            code,
            &[
                CodeSpan {
                    range: 0..6,
                    style: CodeTone::Comment,
                },
                CodeSpan {
                    range: 2..4,
                    style: CodeTone::Keyword,
                },
            ],
        );
        let mut pieces = Vec::new();
        line_pieces(code, &(0..6), &spans, &mut pieces);
        let tones: Vec<_> = pieces.iter().map(|p| p.1).collect();
        assert_eq!(
            tones,
            [CodeTone::Comment, CodeTone::Keyword, CodeTone::Comment]
        );
    }

    #[test]
    fn appending_a_line_changes_only_the_last_chunk_view() {
        let before = chunk_views(&numbered(40), &[], None);
        let after = chunk_views(&numbered(41), &[], None);
        assert_eq!(before.len(), 3);
        assert_eq!(after.len(), 3);
        assert_eq!(before[0], after[0]);
        assert_eq!(before[1], after[1]);
        assert_ne!(before[2], after[2]);
        let grown = chunk_views(&numbered(48), &[], None);
        let spill = chunk_views(&numbered(49), &[], None);
        assert_eq!(spill.len(), grown.len() + 1);
        assert_eq!(&spill[..3], &grown[..]);
    }

    #[test]
    fn an_append_lays_out_fewer_chunks_than_the_first_layout() {
        let mut rig = Rig::new(true, 400.0);
        rig.show(|_| code_block(numbered(64)));
        let first = rig.events();
        rig.show(|_| code_block(numbered(65)));
        let appended = rig.events() - first;
        assert!(
            appended < first,
            "append {appended} layouts vs first layout {first}"
        );
    }

    #[test]
    fn a_long_line_scrolls_inside_the_block_width() {
        let long = "x".repeat(600);
        let mut rig = Rig::new(true, 300.0);
        rig.show(|_| code_block(long.clone()).language("txt"));
        let block = rig.block();
        assert!(block.width() <= 300.0 + 0.01, "block {}", block.width());
        let viewport = rig.node("ViewportWidget")[0];
        assert!(viewport.x1 <= block.x1 + 0.01);
        let rich = rig.node("RichTextWidget")[0];
        assert!(
            rich.width() > 600.0,
            "a non-wrapping child must see an unbounded main axis: {}",
            rich.width()
        );
        let lh = rig.node("RichTextWidget")[0].height();
        assert!(lh < 30.0, "the line must not wrap, got a height of {lh} px");
    }

    #[test]
    fn the_scroll_view_is_as_tall_as_its_chunks() {
        let mut rig = Rig::new(true, 400.0);
        rig.show(|_| code_block(numbered(37)));
        let viewport = rig.node("ViewportWidget")[0];
        let stacked: f64 = rig.node("RichTextWidget").iter().map(Rect::height).sum();
        assert!(
            (viewport.height() - stacked).abs() < 0.5,
            "viewport {} vs chunks {stacked}",
            viewport.height()
        );
    }

    #[test]
    fn an_empty_and_a_blank_last_line_keep_their_height() {
        let mut rig = Rig::new(true, 400.0);
        rig.show(|_| code_block("a\n\n\n"));
        let blank = rig.node("ViewportWidget")[0].height();
        rig.show(|_| code_block("a\nb\nc"));
        let three = rig.node("ViewportWidget")[0].height();
        assert!((blank - three).abs() < 0.5, "{blank} vs {three}");
        rig.show(|_| code_block(""));
        assert!(rig.node("ViewportWidget")[0].height() > 5.0);
    }

    #[test]
    fn copy_writes_the_clipboard_and_calls_back_with_the_exact_text() {
        let code = "fn main() {\n\tprintln!(\"hi\");\n}\n";
        let mut rig = Rig::new(true, 400.0);
        rig.show(|_| {
            code_block(code).on_copy(|app: &mut App, text| app.copied.push(text.to_owned()))
        });
        let block = rig.block();
        rig.click(copy_point(block));
        assert_eq!(rig.app.copied, [code.to_owned()]);
        assert_eq!(rig.root.take_clipboard_write().as_deref(), Some(code));
    }

    #[test]
    fn a_press_outside_the_control_does_not_copy() {
        let mut rig = Rig::new(true, 400.0);
        rig.show(|_| code_block("x").on_copy(|app: &mut App, t| app.copied.push(t.into())));
        let block = rig.block();
        rig.click(Point::new(block.x0 + 6.0, block.y0 + 8.0));
        assert!(rig.app.copied.is_empty());
    }

    #[test]
    fn a_streaming_block_ignores_the_copy_control() {
        let mut rig = Rig::new(true, 400.0);
        rig.show(|_| {
            code_block("x")
                .streaming(true)
                .on_copy(|app: &mut App, t| app.copied.push(t.into()))
        });
        let block = rig.block();
        let height = block.height();
        rig.click(copy_point(block));
        assert!(rig.app.copied.is_empty());
        assert_eq!(rig.root.take_clipboard_write(), None);
        rig.show(|_| {
            code_block("x")
                .streaming(false)
                .on_copy(|app: &mut App, t| app.copied.push(t.into()))
        });
        assert_eq!(rig.block().height(), height);
        rig.click(copy_point(rig.block()));
        assert_eq!(rig.app.copied.len(), 1);
    }

    #[test]
    fn the_copied_confirmation_lasts_a_moment_then_clears() {
        let mut rig = Rig::new(true, 400.0);
        rig.show(|_| code_block("x"));
        let block = rig.block();
        rig.click(copy_point(block));
        assert!(rig.paint(0.0).needs_frame, "confirmation owes frames");
        assert!(rig.paint(0.5).needs_frame);
        assert!(!rig.paint(2.0).needs_frame, "confirmation cleared");
    }

    #[test]
    fn a_collapsed_block_shows_the_limit_and_the_toggle_reports() {
        let view = |expanded: bool| {
            move |_: &mut App| {
                code_block(numbered(30))
                    .max_visible_lines(5)
                    .expanded(expanded)
                    .on_toggle(|app: &mut App| app.toggles += 1)
            }
        };
        let mut rig = Rig::new(true, 400.0);
        rig.show(view(false));
        let short = rig.block();
        rig.show(view(true));
        let tall = rig.block();
        assert!(short.height() < tall.height() / 3.0);
        rig.show(view(false));
        rig.click(Point::new(short.x0 + 20.0, short.y1 - 6.0));
        assert_eq!(rig.app.toggles, 1);
        rig.show(|_| code_block(numbered(30)).max_visible_lines(5));
        let uncut = rig.block();
        assert!(uncut.height() > short.height(), "no toggle, no limit");
    }

    #[test]
    fn a_collapsed_body_covers_only_its_visible_lines() {
        let view = |expanded: bool| {
            move |_: &mut App| {
                code_block(numbered(30))
                    .max_visible_lines(5)
                    .expanded(expanded)
                    .on_toggle(|app: &mut App| app.toggles += 1)
            }
        };
        let mut rig = Rig::new(true, 400.0);
        rig.show(view(false));
        let block = rig.block();
        for chunk in rig.node("RichTextWidget") {
            assert!(
                chunk.y1 <= block.y1 + 0.01,
                "a chunk runs past {block:?}: {chunk:?}"
            );
        }
        let update = rig.root.semantics();
        let mut texts = Vec::new();
        for (_, node) in &update.nodes {
            if let Some(b) = node.bounds().filter(|_| node.role() != Role::Window) {
                let inside = b.x0 >= block.x0 - 0.01
                    && b.x1 <= block.x1 + 0.01
                    && b.y0 >= block.y0 - 0.01
                    && b.y1 <= block.y1 + 0.01;
                assert!(inside, "{:?} {b:?} outside {block:?}", node.role());
            }
            texts.extend(node.label().map(str::to_owned));
            texts.extend(node.value().map(str::to_owned));
        }
        let all = texts.join("\n");
        assert!(all.contains("line 5"), "the visible lines are exposed");
        assert!(!all.contains("line 6"), "a hidden line is not: {all}");
        rig.show(view(true));
        let update = rig.root.semantics();
        let expanded = update.nodes.iter().any(|(_, n)| {
            n.value()
                .or(n.label())
                .is_some_and(|t| t.contains("line 30"))
        });
        assert!(expanded, "expanded, every line is exposed");
    }

    #[test]
    fn equal_views_skip_the_rebuild() {
        let make = || {
            code_block::<()>("a\nb")
                .language("rs")
                .spans(vec![CodeSpan {
                    range: 0..1,
                    style: CodeTone::Keyword,
                }])
        };
        let (a, b) = (make(), make());
        let mut counter = 0u64;
        let mut ctx = BuildCtx::new(&mut counter);
        let mut widget = View::<()>::build(&a, &mut ctx);
        let flags = View::<()>::rebuild(&b, &a, &mut widget, &mut ctx);
        assert_eq!(flags, ChangeFlags::NONE);
        let edited = code_block::<()>("a\nb\nc").language("rs");
        let flags = View::<()>::rebuild(&edited, &b, &mut widget, &mut ctx);
        assert!(flags.contains(ChangeFlags::LAYOUT));
    }

    fn drag(rig: &mut Rig, from: Point, to: Point) {
        let steps = 12;
        let at = |i: usize| {
            let f = i as f64 / steps as f64;
            Point::new(from.x + (to.x - from.x) * f, from.y + (to.y - from.y) * f)
        };
        let mut send = |phase, position| {
            let e = InputEvent::PointerContact {
                pointer_id: frust::authoring::PointerId::touch(0),
                event: PointerEvent {
                    phase,
                    position,
                    button: PointerButton::Primary,
                },
            };
            rig.root.event(&mut rig.app, &e);
        };
        send(PointerPhase::Down, from);
        for i in 1..=steps {
            send(PointerPhase::Move, at(i));
        }
        send(PointerPhase::Up, to);
    }

    fn in_transcript(rig: &mut Rig) {
        rig.root.rebuild(
            &mut |_: &mut App| {
                let long = format!("{}\n{}", "y".repeat(500), numbered(30));
                any(frust::custom_scroll_view((
                    code_block::<App>(long),
                    frust::SizedBox::<App>(None, Some(2000.0)),
                )))
            },
            &mut rig.app,
        );
        rig.root
            .layout_with_text(rig.window, &mut rig.text as &mut dyn Any);
    }

    #[test]
    fn a_vertical_drag_on_the_body_scrolls_the_transcript_not_the_code() {
        let mut rig = Rig::new(true, 300.0);
        rig.window = Size::new(300.0, 300.0);
        in_transcript(&mut rig);
        let code_x = rig.node("RichTextWidget")[0].x0;
        drag(&mut rig, Point::new(150.0, 200.0), Point::new(150.0, 60.0));
        rig.root
            .layout_with_text(rig.window, &mut rig.text as &mut dyn Any);
        let y0 = rig.node("CodeBlockWidget")[0].y0;
        assert!(y0 < -20.0, "transcript moved: {y0}");
        assert!((rig.node("RichTextWidget")[0].x0 - code_x).abs() < 0.5);
    }

    #[test]
    fn a_horizontal_drag_on_the_body_scrolls_the_code_not_the_transcript() {
        let mut rig = Rig::new(true, 300.0);
        rig.window = Size::new(300.0, 300.0);
        in_transcript(&mut rig);
        let code_x = rig.node("RichTextWidget")[0].x0;
        drag(&mut rig, Point::new(250.0, 100.0), Point::new(60.0, 104.0));
        rig.root
            .layout_with_text(rig.window, &mut rig.text as &mut dyn Any);
        assert!(rig.node("RichTextWidget")[0].x0 < code_x - 20.0);
        let y0 = rig.node("CodeBlockWidget")[0].y0;
        assert!(y0 > -1.0, "transcript stays: {y0}");
    }

    #[derive(Default)]
    struct Count(usize);

    impl PaintScene for Count {
        fn fill_rect(&mut self, _o: Point, _s: Size, _c: Color) {}
        fn draw_text(&mut self, _o: Point, _t: &str) {}
        fn draw_glyph_run(&mut self, _run: GlyphRun) {
            self.0 += 1;
        }
    }

    #[test]
    fn themed_and_unthemed_paint_draw_text_without_panicking() {
        for themed in [true, false] {
            let mut rig = Rig::new(themed, 320.0);
            rig.show(|_| {
                code_block("let x = 1;\n// c").language("rust").spans(vec![
                    CodeSpan {
                        range: 0..3,
                        style: CodeTone::Keyword,
                    },
                    CodeSpan {
                        range: 11..15,
                        style: CodeTone::Comment,
                    },
                    CodeSpan {
                        range: 4..5,
                        style: CodeTone::Removed,
                    },
                ])
            });
            let mut scene = Count::default();
            rig.root.paint(&mut scene, FrameTime::ZERO);
            assert!(scene.0 > 0, "themed: {themed}");
        }
    }
}
