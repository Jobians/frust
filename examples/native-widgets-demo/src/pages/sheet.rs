//! **Sheet** — the native page sheet through `show_native_sheet_into`: a
//! request answered by exactly one [`SheetOutcome`], its content a
//! constrained native schema (title, message, image, up to three action rows —
//! never frust widgets), themed from the active frust [`Theme`].
//!
//! Variants: medium + large with a grabber (the default), a non-dismissible
//! sheet (no swipe-down — only a row or a programmatic dismiss ends it), a
//! custom 0.4 detent (iOS 16+; iOS 15 falls back to the nearest system
//! detent, logged), and a programmatic run that expands to large after 1 s
//! and dismisses after 3 s through the returned `SheetHandle`. User detent
//! drags stream through `on_detent` into the detent readout; they are never
//! outcomes.
//!
//! **Alert over a sheet**: a sheet whose destructive "Stop sharing…" row is
//! an event (`SheetSpec::on_row_tap`), not an outcome — its tap opens a
//! confirmation alert above the sheet, which stays up. The readout lists
//! every resolution in arrival order (each presentation must appear once),
//! and the refusals the tap probes while the two are up: a second sheet, a
//! second alert, and a sheet while the alert is up, each `Busy`. A second
//! trigger dismisses the sheet under the alert 3 s after the alert opens:
//! the alert must resolve `Cancelled` before the sheet.
//!
//! iOS/iPadOS present `UISheetPresentationController`; Android presents a
//! Material `BottomSheetDialog`: Medium is half height (ratio 0.5), Large is
//! expanded, `Custom(f)` is half-ratio `f`. Only ONE half ratio is draggable
//! at a time (the others are reachable through `select_detent`);
//! `largest_undimmed` and `scrolling_expands` are ignored on Android; detent
//! changes still stream to `on_detent`. macOS and every Linux/Windows desktop
//! preview refuse with `PresentError::Unsupported`, which the status line
//! reports - the designed behavior there, not a failure. An iPad in regular
//! width presents a centered form sheet that ignores detents (see the plugin
//! README). Zero native slots at rest.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use frust::{AnyView, FlexChild, Get, GetUntracked, Set, Theme, button, inflexible};
use frust_native_widgets::{
    ActionRole, AlertSpec, Detent, PresentError, Presentation, SheetContent, SheetHandle,
    SheetOutcome, SheetSpec, show_native_alert, show_native_sheet,
};

use crate::push_toast;

use super::alerts::note_outcome;
use super::common::{
    S, block, caption, demo_image_bytes, gap, label, local_sig, page_column, page_header, readout,
    theme,
};

/// This page's index in [`SECTION_LABELS`](crate::SECTION_LABELS).
const SECTION: usize = 5;

/// The native class backing a sheet on this target.
const SHEET_CLASS: &str = if cfg!(target_os = "android") {
    "BottomSheetDialog"
} else if cfg!(target_os = "ios") {
    "UISheetPresentationController"
} else {
    "nothing (Unsupported)"
};

/// The custom detent's fraction of the available height.
const CUSTOM_DETENT: f64 = 0.4;

/// When the programmatic run expands to [`Detent::Large`].
const EXPAND_AFTER: Duration = Duration::from_secs(1);
/// When the programmatic run dismisses, counted from the expand.
const DISMISS_AFTER_EXPAND: Duration = Duration::from_secs(2);

/// How long the forced run leaves the alert up before dismissing the sheet
/// under it.
const DISMISS_UNDER_ALERT_AFTER: Duration = Duration::from_secs(3);

local_sig!(outcome_sig, Option<SheetOutcome>, None);
local_sig!(status_sig, String, "No request yet.".to_string());
local_sig!(detent_sig, String, "(none reported)".to_string());
local_sig!(confirm_status_sig, String, "No request yet.".to_string());
local_sig!(confirm_order_sig, Vec<String>, Vec::new());
local_sig!(confirm_probe_sig, String, "(not probed yet)".to_string());
local_sig!(confirm_sheet_sig, Option<SheetHandle>, None);

/// The shared content: title, message, the demo image and three action rows.
fn content() -> SheetContent {
    SheetContent::new()
        .with_title("Share draft")
        .with_message("Anyone with the link can read it. Drag the grabber to change detents.")
        .with_image(demo_image_bytes())
        .with_action("copy", "Copy link", ActionRole::Default)
        .with_action("stop", "Stop sharing", ActionRole::Destructive)
        .with_action("close", "Close", ActionRole::Cancel)
}

