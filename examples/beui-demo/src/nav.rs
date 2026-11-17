//! The gallery's shell: the [`Page`] catalog, the wide shell's rail and top
//! bar, and the phone shell's top bar and page list.
//!
//! # Two shells, one breakpoint
//!
//! At or above [`COMPACT_BREAKPOINT`] the shell is the rail beside the page.
//! Below it there is no rail: [`phone_shell`] is a top bar (a menu button,
//! the page title, the theme toggle) over the page, and the menu button opens
//! every page in a left [`drawer`] grouped by section ([`page_list`]).
//!
//! # Why the rail is one flat list
//!
//! `frust_beui::components::animated_sidebar` ports upstream's panel and
//! menu, not its group-header slot, so there is no beUI primitive for a
//! labelled rail section. The rail folds each grouped page's section into its
//! label instead (`"Motion \u{b7} Text"`). The drawer's list is grouped,
//! because grouped lists with section headers are beUI parts.
//!
//! [`Page::Content`] and [`Page::GpuEffects`] join `Home` and `Theming` as
//! pages standing outside every section. GPU Effects is always listed,
//! whether or not the crate was built with the `gpu-effects` feature; the page
//! itself explains what is on when the feature is off.

use frust::{
    CrossAxisAlignment, EdgeInsets, IconSource, Padding, Set, SizedBox, View, column, icon, icons,
    row, safe_area, scroll_view, stack, text,
};
use frust_beui::BeuiMetrics;
use frust_beui::components::animated_sidebar::{
    AnimatedSidebarView, SidebarItem, animated_sidebar, sidebar_item,
};
use frust_beui::components::button::{ButtonSize, ButtonTone, button};
use frust_beui::components::drawer::{DrawerSide, DrawerView, drawer};
use frust_beui::components::grouped_list::{grouped_list, list_row, section_header};
use frust_beui::components::theme_toggle::theme_toggle;

use crate::AppState;

/// The layout width, in logical px, below which the phone shell replaces the
/// rail. Every phone in portrait and a small phone in landscape fall below
/// it; a tablet and an ordinary desktop window keep the rail.
pub const COMPACT_BREAKPOINT: f64 = 720.0;

/// Whether a layout `width` gets the phone shell.
pub fn is_compact(width: f64) -> bool {
    width < COMPACT_BREAKPOINT
}

/// The phone shell's page padding, in logical px.
const PHONE_PAGE_PADDING: f64 = 16.0;

/// Every gallery page, flattened: `Home` and `Theming` stand alone, the rest
/// carry their section in [`Page::section`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Home,
    Content,
    MotionText,
    MotionButtons,
    MotionControls,
    MotionSelection,
    MotionOverlays,
    MotionNavigation,
    MotionSurfaces,
    MotionData,
    MotionShader,
    AgentsPrimitives,
    AgentsPanels,
    AgentsChat,
    BlocksCommand,
    BlocksMorph,
    BlocksForms,
    BlocksShowcase,
    Theming,
    GpuEffects,
}

impl Page {
    /// Every page, in nav order — the rail's item list, and the array the
    /// rail's `on_select` index looks back into.
    pub const ALL: [Page; 20] = [
        Page::Home,
        Page::Content,
        Page::MotionText,
        Page::MotionButtons,
        Page::MotionControls,
        Page::MotionSelection,
        Page::MotionOverlays,
        Page::MotionNavigation,
        Page::MotionSurfaces,
        Page::MotionData,
        Page::MotionShader,
        Page::AgentsPrimitives,
        Page::AgentsPanels,
        Page::AgentsChat,
        Page::BlocksCommand,
        Page::BlocksMorph,
        Page::BlocksForms,
        Page::BlocksShowcase,
        Page::Theming,
        Page::GpuEffects,
    ];

    /// Whether the phone shell gives this page its whole slot instead of a
    /// scroll view. The Chat page is one screen with its own transcript
    /// scroller: filling the slot is what pins its composer to the slot's
    /// bottom edge, which the shell's safe area holds above the soft keyboard.
    pub fn fills_phone_slot(self) -> bool {
        matches!(self, Page::AgentsChat)
    }

