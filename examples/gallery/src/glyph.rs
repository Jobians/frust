//! `Glyph`-design cases (see [`crate::case::Design::Glyph`]) — one case per
//! non-index page under `apps/website/widgets/design-systems/glyph/`, slug
//! `glyph/<file-stem>`: `accordion`, `alert`, `app-bar`, `badge-and-tag`,
//! `card`, `card-switcher`, `command-palette`, `dialog`, `fab`, `list`,
//! `loaders`, `nav-bar`, `page-dots`, `tabs`, `term-block`, `toast`,
//! `toggle`; plus the `chat-*` previews of `frust_glyph::chat` (see the chat
//! family note below).
//!
//! Every case is a static composition of the real `plugins/glyph` widgets,
//! built directly (never through `frust_glyph::install`/`show_glyph_dialog`/
//! `show_command_palette`'s navigator-overlay path, which would need a
//! reactive runtime this pure-`View` registry doesn't have — see the crate
//! docs' pure-`View` constraint). `GlyphDialogView`/`CommandPaletteView` are
//! themselves plain `View`s, so they compose the same way every other case
//! here does: no [`crate::base::framed`] fill, [`Case::DEFAULT_SIZE`] unless
//! a case has a reason to differ.
//!
//! # Font registration (verified, not fixed here)
//!
//! Glyph's type scale stacks every slot as
//! `FontFamily::stack_with_generic(["Space Mono" | "IBM Plex Mono"],
//! GenericSlot::Monospace)` (`plugins/glyph/src/tokens/scales.rs`) — a named
//! family with a generic monospace fallback baked into the token itself.
//! `crates/frust-testing/src/snapshot.rs`'s `snapshot_text_context` only
//! registers the four bundled *test* faces (Latin/Arabic/CJK/emoji,
//! `crates/frust-testing/src/fonts.rs`) — it does not call
//! `frust_glyph::font_data`/`install`, so "Space Mono"/"IBM Plex Mono" never
//! resolve by name in this harness. Every case here uses plain ASCII/simple
//! Unicode (glyph markers, box-drawing characters) that the generic
//! `Monospace` fallback in the same stack covers, so text still shapes with
//! real glyphs (no tofu) — just via the host's system monospace face rather
//! than the bundled one. Registering the bundled Glyph faces into the
//! snapshot `TextContext` would require a change to
//! `crates/frust-testing/src/snapshot.rs` (and possibly `theme.rs`), both
//! out of this task's `write_files` scope — see the task summary.

use kurbo::Size;

use frust_core::{AnyView, View};
use frust_glyph::chat::{
    ApprovalOption, ApprovalOptionKind, ApprovalState, ChatRole, CodeSpan, CodeTone, DiffInput,
    NoticeTone, PlanEntry, PlanEntryStatus, ToolContent, ToolKind, ToolStatus, UsageLabel,
    approval_card, code_block, composer, diff_view, message_row, message_scroller, notice_row,
    plan_block, tool_call, usage_meter,
};
use frust_glyph::{
    AlertVariant, BadgeVariant, PaletteItem, StatDelta, TermLine, ToastVariant, accordion, alert,
    app_bar, badge, card_switcher, command_palette, dots_loader, empty_state, fab, glyph_card,
    glyph_dialog, glyph_list, glyph_list_item, glyph_nav_bar, glyph_nav_item, page_dots, peer_card,
    progress, segmented_control, skeleton, stat_card, switcher_bar, switcher_chip, tabs, tag,
    term_block, toast, toggle,
};
use frust_widgets::{
    ButtonStyle, ChildKey, CrossAxisAlignment, EdgeInsets, Padding, SizedBox, button, column,
    flexible, icons, row, scaffold, text,
};

use crate::base::{framed, framed_in};
use crate::case::{Case, Design, case_build};

/// A taller-than-[`Case::DEFAULT_SIZE`] frame for a case whose content (a
/// list plus its empty-state counterpart, or a card plus a stat-card row)
/// runs past 240 logical px of height.
const TALL_FRAME: Size = Size::new(360.0, 420.0);

/// A modestly taller frame for a case (three stacked alerts) that clips by
/// only a few px at [`Case::DEFAULT_SIZE`]'s 240.
const MEDIUM_FRAME: Size = Size::new(360.0, 300.0);

/// `glyph/accordion` — one settled-open panel (no tween on first build, per
/// the accordion doc's "already open" contract) above a settled-closed one.
fn accordion_case() -> impl View<()> {
    framed(
        column()
            .child(
                accordion(
                    "Deploy pipeline",
                    text("Builds, tests, and ships the release artifact on every push to main."),
                )
                .open(true),
            )
            .child(SizedBox(None, Some(12.0)))
            .child(accordion("Rollback", text("Revert to the previous release.")).open(false)),
    )
}

/// `glyph/alert` — the four severities stacked.
fn alert_case() -> impl View<()> {
    framed_in(
        MEDIUM_FRAME,
        column()
            .child(alert(
                AlertVariant::Info,
                "Heads up",
                "A new workspace layout is available in settings.",
            ))
            .child(SizedBox(None, Some(10.0)))
            .child(alert(
                AlertVariant::Success,
                "Saved",
                "Your changes have been saved.",
            ))
            .child(SizedBox(None, Some(10.0)))
            .child(alert(
                AlertVariant::Warning,
                "Low disk space",
                "Free up space soon to avoid interruptions.",
            )),
    )
}