/// A sheet spec over [`content`], themed from `theme`, streaming user detent
/// changes into the detent readout.
fn base_spec(theme: &Theme) -> SheetSpec {
    SheetSpec::new(content())
        .with_theme(theme)
        .on_detent(|detent| detent_sig().set(format!("{detent:?}")))
}

/// Present `spec`, clearing the last outcome. An accepted request reads
/// "presented" at once; a refusal resolves on the next task-queue pump and
/// replaces it. The outcome is awaited on the UI task queue, written into this
/// page's signal and counted for the Dialogs badge when another tab is
/// showing. Returns the handle when shown.
fn present_sheet(state: &S, what: &str, spec: SheetSpec) -> Option<SheetHandle> {
    outcome_sig().set(None);
    detent_sig().set("(none reported)".to_string());
    let presentation = show_native_sheet(spec);
    let handle = presentation.sheet_handle();
    if handle.is_some() {
        status_sig().set(format!("{what}: presented"));
    }
    let (tab, unseen) = (state.tab, state.unseen_outcomes);
    let what = what.to_string();
    frust::spawn_local(async move {
        match presentation.await {
            Ok(outcome) => {
                outcome_sig().set(Some(outcome));
                note_outcome(tab, unseen);
            }
            Err(PresentError::Unsupported) => status_sig().set(format!(
                "{what}: Unsupported \u{2014} native sheets are iOS/iPadOS/Android only (designed)"
            )),
            Err(err) => status_sig().set(format!("{what}: refused: {err}")),
        }
    });
    handle
}

/// A readable line for the last outcome.
fn describe_outcome(outcome: Option<&SheetOutcome>) -> String {
    match outcome {
        None => "Outcome: (none yet)".to_string(),
        Some(SheetOutcome::Action(id)) => format!("Outcome: Action(\"{id}\")"),
        Some(SheetOutcome::Dismissed(reason)) => format!("Outcome: Dismissed({reason:?})"),
        Some(SheetOutcome::HostLost) => "Outcome: HostLost".to_string(),
    }
}

/// The confirm sheet's content: "Stop sharing…" is the event row whose tap
/// opens the confirmation alert; the other two rows end the sheet.
fn confirm_content() -> SheetContent {
    SheetContent::new()
        .with_title("Shared draft")
        .with_message("Stop sharing asks first: its confirmation opens over this sheet.")
        .with_action("copy", "Copy link", ActionRole::Default)
        .with_action("stop", "Stop sharing\u{2026}", ActionRole::Destructive)
        .with_action("close", "Close", ActionRole::Cancel)
}

/// Record one resolution, in arrival order.
fn note_resolution(line: String) {
    let mut order = confirm_order_sig().get_untracked();
    order.push(line);
    confirm_order_sig().set(order);
}

/// What a request the demo expects to be refused answered: its error, read
/// without waiting (a refused presentation is already resolved).
fn refusal<T>(mut presentation: Presentation<T>) -> String {
    if presentation.handle().is_some() {
        // Not expected: a refused request has no handle.
        return "ACCEPTED (expected a refusal)".to_string();
    }
    match Pin::new(&mut presentation).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(Err(PresentError::Busy)) => "Busy".to_string(),
        Poll::Ready(Err(err)) => format!("{err}"),
        Poll::Ready(Ok(_)) | Poll::Pending => "no answer".to_string(),
    }
}

/// A minimal sheet for the refusal probes — never shown.
fn probe_sheet() -> SheetSpec {
    SheetSpec::new(SheetContent::new().with_title("Probe"))
}

/// A minimal alert for the refusal probes — never shown.
fn probe_alert() -> AlertSpec {
    AlertSpec::new("Probe", "").with_action("ok", "OK", ActionRole::Default)
}

