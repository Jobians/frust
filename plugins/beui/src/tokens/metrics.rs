//! Size tokens carried on the theme: [`BeuiMetrics`], selected by [`Density`].
//!
//! [`crate::style`]'s size constants are beUI's desktop-first web metrics, and
//! they stay the [`Density::Comfortable`] set. [`Density::Touch`] is the phone
//! set: every control height clears a platform touch-target minimum and the
//! type steps grow toward a phone's reading size. The set rides the theme as an
//! extension, so an app picks a density once
//! ([`install_with_density`](crate::install_with_density)) instead of forking
//! components; a component reads it through [`BeuiMetrics::resolve`].
//!
//! **Sources** for the touch set, retrieved 2026-11-16: Apple's Human Interface
//! Guidelines (*Accessibility*: a 44×44 pt minimum hit target; *Typography*:
//! the default Dynamic Type sizes Footnote 13, Subheadline 15, Body 17) and
//! Material Design 3 (*Accessibility*: a 48×48 dp minimum touch target).

use frust::Theme;

use crate::style;

/// Which metric set a beUI theme carries.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Density {
    /// beUI's own desktop-first web metrics — [`crate::style`]'s constants.
    #[default]
    Comfortable,
    /// The phone set: controls clear the platform touch minimum.
    Touch,
}

/// The size tokens a beUI component lays out with, per [`Density`].
///
/// Attached by [`theme_with_density`](crate::tokens::theme_with_density);
/// recovered with [`BeuiMetrics::resolve`], which answers the comfortable set
/// for a theme that carries none (including plain [`theme()`](fn@crate::theme)).
/// Fields are public so an app can adjust one rung of a resolved set and attach
/// the result to its own theme.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BeuiMetrics {
    /// The density this set was built for.
    pub density: Density,
    /// The smallest edge an interactive control may lay out at. `0.0` in the
    /// comfortable set, which imposes no floor.
    pub min_touch_target: f64,
    /// `sm` button height. Supersedes [`style::HEIGHT_SM`].
    pub height_sm: f64,
    /// `md` (default) button height. Supersedes [`style::HEIGHT_MD`].
    pub height_md: f64,
    /// `lg` button height. Supersedes [`style::HEIGHT_LG`].
    pub height_lg: f64,
    /// Edge of a square icon button. Supersedes [`style::SIZE_ICON_BUTTON`].
    pub size_icon_button: f64,
    /// Text input field height. Supersedes [`style::HEIGHT_INPUT`].
    pub height_input: f64,
    /// `sm` button and badge text. Supersedes [`style::TEXT_XS`].
    pub text_xs: f64,
    /// `md` button and label text. Supersedes [`style::TEXT_SM`].
    pub text_sm: f64,
    /// `lg` button and input text. Supersedes [`style::TEXT_BASE`].
    pub text_base: f64,
    /// Input field line height. Supersedes [`style::LINE_HEIGHT_INPUT`].
    pub line_height_input: f64,
    /// `sm` button horizontal padding. Supersedes [`style::PADDING_X_SM`].
    pub padding_x_sm: f64,
    /// `md` button horizontal padding. Supersedes [`style::PADDING_X_MD`].
    pub padding_x_md: f64,
    /// `lg` button horizontal padding. Supersedes [`style::PADDING_X_LG`].
    pub padding_x_lg: f64,
    /// Input padding on a side with no icon. Supersedes [`style::PADDING_X_INPUT`].
    pub padding_x_input: f64,
    /// Input padding on a side carrying an icon. Supersedes
    /// [`style::PADDING_X_INPUT_ICON`].
    pub padding_x_input_icon: f64,
    /// Icon-to-label gap at `sm`. Supersedes [`style::GAP_SM`].
    pub gap_sm: f64,
    /// Icon-to-label gap at `md`/`lg`. Supersedes [`style::GAP_MD`].
    pub gap_md: f64,
    /// Inline icon edge. Supersedes [`style::ICON_SIZE`].
    pub icon_size: f64,
    /// Larger in-field icon edge. Supersedes [`style::ICON_SIZE_LG`].
    pub icon_size_lg: f64,
    /// Card radius: [`style::RADIUS_2XL`] (`rounded-2xl`).
    pub radius_card: f64,
    /// List and menu panel radius: [`style::RADIUS_XL`] (`rounded-xl`).
    pub radius_list: f64,
    /// Control radius: the pill sentinel. Supersedes [`style::RADIUS_CONTROL`].
    pub radius_control: f64,
    /// Icon button radius. Supersedes [`style::RADIUS_ICON_BUTTON`].
    pub radius_icon_button: f64,
    /// Bordered-control stroke width. Supersedes [`style::BORDER_WIDTH`].
    pub border_width: f64,
}