/// `glyph/app-bar` — the compact bar with a subtitle, elevated.
fn appbar_case() -> impl View<()> {
    framed(
        app_bar::<()>("Deploys")
            .subtitle("production")
            .elevated(true),
    )
}

/// `glyph/badge-and-tag` — one badge per status variant, plus a removable and
/// a static tag.
fn badge_and_tag_case() -> impl View<()> {
    framed(
        column()
            .child(
                row()
                    .child(badge("connected", BadgeVariant::Success).dot(true))
                    .child(SizedBox(Some(8.0), None))
                    .child(badge("degraded", BadgeVariant::Warning).dot(true))
                    .child(SizedBox(Some(8.0), None))
                    .child(badge("offline", BadgeVariant::Error).dot(true))
                    .cross_axis(CrossAxisAlignment::Center),
            )
            .child(SizedBox(None, Some(16.0)))
            .child(
                row()
                    .child(tag::<()>("stable"))
                    .child(SizedBox(Some(8.0), None))
                    .child(tag("v0.44.1").on_remove(|_: &mut ()| {}))
                    .cross_axis(CrossAxisAlignment::Center),
            ),
    )
}

/// `glyph/card` — a three-slot card with a status footer, above a two-up
/// stat-card row mixing both delta directions.
fn card_case() -> impl View<()> {
    framed_in(
        TALL_FRAME,
        column()
            .child(
                glyph_card::<()>()
                    .title(text("Deploy pipeline"))
                    .desc(text(
                        "Builds, tests, and ships the release artifact on every push to main.",
                    ))
                    .footer(row().child(badge("stable", BadgeVariant::Success).dot(true))),
            )
            .child(SizedBox(None, Some(14.0)))
            .child(row().flex(1, stat_card("Sessions", "1,284")).flex(
                1,
                stat_card("Uptime", "99.98%").delta(StatDelta::Up, "0.4%"),
            )),
    )
}

/// The `glyph/fab` frame: a short phone screen, so the scaffold's
/// bottom-right fab slot sits where it would on a device.
const FAB_FRAME: Size = Size::new(360.0, 320.0);

/// `glyph/fab` — the regular fab in a scaffold's fab slot (with its
/// `.tooltip` accessible name), over a body that also shows the `.mini`
/// variant inline. The scaffold is the root: the recorder's bounded root
/// constraints give it the whole frame, as `scaffold` in `crate::base` does.
fn fab_case() -> impl View<()> {
    scaffold(Padding(
        EdgeInsets::all(16.0),
        column()
            .child(glyph_list::<()>(vec![
                glyph_list_item("$", "deploy")
                    .sub("main → production")
                    .meta("2m ago"),
                glyph_list_item("#", "rollback")
                    .sub("v0.44.0 → v0.43.2")
                    .meta("1h ago"),
            ]))
            .child(SizedBox(None, Some(16.0)))
            .child(
                row()
                    .child(
                        fab(icons::ADD, |_: &mut ()| {})
                            .mini(true)
                            .tooltip("Add (mini)"),
                    )
                    .child(SizedBox(Some(12.0), None))
                    .child(text("mini").size(12.0))
                    .cross_axis(CrossAxisAlignment::Center),
            ),
    ))
    .app_bar(app_bar::<()>("Deploys"))
    .fab(fab(icons::ADD, |_: &mut ()| {}).tooltip("New deploy"))
}

/// `glyph/page-dots` — four pages with the second current, above the
/// `Role::Status` wording the indicator reports.
fn page_dots_case() -> impl View<()> {
    framed(
        column()
            .child(page_dots::<()>(4, 1))
            .child(SizedBox(None, Some(14.0)))
            .child(text("page 2 of 4").size(12.0))
            .cross_axis(CrossAxisAlignment::Center),
    )
}

/// The `glyph/card-switcher` frame: tall enough for the label row above the
/// card, the page dots below it and the action bar along the bottom edge.
/// Shared with the live twin in `crate::interactive::glyph`.
pub(crate) const SWITCHER_FRAME: Size = Size::new(360.0, 560.0);

/// The `glyph/card-switcher` servers, `(host, status, badge)`, in strip order.
/// The poster shows the first [`POSTER_HOSTS`]; the live twin's `add server`
/// chip adds the rest one at a time. Shared with that twin so its seed cannot
/// drift from the poster.
pub(crate) const HOSTS: [(&str, &str, BadgeVariant); 5] = [
    ("devbox", "ssh", BadgeVariant::Success),
    ("staging", "ssh", BadgeVariant::Success),
    ("prod-eu", "down", BadgeVariant::Error),
    ("new-host", "new", BadgeVariant::Neutral),
    ("prod-us", "ssh", BadgeVariant::Success),
];

/// How many of [`HOSTS`] the poster's strip holds.
pub(crate) const POSTER_HOSTS: usize = 3;

/// The strip position of the poster's live page: the middle server, so a peer
/// peeks in on each side.
pub(crate) const LIVE_HOST: usize = 1;

