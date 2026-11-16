//! Alerts: an inline [`alert`] banner in an [`AlertTone`].
//!
//! The finished part is the banner a phone app shows for a notice or error: a tone,
//! an icon, a title and body, actions and a dismiss control. This module is the
//! scaffold: the entry point is a minimal working view (the title as text, no
//! styling yet) with the public names the finished part keeps.

use frust::Theme;
use frust::authoring::text::TextStyle;
use frust::authoring::{
    BoxConstraints, BuildCtx, ChangeFlags, ChildPod, LayoutCtx, PaintCtx, PaintScene, View, Widget,
};
use kurbo::{Point, Size, Vec2};
use peniko::Color;

use crate::text::{LabelRun, ThemeTextType};

/// The retained widget behind the alert: stacked text lines with an optional
/// child beneath them.
pub struct AlertWidget {
    lines: Vec<LabelRun>,
    child: Option<ChildPod>,
}

impl AlertWidget {
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

impl Widget for AlertWidget {
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

/// How an [`alert`] reads at a glance.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AlertTone {
    /// Informational.
    #[default]
    Info,
    /// Completed or healthy.
    Success,
    /// Needs attention.
    Warning,
    /// Failed or destructive.
    Danger,
}

/// An inline alert banner. See the [module docs](self).
pub struct AlertView {
    tone: AlertTone,
    title: String,
}

/// An alert in `tone` headed by `title`.
pub fn alert(tone: AlertTone, title: impl Into<String>) -> AlertView {
    AlertView {
        tone,
        title: title.into(),
    }
}

impl AlertView {
    fn lines(&self) -> Vec<String> {
        vec![self.title.clone()]
    }
}

impl<State: 'static> View<State> for AlertView {
    type Element = AlertWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> AlertWidget {
        AlertWidget::new(self.lines(), None)
    }

    fn rebuild(
        &self,
        _prev: &Self,
        element: &mut AlertWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        element.set_lines(self.lines())
    }
}

impl AlertView {
    /// The tone this alert carries.
    pub fn tone(&self) -> AlertTone {
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
    fn the_alert_builds_in_every_tone() {
        for tone in [
            AlertTone::Info,
            AlertTone::Success,
            AlertTone::Warning,
            AlertTone::Danger,
        ] {
            let view = alert(tone, "Heads up");
            assert_eq!(view.tone(), tone);
            let (w, size) = laid_out(&view);
            assert_eq!(w.lines(), vec!["Heads up"]);
            assert!(size.width > 0.0);
        }
    }
}
