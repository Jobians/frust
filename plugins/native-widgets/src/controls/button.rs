//! `Button` — a real `android.widget.Button`, built and driven from Rust.
//!
//! Six properties (the 5–8 things apps actually set): caption,
//! enabled, text colour, background colour, text size, accessibility label.
//! The caption and the two layout-affecting properties are
//! [`Tier::Relayout`](super::Tier::Relayout); the rest are
//! [`Tier::Cheap`](super::Tier::Cheap) — see [`super`]'s tier table.
//!
//! Clicks are platform-owned: `create` attaches the shared
//! `dev.frust.nativewidgets.FrustNativeListener` as an `OnClickListener` on
//! Android, a `FrustNativeControlTarget` as the `TouchUpInside` action on iOS
//! (`crate::apple::events`), and the macOS `FrustNativeControlTarget` as the
//! `NSButton`'s target/action (`crate::appkit::events`); all three arms'
//! [`on_event`](crate::runtime::NativeWidget::on_event) decode the firing via
//! the SAME [`crate::events::decode_click`] into the runtime's event
//! dispatch. Nothing about that goes through `RenderRoot::event` (the crate
//! doc's event-bypass rule) — a click is never caused by `update`'s own
//! setters, so unlike `Switch`/`Slider` this control needs no echo guard, on
//! either platform.

use super::{
    BACKGROUND_COLOR, CONTENT_DESCRIPTION, CORNER_RADIUS_DP, ENABLED, Plan, Setter, TEXT,
    TEXT_COLOR, TEXT_SIZE_SP, TYPEFACE, color, owned_text, plan_color, slot_of, text_or_empty,
};
use crate::NativeWidgetError;
use crate::controls::typeface::{self, Typeface};
use crate::registry::SlotId;
use crate::runtime::Params;

/// The registered kind string the api layer injects as
/// `__frustControl` (`crate::runtime`'s generic-factory contract).
pub(crate) const KIND: &str = "button";

/// `"buttonStyle"` — the button's [`ButtonStyle`] wire value. Absent means
/// [`ButtonStyle::Plain`]: the api layer omits the key for a plain button, so
/// a default button's payload is byte-for-byte what it was before styles
/// existed. The values are append-only.
pub(crate) const BUTTON_STYLE: &str = "buttonStyle";

/// How a button presents itself — the control layer's mirror of the public
/// `NativeButtonStyle`.
///
/// Each platform maps it onto its own idiom (Material text / filled /
/// error-coloured / tonal / filled buttons on Android; `UIButtonConfiguration`
/// on iOS; bezel colours on macOS). The wire values are append-only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ButtonStyle {
    /// The platform's ordinary button. Wire `0` (omitted).
    #[default]
    Plain,
    /// A filled, primary-emphasis button. Wire `1`.
    Prominent,
    /// A filled button in the platform's destructive colour. Wire `2`.
    Destructive,
    /// The translucent "glass" button (iOS 26); a tonal button on Android.
    /// Wire `3`.
    Glass,
    /// The prominent glass button (iOS 26); a filled button on Android.
    /// Wire `4`.
    ProminentGlass,
}

impl ButtonStyle {
    /// The append-only wire value.
    pub(crate) fn wire(self) -> i64 {
        match self {
            Self::Plain => 0,
            Self::Prominent => 1,
            Self::Destructive => 2,
            Self::Glass => 3,
            Self::ProminentGlass => 4,
        }
    }

    /// Decode a wire value; an unknown one (a newer api layer) degrades to
    /// [`Self::Plain`] rather than killing the slot.
    pub(crate) fn from_wire(value: i64) -> Self {
        match value {
            1 => Self::Prominent,
            2 => Self::Destructive,
            3 => Self::Glass,
            4 => Self::ProminentGlass,
            _ => Self::Plain,
        }
    }
}

/// The marker type registered under [`KIND`]; its
/// [`NativeWidget`](crate::runtime::NativeWidget) impl is the Android half
/// below.
pub(crate) struct Button;

/// Everything a `Button` slot can be told, as one Rust-diffed value.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ButtonProps {
    /// The differ's slot id. **Not a property** — no setter is ever planned
    /// for it; `create` needs it to construct the slot's listener and it
    /// cannot change for a live instance.
    pub(crate) slot: SlotId,
    /// The caption. Empty is legal (an icon-only button).
    pub(crate) text: String,
    /// `View.setEnabled`; absent params mean `true`, the platform default.
    pub(crate) enabled: bool,
    /// Packed ARGB text colour, or `None` to leave the theme's own.
    pub(crate) text_color: Option<i32>,
    /// Packed ARGB background fill, or `None` to keep the themed background
    /// (and its ripple — see [`Setter::BackgroundColor`]).
    pub(crate) background_color: Option<i32>,
    /// Text size in scale-independent pixels, or `None` for the platform's.
    pub(crate) text_size_sp: Option<f32>,
    /// The TalkBack label; `None` lets the platform fall back to the caption.
    pub(crate) content_description: Option<String>,
    /// Corner radius, dp — theme ladder L2 (see
    /// `crate::api::theme::ResolvedTheme::corner_radius_dp`, folded in by
    /// `api::builders`). `Some` alongside a `Some` [`Self::background_color`]
    /// plans [`Setter::ThemedBackground`]; a background colour with no radius
    /// keeps the plain flat [`Setter::BackgroundColor`] path (see
    /// [`Self::plan`]).
    pub(crate) corner_radius_dp: Option<f32>,
    /// Theme ladder L3: the resolved
    /// [`crate::api::theme::ResolvedTheme::button_typeface`], or
    /// [`Typeface::System`] when absent (an older api layer, or a control
    /// built with no theme threaded).
    pub(crate) typeface: Typeface,
    /// The button's [`ButtonStyle`]. Not part of the setter [`Plan`] (a style
    /// swap is a platform restyle of the live view, not a property write):
    /// the platform arms read it through [`Self::style_change`] and apply it
    /// ahead of the plan, so a change is an update, never a remount.
    pub(crate) style: ButtonStyle,
}