/// Each server's session rows, `(glyph, title, sub, meta)`, indexed like
/// [`HOSTS`].
pub(crate) const SESSIONS: [[(&str, &str, &str, &str); 3]; 5] = [
    [
        ("$", "build", "cargo check", "1m ago"),
        ("#", "logs", "journalctl", "live"),
        ("~", "shell", "bash", "idle"),
    ],
    [
        ("$", "deploy", "main", "2m ago"),
        ("#", "logs", "tail -f", "live"),
        ("~", "shell", "zsh", "idle"),
    ],
    [
        ("!", "pager", "disk full", "5m ago"),
        ("#", "logs", "stalled", "6m ago"),
        ("~", "shell", "refused", "down"),
    ],
    [
        ("+", "setup", "never connected", "new"),
        ("#", "logs", "empty", "-"),
        ("~", "shell", "not started", "-"),
    ],
    [
        ("$", "deploy", "release", "1h ago"),
        ("#", "logs", "tail -f", "live"),
        ("~", "shell", "fish", "idle"),
    ],
];

/// The label row above a card: a status badge and the host name.
fn host_label(host: usize) -> impl View<()> {
    let (name, status, variant) = HOSTS[host];
    row()
        .child(badge(status, variant))
        .child(SizedBox(Some(8.0), None))
        .child(text(name).size(13.0))
        .cross_axis(CrossAxisAlignment::Center)
}

/// One server's page: an app bar titled with the host, its session rows (the
/// list paints its own surface across the whole body, so a card shows an
/// opaque page) and the fab that opens the switcher.
fn host_page(host: usize) -> impl View<()> {
    let rows = SESSIONS[host]
        .iter()
        .map(|&(glyph, title, sub, meta)| glyph_list_item(glyph, title).sub(sub).meta(meta))
        .collect();
    scaffold(glyph_list::<()>(rows))
        .app_bar(app_bar::<()>(HOSTS[host].0))
        .fab(fab(icons::PANE_MARK, |_: &mut ()| {}).tooltip("Switch server"))
}

/// `glyph/card-switcher` — the Glyph skin open: the live page shrunk into the
/// centred card with its label row, a peer on each side, the page dots the
/// skin wires below the strip, and a [`switcher_bar`] of [`switcher_chip`]s.
/// Mounted open, the stage does not animate in, so the frame at rest is the
/// settled open stage. The stage fills the frame (its scrim is the backdrop),
/// so it is sized directly rather than through [`framed_in`].
fn card_switcher_case() -> impl View<()> {
    let peers = (0..POSTER_HOSTS)
        .filter(|&host| host != LIVE_HOST)
        .map(|host| peer_card(host_page(host)).label(host_label(host)));
    SizedBox(Some(SWITCHER_FRAME.width), Some(SWITCHER_FRAME.height)).child(
        card_switcher(true, host_page(LIVE_HOST))
            .peers(peers)
            .live_index(LIVE_HOST)
            .live_label(host_label(LIVE_HOST))
            .action_bar(switcher_bar((
                switcher_chip(icons::ADD, |_: &mut ()| {}).label("add server"),
                switcher_chip(icons::CLOSE, |_: &mut ()| {}).semantic_label("close"),
            ))),
    )
}

/// `glyph/command-palette` — a live query over a short filtered list, built
/// directly (not through the navigator-pushed `show_command_palette`).
///
/// The hint is spelled out as `"Cmd+D"` rather than the literal `⌘D`.
/// Neither bundled Glyph face — Space Mono nor IBM Plex Mono
/// (`plugins/glyph/src/tokens/scales.rs`) — carries U+2318 PLACE OF INTEREST
/// SIGN (confirmed against both faces' `cmap` tables), so the raw symbol
/// falls back to `.notdef` and paints as an empty box. Registering a
/// symbol-covering fallback face was considered and rejected: the live tier
/// is payload-constrained (the lp-00 fonts already cost +560,157 B gzipped),
/// and reachability, not bundling, is what the wasm linker keeps, so a new
/// face earns its weight back into the bundle the moment anything reaches
/// it. A spelled-out hint costs nothing and keeps this case eligible for the
/// live tier; the one loss is that the catalog no longer demonstrates the
/// literal glyph the real widget would show on a Mac keyboard.
fn command_palette_case() -> impl View<()> {
    framed(
        command_palette::<(), _, _>(
            vec![PaletteItem::new("Deploy").hint("Cmd+D")],
            |_: &mut (), _: String| {},
            |_: &mut (), _: usize| {},
        )
        .query("dep")
        .placeholder("Type a command or search…"),
    )
}

/// `glyph/dialog` — a confirm/cancel modal, built directly (not through the
/// navigator-pushed `show_glyph_dialog`).
fn dialog_case() -> impl View<()> {
    framed(
        glyph_dialog::<()>()
            .title("Revoke observer-token?")
            .body("Any device using this token loses access immediately. This can't be undone.")
            .action(
                button("Cancel", |_: &mut ()| {})
                    .style(ButtonStyle::Ghost)
                    .small(),
            )
            .action(
                button("Revoke token", |_: &mut ()| {})
                    .style(ButtonStyle::Danger)
                    .small(),
            ),
    )
}

