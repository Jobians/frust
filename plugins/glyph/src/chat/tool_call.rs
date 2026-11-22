//! [`tool_call`]: one tool call as a card: a one-line header (kind glyph,
//! title, status) that opens to the call's output.
//!
//! # Contract
//!
//! - **Controlled and non-reactive.** `.open(bool)` and `.on_toggle(..)` work
//!   like `crate::accordion`: a press on the header is reported, never
//!   self-applied. The default is closed.
//! - **One-line header, fixed height.** The kind glyph, the title (one line,
//!   end-elided; the full title is in the semantics label) and the status.
//!   The status area reserves the widest indicator, so a status update moves
//!   nothing in the transcript.
//! - **Status never relies on colour.** Pending is a muted dot, in progress
//!   the Glyph loader, completed a check, failed a cross and the word
//!   "failed" in the error colour.
//! - **Content.** [`ToolContent::Text`] is shown in a [`code_block`] with no
//!   language label (copy control and line cap included), a
//!   [`ToolContent::Diff`] in a [`diff_view`]. Their show-all rows need the
//!   matching `.on_output_toggle(..)` / `.on_diff_toggle(..)` callback, and
//!   the `.output_expanded(..)` / `.diff_expanded(..)` state is app-owned.
//! - **No content, no disclosure.** The header is then plain text and shows
//!   no marker; with content it is a button carrying the expanded state.

use std::rc::Rc;

use frust::authoring::text::{TextContext, TextLayout, TextOverflow, TextStyle};
use frust::authoring::{
    Action, AnyView, BoxConstraints, BuildCtx, ChangeFlags, ChildPod, Color, ErasedCallback,
    EventCtx, EventResult, InputEvent, LayoutCtx, PaintCtx, PaintScene, Point, PointerPhase, Role,
    SemanticsCtx, Size, View, Widget, any, build_child, erase_callback, rebuild_child,
    route_event_single, teardown_child,
};
use frust::{Theme, column};

use super::code_block::code_block;
use super::diff::diff_view;
use super::model::{DiffInput, ToolKind, ToolStatus};
use super::style;
use crate::dots::dots_loader;
use crate::press::presses;

/// Lines of text output shown before the show-all row, by default.
const DEFAULT_MAX_VISIBLE_LINES: usize = 12;
/// Width the three-dot loader occupies (three 8px dots and two 6px gaps; the
/// loader's own constants are private to `crate::dots`).
const LOADER_WIDTH: f64 = 34.0;

/// What a tool call shows when opened.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ToolContent {
    /// Plain output text (a command's output, a file excerpt, a search hit).
    Text(String),
    /// A file change.
    Diff(DiffInput),
}

impl From<String> for ToolContent {
    fn from(text: String) -> Self {
        Self::Text(text)
    }
}

impl From<&str> for ToolContent {
    fn from(text: &str) -> Self {
        Self::Text(text.to_owned())
    }
}

impl From<DiffInput> for ToolContent {
    fn from(diff: DiffInput) -> Self {
        Self::Diff(diff)
    }
}

type OnToggle<State> = Rc<dyn Fn(&mut State)>;
type OnCopy<State> = Rc<dyn Fn(&mut State, &str)>;

/// A declarative tool call card. See the [module docs](self).
pub struct ToolCallView<State: 'static> {
    kind: ToolKind,
    title: String,
    status: ToolStatus,
    open: bool,
    on_toggle: Option<OnToggle<State>>,
    contents: Vec<ToolContent>,
    on_copy: Option<OnCopy<State>>,
    max_visible_lines: usize,
    output_expanded: bool,
    on_output_toggle: Option<OnToggle<State>>,
    diff_expanded: bool,
    on_diff_toggle: Option<OnToggle<State>>,
}