/// What a style change does to a live button, ahead of its [`Plan`].
///
/// A restyled button must look exactly like one freshly created with the new
/// props. The style owns the button's background, corner radius and caption
/// colour, and the api layer folds the theme's values for those only into a
/// [`ButtonStyle::Plain`] button, so a restyle first drops whatever theme
/// look the old props applied (the two `clear_*` flags), letting the new
/// style's own look show, and [`ButtonProps::plan`] then re-applies every
/// look field the new props carry, diffed against a fresh button's rather
/// than the old props'.
///
/// The Apple arms honour the flags one by one. The Android arm's
/// `buttonSetStyle` resets background, shape, insets and caption colours
/// from a reference button of the new style on every restyle, which covers
/// both flags whatever their value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(any(target_os = "ios", target_os = "macos")), allow(dead_code))]
pub(crate) struct Restyle {
    /// The style to install.
    pub(crate) style: ButtonStyle,
    /// The old props applied a theme fill or corner radius
    /// ([`Setter::ThemedBackground`] / [`Setter::BackgroundColor`]): clear
    /// both, back to the platform's own background.
    pub(crate) clear_background: bool,
    /// The old props applied a caption colour ([`Setter::TextColor`]): clear
    /// it, back to the style's own.
    pub(crate) clear_text_color: bool,
}

/// The style-owned look fields of a [`ButtonProps`] — the ones a
/// [`Restyle`] resets, so [`ButtonProps::plan`] diffs them against a fresh
/// button's after a style change. The text size and typeface are here too:
/// a platform style may install its own font (a `UIButtonConfiguration`
/// does), so the theme's size and face are re-applied with the rest.
#[derive(Clone, Copy)]
struct Look {
    text_color: Option<i32>,
    background_color: Option<i32>,
    corner_radius_dp: Option<f32>,
    text_size_sp: Option<f32>,
    typeface: Typeface,
}

impl Look {
    fn of(props: &ButtonProps) -> Self {
        Self {
            text_color: props.text_color,
            background_color: props.background_color,
            corner_radius_dp: props.corner_radius_dp,
            text_size_sp: props.text_size_sp,
            typeface: props.typeface,
        }
    }
}

impl ButtonProps {
    /// The state a freshly constructed `new Button(context)` is already in —
    /// the baseline `create` diffs against, so a control that asked for the
    /// platform defaults performs **zero** setter calls
    /// ([`super`]'s two-halves note).
    pub(crate) fn platform_default(slot: SlotId) -> Self {
        Self {
            slot,
            text: String::new(),
            enabled: true,
            text_color: None,
            background_color: None,
            text_size_sp: None,
            content_description: None,
            corner_radius_dp: None,
            typeface: Typeface::System,
            style: ButtonStyle::Plain,
        }
    }

    /// Decode a `Button` slot's params.
    ///
    /// Every field but the identity is optional and falls back to the
    /// platform default, so a payload from an older or newer api layer
    /// degrades instead of killing the slot
    /// (`NativeWidget::decode_props`'s contract).
    ///
    /// # Errors
    /// [`NativeWidgetError::Params`] when the reserved identity keys are
    /// missing.
    pub(crate) fn decode(params: &Params<'_>) -> Result<Self, NativeWidgetError> {
        Ok(Self {
            slot: slot_of(params)?,
            text: text_or_empty(params, TEXT),
            enabled: params.flag(ENABLED).unwrap_or(true),
            text_color: color(params, TEXT_COLOR),
            background_color: color(params, BACKGROUND_COLOR),
            text_size_sp: params.float(TEXT_SIZE_SP).map(|size| size as f32),
            content_description: owned_text(params, CONTENT_DESCRIPTION),
            corner_radius_dp: params.float(CORNER_RADIUS_DP).map(|radius| radius as f32),
            typeface: typeface::decode(params, TYPEFACE),
            style: params
                .int(BUTTON_STYLE)
                .map_or(ButtonStyle::Plain, ButtonStyle::from_wire),
        })
    }

    /// The [`Restyle`] for `old` → `new`, `None` when the style did not
    /// change. Platform arms apply it BEFORE the [`Self::plan`]: a restyle
    /// resets the caption and the style-owned look the plan then re-applies.
    #[cfg_attr(
        not(any(target_os = "android", target_os = "ios", target_os = "macos")),
        allow(dead_code)
    )]
    pub(crate) fn style_change(old: &Self, new: &Self) -> Option<Restyle> {
        (old.style != new.style).then_some(Restyle {
            style: new.style,
            clear_background: old.background_color.is_some() || old.corner_radius_dp.is_some(),
            clear_text_color: old.text_color.is_some(),
        })
    }

    /// The setter-call plan for `old` → `new`: one entry per field that
    /// actually changed, in declaration order (the tests pin the order).
    ///
    /// Across a style change the look fields ([`Look`]) are diffed against a
    /// freshly created button's instead of `old`'s, because the [`Restyle`]
    /// applied ahead of this plan has already reset them: every look value
    /// the new props carry is planned again.
    pub(crate) fn plan<'a>(old: &Self, new: &'a Self) -> Plan<'a> {
        let look = if old.style == new.style {
            Look::of(old)
        } else {
            Look::of(&Self::platform_default(new.slot))
        };
        let mut plan = Plan::new();
        if old.text != new.text {
            plan.push(Setter::Text(&new.text));
        }
        if old.enabled != new.enabled {
            plan.push(Setter::Enabled(new.enabled));
        }
        plan_color(
            &mut plan,
            look.text_color,
            new.text_color,
            Setter::TextColor,
        );
        plan_themed_background(&mut plan, look, new);
        if look.text_size_sp != new.text_size_sp
            && let Some(size) = new.text_size_sp
        {
            plan.push(Setter::TextSizeSp(size));
        }
        if look.typeface != new.typeface {
            plan.push(Setter::Typeface(new.typeface));
        }
        if old.content_description != new.content_description {
            plan.push(Setter::ContentDescription(
                new.content_description.as_deref(),
            ));
        }
        plan
    }
}

