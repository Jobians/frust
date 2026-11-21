//! The Dialogs tab's root page: the tab header, then the Alerts, Sheet,
//! Toasts and Menus sections, each under its own section header, in one
//! scroll on the tab's shared scroll controller (anchors work as on the
//! Controls tab; see [`controls_tab`](super::controls_tab)).
//!
//! The Toasts section shows native toasts through `show_native_toast` (a
//! Material Snackbar on Android, a capsule above the tab bar on iOS): short
//! and long ones, one with an action, two in a row (the second replaces the
//! first, which reads Dismissed), one shown 3 s later (open a sheet or an
//! alert, or focus the field, before it fires) and one per edit of a text
//! field, so it shows while the keyboard is up. Each toast's one outcome
//! lands in the section's readout. Zero native slots.
//!
//! The Menus section is explanation plus a readout: the "+" and "..." menus
//! in the bar are native pull-downs (`UIMenu` on iOS, `PopupMenu` on
//! Android), the Comment toolbar on a Home detail page is native too (an iOS
//! toolbar, an Android FAB). The readout is frust state fed by the
//! scaffold's `events_into` signal; the menu is chrome. Zero native slots.

use std::time::Duration;

use frust::{
    AnyView, FlexChild, Get, GetUntracked, Set, Update, button, checkbox, inflexible, text_input,
};
use frust_native_widgets::{
    ChromeEvent, ToastDuration, ToastOutcome, ToastSpec, show_native_toast,
};

use super::common::{
    S, anchor_probe, block, caption, gap, label, local_sig, page_column, page_header, readout,
    tab_body,
};
use super::controls_tab::{scroll_on_request, titled};
use super::{SECTION_SUMMARIES, alerts, sheet};
use crate::{SECTION_LABELS, Tab};

/// The deep-link slug of the Menus section.
pub const MENUS_ANCHOR: &str = "Menus";

/// The deep-link slug of the Toasts section.
pub const TOASTS_ANCHOR: &str = "Toasts";

/// The [`anchor_probe`](super::common::anchor_probe) key of this tab's header.
const TOP: &str = "dialogs.top";

/// Every anchor this tab scrolls to.
const ANCHORS: &[&str] = &[alerts::ANCHOR, sheet::ANCHOR, TOASTS_ANCHOR, MENUS_ANCHOR];

/// How many outcome lines the Toasts readout keeps, newest first.
const TOAST_LOG_LINES: usize = 6;

/// How long the delayed toast waits before it shows.
const TOAST_DELAY: Duration = Duration::from_secs(3);

local_sig!(toast_log, Vec<String>, Vec::new());
local_sig!(toast_count, u32, 0);
local_sig!(toast_glass, bool, true);
local_sig!(toast_field, String, String::new());

/// Prepend one line to the Toasts readout.
fn log_toast(line: String) {
    toast_log().update(|log| {
        log.insert(0, line);
        log.truncate(TOAST_LOG_LINES);
    });
}

/// A readable name for one toast outcome.
fn describe_toast(outcome: ToastOutcome) -> &'static str {
    match outcome {
        ToastOutcome::TimedOut => "TimedOut",
        ToastOutcome::Dismissed => "Dismissed",
        ToastOutcome::Action => "Action",
        _ => "(an outcome this demo does not know)",
    }
}

/// Show `spec` as toast number N (wearing the glass toggle), log "shown" for
/// an accepted request, and log its one outcome (or refusal) when it lands.
/// Event/task context only.
fn present_toast(what: &str, spec: ToastSpec) {
    let number = toast_count().get_untracked() + 1;
    toast_count().set(number);
    let what = format!("#{number} {what}");
    let presentation = show_native_toast(spec.with_glass(toast_glass().get_untracked()));
    if presentation.handle().is_some() {
        log_toast(format!("{what}: shown"));
    }
    frust::spawn_local(async move {
        let line = match presentation.await {
            Ok(outcome) => format!("{what}: {}", describe_toast(outcome)),
            Err(err) => format!("{what}: refused: {err}"),
        };
        log_toast(line);
    });
}