impl<State: 'static> std::fmt::Debug for ToolCallView<State> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolCallView")
            .field("kind", &self.kind)
            .field("title", &self.title)
            .field("status", &self.status)
            .field("open", &self.open)
            .field("contents", &self.contents)
            .field("max_visible_lines", &self.max_visible_lines)
            .field("output_expanded", &self.output_expanded)
            .field("diff_expanded", &self.diff_expanded)
            .finish_non_exhaustive()
    }
}

/// Show one tool call: its `kind`, `title` and `status`, closed. The caller
/// owns every piece of state (status, open, expanded) and passes it on every
/// build.
pub fn tool_call<State: 'static>(
    kind: ToolKind,
    title: impl Into<String>,
    status: ToolStatus,
) -> ToolCallView<State> {
    ToolCallView {
        kind,
        title: title.into(),
        status,
        open: false,
        on_toggle: None,
        contents: Vec::new(),
        on_copy: None,
        max_visible_lines: DEFAULT_MAX_VISIBLE_LINES,
        output_expanded: false,
        on_output_toggle: None,
        diff_expanded: false,
        on_diff_toggle: None,
    }
}

impl<State: 'static> ToolCallView<State> {
    /// Set the (app-owned) open state.
    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    /// Fire on a press of the header of a card that has content. The widget
    /// never flips `open` itself.
    pub fn on_toggle(mut self, on_toggle: impl Fn(&mut State) + 'static) -> Self {
        self.on_toggle = Some(Rc::new(on_toggle));
        self
    }

    /// Add one content item. May be called several times.
    pub fn content(mut self, content: impl Into<ToolContent>) -> Self {
        self.contents.push(content.into());
        self
    }

    /// Add several content items, in order.
    pub fn contents(mut self, contents: impl IntoIterator<Item = ToolContent>) -> Self {
        self.contents.extend(contents);
        self
    }

    /// Run `on_copy` with the app state and the exact output text when the
    /// copy control of a text item is pressed (the clipboard is written first).
    pub fn on_copy(mut self, on_copy: impl Fn(&mut State, &str) + 'static) -> Self {
        self.on_copy = Some(Rc::new(on_copy));
        self
    }

    /// Lines of each text item shown before its show-all row (default 12).
    /// Needs [`Self::on_output_toggle`].
    pub fn max_visible_lines(mut self, lines: usize) -> Self {
        self.max_visible_lines = lines;
        self
    }

    /// Set the (app-owned) expanded state of the text items' show-all rows.
    pub fn output_expanded(mut self, expanded: bool) -> Self {
        self.output_expanded = expanded;
        self
    }

    /// Fire on a press of a text item's show-all row.
    pub fn on_output_toggle(mut self, on_toggle: impl Fn(&mut State) + 'static) -> Self {
        self.on_output_toggle = Some(Rc::new(on_toggle));
        self
    }

    /// Set the (app-owned) expanded state of the diff items (default
    /// `false`; it only matters for a long diff with `.on_diff_toggle(..)`).
    pub fn diff_expanded(mut self, expanded: bool) -> Self {
        self.diff_expanded = expanded;
        self
    }

    /// Fire on a press of a diff item's header or show-all row.
    pub fn on_diff_toggle(mut self, on_toggle: impl Fn(&mut State) + 'static) -> Self {
        self.on_diff_toggle = Some(Rc::new(on_toggle));
        self
    }

    /// Whether the card has anything to disclose.
    fn has_content(&self) -> bool {
        !self.contents.is_empty()
    }

    /// Whether the content is part of the tree.
    fn shows_body(&self) -> bool {
        self.open && self.has_content()
    }

    fn item_view(&self, content: &ToolContent) -> AnyView<State> {
        match content {
            ToolContent::Text(text) => {
                let mut v = code_block(text.as_str())
                    .max_visible_lines(self.max_visible_lines)
                    .expanded(self.output_expanded);
                if let Some(cb) = &self.on_output_toggle {
                    let cb = Rc::clone(cb);
                    v = v.on_toggle(move |s| cb(s));
                }
                if let Some(cb) = &self.on_copy {
                    let cb = Rc::clone(cb);
                    v = v.on_copy(move |s, t| cb(s, t));
                }
                any(v)
            }
            ToolContent::Diff(input) => {
                let mut v = diff_view(input.clone()).expanded(self.diff_expanded);
                if let Some(cb) = &self.on_diff_toggle {
                    let cb = Rc::clone(cb);
                    v = v.on_toggle(move |s| cb(s));
                }
                any(v)
            }
        }
    }

    fn body_view(&self) -> impl View<State> + use<State> {
        let items: Vec<AnyView<State>> = self.contents.iter().map(|c| self.item_view(c)).collect();
        column().gap(style::ROW_GAP).children(items)
    }
}