/// Plan [`Self::background_color`]/[`Self::corner_radius_dp`] together — the
/// two need ONE combined [`Setter::ThemedBackground`] whenever a corner
/// radius is present (theme ladder L2), because Android has no "round
/// this `ColorDrawable`'s corners" call; a background colour with no radius
/// keeps the plain flat [`Setter::BackgroundColor`] (a possible future
/// explicit-colour-only override, and the exact plan every test still
/// pins, since a payload with no `cornerRadiusDp` key decodes `None` on both
/// sides of any diff).
///
/// `old` is the [`Look`] the plan diffs against: the old props', or a fresh
/// button's across a style change.
///
/// [`Self`]: ButtonProps
fn plan_themed_background<'a>(plan: &mut Plan<'a>, old: Look, new: &'a ButtonProps) {
    if old.background_color == new.background_color && old.corner_radius_dp == new.corner_radius_dp
    {
        return;
    }
    match (new.background_color, new.corner_radius_dp) {
        (Some(fill), Some(radius_dp)) => plan.push(Setter::ThemedBackground { fill, radius_dp }),
        (Some(fill), None) => plan.push(Setter::BackgroundColor(fill)),
        (None, _) => {}
    }
}

#[cfg(target_os = "android")]
pub(crate) mod platform {
    //! The Android half: build the `Button` and hand its planned setters to
    //! [`crate::controls::platform`].

    use jni::objects::{JObject, JValue};
    use jni::refs::Global;
    use jni::{jni_sig, jni_str};

    use super::{Button, ButtonProps, ButtonStyle};
    use crate::NativeWidgetError;
    use crate::android::{NativeCtx, NativeView};
    use crate::controls::platform::{FRAME_CAPACITY, apply};
    use crate::controls::{Plan, Setter};
    use crate::events::{EventPayload, decode_click};
    use crate::runtime::{NativeEvent, NativeWidget, Params};

    /// The factory class that hosts the `@JvmStatic` button helpers (binary
    /// name). `buttonCreate` builds a `MaterialButton` in the requested style
    /// and falls back to a plain `android.widget.Button` under a theme
    /// Material cannot inflate under, so an app without a Material theme
    /// keeps the framework button it always had.
    const FACTORY_CLASS: &str = "dev.frust.nativewidgets.FrustNativeControlFactory";

    /// `FrustNativeControlFactory.buttonSetStyle(button, style)`: restyle a
    /// live button in place. The Kotlin side resets the background, shape,
    /// insets and caption colours from a reference button of `style`, which
    /// is the whole [`super::Restyle`] contract on this arm.
    fn set_style(
        ctx: &mut NativeCtx<'_, '_>,
        view: &JObject<'_>,
        style: ButtonStyle,
    ) -> Result<(), NativeWidgetError> {
        let class = ctx.class(FACTORY_CLASS)?;
        let wire = i32::try_from(style.wire()).unwrap_or(0);
        ctx.run_jni("FrustNativeControlFactory.buttonSetStyle", |env| {
            env.call_static_method(
                &class,
                jni_str!("buttonSetStyle"),
                jni_sig!("(Landroid/view/View;I)V"),
                &[JValue::Object(view), JValue::Int(wire)],
            )?
            .v()
        })
    }

    /// `FrustNativeControlFactory.buttonSetThemedBackground(button, fill,
    /// radiusPx)`: [`Setter::ThemedBackground`] for a button.
    ///
    /// The shared apply replaces the view's background with a
    /// `GradientDrawable`, and a `MaterialButton` whose background was
    /// replaced never gets its own back: a later restyle could only tint the
    /// leftover drawable. The Kotlin helper instead puts the fill and radius
    /// into the `MaterialButton`'s own background (tint, shape, no insets),
    /// which `buttonSetStyle` can reset to any style's; a framework `Button`
    /// still gets the `GradientDrawable`.
    fn set_themed_background(
        ctx: &mut NativeCtx<'_, '_>,
        view: &JObject<'_>,
        fill: i32,
        radius_dp: f32,
    ) -> Result<(), NativeWidgetError> {
        let radius_px = crate::android::theme::dp_to_px(ctx, radius_dp);
        let class = ctx.class(FACTORY_CLASS)?;
        ctx.run_jni(
            "FrustNativeControlFactory.buttonSetThemedBackground",
            |env| {
                env.call_static_method(
                    &class,
                    jni_str!("buttonSetThemedBackground"),
                    jni_sig!("(Landroid/view/View;IF)V"),
                    &[
                        JValue::Object(view),
                        JValue::Int(fill),
                        JValue::Float(radius_px),
                    ],
                )?
                .v()
            },
        )
    }

    /// Execute a whole [`Plan`], front to back, like the shared
    /// `apply_all`, except that [`Setter::ThemedBackground`] goes through
    /// [`set_themed_background`].
    fn apply_plan(
        ctx: &mut NativeCtx<'_, '_>,
        view: &JObject<'_>,
        plan: &Plan<'_>,
    ) -> Result<(), NativeWidgetError> {
        for setter in plan {
            match *setter {
                Setter::ThemedBackground { fill, radius_dp } => {
                    set_themed_background(ctx, view, fill, radius_dp)?;
                }
                ref other => apply(ctx, view, other)?,
            }
        }
        Ok(())
    }

    /// A live button's retained state.
    pub(crate) struct ButtonState {
        /// The button itself. This is a **second** global reference to the
        /// same object the runtime's [`NativeView`] holds: `update` is handed
        /// only the state, never the view, so the state has to own its own
        /// way back to the control. Both refs are released on dispose (this
        /// one when the state drops at the end of
        /// [`NativeWidget::dispose`]), so the leak bar is `2 × live controls`
        /// and still returns to zero.
        view: Global<JObject<'static>>,
    }

    impl NativeWidget for Button {
        type Props = ButtonProps;
        type State = ButtonState;

        fn decode_props(params: &Params<'_>) -> Result<Self::Props, NativeWidgetError> {
            ButtonProps::decode(params)
        }

        fn create(
            ctx: &mut NativeCtx<'_, '_>,
            props: &Self::Props,
        ) -> Result<(NativeView, Self::State), NativeWidgetError> {
            let wire = i32::try_from(props.style.wire()).unwrap_or(0);
            let view = ctx.with_frame(FRAME_CAPACITY, |ctx| {
                let class = ctx.class(FACTORY_CLASS)?;
                let activity = ctx.activity()?;
                let qualified = ctx.context()?;
                let view = ctx.run_jni("FrustNativeControlFactory.buttonCreate", |env| {
                    env.call_static_method(
                        &class,
                        jni_str!("buttonCreate"),
                        jni_sig!(
                            "(Landroid/app/Activity;Landroid/content/Context;I)Landroid/view/View;"
                        ),
                        &[
                            JValue::Object(activity),
                            JValue::Object(qualified),
                            JValue::Int(wire),
                        ],
                    )?
                    .l()
                })?;
                ctx.retain(&view)
            })?;
            let plan = ButtonProps::plan(&ButtonProps::platform_default(props.slot), props);
            ctx.with_frame(FRAME_CAPACITY, |ctx| apply_plan(ctx, &view, &plan))?;
            // Attached after the initial plan, matching `Switch`/`Slider`'s
            // create order (`switch.rs`'s module doc) — a click setter never
            // fires anyway, but the same order keeps the three controls
            // predictable.
            let listener = ctx.new_listener(props.slot)?;
            ctx.set_on_click_listener(&view, &listener)?;
            let handle = ctx.retain(&view)?;
            let listener_ref = ctx.retain(&listener)?;
            Ok((
                NativeView::with_extra(handle, vec![listener_ref]),
                ButtonState { view },
            ))
        }

