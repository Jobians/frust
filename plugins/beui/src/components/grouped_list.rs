//! Grouped lists: an inset [`grouped_list`] of [`list_row`]s under a [`section_header`].
//!
//! The settings-style list a phone app is built from. [`grouped_list`] is one rounded
//! surface that clips its rows and draws a hairline between neighbours, inset to the
//! title's leading edge so the line starts where the text does, not under the leading
//! icon. [`list_row`] carries a title with optional `leading` view, `subtitle`,
//! `value`, `trailing` view and a drawn `chevron`; it is pressable when it has an
//! `on_press`. [`section_header`] is the small uppercase label above a group.
//!
//! # Metrics
//!
//! Everything is read at layout from [`BeuiMetrics`]. The metrics carry no row-height
//! token, so a row is at least [`BeuiMetrics::height_md`] tall and at least
//! [`BeuiMetrics::min_touch_target`]; the horizontal inset and the list radius are
//! `padding_x_input` and `radius_list`. Text follows the OS text scale (see
//! [`crate::text`]); a row grows with it.
//!
//! # Truncation
//!
//! A long title or subtitle is cut to one line with an ellipsis. The value is
//! measured first and keeps its width (up to 60 % of the row's content width); the
//! title takes what is left. This module keeps its own one-line ellipsis run because
//! [`crate::text::LabelRun`] shapes at an unbounded width.

use std::rc::Rc;

use frust::Theme;
use frust::authoring::text::{LocaleStamp, TextContext, TextLayout, TextOverflow, TextStyle};
use frust::authoring::{
    AnyView, BezPath, BoxConstraints, Brush, BuildCtx, ChangeFlags, ChildPod, Color,
    ErasedCallback, EventCtx, EventResult, InputEvent, LayoutCtx, PaintCtx, PaintScene, Point,
    PointerPhase, Role, SemanticsCtx, Size, View, Widget, any, build_child, erase_callback,
    rebuild_child, rebuild_children, route_event, route_event_single, teardown_child,
};

use crate::press::{draw_focus_ring, inside, is_activation_key, presses};
use crate::style::{ACTIVE_CURSOR, FOCUS_RING_WIDTH, SPACING_UNIT, with_alpha};
use crate::text::{SHAPING_INK, ThemeTextType, themed_style};
use crate::tokens::{BeuiMetrics, BeuiTokens, color_scheme_light};

/// The pressed wash laid over a row, as a share of the ink colour.
const PRESS_WASH_ALPHA: f32 = 0.1;
/// The most of a row's content width the value may claim before it truncates.
const VALUE_MAX_SHARE: f64 = 0.6;
/// A chevron's width and height, and its stroke.
const CHEVRON_SIZE: Size = Size::new(8.0, 14.0);
const CHEVRON_STROKE: f64 = 2.0;
/// The gap between a row's parts, and its vertical padding.
const PART_GAP: f64 = 3.0 * SPACING_UNIT;
const ROW_PAD_Y: f64 = 2.0 * SPACING_UNIT;
/// How far the focus ring's outer edge sits inside a row's bounds. The group clips its
/// rows, so the ring cannot sit outside them the way a free-standing control's does.
const RING_INSET: f64 = 2.0;

/// A cached one-line text run whose ink is applied at paint time and which cuts
/// itself to a width with an ellipsis. Private copy: `LabelRun` has no bound.
struct EllipsisRun {
    content: String,
    layout: Option<TextLayout>,
    key: Option<(TextStyle, Option<u32>)>,
    locale: LocaleStamp,
}

impl EllipsisRun {
    fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            layout: None,
            key: None,
            locale: LocaleStamp::default(),
        }
    }

    fn set_content(&mut self, content: &str) -> bool {
        if self.content == content {
            return false;
        }
        self.content = content.to_owned();
        self.layout = None;
        true
    }

    /// Shape at `max_width` (unbounded for `None`), one line, ellipsis on overflow.
    fn layout(&mut self, ctx: &mut LayoutCtx, style: &TextStyle, max_width: Option<f64>) -> Size {
        let width = max_width.map(|w| w.max(0.0) as f32);
        let key = (style.clone(), width.map(f32::to_bits));
        if let Some(cached) = &self.layout
            && self.key.as_ref() == Some(&key)
            && self.locale.is_current(style.locale.as_deref())
        {
            return cached.size();
        }
        let stamp = LocaleStamp::for_style(style.locale.as_deref());
        let laid = ctx.text_context::<TextContext>().layout_bounded(
            &self.content,
            style,
            width,
            Some(1),
            TextOverflow::Ellipsis,
        );
        let size = laid.size();
        self.layout = Some(laid);
        self.key = Some(key);
        self.locale = stamp;
        size
    }

    #[cfg(test)]
    fn size(&self) -> Size {
        self.layout.as_ref().map_or(Size::ZERO, TextLayout::size)
    }

    fn paint(&self, origin: Point, color: Color, scene: &mut dyn PaintScene) {
        if let Some(layout) = &self.layout {
            for mut run in layout.to_scene_runs(origin) {
                run.brush = Brush::Solid(color);
                scene.draw_glyph_run(run);
            }
        }
    }
}

/// The text style a list part shapes with: the catalog's sans at `size`, themed for
/// `role`, scaled by the OS text scale.
fn part_style(ctx: &LayoutCtx, size: f64, role: ThemeTextType) -> TextStyle {
    let base = TextStyle::new(size as f32, SHAPING_INK).with_family(crate::tokens::sans_family());
    themed_style(base, role, Theme::from_layout_ctx(ctx))
}

/// The scheme roles a list paints with.
#[derive(Clone, Copy)]
struct ListColors {
    surface: Color,
    ink: Color,
    secondary: Color,
    separator: Color,
    destructive: Color,
}

