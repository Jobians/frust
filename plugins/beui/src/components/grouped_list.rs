//! Grouped lists: an inset [`grouped_list`] of [`list_row`]s under a [`section_header`].
//!
//! The finished part is the settings-style list a phone app is built from: an inset
//! rounded group, rows with a leading slot, title, value and chevron, and a small
//! section header. This module is the scaffold: each entry point is a minimal
//! working view (the group shows only its first row for now) with the public names
//! the finished part keeps.

use frust::Theme;
use frust::authoring::text::TextStyle;
use frust::authoring::{
    AnyView, BoxConstraints, BuildCtx, ChangeFlags, ChildPod, LayoutCtx, PaintCtx, PaintScene,
    View, Widget, any, build_child, rebuild_child, teardown_child,
};
use kurbo::{Point, Size, Vec2};
use peniko::Color;

use crate::text::{LabelRun, ThemeTextType};

/// The retained widget behind the grouped-list entry points: stacked text lines with an optional
/// child beneath them.
pub struct GroupedListWidget {
    lines: Vec<LabelRun>,
    child: Option<ChildPod>,
}

impl GroupedListWidget {
    fn new(lines: Vec<String>, child: Option<ChildPod>) -> Self {
        Self {
            lines: lines.into_iter().map(LabelRun::new).collect(),
            child,
        }
    }

    /// The text lines this widget carries, top to bottom.
    pub fn lines(&self) -> Vec<&str> {
        self.lines.iter().map(LabelRun::content).collect()
    }

    fn set_lines(&mut self, lines: Vec<String>) -> ChangeFlags {
        let mut flags = ChangeFlags::NONE;
        if self.lines.len() != lines.len() {
            self.lines = lines.into_iter().map(LabelRun::new).collect();
            return ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        for (run, text) in self.lines.iter_mut().zip(lines) {
            if run.set_content(text) {
                flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
            }
        }
        flags
    }
}

impl Widget for GroupedListWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let style = TextStyle::new(14.0, Color::BLACK);
        let mut width = 0.0_f64;
        let mut height = 0.0_f64;
        for run in &mut self.lines {
            let size = run.layout_themed(ctx, &style, ThemeTextType::BodyMedium);
            width = width.max(size.width);
            height += size.height;
        }
        if let Some(child) = &mut self.child {
            let size = child.layout_child(ctx, &bc.loosen());
            child.set_origin(Point::new(0.0, height));
            width = width.max(size.width);
            height += size.height;
        }
        bc.constrain(Size::new(width, height))
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let ink = Theme::from_paint_ctx(ctx).map_or(Color::BLACK, |t| t.scheme().on_surface);
        let mut y = 0.0;
        for run in &self.lines {
            run.paint(ctx.origin() + Vec2::new(0.0, y), ink, scene);
            y += run.size().height;
        }
        if let Some(child) = &mut self.child {
            child.paint_child(ctx, scene);
        }
    }

    fn visit_children(&self, visitor: &mut dyn FnMut(&ChildPod)) {
        if let Some(child) = &self.child {
            visitor(child);
        }
    }
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
        GroupedListWidget::new(
            vec![format!("{} rows", self.rows.len())],
            self.rows.first().map(|row| build_child(row, ctx)),
        )
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut GroupedListWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = element.set_lines(vec![format!("{} rows", self.rows.len())]);
        if let (Some(pod), Some(prev_row), Some(row)) =
            (&mut element.child, prev.rows.first(), self.rows.first())
        {
            flags |= rebuild_child(prev_row, row, pod, ctx);
        }
        flags
    }

    fn teardown(&self, element: &mut GroupedListWidget, ctx: &mut BuildCtx<'_>) {
        if let (Some(pod), Some(row)) = (&mut element.child, self.rows.first()) {
            teardown_child(row, pod, ctx);
        }
    }
}

/// One row of a grouped list. See the [module docs](self).
pub struct ListRowView {
    title: String,
}

/// A list row titled `title`.
pub fn list_row(title: impl Into<String>) -> ListRowView {
    ListRowView {
        title: title.into(),
    }
}

impl ListRowView {
    fn lines(&self) -> Vec<String> {
        vec![self.title.clone()]
    }
}

impl<State: 'static> View<State> for ListRowView {
    type Element = GroupedListWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> GroupedListWidget {
        GroupedListWidget::new(self.lines(), None)
    }

    fn rebuild(
        &self,
        _prev: &Self,
        element: &mut GroupedListWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        element.set_lines(self.lines())
    }
}

/// A small heading above a group. See the [module docs](self).
pub struct SectionHeaderView {
    text: String,
}

/// A section header reading `text`.
pub fn section_header(text: impl Into<String>) -> SectionHeaderView {
    SectionHeaderView { text: text.into() }
}

impl SectionHeaderView {
    fn lines(&self) -> Vec<String> {
        vec![self.text.clone()]
    }
}

impl<State: 'static> View<State> for SectionHeaderView {
    type Element = GroupedListWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> GroupedListWidget {
        GroupedListWidget::new(self.lines(), None)
    }

    fn rebuild(
        &self,
        _prev: &Self,
        element: &mut GroupedListWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        element.set_lines(self.lines())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn the_list_entry_points_build_and_lay_out() {
        let (header, _) = laid_out(&section_header("Account"));
        assert_eq!(header.lines(), vec!["Account"]);
        let (row, size) = laid_out(&list_row("Profile"));
        assert_eq!(row.lines(), vec!["Profile"]);
        assert!(size.height > 0.0);
        let (group, _) = laid_out(&grouped_list::<(), _>([
            list_row("Profile"),
            list_row("Billing"),
        ]));
        assert_eq!(group.lines(), vec!["2 rows"]);
    }
}