        fn update(
            ctx: &mut NativeCtx<'_, '_>,
            state: &mut Self::State,
            old: &Self::Props,
            new: &Self::Props,
        ) -> Result<(), NativeWidgetError> {
            // The restyle goes first: it resets the style-owned look, and the
            // plan then re-applies every look value the new props carry.
            if let Some(restyle) = ButtonProps::style_change(old, new) {
                ctx.with_frame(FRAME_CAPACITY, |ctx| {
                    set_style(ctx, &state.view, restyle.style)
                })?;
            }
            let plan = ButtonProps::plan(old, new);
            ctx.with_frame(FRAME_CAPACITY, |ctx| apply_plan(ctx, &state.view, &plan))
        }

        fn on_event(_state: &mut Self::State, event: NativeEvent) -> Option<EventPayload> {
            decode_click(event)
        }

        fn dispose(
            ctx: &mut NativeCtx<'_, '_>,
            state: Self::State,
        ) -> Result<(), NativeWidgetError> {
            // Detach so a stray in-flight click can't fire after this slot's
            // instance is gone (tolerated either way —
            // `crate::runtime`'s late/duplicate disposal — but nothing keeps
            // the listener attached once this returns). Dropping `state`
            // releases its own global reference, and the runtime drops the
            // view's (and the listener's, retained in `extra`) immediately
            // afterwards — the paired delete.
            ctx.set_on_click_listener(&state.view, &JObject::null())
        }
    }
}

#[cfg(target_os = "ios")]
pub(crate) mod platform {
    //! The Apple half: build the `UIButton` and apply the same planned setters
    //! the Android half hands JNI.
    //!
    //! # `UIButtonType::System`, not a bare `UIButton::new`
    //!
    //! `ButtonProps::platform_default` describes *"the state a freshly
    //! constructed button is already in"*, and the create plan diffs against
    //! it — so which constructor runs decides which setters a default control
    //! pays for. `UIButton::new` yields `UIButtonType::Custom`: no title
    //! colour, no highlight behaviour, an invisible control until an app sets
    //! something. `buttonWithType(System)` yields UIKit's own button — the
    //! tinted title and press feedback a user recognizes — which is the honest
    //! analogue of Android's `new Button(context)` picking up the themed
    //! `Widget.Material.Button`.
    //!
    //! Together with the fill and title colour the theme ladder folds into
    //! `Props`, that closes an earlier prototype's *"black box with
    //! text"* cosmetic gap: a hand-built `Custom` button had gotten
    //! exactly that.
    //!
    //! # Clicks are platform-owned, exactly like Android's
    //!
    //! `create` attaches a [`FrustNativeControlTarget`] as the button's
    //! `TouchUpInside` action, and `on_event` decodes its firing
    //! via [`crate::events::decode_click`] into the runtime's event dispatch —
    //! the same decoder Android's `on_event` calls, so kind/detail parity is
    //! automatic (`crate::apple::events`'s module doc). Nothing about that
    //! goes through `RenderRoot::event` (the crate doc's event-bypass rule) —
    //! a click is never caused by `update`'s own setters, so unlike
    //! `Switch`/`Slider` this control needs no echo guard, on either
    //! platform.

    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::{MainThreadMarker, msg_send, sel};
    use objc2_foundation::NSString;
    use objc2_ui_kit::{UIButton, UIButtonType, UIColor, UIControlState};

    use super::{Button, ButtonProps, ButtonStyle, KIND, Restyle};
    use crate::NativeWidgetError;
    use crate::apple::{FrustNativeControlTarget, NativeCtx, NativeView};
    use crate::controls::platform;
    use crate::controls::{Plan, Setter};
    use crate::events::{EventPayload, decode_click};
    use crate::runtime::{NativeEvent, NativeWidget, Params};

    /// A live button's retained state.
    ///
    /// Two `Retained` fields, where the Android state holds two: one is the
    /// same "`update` needs its own reference beside the runtime's" reason
    /// (`button.rs`'s Android note) — except here ARC makes that extra retain
    /// free and untracked, with no paired-delete discipline to keep. The
    /// second, `target`, is the one retention this arm genuinely needs
    /// explicitly: `UIControl` holds its target-action pair **weakly**, so
    /// nothing but this field keeps the target alive for the slot's lifetime
    /// (`crate::apple::events`'s module doc's *Target retention*).
    pub(crate) struct ButtonState {
        /// The button, kept typed: `update` needs `UIButton`'s own
        /// state-keyed setters, which a `UIView` handle could not reach.
        view: Retained<UIButton>,
        /// The target-action object `create` attached as `view`'s
        /// `TouchUpInside` action. Retained here only; dropped (and thereby
        /// released) alongside the rest of `State` when
        /// `Instance::dispose` tears this slot down.
        target: Retained<FrustNativeControlTarget>,
        /// Theme ladder L3: the combined typeface/size state
        /// `Setter::TextSizeSp`/`Setter::Typeface` share — see
        /// `crate::controls::platform::FontState`'s doc for why a `Button`
        /// needs this at all.
        font: platform::FontState,
    }

    impl NativeWidget for Button {
        type Props = ButtonProps;
        type State = ButtonState;

        fn decode_props(params: &Params<'_>) -> Result<Self::Props, NativeWidgetError> {
            ButtonProps::decode(params)
        }

