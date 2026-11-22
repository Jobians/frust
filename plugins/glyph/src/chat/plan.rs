//! [`plan_block`]: the agent's plan as a read-only checklist.
//!
//! # Contract
//!
//! - **Controlled and non-reactive.** The caller owns the entries and
//!   re-builds the view when one changes; the block has no interaction.
//! - **One row per entry**, in order: a status mark (`[ ]` pending, `[*]`
//!   in progress, `[✓]` completed), then the entry text, wrapped with
//!   `OverflowWrap::Anywhere` so an unbroken token stays inside the width.
//!   An in-progress entry is set in the accent container role and semi-bold,
//!   a completed one is muted, so no state relies on the mark alone.
//! - **Summary.** The header reads "n of m": completed entries of all.
//! - **Semantics.** The rows form a list; each item is one node labelled
//!   with its status and text, e.g. "completed: write the parser".

use frust::authoring::text::{FontWeight, OverflowWrap, TextOverflow};
use frust::authoring::{
    BoxConstraints, BuildCtx, ChangeFlags, ChildPod, EventCtx, EventResult, InputEvent, LayoutCtx,
    PaintCtx, PaintScene, Point, Role, SemanticsCtx, Size, ThemeTextColor, View, Widget,
    build_child, rebuild_child, route_event_single, teardown_child,
};
use frust::{CrossAxisAlignment, Theme, any, column, flexible, row, semantics, text};

use super::model::{PlanEntry, PlanEntryStatus};
use super::style;

/// Font size of entry text and the header.
const FONT_SIZE: f32 = style::BODY_FONT_SIZE;
/// Header text when the caller sets none.
const DEFAULT_TITLE: &str = "Plan";

/// A declarative plan checklist. See the [module docs](self).
pub struct PlanBlockView<State: 'static> {
    entries: Vec<PlanEntry>,
    title: String,
    _state: std::marker::PhantomData<fn(&mut State)>,
}

impl<State: 'static> std::fmt::Debug for PlanBlockView<State> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlanBlockView")
            .field("entries", &self.entries)
            .field("title", &self.title)
            .finish()
    }
}

/// Show `entries` as a checklist with an "n of m" summary. The caller owns
/// the entries and passes the current list on every build.
pub fn plan_block<State: 'static>(entries: impl Into<Vec<PlanEntry>>) -> PlanBlockView<State> {
    PlanBlockView {
        entries: entries.into(),
        title: DEFAULT_TITLE.to_owned(),
        _state: std::marker::PhantomData,
    }
}

impl<State: 'static> PlanBlockView<State> {
    /// The header label (default "Plan").
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    fn done(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.status == PlanEntryStatus::Completed)
            .count()
    }

    fn summary(&self) -> String {
        format!("{} of {}", self.done(), self.entries.len())
    }

    fn body_view(&self) -> impl View<State> + use<State> {
        let header = row()
            .cross_axis(CrossAxisAlignment::Center)
            .gap(style::PAD)
            .push(flexible(
                1,
                text(self.title.as_str())
                    .size(FONT_SIZE)
                    .weight(FontWeight::SEMI_BOLD)
                    .max_lines(1)
                    .overflow(TextOverflow::Ellipsis),
            ))
            .child(
                text(self.summary())
                    .size(FONT_SIZE)
                    .themed_role(ThemeTextColor::OnSurfaceVariant),
            );
        let rows: Vec<_> = self.entries.iter().map(entry_row::<State>).collect();
        let list = semantics(column().gap(style::ROW_GAP).children(rows))
            .role(Role::List)
            .label(self.title.clone());
        column().gap(style::BLOCK_GAP).child(header).child(list)
    }
}

/// The mark text of a status.
fn mark(status: PlanEntryStatus) -> &'static str {
    match status {
        PlanEntryStatus::Pending => "[ ]",
        PlanEntryStatus::InProgress => "[*]",
        PlanEntryStatus::Completed => "[\u{2713}]",
    }
}

