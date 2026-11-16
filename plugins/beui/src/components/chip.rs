//! Chips: a compact [`chip`] and a toned [`status_badge`].
//!
//! The finished part is the pill a phone app uses for filters, tags and state: a
//! neutral chip, and a status badge coloured by [`StatusTone`]. This module is the
//! scaffold: each entry point is a minimal working view (text only, no styling
//! yet) with the public names the finished part keeps.

use frust::Theme;
use frust::authoring::text::TextStyle;
use frust::authoring::{
    BoxConstraints, BuildCtx, ChangeFlags, ChildPod, LayoutCtx, PaintCtx, PaintScene, View, Widget,
};
use kurbo::{Point, Size, Vec2};
use peniko::Color;

use crate::text::{LabelRun, ThemeTextType};

/// The retained widget behind the chip entry points: stacked text lines with an optional
/// child beneath them.
pub struct ChipWidget {
    lines: Vec<LabelRun>,
    child: Option<ChildPod>,
}

impl ChipWidget {
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

impl Widget for ChipWidget {
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

/// How a [`status_badge`] reads at a glance.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StatusTone {
    /// No particular meaning.
    #[default]
    Neutral,
    /// Informational.
    Info,
    /// Completed or healthy.
    Success,
    /// Needs attention.
    Warning,
    /// Failed or destructive.
    Danger,
}

/// A compact label chip. See the [module docs](self).
pub struct ChipView {
    label: String,
}

/// A chip reading `label`.
pub fn chip(label: impl Into<String>) -> ChipView {
    ChipView {
        label: label.into(),
    }
}

impl ChipView {
    fn lines(&self) -> Vec<String> {
        vec![self.label.clone()]
    }
}

impl<State: 'static> View<State> for ChipView {
    type Element = ChipWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> ChipWidget {
        ChipWidget::new(self.lines(), None)
    }

    fn rebuild(
        &self,
        _prev: &Self,
        element: &mut ChipWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        element.set_lines(self.lines())
    }
}

/// A chip carrying a [`StatusTone`]. See the [module docs](self).
pub struct StatusBadgeView {
    label: String,
    tone: StatusTone,
}

/// A status badge reading `label` in `tone`.
pub fn status_badge(label: impl Into<String>, tone: StatusTone) -> StatusBadgeView {
    StatusBadgeView {
        label: label.into(),
        tone,
    }
}

impl StatusBadgeView {
    fn lines(&self) -> Vec<String> {
        vec![self.label.clone()]
    }
}

impl<State: 'static> View<State> for StatusBadgeView {
    type Element = ChipWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> ChipWidget {
        ChipWidget::new(self.lines(), None)
    }

    fn rebuild(
        &self,
        _prev: &Self,
        element: &mut ChipWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        element.set_lines(self.lines())
    }
}

impl StatusBadgeView {
    /// The tone this badge carries.
    pub fn tone(&self) -> StatusTone {
        self.tone
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
    fn the_chip_entry_points_build_and_lay_out() {
        let (c, size) = laid_out(&chip("Draft"));
        assert_eq!(c.lines(), vec!["Draft"]);
        assert!(size.width > 0.0);
        for tone in [
            StatusTone::Neutral,
            StatusTone::Info,
            StatusTone::Success,
            StatusTone::Warning,
            StatusTone::Danger,
        ] {
            let badge = status_badge("Live", tone);
            assert_eq!(badge.tone(), tone);
            let (w, _) = laid_out(&badge);
            assert_eq!(w.lines(), vec!["Live"]);
        }
    }
}
