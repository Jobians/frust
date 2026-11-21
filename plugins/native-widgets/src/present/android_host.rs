//! The Android presentation host: the `dev.frust.nativewidgets.FrustNativePresenter`
//! Kotlin object, reached over JNI, plus the live-presentation guard its one
//! callback resolves through.
//!
//! # The contract
//!
//! **`FrustNativePresenter.kt` and this module build to this table —
//! changing it means updating both files together** (pinned by
//! `super`'s drift tests). The package and object name are baked into the
//! export's mangled symbol and into [`PRESENTER_CLASS_BINARY`], so they may
//! never move once shipped.
//!
//! | Direction | Member | Notes |
//! |---|---|---|
//! | Rust → Kotlin | `@JvmStatic fun hasResumedActivity(): Boolean` — `()Z` | The host-discovery probe: `false` → [`PresentError::NoHost`] |
//! | Kotlin → Rust | [`Java_dev_frust_nativewidgets_FrustNativePresenter_nativeOnOutcome`]`(env, class, generation: jlong, code: jint, actionIndex: jint)` | The presenter's ONE callback, for every presentation kind; `code` is `super::wire`'s `OUTCOME_*` table, where `OUTCOME_DETENT` and `OUTCOME_ROW` alone are not terminal |
//!
//! # Why an init provider
//!
//! A dialog needs an `Activity` window to attach to, and this plugin's
//! factory only ever sees one inside a `createView` — an app may show an
//! alert with no native control on screen at all. The presenter therefore
//! tracks the resumed `Activity` itself, through
//! `Application.registerActivityLifecycleCallbacks` registered from
//! `FrustNativePresenterInitProvider` (a manifest-declared `ContentProvider`,
//! created before `Application.onCreate` — `frust-auth-session`'s
//! `FrustAuthSessionInitProvider` shape). It holds the `Activity` weakly and
//! clears it on that `Activity`'s `onActivityDestroyed`, never a
//! process-lifetime strong reference.
//!
//! # The live-presentation guard
//!
//! An arm parks a resolver for its presentation in [`LIVE`], tagged with the
//! generation it also hands the Kotlin side and its [`LiveKind`] — at most
//! one entry per kind, so an alert's entry sits beside the sheet it is shown
//! over, and a toast's beside both; [`nativeOnOutcome`] takes the entry
//! back **by generation** and runs it once. A callback for any other
//! generation — a duplicate, or one from a presentation the caller already
//! abandoned and a newer one replaced — finds nothing and is dropped. The resolver maps `(code, actionIndex)` into
//! its own outcome type, so the one callback serves every presentation kind.
//!
//! A sheet parks a [`Stream`] beside its resolver: a
//! `super::wire::OUTCOME_DETENT` or `OUTCOME_ROW` report is handed to it with
//! the entry left in place — a detent change or an event row's tap is an
//! event, not an outcome — so a later terminal report still finds the
//! resolver.
//!
//! # Where the export lives
//!
//! Beside the host it belongs to, not in `crate::android`: that module's
//! exports are pinned to the factory and listener classes
//! (`tests/kotlin_conformance.rs`), and this one is pinned to the presenter
//! by `super`'s drift tests instead. The symbol name is what the JVM binds;
//! the Rust module it is defined in is free to move.
//!
//! # No unwind across FFI, no `unsafe`
//!
//! The export's body runs inside [`jni::EnvUnowned::with_env`], which wraps
//! it in `catch_unwind`; the only `unsafe` token here is its
//! `#[unsafe(no_mangle)]` attribute.
//!
//! [`nativeOnOutcome`]: Java_dev_frust_nativewidgets_FrustNativePresenter_nativeOnOutcome

use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use jni::errors::LogErrorAndDefault;
use jni::objects::{JClass, JObject, JValue};
use jni::refs::Global;
use jni::sys::{jint, jlong};
use jni::{Env, EnvUnowned, jni_sig, jni_str};

use super::{AlertOutcome, PresentError, Sender, SheetSender, wire};

/// The presenter's fully-qualified name in the **binary/dotted** form
/// `ClassLoader.loadClass` expects. Resolved through the application
/// classloader: a bare `FindClass` on a JNI-attached worker thread sees only
/// the bootstrap loader, never app classes.
pub(crate) const PRESENTER_CLASS_BINARY: &str = "dev.frust.nativewidgets.FrustNativePresenter";

/// The cached presenter class, loaded once. A racing loser's reference is
/// dropped immediately, so at most one global reference survives.
static PRESENTER_CLASS: OnceLock<Global<JClass<'static>>> = OnceLock::new();

