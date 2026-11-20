//! Chart text: a [`Label`] shapes one string in layout and paints its glyph runs at an
//! offset, reusing the shape while the [`TextStyle`] and locale stay unchanged.
//!
//! Text is shaped only in layout (a paint has no text context), so a widget whose label text
//! or style changes needs a layout pass before the new text shows.

use frust::authoring::text::{LocaleStamp, TextContext, TextLayout, TextStyle};
use frust::authoring::{LayoutCtx, PaintScene, Point, Size};

/// The cache key behind a label's retained [`TextLayout`]: the style it was shaped at and the
/// [`LocaleStamp`] read before shaping, so a style or default-locale change reshapes it.
#[derive(Default)]
pub(crate) struct LayoutGuard {
    style: Option<TextStyle>,
    stamp: LocaleStamp,
}

impl LayoutGuard {
    /// Whether a shape made under this guard is still valid at `style`.
    pub(crate) fn matches(&self, style: &TextStyle) -> bool {
        self.style.as_ref() == Some(style) && self.stamp.is_current(style.locale.as_deref())
    }
}

/// One chart label: its text, its shaped layout and where it sits.
pub(crate) struct Label {
    /// The text this label shapes.
    pub(crate) text: String,
    layout: Option<TextLayout>,
    guard: LayoutGuard,
    /// The label's top-left, relative to the widget origin [`paint`](Self::paint) is given.
    pub(crate) at: Point,
}

impl Label {
    /// An unshaped label at the origin.
    pub(crate) fn new(text: String) -> Self {
        Self {
            text,
            layout: None,
            guard: LayoutGuard::default(),
            at: Point::ZERO,
        }
    }

    /// Shape at `style`, or reuse the last shape when the style and locale are unchanged.
    pub(crate) fn shape(&mut self, ctx: &mut LayoutCtx, style: &TextStyle) -> Size {
        if let Some(layout) = &self.layout
            && self.guard.matches(style)
        {
            return layout.size();
        }
        let stamp = LocaleStamp::for_style(style.locale.as_deref());
        let layout = ctx
            .text_context::<TextContext>()
            .layout(&self.text, style, None);
        let size = layout.size();
        self.layout = Some(layout);
        self.guard = LayoutGuard {
            style: Some(style.clone()),
            stamp,
        };
        size
    }

    /// The shaped size; zero before the first shape.
    pub(crate) fn size(&self) -> Size {
        self.layout.as_ref().map_or(Size::ZERO, TextLayout::size)
    }

    /// Draw the shaped text at [`at`](Self::at), offset by `origin` (the widget's origin).
    pub(crate) fn paint(&self, origin: Point, scene: &mut dyn PaintScene) {
        self.draw(origin + self.at.to_vec2(), scene);
    }

    /// Draw the shaped text with its top-left at `at` (scene coordinates).
    pub(crate) fn draw(&self, at: Point, scene: &mut dyn PaintScene) {
        if let Some(layout) = &self.layout {
            for run in layout.to_scene_runs(at) {
                scene.draw_glyph_run(run);
            }
        }
    }
}
