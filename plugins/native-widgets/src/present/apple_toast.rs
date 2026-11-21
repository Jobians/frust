//! The iOS/iPadOS toast arm: UIKit has no system toast, so this arm builds a
//! plugin-owned capsule view in the key window, on the main thread, over
//! `super::apple_host`'s shared helpers (the main-queue hop, host discovery
//! and the live-presentation guard, where a toast is its own kind).
//!
//! # The view
//!
//! | Part | UIKit |
//! |---|---|
//! | the capsule | a `UIVisualEffectView`, corner radius half its minimum height, clipped: `UIGlassEffect` (Regular) when [`ToastSpec::glass`] is set **and** the class exists at run time (iOS 26), else `UIBlurEffect` `SystemMaterial` |
//! | `message` | a `UILabel`, system 15 pt, `labelColor`, up to three lines |
//! | `action` | a system `UIButton`, bold 15 pt, wearing the window's tint, target-action on the toast's own target object |
//! | VoiceOver | `UIAccessibilityAnnouncementNotification` with the message when it appears; with VoiceOver running, a toast with an action stays up for [`ToastDuration::MAX`] so the action can be reached |
//! | swipe | a `UIPanGestureRecognizer` on the capsule: dragged down far or fast enough it leaves → [`ToastOutcome::Dismissed`], else it springs back |
//!
//! # Placement
//!
//! The capsule is inserted into the key window directly **above the root
//! view controller's view** — so above the tab bar, the navigation bars and
//! the toolbar, which live in that view — and below anything presented over
//! it (an alert or a sheet covers a toast, as on Android). Horizontally it
//! hugs its content, centred, at most 560 pt wide and 16 pt inside the safe
//! area. Vertically its bottom sits 12 pt above the highest of: the window's
//! safe area, the keyboard (the `keyboardLayoutGuide`, iOS 15+, of the root
//! controller's view when that view is in the window, else the window's;
//! it tracks the docked keyboard and otherwise stands at the safe area),
//! and the safe area of the selected page's view (the root walked through
//! `UITabBarController.selectedViewController` and
//! `UINavigationController.topViewController`), whose bottom inset includes
//! the tab bar and the toolbar. Each constraint is added only when its
//! anchor shares the window (UIKit raises on anchors with no common
//! ancestor).
//!
//! # Resolution
//!
//! Every path takes the toast's live entry back **by generation**
//! (`apple_host::take_live_as`), resolves through `super::toast::resolve`
//! (which drops a report for a toast that is no longer current) and removes
//! the view from the completion of its exit animation, exactly once (the
//! completion owns the view and the target, and takes them once):
//!
//! - **the duration runs out** (a `dispatch_after` on the main queue) →
//!   [`ToastOutcome::TimedOut`]; while the user is dragging the capsule the
//!   timer re-arms instead; a capsule whose window has gone resolves
//!   `Dismissed`;
//! - **the action** → [`ToastOutcome::Action`];
//! - **a swipe down** → [`ToastOutcome::Dismissed`];
//! - **[`super::dismiss`]** → [`ToastOutcome::Dismissed`];
//! - **a newer toast** displaces this entry (`apple_host::install_live`):
//!   its view leaves the same way; its outcome was already resolved
//!   `Dismissed` by `super::toast` when the newer one was accepted;
//! - **no window** when the main-queue step runs → [`PresentError::NoHost`].
//!
//! # `unsafe`
//!
//! Each block has its own `SAFETY` comment: `NSObject`'s `init` on the
//! target, the selector-based `addTarget:action:forControlEvents:` and
//! `initWithTarget:action:`, the gesture's `state` (not bound by
//! `objc2-ui-kit`), `selectedViewController` and the accessibility
//! announcement. `UIGlassEffect` is named only after a run-time check that
//! the class exists (the metaclass answers its constructor).

#[cfg(not(target_os = "ios"))]
compile_error!("the UIKit toast arm targets iOS only");