/// Run `f` with a live [`Env`] and the presenter class inside a scoped JNI
/// attachment (`frust_plugin::android::with_jni_env` — never a permanent
/// attach).
///
/// # Errors
/// [`PresentError::Platform`] when the platform handles are not installed
/// yet, the attach fails, the class cannot be loaded, or `f` fails.
pub(crate) fn with_presenter<T>(
    f: impl FnOnce(&mut Env<'_>, &Global<JClass<'static>>) -> Result<T, PresentError>,
) -> Result<T, PresentError> {
    let attached = frust_plugin::android::with_jni_env(|env, context| {
        let class = presenter_class(env, context)?;
        f(env, class)
    });
    match attached {
        Ok(inner) => inner,
        Err(frust_plugin::PlatformHandleError::NotInitialized) => Err(PresentError::Platform(
            "the Android platform handles are not installed (a scaffold predating \
             nativeInitPlatform)"
                .to_string(),
        )),
        Err(other) => Err(PresentError::Platform(format!(
            "android presenter: platform handle error: {other}"
        ))),
    }
}

/// The cached [`PRESENTER_CLASS`], loading it on first use.
fn presenter_class(
    env: &mut Env<'_>,
    context: &JObject,
) -> Result<&'static Global<JClass<'static>>, PresentError> {
    if let Some(class) = PRESENTER_CLASS.get() {
        return Ok(class);
    }
    let class = load_presenter_class(env, context)?;
    Ok(PRESENTER_CLASS.get_or_init(|| class))
}

/// `context.getClassLoader().loadClass(PRESENTER_CLASS_BINARY)`, promoted to
/// a process-lifetime global reference.
fn load_presenter_class(
    env: &mut Env<'_>,
    context: &JObject,
) -> Result<Global<JClass<'static>>, PresentError> {
    let loader = jni_call(env, "Context.getClassLoader", |env| {
        env.call_method(
            context,
            jni_str!("getClassLoader"),
            jni_sig!("()Ljava/lang/ClassLoader;"),
            &[],
        )?
        .l()
    })?;
    let class = jni_call(
        env,
        "ClassLoader.loadClass(FrustNativePresenter) — is the frust-native-widgets Gradle \
         module linked?",
        |env| {
            let name = env.new_string(PRESENTER_CLASS_BINARY)?;
            let class = env
                .call_method(
                    &loader,
                    jni_str!("loadClass"),
                    jni_sig!("(Ljava/lang/String;)Ljava/lang/Class;"),
                    &[JValue::Object(&name)],
                )?
                .l()?;
            env.cast_local::<JClass>(class)
        },
    )?;
    jni_call(env, "NewGlobalRef(FrustNativePresenter)", |env| {
        env.new_global_ref(class)
    })
}

/// `FrustNativePresenter.hasResumedActivity()` — whether there is an
/// `Activity` to present over right now.
///
/// # Errors
/// [`PresentError::Platform`] on a JNI failure.
pub(crate) fn has_resumed_activity(
    env: &mut Env<'_>,
    class: &Global<JClass<'static>>,
) -> Result<bool, PresentError> {
    jni_call(env, "FrustNativePresenter.hasResumedActivity", |env| {
        env.call_static_method(class, jni_str!("hasResumedActivity"), jni_sig!("()Z"), &[])?
            .z()
    })
}

/// Run JNI calls through the crate's exception-clearing helper
/// (`crate::android::run_jni`), as a [`PresentError::Platform`].
pub(crate) fn jni_call<'local, T>(
    env: &mut Env<'local>,
    op: &str,
    f: impl FnOnce(&mut Env<'local>) -> Result<T, jni::errors::Error>,
) -> Result<T, PresentError> {
    crate::android::run_jni(env, op, f).map_err(|e| PresentError::Platform(e.to_string()))
}

/// Maps one `nativeOnOutcome(code, actionIndex)` report into the parked
/// presentation's own outcome and resolves it — see the module doc's *The
/// live-presentation guard*.
pub(crate) type Resolve = Box<dyn FnOnce(i32, i32) + Send>;

/// Receives each non-terminal report — `(code, actionIndex)` for an
/// `OUTCOME_DETENT` (the detent's spec index) or an `OUTCOME_ROW` (the event
/// row's spec index) — see the module doc's *The live-presentation guard*.
pub(crate) type Stream = Arc<dyn Fn(i32, i32) + Send + Sync>;

/// Which presentation kind a parked entry is — one entry of each at most.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LiveKind {
    Alert,
    Sheet,
    /// A toast (`super::android_toast`): its entry only forwards the report
    /// to `super::toast`, which holds the toast's sender.
    Toast,
}

/// One parked presentation, tagged with its generation and kind.
struct LiveEntry {
    generation: u64,
    kind: LiveKind,
    resolve: Resolve,
    stream: Option<Stream>,
}