/// The Toasts section body: the triggers, the text field and the readout.
fn toasts_section() -> Vec<FlexChild<S>> {
    let log = toast_log().get();
    let glass = toast_glass().get();
    let mut readouts = vec![inflexible(label("Readout")), gap(4.0)];
    if log.is_empty() {
        readouts.push(inflexible(readout("Toast outcomes: (none yet)")));
    }
    for line in log {
        readouts.push(inflexible(readout(line)));
    }
    vec![
        block(readouts),
        block(vec![
            inflexible(label("Short, long and with an action")),
            gap(4.0),
            inflexible(caption(
                "A native toast sits above the bottom navigation and the toolbar, never \
                 under them. Without an action it times out (Short 1.5 s, Long 2.75 s); the \
                 action resolves Action; a swipe resolves Dismissed.",
            )),
            gap(6.0),
            inflexible(checkbox(glass, "glass (iOS 26)", |_: &mut S, on: bool| {
                toast_glass().set(on)
            })),
            gap(6.0),
            inflexible(button("Show short toast", |_: &mut S| {
                present_toast(
                    "Short",
                    ToastSpec::new("Saved to drafts").with_duration(ToastDuration::Short),
                );
            })),
            gap(6.0),
            inflexible(button("Show long toast", |_: &mut S| {
                present_toast(
                    "Long",
                    ToastSpec::new("Synced 12 items with the server")
                        .with_duration(ToastDuration::Long),
                );
            })),
            gap(6.0),
            inflexible(button("Show toast with action", |_: &mut S| {
                present_toast(
                    "With action",
                    ToastSpec::new("Message archived")
                        .with_action("Undo")
                        .with_duration(ToastDuration::Long),
                );
            })),
        ]),
        block(vec![
            inflexible(label("Replace, delay, keyboard")),
            gap(4.0),
            inflexible(caption(
                "Two in a row: the second replaces the first, which reads Dismissed. The \
                 delayed one shows 3 s after the tap: open a sheet or an alert, or focus the \
                 field, before it fires. Typing in the field toasts each edit above the \
                 keyboard.",
            )),
            gap(6.0),
            inflexible(button("Show two toasts", |_: &mut S| {
                present_toast("First", ToastSpec::new("First toast"));
                present_toast(
                    "Second",
                    ToastSpec::new("Second toast, replacing the first")
                        .with_duration(ToastDuration::Long),
                );
            })),
            gap(6.0),
            inflexible(button("Show toast in 3 s", |_: &mut S| {
                log_toast("Delayed toast: waiting 3 s".to_string());
                frust::spawn_local(async move {
                    match frust::spawn_blocking(|| std::thread::sleep(TOAST_DELAY)).await {
                        Ok(()) => present_toast(
                            "Delayed",
                            ToastSpec::new("Shown 3 s after the tap")
                                .with_action("OK")
                                .with_duration(ToastDuration::Long),
                        ),
                        Err(join_err) => {
                            log::warn!(
                                "native-widgets-demo toasts: delay timer panicked: {join_err}"
                            );
                            log_toast("Delayed toast: timer failed".to_string());
                        }
                    }
                });
            })),
            gap(6.0),
            inflexible(
                text_input(toast_field().get(), |_: &mut S, value: String| {
                    let count = value.chars().count();
                    let unit = if count == 1 {
                        "character"
                    } else {
                        "characters"
                    };
                    toast_field().set(value);
                    present_toast(
                        "Typed",
                        ToastSpec::new(format!("The field holds {count} {unit}")),
                    );
                })
                .placeholder("Type to toast above the keyboard"),
            ),
        ]),
    ]
}

/// A readable line for the last chrome event, `None` before any.
fn describe_event(event: Option<&ChromeEvent>) -> String {
    match event {
        None => "Last chrome event: (none yet)".to_string(),
        Some(event) => format!("Last chrome event: {event:?}"),
    }
}

/// The last toolbar/action tap, if the last event was one.
fn describe_action(event: Option<&ChromeEvent>) -> String {
    match event {
        Some(ChromeEvent::ActionTapped(id)) => format!("Last ActionTapped id: {id:?}"),
        _ => "Last ActionTapped id: (the last event was not an action tap)".to_string(),
    }
}

/// The Menus section body: what the bar's menus and the detail toolbar are,
/// and the readout of the last chrome event.
fn menus_section(state: &S) -> Vec<FlexChild<S>> {
    let event = state.chrome_event.get();
    vec![
        block(vec![
            inflexible(label("Pull-down menus")),
            gap(4.0),
            inflexible(caption(
                "The '+' and '...' menus in the bar are native pull-downs (UIMenu on iOS, \
                 PopupMenu on Android), not frust widgets. Choosing an item reports a \
                 ChromeEvent the app routes.",
            )),
            gap(6.0),
            inflexible(readout(describe_event(event.as_ref()))),
        ]),
        block(vec![
            inflexible(label("Toolbar")),
            gap(4.0),
            inflexible(caption(
                "The 'Comment' toolbar on a Home detail page is native too: an iOS toolbar, \
                 an Android FAB. Its tap arrives as ActionTapped with the action's id.",
            )),
            gap(6.0),
            inflexible(readout(describe_action(event.as_ref()))),
            gap(2.0),
            inflexible(caption(
                "The readout is frust; the menu and the toolbar are chrome.",
            )),
        ]),
    ]
}

/// See the page-fn contract in [`crate::pages`].
pub fn page(state: &S) -> AnyView<S> {
    scroll_on_request(state, Tab::Dialogs, TOP, ANCHORS);
    tab_body(
        state,
        Tab::Dialogs,
        vec![
            inflexible(anchor_probe(
                TOP,
                page_column(vec![page_header(Tab::Dialogs)]),
            )),
            titled(
                alerts::ANCHOR,
                SECTION_LABELS[3],
                SECTION_SUMMARIES[3],
                alerts::section(state),
            ),
            titled(
                sheet::ANCHOR,
                SECTION_LABELS[5],
                SECTION_SUMMARIES[5],
                sheet::section(state),
            ),
            titled(
                TOASTS_ANCHOR,
                TOASTS_ANCHOR,
                "Native toasts above the app's bars and the keyboard, with each outcome read back.",
                toasts_section(),
            ),
            titled(
                MENUS_ANCHOR,
                MENUS_ANCHOR,
                "The bar's pull-down menus and the detail toolbar, with the last event read back.",
                menus_section(state),
            ),
        ],
    )
}