/// `glyph/list` — a glyph-led list, above its zero-item `empty_state`
/// counterpart.
fn list_case() -> impl View<()> {
    framed_in(
        TALL_FRAME,
        column()
            .child(glyph_list::<()>(vec![
                glyph_list_item("$", "deploy")
                    .sub("main → production")
                    .meta("2m ago")
                    .chevron(true),
                glyph_list_item("#", "rollback")
                    .sub("v0.44.0 → v0.43.2")
                    .meta("1h ago")
                    .chevron(true),
            ]))
            .child(SizedBox(None, Some(14.0)))
            .child(
                empty_state::<()>(
                    "No deployments yet",
                    "Trigger your first deploy to see activity here.",
                )
                .glyph("▪")
                .action(button("Deploy now", |_: &mut ()| {}).style(ButtonStyle::Primary)),
            ),
    )
}

/// `glyph/loaders` — the determinate bar, the shimmer placeholder and the
/// indeterminate dot cycle.
fn loaders_case() -> impl View<()> {
    framed(
        column()
            .child(progress(0.65))
            .child(SizedBox(None, Some(18.0)))
            .child(skeleton(220.0, 16.0))
            .child(SizedBox(None, Some(18.0)))
            .child(dots_loader())
            .cross_axis(CrossAxisAlignment::Start),
    )
}

/// `glyph/nav-bar` — the glyph-character bottom bar.
fn navbar_case() -> impl View<()> {
    framed(glyph_nav_bar::<(), _>(
        vec![
            glyph_nav_item("┌", "frame"),
            glyph_nav_item("─", "stream"),
            glyph_nav_item("╳", "close"),
        ],
        0,
        |_: &mut (), _: usize| {},
    ))
}

/// `glyph/tabs` — the label tab strip above a pill segmented control.
fn tabs_case() -> impl View<()> {
    framed(
        column()
            .child(tabs::<(), _>(
                vec![
                    "Overview".to_string(),
                    "Activity".to_string(),
                    "Settings".to_string(),
                ],
                0,
                |_: &mut (), _: usize| {},
            ))
            .child(SizedBox(None, Some(20.0)))
            .child(segmented_control::<(), _>(
                vec![
                    "List".to_string(),
                    "Grid".to_string(),
                    "Compact".to_string(),
                ],
                1,
                |_: &mut (), _: usize| {},
            )),
    )
}

/// `glyph/term-block` — a prompt/output/comment transcript, unstaggered.
///
/// The per-line reveal is opt-in ([`term_block`]'s `staggered`, left at its
/// `false` default here), so this case's widget builds no
/// `AnimationController` at all and there is no clock for a [`Case::time_ms`]
/// to move. Warming it would change nothing even if it did: a *settled*
/// stagger paints every line at full alpha, which is exactly what the
/// unstaggered widget already paints. Measured rather than argued — recorded
/// at `time_ms: 400` with the warm pass active, both variants came back
/// byte-identical.
fn term_block_case() -> impl View<()> {
    framed(term_block(vec![
        TermLine::prompt("frust build apk --release"),
        TermLine::output("Compiling glyph-catalog v0.1.0"),
        TermLine::output("Finished release [optimized] target(s) in 38.2s"),
        TermLine::comment("# staggered per-line reveal — replays below"),
    ]))
}

/// `glyph/toast` — three severities of the transient message itself, not the
/// host.
///
/// The enter/hold/exit timeline belongs to
/// [`toast_host`](frust_glyph::toast_host); the bare toast this case composes
/// reads no frame time in `paint` whatsoever, so it has nothing a warm pass or
/// a [`Case::time_ms`] could advance. Measured the same way as
/// [`term_block_case`]: `time_ms: 400` recorded byte-identical PNGs.
fn toast_case() -> impl View<()> {
    framed(
        column()
            .child(toast("New layout available").variant(ToastVariant::Info))
            .child(SizedBox(None, Some(12.0)))
            .child(toast("Changes saved").variant(ToastVariant::Success))
            .child(SizedBox(None, Some(12.0)))
            .child(toast("Low disk space").variant(ToastVariant::Warning)),
    )
}

/// `glyph/toggle` — checked and unchecked, both labelled.
fn toggle_case() -> impl View<()> {
    framed(
        column()
            .child(toggle::<(), _>(true, |_: &mut (), _: bool| {}).label("Notifications"))
            .child(SizedBox(None, Some(16.0)))
            .child(toggle::<(), _>(false, |_: &mut (), _: bool| {}).label("Auto-sync")),
    )
}

// ---- chat family -----------------------------------------------------------
//
// Every chat component is controlled and non-reactive, so a case is the
// component built from plain data with its callbacks wired to no-ops, and only
// in a state a correctly wired app can produce: a collapsible block carries its
// toggle callback (a block with none never collapses), a pending approval its
// `on_select`, a running composer its `on_stop`.
//
// Determinism: no case streams (`.streaming` stays off, so no caret blinks),
// none sets a busy loader or a working notice (both animate), and no case
// makes a clock-dependent label. `ApprovalDeadline` has no public fixed-clock
// seam (it counts down from a real `Instant`), so the pending approval is
// shown WITHOUT a deadline; the countdown bar and its time label are not
// previewed. Monospace text uses the generic fallback (see the module docs).

/// The vertical gap between the states a chat case stacks.
const CHAT_GAP: f64 = 14.0;

/// A tall frame for the cases that stack several states.
const CHAT_TALL: Size = Size::new(360.0, 560.0);

/// A frame for a single, content-heavy component.
const CHAT_MEDIUM: Size = Size::new(360.0, 340.0);

