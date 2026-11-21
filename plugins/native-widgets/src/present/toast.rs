//! Native toasts: a short, non-modal message — optionally with one action —
//! shown above the app's own chrome and resolved to exactly one
//! [`ToastOutcome`] — see [`show_toast`].
//!
//! # Why a native one
//!
//! A frust-drawn toast cannot reliably sit above native chrome: native views
//! (bottom navigation, a toolbar, a tab bar) are layered above the frust
//! surface, and the frust overlay surfaces that could go above them are
//! opt-in. A toast that must be readable over the plugin's own bars is
//! therefore a platform view the host owns, requested like an alert.
//!
//! # The contract
//!
//! - **No slot.** A toast is neither the sheet nor the alert: it is never
//!   refused [`PresentError::Busy`], never waits for either, and never takes
//!   one down. Each side resolves only its own outcome.
//! - **The newest toast wins.** At most one toast is current, process-wide.
//!   A new toast **replaces** the current one, which resolves
//!   [`ToastOutcome::Dismissed`] as soon as the platform accepted the new
//!   request (a request refused on the spot — an invalid spec, no host —
//!   leaves the current one untouched).
//! - **Exactly one terminal outcome.** [`ToastOutcome::TimedOut`] when its
//!   duration runs out, [`ToastOutcome::Action`] when the user taps its
//!   action, [`ToastOutcome::Dismissed`] when the user swipes it away, it is
//!   replaced, [`super::dismiss`] takes it down, or its host goes away. A
//!   platform report for a toast that is no longer current finds nothing and
//!   is dropped.
//! - **Dropping the future** abandons only the outcome: the toast stays up
//!   until it times out, and its report is discarded.
//!
//! # Platform arms
//!
//! `ToastHost` is the seam, selected by `cfg`: **Android** —
//! `android_toast`, a Material `Snackbar` built by `FrustNativePresenter.kt`
//! over the activity's content view, anchored above the plugin's bottom
//! navigation or toolbar when one shows; **iOS/iPadOS** — `apple_toast`, a
//! plugin-owned capsule view in the key window, above the tab and tool bars
//! and the keyboard; **every other target** (macOS included) —
//! `unsupported`'s refusal, [`PresentError::Unsupported`].

use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use super::{PresentError, Presentation, Sender, next_generation, oneshot};

/// How long a toast stays up before it resolves [`ToastOutcome::TimedOut`].
///
/// The platform may lengthen it for accessibility (Android applies the
/// system's recommended timeout for the content, which can be longer).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ToastDuration {
    /// A short message: [`Self::SHORT`] (the length of Material's `LENGTH_SHORT`).
    #[default]
    Short,
    /// A longer message: [`Self::LONG`] (the length of Material's `LENGTH_LONG`).
    Long,
    /// An explicit duration, clamped into [`Self::MIN`]`..=`[`Self::MAX`].
    Custom(Duration),
}

impl ToastDuration {
    /// [`Self::Short`]'s length.
    pub const SHORT: Duration = Duration::from_millis(1500);
    /// [`Self::Long`]'s length.
    pub const LONG: Duration = Duration::from_millis(2750);
    /// The shortest [`Self::Custom`] duration: anything shorter cannot be read.
    pub const MIN: Duration = Duration::from_secs(1);
    /// The longest [`Self::Custom`] duration: a toast is transient, never a
    /// banner that stays.
    pub const MAX: Duration = Duration::from_secs(10);

    /// The duration the toast is shown for, [`Self::Custom`] clamped.
    pub fn resolved(self) -> Duration {
        match self {
            Self::Short => Self::SHORT,
            Self::Long => Self::LONG,
            Self::Custom(duration) => duration.clamp(Self::MIN, Self::MAX),
        }
    }
}