    /// The section this page belongs to, or `None` for the four pages that
    /// stand outside every section.
    pub fn section(self) -> Option<&'static str> {
        match self {
            Page::Home | Page::Content | Page::Theming | Page::GpuEffects => None,
            Page::MotionText
            | Page::MotionButtons
            | Page::MotionControls
            | Page::MotionSelection
            | Page::MotionOverlays
            | Page::MotionNavigation
            | Page::MotionSurfaces
            | Page::MotionData
            | Page::MotionShader => Some("Motion"),
            Page::AgentsPrimitives | Page::AgentsPanels | Page::AgentsChat => Some("Agents"),
            Page::BlocksCommand | Page::BlocksMorph | Page::BlocksForms | Page::BlocksShowcase => {
                Some("Blocks")
            }
        }
    }

    /// The page's own name, without its section prefix.
    pub fn label(self) -> &'static str {
        match self {
            Page::Home => "Home",
            Page::Content => "Content",
            Page::MotionText => "Text",
            Page::MotionButtons => "Buttons",
            Page::MotionControls => "Controls",
            Page::MotionSelection => "Selection",
            Page::MotionOverlays => "Overlays",
            Page::MotionNavigation => "Navigation",
            Page::MotionSurfaces => "Surfaces",
            Page::MotionData => "Data",
            Page::MotionShader => "Shader",
            Page::AgentsPrimitives => "Primitives",
            Page::AgentsPanels => "Panels",
            Page::AgentsChat => "Chat",
            Page::BlocksCommand => "Command",
            Page::BlocksMorph => "Morph",
            Page::BlocksForms => "Forms",
            Page::BlocksShowcase => "Showcase",
            Page::Theming => "Theming",
            Page::GpuEffects => "GPU Effects",
        }
    }

    /// The rail's label and the top bar's title: `"<section> \u{b7}
    /// <label>"` for a grouped page, just `label()` for the standalone
    /// ones.
    pub fn title(self) -> String {
        match self.section() {
            Some(section) => format!("{section} \u{b7} {}", self.label()),
            None => self.label().to_string(),
        }
    }

    /// The rail glyph for this page.
    fn icon(self) -> IconSource {
        match self {
            Page::Home => icons::HOME,
            Page::Content => icons::PERSON,
            Page::MotionText => icons::FORMAT_SIZE,
            Page::MotionButtons => icons::TAG,
            Page::MotionControls => icons::SETTINGS,
            Page::MotionSelection => icons::DONE_ALL,
            Page::MotionOverlays => icons::SCAN_MARK,
            Page::MotionNavigation => icons::ARROW_FORWARD,
            Page::MotionSurfaces => icons::PANE_MARK,
            Page::MotionData => icons::DESCRIPTION,
            Page::MotionShader => icons::IMAGE,
            Page::AgentsPrimitives => icons::STAR,
            Page::AgentsPanels => icons::GROUP,
            Page::AgentsChat => icons::FORUM,
            Page::BlocksCommand => icons::SEARCH,
            Page::BlocksMorph => icons::REFRESH,
            Page::BlocksForms => icons::EDIT,
            Page::BlocksShowcase => icons::CONTENT_COPY,
            Page::Theming => icons::LIGHT_MODE,
            Page::GpuEffects => icons::GLOBE,
        }
    }
}

/// The nav rail: [`animated_sidebar`], carrying every gallery page as one
/// springing, collapsible list. See the [module docs](self) for why it is
/// flat rather than grouped.
pub fn shell_sidebar(state: &AppState) -> AnimatedSidebarView<AppState> {
    let active = state.page;
    let items: Vec<SidebarItem<AppState>> = Page::ALL
        .iter()
        .map(|&page| {
            sidebar_item(icon(page.icon()).size(16.0), page.title()).active(page == active)
        })
        .collect();

    animated_sidebar(
        state.sidebar_open,
        items,
        |s: &mut AppState, index: usize| {
            if let Some(&page) = Page::ALL.get(index) {
                s.page = page;
            }
        },
    )
}

/// The inset's top bar: a rail-collapse trigger, the active page's title, and
/// a persistent [`theme_toggle`] — the scheme switch reachable from every
/// page, not just [`crate::pages::theming`]'s own copy.
pub fn top_bar(page: Page, open: bool) -> impl View<AppState> + use<> {
    let bar = row()
        .child(
            button(
                if open { "Collapse" } else { "Expand" },
                |s: &mut AppState| s.sidebar_open = !s.sidebar_open,
            )
            .tone(ButtonTone::Ghost)
            .size(ButtonSize::Sm),
        )
        .child(SizedBox(Some(12.0), None))
        .child(text(page.title()).size(16.0))
        .child(SizedBox(Some(12.0), None))
        .child(theme_toggle::<AppState>())
        .cross_axis(CrossAxisAlignment::Center);

    Padding(
        EdgeInsets {
            left: 16.0,
            top: 10.0,
            right: 16.0,
            bottom: 10.0,
        },
        bar,
    )
}

/// The gallery's ordinary page slot: 24px of padding inside a vertical
/// scroll view.
pub fn scroll_slot(content: impl View<AppState>) -> impl View<AppState> {
    scroll_view(Padding(EdgeInsets::all(24.0), content))
}

/// The inset's whole content: the top bar over the swapped page content,
/// stretched to the finite height the root row hands it.
pub fn shell_inset(page: Page, open: bool, content: impl View<AppState>) -> impl View<AppState> {
    column()
        .child(top_bar(page, open))
        .flex(1, content)
        .cross_axis(CrossAxisAlignment::Stretch)
}

/// The phone shell's page slot: [`scroll_slot`] with phone-width padding.
pub fn phone_scroll_slot(content: impl View<AppState>) -> impl View<AppState> {
    scroll_view(Padding(EdgeInsets::all(PHONE_PAGE_PADDING), content))
}