impl ListColors {
    fn resolve(theme: Option<&Theme>) -> Self {
        let scheme = theme.map(Theme::scheme);
        macro_rules! role {
            ($role:ident) => {
                scheme.map_or(color_scheme_light().$role, |s| s.$role)
            };
        }
        Self {
            surface: role!(surface_container),
            ink: role!(on_surface),
            secondary: role!(on_surface_variant),
            separator: role!(outline_variant),
            destructive: role!(error),
        }
    }
}

// ---- The group -------------------------------------------------------------

/// The retained widget behind [`grouped_list`]: a clipped, rounded surface stacking
/// its row children with an inset hairline between neighbours.
pub struct GroupedListWidget {
    rows: Vec<ChildPod>,
    /// `(y, inset)` of each separator, from the last layout.
    separators: Vec<(f64, f64)>,
    size: Size,
}

impl GroupedListWidget {
    /// How many rows this group holds.
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }
}

/// The title inset a row pod reports, or `fallback` for a child that is not a row.
fn title_inset_of(pod: &mut ChildPod, fallback: f64) -> f64 {
    let widget = pod.widget_mut();
    if let Some(row) = widget.downcast_mut::<ListRowWidget>() {
        return row.title_inset;
    }
    if let Some(boxed) = widget.downcast_mut::<Box<dyn Widget>>()
        && let Some(row) = boxed.downcast_mut::<ListRowWidget>()
    {
        return row.title_inset;
    }
    fallback
}

impl Widget for GroupedListWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let m = BeuiMetrics::resolve(Theme::from_layout_ctx(ctx));
        let mut width = bc.max().width;
        if !width.is_finite() {
            // No bound: measure every row loose, then lay them at the widest.
            let loose = BoxConstraints::new(Size::ZERO, Size::new(f64::INFINITY, f64::INFINITY));
            width = self
                .rows
                .iter_mut()
                .map(|pod| pod.layout_child(ctx, &loose).width)
                .fold(0.0, f64::max);
        }
        let row_bc = BoxConstraints::new(Size::new(width, 0.0), Size::new(width, f64::INFINITY));
        self.separators.clear();
        let mut y = 0.0_f64;
        let count = self.rows.len();
        for (index, pod) in self.rows.iter_mut().enumerate() {
            let size = pod.layout_child(ctx, &row_bc);
            pod.set_origin(Point::new(0.0, y));
            y += size.height;
            if index + 1 < count {
                let inset = title_inset_of(pod, m.padding_x_input);
                self.separators.push((y - m.border_width, inset));
            }
        }
        self.size = bc.constrain(Size::new(width, y));
        self.size
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let theme = Theme::from_paint_ctx(ctx);
        let colors = ListColors::resolve(theme);
        let m = BeuiMetrics::resolve(theme);
        let origin = ctx.origin();
        let size = self.size;
        scene.push_clip_rounded(origin, size, m.radius_list);
        scene.fill_rounded_rect(origin, size, m.radius_list, colors.surface);
        for pod in &mut self.rows {
            pod.paint_child(ctx, scene);
        }
        for &(y, inset) in &self.separators {
            scene.fill_rect(
                origin + kurbo::Vec2::new(inset, y),
                Size::new((size.width - inset).max(0.0), m.border_width),
                colors.separator,
            );
        }
        scene.pop_clip();
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        route_event(&mut self.rows, ctx, event)
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        ctx.push_container(
            Role::List,
            |_| {},
            |ctx| {
                for pod in &self.rows {
                    pod.semantics_child(ctx);
                }
            },
        );
    }

    frust::authoring::visit_children!(rows);
}

/// An inset group of rows. See the [module docs](self).
pub struct GroupedListView<State: 'static> {
    rows: Vec<AnyView<State>>,
}

/// Group `rows` into one inset list.
pub fn grouped_list<State: 'static, V: View<State>>(
    rows: impl IntoIterator<Item = V>,
) -> GroupedListView<State> {
    GroupedListView {
        rows: rows.into_iter().map(any).collect(),
    }
}

impl<State: 'static> View<State> for GroupedListView<State> {
    type Element = GroupedListWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> GroupedListWidget {
        GroupedListWidget {
            rows: self.rows.iter().map(|row| build_child(row, ctx)).collect(),
            separators: Vec::new(),
            size: Size::ZERO,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut GroupedListWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        rebuild_children(
            &prev.rows,
            &self.rows,
            &mut element.rows,
            ctx,
            |v| v,
            |_| None,
        )
    }

    fn teardown(&self, element: &mut GroupedListWidget, ctx: &mut BuildCtx<'_>) {
        for (view, pod) in self.rows.iter().zip(element.rows.iter_mut()) {
            teardown_child(view, pod, ctx);
        }
    }
}

// ---- The row ---------------------------------------------------------------

/// The retained widget behind [`list_row`].
pub struct ListRowWidget {
    title: EllipsisRun,
    subtitle: Option<EllipsisRun>,
    value: Option<EllipsisRun>,
    leading: Option<ChildPod>,
    trailing: Option<ChildPod>,
    chevron: bool,
    destructive: bool,
    on_press: Option<ErasedCallback>,
    pressed: bool,
    captured: bool,
    /// Where the title starts, from the last layout: the separator inset.
    title_inset: f64,
    title_at: Point,
    subtitle_at: Point,
    value_at: Point,
    chevron_at: Point,
    size: Size,
}

impl ListRowWidget {
    /// Whether the leading or trailing child holds focus, so the row is only on the path.
    fn child_holds_focus(&self) -> bool {
        [&self.leading, &self.trailing]
            .into_iter()
            .flatten()
            .any(ChildPod::holds_live_focus)
    }