/// A chat case's frame: the states stacked with [`CHAT_GAP`] between them,
/// inside a 12 px margin.
fn chat_frame(size: Size, states: Vec<AnyView<()>>) -> impl View<()> {
    let mut stack = column();
    for (i, state) in states.into_iter().enumerate() {
        if i > 0 {
            stack = stack.child(SizedBox(None, Some(CHAT_GAP)));
        }
        stack = stack.child(state);
    }
    framed_in(size, Padding(EdgeInsets::all(12.0), stack))
}

fn option(id: &str, label: &str, kind: ApprovalOptionKind) -> ApprovalOption {
    ApprovalOption {
        id: id.to_owned(),
        label: label.to_owned(),
        kind,
    }
}

fn plan_entry(text: &str, status: PlanEntryStatus) -> PlanEntry {
    PlanEntry {
        text: text.to_owned(),
        status,
    }
}

/// The two-hunk-free edit the diff and tool-call cases share.
fn sample_edit() -> DiffInput {
    DiffInput {
        path: "src/parse.rs".to_owned(),
        old_text: Some(
            "fn parse(input: &str) -> Option<u32> {\n    let n = input.trim().parse().ok()?;\n    Some(n)\n}\n"
                .to_owned(),
        ),
        new_text: "fn parse(input: &str) -> Option<u32> {\n    let n: u32 = input.trim().parse().ok()?;\n    n.checked_add(1)\n}\n"
            .to_owned(),
    }
}

/// `glyph/chat-message-user` — a short and a long user bubble.
fn chat_message_user_case() -> impl View<()> {
    chat_frame(
        Case::DEFAULT_SIZE,
        vec![
            AnyView::new(message_row(ChatRole::User, "Fix the failing parser test.")),
            AnyView::new(message_row(
                ChatRole::User,
                "The parser test fails on an input with a trailing newline. Can you find out why \
                 and fix it without changing the public API?",
            )),
        ],
    )
}

/// `glyph/chat-message-assistant` — settled assistant markdown: a heading, a
/// list, inline code, a link and a table.
fn chat_message_assistant_case() -> impl View<()> {
    chat_frame(
        CHAT_TALL,
        vec![AnyView::new(
            message_row(
                ChatRole::Assistant,
                "## Findings\n\n\
                 The test fails because `parse` trims the input but never the **newline** \
                 inside it. Three things to change:\n\n\
                 - read the value with `str::trim_end`\n\
                 - return `None` for an empty string\n\
                 - add a regression test\n\n\
                 See the [parser notes](https://example.com/parser) for the grammar.\n\n\
                 | Input | Before | After |\n\
                 |---|---|---|\n\
                 | `\"7\\n\"` | `None` | `Some(7)` |\n\
                 | `\"\"` | `None` | `None` |\n",
            )
            .on_link(|_: &mut (), _url: &str| {}),
        )],
    )
}

/// `glyph/chat-message-thought` — a thought row closed, then open.
fn chat_message_thought_case() -> impl View<()> {
    chat_frame(
        Case::DEFAULT_SIZE,
        vec![
            AnyView::new(
                message_row(ChatRole::Thought, "Checking how the input is trimmed.")
                    .thought_open(false)
                    .on_thought_toggle(|_: &mut ()| {}),
            ),
            AnyView::new(
                message_row(
                    ChatRole::Thought,
                    "The failing input ends in a newline. `trim` removes it, so the cause must \
                     be elsewhere; the number parse is the next place to look.",
                )
                .thought_open(true)
                .on_thought_toggle(|_: &mut ()| {}),
            ),
        ],
    )
}

/// `glyph/chat-code-block` — a highlighted Rust block, then a plain one.
fn chat_code_block_case() -> impl View<()> {
    let rust = "fn main() {\n    // greet\n    let name = \"frust\";\n    println!(\"{name}\");\n}";
    chat_frame(
        CHAT_MEDIUM,
        vec![
            AnyView::new(
                code_block(rust)
                    .language("rust")
                    .spans(vec![
                        CodeSpan {
                            range: 0..2,
                            style: CodeTone::Keyword,
                        },
                        CodeSpan {
                            range: 3..7,
                            style: CodeTone::Function,
                        },
                        CodeSpan {
                            range: 17..24,
                            style: CodeTone::Comment,
                        },
                    ])
                    .on_copy(|_: &mut (), _code: &str| {}),
            ),
            AnyView::new(
                code_block("$ cargo test -p parser\nrunning 3 tests\ntest result: ok")
                    .on_copy(|_: &mut (), _code: &str| {}),
            ),
        ],
    )
}

/// `glyph/chat-diff` — an edit of an existing file.
fn chat_diff_case() -> impl View<()> {
    chat_frame(
        CHAT_MEDIUM,
        vec![AnyView::new(
            diff_view(sample_edit()).on_toggle(|_: &mut ()| {}),
        )],
    )
}

/// `glyph/chat-diff-new-file` — a file the agent created.
fn chat_diff_new_file_case() -> impl View<()> {
    chat_frame(
        Case::DEFAULT_SIZE,
        vec![AnyView::new(
            diff_view(DiffInput {
                path: "tests/parse.rs".to_owned(),
                old_text: None,
                new_text: "#[test]\nfn trailing_newline() {\n    assert_eq!(parse(\"7\\n\"), Some(8));\n}\n"
                    .to_owned(),
            })
            .on_toggle(|_: &mut ()| {}),
        )],
    )
}