/// The phone shell's slot for a page that [fills it](Page::fills_phone_slot):
/// the same padding and no scroll view, so the page gets a finite height that
/// ends where the safe area ends.
pub fn phone_fill_slot(content: impl View<AppState>) -> impl View<AppState> {
    Padding(EdgeInsets::all(PHONE_PAGE_PADDING), content)
}

/// The phone shell's top bar: the menu button, the page title and the theme
/// toggle. Both controls are at least the touch minimum on every edge,
/// whatever the density, since a narrow desktop window shows this bar too.
pub fn phone_top_bar(page: Page) -> impl View<AppState> + use<> {
    let touch = BeuiMetrics::touch().min_touch_target;
    let menu = SizedBox(None, Some(touch))
        .child(button("Menu", |s: &mut AppState| s.menu_open.set(true)).tone(ButtonTone::Ghost));
    let bar = row()
        .child(menu)
        .child(SizedBox(Some(8.0), None))
        .flex(1, text(page.title()).size(16.0))
        .child(SizedBox(Some(8.0), None))
        .child(theme_toggle::<AppState>().size(touch))
        .cross_axis(CrossAxisAlignment::Center);
    Padding(
        EdgeInsets {
            left: 8.0,
            top: 4.0,
            right: 8.0,
            bottom: 4.0,
        },
        bar,
    )
}

/// Every page in nav order, cut into runs that share a section: the
/// standalone pages before and after the sections form their own runs.
fn page_groups() -> Vec<(Option<&'static str>, Vec<Page>)> {
    let mut groups: Vec<(Option<&'static str>, Vec<Page>)> = Vec::new();
    for page in Page::ALL {
        match groups.last_mut() {
            Some((section, pages)) if *section == page.section() => pages.push(page),
            _ => groups.push((page.section(), vec![page])),
        }
    }
    groups
}

/// The drawer's content: every page as a pressable list row, grouped under
/// its section's header. Choosing a row shows that page and closes the list.
pub fn page_list(active: Page) -> impl View<AppState> + use<> {
    let groups = page_groups().into_iter().map(|(section, pages)| {
        let rows = pages.into_iter().map(move |page| {
            let row = list_row(page.label())
                .leading(icon(page.icon()).size(18.0))
                .on_press(move |s: &mut AppState| {
                    s.page = page;
                    s.menu_open.set(false);
                });
            if page == active {
                row.value("Showing")
            } else {
                row
            }
        });
        column()
            .when_some(section, |c, label| {
                c.child(section_header(label))
                    .child(SizedBox(None, Some(6.0)))
            })
            .child(grouped_list(rows.collect::<Vec<_>>()))
            .child(SizedBox(None, Some(16.0)))
            .cross_axis(CrossAxisAlignment::Stretch)
    });
    let list = column()
        .children(groups.collect::<Vec<_>>())
        .cross_axis(CrossAxisAlignment::Stretch);
    let body = column()
        .child(Padding(
            EdgeInsets {
                left: 16.0,
                top: 16.0,
                right: 16.0,
                bottom: 8.0,
            },
            text("Pages").size(16.0),
        ))
        .flex(
            1,
            scroll_view(Padding(
                EdgeInsets {
                    left: 12.0,
                    top: 0.0,
                    right: 12.0,
                    bottom: 12.0,
                },
                list,
            )),
        )
        .cross_axis(CrossAxisAlignment::Stretch);
    // The panel runs the window's full height from its left edge.
    safe_area(body).right(false)
}

/// The page list in a left [`drawer`], open while `open`. A backdrop tap and
/// Escape close it through the drawer; the system back press closes it
/// through the app's back fallback (see `lib.rs`), because a drawer mounted
/// in a stack, rather than pushed as a navigator page, never sees one itself.
pub fn page_menu(active: Page, open: bool) -> DrawerView<AppState> {
    drawer(page_list(active))
        .side(DrawerSide::Left)
        .label("Pages")
        .open(open)
        .on_open_change(|s: &mut AppState, open| s.menu_open.set(open))
}

/// The phone shell: the top bar over the page, inside a safe area that also
/// holds the bottom edge above the soft keyboard, with the page list's drawer
/// over the whole window.
pub fn phone_shell(
    page: Page,
    menu_open: bool,
    content: impl View<AppState>,
) -> impl View<AppState> {
    let screen = column()
        .child(phone_top_bar(page))
        .flex(1, content)
        .cross_axis(CrossAxisAlignment::Stretch);
    stack()
        .child(safe_area(screen).keyboard(true))
        .child(page_menu(page, menu_open))
}

/// A section heading used atop every gallery page.
pub fn heading(title: &str) -> impl View<AppState> + use<> {
    text(title.to_string()).size(24.0)
}

/// A page's small print — the caption style every gallery page explains
/// itself in.
pub fn caption(body: impl Into<String>) -> frust::TextView {
    text(body.into()).size(12.0)
}
