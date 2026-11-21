//! The Android toast arm: a Material `com.google.android.material.snackbar.Snackbar`
//! built, shown and resolved by `FrustNativePresenter.kt`, over
//! `super::android_host`'s shared JNI plumbing (`with_presenter`, the
//! generation-keyed live entry, the one `nativeOnOutcome` callback).
//!
//! # The contract
//!
//! Two Rust → Kotlin members beside `android_host`'s table — plain
//! `@JvmStatic fun`s, so the frozen export set is unchanged; each hops to the
//! main thread itself (inline when already there):
//!
//! | Kotlin | Contract |
//! |---|---|
//! | `showToast(generation, message, action: String?, duration: Int)` | Shows the Snackbar; `action` is the one action's label (`null` for none), `duration` the length in milliseconds ([`wire::duration_code`]) |
//! | `dismissToast(generation)` | Takes the toast down, if it is still `generation`'s |
//!
//! # Placement
//!
//! The Snackbar lives in its own `CoordinatorLayout` layer added last to the
//! Activity's content view, so it draws above the frust surface and the
//! plugin's chrome and can be swiped away; the layer passes on every touch
//! that does not start on the Snackbar. While the chrome shows its toolbar
//! or bottom navigation the Snackbar is anchored above it
//! (`FrustNativeChrome.toastAnchor`); otherwise — no chrome, or the keyboard
//! hid the bars — it sits above the larger of the keyboard and the
//! navigation bar. Dialogs are windows of their own: a toast shown while an
//! alert or a sheet is up draws behind that dialog.
//!
//! # Resolution
//!
//! Every report goes through `nativeOnOutcome`, mapped by
//! `super::toast::wire::toast_outcome`: the timeout → `OUTCOME_TIMED_OUT` →
//! [`ToastOutcome::TimedOut`](super::ToastOutcome::TimedOut); the action →
//! `OUTCOME_ACTION` → `Action`; a swipe → `OUTCOME_CANCELLED`, a
//! [`dismissToast`](ToastHost::dismiss) → `OUTCOME_DISMISSED` and the
//! Activity destroyed → `OUTCOME_HOST_LOST`, each `Dismissed`. A toast
//! replaced by a newer one was resolved `Dismissed` on the Rust side when the
//! newer one was accepted (`super::toast`); its own late report finds no live
//! entry here — the newer toast displaced it — and is dropped.
//!
//! # `unsafe`
//!
//! None — the raw FFI boundary is `android_host`'s exported
//! `nativeOnOutcome`.

#[cfg(not(target_os = "android"))]
compile_error!("the Android toast arm targets Android only");

use jni::objects::{JClass, JObject, JValue};
use jni::refs::Global;
use jni::sys::jlong;
use jni::{Env, jni_sig, jni_str};

use super::android_host::{
    LiveKind, Resolve, has_resumed_activity, install_live, jni_call, take_live, with_presenter,
};
use super::toast::{ToastHost, resolve, wire};
use super::{PresentError, ToastSpec};

/// The Android host: `super::toast::ToastHost` over a Material Snackbar.
pub(crate) struct Host;

impl ToastHost for Host {
    fn show_toast(spec: ToastSpec, generation: u64) -> Result<(), PresentError> {
        if !with_presenter(has_resumed_activity)? {
            return Err(PresentError::NoHost);
        }
        let report: Resolve = Box::new(move |code, _action_index| {
            resolve(generation, wire::toast_outcome(code));
        });
        // The toast this one replaces was resolved by `super::toast` (or will
        // be, once this show is accepted): its parked entry is only dropped.
        drop(install_live(LiveKind::Toast, generation, report, None));
        let duration = wire::duration_code(spec.duration);
        let result = with_presenter(|env, class| {
            call_show_toast(
                env,
                class,
                generation,
                &spec.message,
                spec.action.as_deref(),
                duration,
            )
        });
        if result.is_err() {
            // `showToast` never ran: nothing will report this toast.
            drop(take_live(generation));
        }
        result
    }

    fn dismiss(generation: u64) {
        let result = with_presenter(|env, class| {
            jni_call(env, "FrustNativePresenter.dismissToast", |env| {
                env.call_static_method(
                    class,
                    jni_str!("dismissToast"),
                    jni_sig!("(J)V"),
                    &[JValue::Long(generation as jlong)],
                )?
                .v()
            })
        });
        if let Err(err) = result {
            log::warn!(
                "frust-native-widgets: FrustNativePresenter.dismissToast({generation}) failed: {err}"
            );
        }
    }
}

/// `FrustNativePresenter.showToast(generation, message, action, duration)` —
/// see the module doc's contract table. `generation` crosses as the same 64
/// bits (`android_alert`'s `showAlert` call explains the cast).
fn call_show_toast(
    env: &mut Env<'_>,
    class: &Global<JClass<'static>>,
    generation: u64,
    message: &str,
    action: Option<&str>,
    duration: i32,
) -> Result<(), PresentError> {
    jni_call(env, "FrustNativePresenter.showToast", |env| {
        let message = env.new_string(message)?;
        let action: JObject = match action {
            Some(label) => env.new_string(label)?.into(),
            None => JObject::null(),
        };
        env.call_static_method(
            class,
            jni_str!("showToast"),
            jni_sig!("(JLjava/lang/String;Ljava/lang/String;I)V"),
            &[
                JValue::Long(generation as jlong),
                JValue::Object(&message),
                JValue::Object(&action),
                JValue::Int(duration),
            ],
        )?
        .v()
    })
}