        fn create(
            ctx: &mut NativeCtx<'_, '_>,
            props: &Self::Props,
        ) -> Result<(NativeView, Self::State), NativeWidgetError> {
            let mtm = ctx.mtm();
            let view = UIButton::buttonWithType(UIButtonType::System, mtm);
            let mut font = platform::FontState::platform_default();
            let default = ButtonProps::platform_default(props.slot);
            // The style first: installing a configuration resets the caption,
            // which the plan then writes.
            if let Some(restyle) = ButtonProps::style_change(&default, props) {
                apply_style(&view, restyle, &props.text);
            }
            let plan = ButtonProps::plan(&default, props);
            apply_all(mtm, &view, &mut font, &plan);
            // Attached after the initial plan, matching the Android arm's
            // create order (`switch.rs`'s module doc) — a click setter never
            // fires anyway, but the same order keeps the three controls
            // predictable.
            let target = FrustNativeControlTarget::attach_button(mtm, props.slot, &view);
            let handle = NativeView::new(Retained::clone(&view).into_super().into_super(), mtm);
            Ok((handle, ButtonState { view, target, font }))
        }

        fn update(
            ctx: &mut NativeCtx<'_, '_>,
            state: &mut Self::State,
            old: &Self::Props,
            new: &Self::Props,
        ) -> Result<(), NativeWidgetError> {
            if let Some(restyle) = ButtonProps::style_change(old, new) {
                apply_style(&state.view, restyle, &new.text);
            }
            apply_all(
                ctx.mtm(),
                &state.view,
                &mut state.font,
                &ButtonProps::plan(old, new),
            );
            Ok(())
        }

        fn on_event(_state: &mut Self::State, event: NativeEvent) -> Option<EventPayload> {
            decode_click(event)
        }

        fn dispose(
            _ctx: &mut NativeCtx<'_, '_>,
            state: Self::State,
        ) -> Result<(), NativeWidgetError> {
            // Detach so a stray in-flight click can't fire after this slot's
            // instance is gone (tolerated either way —
            // `crate::runtime`'s late/duplicate disposal — mirroring the
            // Android arm's explicit `setOnClickListener(null)`). Dropping
            // `state` afterwards releases the target's own retain — ARC's
            // own paired delete.
            state.target.detach_button(&state.view);
            Ok(())
        }
    }

    /// The `UIButtonConfiguration` for `style`, `None` for the legacy
    /// system button.
    ///
    /// `objc2-ui-kit`'s `UIButtonConfiguration` binding sits behind a cargo
    /// feature this crate does not enable, so the class is reached by name at
    /// run time, the way the chrome's bar-item badge is: every constructor
    /// is `responds_to`-guarded, which is also what makes the glass styles
    /// safe below iOS 26. A glass style falls back to its non-glass
    /// counterpart ([`ButtonStyle::Glass`] to plain,
    /// [`ButtonStyle::ProminentGlass`] to filled) when the system has no
    /// glass configuration.
    ///
    /// The constructors are class methods, so they are looked up on the
    /// metaclass: `AnyClass::responds_to` on the class itself answers for
    /// instance methods.
    fn configuration_for(style: ButtonStyle) -> Option<Retained<AnyObject>> {
        let class = AnyClass::get(c"UIButtonConfiguration")?;
        let factories = class.metaclass();
        let filled = factories.responds_to(sel!(filledButtonConfiguration));
        // SAFETY: each selector is a documented class method of
        // `UIButtonConfiguration` returning a configuration object, and was
        // just checked with `responds_to` on the metaclass.
        unsafe {
            match style {
                ButtonStyle::Plain => None,
                ButtonStyle::Prominent | ButtonStyle::Destructive if filled => {
                    msg_send![class, filledButtonConfiguration]
                }
                ButtonStyle::Glass if factories.responds_to(sel!(glassButtonConfiguration)) => {
                    msg_send![class, glassButtonConfiguration]
                }
                ButtonStyle::ProminentGlass
                    if factories.responds_to(sel!(prominentGlassButtonConfiguration)) =>
                {
                    msg_send![class, prominentGlassButtonConfiguration]
                }
                ButtonStyle::ProminentGlass if filled => {
                    msg_send![class, filledButtonConfiguration]
                }
                _ => None,
            }
        }
    }

    /// Restyle `view` in place and put `text` back: installing (or
    /// removing) a configuration replaces the caption the legacy
    /// per-state title held.
    ///
    /// The theme look the old props applied is dropped first ([`Restyle`]):
    /// the view's own `backgroundColor` and the layer's corner radius would
    /// otherwise stay under the new configuration's background, and the
    /// state-keyed title colour over its caption colour. The plan that runs
    /// next re-applies whatever the new props carry.
    fn apply_style(view: &UIButton, restyle: Restyle, text: &str) {
        let style = restyle.style;
        if restyle.clear_background {
            view.setBackgroundColor(None);
            let layer = view.layer();
            layer.setCornerRadius(0.0);
            layer.setMasksToBounds(false);
        }
        if restyle.clear_text_color {
            view.setTitleColor_forState(None, UIControlState::Normal);
        }
        let configuration = configuration_for(style);
        if let Some(configuration) = &configuration {
            if style == ButtonStyle::Destructive {
                let red = UIColor::systemRedColor();
                // SAFETY: `setBaseBackgroundColor:` takes a nullable `UIColor`.
                let _: () = unsafe { msg_send![&**configuration, setBaseBackgroundColor: &*red] };
            }
            let title = NSString::from_str(text);
            // SAFETY: `setTitle:` takes a nullable `NSString`.
            let _: () = unsafe { msg_send![&**configuration, setTitle: &*title] };
        }
        // SAFETY: `setConfiguration:` takes a nullable `UIButtonConfiguration`
        // and copies it; nil restores the legacy system button.
        let _: () = unsafe { msg_send![view, setConfiguration: configuration.as_deref()] };
        view.setTitle_forState(Some(&NSString::from_str(text)), UIControlState::Normal);
    }

    /// Set the caption. A configuration-styled button shows its
    /// configuration's title, so the copy it hands back is updated too.
    fn set_title(view: &UIButton, text: &str) {
        let title = NSString::from_str(text);
        view.setTitle_forState(Some(&title), UIControlState::Normal);
        // SAFETY: `configuration` returns a copy (or nil on the legacy
        // button); `setTitle:` and `setConfiguration:` are nullable setters.
        unsafe {
            let current: Option<Retained<AnyObject>> = msg_send![view, configuration];
            if let Some(current) = current {
                let _: () = msg_send![&*current, setTitle: &*title];
                let _: () = msg_send![view, setConfiguration: &*current];
            }
        }
    }