use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyClass, AnyObject, Bool, NSObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_core_foundation::{CGAffineTransform, CGFloat};
use objc2_foundation::{NSArray, NSString};
use objc2_ui_kit::{
    NSLayoutConstraint, UIAccessibilityAnnouncementNotification, UIAccessibilityIsVoiceOverRunning,
    UIAccessibilityPostNotification, UIBlurEffect, UIBlurEffectStyle, UIButton, UIButtonType,
    UIColor, UIControlEvents, UIControlState, UIFont, UIGestureRecognizerState, UIGlassEffect,
    UIGlassEffectStyle, UILabel, UILayoutConstraintAxis, UILayoutPriorityDefaultLow,
    UILayoutPriorityRequired, UINavigationController, UIPanGestureRecognizer, UIStackView,
    UIStackViewAlignment, UITabBarController, UIView, UIViewAnimationOptions, UIViewController,
    UIVisualEffect, UIVisualEffectView, UIWindow,
};

use super::apple_host::{
    AfterDismiss, LivePresentation, anchor_window, install_live, on_main, on_main_after,
    take_live_as,
};
use super::toast::{ToastHost, resolve};
use super::{PresentError, ToastDuration, ToastOutcome, ToastSpec};
use crate::controls::platform::{set_label_font, set_label_text_color};

/// The capsule's minimum height; its corner radius is half of it.
const MIN_HEIGHT: CGFloat = 48.0;
/// The widest the capsule grows.
const MAX_WIDTH: CGFloat = 560.0;
/// The capsule's distance from the safe area's sides and from what is below it.
const EDGE_MARGIN: CGFloat = 16.0;
const BOTTOM_MARGIN: CGFloat = 12.0;
/// The content's inset inside the capsule.
const SIDE_INSET: CGFloat = 20.0;
const VERTICAL_INSET: CGFloat = 12.0;
/// The space between the message and the action.
const SPACING: CGFloat = 12.0;
/// How far below its resting place the capsule enters from and leaves to.
const TRAVEL: CGFloat = 24.0;
/// A drag past this distance (or flung faster than [`SWIPE_VELOCITY`])
/// dismisses; anything less springs back.
const SWIPE_DISTANCE: CGFloat = 40.0;
const SWIPE_VELOCITY: CGFloat = 500.0;
/// How long the timer waits again when it fires mid-drag.
const DRAG_GRACE: Duration = Duration::from_secs(1);

/// The iOS host: `super::toast::ToastHost` over a plugin-owned window view.
pub(crate) struct Host;

impl ToastHost for Host {
    fn show_toast(spec: ToastSpec, generation: u64) -> Result<(), PresentError> {
        on_main(move |mtm| start(mtm, spec, generation));
        Ok(())
    }

    fn dismiss(generation: u64) {
        on_main(move |mtm| {
            if let Some(live) = take_live_as::<LiveToast>(mtm, generation) {
                live.finish(mtm, Ok(ToastOutcome::Dismissed));
            }
        });
    }
}

/// Run a UIKit callback's body without letting a panic unwind into UIKit's
/// frames (`docs/PLUGINS_CODE_STANDARDS.md`'s no-unwind-near-FFI rule).
fn guarded(which: &str, body: impl FnOnce()) {
    if catch_unwind(AssertUnwindSafe(body)).is_err() {
        log::error!("frust-native-widgets: the toast's {which} callback panicked");
    }
}

/// A vertical translation.
fn translate(ty: CGFloat) -> CGAffineTransform {
    CGAffineTransform {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        tx: 0.0,
        ty,
    }
}

/// Whether `a` and `b` are the same object.
fn same(a: &AnyObject, b: &AnyObject) -> bool {
    std::ptr::eq(a, b)
}