/// The "Stop sharing…" row's tap, on the platform's main thread with the
/// sheet still up: probe a second sheet, open the confirmation over the
/// sheet, probe a second alert and a sheet while the alert is up, and — for
/// the forced run — dismiss the sheet under the alert after
/// [`DISMISS_UNDER_ALERT_AFTER`].
fn confirm_over_sheet(dismiss_sheet_under_alert: bool) {
    let second_sheet = refusal(show_native_sheet(probe_sheet()));
    let presentation = show_native_alert(
        AlertSpec::new(
            "Stop sharing?",
            "People with the link lose access. The sheet stays open behind this alert.",
        )
        .with_action("keep", "Keep sharing", ActionRole::Cancel)
        .with_action("stop", "Stop sharing", ActionRole::Destructive),
    );
    let second_alert = refusal(show_native_alert(probe_alert()));
    let sheet_over_alert = refusal(show_native_sheet(probe_sheet()));
    confirm_probe_sig().set(format!(
        "Probes: second sheet {second_sheet} \u{b7} second alert {second_alert} \u{b7} sheet \
         while the alert is up {sheet_over_alert}"
    ));
    let accepted = presentation.handle().is_some();
    confirm_status_sig().set(if accepted {
        "Confirmation: presented over the sheet".to_string()
    } else {
        "Confirmation: refused".to_string()
    });
    frust::spawn_local(async move {
        match presentation.await {
            Ok(outcome) => note_resolution(format!("alert {outcome:?}")),
            Err(err) => note_resolution(format!("alert refused: {err}")),
        }
    });
    if accepted && dismiss_sheet_under_alert {
        frust::spawn_local(async move {
            match frust::spawn_blocking(|| std::thread::sleep(DISMISS_UNDER_ALERT_AFTER)).await {
                Ok(()) => {
                    if let Some(handle) = confirm_sheet_sig().get_untracked() {
                        handle.dismiss();
                    }
                }
                Err(join_err) => log::warn!(
                    "native-widgets-demo sheet: dismiss-under-alert timer panicked: {join_err}"
                ),
            }
        });
    }
}

/// Present the confirm sheet, clearing the last run's readout. Its outcome is
/// awaited on the UI task queue, recorded in arrival order and counted for
/// the Dialogs badge when another tab is showing.
fn present_confirm_sheet(state: &S, theme: &Theme, dismiss_sheet_under_alert: bool) {
    confirm_order_sig().set(Vec::new());
    confirm_probe_sig().set("(not probed yet)".to_string());
    let spec = SheetSpec::new(confirm_content())
        .with_theme(theme)
        .on_row_tap("stop", move || {
            confirm_over_sheet(dismiss_sheet_under_alert)
        });
    let presentation = show_native_sheet(spec);
    let handle = presentation.sheet_handle();
    confirm_sheet_sig().set(handle);
    if handle.is_some() {
        confirm_status_sig().set("Sheet: presented".to_string());
    }
    let (tab, unseen) = (state.tab, state.unseen_outcomes);
    frust::spawn_local(async move {
        match presentation.await {
            Ok(outcome) => {
                note_resolution(format!("sheet {outcome:?}"));
                note_outcome(tab, unseen);
            }
            Err(PresentError::Unsupported) => confirm_status_sig().set(
                "Sheet: Unsupported \u{2014} native sheets are iOS/iPadOS/Android only (designed)"
                    .to_string(),
            ),
            Err(err) => confirm_status_sig().set(format!("Sheet: refused: {err}")),
        }
        confirm_sheet_sig().set(None);
    });
}

/// One trigger block: a heading, a caption and a button.
fn trigger(
    title: &str,
    note: &str,
    button_label: &str,
    handler: impl Fn(&mut S) + 'static,
) -> frust::FlexChild<S> {
    block(vec![
        inflexible(label(title)),
        gap(4.0),
        inflexible(caption(note)),
        gap(6.0),
        inflexible(button(button_label, handler)),
    ])
}

/// The deep-link slug of this section: the anchor its tab scrolls to.
pub const ANCHOR: &str = "Sheet";