/// What a toast shows: a message, an optional action, how long it stays and
/// whether it asks for the glass material where the platform has one.
///
/// Validated when submitted ([`show_toast`]): the message is not empty
/// (after trimming whitespace), and an action label, when given, is not
/// empty either.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToastSpec {
    /// The text shown (up to two lines on Android; wrapped on iOS).
    pub message: String,
    /// The one action's label; tapping it resolves [`ToastOutcome::Action`].
    pub action: Option<String>,
    /// How long it stays up.
    pub duration: ToastDuration,
    /// Ask for Liquid Glass (`UIGlassEffect`) on iOS 26 and later; a standard
    /// blur stands in where the class does not exist. Ignored on Android,
    /// whose Snackbar follows the Material theme.
    pub glass: bool,
}

impl ToastSpec {
    /// A [`ToastDuration::Short`] toast with no action showing `message`.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            ..Self::default()
        }
    }

    /// Add the one action, labelled `label`.
    #[must_use]
    pub fn with_action(mut self, label: impl Into<String>) -> Self {
        self.action = Some(label.into());
        self
    }

    /// Set [`Self::duration`].
    #[must_use]
    pub fn with_duration(mut self, duration: ToastDuration) -> Self {
        self.duration = duration;
        self
    }

    /// Set [`Self::glass`].
    #[must_use]
    pub fn with_glass(mut self, glass: bool) -> Self {
        self.glass = glass;
        self
    }

    /// The submit-time validation described on [`ToastSpec`].
    ///
    /// # Errors
    /// [`PresentError::InvalidSpec`] naming the rule broken.
    pub fn validate(&self) -> Result<(), PresentError> {
        if self.message.trim().is_empty() {
            return Err(PresentError::InvalidSpec(
                "a toast needs a non-empty message".to_string(),
            ));
        }
        if self
            .action
            .as_deref()
            .is_some_and(|label| label.trim().is_empty())
        {
            return Err(PresentError::InvalidSpec(
                "a toast's action label must not be empty".to_string(),
            ));
        }
        Ok(())
    }
}

/// How a toast ended — exactly one per accepted [`show_toast`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ToastOutcome {
    /// Its duration ran out.
    TimedOut,
    /// It went away without its action: swiped away by the user, replaced by
    /// a newer toast, taken down by [`super::dismiss`], or its host went away.
    Dismissed,
    /// The user tapped its action.
    Action,
}

/// The platform seam a toast arm implements (module doc's *Platform arms*).
pub(crate) trait ToastHost {
    /// Show `spec` as toast `generation`, replacing whatever toast the
    /// platform still shows. Its outcome arrives later through
    /// [`resolve`]; an error returned here is a refusal on the spot, with
    /// nothing shown. `spec` has already passed [`ToastSpec::validate`].
    fn show_toast(spec: ToastSpec, generation: u64) -> Result<(), PresentError>;

    /// Take toast `generation` down; it resolves [`ToastOutcome::Dismissed`]
    /// through [`resolve`]. Must ignore a generation it is not showing.
    fn dismiss(generation: u64);
}

/// The current toast: its generation and the sender it resolves through.
struct CurrentToast {
    generation: u64,
    tx: Sender<ToastOutcome>,
}

/// The current toast, if any (module doc's *The newest toast wins*). A plain
/// `Mutex`: requests come from the UI thread, reports from the platform's
/// main thread, which is the same thread on every arm today.
static CURRENT: Mutex<Option<CurrentToast>> = Mutex::new(None);

/// Lock [`CURRENT`], recovering from poisoning — the guarded data is one
/// plain `Option`, coherent after any panic.
fn lock_current() -> MutexGuard<'static, Option<CurrentToast>> {
    CURRENT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Resolve toast `generation` with `outcome` if it is still current — the