    /// Execute a whole [`Plan`], front to back — the same order contract the
    /// Android arm's `apply_all` keeps.
    fn apply_all(
        mtm: MainThreadMarker,
        view: &UIButton,
        font: &mut platform::FontState,
        plan: &Plan<'_>,
    ) {
        for setter in plan {
            apply(mtm, view, font, setter);
        }
    }

    /// Execute one planned property write against `view`.
    fn apply(
        mtm: MainThreadMarker,
        view: &UIButton,
        font: &mut platform::FontState,
        setter: &Setter<'_>,
    ) {
        match *setter {
            // A `UIButton`'s caption is per-control-state, unlike a
            // `TextView`'s: `Normal` is the base every other state falls back
            // to, so setting it alone is what "the button's title" means.
            Setter::Text(text) => set_title(view, text),
            Setter::Enabled(enabled) => view.setEnabled(enabled),
            Setter::TextColor(argb) => {
                view.setTitleColor_forState(
                    Some(&platform::ui_color(argb)),
                    UIControlState::Normal,
                );
            }
            Setter::BackgroundColor(argb) => platform::set_background_color(view, argb),
            Setter::TextSizeSp(sp) => {
                let resolved = font.apply_size(sp);
                // `titleLabel` is documented non-null for every stock button
                // type, but the binding is `Option`-typed, so a missing one
                // degrades to "no size change" rather than a panic — the
                // combined-font state above still records the size for a
                // later combining apply.
                if let Some(label) = view.titleLabel() {
                    platform::set_label_font(&label, resolved.as_ui_font());
                }
            }
            Setter::ThemedBackground { fill, radius_dp } => {
                platform::set_background_color(view, fill);
                platform::set_corner_radius(view, radius_dp);
            }
            Setter::Typeface(face) => {
                let resolved = font.apply_typeface(face);
                if let Some(label) = view.titleLabel() {
                    platform::set_label_font(&label, resolved.as_ui_font());
                }
            }
            Setter::ContentDescription(label) => {
                platform::set_accessibility_label(view, label, mtm);
            }
            ref other => platform::warn_unexpected_setter(KIND, other),
        }
    }
}

#[cfg(target_os = "macos")]
pub(crate) mod platform {
    //! The macOS half: build an `NSButton` and apply the same planned setters
    //! the Android and iOS halves do.
    //!
    //! # A momentary push button with the push bezel
    //!
    //! `ButtonProps::platform_default` describes *"the state a freshly
    //! constructed button is already in"*, and the create plan diffs against
    //! it, so construction must land exactly there. `NSButton::new` (i.e.
    //! `initWithFrame:` with a zero frame) yields a momentary push-in button
    //! titled `"Button"`; this arm pins its type and bezel explicitly —
    //! `NSButtonType::MomentaryPushIn` and `NSBezelStyle::Push` (the modern
    //! name of the deprecated `NSBezelStyle::Rounded` alias in objc2-app-kit
    //! 0.3.2: the standard rounded push button, which picks up the system
    //! accent/appearance like `UIButtonType::System` does on iOS) — and clears
    //! the placeholder title, so an empty caption in `Props` really is an empty
    //! button rather than one reading "Button".
    //!
    //! # Clicks are platform-owned, exactly like the other two arms
    //!
    //! `create` attaches a [`FrustNativeControlTarget`] as the button's
    //! target/action with [`EVENT_KIND_CLICK`], and `on_event` decodes its
    //! firing via [`crate::events::decode_click`] — the same decoder Android's
    //! and iOS's `on_event` call. The click reaches the button directly
    //! through AppKit's responder chain (the desktop host parents it above
    //! frust's surface and adds no input shield), never through
    //! `RenderRoot::event`; a click is never caused by `update`'s own setters,
    //! so no echo guard.

    use objc2::rc::Retained;
    use objc2_app_kit::{NSBezelStyle, NSButton, NSButtonType, NSColor};
    use objc2_foundation::NSString;

    use super::{Button, ButtonProps, ButtonStyle, KIND, Restyle};
    use crate::NativeWidgetError;
    use crate::appkit::{FrustNativeControlTarget, NativeCtx, NativeView};
    use crate::controls::platform;
    use crate::controls::{Plan, Setter};
    use crate::events::{EVENT_KIND_CLICK, EventPayload, decode_click};
    use crate::runtime::{NativeEvent, NativeWidget, Params};

    /// A live button's retained state — the iOS arm's shape: the typed
    /// button beside the runtime's own `NSView` reference, and the
    /// target-action object nothing else retains (`NSControl.target` is weak —
    /// `crate::appkit::events`' *Target retention*).
    pub(crate) struct ButtonState {
        /// The button, kept typed: `update` needs `NSButton`'s own
        /// `setTitle:`, which an `NSView` handle could not reach.
        view: Retained<NSButton>,
        /// The target `create` attached; released with the rest of `State`
        /// when `Instance::dispose` tears the slot down.
        target: Retained<FrustNativeControlTarget>,
    }

    impl NativeWidget for Button {
        type Props = ButtonProps;
        type State = ButtonState;

        fn decode_props(params: &Params<'_>) -> Result<Self::Props, NativeWidgetError> {
            ButtonProps::decode(params)
        }

        fn create(
            ctx: &mut NativeCtx<'_, '_>,
            props: &Self::Props,
        ) -> Result<(NativeView, Self::State), NativeWidgetError> {
            let mtm = ctx.mtm();
            let view = NSButton::new(mtm);
            view.setButtonType(NSButtonType::MomentaryPushIn);
            view.setBezelStyle(NSBezelStyle::Push);
            // Module doc: land exactly on `platform_default`'s empty caption.
            view.setTitle(&NSString::from_str(""));
            let default = ButtonProps::platform_default(props.slot);
            if let Some(restyle) = ButtonProps::style_change(&default, props) {
                apply_style(&view, restyle);
            }
            let plan = ButtonProps::plan(&default, props);
            apply_all(&view, &plan);
            // Attached after the initial plan, matching the other two arms'
            // create order.
            let target = FrustNativeControlTarget::attach(mtm, &view, props.slot, EVENT_KIND_CLICK);
            let handle = NativeView::new(Retained::clone(&view).into_super().into_super(), mtm);
            Ok((handle, ButtonState { view, target }))
        }

