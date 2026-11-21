//! The Home tab's root page, after the GitHub app's home: a "My Work" card of
//! icon-tile rows that each push a [`detail`](super::detail) page, filtered
//! live by the bar's search field, a "Favorites" card whose one control is a
//! real native button, a "Notifications" card driving the bar's bell badge, a
//! "Search" card that drives the search field from the app, rebuilds the bar
//! around it on demand and holds a frust text field, and a "Shortcuts" note. The body is frust, the bars are the
//! shell's native chrome, and the button that stays put is native.

use frust::authoring::text::FontWeight;
use frust::{
    AnyView, Axis, Color, EdgeInsets, FlexChild, FlexView, GestureDetector, Get, IconSource,
    Padding, Set, Theme, Update, View, container, divider, flexible, icon, icons, inflexible, text,
    text_input,
};
use frust_native_widgets::{ChromeEvent, native_button};

use super::common::{
    S, caption, chip, gap, gap_h, local_sig, page_column, readout, row, tab_body, theme,
};
use super::detail;
use crate::{DetailRoute, Tab, push_toast};

/// The repository every pushed page is subtitled with.
const REPOSITORY: &str = "frust-rs/frust";

/// The most characters a pushed page's bar title keeps before its ellipsis.
const TITLE_CHARS: usize = 32;

/// Side length of a row's coloured icon tile.
const TILE: f64 = 40.0;

/// Width and height of the native "Add Favorites" button.
const FAVORITES_BUTTON: (f64, f64) = (180.0, 44.0);

/// How long the Search card's rebuild chip waits before it flips the bell:
/// time to focus a field or type into the search first.
const REBUILD_DELAY: std::time::Duration = std::time::Duration::from_secs(3);

/// One "My Work" row: its label, icon, the theme colour that fills its tile,
/// and the colour of the icon on it.
struct WorkRow {
    label: &'static str,
    icon: IconSource,
    tile: fn(&Theme) -> Color,
    glyph: fn(&Theme) -> Color,
}

/// The "My Work" rows, in the GitHub app's order.
const WORK_ROWS: [WorkRow; 7] = [
    WorkRow {
        label: "Issues",
        icon: icons::DESCRIPTION,
        tile: |t| t.scheme().primary,
        glyph: |t| t.scheme().surface,
    },
    WorkRow {
        label: "Pull Requests",
        icon: icons::ARROW_FORWARD,
        tile: |t| t.scheme().tertiary,
        glyph: |t| t.scheme().surface,
    },
    WorkRow {
        label: "Discussions",
        icon: icons::FORUM,
        tile: |t| t.scheme().secondary,
        glyph: |t| t.scheme().surface,
    },
    WorkRow {
        label: "Projects",
        icon: icons::PUSH_PIN,
        tile: |t| t.scheme().error,
        glyph: |t| t.scheme().surface,
    },
    WorkRow {
        label: "Top Repositories",
        icon: icons::TAG,
        tile: |t| t.scheme().tertiary_container,
        glyph: |t| t.scheme().on_tertiary_container,
    },
    WorkRow {
        label: "Organizations",
        icon: icons::GROUP,
        tile: |t| t.scheme().on_tertiary_container,
        glyph: |t| t.scheme().surface,
    },
    WorkRow {
        label: "Starred",
        icon: icons::STAR,
        tile: |t| t.scheme().on_secondary_container,
        glyph: |t| t.scheme().surface,
    },
];

// The Search card's frust text field, for moving the keyboard between it and
// the bar's native search field.
local_sig!(note_field, String, String::new());

/// See the page-fn contract in [`crate::pages`].
pub fn page(state: &S) -> AnyView<S> {
    let sections = vec![
        section_title("My Work", Some("...")),
        gap(8.0),
        inflexible(card(work_rows(state, &state.search_query.get()))),
        gap(20.0),
        section_title("Favorites", None),
        gap(8.0),
        inflexible(card(favorites_body(state))),
        gap(20.0),
        section_title("Notifications", None),
        gap(8.0),
        inflexible(card(notifications_body(state))),
        gap(20.0),
        section_title("Search", None),
        gap(8.0),
        inflexible(card(search_body(state))),
        gap(20.0),
        section_title("Shortcuts", None),
        gap(8.0),
        inflexible(caption(
            "Shortcuts you add show up here for one-tap access.",
        )),
    ];
    tab_body(state, Tab::Home, vec![inflexible(page_column(sections))])
}