/// Which touch-target convention [`BeuiMetrics::touch`] follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TouchPlatform {
    /// Apple's 44 pt minimum.
    Apple,
    /// Material's 48 dp minimum, used on every non-Apple target.
    Other,
}

impl TouchPlatform {
    /// The convention of the target this crate is compiled for.
    const fn host() -> Self {
        if cfg!(any(
            target_os = "ios",
            target_os = "macos",
            target_os = "tvos",
            target_os = "visionos",
            target_os = "watchos"
        )) {
            Self::Apple
        } else {
            Self::Other
        }
    }

    const fn min_touch_target(self) -> f64 {
        match self {
            Self::Apple => 44.0,
            Self::Other => 48.0,
        }
    }
}

impl BeuiMetrics {
    /// beUI's own metrics: exactly [`crate::style`]'s constants, no touch floor.
    pub const fn comfortable() -> Self {
        Self {
            density: Density::Comfortable,
            min_touch_target: 0.0,
            height_sm: style::HEIGHT_SM,
            height_md: style::HEIGHT_MD,
            height_lg: style::HEIGHT_LG,
            size_icon_button: style::SIZE_ICON_BUTTON,
            height_input: style::HEIGHT_INPUT,
            text_xs: style::TEXT_XS,
            text_sm: style::TEXT_SM,
            text_base: style::TEXT_BASE,
            line_height_input: style::LINE_HEIGHT_INPUT,
            padding_x_sm: style::PADDING_X_SM,
            padding_x_md: style::PADDING_X_MD,
            padding_x_lg: style::PADDING_X_LG,
            padding_x_input: style::PADDING_X_INPUT,
            padding_x_input_icon: style::PADDING_X_INPUT_ICON,
            gap_sm: style::GAP_SM,
            gap_md: style::GAP_MD,
            icon_size: style::ICON_SIZE,
            icon_size_lg: style::ICON_SIZE_LG,
            radius_card: style::RADIUS_2XL,
            radius_list: style::RADIUS_XL,
            radius_control: style::RADIUS_CONTROL,
            radius_icon_button: style::RADIUS_ICON_BUTTON,
            border_width: style::BORDER_WIDTH,
        }
    }

    /// The phone set for the target this crate is compiled for: a 44 px
    /// minimum on Apple targets, 48 px everywhere else.
    pub const fn touch() -> Self {
        Self::touch_for(TouchPlatform::host())
    }

    /// The touch set under `platform`'s convention.
    ///
    /// The height ladder keeps its rungs above the floor: `sm` and the icon
    /// button sit on it, `md` and the input one spacing step above, `lg` two.
    /// Text takes the Apple default Dynamic Type steps, which are no smaller
    /// than the comfortable ones on any platform. Radii and the border do not
    /// change with density.
    const fn touch_for(platform: TouchPlatform) -> Self {
        let floor = platform.min_touch_target();
        let step = style::SPACING_UNIT;
        let comfortable = Self::comfortable();
        Self {
            density: Density::Touch,
            min_touch_target: floor,
            height_sm: floor,
            height_md: floor + step,
            height_lg: floor + 2.0 * step,
            size_icon_button: floor,
            height_input: floor + step,
            text_xs: 13.0,
            text_sm: 15.0,
            text_base: 17.0,
            line_height_input: comfortable.line_height_input,
            // Paddings grow to `px-4` / `px-11` around the 20px (`size-5`)
            // icon; `md`/`lg` padding and the `md` gap are already roomy.
            padding_x_sm: 16.0,
            padding_x_md: comfortable.padding_x_md,
            padding_x_lg: comfortable.padding_x_lg,
            padding_x_input: 16.0,
            padding_x_input_icon: 44.0,
            gap_sm: 8.0,
            gap_md: comfortable.gap_md,
            icon_size: 20.0,
            icon_size_lg: 24.0,
            ..comfortable
        }
    }

