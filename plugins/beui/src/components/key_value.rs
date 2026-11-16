//! Key-value rows and stage timelines: [`key_value_row`] and [`timeline`].
//!
//! The finished part is the read-only detail layout of a phone screen: a row pairing
//! a key with its value, and a vertical timeline of [`TimelineStep`]s each in a
//! [`StepState`]. This module is the scaffold: each entry point is a minimal working
//! view (text only, no styling yet) with the public names the finished part keeps.

use frust::Theme;
use frust::authoring::text::TextStyle;
use frust::authoring::{
    BoxConstraints, BuildCtx, ChangeFlags, ChildPod, LayoutCtx, PaintCtx, PaintScene, View, Widget,
};
use kurbo::{Point, Size, Vec2};
use peniko::Color;

use crate::text::{LabelRun, ThemeTextType};

/// The retained widget behind the key-value and timeline entry points: stacked text lines with an optional
/// child beneath them.
pub struct KeyValueWidget {
    lines: Vec<LabelRun>,
    child: Option<ChildPod>,
}

impl KeyValueWidget {
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

impl Widget for KeyValueWidget {
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

/// Where a [`TimelineStep`] stands.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StepState {
    /// Finished.
    Done,
    /// In progress.
    Current,
    /// Not reached yet.
    #[default]
    Pending,
}

/// One stage of a [`timeline`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineStep {
    /// The stage's label.
    pub label: String,
    /// Where the stage stands.
    pub state: StepState,
}

impl TimelineStep {
    /// A step labelled `label` in `state`.
    pub fn new(label: impl Into<String>, state: StepState) -> Self {
        Self {
            label: label.into(),
            state,
        }
    }
}

/// A key and its value on one row. See the [module docs](self).
pub struct KeyValueRowView {
    key: String,
    value: String,
}

/// A row pairing `key` with `value`.
pub fn key_value_row(key: impl Into<String>, value: impl Into<String>) -> KeyValueRowView {
    KeyValueRowView {
        key: key.into(),
        value: value.into(),
    }
}

impl KeyValueRowView {
    fn lines(&self) -> Vec<String> {
        vec![self.key.clone(), self.value.clone()]
    }
}

impl<State: 'static> View<State> for KeyValueRowView {
    type Element = KeyValueWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> KeyValueWidget {
        KeyValueWidget::new(self.lines(), None)
    }

    fn rebuild(
        &self,
        _prev: &Self,
        element: &mut KeyValueWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        element.set_lines(self.lines())
    }
}

/// A vertical stage timeline. See the [module docs](self).
pub struct TimelineView {
    steps: Vec<TimelineStep>,
}

/// A timeline through `steps`, in order.
pub fn timeline(steps: impl IntoIterator<Item = TimelineStep>) -> TimelineView {
    TimelineView {
        steps: steps.into_iter().collect(),
    }
}

impl TimelineView {
    fn lines(&self) -> Vec<String> {
        self.steps.iter().map(|step| step.label.clone()).collect()
    }
}

impl<State: 'static> View<State> for TimelineView {
    type Element = KeyValueWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> KeyValueWidget {
        KeyValueWidget::new(self.lines(), None)
    }

    fn rebuild(
        &self,
        _prev: &Self,
        element: &mut KeyValueWidget,
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
    fn the_key_value_entry_points_build_and_lay_out() {
        let (row, size) = laid_out(&key_value_row("Region", "eu-west"));
        assert_eq!(row.lines(), vec!["Region", "eu-west"]);
        assert!(size.height > 0.0);
        let steps = [
            TimelineStep::new("Queued", StepState::Done),
            TimelineStep::new("Running", StepState::Current),
            TimelineStep::new("Shipped", StepState::Pending),
        ];
        let (tl, _) = laid_out(&timeline(steps));
        assert_eq!(tl.lines(), vec!["Queued", "Running", "Shipped"]);
    }
}