/// The live presentations' resolvers: at most one per [`LiveKind`]. A
/// `Mutex` rather than a thread-local: the arm parks it from the requesting
/// thread, the callback takes it on the main thread.
static LIVE: Mutex<Vec<LiveEntry>> = Mutex::new(Vec::new());

/// Lock [`LIVE`], recovering from poisoning rather than panicking near the
/// FFI boundary — the guarded data is a plain `Vec`.
fn lock_live() -> MutexGuard<'static, Vec<LiveEntry>> {
    LIVE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Park `resolve` (and a sheet's `stream`) as the live `kind` entry for
/// `generation`, answering the entry of the same kind it displaced (one whose
/// caller dropped the future). Returned rather than dropped here so no
/// resolver code runs under the lock; the arm resolves it, into a receiver
/// that is already gone.
pub(crate) fn install_live(
    kind: LiveKind,
    generation: u64,
    resolve: Resolve,
    stream: Option<Stream>,
) -> Option<Resolve> {
    let mut live = lock_live();
    let displaced = take_where(&mut live, |entry| entry.kind == kind);
    live.push(LiveEntry {
        generation,
        kind,
        resolve,
        stream,
    });
    displaced
}

/// Take the parked `kind` entry out unless it is `keep`'s — an abandoned
/// presentation a newer one replaces (`android_alert` takes a sheet that is
/// not the one its alert is shown over; `android_sheet` takes any alert).
pub(crate) fn take_stale(kind: LiveKind, keep: Option<u64>) -> Option<Resolve> {
    take_where(&mut lock_live(), |entry| {
        entry.kind == kind && Some(entry.generation) != keep
    })
}

/// Remove the first entry matching `pick` and answer its resolver.
fn take_where(live: &mut Vec<LiveEntry>, pick: impl Fn(&LiveEntry) -> bool) -> Option<Resolve> {
    let index = live.iter().position(pick)?;
    Some(live.swap_remove(index).resolve)
}

/// The live entry's [`Stream`], if `generation` is still parked and has one
/// — cloned out, so the caller runs it with the lock released.
fn live_stream(generation: u64) -> Option<Stream> {
    lock_live()
        .iter()
        .find(|entry| entry.generation == generation)
        .and_then(|entry| entry.stream.clone())
}

/// Take the live resolver out, but only if `generation` is still parked. The
/// lock is released by the time the caller runs it.
pub(crate) fn take_live(generation: u64) -> Option<Resolve> {
    take_where(&mut lock_live(), |entry| entry.generation == generation)
}

/// The resolver an alert parks: `super::wire`'s table over the spec's action
/// ids, into the alert's sender.
pub(crate) fn alert_resolver(tx: Sender<AlertOutcome>, action_ids: Vec<String>) -> Resolve {
    Box::new(move |code, action_index| {
        tx.send(wire::alert_outcome(code, action_index, &action_ids));
    })
}

/// The resolver a sheet parks: `super::wire`'s sheet table over the spec's
/// action ids, into the sheet's sender (which cancels an alert still shown
/// over the sheet first — `super::SheetSender`).
pub(crate) fn sheet_resolver(tx: SheetSender, action_ids: Vec<String>) -> Resolve {
    Box::new(move |code, action_index| {
        tx.send(wire::sheet_outcome(code, action_index, &action_ids));
    })
}

/// `FrustNativePresenter.nativeOnOutcome` — the presenter's one callback
/// into Rust (module doc's contract table), resolving presentation
/// `generation` through its parked resolver, or streaming an
/// `OUTCOME_DETENT`/`OUTCOME_ROW` report to its [`Stream`]. Called on the
/// main thread; a generation with no parked resolver is dropped.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_frust_nativewidgets_FrustNativePresenter_nativeOnOutcome<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    generation: jlong,
    code: jint,
    action_index: jint,
) {
    env.with_env(|_env| {
        // The inverse of the `u64 as jlong` an arm hands Kotlin — the same
        // 64 bits back.
        let generation = generation as u64;
        if code == wire::OUTCOME_DETENT || code == wire::OUTCOME_ROW {
            match live_stream(generation) {
                Some(stream) => stream(code, action_index),
                None => log::debug!(
                    "frust-native-widgets: sheet event {code} for generation {generation} has no \
                     live sheet listening — dropped"
                ),
            }
            return Ok::<(), jni::errors::Error>(());
        }
        match take_live(generation) {
            Some(resolve) => resolve(code, action_index),
            None => log::debug!(
                "frust-native-widgets: presenter outcome {code} for generation {generation} \
                 has no live presentation — dropped"
            ),
        }
        Ok::<(), jni::errors::Error>(())
    })
    .resolve::<LogErrorAndDefault>();
}