/// The main-queue half of a request: build the capsule in the key window,
/// park it (displacing the toast it replaces), animate it in, announce it
/// and arm its timeout.
fn start(mtm: MainThreadMarker, spec: ToastSpec, generation: u64) {
    let Some(window) = anchor_window(mtm) else {
        resolve(generation, Err(PresentError::NoHost));
        return;
    };
    let target = FrustNativeToastTarget::new(mtm, generation);
    let container = build_capsule(mtm, &spec, &target);
    let root_view = insert_into(&window, &container);
    activate_placement(mtm, &window, root_view.as_deref(), &container);

    container.setAlpha(0.0);
    container.setTransform(translate(TRAVEL));
    window.layoutIfNeeded();

    let live = LiveToast {
        generation,
        container: container.clone(),
        window: Weak::from_retained(&window),
        target,
    };
    if let Some(replaced) = install_live(mtm, generation, Box::new(live)) {
        replaced.dismiss(mtm, None);
    }

    let appear = RcBlock::new(move || {
        container.setAlpha(1.0);
        container.setTransform(translate(0.0));
    });
    UIView::animateWithDuration_delay_options_animations_completion(
        0.25,
        0.0,
        UIViewAnimationOptions::CurveEaseOut | UIViewAnimationOptions::AllowUserInteraction,
        &appear,
        None,
        mtm,
    );

    let message = NSString::from_str(&spec.message);
    let argument: &AnyObject = &message;
    // SAFETY: `UIAccessibilityAnnouncementNotification` is a UIKit constant
    // initialized before any Rust code runs, and an announcement's argument
    // is the `NSString` to speak, which `argument` is.
    unsafe {
        UIAccessibilityPostNotification(UIAccessibilityAnnouncementNotification, Some(argument));
    }

    let duration = if spec.action.is_some() && UIAccessibilityIsVoiceOverRunning() {
        ToastDuration::MAX
    } else {
        spec.duration.resolved()
    };
    arm_timeout(generation, duration);
}

/// Time toast `generation` out after `delay`, unless it is gone by then.
fn arm_timeout(generation: u64, delay: Duration) {
    on_main_after(delay, move |mtm| {
        let Some(live) = take_live_as::<LiveToast>(mtm, generation) else {
            return;
        };
        if live.target.ivars().dragging.get() {
            // The user holds it: give them a moment more.
            if let Some(unexpected) = install_live(mtm, generation, live) {
                unexpected.dismiss(mtm, None);
            }
            arm_timeout(generation, DRAG_GRACE);
            return;
        }
        let on_screen = live.window.load().is_some() && live.container.window().is_some();
        let outcome = if on_screen {
            ToastOutcome::TimedOut
        } else {
            ToastOutcome::Dismissed
        };
        live.finish(mtm, Ok(outcome));
    });
}

/// The capsule: an effect view holding the message and the optional action.
fn build_capsule(
    mtm: MainThreadMarker,
    spec: &ToastSpec,
    target: &FrustNativeToastTarget,
) -> Retained<UIView> {
    let capsule = UIVisualEffectView::initWithEffect(
        UIVisualEffectView::alloc(mtm),
        Some(&capsule_effect(mtm, spec.glass)),
    );
    capsule.setTranslatesAutoresizingMaskIntoConstraints(false);
    capsule.setClipsToBounds(true);
    capsule.layer().setCornerRadius(MIN_HEIGHT / 2.0);

    let label = UILabel::new(mtm);
    label.setText(Some(&NSString::from_str(&spec.message)));
    label.setNumberOfLines(3);
    set_label_font(&label, &UIFont::systemFontOfSize(15.0));
    set_label_text_color(&label, &UIColor::labelColor());
    label.setContentCompressionResistancePriority_forAxis(
        UILayoutPriorityDefaultLow,
        UILayoutConstraintAxis::Horizontal,
    );

    let stack = UIStackView::new(mtm);
    stack.setAxis(UILayoutConstraintAxis::Horizontal);
    stack.setAlignment(UIStackViewAlignment::Center);
    stack.setSpacing(SPACING);
    stack.setTranslatesAutoresizingMaskIntoConstraints(false);
    stack.addArrangedSubview(&label);

    if let Some(action) = spec.action.as_deref() {
        let button = UIButton::buttonWithType(UIButtonType::System, mtm);
        button.setTitle_forState(Some(&NSString::from_str(action)), UIControlState::Normal);
        if let Some(title) = button.titleLabel() {
            set_label_font(&title, &UIFont::boldSystemFontOfSize(15.0));
        }
        button.setContentHuggingPriority_forAxis(
            UILayoutPriorityRequired,
            UILayoutConstraintAxis::Horizontal,
        );
        button.setContentCompressionResistancePriority_forAxis(
            UILayoutPriorityRequired,
            UILayoutConstraintAxis::Horizontal,
        );
        let receiver: &AnyObject = target;
        // SAFETY: `receiver` is the toast's target, implementing
        // `frustToastAction:` (defined below, taking the sender). UIKit holds
        // a control's target weakly; the live entry holds the target until
        // the toast resolves, then the exit animation's completion holds it
        // until the view (and so the button) is removed.
        unsafe {
            button.addTarget_action_forControlEvents(
                Some(receiver),
                sel!(frustToastAction:),
                UIControlEvents::TouchUpInside,
            );
        }
        stack.addArrangedSubview(&button);
    }

    let content = capsule.contentView();
    content.addSubview(&stack);
    let constraints = [
        stack
            .leadingAnchor()
            .constraintEqualToAnchor_constant(&content.leadingAnchor(), SIDE_INSET),
        content
            .trailingAnchor()
            .constraintEqualToAnchor_constant(&stack.trailingAnchor(), SIDE_INSET),
        stack
            .topAnchor()
            .constraintGreaterThanOrEqualToAnchor_constant(&content.topAnchor(), VERTICAL_INSET),
        content
            .bottomAnchor()
            .constraintGreaterThanOrEqualToAnchor_constant(&stack.bottomAnchor(), VERTICAL_INSET),
        stack
            .centerYAnchor()
            .constraintEqualToAnchor(&content.centerYAnchor()),
        capsule
            .heightAnchor()
            .constraintGreaterThanOrEqualToConstant(MIN_HEIGHT),
        capsule
            .widthAnchor()
            .constraintLessThanOrEqualToConstant(MAX_WIDTH),
    ];
    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&constraints), mtm);

    let receiver: &AnyObject = target;
    // SAFETY: `receiver` implements `frustToastPan:` (defined below, taking
    // the recognizer). The recognizer does not keep its target alive; the
    // target outlives the capsule the same way as for the button above.
    let pan = unsafe {
        UIPanGestureRecognizer::initWithTarget_action(
            UIPanGestureRecognizer::alloc(mtm),
            Some(receiver),
            Some(sel!(frustToastPan:)),
        )
    };
    capsule.addGestureRecognizer(&pan);

    Retained::into_super(capsule)
}