fn status_word(status: ToolStatus) -> &'static str {
    match status {
        ToolStatus::Pending => "pending",
        ToolStatus::InProgress => "in progress",
        ToolStatus::Completed => "completed",
        ToolStatus::Failed => "failed",
    }
}

fn kind_word(kind: ToolKind) -> &'static str {
    match kind {
        ToolKind::Read => "read",
        ToolKind::Edit => "edit",
        ToolKind::Delete => "delete",
        ToolKind::Move => "move",
        ToolKind::Search => "search",
        ToolKind::Execute => "execute",
        ToolKind::Think => "think",
        ToolKind::Fetch => "fetch",
        _ => "tool",
    }
}

/// The title on one line: line breaks and tabs become spaces.
fn one_line(title: &str) -> String {
    title
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// The text marking a status, if it has one (in progress shows the loader).
fn status_text(status: ToolStatus) -> Option<String> {
    match status {
        ToolStatus::Pending | ToolStatus::Completed => {
            Some(style::status_glyph(status).to_string())
        }
        ToolStatus::Failed => Some(format!("{} failed", style::status_glyph(status))),
        _ => None,
    }
}

const DISCLOSURE_OPEN: &str = "-";
const DISCLOSURE_CLOSED: &str = "+";

/// Shaped header text.
#[derive(Default)]
struct Labels {
    kind: Option<TextLayout>,
    title: Option<TextLayout>,
    status: Option<TextLayout>,
    disclosure: Option<TextLayout>,
}

/// The retained widget for a [`ToolCallView`].
pub struct ToolCallWidget {
    kind: ToolKind,
    title: String,
    status: ToolStatus,
    open: bool,
    has_content: bool,
    on_toggle: Option<ErasedCallback>,
    body: Option<ChildPod>,
    loader: Option<ChildPod>,
    labels: Labels,
    width: f64,
    header_h: f64,
    body_h: f64,
    status_w: f64,
    mark_w: f64,
    pressed: bool,
    captured: bool,
}

impl ToolCallWidget {
    fn hit(&self, p: Point) -> bool {
        self.has_content && p.y >= 0.0 && p.y < self.header_h && p.x >= 0.0 && p.x < self.width
    }

    /// Shape every header text and set the fixed header height and the
    /// widths of the status and disclosure areas.
    fn shape(&mut self, ctx: &mut LayoutCtx) {
        let theme = Theme::from_layout_ctx(ctx);
        let family = style::mono_family(theme);
        let active = style::ink(theme);
        let muted = style::muted(theme);
        let mk = |size: f32, color| TextStyle::new(size, color).with_family(family.clone());
        let title_style = mk(style::CODE_FONT_SIZE, active);
        let failed = style::status_color(theme, ToolStatus::Failed);
        let colors = [
            ToolStatus::Pending,
            ToolStatus::InProgress,
            ToolStatus::Completed,
            ToolStatus::Failed,
        ]
        .map(|s| (s, style::status_color(theme, s)));
        let color_of = |status| colors.iter().find(|c| c.0 == status).map_or(muted, |c| c.1);
        let label_style = |status| mk(style::LABEL_FONT_SIZE, color_of(status));
        let text = ctx.text_context::<TextContext>();

        let sample = text.layout("0", &title_style, None).size();
        let small = text
            .layout("0", &label_style(ToolStatus::Pending), None)
            .size();
        self.header_h = sample.height.max(small.height) + style::PAD_SMALL * 2.0;

        let kind_color = match self.status {
            ToolStatus::Failed => failed,
            _ => muted,
        };
        let glyph = style::kind_glyph(self.kind).to_string();
        self.labels.kind = Some(text.layout(&glyph, &mk(style::CODE_FONT_SIZE, kind_color), None));

        let mut status_w = LOADER_WIDTH;
        for s in [
            ToolStatus::Pending,
            ToolStatus::Completed,
            ToolStatus::Failed,
        ] {
            if let Some(t) = status_text(s) {
                status_w = status_w.max(text.layout(&t, &label_style(s), None).size().width);
            }
        }
        self.labels.status =
            status_text(self.status).map(|t| text.layout(&t, &label_style(self.status), None));

        let mark_w = if self.has_content {
            let mut make = |s: &str| text.layout(s, &mk(style::LABEL_FONT_SIZE, muted), None);
            let mark = if self.open {
                DISCLOSURE_OPEN
            } else {
                DISCLOSURE_CLOSED
            };
            let widest = make(DISCLOSURE_OPEN)
                .size()
                .width
                .max(make(DISCLOSURE_CLOSED).size().width);
            self.labels.disclosure = Some(make(mark));
            widest
        } else {
            self.labels.disclosure = None;
            0.0
        };

        let kind_w = sample.width;
        let gaps = style::PAD * 2.0 + style::PAD_SMALL * 2.0;
        let room = self.width - kind_w - status_w - mark_w - gaps;
        let line = one_line(&self.title);
        self.labels.title = Some(text.layout_bounded(
            &line,
            &title_style,
            Some(room.max(0.0) as f32),
            Some(1),
            TextOverflow::Ellipsis,
        ));
        self.status_w = status_w;
        self.mark_w = mark_w;
    }
}

impl<State: 'static> View<State> for ToolCallView<State> {
    type Element = ToolCallWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> ToolCallWidget {
        ToolCallWidget {
            kind: self.kind,
            title: self.title.clone(),
            status: self.status,
            open: self.open,
            has_content: self.has_content(),
            on_toggle: self.on_toggle.as_ref().map(erase_callback),
            body: self
                .shows_body()
                .then(|| build_child(&self.body_view(), ctx)),
            loader: (self.status == ToolStatus::InProgress)
                .then(|| build_child::<State, _>(&dots_loader(), ctx)),
            labels: Labels::default(),
            width: 0.0,
            header_h: 0.0,
            body_h: 0.0,
            status_w: 0.0,
            mark_w: 0.0,
            pressed: false,
            captured: false,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut ToolCallWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = ChangeFlags::NONE;
        element.on_toggle = self.on_toggle.as_ref().map(erase_callback);

        match (prev.shows_body(), self.shows_body(), element.body.as_mut()) {
            (true, true, Some(pod)) => {
                flags |= rebuild_child(&prev.body_view(), &self.body_view(), pod, ctx);
            }
            (false, true, _) => {
                element.body = Some(build_child(&self.body_view(), ctx));
                flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
            }
            (true, false, Some(pod)) => {
                teardown_child(&prev.body_view(), pod, ctx);
                element.body = None;
                flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
            }
            _ => {}
        }

        let was_loading = prev.status == ToolStatus::InProgress;
        let is_loading = self.status == ToolStatus::InProgress;
        if was_loading != is_loading {
            if is_loading {
                element.loader = Some(build_child::<State, _>(&dots_loader(), ctx));
            } else if let Some(mut pod) = element.loader.take() {
                teardown_child::<State, _>(&dots_loader(), &mut pod, ctx);
            }
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }

        if prev.kind != self.kind
            || prev.title != self.title
            || prev.status != self.status
            || prev.open != self.open
            || prev.has_content() != self.has_content()
        {
            element.kind = self.kind;
            element.title = self.title.clone();
            element.status = self.status;
            element.open = self.open;
            element.has_content = self.has_content();
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        flags
    }

    fn teardown(&self, element: &mut ToolCallWidget, ctx: &mut BuildCtx<'_>) {
        if let Some(pod) = element.body.as_mut() {
            teardown_child(&self.body_view(), pod, ctx);
        }
        if let Some(pod) = element.loader.as_mut() {
            teardown_child::<State, _>(&dots_loader(), pod, ctx);
        }
    }
}

impl Widget for ToolCallWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        self.width = style::width_of(bc);
        self.shape(ctx);

        if let Some(pod) = self.loader.as_mut() {
            let size = pod.layout_child(
                ctx,
                &BoxConstraints::loose(Size::new(LOADER_WIDTH, self.header_h)),
            );
            let right = self.width
                - style::PAD
                - if self.has_content {
                    self.mark_w + style::PAD_SMALL
                } else {
                    0.0
                };
            pod.set_origin(Point::new(
                right - size.width,
                (self.header_h - size.height) / 2.0,
            ));
        }

        self.body_h = 0.0;
        if let Some(pod) = self.body.as_mut() {
            let inner = (self.width - style::PAD_SMALL * 2.0).max(0.0);
            let body_bc =
                BoxConstraints::new(Size::new(inner, 0.0), Size::new(inner, f64::INFINITY));
            let size = pod.layout_child(ctx, &body_bc);
            pod.set_origin(Point::new(
                style::PAD_SMALL,
                self.header_h + style::PAD_SMALL,
            ));
            self.body_h = size.height;
        }
        let height = if self.body.is_some() {
            self.header_h + style::PAD_SMALL * 2.0 + self.body_h
        } else {
            self.header_h
        };
        bc.constrain(Size::new(self.width, height))
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let theme = Theme::from_paint_ctx(ctx);
        let (surface, border) = (style::surface(theme), style::border(theme));
        let o = ctx.origin();
        let size = ctx.size();
        scene.fill_rounded_rect(o, size, style::RADIUS, surface);

        let centred = |l: &TextLayout| (self.header_h - l.size().height) / 2.0;
        let mut x = style::PAD;
        if let Some(l) = &self.labels.kind {
            style::paint_layout(l, Point::new(o.x + x, o.y + centred(l)), scene);
            x += l.size().width + style::PAD_SMALL;
        }
        if let Some(l) = &self.labels.title {
            style::paint_layout(l, Point::new(o.x + x, o.y + centred(l)), scene);
        }
        let mut right = self.width - style::PAD;
        if let Some(l) = &self.labels.disclosure {
            let w = l.size().width;
            style::paint_layout(l, Point::new(o.x + right - w, o.y + centred(l)), scene);
            right -= self.mark_w + style::PAD_SMALL;
        }
        if let Some(l) = &self.labels.status {
            let w = l.size().width;
            style::paint_layout(l, Point::new(o.x + right - w, o.y + centred(l)), scene);
        }
        if self.pressed {
            let wash = Color::new([1.0, 1.0, 1.0, 0.06]);
            scene.fill_rounded_rect(o, Size::new(self.width, self.header_h), style::RADIUS, wash);
        }
        if let Some(pod) = self.loader.as_mut() {
            pod.paint_child(ctx, scene);
        }
        if let Some(pod) = self.body.as_mut() {
            scene.stroke_line(
                Point::new(o.x, o.y + self.header_h),
                Point::new(o.x + size.width, o.y + self.header_h),
                style::BORDER_WIDTH,
                border,
            );
            pod.paint_child(ctx, scene);
        }
        style::stroke_outline(scene, o, size, style::RADIUS, border);
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        let route = |body: &mut Option<ChildPod>, ctx: &mut EventCtx, event: &InputEvent| match body
        {
            Some(pod) => route_event_single(pod, ctx, event),
            None => EventResult::Ignored,
        };
        let InputEvent::Pointer(p) = event else {
            return route(&mut self.body, ctx, event);
        };
        match p.phase {
            PointerPhase::Down => {
                if presses(p) && self.hit(p.position) {
                    self.captured = true;
                    self.pressed = true;
                    ctx.capture_pointer();
                    ctx.request_redraw();
                    return EventResult::Handled;
                }
                route(&mut self.body, ctx, event)
            }
            PointerPhase::Move => {
                if !self.captured {
                    return route(&mut self.body, ctx, event);
                }
                let inside = self.hit(p.position);
                if inside != self.pressed {
                    self.pressed = inside;
                    ctx.request_redraw();
                }
                EventResult::Handled
            }
            PointerPhase::Up => {
                if !std::mem::take(&mut self.captured) {
                    return route(&mut self.body, ctx, event);
                }
                self.pressed = false;
                if self.hit(p.position)
                    && let Some(cb) = self.on_toggle.as_mut()
                {
                    cb(ctx);
                }
                ctx.request_redraw();
                EventResult::Handled
            }
            PointerPhase::Cancel => {
                if std::mem::take(&mut self.captured) {
                    self.pressed = false;
                    ctx.request_redraw();
                    return EventResult::Handled;
                }
                route(&mut self.body, ctx, event)
            }
        }
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        let group = format!("{} tool call", kind_word(self.kind));
        let label = format!("{} ({})", self.title, status_word(self.status));
        ctx.push_container(
            Role::Group,
            |node| node.set_label(group.as_str()),
            |ctx| {
                let origin = ctx.origin();
                let bounds = frust::accesskit::Rect {
                    x0: origin.x,
                    y0: origin.y,
                    x1: origin.x + self.width,
                    y1: origin.y + self.header_h,
                };
                if self.has_content {
                    ctx.push_node(Role::Button, |node| {
                        node.set_label(label.as_str());
                        node.set_expanded(self.open);
                        node.add_action(Action::Click);
                        node.set_bounds(bounds);
                    });
                } else {
                    ctx.push_node(Role::Label, |node| {
                        node.set_label(label.as_str());
                        node.set_bounds(bounds);
                    });
                }
                if let Some(pod) = &self.body {
                    pod.semantics_child(ctx);
                }
            },
        );
    }

    frust::authoring::visit_children!(body, loader);
}