/// `glyph/chat-tool-call` — one collapsed card per status, then an open card
/// with output.
fn chat_tool_call_case() -> impl View<()> {
    let toggle = |_: &mut ()| {};
    chat_frame(
        CHAT_TALL,
        vec![
            AnyView::new(
                tool_call(ToolKind::Read, "Read src/parse.rs", ToolStatus::Pending)
                    .on_toggle(toggle),
            ),
            AnyView::new(
                tool_call(
                    ToolKind::Search,
                    "Search for parse(",
                    ToolStatus::InProgress,
                )
                .on_toggle(toggle),
            ),
            AnyView::new(
                tool_call(ToolKind::Edit, "Edit src/parse.rs", ToolStatus::Completed)
                    .on_toggle(toggle),
            ),
            AnyView::new(
                tool_call(ToolKind::Execute, "cargo test", ToolStatus::Failed).on_toggle(toggle),
            ),
            AnyView::new(
                tool_call(
                    ToolKind::Execute,
                    "cargo test -p parser",
                    ToolStatus::Completed,
                )
                .open(true)
                .on_toggle(toggle)
                .content(ToolContent::Text(
                    "running 3 tests\ntest trims_input ... ok\ntest rejects_empty ... ok\n\
                         test trailing_newline ... ok\n\ntest result: ok. 3 passed"
                        .to_owned(),
                ))
                .on_output_toggle(toggle)
                .on_copy(|_: &mut (), _text: &str| {}),
            ),
        ],
    )
}

/// `glyph/chat-plan` — one entry in each status.
fn chat_plan_case() -> impl View<()> {
    chat_frame(
        Case::DEFAULT_SIZE,
        vec![AnyView::new(
            plan_block(vec![
                plan_entry("Reproduce the failing test", PlanEntryStatus::Completed),
                plan_entry("Trim the input before parsing", PlanEntryStatus::InProgress),
                plan_entry("Add a regression test", PlanEntryStatus::Pending),
            ])
            .title("Plan"),
        )],
    )
}

/// `glyph/chat-approval` — a pending request (no countdown: see the family
/// note above), an allowed one and an expired one.
fn chat_approval_case() -> impl View<()> {
    let options = || {
        vec![
            option("allow", "Allow once", ApprovalOptionKind::AllowOnce),
            option("always", "Always allow", ApprovalOptionKind::AllowAlways),
            option("deny", "Reject", ApprovalOptionKind::RejectOnce),
        ]
    };
    chat_frame(
        Size::new(360.0, 620.0),
        vec![
            AnyView::new(
                approval_card("Run a shell command?", options())
                    .detail("cargo fmt --all")
                    .on_select(|_: &mut (), _id: &str| {}),
            ),
            AnyView::new(
                approval_card("Run a shell command?", options())
                    .detail("cargo fmt --all")
                    .state(ApprovalState::Allowed)
                    .selected("allow")
                    .on_select(|_: &mut (), _id: &str| {}),
            ),
            AnyView::new(
                approval_card("Delete build output?", options())
                    .detail("rm -rf target/debug")
                    .state(ApprovalState::Expired)
                    .expired_hint("Ask the agent to try again.")
                    .on_select(|_: &mut (), _id: &str| {}),
            ),
        ],
    )
}

/// `glyph/chat-composer` — empty, multi-line, running and disabled.
fn chat_composer_case() -> impl View<()> {
    fn wired(value: &str) -> frust_glyph::chat::ComposerView<()> {
        composer(value)
            .on_change(|_: &mut (), _text: String| {})
            .on_submit(|_: &mut (), _text: String| {})
            .on_stop(|_: &mut ()| {})
    }
    chat_frame(
        CHAT_TALL,
        vec![
            AnyView::new(wired("").placeholder("Ask anything")),
            AnyView::new(wired(
                "Fix the parser test.\nIt fails on a trailing newline.\nKeep the API.",
            )),
            AnyView::new(wired("Also update the docs").running(true)),
            AnyView::new(wired("").disabled("Sign in to chat")),
        ],
    )
}

/// `glyph/chat-scroller` — a short transcript at its end, then one scrolled
/// up from it with the jump control and an unread count.
fn chat_scroller_case() -> impl View<()> {
    fn transcript(at_end: bool, unread: usize) -> impl View<()> {
        let mut scroller = message_scroller(
            3,
            |i| ChildKey::new(i as u64),
            |i| match i {
                0 => AnyView::new(message_row(ChatRole::User, "Fix the parser test.")),
                1 => AnyView::new(message_row(
                    ChatRole::Assistant,
                    "The input keeps its trailing newline. I will trim it first.",
                )),
                _ => AnyView::new(tool_call(
                    ToolKind::Edit,
                    "Edit src/parse.rs",
                    ToolStatus::Completed,
                )),
            },
        )
        .at_end(at_end)
        .on_at_end_change(|_: &mut (), _at_end: bool| {})
        .padding(EdgeInsets::all(12.0));
        if !at_end {
            scroller = scroller.unread(unread);
        }
        scroller
    }
    framed_in(
        Size::new(360.0, 520.0),
        column()
            .push(flexible(1, transcript(true, 0)))
            .child(SizedBox(None, Some(CHAT_GAP)))
            .push(flexible(1, transcript(false, 3))),
    )
}