/// one path every arm reports through. Answers whether it was; a stale or
/// duplicate report answers `false` and does nothing. The sender runs with
/// the lock released, so a woken caller may show the next toast at once.
#[cfg_attr(
    not(any(test, target_os = "android", target_os = "ios")),
    allow(dead_code)
)]
pub(crate) fn resolve(generation: u64, outcome: Result<ToastOutcome, PresentError>) -> bool {
    let current = {
        let mut current = lock_current();
        if current.as_ref().is_some_and(|c| c.generation == generation) {
            current.take()
        } else {
            None
        }
    };
    match current {
        Some(current) => {
            current.tx.send(outcome);
            true
        }
        None => false,
    }
}

/// Whether `generation` is the current toast — [`super::dismiss`]'s filter.
pub(crate) fn is_current(generation: u64) -> bool {
    lock_current()
        .as_ref()
        .is_some_and(|c| c.generation == generation)
}

/// [`show_toast`] over host `H`: validate, make the new toast current, ask
/// `H` to show it, then resolve the toast it replaced `Dismissed` — or, when
/// `H` refuses on the spot, put the replaced toast back.
pub(crate) fn show_toast_on<H: ToastHost>(spec: ToastSpec) -> Presentation<ToastOutcome> {
    if let Err(err) = spec.validate() {
        return Presentation::ready(Err(err));
    }
    let generation = next_generation();
    let (tx, rx) = oneshot::channel(generation);
    // Current before the host runs: an arm may report this toast from
    // inside `show_toast` (Android's presenter does for "no host").
    let replaced = lock_current().replace(CurrentToast { generation, tx });
    match H::show_toast(spec, generation) {
        Ok(()) => {
            if let Some(replaced) = replaced {
                replaced.tx.send(Ok(ToastOutcome::Dismissed));
            }
            Presentation::pending(rx, generation)
        }
        Err(err) => {
            let (ours, superseded) = {
                let mut current = lock_current();
                if current.as_ref().is_some_and(|c| c.generation == generation) {
                    (std::mem::replace(&mut *current, replaced), None)
                } else {
                    // Already resolved from inside `show_toast`: what it
                    // replaced has been superseded all the same.
                    (None, replaced)
                }
            };
            // The refusal is the answer; dropping `rx` first discards
            // whatever the dropped sender would report.
            drop(rx);
            drop(ours);
            if let Some(superseded) = superseded {
                superseded.tx.send(Ok(ToastOutcome::Dismissed));
            }
            Presentation::ready(Err(err))
        }
    }
}

/// [`super::dismiss`] for a toast: reaches `H` only while `generation` is
/// the current toast.
pub(crate) fn dismiss_on<H: ToastHost>(generation: u64) {
    if is_current(generation) {
        H::dismiss(generation);
    }
}

/// Show a native toast — see the module doc's *The contract*.
///
/// The returned [`Presentation`] resolves one [`ToastOutcome`], or
/// [`PresentError`]: `InvalidSpec` or a refusal on the spot (`NoHost`,
/// `Unsupported` on every platform but Android and iOS/iPadOS, `Platform`)
/// on its first poll, or a later `NoHost`/`Platform` from an arm that found
/// no window only on the platform's main thread. Never `Busy`.
/// [`Presentation::handle`] names it for [`super::dismiss`].
pub fn show_toast(spec: ToastSpec) -> Presentation<ToastOutcome> {
    show_toast_on::<super::ToastPlatformHost>(spec)
}

/// The Android presenter's toast wire: the `duration` argument of
/// `FrustNativePresenter.showToast` and the decoding of its reports.
#[cfg_attr(not(any(test, target_os = "android")), allow(dead_code))]
pub(crate) mod wire {
    use super::super::wire::{
        OUTCOME_ACTION, OUTCOME_CANCELLED, OUTCOME_DISMISSED, OUTCOME_FAILED, OUTCOME_HOST_LOST,
        OUTCOME_NO_HOST, OUTCOME_TIMED_OUT,
    };
    use super::{PresentError, ToastDuration, ToastOutcome};