/// Liquid Glass when asked for and present at run time, else the system
/// material blur.
fn capsule_effect(mtm: MainThreadMarker, glass: bool) -> Retained<UIVisualEffect> {
    if glass && glass_available() {
        let effect = UIGlassEffect::effectWithStyle(UIGlassEffectStyle::Regular, mtm);
        return Retained::into_super(effect);
    }
    Retained::into_super(UIBlurEffect::effectWithStyle(
        UIBlurEffectStyle::SystemMaterial,
        mtm,
    ))
}

/// Whether `UIGlassEffect` exists (iOS 26+). Its constructor is a class
/// method, so the question goes to the metaclass: naming the class through
/// `objc2-ui-kit` before this answers `true` would abort on an older system.
fn glass_available() -> bool {
    AnyClass::get(c"UIGlassEffect")
        .is_some_and(|class| class.metaclass().responds_to(sel!(effectWithStyle:)))
}

/// Insert `container` into `window` directly above the root controller's
/// view — above the bars it holds, below any presented controller — and
/// answer that view; or on top, answering `None`, when that view is not the
/// window's own subview.
fn insert_into(window: &UIWindow, container: &UIView) -> Option<Retained<UIView>> {
    let root_view = window
        .rootViewController()
        .and_then(|root| root.viewIfLoaded())
        .filter(|view| view.superview().is_some_and(|parent| same(&parent, window)));
    match &root_view {
        Some(root_view) => window.insertSubview_aboveSubview(container, root_view),
        None => window.addSubview(container),
    }
    root_view
}