/// A section heading, with an optional trailing chip (the "..." menu).
fn section_title(title: &str, trailing: Option<&str>) -> FlexChild<S> {
    let mut children = vec![flexible(
        1,
        text(title)
            .size(20.0)
            .weight(FontWeight::BOLD)
            .color(theme().scheme().on_surface),
    )];
    if let Some(trailing) = trailing {
        children.push(inflexible(chip(trailing, |_: &mut S| {})));
    }
    inflexible(row(children))
}

/// A rounded surface card around `children`.
fn card(children: Vec<FlexChild<S>>) -> impl View<S> {
    container(Padding(
        EdgeInsets::all(12.0),
        FlexView::new(Axis::Vertical, children),
    ))
    .fill(theme().scheme().surface_container)
    .radius(12.0)
}

/// Whether `label` matches the search `query`: every row for an empty one,
/// else a case-insensitive substring match.
fn matches(label: &str, query: &str) -> bool {
    let query = query.trim();
    query.is_empty() || label.to_lowercase().contains(&query.to_lowercase())
}

/// The "My Work" rows matching the search `query`, a divider between each
/// pair, or a line saying none does.
fn work_rows(state: &S, query: &str) -> Vec<FlexChild<S>> {
    let mut children = Vec::new();
    let shown = WORK_ROWS.iter().filter(|work| matches(work.label, query));
    for (index, work) in shown.enumerate() {
        if index > 0 {
            children.push(gap(8.0));
            children.push(inflexible(divider(theme().scheme().outline_variant)));
            children.push(gap(8.0));
        }
        children.push(inflexible(work_row(state, work)));
    }
    if children.is_empty() {
        children.push(inflexible(caption(format!(
            "No work matches '{}'.",
            query.trim()
        ))));
    }
    children
}

/// One tappable row: coloured icon tile, label, trailing chevron. Tapping
/// pushes the detail page onto the Home navigator.
fn work_row(state: &S, work: &WorkRow) -> impl View<S> {
    let theme = theme();
    let tile = container(icon(work.icon).size(22.0).color((work.glyph)(&theme)))
        .fill((work.tile)(&theme))
        .radius(10.0)
        .size_centered(TILE, TILE);
    let label = text(work.label).size(16.0).color(theme.scheme().on_surface);
    let chevron = icon(icons::CHEVRON_RIGHT)
        .size(22.0)
        .color(theme.scheme().on_surface_variant);
    let title = detail_title(work.label);
    let handles = state.clone();
    GestureDetector(row(vec![
        inflexible(tile),
        gap_h(12.0),
        flexible(1, label),
        inflexible(chevron),
    ]))
    .on_tap(move |state: &mut S| open_detail(state, &handles, &title))
}

/// The bar title for a pushed row: its label over the sample subject,
/// truncated with an ellipsis like the GitHub app does.
fn detail_title(label: &str) -> String {
    let full = format!("{label}: damage-aware present pipeline");
    if full.chars().count() <= TITLE_CHARS {
        return full;
    }
    let kept: String = full.chars().take(TITLE_CHARS - 1).collect();
    format!("{}\u{2026}", kept.trim_end())
}

/// Push a detail page titled `title` onto the Home navigator. Runs in a tap
/// handler, never in `build`. The page opens with its composer closed.
fn open_detail(state: &mut S, handles: &S, title: &str) {
    state.end_search();
    state.composer_open.set(false);
    let route = DetailRoute::new(title, REPOSITORY);
    let (handles, shown) = (handles.clone(), route.clone());
    route.push(state.nav(Tab::Home), move || detail::page(&handles, &shown));
}

/// The Favorites card: two lines of explanation and the one native control in
/// this body.
fn favorites_body(state: &S) -> Vec<FlexChild<S>> {
    let toasts = state.toasts;
    vec![
        inflexible(readout(
            "Favorites give you quick access to your most used items.",
        )),
        gap(4.0),
        inflexible(caption(
            "Add repositories, issues or pull requests to see them here.",
        )),
        gap(12.0),
        inflexible(
            native_button("Add Favorites")
                .content_description("Add a favorite")
                .size(FAVORITES_BUTTON.0, FAVORITES_BUTTON.1)
                .on_press(move || push_toast(toasts, "Favorites added".to_string())),
        ),
    ]
}