    /// Where this row's title starts, in row-local x, from the last layout.
    pub fn title_inset(&self) -> f64 {
        self.title_inset
    }
}

impl Widget for ListRowWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let m = BeuiMetrics::resolve(Theme::from_layout_ctx(ctx));
        let pad = m.padding_x_input;
        let min_height = m.height_md.max(m.min_touch_target);
        let max_width = bc.max().width;
        let bounded = max_width.is_finite();

        let title_style = part_style(ctx, m.text_base, ThemeTextType::BodyLarge);
        let aux_style = part_style(ctx, m.text_sm, ThemeTextType::BodyMedium);

        let slot_bc = BoxConstraints::new(
            Size::ZERO,
            Size::new(
                if bounded {
                    max_width / 3.0
                } else {
                    f64::INFINITY
                },
                f64::INFINITY,
            ),
        );
        let leading = self
            .leading
            .as_mut()
            .map_or(Size::ZERO, |pod| pod.layout_child(ctx, &slot_bc));
        let trailing = self
            .trailing
            .as_mut()
            .map_or(Size::ZERO, |pod| pod.layout_child(ctx, &slot_bc));
        let chevron = if self.chevron {
            CHEVRON_SIZE
        } else {
            Size::ZERO
        };

        let gap_if = |size: Size| if size.width > 0.0 { PART_GAP } else { 0.0 };
        let fixed = pad * 2.0
            + leading.width
            + gap_if(leading)
            + trailing.width
            + gap_if(trailing)
            + chevron.width
            + gap_if(chevron);
        let content = if bounded {
            (max_width - fixed).max(0.0)
        } else {
            f64::INFINITY
        };

        // The value is measured first and keeps its width; the title gets the rest.
        let value_cap = bounded.then_some(content * VALUE_MAX_SHARE);
        let value = self
            .value
            .as_mut()
            .map_or(Size::ZERO, |run| run.layout(ctx, &aux_style, value_cap));
        let text_width = bounded.then(|| (content - value.width - gap_if(value)).max(0.0));
        let title = self.title.layout(ctx, &title_style, text_width);
        let subtitle = self
            .subtitle
            .as_mut()
            .map_or(Size::ZERO, |run| run.layout(ctx, &aux_style, text_width));

        let text_height = title.height + subtitle.height;
        let inner = text_height
            .max(leading.height)
            .max(trailing.height)
            .max(value.height)
            .max(chevron.height);
        let height = min_height.max(inner + ROW_PAD_Y * 2.0);
        let width = if bounded {
            max_width
        } else {
            fixed + title.width.max(subtitle.width) + value.width + gap_if(value)
        };
        let width = bc.constrain(Size::new(width, height)).width;

        let centred = |h: f64| (height - h) / 2.0;
        let mut x = pad;
        if let Some(pod) = &mut self.leading {
            pod.set_origin(Point::new(x, centred(leading.height)));
            x += leading.width + gap_if(leading);
        }
        self.title_inset = x;
        self.title_at = Point::new(x, centred(text_height));
        self.subtitle_at = Point::new(x, self.title_at.y + title.height);

        let mut right = width - pad;
        if self.chevron {
            right -= chevron.width;
            self.chevron_at = Point::new(right, centred(chevron.height));
            right -= PART_GAP;
        }
        if let Some(pod) = &mut self.trailing {
            right -= trailing.width;
            pod.set_origin(Point::new(right, centred(trailing.height)));
            right -= PART_GAP;
        }
        self.value_at = Point::new(right - value.width, centred(value.height));

        self.size = bc.constrain(Size::new(width, height));
        self.size
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let theme = Theme::from_paint_ctx(ctx);
        let colors = ListColors::resolve(theme);
        let radius = BeuiMetrics::resolve(theme).radius_list;
        let ring = BeuiTokens::resolve_ring(None, theme);
        let origin = ctx.origin();
        if self.pressed {
            scene.fill_rect(origin, self.size, with_alpha(colors.ink, PRESS_WASH_ALPHA));
        }
        if let Some(pod) = &mut self.leading {
            pod.paint_child(ctx, scene);
        }
        if let Some(pod) = &mut self.trailing {
            pod.paint_child(ctx, scene);
        }
        let title_ink = if self.destructive {
            colors.destructive
        } else {
            colors.ink
        };
        self.title
            .paint(origin + self.title_at.to_vec2(), title_ink, scene);
        if let Some(run) = &self.subtitle {
            run.paint(origin + self.subtitle_at.to_vec2(), colors.secondary, scene);
        }
        if let Some(run) = &self.value {
            run.paint(origin + self.value_at.to_vec2(), colors.secondary, scene);
        }
        if self.chevron {
            let mut path = BezPath::new();
            path.move_to(Point::new(0.0, 0.0));
            path.line_to(Point::new(CHEVRON_SIZE.width, CHEVRON_SIZE.height / 2.0));
            path.line_to(Point::new(0.0, CHEVRON_SIZE.height));
            scene.stroke_path(
                origin + self.chevron_at.to_vec2(),
                &path,
                CHEVRON_STROKE,
                &Brush::Solid(colors.secondary),
            );
        }
        if self.on_press.is_some() && ctx.is_focused_for_ring() && !self.child_holds_focus() {
            // A negative offset draws the ring inward; the radius follows the group's
            // corner so the first and last row's ring stays inside the rounded clip.
            draw_focus_ring(
                scene,
                origin,
                self.size,
                radius.max(RING_INSET + FOCUS_RING_WIDTH),
                -(RING_INSET + FOCUS_RING_WIDTH),
                ring,
            );
        }
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        if !self.captured {
            for pod in [&mut self.leading, &mut self.trailing]
                .into_iter()
                .flatten()
            {
                if route_event_single(pod, ctx, event) == EventResult::Handled {
                    return EventResult::Handled;
                }
            }
        }
        if self.on_press.is_none() {
            return EventResult::Ignored;
        }
        let p = match event {
            InputEvent::Pointer(p) => p,
            InputEvent::Key(key) => {
                // Only when the row itself holds focus: a focused leading or trailing
                // child already had the key above and keeps it.
                if !is_activation_key(key) || self.child_holds_focus() {
                    return EventResult::Ignored;
                }
                if let Some(on_press) = &mut self.on_press {
                    on_press(ctx);
                }
                return EventResult::Handled;
            }
            _ => return EventResult::Ignored,
        };
        let size = ctx.size();
        match p.phase {
            PointerPhase::Down => {
                if !presses(p) || !inside(p.position, size) {
                    return EventResult::Ignored;
                }
                self.pressed = true;
                self.captured = true;
                ctx.capture_pointer();
                ctx.request_focus();
                ctx.request_redraw();
                EventResult::Handled
            }
            PointerPhase::Move => {
                if self.captured {
                    self.pressed = inside(p.position, size);
                    ctx.request_redraw();
                    return EventResult::Handled;
                }
                if inside(p.position, size) {
                    ctx.claim_hover();
                    ctx.set_cursor(ACTIVE_CURSOR);
                }
                EventResult::Ignored
            }
            PointerPhase::Up => {
                if !self.captured {
                    return EventResult::Ignored;
                }
                if inside(p.position, size)
                    && let Some(on_press) = &mut self.on_press
                {
                    on_press(ctx);
                }
                self.pressed = false;
                self.captured = false;
                ctx.request_redraw();
                EventResult::Handled
            }
            PointerPhase::Cancel => {
                if !self.captured {
                    return EventResult::Ignored;
                }
                self.pressed = false;
                self.captured = false;
                ctx.request_redraw();
                EventResult::Handled
            }
        }
    }

    fn is_focusable(&self) -> bool {
        self.on_press.is_some()
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        let focused = self.on_press.is_some() && ctx.has_focus() && !self.child_holds_focus();
        let role = if self.on_press.is_some() {
            Role::Button
        } else {
            Role::ListItem
        };
        let pressable = self.on_press.is_some();
        let id = ctx.push_container(
            role,
            |node| {
                node.set_label(self.title.content.as_str());
                if let Some(value) = &self.value {
                    node.set_value(value.content.as_str());
                }
                if let Some(subtitle) = &self.subtitle {
                    node.set_description(subtitle.content.as_str());
                }
                if pressable {
                    node.add_action(frust::authoring::Action::Click);
                }
            },
            |ctx| {
                for pod in [&self.leading, &self.trailing].into_iter().flatten() {
                    pod.semantics_child(ctx);
                }
            },
        );
        if focused {
            ctx.set_focused(id);
        }
    }

    fn visit_children(&self, visitor: &mut dyn FnMut(&ChildPod)) {
        for pod in [&self.leading, &self.trailing].into_iter().flatten() {
            visitor(pod);
        }
    }
}