/// The section body, without a page header: the rows the Dialogs tab stacks
/// under its own section header. See the [module docs](self).
pub fn section(state: &S) -> Vec<FlexChild<S>> {
    // Copy handle, cheap to move into the programmatic run's timers below —
    // see their `Err(join_err)` arms.
    let toasts = state.toasts;
    let outcome = outcome_sig().get();
    let status = status_sig().get();
    let detent = detent_sig().get();
    // Captured at build: an event handler runs outside the build pass, where
    // no theme context is provided.
    let active = theme();

    let readouts = block(vec![
        inflexible(label("Readout")),
        gap(4.0),
        inflexible(readout(format!("Last request: {status}"))),
        inflexible(readout(describe_outcome(outcome.as_ref()))),
        inflexible(readout(format!("Last user detent: {detent}"))),
    ]);

    let arms = block(vec![
        inflexible(label("Per-platform arm")),
        gap(4.0),
        inflexible(caption(format!(
            "iOS/iPadOS UISheetPresentationController \u{b7} Android BottomSheetDialog \u{b7} \
             macOS and desktop previews Unsupported \u{2014} here: {}.",
            SHEET_CLASS
        ))),
        gap(2.0),
        inflexible(caption(
            "Android: Medium = half height (ratio 0.5), Large = expanded, Custom(f) = half \
             ratio f. Only one half ratio is draggable at a time (others via select_detent); \
             largest_undimmed and scrolling_expands are ignored; detent changes still stream \
             to the readout below.",
        )),
    ]);

    let t1 = active.clone();
    let standard = trigger(
        "Medium + large, grabber",
        "The default spec: resting at medium, draggable to large, swipe down to dismiss.",
        "Show sheet",
        move |state: &mut S| {
            present_sheet(
                state,
                "Medium + large",
                base_spec(&t1).with_detents([Detent::Medium, Detent::Large]),
            );
        },
    );

    let t2 = active.clone();
    let non_dismissible = trigger(
        "Non-dismissible",
        "No swipe-down: only an action row ends it.",
        "Show non-dismissible sheet",
        move |state: &mut S| {
            present_sheet(
                state,
                "Non-dismissible",
                base_spec(&t2).with_dismissible(false),
            );
        },
    );

    let t3 = active.clone();
    let custom = trigger(
        "Custom 0.4 detent",
        "Opens at 40% height with large available. iOS 16+; iOS 15 uses the nearest of \
         medium/large.",
        "Show 0.4 sheet",
        move |state: &mut S| {
            present_sheet(
                state,
                "Custom 0.4",
                base_spec(&t3)
                    .with_detents([Detent::Custom(CUSTOM_DETENT), Detent::Large])
                    .with_selected(Detent::Custom(CUSTOM_DETENT)),
            );
        },
    );

    let t5 = active.clone();
    let confirm = trigger(
        "Alert over a sheet",
        "\"Stop sharing\u{2026}\" keeps the sheet up and opens a confirmation above it; \
         confirm or cancel it, then close the sheet. Each resolution is listed once, in order.",
        "Show sheet with a confirmation",
        move |state: &mut S| present_confirm_sheet(state, &t5, false),
    );

    let t6 = active.clone();
    let confirm_forced = trigger(
        "Sheet dismissed under its alert",
        "As above, but 3 s after the confirmation opens the app dismisses the sheet: the alert \
         must resolve Cancelled first, then the sheet Dismissed(Programmatic).",
        "Show, confirm, dismiss the sheet",
        move |state: &mut S| present_confirm_sheet(state, &t6, true),
    );

    let confirm_order = confirm_order_sig().get();
    let confirm_readouts = block(vec![
        inflexible(label("Alert over a sheet \u{2014} readout")),
        gap(4.0),
        inflexible(readout(format!(
            "Last request: {}",
            confirm_status_sig().get()
        ))),
        inflexible(readout(if confirm_order.is_empty() {
            "Resolved: (none yet)".to_string()
        } else {
            format!("Resolved, in order: {}", confirm_order.join(" \u{2192} "))
        })),
        inflexible(readout(confirm_probe_sig().get())),
    ]);

    let t4 = active;
    let programmatic = trigger(
        "Programmatic control",
        "Presents at medium, expands to large after 1 s (select_detent), then dismisses after \
         2 s more: the outcome must read Dismissed(Programmatic).",
        "Show, expand, dismiss",
        move |state: &mut S| {
            let Some(handle) = present_sheet(state, "Programmatic", base_spec(&t4)) else {
                return;
            };
            frust::spawn_local(async move {
                // Match each join `Result` and surface a failure rather than
                // silently dropping it (`examples/playground`'s `camera.rs`
                // convention); an expand-timer failure skips the dismiss
                // timer too, since the sheet's detent state is now unknown.
                match frust::spawn_blocking(|| std::thread::sleep(EXPAND_AFTER)).await {
                    Ok(()) => handle.select_detent(Detent::Large),
                    Err(join_err) => {
                        log::warn!(
                            "native-widgets-demo sheet: programmatic expand timer panicked: \
                             {join_err}"
                        );
                        push_toast(
                            toasts,
                            "Sheet: programmatic expand timer failed".to_string(),
                        );
                        return;
                    }
                }
                match frust::spawn_blocking(|| std::thread::sleep(DISMISS_AFTER_EXPAND)).await {
                    Ok(()) => handle.dismiss(),
                    Err(join_err) => {
                        log::warn!(
                            "native-widgets-demo sheet: programmatic dismiss timer panicked: \
                             {join_err}"
                        );
                        push_toast(
                            toasts,
                            "Sheet: programmatic dismiss timer failed".to_string(),
                        );
                    }
                }
            });
        },
    );

    vec![
        readouts,
        arms,
        standard,
        non_dismissible,
        custom,
        programmatic,
        confirm_readouts,
        confirm,
        confirm_forced,
    ]
}

/// The whole section as a standalone page: its [`page_header`] then
/// [`section`]. Kept for [`super::current`] and the headless gate tests; the
/// Dialogs tab stacks [`section`] under its own section header instead.
pub fn page(state: &S) -> AnyView<S> {
    let mut children = vec![page_header(SECTION)];
    children.extend(section(state));
    page_column(children)
}