/// The status word a screen reader hears.
fn status_word(status: PlanEntryStatus) -> &'static str {
    match status {
        PlanEntryStatus::Pending => "pending",
        PlanEntryStatus::InProgress => "in progress",
        PlanEntryStatus::Completed => "completed",
    }
}

fn entry_row<State: 'static>(entry: &PlanEntry) -> impl View<State> + use<State> {
    let role = match entry.status {
        PlanEntryStatus::Pending => ThemeTextColor::OnSurface,
        PlanEntryStatus::InProgress => ThemeTextColor::OnPrimaryContainer,
        PlanEntryStatus::Completed => ThemeTextColor::OnSurfaceVariant,
    };
    let weight = if entry.status == PlanEntryStatus::InProgress {
        FontWeight::SEMI_BOLD
    } else {
        FontWeight::REGULAR
    };
    let label = format!("{}: {}", status_word(entry.status), entry.text);
    let visual = row()
        .cross_axis(CrossAxisAlignment::Start)
        .gap(style::PAD_SMALL)
        .child(
            text(mark(entry.status))
                .size(FONT_SIZE)
                .themed_mono_family()
                .themed_role(role)
                .weight(weight),
        )
        .push(flexible(
            1,
            text(entry.text.as_str())
                .size(FONT_SIZE)
                .themed_role(role)
                .weight(weight)
                .overflow_wrap(OverflowWrap::Anywhere),
        ));
    semantics(visual)
        .role(Role::ListItem)
        .label(label)
        .merged(true)
}

/// The retained widget for a [`PlanBlockView`]: card chrome around one body.
pub struct PlanBlockWidget {
    body: ChildPod,
}

impl<State: 'static> View<State> for PlanBlockView<State> {
    type Element = PlanBlockWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> PlanBlockWidget {
        PlanBlockWidget {
            body: build_child(&any(self.body_view()), ctx),
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut PlanBlockWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        if prev.entries == self.entries && prev.title == self.title {
            return ChangeFlags::NONE;
        }
        rebuild_child(
            &any(prev.body_view()),
            &any(self.body_view()),
            &mut element.body,
            ctx,
        )
    }

    fn teardown(&self, element: &mut PlanBlockWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&any(self.body_view()), &mut element.body, ctx);
    }
}

impl Widget for PlanBlockWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let width = style::width_of(bc);
        let inner = (width - style::PAD * 2.0).max(0.0);
        let child_bc = BoxConstraints::new(Size::new(inner, 0.0), Size::new(inner, f64::INFINITY));
        let size = self.body.layout_child(ctx, &child_bc);
        self.body.set_origin(Point::new(style::PAD, style::PAD));
        bc.constrain(Size::new(width, size.height + style::PAD * 2.0))
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let theme = Theme::from_paint_ctx(ctx);
        let (surface, border) = (style::surface(theme), style::border(theme));
        let o = ctx.origin();
        let size = ctx.size();
        scene.fill_rounded_rect(o, size, style::RADIUS, surface);
        self.body.paint_child(ctx, scene);
        style::stroke_outline(scene, o, size, style::RADIUS, border);
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        route_event_single(&mut self.body, ctx, event)
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        self.body.semantics_child(ctx);
    }

    frust::authoring::visit_children!(body);
}

#[cfg(test)]
mod tests {
    use super::*;
    use frust::FrameTime;
    use frust::authoring::text::TextContext;
    use frust::authoring::{AnyView, DiscardScene, Rect};
    use frust_core::RenderRoot;
    use std::any::Any;

    type Root = RenderRoot<(), AnyView<()>>;