/// A row's press handler, shared between builds of the view.
type PressHandler<State> = Rc<dyn Fn(&mut State)>;

/// One row of a grouped list. See the [module docs](self).
pub struct ListRowView<State: 'static> {
    title: String,
    subtitle: Option<String>,
    value: Option<String>,
    leading: Option<AnyView<State>>,
    trailing: Option<AnyView<State>>,
    chevron: bool,
    destructive: bool,
    on_press: Option<PressHandler<State>>,
}

/// A list row titled `title`.
pub fn list_row<State: 'static>(title: impl Into<String>) -> ListRowView<State> {
    ListRowView {
        title: title.into(),
        subtitle: None,
        value: None,
        leading: None,
        trailing: None,
        chevron: false,
        destructive: false,
        on_press: None,
    }
}

impl<State: 'static> ListRowView<State> {
    /// The view at the row's leading edge: an icon or an avatar.
    pub fn leading<V: View<State>>(mut self, view: V) -> Self {
        self.leading = Some(any(view));
        self
    }

    /// A secondary line under the title.
    pub fn subtitle(mut self, text: impl Into<String>) -> Self {
        self.subtitle = Some(text.into());
        self
    }

    /// A trailing value in the secondary colour. It keeps its width when the title
    /// is long.
    pub fn value(mut self, text: impl Into<String>) -> Self {
        self.value = Some(text.into());
        self
    }

    /// A view at the trailing edge, before the chevron.
    pub fn trailing<V: View<State>>(mut self, view: V) -> Self {
        self.trailing = Some(any(view));
        self
    }

    /// Draw a disclosure chevron at the trailing edge.
    pub fn chevron(mut self, chevron: bool) -> Self {
        self.chevron = chevron;
        self
    }

    /// Make the row pressable: `handler` runs when a press ends inside the row.
    pub fn on_press(mut self, handler: impl Fn(&mut State) + 'static) -> Self {
        self.on_press = Some(Rc::new(handler));
        self
    }

    /// Set the title in the destructive colour.
    pub fn destructive(mut self, destructive: bool) -> Self {
        self.destructive = destructive;
        self
    }
}

/// Reconcile one optional slot between two builds of the row.
fn rebuild_slot<State: 'static>(
    prev: &Option<AnyView<State>>,
    next: &Option<AnyView<State>>,
    pod: &mut Option<ChildPod>,
    ctx: &mut BuildCtx<'_>,
) -> ChangeFlags {
    match (prev, next, pod.as_mut()) {
        (Some(prev), Some(next), Some(pod)) => rebuild_child(prev, next, pod, ctx),
        (Some(prev), None, Some(old)) => {
            teardown_child(prev, old, ctx);
            *pod = None;
            ChangeFlags::LAYOUT | ChangeFlags::PAINT
        }
        (_, Some(next), _) => {
            *pod = Some(build_child(next, ctx));
            ChangeFlags::LAYOUT | ChangeFlags::PAINT
        }
        _ => ChangeFlags::NONE,
    }
}