/// The Notifications card: a readout of the bell's unread count and the last
/// chrome event (the bell's tap lands here), and frust buttons that bump and
/// clear the count the bell is badged with. No native slot, so Home's
/// documented at-rest count is unchanged.
fn notifications_body(state: &S) -> Vec<FlexChild<S>> {
    let unread = state.notifications.get();
    let event = state.chrome_event.get();
    vec![
        inflexible(readout(describe_bell(unread, event.as_ref()))),
        gap(4.0),
        inflexible(caption(
            "The bell in the bar is native chrome badged with this count; '+' is the \
             prominent action.",
        )),
        gap(12.0),
        inflexible(row(vec![
            inflexible(chip("Notify", |state: &mut S| {
                state.notifications.update(|n| *n = n.saturating_add(1));
            })),
            gap_h(8.0),
            inflexible(chip("Clear", |state: &mut S| state.notifications.set(0))),
        ])),
    ]
}

/// The Search card: the bar field's state, chips that drive it from the app
/// (a preset query that opens it, a clear, a close that keeps the query), a
/// chip that rebuilds the bar's actions a few seconds later (it hides or
/// shows the bell), and a frust text field to move the keyboard to and from.
fn search_body(state: &S) -> Vec<FlexChild<S>> {
    let (query, active) = (state.search_query.get(), state.search_active.get());
    let bell_hidden = state.bell_hidden;
    let note = note_field();
    vec![
        inflexible(readout(describe_search(&query, active))),
        gap(4.0),
        inflexible(caption(
            "The bar's search field is native; the My Work rows above filter as it is typed into.",
        )),
        gap(12.0),
        inflexible(row(vec![
            inflexible(chip("Find 'pull'", |state: &mut S| {
                state.search_active.set(true);
                state.search_query.set("pull".to_string());
            })),
            gap_h(8.0),
            inflexible(chip("Clear query", |state: &mut S| {
                state.search_query.set(String::new());
            })),
        ])),
        gap(8.0),
        inflexible(chip("Close, keep the query", |state: &mut S| {
            state.search_active.set(false);
        })),
        gap(8.0),
        inflexible(chip(
            "Toggle the bell in 3 s (rebuilds the bar)",
            move |_: &mut S| {
                frust::spawn_local(async move {
                    match frust::spawn_blocking(|| std::thread::sleep(REBUILD_DELAY)).await {
                        Ok(()) => bell_hidden.update(|hidden| *hidden = !*hidden),
                        Err(join_err) => {
                            log::warn!(
                                "native-widgets-demo search: rebuild timer panicked: {join_err}"
                            );
                        }
                    }
                });
            },
        )),
        gap(12.0),
        inflexible(
            text_input(note.get(), move |_: &mut S, value: String| note.set(value))
                .placeholder("A frust field: tap it while searching"),
        ),
    ]
}

/// The Search readout: the query and whether the field is open, and how many
/// work rows match.
fn describe_search(query: &str, active: bool) -> String {
    let shown = WORK_ROWS
        .iter()
        .filter(|work| matches(work.label, query))
        .count();
    let state = if active { "open" } else { "closed" };
    format!(
        "Search {state} \u{b7} query '{query}' \u{b7} {shown} of {} rows",
        WORK_ROWS.len()
    )
}

/// The Notifications readout: `unread` and the last chrome event.
fn describe_bell(unread: u32, event: Option<&ChromeEvent>) -> String {
    let last = event.map_or_else(|| "(none yet)".to_string(), |event| format!("{event:?}"));
    format!("Unread: {unread} \u{b7} last chrome event: {last}")
}

#[cfg(test)]
mod tests {
    use frust_native_widgets::ActionId;

    use super::*;

    #[test]
    fn the_search_filters_rows_case_insensitively_and_the_readout_counts_them() {
        assert!(matches("Pull Requests", ""));
        assert!(matches("Pull Requests", "  "));
        assert!(matches("Pull Requests", "pull"));
        assert!(matches("Pull Requests", "REQ"));
        assert!(!matches("Issues", "pull"));
        assert_eq!(
            describe_search("pull", true),
            "Search open \u{b7} query 'pull' \u{b7} 1 of 7 rows"
        );
        assert_eq!(
            describe_search("", false),
            "Search closed \u{b7} query '' \u{b7} 7 of 7 rows"
        );
    }

    #[test]
    fn the_readout_names_the_count_and_the_last_event() {
        assert_eq!(
            describe_bell(0, None),
            "Unread: 0 \u{b7} last chrome event: (none yet)"
        );
        let tapped = ChromeEvent::ActionTapped(ActionId::new("bell"));
        let line = describe_bell(3, Some(&tapped));
        assert!(line.starts_with("Unread: 3"), "{line}");
        assert!(line.contains("bell"), "{line}");
    }
}