/// `glyph/chat-usage` — normal, warning and critical, and the compact form.
fn chat_usage_case() -> impl View<()> {
    chat_frame(
        Case::DEFAULT_SIZE,
        vec![
            AnyView::new(usage_meter(42_000, 200_000)),
            AnyView::new(usage_meter(170_000, 200_000).label(UsageLabel::Counts)),
            AnyView::new(usage_meter(196_000, 200_000)),
            AnyView::new(usage_meter(98_000, 200_000).compact(true)),
        ],
    )
}

/// `glyph/chat-notice` — each tone.
fn chat_notice_case() -> impl View<()> {
    chat_frame(
        Case::DEFAULT_SIZE,
        vec![
            AnyView::new(notice_row("Connected to the agent")),
            AnyView::new(notice_row("Changes saved").tone(NoticeTone::Success)),
            AnyView::new(notice_row("Context nearly full").tone(NoticeTone::Warning)),
            AnyView::new(notice_row("The agent stopped unexpectedly").tone(NoticeTone::Error)),
        ],
    )
}

/// This module's slice of the registry [`crate::cases`]: one entry per
/// `apps/website/widgets/design-systems/glyph/*.mdx` page (see the module
/// docs' slug list).
pub const CASES: &[Case] = &[
    Case {
        slug: "glyph/accordion",
        title: "Accordion",
        size: Case::DEFAULT_SIZE,
        scale: Case::DEFAULT_SCALE,
        time_ms: Case::DEFAULT_TIME_MS,
        design: Design::Glyph,
        build: case_build!(accordion_case),
    },
    Case {
        slug: "glyph/alert",
        title: "Alert",
        size: MEDIUM_FRAME,
        scale: Case::DEFAULT_SCALE,
        time_ms: Case::DEFAULT_TIME_MS,
        design: Design::Glyph,
        build: case_build!(alert_case),
    },
    Case {
        slug: "glyph/app-bar",
        title: "App bar",
        size: Case::DEFAULT_SIZE,
        scale: Case::DEFAULT_SCALE,
        time_ms: Case::DEFAULT_TIME_MS,
        design: Design::Glyph,
        build: case_build!(appbar_case),
    },
    Case {
        slug: "glyph/badge-and-tag",
        title: "Badge and tag",
        size: Case::DEFAULT_SIZE,
        scale: Case::DEFAULT_SCALE,
        time_ms: Case::DEFAULT_TIME_MS,
        design: Design::Glyph,
        build: case_build!(badge_and_tag_case),
    },
    Case {
        slug: "glyph/card",
        title: "Card",
        size: TALL_FRAME,
        scale: Case::DEFAULT_SCALE,
        time_ms: Case::DEFAULT_TIME_MS,
        design: Design::Glyph,
        build: case_build!(card_case),
    },
    Case {
        slug: "glyph/card-switcher",
        title: "Card switcher",
        size: SWITCHER_FRAME,
        scale: Case::DEFAULT_SCALE,
        time_ms: Case::DEFAULT_TIME_MS,
        design: Design::Glyph,
        build: case_build!(card_switcher_case),
    },
    Case {
        slug: "glyph/command-palette",
        title: "Command palette",
        size: Case::DEFAULT_SIZE,
        scale: Case::DEFAULT_SCALE,
        time_ms: Case::DEFAULT_TIME_MS,
        design: Design::Glyph,
        build: case_build!(command_palette_case),
    },
    Case {
        slug: "glyph/dialog",
        title: "Dialog",
        size: Case::DEFAULT_SIZE,
        scale: Case::DEFAULT_SCALE,
        time_ms: Case::DEFAULT_TIME_MS,
        design: Design::Glyph,
        build: case_build!(dialog_case),
    },
    Case {
        slug: "glyph/fab",
        title: "Floating action button",
        size: FAB_FRAME,
        scale: Case::DEFAULT_SCALE,
        time_ms: Case::DEFAULT_TIME_MS,
        design: Design::Glyph,
        build: case_build!(fab_case),
    },
    Case {
        slug: "glyph/list",
        title: "List",
        size: TALL_FRAME,
        scale: Case::DEFAULT_SCALE,
        time_ms: Case::DEFAULT_TIME_MS,
        design: Design::Glyph,
        build: case_build!(list_case),
    },
    Case {
        slug: "glyph/loaders",
        title: "Loaders",
        size: Case::DEFAULT_SIZE,
        scale: Case::DEFAULT_SCALE,
        time_ms: Case::DEFAULT_TIME_MS,
        design: Design::Glyph,
        build: case_build!(loaders_case),
    },
    Case {
        slug: "glyph/nav-bar",
        title: "Nav bar",
        size: Case::DEFAULT_SIZE,
        scale: Case::DEFAULT_SCALE,
        time_ms: Case::DEFAULT_TIME_MS,
        design: Design::Glyph,
        build: case_build!(navbar_case),
    },
    Case {
        slug: "glyph/page-dots",
        title: "Page dots",
        size: Case::DEFAULT_SIZE,
        scale: Case::DEFAULT_SCALE,
        time_ms: Case::DEFAULT_TIME_MS,
        design: Design::Glyph,
        build: case_build!(page_dots_case),
    },
    Case {
        slug: "glyph/tabs",
        title: "Tabs and segmented control",
        size: Case::DEFAULT_SIZE,
        scale: Case::DEFAULT_SCALE,
        time_ms: Case::DEFAULT_TIME_MS,
        design: Design::Glyph,
        build: case_build!(tabs_case),
    },
    Case {
        slug: "glyph/term-block",
        title: "Terminal block",
        size: Case::DEFAULT_SIZE,
        scale: Case::DEFAULT_SCALE,
        time_ms: Case::DEFAULT_TIME_MS,
        design: Design::Glyph,
        build: case_build!(term_block_case),
    },
    Case {
        slug: "glyph/toast",
        title: "Toast",
        size: Case::DEFAULT_SIZE,
        scale: Case::DEFAULT_SCALE,
        time_ms: Case::DEFAULT_TIME_MS,
        design: Design::Glyph,
        build: case_build!(toast_case),
    },
    Case {
        slug: "glyph/toggle",
        title: "Toggle",
        size: Case::DEFAULT_SIZE,
        scale: Case::DEFAULT_SCALE,
        time_ms: Case::DEFAULT_TIME_MS,
        design: Design::Glyph,
        build: case_build!(toggle_case),
    },
    Case {
        slug: "glyph/chat-message-user",
        title: "Chat message, user",
        size: Case::DEFAULT_SIZE,
        scale: Case::DEFAULT_SCALE,
        time_ms: Case::DEFAULT_TIME_MS,
        design: Design::Glyph,
        build: case_build!(chat_message_user_case),
    },
    Case {
        slug: "glyph/chat-message-assistant",
        title: "Chat message, assistant",
        size: CHAT_TALL,
        scale: Case::DEFAULT_SCALE,
        time_ms: Case::DEFAULT_TIME_MS,
        design: Design::Glyph,
        build: case_build!(chat_message_assistant_case),
    },
    Case {
        slug: "glyph/chat-message-thought",
        title: "Chat message, thought",
        size: Case::DEFAULT_SIZE,
        scale: Case::DEFAULT_SCALE,
        time_ms: Case::DEFAULT_TIME_MS,
        design: Design::Glyph,
        build: case_build!(chat_message_thought_case),
    },
    Case {
        slug: "glyph/chat-code-block",
        title: "Chat code block",
        size: CHAT_MEDIUM,
        scale: Case::DEFAULT_SCALE,
        time_ms: Case::DEFAULT_TIME_MS,
        design: Design::Glyph,
        build: case_build!(chat_code_block_case),
    },
    Case {
        slug: "glyph/chat-diff",
        title: "Chat diff, edit",
        size: CHAT_MEDIUM,
        scale: Case::DEFAULT_SCALE,
        time_ms: Case::DEFAULT_TIME_MS,
        design: Design::Glyph,
        build: case_build!(chat_diff_case),
    },
    Case {
        slug: "glyph/chat-diff-new-file",
        title: "Chat diff, new file",
        size: Case::DEFAULT_SIZE,
        scale: Case::DEFAULT_SCALE,
        time_ms: Case::DEFAULT_TIME_MS,
        design: Design::Glyph,
        build: case_build!(chat_diff_new_file_case),
    },
    Case {
        slug: "glyph/chat-tool-call",
        title: "Chat tool call",
        size: CHAT_TALL,
        scale: Case::DEFAULT_SCALE,
        time_ms: Case::DEFAULT_TIME_MS,
        design: Design::Glyph,
        build: case_build!(chat_tool_call_case),
    },
    Case {
        slug: "glyph/chat-plan",
        title: "Chat plan",
        size: Case::DEFAULT_SIZE,
        scale: Case::DEFAULT_SCALE,
        time_ms: Case::DEFAULT_TIME_MS,
        design: Design::Glyph,
        build: case_build!(chat_plan_case),
    },
    Case {
        slug: "glyph/chat-approval",
        title: "Chat approval",
        size: Size::new(360.0, 620.0),
        scale: Case::DEFAULT_SCALE,
        time_ms: Case::DEFAULT_TIME_MS,
        design: Design::Glyph,
        build: case_build!(chat_approval_case),
    },
    Case {
        slug: "glyph/chat-composer",
        title: "Chat composer",
        size: CHAT_TALL,
        scale: Case::DEFAULT_SCALE,
        time_ms: Case::DEFAULT_TIME_MS,
        design: Design::Glyph,
        build: case_build!(chat_composer_case),
    },
    Case {
        slug: "glyph/chat-scroller",
        title: "Chat scroller",
        size: Size::new(360.0, 520.0),
        scale: Case::DEFAULT_SCALE,
        time_ms: Case::DEFAULT_TIME_MS,
        design: Design::Glyph,
        build: case_build!(chat_scroller_case),
    },
    Case {
        slug: "glyph/chat-usage",
        title: "Chat usage meter",
        size: Case::DEFAULT_SIZE,
        scale: Case::DEFAULT_SCALE,
        time_ms: Case::DEFAULT_TIME_MS,
        design: Design::Glyph,
        build: case_build!(chat_usage_case),
    },
    Case {
        slug: "glyph/chat-notice",
        title: "Chat notice",
        size: Case::DEFAULT_SIZE,
        scale: Case::DEFAULT_SCALE,
        time_ms: Case::DEFAULT_TIME_MS,
        design: Design::Glyph,
        build: case_build!(chat_notice_case),
    },
];