        fn update(
            _ctx: &mut NativeCtx<'_, '_>,
            state: &mut Self::State,
            old: &Self::Props,
            new: &Self::Props,
        ) -> Result<(), NativeWidgetError> {
            if let Some(restyle) = ButtonProps::style_change(old, new) {
                apply_style(&state.view, restyle);
            }
            apply_all(&state.view, &ButtonProps::plan(old, new));
            Ok(())
        }

        fn on_event(_state: &mut Self::State, event: NativeEvent) -> Option<EventPayload> {
            decode_click(event)
        }

        fn dispose(
            _ctx: &mut NativeCtx<'_, '_>,
            state: Self::State,
        ) -> Result<(), NativeWidgetError> {
            // Detach so a stray in-flight click can't reach a torn-down slot,
            // mirroring the other two arms; dropping `state` afterwards
            // releases the target's only retain.
            state.target.detach(&state.view);
            Ok(())
        }
    }

    /// Restyle `view` in place: the push bezel, tinted with the accent colour
    /// for the prominent styles and the system red (with AppKit's destructive
    /// flag) for [`ButtonStyle::Destructive`]. `NSBezelStyle::Glass` needs
    /// macOS 26 and this crate has no cheap version gate, so
    /// [`ButtonStyle::Glass`] keeps the plain push bezel and
    /// [`ButtonStyle::ProminentGlass`] is the accent-tinted one.
    ///
    /// The theme look the old props applied is dropped first ([`Restyle`]):
    /// the backing layer's fill and corner radius would otherwise stay under
    /// the new bezel colour, and the content tint over the caption. The
    /// bezel colour itself is set for every style below; the plan that runs
    /// next re-applies whatever the new props carry.
    fn apply_style(view: &NSButton, restyle: Restyle) {
        if restyle.clear_background
            && let Some(layer) = view.layer()
        {
            layer.setBackgroundColor(None);
            layer.setCornerRadius(0.0);
            layer.setMasksToBounds(false);
        }
        if restyle.clear_text_color {
            view.setContentTintColor(None);
        }
        view.setBezelStyle(NSBezelStyle::Push);
        match restyle.style {
            ButtonStyle::Plain | ButtonStyle::Glass => {
                view.setBezelColor(None);
                view.setHasDestructiveAction(false);
            }
            ButtonStyle::Prominent | ButtonStyle::ProminentGlass => {
                view.setBezelColor(Some(&NSColor::controlAccentColor()));
                view.setHasDestructiveAction(false);
            }
            ButtonStyle::Destructive => {
                view.setBezelColor(Some(&NSColor::systemRedColor()));
                view.setHasDestructiveAction(true);
            }
        }
    }

    /// Execute a whole [`Plan`], front to back — the same order contract the
    /// other two arms keep.
    fn apply_all(view: &NSButton, plan: &Plan<'_>) {
        for setter in plan {
            apply(view, setter);
        }
    }

