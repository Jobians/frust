//! Card surfaces: a plain [`card`], a [`hero_card`] with a title, and a [`stat_tile`].
//!
//! The finished part is a set of themed surfaces for grouping content on a phone:
//! a plain card around one child, a hero card with a title and an action row, and a
//! stat tile showing one labelled figure. This module is the scaffold: each entry
//! point is a minimal working view (stacked text and an optional child, no styling
//! yet) with the public names the finished part keeps.

use frust::Theme;
use frust::authoring::text::TextStyle;
use frust::authoring::{
    AnyView, BoxConstraints, BuildCtx, ChangeFlags, ChildPod, LayoutCtx, PaintCtx, PaintScene,
    View, Widget, any, build_child, rebuild_child, teardown_child,
};
use kurbo::{Point, Size, Vec2};
use peniko::Color;

use crate::text::{LabelRun, ThemeTextType};

/// The retained widget behind the card entry points: stacked text lines with an optional
/// child beneath them.
pub struct CardWidget {
    lines: Vec<LabelRun>,
    child: Option<ChildPod>,
}

impl CardWidget {
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

impl Widget for CardWidget {
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

/// A plain card around one child. See the [module docs](self).
pub struct CardView<State: 'static> {
    child: AnyView<State>,
}

/// Wrap `child` in a plain card.
pub fn card<State: 'static, V: View<State>>(child: V) -> CardView<State> {
    CardView { child: any(child) }
}

impl<State: 'static> CardView<State> {
    fn lines(&self) -> Vec<String> {
        Vec::new()
    }
}

impl<State: 'static> View<State> for CardView<State> {
    type Element = CardWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> CardWidget {
        CardWidget::new(self.lines(), Some(build_child(&self.child, ctx)))
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut CardWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = element.set_lines(self.lines());
        if let Some(pod) = &mut element.child {
            flags |= rebuild_child(&prev.child, &self.child, pod, ctx);
        }
        flags
    }

    fn teardown(&self, element: &mut CardWidget, ctx: &mut BuildCtx<'_>) {
        if let Some(pod) = &mut element.child {
            teardown_child(&self.child, pod, ctx);
        }
    }
}

/// A card with a title above its content. See the [module docs](self).
pub struct HeroCardView<State: 'static> {
    title: String,
    child: AnyView<State>,
}

/// Wrap `child` in a hero card headed by `title`.
pub fn hero_card<State: 'static, V: View<State>>(
    title: impl Into<String>,
    child: V,
) -> HeroCardView<State> {
    HeroCardView {
        title: title.into(),
        child: any(child),
    }
}

impl<State: 'static> HeroCardView<State> {
    fn lines(&self) -> Vec<String> {
        vec![self.title.clone()]
    }
}

impl<State: 'static> View<State> for HeroCardView<State> {
    type Element = CardWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> CardWidget {
        CardWidget::new(self.lines(), Some(build_child(&self.child, ctx)))
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut CardWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = element.set_lines(self.lines());
        if let Some(pod) = &mut element.child {
            flags |= rebuild_child(&prev.child, &self.child, pod, ctx);
        }
        flags
    }

    fn teardown(&self, element: &mut CardWidget, ctx: &mut BuildCtx<'_>) {
        if let Some(pod) = &mut element.child {
            teardown_child(&self.child, pod, ctx);
        }
    }
}

/// A tile showing one labelled figure. See the [module docs](self).
pub struct StatTileView {
    label: String,
    value: String,
}

/// A stat tile pairing a `label` with its `value`.
pub fn stat_tile(label: impl Into<String>, value: impl Into<String>) -> StatTileView {
    StatTileView {
        label: label.into(),
        value: value.into(),
    }
}

impl StatTileView {
    fn lines(&self) -> Vec<String> {
        vec![self.label.clone(), self.value.clone()]
    }
}

impl<State: 'static> View<State> for StatTileView {
    type Element = CardWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> CardWidget {
        CardWidget::new(self.lines(), None)
    }

    fn rebuild(
        &self,
        _prev: &Self,
        element: &mut CardWidget,
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
    fn the_card_entry_points_build_and_lay_out() {
        let (_, plain) = laid_out(&card::<(), _>(stat_tile("Open", "12")));
        assert!(plain.width > 0.0 && plain.height > 0.0);
        let (hero, size) = laid_out(&hero_card::<(), _>("Title", stat_tile("Open", "12")));
        assert_eq!(hero.lines(), vec!["Title"]);
        assert!(size.height > plain.height * 0.5);
        let (tile, _) = laid_out(&stat_tile("Open", "12"));
        assert_eq!(tile.lines(), vec!["Open", "12"]);
    }
}