    struct Rig {
        root: Root,
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
                text: TextContext::new(),
                window: Size::new(window_w, 4000.0),
            }
        }

        fn show(&mut self, view: PlanBlockView<()>) {
            let mut view = Some(view);
            self.root.rebuild(
                &mut |_: &mut ()| any(view.take().expect("built once per show")),
                &mut (),
            );
            self.root
                .layout_with_text(self.window, &mut self.text as &mut dyn Any);
        }

        fn block(&self) -> Rect {
            self.root.inspect()[0].bounds
        }

        fn events(&self) -> u64 {
            let s = self.text.shape_cache_stats();
            s.shapes + s.line_breaks
        }
    }

    fn entries(n: usize) -> Vec<PlanEntry> {
        (0..n)
            .map(|i| PlanEntry {
                text: format!("step {i}"),
                status: match i % 3 {
                    0 => PlanEntryStatus::Completed,
                    1 => PlanEntryStatus::InProgress,
                    _ => PlanEntryStatus::Pending,
                },
            })
            .collect()
    }

    #[test]
    fn zero_one_and_fifty_entries_lay_out_within_width() {
        for width in [360.0, 1024.0] {
            let mut heights = Vec::new();
            for n in [0, 1, 50] {
                let mut rig = Rig::new(true, width);
                rig.show(plan_block(entries(n)));
                let b = rig.block();
                assert!(b.width() <= width + 0.01, "{n} entries at {width}");
                assert!(b.height() > 0.0);
                heights.push(b.height());
            }
            assert!(heights[0] < heights[1] && heights[1] < heights[2]);
        }
    }

    #[test]
    fn summary_counts_completed_entries() {
        let v = plan_block::<()>(entries(7));
        assert_eq!(v.summary(), "3 of 7");
        assert_eq!(plan_block::<()>(Vec::new()).summary(), "0 of 0");
    }

    #[test]
    fn a_long_unbroken_entry_wraps_inside_the_width() {
        let long = PlanEntry {
            text: "x".repeat(400),
            status: PlanEntryStatus::Pending,
        };
        let mut rig = Rig::new(true, 360.0);
        rig.show(plan_block(vec![long]));
        for n in rig.root.inspect() {
            assert!(n.bounds.x1 <= 360.0 + 0.01, "{}", n.type_name);
        }
        let short = Rig::new(true, 360.0);
        let mut short = short;
        short.show(plan_block(vec![PlanEntry {
            text: "x".into(),
            status: PlanEntryStatus::Pending,
        }]));
        assert!(rig.block().height() > short.block().height());
    }

    #[test]
    fn a_long_title_stays_on_one_line() {
        let mut rig = Rig::new(true, 360.0);
        rig.show(plan_block(entries(1)).title("t".repeat(300)));
        let mut one = Rig::new(true, 360.0);
        one.show(plan_block(entries(1)));
        assert!((rig.block().height() - one.block().height()).abs() < 0.5);
    }

    #[test]
    fn equal_views_do_no_text_work() {
        let mut rig = Rig::new(true, 360.0);
        rig.show(plan_block(entries(20)));
        let before = rig.events();
        rig.show(plan_block(entries(20)));
        assert_eq!(rig.events(), before);
        rig.show(plan_block(entries(21)));
        assert!(rig.events() > before);
    }

    #[test]
    fn items_expose_their_status_as_text() {
        let mut rig = Rig::new(true, 360.0);
        rig.show(plan_block(entries(3)));
        let update = rig.root.semantics();
        let labels: Vec<_> = update
            .nodes
            .iter()
            .filter(|(_, n)| n.role() == Role::ListItem)
            .filter_map(|(_, n)| n.label().map(str::to_owned))
            .collect();
        assert_eq!(
            labels,
            [
                "completed: step 0",
                "in progress: step 1",
                "pending: step 2"
            ]
        );
        assert!(update.nodes.iter().any(|(_, n)| n.role() == Role::List));
    }

    #[test]
    fn themed_and_unthemed_paint_do_not_panic() {
        for themed in [true, false] {
            let mut rig = Rig::new(themed, 360.0);
            rig.show(plan_block(entries(6)));
            rig.root
                .paint(&mut DiscardScene, FrameTime::from_nanos(1_000_000));
        }
    }
}