/// Reconcile an optional text run against its new content.
fn sync_run(run: &mut Option<EllipsisRun>, next: Option<&str>) -> bool {
    match (run.as_mut(), next) {
        (Some(run), Some(text)) => run.set_content(text),
        (None, Some(text)) => {
            *run = Some(EllipsisRun::new(text));
            true
        }
        (Some(_), None) => {
            *run = None;
            true
        }
        (None, None) => false,
    }
}

impl<State: 'static> View<State> for ListRowView<State> {
    type Element = ListRowWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> ListRowWidget {
        ListRowWidget {
            title: EllipsisRun::new(self.title.clone()),
            subtitle: self.subtitle.as_deref().map(EllipsisRun::new),
            value: self.value.as_deref().map(EllipsisRun::new),
            leading: self.leading.as_ref().map(|v| build_child(v, ctx)),
            trailing: self.trailing.as_ref().map(|v| build_child(v, ctx)),
            chevron: self.chevron,
            destructive: self.destructive,
            on_press: self.on_press.as_ref().map(erase_callback),
            pressed: false,
            captured: false,
            title_inset: 0.0,
            title_at: Point::ZERO,
            subtitle_at: Point::ZERO,
            value_at: Point::ZERO,
            chevron_at: Point::ZERO,
            size: Size::ZERO,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut ListRowWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = ChangeFlags::NONE;
        let layout = ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        if element.title.set_content(&self.title) {
            flags |= layout;
        }
        if sync_run(&mut element.subtitle, self.subtitle.as_deref()) {
            flags |= layout;
        }
        if sync_run(&mut element.value, self.value.as_deref()) {
            flags |= layout;
        }
        flags |= rebuild_slot(&prev.leading, &self.leading, &mut element.leading, ctx);
        flags |= rebuild_slot(&prev.trailing, &self.trailing, &mut element.trailing, ctx);
        if element.chevron != self.chevron {
            element.chevron = self.chevron;
            flags |= layout;
        }
        if element.destructive != self.destructive {
            element.destructive = self.destructive;
            flags |= ChangeFlags::PAINT;
        }
        if self.on_press.is_some() != element.on_press.is_some() {
            flags |= ChangeFlags::PAINT;
            if self.on_press.is_none() {
                element.pressed = false;
                element.captured = false;
            }
        }
        // Closures are not comparable: the adapter is reinstalled on every rebuild.
        element.on_press = self.on_press.as_ref().map(erase_callback);
        flags
    }

    fn teardown(&self, element: &mut ListRowWidget, ctx: &mut BuildCtx<'_>) {
        if let (Some(view), Some(pod)) = (&self.leading, &mut element.leading) {
            teardown_child(view, pod, ctx);
        }
        if let (Some(view), Some(pod)) = (&self.trailing, &mut element.trailing) {
            teardown_child(view, pod, ctx);
        }
    }
}

// ---- The section header ----------------------------------------------------

/// The retained widget behind [`section_header`].
pub struct SectionHeaderWidget {
    text: String,
    run: EllipsisRun,
    at: Point,
}

impl Widget for SectionHeaderWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let m = BeuiMetrics::resolve(Theme::from_layout_ctx(ctx));
        let style = part_style(ctx, m.text_xs, ThemeTextType::LabelSmall);
        let inset = m.padding_x_input;
        let bounded = bc.max().width.is_finite();
        let text_width = bounded.then(|| (bc.max().width - inset * 2.0).max(0.0));
        let text = self.run.layout(ctx, &style, text_width);
        self.at = Point::new(inset, ROW_PAD_Y * 2.0);
        let width = if bounded {
            bc.max().width
        } else {
            text.width + inset * 2.0
        };
        bc.constrain(Size::new(width, text.height + ROW_PAD_Y * 2.0 + ROW_PAD_Y))
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let colors = ListColors::resolve(Theme::from_paint_ctx(ctx));
        self.run
            .paint(ctx.origin() + self.at.to_vec2(), colors.secondary, scene);
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        ctx.push_node(Role::Heading, |node| node.set_label(self.text.as_str()));
    }
}

/// A small heading above a group. See the [module docs](self).
pub struct SectionHeaderView {
    text: String,
}

/// A section header reading `text`, shown uppercase.
pub fn section_header(text: impl Into<String>) -> SectionHeaderView {
    SectionHeaderView { text: text.into() }
}