    /// Execute one planned property write against `view`.
    fn apply(view: &NSButton, setter: &Setter<'_>) {
        match *setter {
            Setter::Text(text) => view.setTitle(&NSString::from_str(text)),
            Setter::Enabled(enabled) => platform::set_enabled(view, enabled),
            Setter::TextColor(argb) => platform::set_text_color(view, argb),
            Setter::BackgroundColor(argb) => platform::set_background_color(view, argb),
            Setter::TextSizeSp(sp) => platform::set_text_size(view, sp),
            Setter::ThemedBackground { fill, radius_dp } => {
                platform::set_background_color(view, fill);
                platform::set_corner_radius(view, radius_dp);
            }
            Setter::Typeface(face) => platform::set_typeface(view, face),
            Setter::ContentDescription(label) => platform::set_accessibility_label(view, label),
            ref other => platform::warn_unexpected_setter(KIND, other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::with_identity;

    fn params(body: &str) -> String {
        with_identity(KIND, 7, body)
    }

    fn decode(body: &str) -> ButtonProps {
        ButtonProps::decode(&Params::new(&params(body))).expect("decodes")
    }

    #[test]
    fn absent_fields_decode_to_the_platform_defaults() {
        let props = decode("");
        assert_eq!(props, ButtonProps::platform_default(7));
        // ...and therefore create plans nothing at all.
        assert!(ButtonProps::plan(&ButtonProps::platform_default(7), &props).is_empty());
    }

    #[test]
    fn the_create_plan_sets_exactly_the_non_default_fields() {
        let props = decode(
            "\"text\":\"Save\",\"enabled\":false,\"textColor\":-1,\"backgroundColor\":255,\
             \"textSizeSp\":18.5,\"contentDescription\":\"Save the note\"",
        );
        assert_eq!(
            ButtonProps::plan(&ButtonProps::platform_default(7), &props),
            vec![
                Setter::Text("Save"),
                Setter::Enabled(false),
                Setter::TextColor(-1),
                Setter::BackgroundColor(255),
                Setter::TextSizeSp(18.5),
                Setter::ContentDescription(Some("Save the note")),
            ]
        );
    }

    #[test]
    fn an_unchanged_field_never_reaches_the_platform() {
        let old = decode("\"text\":\"Save\",\"textColor\":16711680");
        let new = decode("\"text\":\"Saved\",\"textColor\":16711680");
        // Only the caption changed: the (35× more expensive) text setter runs
        // alone, and the colour is not re-applied.
        assert_eq!(ButtonProps::plan(&old, &new), vec![Setter::Text("Saved")]);
        assert_eq!(ButtonProps::plan(&new, &new), vec![]);
    }

    #[test]
    fn clearing_a_color_plans_nothing_but_clearing_a_label_does() {
        let with_color = decode("\"textColor\":123,\"contentDescription\":\"a\"");
        let without = decode("");
        // No platform call can restore a text colour...
        assert_eq!(
            ButtonProps::plan(&with_color, &without),
            vec![Setter::ContentDescription(None)]
        );
        // ...but re-applying one is a normal cheap setter.
        assert_eq!(
            ButtonProps::plan(&without, &with_color),
            vec![
                Setter::TextColor(123),
                Setter::ContentDescription(Some("a")),
            ]
        );
    }

    #[test]
    fn params_without_identity_are_a_params_error() {
        assert!(matches!(
            ButtonProps::decode(&Params::new("{\"text\":\"x\"}")),
            Err(NativeWidgetError::Params(_))
        ));
    }

    // --- theme ladder L2: the combined themed-background setter ------------

    #[test]
    fn a_fill_with_a_radius_plans_the_combined_themed_background() {
        let props = decode("\"backgroundColor\":255,\"cornerRadiusDp\":6.0");
        assert_eq!(props.corner_radius_dp, Some(6.0));
        assert_eq!(
            ButtonProps::plan(&ButtonProps::platform_default(7), &props),
            vec![Setter::ThemedBackground {
                fill: 255,
                radius_dp: 6.0,
            }]
        );
    }

    #[test]
    fn a_fill_with_no_radius_keeps_the_pre_p1_07_flat_setter() {
        // No `cornerRadiusDp` key at all: exactly the plain-fill shape, still
        // the plan a future explicit-colour-only override takes.
        let props = decode("\"backgroundColor\":255");
        assert_eq!(props.corner_radius_dp, None);
        assert_eq!(
            ButtonProps::plan(&ButtonProps::platform_default(7), &props),
            vec![Setter::BackgroundColor(255)]
        );
    }

    #[test]
    fn a_radius_change_alone_replans_the_themed_background() {
        let old = decode("\"backgroundColor\":255,\"cornerRadiusDp\":6.0");
        let new = decode("\"backgroundColor\":255,\"cornerRadiusDp\":10.0");
        assert_eq!(
            ButtonProps::plan(&old, &new),
            vec![Setter::ThemedBackground {
                fill: 255,
                radius_dp: 10.0,
            }]
        );
        assert_eq!(ButtonProps::plan(&new, &new), vec![]);
    }

    #[test]
    fn clearing_a_radius_back_to_a_flat_fill_falls_back_to_background_color() {
        let themed = decode("\"backgroundColor\":255,\"cornerRadiusDp\":6.0");
        let flat = decode("\"backgroundColor\":255");
        assert_eq!(
            ButtonProps::plan(&themed, &flat),
            vec![Setter::BackgroundColor(255)]
        );
    }

    // --- theme ladder L3: the typeface setter, and Props gating ------------

    #[test]
    fn typeface_defaults_to_system_when_absent() {
        let props = decode("");
        assert_eq!(props.typeface, Typeface::System);
        assert_eq!(props, ButtonProps::platform_default(7));
    }

    #[test]
    fn a_typeface_change_alone_plans_exactly_one_setter() {
        let old = decode("\"typeface\":\"glyphPlex\"");
        let new = decode("\"typeface\":\"glyphMono\"");
        assert_eq!(
            ButtonProps::plan(&old, &new),
            vec![Setter::Typeface(Typeface::GlyphMono)]
        );
    }

    #[test]
    fn an_unchanged_typeface_plans_nothing_the_zero_ffi_property() {
        let props = decode("\"typeface\":\"glyphMono\"");
        assert_eq!(
            ButtonProps::plan(&props, &props),
            vec![],
            "identical typeface Props must plan no setter — the whole-struct \
             PartialEq gate this crate's `Props: PartialEq` contract relies on"
        );
    }

    // --- button styles ---------------------------------------------------

    #[test]
    fn a_default_button_decodes_plain_and_plans_no_restyle() {
        let props = decode("");
        assert_eq!(props.style, ButtonStyle::Plain);
        assert_eq!(
            ButtonProps::style_change(&ButtonProps::platform_default(7), &props),
            None
        );
    }

    #[test]
    fn every_style_round_trips_through_its_wire_value() {
        for style in [
            ButtonStyle::Plain,
            ButtonStyle::Prominent,
            ButtonStyle::Destructive,
            ButtonStyle::Glass,
            ButtonStyle::ProminentGlass,
        ] {
            let props = decode(&format!("\"{BUTTON_STYLE}\":{}", style.wire()));
            assert_eq!(props.style, style);
        }
    }

    #[test]
    fn the_wire_values_are_append_only() {
        assert_eq!(
            [
                ButtonStyle::Plain.wire(),
                ButtonStyle::Prominent.wire(),
                ButtonStyle::Destructive.wire(),
                ButtonStyle::Glass.wire(),
                ButtonStyle::ProminentGlass.wire(),
            ],
            [0, 1, 2, 3, 4]
        );
        // A value from a newer api layer degrades to Plain.
        assert_eq!(decode("\"buttonStyle\":99").style, ButtonStyle::Plain);
    }

    #[test]
    fn a_style_change_is_an_update_not_a_remount() {
        let old = decode("\"text\":\"Go\",\"buttonStyle\":1");
        let new = decode("\"text\":\"Go\",\"buttonStyle\":2");
        assert_ne!(old, new, "the style is a diffed prop");
        assert_eq!(
            ButtonProps::style_change(&old, &new),
            Some(Restyle {
                style: ButtonStyle::Destructive,
                clear_background: false,
                clear_text_color: false,
            })
        );
        // Same slot, no setter planned: the platform restyles the live view.
        assert_eq!(old.slot, new.slot);
        assert_eq!(ButtonProps::plan(&old, &new), vec![]);
        assert_eq!(ButtonProps::style_change(&new, &new), None);
    }

    #[test]
    fn a_restyle_replans_the_look_against_a_fresh_button() {
        let plain = decode(
            "\"text\":\"Go\",\"textColor\":-1,\"backgroundColor\":255,\
             \"cornerRadiusDp\":6.0,\"textSizeSp\":15.0,\"typeface\":\"glyphMono\"",
        );
        let styled = decode(
            "\"text\":\"Go\",\"buttonStyle\":1,\"textSizeSp\":15.0,\"typeface\":\"glyphMono\"",
        );
        // The restyle drops the old theme look; the plan puts back the size
        // and face, which a style may reset too, exactly as `create` would.
        let fresh = ButtonProps::plan(&ButtonProps::platform_default(7), &styled);
        assert_eq!(
            fresh,
            vec![
                Setter::Text("Go"),
                Setter::TextSizeSp(15.0),
                Setter::Typeface(Typeface::GlyphMono),
            ]
        );
        assert_eq!(ButtonProps::plan(&plain, &styled), fresh[1..]);
        // Without a style change the same fields stay diffed against `old`.
        assert_eq!(ButtonProps::plan(&styled, &styled), vec![]);
    }
}