#[cfg(test)]
mod tests {
    use super::*;
    use frust::FrameTime;
    use frust::authoring::{DiscardScene, PointerButton, PointerEvent, Rect};
    use frust_core::RenderRoot;
    use std::any::Any;

    const KINDS: [ToolKind; 9] = [
        ToolKind::Read,
        ToolKind::Edit,
        ToolKind::Delete,
        ToolKind::Move,
        ToolKind::Search,
        ToolKind::Execute,
        ToolKind::Think,
        ToolKind::Fetch,
        ToolKind::Other,
    ];
    const STATUSES: [ToolStatus; 4] = [
        ToolStatus::Pending,
        ToolStatus::InProgress,
        ToolStatus::Completed,
        ToolStatus::Failed,
    ];

    #[derive(Default)]
    struct App {
        toggles: usize,
    }

    type Root = RenderRoot<App, AnyView<App>>;

    struct Rig {
        root: Root,
        app: App,
        text: TextContext,
        window: Size,
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
                window: Size::new(window_w, 4000.0),
            }
        }

        fn show(&mut self, mut view: impl FnMut() -> ToolCallView<App>) {
            self.root
                .rebuild(&mut |_: &mut App| any(view()), &mut self.app);
            self.root
                .layout_with_text(self.window, &mut self.text as &mut dyn Any);
        }

        fn block(&self) -> Rect {
            self.root.inspect()[0].bounds
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

        fn buttons(&self) -> Vec<Option<bool>> {
            self.root
                .semantics()
                .nodes
                .iter()
                .filter(|(_, n)| n.role() == Role::Button)
                .map(|(_, n)| n.is_expanded())
                .collect()
        }
    }

    fn diff() -> DiffInput {
        DiffInput {
            path: "a.rs".into(),
            old_text: Some("one\ntwo\n".into()),
            new_text: "one\n2\n".into(),
        }
    }

    #[test]
    fn every_kind_and_status_has_the_same_header_height_at_both_widths() {
        for width in [360.0, 1024.0] {
            let mut heights = Vec::new();
            for kind in KINDS {
                for status in STATUSES {
                    let mut rig = Rig::new(true, width);
                    rig.show(|| tool_call(kind, "Read src/main.rs", status));
                    let b = rig.block();
                    assert!(b.width() <= width + 0.01);
                    heights.push(b.height());
                }
            }
            assert!(heights[0] > 0.0);
            assert!(
                heights.iter().all(|h| (h - heights[0]).abs() < 0.01),
                "{heights:?}"
            );
        }
    }

    #[test]
    fn a_status_update_keeps_the_height_and_closed_content_adds_none() {
        let mut rig = Rig::new(true, 360.0);
        rig.show(|| tool_call(ToolKind::Execute, "ls", ToolStatus::InProgress));
        let plain = rig.block().height();
        rig.show(|| tool_call(ToolKind::Execute, "ls", ToolStatus::Failed).content("x"));
        assert!((rig.block().height() - plain).abs() < 0.01);
    }

    #[test]
    fn long_and_multiline_titles_stay_on_one_line_inside_the_width() {
        let mut one = Rig::new(true, 360.0);
        one.show(|| tool_call(ToolKind::Read, "a", ToolStatus::Completed));
        for title in ["p/".repeat(300), "line one\nline two\n\tthree".to_owned()] {
            let mut rig = Rig::new(true, 360.0);
            rig.show(|| tool_call(ToolKind::Read, title.clone(), ToolStatus::Failed).content("x"));
            assert!((rig.block().height() - one.block().height()).abs() < 0.01);
            assert!(rig.block().width() <= 360.0 + 0.01);
        }
    }

    #[test]
    fn disclosure_only_with_content() {
        let mut rig = Rig::new(true, 360.0);
        rig.show(|| tool_call(ToolKind::Read, "t", ToolStatus::Completed));
        assert!(rig.buttons().is_empty());
        rig.show(|| tool_call(ToolKind::Read, "t", ToolStatus::Completed).content("out"));
        assert_eq!(rig.buttons(), [Some(false)]);
        rig.show(|| {
            tool_call(ToolKind::Read, "t", ToolStatus::Completed)
                .content("out")
                .open(true)
        });
        assert_eq!(rig.buttons()[0], Some(true));
    }

    #[test]
    fn a_header_press_reports_a_toggle_only_with_content_and_never_self_applies() {
        let mut rig = Rig::new(true, 360.0);
        rig.show(|| {
            tool_call(ToolKind::Read, "t", ToolStatus::Completed)
                .content("out")
                .on_toggle(|a: &mut App| a.toggles += 1)
        });
        let before = rig.block();
        rig.click(Point::new(40.0, before.height() / 2.0));
        assert_eq!(rig.app.toggles, 1);
        assert!((rig.block().height() - before.height()).abs() < 0.01);

        let mut bare = Rig::new(true, 360.0);
        bare.show(|| {
            tool_call(ToolKind::Read, "t", ToolStatus::Completed)
                .on_toggle(|a: &mut App| a.toggles += 1)
        });
        bare.click(Point::new(40.0, 10.0));
        assert_eq!(bare.app.toggles, 0);
    }

    #[test]
    fn open_with_text_and_with_a_diff_grows_the_card() {
        let mut rig = Rig::new(true, 360.0);
        rig.show(|| tool_call(ToolKind::Execute, "t", ToolStatus::Completed).content("a\nb\nc"));
        let closed = rig.block().height();
        assert!(!rig.has_node("CodeBlockWidget"));
        rig.show(|| {
            tool_call(ToolKind::Execute, "t", ToolStatus::Completed)
                .content("a\nb\nc")
                .open(true)
        });
        assert!(rig.has_node("CodeBlockWidget"));
        assert!(rig.block().height() > closed);

        let mut d = Rig::new(true, 360.0);
        d.show(|| {
            tool_call(ToolKind::Edit, "t", ToolStatus::Completed)
                .content(diff())
                .content("note")
                .open(true)
        });
        assert!(d.has_node("DiffWidget"));
        assert!(d.has_node("CodeBlockWidget"));
        assert!(d.block().width() <= 360.0 + 0.01);
        // Closing again drops the content from the tree.
        d.show(|| {
            tool_call(ToolKind::Edit, "t", ToolStatus::Completed)
                .content(diff())
                .open(false)
        });
        assert!(!d.has_node("DiffWidget"));
    }

    #[test]
    fn many_text_lines_are_capped_until_expanded() {
        let out: String = (0..100).map(|i| format!("line {i}\n")).collect();
        let build = |expanded| {
            let out = out.clone();
            move || {
                tool_call(ToolKind::Execute, "t", ToolStatus::Completed)
                    .content(out.clone())
                    .open(true)
                    .max_visible_lines(5)
                    .on_output_toggle(|a: &mut App| a.toggles += 1)
                    .output_expanded(expanded)
            }
        };
        let mut rig = Rig::new(true, 360.0);
        rig.show(build(false));
        let capped = rig.block().height();
        rig.show(build(true));
        assert!(rig.block().height() > capped * 2.0);
    }

    #[test]
    fn equal_views_do_no_text_work() {
        let mut rig = Rig::new(true, 360.0);
        let make = || {
            tool_call(ToolKind::Execute, "t", ToolStatus::Completed)
                .content("a\nb")
                .content(diff())
                .open(true)
        };
        rig.show(make);
        let before = rig.events();
        rig.show(make);
        assert_eq!(rig.events(), before);
    }

    #[test]
    fn the_loader_follows_the_status() {
        let mut rig = Rig::new(true, 360.0);
        rig.show(|| tool_call(ToolKind::Read, "t", ToolStatus::InProgress));
        assert!(rig.has_node("DotsLoaderWidget"));
        rig.show(|| tool_call(ToolKind::Read, "t", ToolStatus::Completed));
        assert!(!rig.has_node("DotsLoaderWidget"));
        rig.show(|| tool_call(ToolKind::Read, "t", ToolStatus::InProgress));
        assert!(rig.has_node("DotsLoaderWidget"));
    }

    #[test]
    fn the_header_label_names_the_full_title_and_status() {
        let mut rig = Rig::new(true, 200.0);
        let title = "x/".repeat(100);
        rig.show(|| tool_call(ToolKind::Read, title.clone(), ToolStatus::Failed).content("o"));
        let update = rig.root.semantics();
        assert!(update.nodes.iter().any(|(_, n)| n.role() == Role::Button
            && n.label() == Some(format!("{title} (failed)").as_str())));
    }

    #[test]
    fn themed_and_unthemed_paint_do_not_panic() {
        for themed in [true, false] {
            for status in STATUSES {
                let mut rig = Rig::new(themed, 360.0);
                rig.show(|| {
                    tool_call(ToolKind::Edit, "t", status)
                        .content(diff())
                        .content("out")
                        .open(true)
                });
                rig.root
                    .paint(&mut DiscardScene, FrameTime::from_nanos(1_000_000));
            }
        }
    }
}
