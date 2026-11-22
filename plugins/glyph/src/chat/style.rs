//! Shared visual constants and resolvers for the chat family, so components
//! agree on spacing, radii, fonts and colors.
//!
//! Glyph has no spacing token scale: padding and gaps are per-widget
//! constants, and the chat family's live here. Colors resolve from the live
//! `Theme` (`ColorScheme`, [`StatusPalette`], [`GlyphInk`]) with an unthemed
//! Glyph-dark fallback per value, never a panic and never a design-language
//! match.
//!
//! The glyph characters below are all present in the bundled IBM Plex Mono
//! (the face [`mono_family`] resolves to); `●`, `○`, `✗`, `▶` and friends are
//! not, and would silently draw nothing. Space Mono, the display face, also
//! lacks `✓` and `↻`, so these glyphs must be shaped with the mono family, not
//! the display one.

#![allow(dead_code)] // consumed by the component cards that follow

use frust::authoring::text::{FontFamily, GenericSlot};
use frust::{StatusPalette, Theme};
use peniko::Color;

use super::model::{ToolKind, ToolStatus};
use crate::GlyphInk;
use crate::tokens::color;

/// Gap between sibling blocks inside a message.
pub(crate) const BLOCK_GAP: f64 = 8.0;
/// Gap between rows of a list-like block.
pub(crate) const ROW_GAP: f64 = 4.0;
/// Inner padding of a card-like block (code, diff, tool call).
pub(crate) const PAD: f64 = 10.0;
/// Inner padding of a compact row.
pub(crate) const PAD_SMALL: f64 = 6.0;
/// Corner radius of a block.
pub(crate) const RADIUS: f64 = 8.0;
/// Corner radius of a small chip or button.
pub(crate) const RADIUS_SMALL: f64 = 4.0;
/// Border width of a block.
pub(crate) const BORDER_WIDTH: f64 = 1.0;
/// Font size of code and diff text.
pub(crate) const CODE_FONT_SIZE: f64 = 13.0;
/// Font size of secondary labels.
pub(crate) const LABEL_FONT_SIZE: f64 = 12.0;

const DARK: frust::ColorScheme = color::dark();
const DARK_STATUS: StatusPalette = color::status();

/// Unthemed surface fill.
pub(crate) const FALLBACK_SURFACE: Color = DARK.surface_container;
/// Unthemed border.
pub(crate) const FALLBACK_BORDER: Color = DARK.outline_variant;
/// Unthemed muted text.
pub(crate) const FALLBACK_MUTED: Color = DARK.on_surface_variant;
/// Unthemed code background.
pub(crate) const FALLBACK_CODE_BG: Color = GlyphInk::default_ink().terminal_bg;
/// Unthemed added-row background (success wash).
pub(crate) const FALLBACK_ADDED_BG: Color = DARK_STATUS.dark.success_container;
/// Unthemed removed-row background (error wash).
pub(crate) const FALLBACK_REMOVED_BG: Color = DARK.error_container;

/// The monospace family: the theme's `MonospaceFamily` extension, else
/// IBM Plex Mono with the generic monospace slot behind it.
pub(crate) fn mono_family(theme: Option<&Theme>) -> FontFamily {
    match theme {
        Some(t) => t.monospace_family(),
        None => unthemed_mono(),
    }
}

fn unthemed_mono() -> FontFamily {
    FontFamily::stack_with_generic(["IBM Plex Mono"], GenericSlot::Monospace)
}

/// Surface fill of a block.
pub(crate) fn surface(theme: Option<&Theme>) -> Color {
    theme.map_or(FALLBACK_SURFACE, |t| t.scheme().surface_container)
}

/// Border of a block.
pub(crate) fn border(theme: Option<&Theme>) -> Color {
    theme.map_or(FALLBACK_BORDER, |t| t.scheme().outline_variant)
}

/// Muted (secondary) text.
pub(crate) fn muted(theme: Option<&Theme>) -> Color {
    theme.map_or(FALLBACK_MUTED, |t| t.scheme().on_surface_variant)
}