    /// `showToast`'s `duration`: always the resolved length in milliseconds,
    /// never Material's `LENGTH_SHORT`/`LENGTH_LONG` (`-1`/`0`). From
    /// Android 10 Material passes the duration through
    /// `AccessibilityManager.getRecommendedTimeoutMillis`, which answers the
    /// larger of the two, so `LENGTH_SHORT` comes back as `0` — `LENGTH_LONG`
    /// — and the two would be indistinguishable. A positive length keeps
    /// them apart and still takes a longer accessibility timeout.
    pub(crate) fn duration_code(duration: ToastDuration) -> i32 {
        i32::try_from(duration.resolved().as_millis()).unwrap_or(i32::MAX)
    }

    /// Map one `nativeOnOutcome` report onto a toast's result: the swipe
    /// (`OUTCOME_CANCELLED`), a replacement or dismissal (`OUTCOME_DISMISSED`)
    /// and a lost host all end it `Dismissed`.
    pub(crate) fn toast_outcome(code: i32) -> Result<ToastOutcome, PresentError> {
        match code {
            OUTCOME_ACTION => Ok(ToastOutcome::Action),
            OUTCOME_TIMED_OUT => Ok(ToastOutcome::TimedOut),
            OUTCOME_CANCELLED | OUTCOME_DISMISSED | OUTCOME_HOST_LOST => {
                Ok(ToastOutcome::Dismissed)
            }
            OUTCOME_NO_HOST => Err(PresentError::NoHost),
            OUTCOME_FAILED => Err(PresentError::Platform(
                "the presenter failed to show the toast".to_string(),
            )),
            other => Err(PresentError::Platform(format!(
                "unknown presenter outcome code {other} for a toast"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::task::{Context, Poll, Waker};

    use super::super::test_support::serialize;
    use super::super::wire as presenter_wire;
    use super::*;

    /// What the fake host was asked, and how it answers the next show.
    #[derive(Default)]
    struct FakeToasts {
        refuse: Option<PresentError>,
        /// Report this outcome for the new toast from inside `show_toast`.
        report_inside: Option<Result<ToastOutcome, PresentError>>,
        shown: Vec<(u64, ToastSpec)>,
        dismissed: Vec<u64>,
    }

    static FAKE: Mutex<Option<FakeToasts>> = Mutex::new(None);

    fn with_fake<R>(f: impl FnOnce(&mut FakeToasts) -> R) -> R {
        let mut fake = FAKE.lock().unwrap_or_else(|p| p.into_inner());
        f(fake.get_or_insert_with(FakeToasts::default))
    }

    struct FakeHost;

    impl ToastHost for FakeHost {
        fn show_toast(spec: ToastSpec, generation: u64) -> Result<(), PresentError> {
            let (refuse, inside) = with_fake(|fake| {
                fake.shown.push((generation, spec));
                (fake.refuse.clone(), fake.report_inside.take())
            });
            if let Some(outcome) = inside {
                resolve(generation, outcome);
            }
            refuse.map_or(Ok(()), Err)
        }

        fn dismiss(generation: u64) {
            with_fake(|fake| fake.dismissed.push(generation));
            resolve(generation, Ok(ToastOutcome::Dismissed));
        }
    }

    /// Run `f` serialized with every other test touching the presentation
    /// statics, with the fake and [`CURRENT`] reset around it.
    fn isolated<R>(f: impl FnOnce() -> R) -> R {
        let _guard = serialize();
        let reset = || {
            drop(lock_current().take());
            *FAKE.lock().unwrap_or_else(|p| p.into_inner()) = None;
        };
        reset();
        let result = f();
        reset();
        result
    }

    fn poll<T>(presentation: &mut Presentation<T>) -> Poll<Result<T, PresentError>> {
        Pin::new(presentation).poll(&mut Context::from_waker(Waker::noop()))
    }

    fn generation_of<T>(presentation: &Presentation<T>) -> u64 {
        presentation.handle().expect("accepted").generation
    }

    #[test]
    fn a_platform_report_resolves_the_toast_once() {
        isolated(|| {
            let mut toast = show_toast_on::<FakeHost>(ToastSpec::new("Saved"));
            let generation = generation_of(&toast);
            assert_eq!(poll(&mut toast), Poll::Pending);
            assert!(resolve(generation, Ok(ToastOutcome::TimedOut)));
            assert!(
                !resolve(generation, Ok(ToastOutcome::Action)),
                "a duplicate report"
            );
            assert_eq!(poll(&mut toast), Poll::Ready(Ok(ToastOutcome::TimedOut)));
            assert_eq!(poll(&mut toast), Poll::Pending);
            assert!(!is_current(generation));
        });
    }

    #[test]
    fn a_new_toast_replaces_the_current_one_which_resolves_dismissed() {
        isolated(|| {
            let mut first = show_toast_on::<FakeHost>(ToastSpec::new("First"));
            let first_generation = generation_of(&first);
            let mut second =
                show_toast_on::<FakeHost>(ToastSpec::new("Second").with_action("Undo"));
            let second_generation = generation_of(&second);

            assert_eq!(poll(&mut first), Poll::Ready(Ok(ToastOutcome::Dismissed)));
            assert_eq!(poll(&mut second), Poll::Pending);
            assert!(is_current(second_generation));
            // The platform's own late report for the replaced toast is stale.
            assert!(!resolve(first_generation, Ok(ToastOutcome::Dismissed)));
            assert!(resolve(second_generation, Ok(ToastOutcome::Action)));
            assert_eq!(poll(&mut second), Poll::Ready(Ok(ToastOutcome::Action)));
            assert_eq!(with_fake(|fake| fake.shown.len()), 2);
        });
    }

    #[test]
    fn a_refused_toast_leaves_the_current_one_alone() {
        isolated(|| {
            let mut first = show_toast_on::<FakeHost>(ToastSpec::new("First"));
            let first_generation = generation_of(&first);
            with_fake(|fake| fake.refuse = Some(PresentError::NoHost));
            let mut refused = show_toast_on::<FakeHost>(ToastSpec::new("Second"));
            assert_eq!(refused.handle(), None);
            assert_eq!(poll(&mut refused), Poll::Ready(Err(PresentError::NoHost)));

            assert_eq!(poll(&mut first), Poll::Pending, "not replaced");
            assert!(is_current(first_generation));
            assert!(resolve(first_generation, Ok(ToastOutcome::TimedOut)));
            assert_eq!(poll(&mut first), Poll::Ready(Ok(ToastOutcome::TimedOut)));
        });
    }

    #[test]
    fn a_toast_reported_from_inside_the_show_still_supersedes_the_current_one() {
        isolated(|| {
            let mut first = show_toast_on::<FakeHost>(ToastSpec::new("First"));
            with_fake(|fake| {
                fake.report_inside = Some(Err(PresentError::NoHost));
            });
            let mut second = show_toast_on::<FakeHost>(ToastSpec::new("Second"));
            assert_eq!(poll(&mut second), Poll::Ready(Err(PresentError::NoHost)));
            assert_eq!(poll(&mut first), Poll::Ready(Ok(ToastOutcome::Dismissed)));
        });
    }

    #[test]
    fn dismiss_reaches_the_host_only_for_the_current_toast() {
        isolated(|| {
            let mut toast = show_toast_on::<FakeHost>(ToastSpec::new("Saved"));
            let generation = generation_of(&toast);
            dismiss_on::<FakeHost>(generation);
            assert_eq!(poll(&mut toast), Poll::Ready(Ok(ToastOutcome::Dismissed)));
            dismiss_on::<FakeHost>(generation);
            assert_eq!(with_fake(|fake| fake.dismissed.clone()), vec![generation]);
        });
    }

    #[test]
    fn a_toast_never_touches_the_alert_and_sheet_slots() {
        isolated(|| {
            let toast = show_toast_on::<FakeHost>(ToastSpec::new("Saved"));
            assert!(super::super::live_since().is_none());
            drop(toast);
            assert!(super::super::live_since().is_none());
        });
    }

    #[test]
    fn an_invalid_spec_is_refused_before_the_host() {
        isolated(|| {
            for spec in [
                ToastSpec::new(""),
                ToastSpec::new("   "),
                ToastSpec::new("Saved").with_action(""),
                ToastSpec::new("Saved").with_action(" "),
            ] {
                let mut toast = show_toast_on::<FakeHost>(spec.clone());
                assert!(
                    matches!(
                        poll(&mut toast),
                        Poll::Ready(Err(PresentError::InvalidSpec(_)))
                    ),
                    "{spec:?} should be refused"
                );
            }
            assert!(with_fake(|fake| fake.shown.is_empty()));
            assert!(lock_current().is_none());
        });
    }

    #[test]
    fn the_spec_reaches_the_host_unchanged() {
        isolated(|| {
            let spec = ToastSpec::new("Archived")
                .with_action("Undo")
                .with_duration(ToastDuration::Long)
                .with_glass(true);
            let toast = show_toast_on::<FakeHost>(spec.clone());
            let generation = generation_of(&toast);
            assert_eq!(
                with_fake(|fake| fake.shown.clone()),
                vec![(generation, spec)]
            );
        });
    }

    #[test]
    fn durations_resolve_and_custom_ones_are_clamped() {
        assert_eq!(ToastDuration::Short.resolved(), ToastDuration::SHORT);
        assert_eq!(ToastDuration::Long.resolved(), ToastDuration::LONG);
        assert!(ToastDuration::SHORT < ToastDuration::LONG);
        assert_eq!(
            ToastDuration::Custom(Duration::ZERO).resolved(),
            ToastDuration::MIN
        );
        assert_eq!(
            ToastDuration::Custom(Duration::from_secs(3600)).resolved(),
            ToastDuration::MAX
        );
        let inside = Duration::from_millis(4200);
        assert_eq!(ToastDuration::Custom(inside).resolved(), inside);
        assert_eq!(ToastDuration::default(), ToastDuration::Short);
    }

    #[test]
    fn the_duration_wire_is_always_positive_clamped_millis() {
        assert_eq!(wire::duration_code(ToastDuration::Short), 1500);
        assert_eq!(wire::duration_code(ToastDuration::Long), 2750);
        assert_eq!(
            wire::duration_code(ToastDuration::Custom(Duration::from_millis(4200))),
            4200
        );
        assert_eq!(
            wire::duration_code(ToastDuration::Custom(Duration::from_millis(10))),
            1000
        );
        assert_eq!(
            wire::duration_code(ToastDuration::Custom(Duration::from_secs(99))),
            10_000
        );
    }

    #[test]
    fn the_toast_wire_maps_every_code() {
        use presenter_wire::*;
        assert_eq!(
            wire::toast_outcome(OUTCOME_ACTION),
            Ok(ToastOutcome::Action)
        );
        assert_eq!(
            wire::toast_outcome(OUTCOME_TIMED_OUT),
            Ok(ToastOutcome::TimedOut)
        );
        for code in [OUTCOME_CANCELLED, OUTCOME_DISMISSED, OUTCOME_HOST_LOST] {
            assert_eq!(wire::toast_outcome(code), Ok(ToastOutcome::Dismissed));
        }
        assert_eq!(
            wire::toast_outcome(OUTCOME_NO_HOST),
            Err(PresentError::NoHost)
        );
        assert!(matches!(
            wire::toast_outcome(OUTCOME_FAILED),
            Err(PresentError::Platform(_))
        ));
        for code in [OUTCOME_DETENT, OUTCOME_ROW, 99, -1] {
            assert!(matches!(
                wire::toast_outcome(code),
                Err(PresentError::Platform(_))
            ));
        }
    }
}