impl<State: 'static> View<State> for SectionHeaderView {
    type Element = SectionHeaderWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> SectionHeaderWidget {
        SectionHeaderWidget {
            text: self.text.clone(),
            run: EllipsisRun::new(self.text.to_uppercase()),
            at: Point::ZERO,
        }
    }

    fn rebuild(
        &self,
        _prev: &Self,
        element: &mut SectionHeaderWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        if element.text == self.text {
            return ChangeFlags::NONE;
        }
        element.text.clone_from(&self.text);
        element.run.set_content(&self.text.to_uppercase());
        ChangeFlags::LAYOUT | ChangeFlags::PAINT
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::{Density, theme, theme_with_density};
    use frust::FrameTime;
    use frust::authoring::scene::GlyphRun;
    use frust::authoring::{
        Key, KeyEvent, Modifiers, NamedKey, PointerButton, PointerEvent, SemanticsUpdate,
    };
    use std::any::Any;

    #[derive(Default)]
    struct App {
        pressed: Vec<&'static str>,
    }

    /// Records the fills a paint pass makes.
    #[derive(Default)]
    struct Recorder {
        rects: Vec<(Point, Size, Color)>,
        clips: usize,
        glyphs: usize,
        strokes: usize,
        /// `(origin, path bounds, width)` of every stroke.
        stroke_boxes: Vec<(Point, kurbo::Rect, f64)>,
    }

    impl PaintScene for Recorder {
        fn fill_rect(&mut self, o: Point, s: Size, c: Color) {
            self.rects.push((o, s, c));
        }
        fn draw_text(&mut self, _o: Point, _t: &str) {}
        fn push_clip_rounded(&mut self, _o: Point, _s: Size, _r: f64) {
            self.clips += 1;
        }
        fn draw_glyph_run(&mut self, _run: GlyphRun) {
            self.glyphs += 1;
        }
        fn stroke_path(&mut self, o: Point, p: &BezPath, w: f64, _b: &Brush) {
            self.strokes += 1;
            self.stroke_boxes
                .push((o, kurbo::Shape::bounding_box(p), w));
        }
    }

    fn build_of<V: View<App>>(view: &V) -> V::Element {
        let mut next_id = 0u64;
        view.build(&mut BuildCtx::new(&mut next_id))
    }

    fn lay_out<W: Widget>(w: &mut W, theme: &Theme, width: f64) -> Size {
        let mut text_ctx = TextContext::new();
        let mut ctx = LayoutCtx::with_resources(Some(&mut text_ctx as &mut dyn Any), Some(theme));
        w.layout(&mut ctx, &BoxConstraints::loose(Size::new(width, 2000.0)))
    }

    fn pointer(phase: PointerPhase, x: f64, y: f64) -> InputEvent {
        InputEvent::Pointer(PointerEvent {
            phase,
            position: Point::new(x, y),
            button: PointerButton::Primary,
        })
    }

    fn dispatch<W: Widget>(w: &mut W, state: &mut App, size: Size, event: &InputEvent) {
        let any_state: &mut dyn Any = state;
        let mut ctx = EventCtx::new(any_state, Point::ZERO, size);
        w.event(&mut ctx, event);
    }

    fn three_rows() -> GroupedListView<App> {
        grouped_list([
            list_row::<App>("Profile").on_press(|s| s.pressed.push("profile")),
            list_row::<App>("Billing").on_press(|s| s.pressed.push("billing")),
            list_row::<App>("Privacy").on_press(|s| s.pressed.push("privacy")),
        ])
    }

    fn paint_group(w: &mut GroupedListWidget, theme: &Theme, size: Size) -> Recorder {
        let mut rec = Recorder::default();
        let mut ctx = PaintCtx::new(Point::ZERO, size).with_theme(theme);
        w.paint(&mut ctx, &mut rec);
        rec
    }

    #[test]
    fn a_three_row_group_lays_out_with_two_separators_and_every_row() {
        let theme = theme();
        let mut w = build_of(&three_rows());
        assert_eq!(w.row_count(), 3);
        let size = lay_out(&mut w, &theme, 400.0);
        assert_eq!(w.separators.len(), 2);
        let m = BeuiMetrics::resolve(Some(&theme));
        // Rows stack: the group is three rows tall.
        assert!(size.height >= 3.0 * m.height_md);
        let rec = paint_group(&mut w, &theme, size);
        assert_eq!(rec.clips, 1, "the group clips its rows");
        let sep = color_scheme_light().outline_variant;
        let hairlines: Vec<_> = rec
            .rects
            .iter()
            .filter(|(_, s, c)| s.height == m.border_width && *c == sep)
            .collect();
        assert_eq!(hairlines.len(), 2);
        assert!(rec.glyphs > 0, "every row paints its title");
    }

    #[test]
    fn separators_start_at_the_title_edge_after_a_leading_view() {
        let theme = theme();
        let mut w = build_of(&grouped_list([list_row::<App>("A"), list_row::<App>("B")]));
        lay_out(&mut w, &theme, 400.0);
        let inset = BeuiMetrics::resolve(Some(&theme)).padding_x_input;
        assert_eq!(w.separators.len(), 1);
        assert_eq!(
            w.separators[0].1, inset,
            "the first row has no leading view"
        );
        let mut w = build_of(&grouped_list([
            list_row::<App>("B").leading(frust::text("i")),
            list_row::<App>("C"),
        ]));
        lay_out(&mut w, &theme, 400.0);
        assert!(w.separators[0].1 > inset, "the line starts at the title");
    }

    #[test]
    fn pressing_a_row_calls_its_handler_and_not_its_neighbours() {
        let theme = theme();
        let mut w = build_of(&three_rows());
        let size = lay_out(&mut w, &theme, 400.0);
        let row_h = size.height / 3.0;
        let mut state = App::default();
        let y = row_h * 1.5;
        dispatch(
            &mut w,
            &mut state,
            size,
            &pointer(PointerPhase::Down, 20.0, y),
        );
        dispatch(
            &mut w,
            &mut state,
            size,
            &pointer(PointerPhase::Up, 20.0, y),
        );
        assert_eq!(state.pressed, vec!["billing"]);
    }

    #[test]
    fn releasing_outside_the_row_does_not_press_it() {
        let theme = theme();
        let mut w = build_of(&three_rows());
        let size = lay_out(&mut w, &theme, 400.0);
        let mut state = App::default();
        dispatch(
            &mut w,
            &mut state,
            size,
            &pointer(PointerPhase::Down, 20.0, 5.0),
        );
        dispatch(
            &mut w,
            &mut state,
            size,
            &pointer(PointerPhase::Up, 20.0, 2000.0),
        );
        assert!(state.pressed.is_empty());
    }

    #[test]
    fn row_height_under_touch_clears_the_minimum() {
        let touch = theme_with_density(Density::Touch);
        let floor = BeuiMetrics::resolve(Some(&touch)).min_touch_target;
        assert!(floor >= 44.0);
        let mut w = build_of(&list_row::<App>("Profile"));
        let size = lay_out(&mut w, &touch, 400.0);
        assert!(size.height >= floor, "row height {}", size.height);
    }

    #[test]
    fn a_long_title_does_not_push_the_value_out_of_the_row() {
        let theme = theme();
        let long =
            "A very long title that cannot possibly fit on one line of a phone row ".repeat(3);
        let mut value_only = build_of(&list_row::<App>("Short").value("Enabled"));
        lay_out(&mut value_only, &theme, 320.0);
        let value_w = value_only.value.as_ref().unwrap().size().width;
        assert!(value_w > 0.0);

        let mut w = build_of(&list_row::<App>(long).value("Enabled").chevron(true));
        let size = lay_out(&mut w, &theme, 320.0);
        let value = w.value.as_ref().unwrap().size();
        assert_eq!(value.width, value_w, "the value keeps its width");
        assert!(w.value_at.x + value.width <= size.width);
        assert!(
            w.value_at.x >= w.title_at.x + w.title.size().width,
            "title ends before the value"
        );
        assert!(w.title.size().width < 320.0);
        assert_eq!(size.width, 320.0);
        // One line: the height is a row's, not a paragraph's.
        assert!(size.height < 100.0);
    }

    #[test]
    fn semantics_publish_the_label_and_the_value() {
        fn logic(_: &mut App) -> GroupedListView<App> {
            grouped_list([
                list_row::<App>("Notifications")
                    .value("On")
                    .on_press(|_| {}),
                list_row::<App>("Version").value("1.2").subtitle("Build 40"),
            ])
        }
        let mut root = frust_core::RenderRoot::new();
        let mut state = App::default();
        let mut tcx = TextContext::new();
        root.rebuild(&mut logic, &mut state);
        root.layout_with_text(Size::new(400.0, 800.0), &mut tcx as &mut dyn Any);
        let update: SemanticsUpdate = root.semantics();
        let button = update
            .nodes
            .iter()
            .find(|(_, n)| n.role() == Role::Button)
            .expect("a pressable row is a button");
        assert_eq!(button.1.label(), Some("Notifications"));
        assert_eq!(button.1.value(), Some("On"));
        let item = update
            .nodes
            .iter()
            .find(|(_, n)| n.role() == Role::ListItem)
            .expect("a plain row is a list item");
        assert_eq!(item.1.label(), Some("Version"));
        assert_eq!(item.1.value(), Some("1.2"));
    }

    #[test]
    fn both_brightnesses_build_lay_out_and_paint() {
        for theme in [
            Theme::builder(theme())
                .brightness(frust::Brightness::Light)
                .build(),
            Theme::builder(theme())
                .brightness(frust::Brightness::Dark)
                .build(),
        ] {
            let mut group = build_of(&grouped_list([
                list_row::<App>("Delete account")
                    .destructive(true)
                    .chevron(true),
                list_row::<App>("Plan").subtitle("Pro").value("Annual"),
            ]));
            let size = lay_out(&mut group, &theme, 360.0);
            let rec = paint_group(&mut group, &theme, size);
            assert!(rec.glyphs > 0);
            assert_eq!(rec.strokes, 1, "one chevron");
            let mut header = build_of_header("Account");
            let header_size = lay_out(&mut header, &theme, 360.0);
            assert!(header_size.height > 0.0);
        }
    }

    fn build_of_header(text: &str) -> SectionHeaderWidget {
        let mut next_id = 0u64;
        View::<App>::build(&section_header(text), &mut BuildCtx::new(&mut next_id))
    }

    #[test]
    fn a_section_header_is_uppercase_and_published_as_a_heading() {
        let header = build_of_header("Account");
        assert_eq!(header.run.content, "ACCOUNT");
        assert_eq!(header.text, "Account");
    }

    #[test]
    fn rebuild_follows_the_new_rows() {
        let before = grouped_list([list_row::<App>("A"), list_row::<App>("B")]);
        let after = grouped_list([
            list_row::<App>("A"),
            list_row::<App>("B"),
            list_row::<App>("C"),
        ]);
        let mut next_id = 0u64;
        let mut w = View::<App>::build(&before, &mut BuildCtx::new(&mut next_id));
        let flags = View::<App>::rebuild(&after, &before, &mut w, &mut BuildCtx::new(&mut next_id));
        assert_eq!(w.row_count(), 3);
        assert!(flags.contains(ChangeFlags::LAYOUT));
    }

    // ---- Keyboard: a pressable row is a Tab stop ----

    /// Rows 1 and 3 pressable, row 2 plain; row 3 also carries a trailing button.
    fn mixed_group(_: &mut App) -> GroupedListView<App> {
        grouped_list([
            list_row::<App>("Profile").on_press(|s| s.pressed.push("profile")),
            list_row::<App>("Billing"),
            list_row::<App>("Privacy")
                .on_press(|s| s.pressed.push("privacy"))
                .trailing(frust::button::<App, _>("Reset", |s| {
                    s.pressed.push("button")
                })),
        ])
    }

    /// A real root: focus traversal, the focus ring policy and semantics are the root's.
    struct Keyboard {
        root: frust_core::RenderRoot<App, GroupedListView<App>>,
        state: App,
        size: Size,
    }

    impl Keyboard {
        fn new() -> Self {
            let mut root = frust_core::RenderRoot::new();
            root.set_theme(Box::new(theme()));
            let mut state = App::default();
            let mut tcx = TextContext::new();
            root.rebuild(&mut mixed_group, &mut state);
            let size = root.layout_with_text(Size::new(400.0, 800.0), &mut tcx as &mut dyn Any);
            Self { root, state, size }
        }

        fn key(&mut self, key: Key, shift: bool) {
            self.root.event(
                &mut self.state,
                &InputEvent::Key(KeyEvent {
                    key,
                    modifiers: Modifiers {
                        shift,
                        ..Modifiers::default()
                    },
                    repeat: false,
                }),
            );
        }

        fn tab(&mut self) {
            self.key(Key::Named(NamedKey::Tab), false);
        }

        fn shift_tab(&mut self) {
            self.key(Key::Named(NamedKey::Tab), true);
        }

        fn enter(&mut self) {
            self.key(Key::Named(NamedKey::Enter), false);
        }

        fn space(&mut self) {
            self.key(Key::Character(" ".into()), false);
        }

        /// The label of the node the semantics report as focused.
        fn focused_label(&self) -> Option<String> {
            let update = self.root.semantics();
            let focus = update.focus?;
            update
                .nodes
                .iter()
                .find(|(id, _)| *id == focus)
                .and_then(|(_, n)| n.label().map(str::to_owned))
        }

        fn paint(&mut self) -> Recorder {
            let mut rec = Recorder::default();
            self.root.paint(&mut rec, FrameTime::ZERO);
            rec
        }

        fn row_bounds(&self) -> Vec<kurbo::Rect> {
            self.root
                .inspect()
                .into_iter()
                .filter(|n| n.type_name.ends_with("ListRowWidget"))
                .map(|n| n.bounds)
                .collect()
        }
    }

    #[test]
    fn tab_visits_the_pressable_rows_in_order_and_shift_tab_returns() {
        let mut kb = Keyboard::new();
        assert_eq!(kb.focused_label(), None);
        kb.tab();
        assert_eq!(kb.focused_label().as_deref(), Some("Profile"));
        kb.tab();
        assert_eq!(
            kb.focused_label().as_deref(),
            Some("Privacy"),
            "the plain row is skipped"
        );
        kb.shift_tab();
        assert_eq!(kb.focused_label().as_deref(), Some("Profile"));
    }

    #[test]
    fn enter_and_space_fire_only_the_focused_rows_handler() {
        let mut kb = Keyboard::new();
        kb.tab();
        kb.enter();
        assert_eq!(kb.state.pressed, vec!["profile"]);
        kb.space();
        assert_eq!(kb.state.pressed, vec!["profile", "profile"]);
        kb.tab();
        kb.enter();
        assert_eq!(kb.state.pressed, vec!["profile", "profile", "privacy"]);
        kb.space();
        assert_eq!(kb.state.pressed.last(), Some(&"privacy"));
        assert_eq!(kb.state.pressed.len(), 4);
    }

    #[test]
    fn a_pointer_press_focuses_the_row() {
        let mut kb = Keyboard::new();
        let y = kb.row_bounds()[2].center().y;
        for phase in [PointerPhase::Down, PointerPhase::Up] {
            kb.root.event(&mut kb.state, &pointer(phase, 20.0, y));
        }
        assert_eq!(kb.state.pressed, vec!["privacy"]);
        assert_eq!(kb.focused_label().as_deref(), Some("Privacy"));
    }

    #[test]
    fn the_ring_is_painted_only_for_the_focused_row_and_inside_the_group() {
        let mut kb = Keyboard::new();
        assert!(kb.paint().stroke_boxes.is_empty(), "no focus, no ring");
        let rows = kb.row_bounds();
        for (stop, row) in [(0, 0), (1, 2)] {
            kb.tab();
            let rec = kb.paint();
            let rings: Vec<_> = rec
                .stroke_boxes
                .iter()
                .filter(|b| b.2 == FOCUS_RING_WIDTH)
                .collect();
            assert_eq!(
                rings.len(),
                1,
                "stop {stop}: one ring, got {:?}",
                rec.stroke_boxes
            );
            let (origin, bounds, width) = *rings[0];
            let outer = (bounds + origin.to_vec2()).inflate(width / 2.0, width / 2.0);
            let group = kurbo::Rect::from_origin_size(Point::ORIGIN, kb.size);
            assert!(
                outer.x0 >= rows[row].x0
                    && outer.x1 <= rows[row].x1
                    && outer.y0 >= rows[row].y0
                    && outer.y1 <= rows[row].y1,
                "ring {outer:?} leaves its row {:?}",
                rows[row]
            );
            assert!(outer.x0 >= group.x0 && outer.x1 <= group.x1);
            assert!(outer.y0 >= group.y0 && outer.y1 <= group.y1);
        }
    }

    #[test]
    fn a_plain_row_is_not_a_stop() {
        let mut kb = Keyboard::new();
        kb.tab();
        kb.tab();
        kb.tab(); // the trailing button
        kb.tab(); // wraps to the first row
        assert_eq!(kb.focused_label().as_deref(), Some("Profile"));
        assert!(
            !kb.root
                .semantics()
                .nodes
                .iter()
                .any(|(id, n)| n.label() == Some("Billing")
                    && Some(*id) == kb.root.semantics().focus),
            "the plain row never takes focus"
        );
    }

    #[test]
    fn a_focused_trailing_button_keeps_enter_and_the_row_does_not_fire() {
        let mut kb = Keyboard::new();
        kb.tab();
        kb.tab();
        kb.tab();
        assert_eq!(kb.focused_label().as_deref(), Some("Reset"));
        kb.enter();
        assert_eq!(kb.state.pressed, vec!["button"]);
        // The ring belongs to the button's own focus, not the row's.
        let rec = kb.paint();
        assert!(
            !rec.stroke_boxes
                .iter()
                .any(|b| b.2 == FOCUS_RING_WIDTH && b.1.width() > 300.0)
        );
    }
}