/// The module doc's *Placement*, as constraints between `container` (already
/// in `window`) and anchors in the same window.
fn activate_placement(
    mtm: MainThreadMarker,
    window: &UIWindow,
    root_view: Option<&UIView>,
    container: &UIView,
) {
    let safe = window.safeAreaLayoutGuide();
    let mut constraints = vec![
        container
            .centerXAnchor()
            .constraintEqualToAnchor(&window.centerXAnchor()),
        container
            .leadingAnchor()
            .constraintGreaterThanOrEqualToAnchor_constant(&safe.leadingAnchor(), EDGE_MARGIN),
        safe.trailingAnchor()
            .constraintGreaterThanOrEqualToAnchor_constant(
                &container.trailingAnchor(),
                EDGE_MARGIN,
            ),
        safe.bottomAnchor()
            .constraintGreaterThanOrEqualToAnchor_constant(
                &container.bottomAnchor(),
                BOTTOM_MARGIN,
            ),
    ];
    // Pulled down onto the safe area as far as the required ones allow.
    let resting = safe
        .bottomAnchor()
        .constraintEqualToAnchor_constant(&container.bottomAnchor(), BOTTOM_MARGIN);
    resting.setPriority(UILayoutPriorityDefaultLow);
    constraints.push(resting);

    // The root controller's view owns the keyboard guide whenever it is in
    // this window, direct subview or not (UIKit may wrap it in a transition
    // view): the window's own guide does not follow the keyboard there. Only
    // a window with no such view falls back to its own guide.
    let root_controller_view = window
        .rootViewController()
        .and_then(|root| root.viewIfLoaded())
        .filter(|view| view.window().is_some_and(|w| same(&w, window)));
    let keyboard_owner: &UIView = root_view
        .or(root_controller_view.as_deref())
        .unwrap_or(window);
    if keyboard_owner.respondsToSelector(sel!(keyboardLayoutGuide)) {
        constraints.push(
            keyboard_owner
                .keyboardLayoutGuide()
                .topAnchor()
                .constraintGreaterThanOrEqualToAnchor_constant(
                    &container.bottomAnchor(),
                    BOTTOM_MARGIN,
                ),
        );
    }
    if let Some(page) = selected_page(window)
        .and_then(|page| page.viewIfLoaded())
        .filter(|view| view.window().is_some_and(|w| same(&w, window)))
    {
        constraints.push(
            page.safeAreaLayoutGuide()
                .bottomAnchor()
                .constraintGreaterThanOrEqualToAnchor_constant(
                    &container.bottomAnchor(),
                    BOTTOM_MARGIN,
                ),
        );
    }
    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&constraints), mtm);
}

/// The page the user sees: the window's root controller walked through a
/// tab bar controller's selected page and a navigation controller's top one
/// (bounded, in case of an unexpected cycle).
fn selected_page(window: &UIWindow) -> Option<Retained<UIViewController>> {
    let mut controller = window.rootViewController()?;
    for _ in 0..8 {
        let next = if let Some(tabs) = controller.downcast_ref::<UITabBarController>() {
            // SAFETY: the selected controller is one of `tabs`' own children,
            // retained by it; the `Retained` answered here keeps it alive for
            // as long as this turn holds it.
            unsafe { tabs.selectedViewController() }
        } else if let Some(navigation) = controller.downcast_ref::<UINavigationController>() {
            navigation.topViewController()
        } else {
            None
        };
        match next {
            Some(next) => controller = next,
            None => break,
        }
    }
    Some(controller)
}

/// Everything one shown toast keeps alive until its outcome.
struct LiveToast {
    generation: u64,
    container: Retained<UIView>,
    /// Weak: the window owns the capsule, never the other way round.
    window: Weak<UIWindow>,
    target: Retained<FrustNativeToastTarget>,
}

impl LiveToast {
    /// The terminal step: resolve `outcome` (a no-op for a toast no longer
    /// current), then take the view away.
    fn finish(self, mtm: MainThreadMarker, outcome: Result<ToastOutcome, PresentError>) {
        resolve(self.generation, outcome);
        self.leave(mtm);
    }

    /// Animate the capsule out from wherever it is and remove it from the
    /// window in the animation's completion, exactly once. The completion
    /// owns the view and the target until then.
    fn leave(self, mtm: MainThreadMarker) {
        let Self {
            container, target, ..
        } = self;
        container.setUserInteractionEnabled(false);
        let moving = container.clone();
        let animations = RcBlock::new(move || {
            moving.setAlpha(0.0);
            moving.setTransform(translate(TRAVEL));
        });
        let pending = Cell::new(Some((container, target)));
        let completion = RcBlock::new(move |_finished: Bool| {
            if let Some((container, _target)) = pending.take() {
                container.removeFromSuperview();
            }
        });
        UIView::animateWithDuration_delay_options_animations_completion(
            0.2,
            0.0,
            UIViewAnimationOptions::CurveEaseIn | UIViewAnimationOptions::BeginFromCurrentState,
            &animations,
            Some(&completion),
            mtm,
        );
    }
}

impl LivePresentation for LiveToast {
    /// Displaced by a newer toast, or taken down: leave, resolving
    /// `Dismissed` (already resolved when a newer toast replaced it).
    fn dismiss(self: Box<Self>, mtm: MainThreadMarker, then: Option<AfterDismiss>) {
        self.finish(mtm, Ok(ToastOutcome::Dismissed));
        if let Some(then) = then {
            then(mtm);
        }
    }