    /// The set for `density`.
    pub const fn for_density(density: Density) -> Self {
        match density {
            Density::Comfortable => Self::comfortable(),
            Density::Touch => Self::touch(),
        }
    }

    /// Recover the set from a theme, falling back to
    /// [`BeuiMetrics::comfortable`] when the theme carries none — the same
    /// extension-then-fallback rung [`BeuiTokens::resolve`](crate::BeuiTokens::resolve)
    /// is.
    pub fn resolve(theme: Option<&Theme>) -> Self {
        theme
            .and_then(|t| t.extension::<BeuiMetrics>())
            .copied()
            .unwrap_or_else(Self::comfortable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::{BeuiTokens, theme, theme_with_density};
    use frust::{MonospaceFamily, NativeTypefaces};

    /// Every control height in `m`, named, in ladder order.
    fn control_heights(m: &BeuiMetrics) -> [(&'static str, f64); 5] {
        [
            ("height_sm", m.height_sm),
            ("size_icon_button", m.size_icon_button),
            ("height_md", m.height_md),
            ("height_input", m.height_input),
            ("height_lg", m.height_lg),
        ]
    }

    #[test]
    fn comfortable_is_the_style_constants_field_by_field() {
        let m = BeuiMetrics::comfortable();
        assert_eq!(m.density, Density::Comfortable);
        assert_eq!(m.min_touch_target, 0.0, "the comfortable set has no floor");
        assert_eq!(m.height_sm, style::HEIGHT_SM);
        assert_eq!(m.height_md, style::HEIGHT_MD);
        assert_eq!(m.height_lg, style::HEIGHT_LG);
        assert_eq!(m.size_icon_button, style::SIZE_ICON_BUTTON);
        assert_eq!(m.height_input, style::HEIGHT_INPUT);
        assert_eq!(m.text_xs, style::TEXT_XS);
        assert_eq!(m.text_sm, style::TEXT_SM);
        assert_eq!(m.text_base, style::TEXT_BASE);
        assert_eq!(m.line_height_input, style::LINE_HEIGHT_INPUT);
        assert_eq!(m.padding_x_sm, style::PADDING_X_SM);
        assert_eq!(m.padding_x_md, style::PADDING_X_MD);
        assert_eq!(m.padding_x_lg, style::PADDING_X_LG);
        assert_eq!(m.padding_x_input, style::PADDING_X_INPUT);
        assert_eq!(m.padding_x_input_icon, style::PADDING_X_INPUT_ICON);
        assert_eq!(m.gap_sm, style::GAP_SM);
        assert_eq!(m.gap_md, style::GAP_MD);
        assert_eq!(m.icon_size, style::ICON_SIZE);
        assert_eq!(m.icon_size_lg, style::ICON_SIZE_LG);
        assert_eq!(m.radius_card, style::RADIUS_2XL);
        assert_eq!(m.radius_list, style::RADIUS_XL);
        assert_eq!(m.radius_control, style::RADIUS_CONTROL);
        assert_eq!(m.radius_icon_button, style::RADIUS_ICON_BUTTON);
        assert_eq!(m.border_width, style::BORDER_WIDTH);
        assert_eq!(BeuiMetrics::for_density(Density::Comfortable), m);
        assert_eq!(Density::default(), Density::Comfortable);
    }

    #[test]
    fn touch_takes_the_platform_minimum_and_every_control_clears_it() {
        for (platform, floor) in [(TouchPlatform::Apple, 44.0), (TouchPlatform::Other, 48.0)] {
            let m = BeuiMetrics::touch_for(platform);
            assert_eq!(m.density, Density::Touch);
            assert_eq!(m.min_touch_target, floor, "{platform:?}");
            for (name, height) in control_heights(&m) {
                assert!(height >= floor, "{platform:?} {name} {height} < {floor}");
            }
            // The ladder still climbs, so `sm`/`md`/`lg` stay distinguishable.
            assert!(m.height_sm < m.height_md && m.height_md < m.height_lg);
        }
        assert_eq!(
            BeuiMetrics::touch(),
            BeuiMetrics::touch_for(TouchPlatform::host())
        );
        assert_eq!(
            BeuiMetrics::for_density(Density::Touch),
            BeuiMetrics::touch()
        );
    }

    #[test]
    fn touch_never_shrinks_the_type_or_the_spacing() {
        let c = BeuiMetrics::comfortable();
        for platform in [TouchPlatform::Apple, TouchPlatform::Other] {
            let t = BeuiMetrics::touch_for(platform);
            for (name, comfortable, touch) in [
                ("text_xs", c.text_xs, t.text_xs),
                ("text_sm", c.text_sm, t.text_sm),
                ("text_base", c.text_base, t.text_base),
                (
                    "line_height_input",
                    c.line_height_input,
                    t.line_height_input,
                ),
                ("padding_x_sm", c.padding_x_sm, t.padding_x_sm),
                ("padding_x_md", c.padding_x_md, t.padding_x_md),
                ("padding_x_lg", c.padding_x_lg, t.padding_x_lg),
                ("padding_x_input", c.padding_x_input, t.padding_x_input),
                (
                    "padding_x_input_icon",
                    c.padding_x_input_icon,
                    t.padding_x_input_icon,
                ),
                ("gap_sm", c.gap_sm, t.gap_sm),
                ("gap_md", c.gap_md, t.gap_md),
                ("icon_size", c.icon_size, t.icon_size),
                ("icon_size_lg", c.icon_size_lg, t.icon_size_lg),
            ] {
                assert!(touch >= comfortable, "{platform:?} {name}");
            }
            assert!(t.line_height_input >= t.text_base, "{platform:?}");
            assert_eq!(t.radius_card, c.radius_card);
            assert_eq!(t.radius_list, c.radius_list);
            assert_eq!(t.radius_control, c.radius_control);
            assert_eq!(t.border_width, c.border_width);
        }
    }

    #[test]
    fn resolve_falls_back_to_comfortable_and_reads_the_extension() {
        assert_eq!(BeuiMetrics::resolve(None), BeuiMetrics::comfortable());
        assert_eq!(
            BeuiMetrics::resolve(Some(&Theme::neutral())),
            BeuiMetrics::comfortable()
        );
        assert_eq!(
            BeuiMetrics::resolve(Some(&theme())),
            BeuiMetrics::comfortable(),
            "plain theme() carries no metrics"
        );
        assert_eq!(
            BeuiMetrics::resolve(Some(&theme_with_density(Density::Touch))),
            BeuiMetrics::touch()
        );
        assert_eq!(
            BeuiMetrics::resolve(Some(&theme_with_density(Density::Comfortable))),
            BeuiMetrics::comfortable()
        );
    }

    /// `theme_with_density` adds the metrics extension and changes nothing
    /// else — every token group and every other extension is `theme()`'s own.
    #[test]
    fn a_density_theme_differs_from_theme_only_by_the_extension() {
        let base = theme();
        let dense = theme_with_density(Density::Comfortable);
        assert!(base.extension::<BeuiMetrics>().is_none());
        assert_eq!(
            dense.extension::<BeuiMetrics>(),
            Some(&BeuiMetrics::comfortable())
        );
        assert_eq!(dense.extensions.len(), base.extensions.len() + 1);

        assert_eq!(dense.light, base.light);
        assert_eq!(dense.dark, base.dark);
        assert_eq!(dense.type_scale, base.type_scale);
        assert_eq!(dense.shape, base.shape);
        assert_eq!(dense.elevation, base.elevation);
        assert_eq!(dense.motion, base.motion);
        assert_eq!(dense.glass, base.glass);
        assert_eq!(dense.brightness, base.brightness);
        assert_eq!(dense.design_language, base.design_language);
        assert_eq!(
            dense.extension::<BeuiTokens>(),
            base.extension::<BeuiTokens>()
        );
        assert_eq!(
            dense.extension::<NativeTypefaces>(),
            base.extension::<NativeTypefaces>()
        );
        assert_eq!(
            dense.extension::<MonospaceFamily>(),
            base.extension::<MonospaceFamily>()
        );
        assert_eq!(
            dense,
            Theme::builder(base)
                .extension(BeuiMetrics::comfortable())
                .build()
        );
    }
}