/// Code and diff background: the Glyph terminal ink, which stays dark in
/// both brightnesses.
pub(crate) fn code_bg(theme: Option<&Theme>) -> Color {
    theme
        .and_then(|t| t.extension::<GlyphInk>())
        .map_or(FALLBACK_CODE_BG, |ink| ink.terminal_bg)
}

/// Background of an added diff row.
pub(crate) fn added_bg(theme: Option<&Theme>) -> Color {
    theme.map_or(FALLBACK_ADDED_BG, |t| {
        t.extension::<StatusPalette>()
            .map_or(FALLBACK_ADDED_BG, |s| {
                s.colors(t.brightness).success_container
            })
    })
}

/// Background of a removed diff row.
pub(crate) fn removed_bg(theme: Option<&Theme>) -> Color {
    theme.map_or(FALLBACK_REMOVED_BG, |t| t.scheme().error_container)
}

/// Color of a tool status mark.
pub(crate) fn status_color(theme: Option<&Theme>, status: ToolStatus) -> Color {
    let fallback = || match status {
        ToolStatus::Completed => DARK_STATUS.dark.success,
        ToolStatus::Failed => DARK.error,
        _ => FALLBACK_MUTED,
    };
    let Some(t) = theme else { return fallback() };
    match status {
        ToolStatus::Completed => t
            .extension::<StatusPalette>()
            .map_or_else(fallback, |s| s.colors(t.brightness).success),
        ToolStatus::Failed => t.scheme().error,
        ToolStatus::InProgress => t.scheme().primary,
        _ => t.scheme().on_surface_variant,
    }
}

/// The single character that stands for a tool kind.
pub(crate) fn kind_glyph(kind: ToolKind) -> char {
    match kind {
        ToolKind::Read => '\u{203A}',   // ›
        ToolKind::Edit => '\u{00B1}',   // ±
        ToolKind::Delete => '\u{00D7}', // ×
        ToolKind::Move => '\u{2192}',   // →
        ToolKind::Search => '?',
        ToolKind::Execute => '$',
        ToolKind::Think => '\u{2026}', // …
        ToolKind::Fetch => '\u{2193}', // ↓
        ToolKind::Other => '\u{2022}', // •
    }
}

/// The single character that stands for a tool status.
pub(crate) fn status_glyph(status: ToolStatus) -> char {
    match status {
        ToolStatus::Pending => '\u{00B7}', // ·
        ToolStatus::InProgress => '*',
        ToolStatus::Completed => '\u{2713}', // ✓
        ToolStatus::Failed => '\u{00D7}',    // ×
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn kind_glyphs_are_distinct() {
        for (i, a) in KINDS.iter().enumerate() {
            for b in &KINDS[i + 1..] {
                assert_ne!(kind_glyph(*a), kind_glyph(*b), "{a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn status_glyphs_are_distinct() {
        for (i, a) in STATUSES.iter().enumerate() {
            for b in &STATUSES[i + 1..] {
                assert_ne!(status_glyph(*a), status_glyph(*b), "{a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn glyphs_avoid_known_missing_characters() {
        // Verified absent from the bundled IBM Plex Mono and Space Mono.
        const MISSING: &[char] = &[
            '●', '○', '✗', '✕', '✔', '✘', '▶', '▸', '▾', '▼', '◆', '◇', '⋯', '⏵', '⏸', '⌫',
        ];
        for k in KINDS {
            assert!(!MISSING.contains(&kind_glyph(k)), "{k:?}");
        }
        for s in STATUSES {
            assert!(!MISSING.contains(&status_glyph(s)), "{s:?}");
        }
    }

    #[test]
    fn unthemed_matches_baseline_theme() {
        let t = crate::baseline();
        assert_eq!(surface(Some(&t)), surface(None));
        assert_eq!(border(Some(&t)), border(None));
        assert_eq!(muted(Some(&t)), muted(None));
        assert_eq!(code_bg(Some(&t)), code_bg(None));
        assert_eq!(removed_bg(Some(&t)), removed_bg(None));
        assert_eq!(added_bg(Some(&t)), added_bg(None));
    }

    #[test]
    fn mono_family_unthemed_and_themed_both_resolve() {
        let _ = mono_family(None);
        let _ = mono_family(Some(&crate::baseline()));
    }
}