    fn is_toast(&self) -> bool {
        true
    }
}

/// The toast target's state: whose toast it answers for, and whether the
/// user is dragging the capsule right now.
struct ToastIvars {
    generation: u64,
    dragging: Cell<bool>,
}

define_class!(
    // SAFETY:
    // - `NSObject` has no subclassing requirements.
    // - The ivars are plain values with no `Drop` impl, so the macro's
    //   generated `dealloc` has nothing extra to uphold.
    #[unsafe(super(NSObject))]
    // Every UIKit callback it receives runs on the main thread, and the arm
    // builds it there (`Self::alloc(mtm)`).
    #[thread_kind = MainThreadOnly]
    #[ivars = ToastIvars]
    struct FrustNativeToastTarget;

    unsafe impl NSObjectProtocol for FrustNativeToastTarget {}

    impl FrustNativeToastTarget {
        /// The action's `TouchUpInside` → `Action`.
        #[unsafe(method(frustToastAction:))]
        fn toast_action(&self, _sender: &UIButton) {
            let generation = self.ivars().generation;
            guarded("action", || {
                let Some(mtm) = MainThreadMarker::new() else {
                    return;
                };
                if let Some(live) = take_live_as::<LiveToast>(mtm, generation) {
                    live.finish(mtm, Ok(ToastOutcome::Action));
                }
            });
        }

        /// The capsule's pan: follow a downward drag, then dismiss or spring back.
        #[unsafe(method(frustToastPan:))]
        fn toast_pan(&self, recognizer: &UIPanGestureRecognizer) {
            guarded("swipe", || self.pan(recognizer));
        }
    }
);

impl FrustNativeToastTarget {
    fn new(mtm: MainThreadMarker, generation: u64) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ToastIvars {
            generation,
            dragging: Cell::new(false),
        });
        // SAFETY: `NSObject`'s designated initializer, called on a freshly
        // allocated instance whose ivars are already set (the alert
        // delegate's idiom).
        unsafe { msg_send![super(this), init] }
    }

    fn pan(&self, recognizer: &UIPanGestureRecognizer) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let Some(view) = recognizer.view() else {
            return;
        };
        // SAFETY: `state` is a documented `UIGestureRecognizer` property
        // returning `UIGestureRecognizerState` (an `NSInteger` enum), which
        // `objc2-ui-kit` declares only as a setter.
        let state: UIGestureRecognizerState = unsafe { msg_send![recognizer, state] };
        let superview = view.superview();
        let dy = recognizer.translationInView(superview.as_deref()).y;
        // Downward follows the finger; upward resists.
        let offset = if dy > 0.0 { dy } else { dy * 0.2 };
        match state {
            UIGestureRecognizerState::Began | UIGestureRecognizerState::Changed => {
                self.ivars().dragging.set(true);
                view.setTransform(translate(offset));
                view.setAlpha((1.0 - offset.max(0.0) / (SWIPE_DISTANCE * 3.0)).max(0.3));
            }
            UIGestureRecognizerState::Ended
            | UIGestureRecognizerState::Cancelled
            | UIGestureRecognizerState::Failed => {
                self.ivars().dragging.set(false);
                let velocity = recognizer.velocityInView(superview.as_deref()).y;
                let swiped = state == UIGestureRecognizerState::Ended
                    && (offset > SWIPE_DISTANCE || velocity > SWIPE_VELOCITY);
                if swiped
                    && let Some(live) = take_live_as::<LiveToast>(mtm, self.ivars().generation)
                {
                    live.finish(mtm, Ok(ToastOutcome::Dismissed));
                    return;
                }
                let back = RcBlock::new(move || {
                    view.setAlpha(1.0);
                    view.setTransform(translate(0.0));
                });
                UIView::animateWithDuration_delay_options_animations_completion(
                    0.2,
                    0.0,
                    UIViewAnimationOptions::CurveEaseOut
                        | UIViewAnimationOptions::BeginFromCurrentState
                        | UIViewAnimationOptions::AllowUserInteraction,
                    &back,
                    None,
                    mtm,
                );
            }
            _ => {}
        }
    }
}
