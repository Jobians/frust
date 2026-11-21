//! App chrome's platform-free half (decision C1): the [`ChromeSpec`] data
//! contract every arm consumes, the [`ChromeEvent`] vocabulary that comes
//! back, and the process-wide publish slot between them.
//!
//! # A host-level surface, not a `platform_view` slot
//!
//! Tabs, title, leading item, actions and the bottom toolbar belong to the
//! host (iOS `UITabBarController` and `UINavigationController`, Android
//! `MaterialToolbar`, `BottomNavigationView` and floating action buttons), not
//! to the differ, so one `native_scaffold` per app publishes a spec here and an
//! arm behind [`ChromeHost`] owns the real containers. The bare `native_tab_bar` control is unchanged (D5): it
//! stays an embedded `platform_view` slot.
//!
//! # The slot
//!
//! - **Diff-gated.** The first [`publish`] installs; later ones diff against
//!   the retained spec ([`ChromeDiff`]) and apply only the changed fields; an
//!   identical spec makes no host call.
//! - **One publisher.** Only a generation-less publish installs, under a fresh, never-zero,
//!   increasing [`ChromeGeneration`]; naming another generation while one is live is refused
//!   [`ChromeError::Busy`], a withdrawn one on an empty slot is [`PublishOutcome::Stale`].
//!   [`withdraw`] (scaffold teardown or drop) frees the slot only for its own generation.
//! - **Controlled (Android and the fallback), converging (iOS).** [`deliver`]
//!   decodes a `(kind, detail)` report against the live spec into a
//!   [`ChromeEvent`] for the registered sink, and the app confirms by
//!   publishing again. Android and the fallback bars never navigate on a tap;
//!   iOS selects the tapped tab natively and converges to the app's spec
//!   within a ~250 ms settle (`apple::chrome`'s *Selection*).
//!   Reports stamped with a withdrawn generation are dropped, and so is a tap
//!   on a disabled tab or menu item.
//! - **One action index space.** An action report names its item by index
//!   into [`chrome_actions`]: the top bar's actions, then the toolbar's items,
//!   then its primary. So a toolbar change is diffed as
//!   [`ChromeField::Actions`] ([`diff_specs`]): the two always travel together.
//!   A change of badges alone, in either, is [`ChromeField::ActionBadges`],
//!   which an arm applies to the shown items in place.
//! - **Resolved where it was drawn.** An arm whose bars lag the slot (iOS
//!   applies on a later main-queue turn) resolves a tap against the spec it
//!   shows ([`resolve_tap`]) and hands the id-based event to [`deliver_event`],
//!   which re-encodes it against the live spec, so a reorder in between cannot
//!   retarget it and an item that is gone or disabled by then is dropped.
//! - **The search field echoes nothing.** The app owns the search text and
//!   whether the field is expanded ([`SearchSpec`]), but the field changes
//!   them itself as the user types, so the slot keeps what the field holds
//!   (`FieldState`): an arm's search report updates it ([`deliver_search`]),
//!   a report repeating it is dropped, and a publish sends
//!   [`ChromeField::SearchText`]/[`ChromeField::SearchActive`] only for a
//!   value the app changed that the field does not already hold — an app
//!   writing the typed text back is never applied, so it cannot move the
//!   caret or break an IME composition. A collapse empties the field.
//! - **Failures reported late.** An arm that installs on a later turn reports
//!   a failure it finds there through [`ChromeHost::take_failure`]; the slot
//!   takes it at the top of the generation's next publish (or withdraw),
//!   answers that publish [`PublishOutcome::Fallback`] and, for
//!   [`ChromeError::NoHost`], retries the install on the one after.
//!
//! # The epoch
//!
//! An outcome change off the scaffold's build path (a failure an arm records
//! on the main queue, a scroll flush whose publish crosses between native and
//! fallback) calls [`bump_epoch`] outside the slot lock: it counts the change
//! and owes the shell a frame, whose rebuild re-runs the scaffold's sync so
//! its fallback bars and `native` flag follow. It is not a signal: every
//! frame that runs rebuilds the whole tree, so the frame is all that is owed,
//! and a signal the plugin created inside a build would die with that
//! component's owner.
//!
//! The module is behind `frust-api` because the spec reuses the api layer's
//! [`TabItem`]/[`TabIcon`]/[`TabId`]; it compiles on every target. iOS
//! installs `crate::apple::chrome`'s host and Android `crate::android::chrome`'s
//! (over the [`android_wire`] encoding); every other target uses the inert
//! `NullChromeHost`, and tests use `RecordingChromeHost`.

/// The frust-drawn bars the scaffold shows from the same spec wherever no arm
/// owns the chrome (see [`NATIVE_CHROME_ARM`]).
pub(crate) mod fallback;
pub(crate) mod host;

#[cfg(test)]
pub(crate) use host::RecordingChromeHost;
pub(crate) use host::{ChromeDiff, ChromeError, ChromeHost};

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Once};

use crate::api::{TabIcon, TabId, TabItem};
use crate::events::{
    EVENT_KIND_CHROME_ACTION, EVENT_KIND_CHROME_BACK, EVENT_KIND_CHROME_MENU,
    EVENT_KIND_CHROME_SEARCH_BEGAN, EVENT_KIND_CHROME_SEARCH_CHANGED,
    EVENT_KIND_CHROME_SEARCH_ENDED, EVENT_KIND_CHROME_SEARCH_SUBMITTED, EVENT_KIND_CHROME_TAB,
    pack_chrome_menu, pack_chrome_tab, pack_index, unpack_chrome_menu, unpack_chrome_tab,
    unpack_index,
};
use host::ChromeField;

/// Whether this target has a native chrome arm: iOS and Android, so every
/// other target's scaffold draws the [`fallback`] bars. A target with an arm
/// still draws the fallback for a [`PublishOutcome::Fallback`] (its install
/// was refused — on Android, a theme that is not Material-derived).
pub(crate) const NATIVE_CHROME_ARM: bool = cfg!(any(target_os = "ios", target_os = "android"));

#[cfg(test)]
thread_local! {
    /// [`native_arm`]'s per-test-thread override.
    static TEST_ARM: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// [`NATIVE_CHROME_ARM`], which a test may override on its own thread
/// ([`ArmOverride`]) to drive the arm's outcome classes on a host build.
pub(crate) fn native_arm() -> bool {
    #[cfg(test)]
    if let Some(arm) = TEST_ARM.with(std::cell::Cell::get) {
        return arm;
    }
    NATIVE_CHROME_ARM
}

/// Test-only: [`native_arm`] answers `arm` on this thread until dropped.
#[cfg(test)]
pub(crate) struct ArmOverride;

#[cfg(test)]
impl ArmOverride {
    pub(crate) fn new(arm: bool) -> Self {
        TEST_ARM.with(|cell| cell.set(Some(arm)));
        Self
    }
}

#[cfg(test)]
impl Drop for ArmOverride {
    fn drop(&mut self) {
        TEST_ARM.with(|cell| cell.set(None));
    }
}

static EPOCH: AtomicU64 = AtomicU64::new(0);

/// Record an outcome change made off the scaffold's build path and owe the
/// shell a frame, whose rebuild re-syncs the scaffold (module doc, "The
/// epoch"). Callable from any thread; never with the slot locked.
pub(crate) fn bump_epoch() {
    EPOCH.fetch_add(1, Ordering::AcqRel);
    frust::texture::request_frame();
}

/// How many times [`bump_epoch`] ran.
#[cfg(test)]
pub(crate) fn epoch() -> u64 {
    EPOCH.load(Ordering::Acquire)
}

// --- the data contract ------------------------------------------------------

/// Everything the app chrome shows. Byte payloads compare by content, so a
/// spec rebuilt every frame from `include_bytes!` diffs as unchanged.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ChromeSpec {
    pub(crate) tabs: Vec<TabItem>,
    /// The app-confirmed tab; an id no tab carries shows no selection.
    pub(crate) selected: Option<TabId>,
    pub(crate) top: Option<TopBarSpec>,
    /// The bottom toolbar; `None` or an empty one shows none.
    pub(crate) toolbar: Option<ToolbarSpec>,
    pub(crate) scroll: Option<ScrollState>,
    /// The app theme's brightness (`crate::api::theme::is_dark`), which an
    /// arm styles its bars with; the fallback bars read the theme directly.
    pub(crate) dark: bool,
    /// The search item; `None` shows none.
    pub(crate) search: Option<SearchSpec>,
}

/// The scaffold's search item: a field that reports
/// [`ChromeEvent::Search`] as it is typed into, submitted, expanded and
/// collapsed — iOS's `UISearchController` on the navigation item, a
/// search action that expands into a field in the Android top bar, a
/// frust text field row under the frust-drawn top bar. The results are the
/// app's to show, in the body.
///
/// Controlled, but echo-free: [`Self::text`] and [`Self::active`] are the
/// app's values, applied to the field only when the app changes them and
/// the field does not already hold them, so writing the typed text back
/// (the usual pattern) never touches the field.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchSpec {
    pub(crate) placeholder: String,
    pub(crate) text: String,
    pub(crate) active: bool,
    pub(crate) placement: SearchPlacement,
}

impl SearchSpec {
    /// A collapsed, empty search item with no placeholder.
    pub fn new() -> Self {
        Self::default()
    }

    /// The hint the empty field shows (and the item's accessibility label).
    pub fn placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// The app's query: the field's text. Applied only when it changes and
    /// differs from what the field holds (the type doc). Default empty.
    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.text = text.into();
        self
    }

    /// Whether the field is expanded and focused, keyboard up. Default
    /// `false`. Applied like [`Self::text`]: confirm a
    /// [`SearchEvent::Began`]/[`SearchEvent::Ended`] by writing it back, and
    /// flip it to open or close the field from the app. Collapsing empties
    /// the field.
    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    /// Where the field sits, a hint the platform may not honour. Default
    /// [`SearchPlacement::Automatic`].
    pub fn placement(mut self, placement: SearchPlacement) -> Self {
        self.placement = placement;
        self
    }
}

/// Where the search field sits ([`SearchSpec::placement`]). iOS maps it onto
/// the navigation item's search-bar placement (iOS 16+); Android and the
/// frust-drawn chrome have one presentation and ignore it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SearchPlacement {
    /// The platform's choice: on iOS the field below the title, revealed
    /// with the large title.
    #[default]
    Automatic,
    /// Inside the navigation bar itself: iOS's integrated placement.
    NavigationBar,
}

/// One search-field interaction ([`ChromeEvent::Search`]).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SearchEvent {
    /// The field's whole text, after an edit (IME composing text included).
    Changed(String),
    /// The keyboard's search key, with the field's text.
    Submitted(String),
    /// The field expanded (the user opened search).
    Began,
    /// The field collapsed (cancel or back); a non-empty field reports
    /// `Changed("")` first.
    Ended,
}

/// The top bar: title, optional subtitle, leading item and trailing actions.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct TopBarSpec {
    pub(crate) title: String,
    pub(crate) subtitle: Option<String>,
    pub(crate) leading: Leading,
    pub(crate) actions: Vec<ActionItem>,
    /// Large-title display: the iOS large title, a collapsing medium top bar on
    /// Android; ignored where the platform has none.
    pub(crate) large_title: bool,
}

/// The top bar's leading item.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Leading {
    /// Nothing.
    #[default]
    None,
    /// A back item, reported as [`ChromeEvent::BackPressed`].
    Back,
    /// Encoded image bytes (an avatar), compared by content.
    Image(Arc<[u8]>),
    /// An Apple SF Symbol name; Android has no symbol-by-name, so it shows
    /// nothing there — [`Self::Icon`] with a [`TabIcon::AppleSymbolWithBytes`]
    /// shows on both phones.
    Symbol(String),
    /// A plain (not round, not tappable) icon: an SF Symbol where the
    /// platform has them, encoded image bytes elsewhere, compared by content.
    /// `Icon(TabIcon::AppleSymbol(name))` shows like [`Self::Symbol`].
    Icon(TabIcon),
}

/// A chrome action's stable, app-chosen identity — never an index.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ActionId(String);

/// A pull-down menu item's stable, app-chosen identity — never an index.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MenuItemId(String);

macro_rules! string_id {
    ($id:ident) => {
        impl $id {
            /// An id spelled `id`.
            pub fn new(id: impl Into<String>) -> Self {
                Self(id.into())
            }

            /// The id's spelling.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl From<&str> for $id {
            fn from(id: &str) -> Self {
                Self::new(id)
            }
        }

        impl From<String> for $id {
            fn from(id: String) -> Self {
                Self(id)
            }
        }

        impl fmt::Display for $id {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

string_id!(ActionId);
string_id!(MenuItemId);

/// One trailing top-bar action; with a [`Self::menu`] it is a pull-down
/// whose choice reports [`ChromeEvent::MenuItemChosen`] instead of a tap.
#[derive(Clone, Debug, PartialEq)]
pub struct ActionItem {
    pub(crate) id: ActionId,
    pub(crate) title: String,
    pub(crate) icon: Option<TabIcon>,
    pub(crate) menu: Option<Vec<MenuItem>>,
    pub(crate) badge: Option<String>,
    pub(crate) prominent: bool,
}

impl ActionItem {
    /// An action `id` titled `title`, with no icon, no menu, no badge and
    /// the plain style.
    pub fn new(id: impl Into<ActionId>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            icon: None,
            menu: None,
            badge: None,
            prominent: false,
        }
    }

    /// The action's icon; the title then becomes its accessibility label.
    pub fn icon(mut self, icon: TabIcon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Make this action a pull-down over `items`.
    pub fn menu(mut self, items: Vec<MenuItem>) -> Self {
        self.menu = Some(items);
        self
    }

    /// A badge on the action, with [`TabItem::badge`]'s grammar: a count,
    /// `"new"`, or `""` for a bare dot. Default: none. A change of badge
    /// alone updates the shown item in place rather than rebuilding the bar.
    /// iOS shows it on top-bar actions where `UIBarButtonItem` has a badge
    /// (iOS 26; UIKit supports it in navigation bars only), Android as a
    /// Material badge, the frust-drawn chrome as a dot or a label.
    pub fn badge(mut self, badge: impl Into<String>) -> Self {
        self.badge = Some(badge.into());
        self
    }

    /// Whether the action is emphasised: iOS's prominent bar-item style,
    /// the theme's primary colour on Android and in the frust-drawn chrome.
    /// Default `false`.
    pub fn prominent(mut self, prominent: bool) -> Self {
        self.prominent = prominent;
        self
    }

    /// This action's id.
    pub fn id(&self) -> &ActionId {
        &self.id
    }

    /// Whether `self` and `other` differ in nothing but their badges.
    pub(crate) fn same_but_badge(&self, other: &Self) -> bool {
        self.id == other.id
            && self.title == other.title
            && self.icon == other.icon
            && self.menu == other.menu
            && self.prominent == other.prominent
    }
}

/// How two lists of actions differ: not at all, only in their badges, or
/// otherwise (an item added, removed, moved or changed beyond its badge).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActionsChange {
    None,
    Badges,
    Items,
}

impl ActionsChange {
    /// Classify the change from `old` to `new`.
    pub(crate) fn between<'a>(
        old: impl ExactSizeIterator<Item = &'a ActionItem>,
        new: impl ExactSizeIterator<Item = &'a ActionItem>,
    ) -> Self {
        if old.len() != new.len() {
            return Self::Items;
        }
        let mut change = Self::None;
        for (old, new) in old.zip(new) {
            if !old.same_but_badge(new) {
                return Self::Items;
            }
            if old.badge != new.badge {
                change = Self::Badges;
            }
        }
        change
    }
}

/// One item of an [`ActionItem`]'s pull-down menu.
#[derive(Clone, Debug, PartialEq)]
pub struct MenuItem {
    pub(crate) id: MenuItemId,
    pub(crate) title: String,
    pub(crate) icon: Option<TabIcon>,
    pub(crate) destructive: bool,
    pub(crate) enabled: bool,
}

impl MenuItem {
    /// An enabled, non-destructive item `id` titled `title`.
    pub fn new(id: impl Into<MenuItemId>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            icon: None,
            destructive: false,
            enabled: true,
        }
    }

    /// The item's icon.
    pub fn icon(mut self, icon: TabIcon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Whether the item is styled destructive. Default `false`.
    pub fn destructive(mut self, destructive: bool) -> Self {
        self.destructive = destructive;
        self
    }

    /// Whether the item can be chosen. Default `true`.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// This item's id.
    pub fn id(&self) -> &MenuItemId {
        &self.id
    }
}

/// The bottom toolbar: secondary `items` and an optional `primary` action,
/// shown trailing and emphasised — iOS navigation-controller toolbar items,
/// Android floating action buttons (or, for [`ToolbarStyle::Floating`], a
/// Material floating toolbar beside the primary), a frust-drawn row in the
/// fallback. Its
/// actions report like the top bar's ([`ChromeEvent::ActionTapped`],
/// [`ChromeEvent::MenuItemChosen`]), so their ids must not repeat a top-bar
/// action's or each other's: the scaffold drops a repeat and says so.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolbarSpec {
    pub(crate) items: Vec<ActionItem>,
    pub(crate) primary: Option<ActionItem>,
    pub(crate) style: ToolbarStyle,
    pub(crate) primary_label: bool,
}

/// How the bottom toolbar's items are presented ([`ToolbarSpec::style`]).
/// iOS ignores it: its toolbar is the navigation controller's own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ToolbarStyle {
    /// Each entry its own button: on Android a column of floating action
    /// buttons above the bottom navigation, trailing; a trailing row of
    /// buttons in the frust-drawn chrome.
    #[default]
    Buttons,
    /// The items together in one floating pill, the primary beside it: on
    /// Android a Material 3 floating toolbar anchored bottom-centre with the
    /// primary as a floating action button, hidden while the keyboard shows;
    /// one pill of items in the frust-drawn chrome.
    Floating,
}

impl ToolbarSpec {
    /// An empty toolbar, which shows nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// How the items are presented. Default [`ToolbarStyle::Buttons`].
    pub fn style(mut self, style: ToolbarStyle) -> Self {
        self.style = style;
        self
    }

    /// Whether a primary with an icon also shows its title beside the icon
    /// (Android's extended floating action button). Default `false`: an
    /// icon primary shows its icon alone, its title the accessibility label.
    /// A primary without an icon always shows its title. iOS and the
    /// frust-drawn chrome ignore it.
    pub fn primary_label(mut self, label: bool) -> Self {
        self.primary_label = label;
        self
    }

    /// The secondary items, leading-first.
    pub fn items(mut self, items: Vec<ActionItem>) -> Self {
        self.items = items;
        self
    }

    /// Append one secondary item.
    pub fn item(mut self, item: ActionItem) -> Self {
        self.items.push(item);
        self
    }

    /// The primary action, trailing and emphasised (the floating button).
    pub fn primary(mut self, primary: ActionItem) -> Self {
        self.primary = Some(primary);
        self
    }

    /// Whether it has no item and no primary.
    pub(crate) fn is_empty(&self) -> bool {
        self.items.is_empty() && self.primary.is_none()
    }

    /// The items, then the primary: the toolbar's report order.
    pub(crate) fn entries(&self) -> impl Iterator<Item = &ActionItem> {
        self.items.iter().chain(self.primary.as_ref())
    }

    /// [`Self::entries`], collected so its length is known.
    fn entries_exact(&self) -> std::vec::IntoIter<&ActionItem> {
        self.entries().collect::<Vec<_>>().into_iter()
    }
}

/// The toolbar `spec` shows: none for an absent or empty one.
pub(crate) fn shown_toolbar(spec: &ChromeSpec) -> Option<&ToolbarSpec> {
    spec.toolbar.as_ref().filter(|toolbar| !toolbar.is_empty())
}

/// Every action `spec` shows, in report-index order: the top bar's actions,
/// then the toolbar's items, then its primary.
pub(crate) fn chrome_actions(spec: &ChromeSpec) -> impl Iterator<Item = &ActionItem> {
    let top = spec.top.as_ref().map_or(&[][..], |top| &top.actions[..]);
    top.iter()
        .chain(spec.toolbar.iter().flat_map(ToolbarSpec::entries))
}

/// The report index of the toolbar's first entry: the top bar's action count.
#[cfg_attr(not(any(test, target_os = "ios")), allow(dead_code))]
pub(crate) fn toolbar_offset(spec: &ChromeSpec) -> usize {
    spec.top.as_ref().map_or(0, |top| top.actions.len())
}

/// A field's position in [`ChromeField`]'s declaration order; exhaustive, so
/// a new field must be placed here too.
fn declaration_rank(field: ChromeField) -> u8 {
    match field {
        ChromeField::Tabs => 0,
        ChromeField::Selected => 1,
        ChromeField::TopBar => 2,
        ChromeField::Title => 3,
        ChromeField::Subtitle => 4,
        ChromeField::Leading => 5,
        ChromeField::Actions => 6,
        ChromeField::LargeTitle => 7,
        ChromeField::Scroll => 8,
        ChromeField::Brightness => 9,
        ChromeField::ActionBadges => 10,
        ChromeField::Search => 11,
        ChromeField::SearchText => 12,
        ChromeField::SearchActive => 13,
    }
}

/// How the toolbar changed from `old` to `new`: a badge-only change keeps
/// both present with the same entries in the same roles, the same style and
/// the same primary label.
fn toolbar_change(old: Option<&ToolbarSpec>, new: Option<&ToolbarSpec>) -> ActionsChange {
    match (old, new) {
        _ if old == new => ActionsChange::None,
        (Some(old), Some(new))
            if old.primary.is_some() == new.primary.is_some()
                && old.style == new.style
                && old.primary_label == new.primary_label =>
        {
            ActionsChange::between(old.entries_exact(), new.entries_exact())
        }
        _ => ActionsChange::Items,
    }
}

/// [`ChromeDiff::between`], with a toolbar change reported as
/// [`ChromeField::Actions`] (module doc, "One action index space") — or as
/// [`ChromeField::ActionBadges`] when the toolbar, like the top bar's
/// actions, changed in its badges alone.
pub(crate) fn diff_specs<'a>(old: &ChromeSpec, new: &'a ChromeSpec) -> ChromeDiff<'a> {
    let mut diff = ChromeDiff::between(old, new);
    let field = match toolbar_change(old.toolbar.as_ref(), new.toolbar.as_ref()) {
        ActionsChange::None => None,
        ActionsChange::Badges => Some(ChromeField::ActionBadges),
        ActionsChange::Items => Some(ChromeField::Actions),
    };
    if let Some(field) = field
        && !diff.contains(field)
    {
        diff.changed.push(field);
    }
    if diff.contains(ChromeField::Actions) {
        // A rebuild carries every badge.
        diff.changed
            .retain(|field| *field != ChromeField::ActionBadges);
    }
    // Restore `ChromeField` declaration order (the `ChromeDiff::changed` contract).
    diff.changed.sort_by_key(|field| declaration_rank(*field));
    diff
}

/// Whether the host's bars float over the body (iOS reports them through the
/// window insets); Android's arm shrinks the surface between its bars and the
/// fallback bars are laid out beside the body, so neither floats.
#[cfg_attr(not(feature = "frust-api"), allow(dead_code))]
pub(crate) const BARS_FLOAT_OVER_BODY: bool = cfg!(target_os = "ios");

/// The body's scroll position, mirrored to the host so the bars can react
/// (scroll-edge appearance, lifted app bar). Logical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScrollState {
    /// How far the body is scrolled from its top.
    pub offset: f64,
    /// The largest reachable offset.
    pub max_offset: f64,
    /// The visible extent along the scroll axis.
    pub viewport: f64,
}

// --- the event vocabulary ---------------------------------------------------

/// One chrome interaction, decoded against the spec that was live when it
/// arrived. Requests, not facts: the app routes and republishes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChromeEvent {
    /// A tab other than the showing one was tapped.
    TabSelected(TabId),
    /// The showing tab was tapped again ("scroll to top / pop to root").
    TabReselected(TabId),
    /// A plain action was tapped.
    ActionTapped(ActionId),
    /// An item of an action's pull-down menu was chosen.
    MenuItemChosen(ActionId, MenuItemId),
    /// The leading back item was tapped.
    BackPressed,
    /// The search item reported (`SearchSpec`).
    Search(SearchEvent),
}

/// A search report's `kind` and `text` as the [`SearchEvent`] it names (the
/// text read only for a change or a submit); `None` for any other kind.
pub(crate) fn decode_search(kind: i32, text: &str) -> Option<SearchEvent> {
    match kind {
        EVENT_KIND_CHROME_SEARCH_CHANGED => Some(SearchEvent::Changed(text.to_owned())),
        EVENT_KIND_CHROME_SEARCH_SUBMITTED => Some(SearchEvent::Submitted(text.to_owned())),
        EVENT_KIND_CHROME_SEARCH_BEGAN => Some(SearchEvent::Began),
        EVENT_KIND_CHROME_SEARCH_ENDED => Some(SearchEvent::Ended),
        _ => None,
    }
}

/// [`decode_search`]'s inverse: `event`'s kind and the text beside it (`""`
/// for a payload-free event).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn encode_search(event: &SearchEvent) -> (i32, &str) {
    match event {
        SearchEvent::Changed(text) => (EVENT_KIND_CHROME_SEARCH_CHANGED, text),
        SearchEvent::Submitted(text) => (EVENT_KIND_CHROME_SEARCH_SUBMITTED, text),
        SearchEvent::Began => (EVENT_KIND_CHROME_SEARCH_BEGAN, ""),
        SearchEvent::Ended => (EVENT_KIND_CHROME_SEARCH_ENDED, ""),
    }
}

/// `event` as the `(kind, detail)` an arm reports, resolving ids to the
/// first item carrying them in `spec`. `None` when `spec` has no such item,
/// and for a search event, whose text no `detail` carries
/// ([`encode_search`]).
pub(crate) fn encode_event(spec: &ChromeSpec, event: &ChromeEvent) -> Option<(i32, i64)> {
    let tab_index = |id: &TabId| spec.tabs.iter().position(|tab| tab.id() == id);
    let action_index = |id: &ActionId| chrome_actions(spec).position(|action| &action.id == id);
    match event {
        ChromeEvent::TabSelected(id) => Some((
            EVENT_KIND_CHROME_TAB,
            pack_chrome_tab(tab_index(id)?, false)?,
        )),
        ChromeEvent::TabReselected(id) => Some((
            EVENT_KIND_CHROME_TAB,
            pack_chrome_tab(tab_index(id)?, true)?,
        )),
        ChromeEvent::ActionTapped(id) => Some((
            EVENT_KIND_CHROME_ACTION,
            pack_index(action_index(id)? as isize),
        )),
        ChromeEvent::MenuItemChosen(action, item) => {
            let a = action_index(action)?;
            let menu = chrome_actions(spec).nth(a)?.menu.as_deref()?;
            let m = menu.iter().position(|entry| &entry.id == item)?;
            Some((EVENT_KIND_CHROME_MENU, pack_chrome_menu(a, m)?))
        }
        ChromeEvent::BackPressed => Some((EVENT_KIND_CHROME_BACK, 0)),
        ChromeEvent::Search(_) => None,
    }
}

/// [`encode_event`]'s inverse: `None` for an unknown kind, a malformed
/// `detail` or an index `spec` does not have.
pub(crate) fn decode_event(spec: &ChromeSpec, kind: i32, detail: i64) -> Option<ChromeEvent> {
    match kind {
        EVENT_KIND_CHROME_TAB => {
            let (index, reselect) = unpack_chrome_tab(detail)?;
            let tab = spec.tabs.get(index).filter(|tab| tab.is_enabled())?;
            let id = tab.id().clone();
            Some(if reselect {
                ChromeEvent::TabReselected(id)
            } else {
                ChromeEvent::TabSelected(id)
            })
        }
        EVENT_KIND_CHROME_ACTION => {
            let action = chrome_actions(spec).nth(unpack_index(detail)?)?;
            Some(ChromeEvent::ActionTapped(action.id.clone()))
        }
        EVENT_KIND_CHROME_MENU => {
            let (a, m) = unpack_chrome_menu(detail)?;
            let action = chrome_actions(spec).nth(a)?;
            let item = action.menu.as_ref()?.get(m).filter(|item| item.enabled)?;
            Some(ChromeEvent::MenuItemChosen(
                action.id.clone(),
                item.id.clone(),
            ))
        }
        EVENT_KIND_CHROME_BACK => (detail == 0).then_some(ChromeEvent::BackPressed),
        _ => None,
    }
}

/// One tap on an arm's on-screen chrome, by position in the spec it was
/// built from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(any(test, target_os = "ios")), allow(dead_code))]
pub(crate) enum ChromeTap {
    /// The tab at this index of `spec.tabs`.
    Tab(usize),
    /// The action at this [`chrome_actions`] index.
    Action(usize),
    /// Item `.1` of action `.0`'s pull-down menu.
    Menu(usize, usize),
    /// The leading back item.
    Back,
}

/// What `tap` requests, against the `spec` the tapped bar was built from: a
/// tab tap is a reselect when it lands on the app-confirmed `selected` tab
/// and a selection otherwise. `None` when `spec` has no such item, or it is
/// disabled, or the back tap meets no back item.
#[cfg_attr(not(any(test, target_os = "ios")), allow(dead_code))]
pub(crate) fn resolve_tap(spec: &ChromeSpec, tap: ChromeTap) -> Option<ChromeEvent> {
    let (kind, detail) = match tap {
        ChromeTap::Tab(index) => {
            let reselect = spec.selected.as_ref() == Some(spec.tabs.get(index)?.id());
            (EVENT_KIND_CHROME_TAB, pack_chrome_tab(index, reselect)?)
        }
        ChromeTap::Action(index) => (
            EVENT_KIND_CHROME_ACTION,
            pack_index(isize::try_from(index).ok()?),
        ),
        ChromeTap::Menu(action, item) => (EVENT_KIND_CHROME_MENU, pack_chrome_menu(action, item)?),
        ChromeTap::Back => {
            let top = spec.top.as_ref()?;
            (top.leading == Leading::Back).then_some((EVENT_KIND_CHROME_BACK, 0))?
        }
    };
    decode_event(spec, kind, detail)
}

// --- the slot ---------------------------------------------------------------

/// One install of the app chrome; never zero, increasing per slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct ChromeGeneration(u64);

impl ChromeGeneration {
    /// The raw value an arm stamps on its reports.
    pub(crate) fn get(self) -> u64 {
        self.0
    }
}

/// Where decoded events go — set by the scaffold, called outside the slot
/// lock so it may publish again.
pub(crate) type ChromeSink = Arc<dyn Fn(ChromeEvent) + Send + Sync>;

/// What one [`publish`] did.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PublishOutcome {
    /// A fresh generation claimed the slot and the host installed it.
    Installed(ChromeGeneration),
    /// Changed fields were applied to the host.
    Applied(ChromeGeneration),
    /// The spec was identical; no host call.
    Unchanged(ChromeGeneration),
    /// The generation holds the slot but its host refused the install, so
    /// the app renders fallback chrome; no host call for this generation.
    Fallback(ChromeGeneration, ChromeError),
    /// Another generation holds the slot ([`ChromeError::Busy`]).
    Refused(ChromeError),
    /// The publisher's generation was withdrawn and the slot is empty; nothing
    /// was installed. Only a publisher holding no generation may claim it.
    Stale,
}

impl PublishOutcome {
    /// The generation that holds the slot for this publisher, if any.
    pub(crate) fn generation(&self) -> Option<ChromeGeneration> {
        match self {
            Self::Installed(generation)
            | Self::Applied(generation)
            | Self::Unchanged(generation)
            | Self::Fallback(generation, _) => Some(*generation),
            Self::Refused(_) | Self::Stale => None,
        }
    }
}

struct Live {
    generation: ChromeGeneration,
    spec: ChromeSpec,
    /// `Err` when the host refused the install.
    installed: Result<(), ChromeError>,
    sink: Option<ChromeSink>,
    busy_logged: bool,
    /// What the search field holds (module doc, "The search field echoes
    /// nothing").
    field: FieldState,
}

/// What the search field holds, as its reports and the slot's own applies
/// left it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FieldState {
    pub(crate) text: String,
    pub(crate) active: bool,
}

impl FieldState {
    /// The field a fresh install of `search` builds.
    fn of(search: Option<&SearchSpec>) -> Self {
        search.map_or_else(Self::default, |search| Self {
            text: search.text.clone(),
            active: search.active,
        })
    }

    /// Adopt what `event` says the field now holds; `false` when it says
    /// nothing new (a repeated text, a second begin or end), which is then
    /// not delivered. A submit always is.
    fn adopt(&mut self, event: &SearchEvent) -> bool {
        match event {
            SearchEvent::Changed(text) if *text == self.text => false,
            SearchEvent::Changed(text) => {
                self.text.clone_from(text);
                true
            }
            SearchEvent::Submitted(text) => {
                self.text.clone_from(text);
                true
            }
            SearchEvent::Began if self.active => false,
            SearchEvent::Began => {
                self.active = true;
                true
            }
            SearchEvent::Ended if !self.active => false,
            SearchEvent::Ended => {
                self.active = false;
                true
            }
        }
    }
}

/// Bring `field` up to what the app changed from `old` to `new`, answering
/// whether the text and the active state must reach the field: only a value
/// the app changed that the field does not already hold, so an app writing
/// back what the field reported sends nothing. A search item appearing or
/// going away starts the field fresh (empty, collapsed); collapsing it
/// empties it.
pub(crate) fn track_search(
    field: &mut FieldState,
    old: Option<&SearchSpec>,
    new: Option<&SearchSpec>,
) -> (bool, bool) {
    let blank = SearchSpec::default();
    let Some(new) = new else {
        *field = FieldState::default();
        return (false, false);
    };
    let old = match old {
        Some(old) => old,
        None => {
            *field = FieldState::default();
            &blank
        }
    };
    let mut active = false;
    if new.active != old.active && new.active != field.active {
        field.active = new.active;
        if !new.active {
            field.text.clear();
        }
        active = true;
    }
    let mut text = false;
    if new.text != old.text && new.text != field.text {
        field.text.clone_from(&new.text);
        text = true;
    }
    (text, active)
}

/// The slot over one host. The process-wide instance is behind [`publish`]
/// and friends; tests build their own over a [`RecordingChromeHost`].
pub(crate) struct ChromeSlot<H> {
    host: H,
    live: Option<Live>,
    last_generation: u64,
}

impl<H: ChromeHost> ChromeSlot<H> {
    pub(crate) const fn new(host: H) -> Self {
        Self {
            host,
            live: None,
            last_generation: 0,
        }
    }

    /// Publish `spec` as `publisher` — `None` for a scaffold that holds no
    /// generation yet, else the one its first publish returned. A generation
    /// whose install failed [`ChromeError::NoHost`] retries it here.
    pub(crate) fn publish(
        &mut self,
        publisher: Option<ChromeGeneration>,
        spec: ChromeSpec,
    ) -> PublishOutcome {
        let Some(live) = self.live.as_mut() else {
            if publisher.is_some() {
                return PublishOutcome::Stale;
            }
            self.last_generation = self.last_generation.checked_add(1).unwrap_or(1);
            let generation = ChromeGeneration(self.last_generation);
            let installed = self.host.install(generation, &spec);
            let outcome = match &installed {
                Ok(()) => PublishOutcome::Installed(generation),
                Err(error) => {
                    log_install_failure(error);
                    PublishOutcome::Fallback(generation, error.clone())
                }
            };
            self.live = Some(Live {
                generation,
                field: FieldState::of(spec.search.as_ref()),
                spec,
                installed,
                sink: None,
                busy_logged: false,
            });
            return outcome;
        };
        if publisher != Some(live.generation) {
            if !live.busy_logged {
                live.busy_logged = true;
                log::warn!(
                    "frust-native-widgets: native_scaffold refused — chrome generation {} is \
                     live (one native_scaffold per app)",
                    live.generation.0
                );
            }
            return PublishOutcome::Refused(ChromeError::Busy);
        }
        let generation = live.generation;
        if absorb_failure(&mut self.host, live)
            && let Err(error) = &live.installed
        {
            // Fresh from the arm: fall back now, retry (NoHost) on the next publish.
            let error = error.clone();
            track_search(
                &mut live.field,
                live.spec.search.as_ref(),
                spec.search.as_ref(),
            );
            live.spec = spec;
            return PublishOutcome::Fallback(generation, error);
        }
        if live.installed == Err(ChromeError::NoHost) {
            // NoHost is transient: retry; a success switches to the arm.
            live.installed = self.host.install(generation, &spec);
            if live.installed.is_ok() {
                live.field = FieldState::of(spec.search.as_ref());
            } else {
                // The fallback's field carries on.
                track_search(
                    &mut live.field,
                    live.spec.search.as_ref(),
                    spec.search.as_ref(),
                );
            }
            live.spec = spec;
            return match &live.installed {
                Ok(()) => PublishOutcome::Installed(generation),
                Err(error) => {
                    if *error != ChromeError::NoHost {
                        log_install_failure(error);
                    }
                    PublishOutcome::Fallback(generation, error.clone())
                }
            };
        }
        if let Err(error) = &live.installed {
            let error = error.clone();
            track_search(
                &mut live.field,
                live.spec.search.as_ref(),
                spec.search.as_ref(),
            );
            live.spec = spec;
            return PublishOutcome::Fallback(generation, error);
        }
        let mut diff = diff_specs(&live.spec, &spec);
        let (text, active) = track_search(
            &mut live.field,
            live.spec.search.as_ref(),
            spec.search.as_ref(),
        );
        // An echo of what the field holds never reaches it.
        diff.changed.retain(|field| match field {
            ChromeField::SearchText => text,
            ChromeField::SearchActive => active,
            _ => true,
        });
        if diff.is_empty() {
            // The retained spec still adopts the app's values, so the next
            // diff compares against what the app last said.
            live.spec = spec;
            return PublishOutcome::Unchanged(generation);
        }
        self.host.apply(&diff);
        live.spec = spec;
        PublishOutcome::Applied(generation)
    }

    /// Free the slot if `generation` holds it, disposing an installed host.
    /// `false` (and nothing done) for any other generation.
    pub(crate) fn withdraw(&mut self, generation: ChromeGeneration) -> bool {
        if self.live.as_ref().map(|live| live.generation) != Some(generation) {
            return false;
        }
        if let Some(live) = self.live.as_mut() {
            absorb_failure(&mut self.host, live);
        }
        self.release();
        true
    }

    /// Drop the live generation: dispose an installed host, else tell it the
    /// failed generation is gone.
    fn release(&mut self) {
        match self.live.take() {
            Some(Live {
                installed: Ok(()), ..
            }) => self.host.dispose(),
            Some(live) => self.host.abandon(live.generation),
            None => {}
        }
    }

    /// Test-only: drop whatever is live, disposing an installed host.
    #[cfg(test)]
    fn clear(&mut self) {
        if let Some(live) = self.live.as_mut() {
            absorb_failure(&mut self.host, live);
        }
        self.release();
    }

    /// Register where `generation`'s events go; `false` if it is not live.
    pub(crate) fn set_sink(&mut self, generation: ChromeGeneration, sink: ChromeSink) -> bool {
        match self.live.as_mut() {
            Some(live) if live.generation == generation => {
                live.sink = Some(sink);
                true
            }
            _ => false,
        }
    }

    /// Decode a report stamped `generation` against the live spec, with the
    /// sink to hand it to. `None` for a withdrawn generation, an undecodable
    /// report or no sink.
    pub(crate) fn route(
        &self,
        generation: u64,
        kind: i32,
        detail: i64,
    ) -> Option<(ChromeEvent, ChromeSink)> {
        let live = self
            .live
            .as_ref()
            .filter(|live| live.generation.0 == generation)?;
        let event = decode_event(&live.spec, kind, detail)?;
        Some((event, Arc::clone(live.sink.as_ref()?)))
    }

    /// [`Self::route`] for an id-based `event` an arm resolved against the
    /// spec it shows: re-encoded against the live spec first, so it reaches
    /// the sink only while that spec still carries the same, enabled item.
    /// A search event goes to [`Self::route_search`].
    pub(crate) fn route_event(
        &mut self,
        generation: u64,
        event: &ChromeEvent,
    ) -> Option<(ChromeEvent, ChromeSink)> {
        if let ChromeEvent::Search(search) = event {
            return self.route_search(generation, search.clone());
        }
        let live = self
            .live
            .as_ref()
            .filter(|live| live.generation.0 == generation)?;
        let (kind, detail) = encode_event(&live.spec, event)?;
        self.route(generation, kind, detail)
    }

    /// A search report stamped `generation`: adopted into what the field
    /// holds, then handed back with the sink — unless the live spec has no
    /// search item, the report repeats the field's state
    /// ([`FieldState::adopt`]) or there is no sink.
    pub(crate) fn route_search(
        &mut self,
        generation: u64,
        event: SearchEvent,
    ) -> Option<(ChromeEvent, ChromeSink)> {
        let live = self
            .live
            .as_mut()
            .filter(|live| live.generation.0 == generation && live.spec.search.is_some())?;
        if !live.field.adopt(&event) {
            return None;
        }
        Some((ChromeEvent::Search(event), Arc::clone(live.sink.as_ref()?)))
    }

    /// What the search field of `generation` holds, if it is live and shows
    /// one.
    pub(crate) fn field(&self, generation: ChromeGeneration) -> Option<&FieldState> {
        self.live
            .as_ref()
            .filter(|live| live.generation == generation && live.spec.search.is_some())
            .map(|live| &live.field)
    }

    /// The live generation, if any.
    #[cfg(test)]
    pub(crate) fn generation(&self) -> Option<ChromeGeneration> {
        self.live.as_ref().map(|live| live.generation)
    }

    /// The retained spec of the live generation, if any.
    #[cfg(test)]
    pub(crate) fn spec(&self) -> Option<&ChromeSpec> {
        self.live.as_ref().map(|live| &live.spec)
    }

    #[cfg(test)]
    pub(crate) fn host(&self) -> &H {
        &self.host
    }

    #[cfg(test)]
    pub(crate) fn host_mut(&mut self) -> &mut H {
        &mut self.host
    }
}

/// Take a failure `host` reported for `live` since it answered the install
/// `Ok` (the arm logged it); `true` when one arrived.
fn absorb_failure<H: ChromeHost>(host: &mut H, live: &mut Live) -> bool {
    let Some(error) = host.take_failure(live.generation) else {
        return false;
    };
    live.installed = Err(error);
    true
}

/// Log an arm's failed install. A refusal is a property of the app's theme,
/// not of one scaffold, so it is logged once per process and names the fix.
fn log_install_failure(error: &ChromeError) {
    static REFUSAL_LOGGED: Once = Once::new();
    match error {
        ChromeError::Refused(_) => REFUSAL_LOGGED.call_once(|| {
            log::warn!(
                "frust-native-widgets: {error}; the native chrome arm needs a Material3-derived \
                 app theme (Android: a Theme.Material3.* parent) — rendering frust-drawn \
                 fallback chrome"
            );
        }),
        _ => log::warn!("frust-native-widgets: {error}; rendering fallback chrome"),
    }
}

/// The host behind the process-wide slot, per target: the iOS and Android
/// arms, the recorder under test, and the inert host everywhere else.
#[cfg(all(target_os = "ios", not(test)))]
type PlatformChromeHost = crate::apple::IosChromeHost;
#[cfg(all(target_os = "android", not(test)))]
type PlatformChromeHost = crate::android::AndroidChromeHost;
#[cfg(test)]
type PlatformChromeHost = RecordingChromeHost;
#[cfg(not(any(test, target_os = "ios", target_os = "android")))]
type PlatformChromeHost = host::NullChromeHost;

static SLOT: Mutex<ChromeSlot<PlatformChromeHost>> =
    Mutex::new(ChromeSlot::new(PlatformChromeHost::new()));

/// Lock [`SLOT`], recovering from poisoning: a panicking caller must not turn
/// every later chrome call into a panic.
fn lock_slot() -> MutexGuard<'static, ChromeSlot<PlatformChromeHost>> {
    SLOT.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Serialises every test that touches the process-wide slot and hands it
/// over clean, so a test that panicked while holding it cannot leave a
/// generation live for the next. Poisoning is ignored for the same reason.
#[cfg(test)]
pub(crate) fn slot_test_guard() -> MutexGuard<'static, ()> {
    static GUARD: Mutex<()> = Mutex::new(());
    let guard = GUARD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut slot = lock_slot();
    slot.clear();
    *slot.host_mut() = RecordingChromeHost::new();
    guard
}

/// Test-only: run `f` on the process-wide slot's recording host.
#[cfg(test)]
pub(crate) fn with_test_host<R>(f: impl FnOnce(&mut RecordingChromeHost) -> R) -> R {
    f(lock_slot().host_mut())
}

/// Test-only: the process-wide slot's live generation, if any.
#[cfg(test)]
pub(crate) fn live_generation() -> Option<ChromeGeneration> {
    lock_slot().generation()
}

/// Test-only: the process-wide slot's retained spec, if a generation is live.
#[cfg(test)]
pub(crate) fn live_spec() -> Option<ChromeSpec> {
    lock_slot().spec().cloned()
}

/// Publish `spec` to the process-wide slot — see [`ChromeSlot::publish`].
pub(crate) fn publish(publisher: Option<ChromeGeneration>, spec: ChromeSpec) -> PublishOutcome {
    lock_slot().publish(publisher, spec)
}

/// Scaffold teardown — see [`ChromeSlot::withdraw`].
pub(crate) fn withdraw(generation: ChromeGeneration) -> bool {
    lock_slot().withdraw(generation)
}

/// Register `generation`'s event sink — see [`ChromeSlot::set_sink`].
pub(crate) fn set_sink(generation: ChromeGeneration, sink: ChromeSink) -> bool {
    lock_slot().set_sink(generation, sink)
}

/// An arm's report: decode it and hand it to the sink, outside the lock.
/// Answers whether it was delivered.
pub(crate) fn deliver(generation: u64, kind: i32, detail: i64) -> bool {
    let routed = lock_slot().route(generation, kind, detail);
    hand_over(routed)
}

/// An arm's already-resolved report — see [`ChromeSlot::route_event`].
/// Answers whether it was delivered.
pub(crate) fn deliver_event(generation: u64, event: &ChromeEvent) -> bool {
    let routed = lock_slot().route_event(generation, event);
    hand_over(routed)
}

/// An arm's search report, `kind` one of the search kinds and `text` beside
/// it ([`decode_search`]) — see [`ChromeSlot::route_search`]. Answers
/// whether it was delivered.
#[cfg_attr(not(any(test, target_os = "android")), allow(dead_code))]
pub(crate) fn deliver_search(generation: u64, kind: i32, text: &str) -> bool {
    let Some(event) = decode_search(kind, text) else {
        return false;
    };
    let routed = lock_slot().route_search(generation, event);
    hand_over(routed)
}

/// The live generation, when its spec shows a search item.
#[cfg_attr(not(any(test, target_os = "android")), allow(dead_code))]
pub(crate) fn live_search_generation() -> Option<u64> {
    let slot = lock_slot();
    let live = slot.live.as_ref()?;
    live.spec.search.is_some().then_some(live.generation.0)
}

/// What generation `generation`'s search field holds, when it is live and
/// shows one: the frust-drawn fallback field shows this text.
pub(crate) fn search_field(generation: ChromeGeneration) -> Option<FieldState> {
    lock_slot().field(generation).cloned()
}

/// The native search field took the platform's input focus: end frust's
/// own focus session (a focused text input blurs and gives up its IME) at
/// the next rebuild, and owe the frame that runs it. Main (UI) thread only;
/// never with the slot locked.
#[cfg_attr(not(any(target_os = "ios", target_os = "android")), allow(dead_code))]
pub(crate) fn release_frust_focus() {
    frust_core::mark_focus_orphaned();
    frust::texture::request_frame();
}

/// Call the sink with the routed event; the slot lock is already released.
fn hand_over(routed: Option<(ChromeEvent, ChromeSink)>) -> bool {
    match routed {
        Some((event, sink)) => {
            sink(event);
            true
        }
        None => false,
    }
}

// --- the Android arm's wire -------------------------------------------------

/// The Android arm's wire (decision C3): the flat JSON
/// `FrustNativeChrome.install`/`apply` read and the revision-gated icon table
/// whose bytes travel through `setIcon` instead. Pure, so it compiles and is
/// host-tested on every target; `crate::android::chrome` is its one
/// production caller.
///
/// # The contract
///
/// `FrustNativeChrome.kt`'s file header is the authority: the keys, the
/// `changed` array's [`ChromeField`] spellings, the badge encoding (absent or
/// `null` = none, `""` = dot, anything else shown as-is — Kotlin turns a
/// numeric string into a count) and the `ICON_SLOT_*` codes.
/// `tests/kotlin_conformance.rs` pins the copy of that header the tests below
/// round-trip against, and the slot codes.
///
/// # What `apply` sends
///
/// Only the changed fields' keys, plus a `changed` array naming them —
/// except [`ChromeField::Scroll`], which is never JSON (the arm calls
/// `setScroll` directly), and except that a [`ChromeField::Tabs`] change
/// also carries [`ChromeField::Selected`]: the wire names the selection by
/// index, so a reorder that keeps the selected id moves its index.
///
/// # Icons
///
/// [`IconTable`] is `controls::tab_bar`'s icon side table in miniature: one
/// entry per `(slot, index)` the arm has handed Kotlin, re-sent only when the
/// bytes change (compared by content, like the spec), and a revision per
/// entry that `tab<i>IconRev` carries. Sync it before encoding: the JSON
/// reads its revisions. An icon's bytes are a [`TabIcon::Bytes`] or the
/// bytes half of a [`TabIcon::AppleSymbolWithBytes`]; an Apple symbol alone
/// has no Android rendering — a tab or action carrying one is title-only and
/// a `Leading::Symbol` (or a `Leading::Icon` without bytes) encodes as
/// `"none"` (limitation `native-chrome-android-bitmap-icons`). A
/// `Leading::Icon` with bytes encodes as `"icon"` and its bytes ride
/// [`ICON_SLOT_LEADING`] like the avatar's.
///
/// # Appended keys
///
/// `action<i>Badge`/`toolbar<i>Badge` (the tab badge's encoding) and
/// `action<i>Prominent`/`toolbar<i>Prominent` are written only when set — a
/// badge present, prominent `true` — so a spec using neither encodes exactly
/// as before they existed; Kotlin reads an absent key as none/`false`. A
/// [`ChromeField::ActionBadges`] apply carries every action's and toolbar
/// entry's badge key, `null` for none, and nothing else. Likewise
/// `toolbarStyle` ([`toolbar_style_name`]) is written only for a style other
/// than [`ToolbarStyle::Buttons`] and `toolbarPrimaryLabel` only when `true`;
/// Kotlin reads them absent as `"buttons"`/`false`.
///
/// # Search
///
/// [`SEARCH_FIELDS`] ride an install only when the spec has a search item:
/// `hasSearch` and `searchPlaceholder` ([`ChromeField::Search`]),
/// `searchText` and `searchActive`; Kotlin reads them absent as no search.
/// An apply carries `hasSearch` (`false` when the item went away) for a
/// [`ChromeField::Search`] change and `searchText`/`searchActive` only for
/// the changes the slot did not drop as echoes. The placement hint is not
/// sent: Android has one presentation. The field reports through
/// `nativeOnChromeSearch(generation, kind, text)` with the search kinds.
///
/// # The toolbar
///
/// It rides [`ChromeField::Actions`] (the parent module's "One action index
/// space"): the `toolbar*` keys sit in that group, its entries are the items
/// then the primary (`toolbarPrimary` says whether the last one is), and
/// entry `i` reports as action `actionCount + i`. Its byte icons use
/// [`ICON_SLOT_TOOLBAR`].
#[cfg_attr(not(any(test, target_os = "android")), allow(dead_code))]
pub(crate) mod android_wire {
    use std::fmt::Write as _;
    use std::sync::Arc;

    use super::host::ChromeField;
    use super::{ActionItem, ChromeSpec, Leading, ToolbarSpec, ToolbarStyle, TopBarSpec};
    use crate::api::TabIcon;
    use crate::runtime::escape;

    /// `setIcon` slot: a bottom tab's icon, `index` = tab index.
    pub(crate) const ICON_SLOT_TAB: i32 = 0;
    /// `setIcon` slot: a top-bar action's icon, `index` = action index.
    pub(crate) const ICON_SLOT_ACTION: i32 = 1;
    /// `setIcon` slot: the leading image (avatar), `index` `0`.
    pub(crate) const ICON_SLOT_LEADING: i32 = 2;
    /// `setIcon` slot: a toolbar entry's icon, `index` = toolbar entry index.
    pub(crate) const ICON_SLOT_TOOLBAR: i32 = 3;

    /// Every field `install` encodes, in [`ChromeField`] declaration order.
    pub(crate) const INSTALL_FIELDS: [ChromeField; 9] = [
        ChromeField::Tabs,
        ChromeField::Selected,
        ChromeField::TopBar,
        ChromeField::Title,
        ChromeField::Subtitle,
        ChromeField::Leading,
        ChromeField::Actions,
        ChromeField::LargeTitle,
        ChromeField::Brightness,
    ];

    /// The search item's fields, appended to an install only when the spec
    /// has one (module doc, *Search*).
    pub(crate) const SEARCH_FIELDS: [ChromeField; 3] = [
        ChromeField::Search,
        ChromeField::SearchText,
        ChromeField::SearchActive,
    ];

    /// `field`'s spelling in an apply's `changed` array — its Rust name.
    pub(crate) fn field_name(field: ChromeField) -> &'static str {
        match field {
            ChromeField::Tabs => "Tabs",
            ChromeField::Selected => "Selected",
            ChromeField::TopBar => "TopBar",
            ChromeField::Title => "Title",
            ChromeField::Subtitle => "Subtitle",
            ChromeField::Leading => "Leading",
            ChromeField::Actions => "Actions",
            ChromeField::LargeTitle => "LargeTitle",
            ChromeField::Scroll => "Scroll",
            ChromeField::Brightness => "Brightness",
            ChromeField::ActionBadges => "ActionBadges",
            ChromeField::Search => "Search",
            ChromeField::SearchText => "SearchText",
            ChromeField::SearchActive => "SearchActive",
        }
    }

    /// One JSON value of the flat object.
    #[derive(Clone, Debug, PartialEq)]
    pub(crate) enum Value {
        Str(String),
        /// `null` for `None`.
        OptStr(Option<String>),
        Bool(bool),
        Int(i64),
    }

    /// The flat object's entries, in encoding order.
    pub(crate) type Fields = Vec<(String, Value)>;

    /// The `install` payload: every key, no `changed` array; the search keys
    /// only for a spec with a search item, so one without encodes exactly as
    /// before they existed.
    pub(crate) fn install_json(spec: &ChromeSpec, icons: &IconTable) -> String {
        let search = spec.search.as_ref().map_or(&[][..], |_| &SEARCH_FIELDS[..]);
        let all: Vec<ChromeField> = INSTALL_FIELDS.iter().chain(search).copied().collect();
        to_json(&fields(spec, &all, icons), None)
    }

    /// The `apply` payload for a diff whose changed fields are `changed`, or
    /// `None` when none of them travels as JSON (a scroll-only diff).
    pub(crate) fn apply_json(
        spec: &ChromeSpec,
        changed: &[ChromeField],
        icons: &IconTable,
    ) -> Option<String> {
        let wire = wire_fields(changed);
        if wire.is_empty() {
            return None;
        }
        Some(to_json(&fields(spec, &wire, icons), Some(&wire)))
    }

    /// The fields an apply names on the wire for `changed`: no
    /// [`ChromeField::Scroll`], [`ChromeField::Selected`] beside a
    /// [`ChromeField::Tabs`] change (module doc), and
    /// [`ChromeField::ActionBadges`] only without [`ChromeField::Actions`],
    /// which carries the badges itself; in declaration order.
    pub(crate) fn wire_fields(changed: &[ChromeField]) -> Vec<ChromeField> {
        let tabs = changed.contains(&ChromeField::Tabs);
        let actions = changed.contains(&ChromeField::Actions);
        INSTALL_FIELDS
            .into_iter()
            .chain([ChromeField::ActionBadges])
            .chain(SEARCH_FIELDS)
            .filter(|field| match field {
                ChromeField::Selected => tabs || changed.contains(field),
                ChromeField::ActionBadges => !actions && changed.contains(field),
                _ => changed.contains(field),
            })
            .collect()
    }

    /// The entries `wire` names, read from `spec` (an absent top bar reads as
    /// an empty one, `hasTop` false).
    pub(crate) fn fields(spec: &ChromeSpec, wire: &[ChromeField], icons: &IconTable) -> Fields {
        let empty = TopBarSpec::default();
        let top = spec.top.as_ref().unwrap_or(&empty);
        let mut out = Fields::new();
        let mut put = |key: String, value: Value| out.push((key, value));
        for field in wire {
            match field {
                ChromeField::Tabs => {
                    put("tabCount".into(), Value::Int(count(spec.tabs.len())));
                    for (i, tab) in spec.tabs.iter().enumerate() {
                        put(format!("tab{i}Title"), Value::Str(tab.title().into()));
                        put(format!("tab{i}Enabled"), Value::Bool(tab.is_enabled()));
                        put(
                            format!("tab{i}Badge"),
                            Value::OptStr(tab.badge_text().map(str::to_owned)),
                        );
                        let rev = icons.rev(ICON_SLOT_TAB, i);
                        put(format!("tab{i}IconRev"), Value::Int(rev_value(rev)));
                    }
                }
                ChromeField::Selected => {
                    put("selectedIndex".into(), Value::Int(selected_index(spec)));
                }
                ChromeField::TopBar => put("hasTop".into(), Value::Bool(spec.top.is_some())),
                ChromeField::Title => put("title".into(), Value::Str(top.title.clone())),
                ChromeField::Subtitle => {
                    put("subtitle".into(), Value::OptStr(top.subtitle.clone()));
                }
                ChromeField::Leading => {
                    put(
                        "leading".into(),
                        Value::Str(leading_name(&top.leading).into()),
                    );
                }
                ChromeField::Actions => {
                    put("actionCount".into(), Value::Int(count(top.actions.len())));
                    for (a, action) in top.actions.iter().enumerate() {
                        put_action(&mut put, ("action", "menu"), a, action);
                    }
                    let empty = ToolbarSpec::default();
                    let toolbar = spec.toolbar.as_ref().unwrap_or(&empty);
                    let entries: Vec<&ActionItem> = toolbar.entries().collect();
                    put("toolbarCount".into(), Value::Int(count(entries.len())));
                    put(
                        "toolbarPrimary".into(),
                        Value::Bool(toolbar.primary.is_some()),
                    );
                    for (t, entry) in entries.into_iter().enumerate() {
                        put_action(&mut put, ("toolbar", "toolbarMenu"), t, entry);
                    }
                    // Appended keys, written only when set (module doc).
                    if toolbar.style != ToolbarStyle::Buttons {
                        put(
                            "toolbarStyle".into(),
                            Value::Str(toolbar_style_name(toolbar.style).into()),
                        );
                    }
                    if toolbar.primary_label {
                        put("toolbarPrimaryLabel".into(), Value::Bool(true));
                    }
                }
                ChromeField::LargeTitle => {
                    put("largeTitle".into(), Value::Bool(top.large_title));
                }
                ChromeField::Scroll => {}
                ChromeField::Brightness => put("dark".into(), Value::Bool(spec.dark)),
                ChromeField::ActionBadges => {
                    for (a, action) in top.actions.iter().enumerate() {
                        put(
                            format!("action{a}Badge"),
                            Value::OptStr(action.badge.clone()),
                        );
                    }
                    let entries = spec.toolbar.iter().flat_map(ToolbarSpec::entries);
                    for (t, entry) in entries.enumerate() {
                        put(
                            format!("toolbar{t}Badge"),
                            Value::OptStr(entry.badge.clone()),
                        );
                    }
                }
                ChromeField::Search => {
                    put("hasSearch".into(), Value::Bool(spec.search.is_some()));
                    if let Some(search) = &spec.search {
                        put(
                            "searchPlaceholder".into(),
                            Value::Str(search.placeholder.clone()),
                        );
                    }
                }
                ChromeField::SearchText => {
                    let text = spec.search.as_ref().map(|search| search.text.clone());
                    put("searchText".into(), Value::Str(text.unwrap_or_default()));
                }
                ChromeField::SearchActive => {
                    let active = spec.search.as_ref().is_some_and(|search| search.active);
                    put("searchActive".into(), Value::Bool(active));
                }
            }
        }
        out
    }

    /// One action's keys under `prefix` (`action`/`toolbar`) and its menu's
    /// under `menu_prefix` (`menu`/`toolbarMenu`), at index `a`.
    fn put_action(
        put: &mut impl FnMut(String, Value),
        (prefix, menu_prefix): (&str, &str),
        a: usize,
        action: &ActionItem,
    ) {
        let menu = action.menu.as_deref().unwrap_or_default();
        put(
            format!("{prefix}{a}Title"),
            Value::Str(action.title.clone()),
        );
        put(
            format!("{prefix}{a}HasIcon"),
            Value::Bool(action.icon.as_ref().and_then(TabIcon::bytes).is_some()),
        );
        put(
            format!("{prefix}{a}MenuCount"),
            Value::Int(count(menu.len())),
        );
        // Appended keys, written only when set (module doc).
        if let Some(badge) = &action.badge {
            put(format!("{prefix}{a}Badge"), Value::Str(badge.clone()));
        }
        if action.prominent {
            put(format!("{prefix}{a}Prominent"), Value::Bool(true));
        }
        for (m, item) in menu.iter().enumerate() {
            let key = |field: &str| format!("{menu_prefix}{a}_{m}{field}");
            put(key("Title"), Value::Str(item.title.clone()));
            put(key("Destructive"), Value::Bool(item.destructive));
            put(key("Enabled"), Value::Bool(item.enabled));
        }
    }

    /// `leading`'s wire spelling: an Apple symbol has no Android rendering,
    /// so a leading icon is `"icon"` only when it carries bytes.
    pub(crate) fn leading_name(leading: &Leading) -> &'static str {
        match leading {
            Leading::None | Leading::Symbol(_) => "none",
            Leading::Back => "back",
            Leading::Image(_) => "image",
            Leading::Icon(icon) if icon.bytes().is_some() => "icon",
            Leading::Icon(_) => "none",
        }
    }

    /// `style`'s wire spelling, the `toolbarStyle` value.
    pub(crate) fn toolbar_style_name(style: ToolbarStyle) -> &'static str {
        match style {
            ToolbarStyle::Buttons => "buttons",
            ToolbarStyle::Floating => "floating",
        }
    }

    /// The selected tab's index, `-1` for none (an id no tab carries).
    fn selected_index(spec: &ChromeSpec) -> i64 {
        spec.selected
            .as_ref()
            .and_then(|id| spec.tabs.iter().position(|tab| tab.id() == id))
            .map_or(-1, count)
    }

    fn count(n: usize) -> i64 {
        i64::try_from(n).unwrap_or(i64::MAX)
    }

    fn rev_value(rev: u64) -> i64 {
        i64::try_from(rev).unwrap_or(i64::MAX)
    }

    /// Serialize `fields` as one flat JSON object, with a `changed` array
    /// when it is an apply. Keys are built from ASCII and never escaped.
    pub(crate) fn to_json(fields: &Fields, changed: Option<&[ChromeField]>) -> String {
        let mut out = String::with_capacity(32 + fields.len() * 24);
        out.push('{');
        for (i, (key, value)) in fields.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let _ = write!(out, "\"{key}\":");
            match value {
                Value::Str(text) | Value::OptStr(Some(text)) => {
                    let _ = write!(out, "\"{}\"", escape(text));
                }
                Value::OptStr(None) => out.push_str("null"),
                Value::Bool(flag) => {
                    let _ = write!(out, "{flag}");
                }
                Value::Int(n) => {
                    let _ = write!(out, "{n}");
                }
            }
        }
        if let Some(changed) = changed {
            if !fields.is_empty() {
                out.push(',');
            }
            out.push_str("\"changed\":[");
            for (i, field) in changed.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                let _ = write!(out, "\"{}\"", field_name(*field));
            }
            out.push(']');
        }
        out.push('}');
        out
    }

    /// One `setIcon(slot, index, bytes)` call: `None` clears the entry
    /// (empty bytes on the wire).
    #[derive(Clone, Debug, PartialEq)]
    pub(crate) struct IconUpload {
        pub(crate) slot: i32,
        pub(crate) index: usize,
        pub(crate) bytes: Option<Arc<[u8]>>,
    }

    #[derive(Debug)]
    struct SentIcon {
        slot: i32,
        index: usize,
        rev: u64,
        bytes: Arc<[u8]>,
    }

    /// The icons handed to Kotlin, by `(slot, index)` — module doc's *Icons*.
    #[derive(Debug)]
    pub(crate) struct IconTable {
        sent: Vec<SentIcon>,
        next_rev: u64,
    }

    impl Default for IconTable {
        fn default() -> Self {
            Self::new()
        }
    }

    impl IconTable {
        /// An empty table (const, for the host in the slot's `static`).
        pub(crate) const fn new() -> Self {
            Self {
                sent: Vec::new(),
                next_rev: 1,
            }
        }

        /// The revision the bytes at `(slot, index)` were sent at; `0` for
        /// none.
        pub(crate) fn rev(&self, slot: i32, index: usize) -> u64 {
            self.sent
                .iter()
                .find(|sent| sent.slot == slot && sent.index == index)
                .map_or(0, |sent| sent.rev)
        }

        /// Forget every entry — Kotlin's cache is gone (`dispose`) or about
        /// to be refilled from scratch (`install`).
        pub(crate) fn clear(&mut self) {
            self.sent.clear();
        }

        /// Bring the table to `spec`'s byte icons, answering the calls that
        /// get Kotlin there: clears for entries `spec` no longer has, then
        /// new or changed bytes, in slot then index order. Unchanged bytes
        /// send nothing.
        pub(crate) fn sync(&mut self, spec: &ChromeSpec) -> Vec<IconUpload> {
            let wanted = wanted_icons(spec);
            let mut uploads = Vec::new();
            self.sent.retain(|sent| {
                let keep = wanted
                    .iter()
                    .any(|(slot, index, _)| *slot == sent.slot && *index == sent.index);
                if !keep {
                    uploads.push(IconUpload {
                        slot: sent.slot,
                        index: sent.index,
                        bytes: None,
                    });
                }
                keep
            });
            for (slot, index, bytes) in wanted {
                let rev = self.next_rev;
                let entry = self
                    .sent
                    .iter_mut()
                    .find(|sent| sent.slot == slot && sent.index == index);
                match entry {
                    Some(sent) if *sent.bytes == **bytes => continue,
                    Some(sent) => {
                        sent.bytes = Arc::clone(bytes);
                        sent.rev = rev;
                    }
                    None => self.sent.push(SentIcon {
                        slot,
                        index,
                        rev,
                        bytes: Arc::clone(bytes),
                    }),
                }
                self.next_rev += 1;
                uploads.push(IconUpload {
                    slot,
                    index,
                    bytes: Some(Arc::clone(bytes)),
                });
            }
            uploads
        }
    }

    /// Every byte icon `spec` shows, as `(slot, index, bytes)`: tab icons
    /// (the unselected one — Kotlin's bottom bar has one icon per item),
    /// action icons, the leading image or icon and toolbar entry icons. Each
    /// is an icon's bytes ([`TabIcon::bytes`]).
    fn wanted_icons(spec: &ChromeSpec) -> Vec<(i32, usize, &Arc<[u8]>)> {
        let mut wanted = Vec::new();
        for (i, tab) in spec.tabs.iter().enumerate() {
            if let Some(bytes) = tab.icon().bytes() {
                wanted.push((ICON_SLOT_TAB, i, bytes));
            }
        }
        if let Some(top) = &spec.top {
            for (a, action) in top.actions.iter().enumerate() {
                if let Some(bytes) = action.icon.as_ref().and_then(TabIcon::bytes) {
                    wanted.push((ICON_SLOT_ACTION, a, bytes));
                }
            }
            let leading = match &top.leading {
                Leading::Image(bytes) => Some(bytes),
                Leading::Icon(icon) => icon.bytes(),
                Leading::None | Leading::Back | Leading::Symbol(_) => None,
            };
            if let Some(bytes) = leading {
                wanted.push((ICON_SLOT_LEADING, 0, bytes));
            }
        }
        let entries = spec.toolbar.iter().flat_map(ToolbarSpec::entries);
        for (t, entry) in entries.enumerate() {
            if let Some(bytes) = entry.icon.as_ref().and_then(TabIcon::bytes) {
                wanted.push((ICON_SLOT_TOOLBAR, t, bytes));
            }
        }
        wanted
    }

    #[cfg(test)]
    mod tests {
        use std::collections::BTreeSet;

        use super::*;
        use crate::api::{TabId, TabItem};
        use crate::chrome::{ActionItem, MenuItem, SearchSpec};

        /// `FrustNativeChrome.kt`'s key table, copied verbatim from its file
        /// header (`tests/kotlin_conformance.rs` fails if the two drift).
        const KOTLIN_KEY_HEADER: &str = r#"
//   Tabs:     tabCount, tab<i>Title, tab<i>Enabled, tab<i>Badge (absent/null =
//             none, "" = dot), tab<i>IconRev (0 = no icon)
//   Selected: selectedIndex (-1 = none)
//   TopBar:   hasTop          Title: title          Subtitle: subtitle (absent/null = none)
//   Leading:  leading = "none" | "back" | "image" | "icon"   LargeTitle: largeTitle (true = collapsing bar)
//   Actions:  actionCount, action<i>Title, action<i>HasIcon, action<i>MenuCount,
//             action<i>Badge (absent/null = none, "" = dot), action<i>Prominent (absent = false),
//             menu<i>_<j>Title, menu<i>_<j>Destructive, menu<i>_<j>Enabled,
//             toolbarCount, toolbarPrimary (the last entry is the primary),
//             toolbar<i>Title, toolbar<i>HasIcon, toolbar<i>MenuCount, toolbar<i>Badge, toolbar<i>Prominent,
//             toolbarMenu<i>_<j>Title, toolbarMenu<i>_<j>Destructive, toolbarMenu<i>_<j>Enabled,
//             toolbarStyle = "buttons" | "floating" (absent = "buttons"), toolbarPrimaryLabel (absent = false)
//   Brightness: dark
//   ActionBadges (apply only, never beside Actions): action<i>Badge and toolbar<i>Badge
//             for every entry (null = none); the counts are unchanged
//   Search:   hasSearch (absent = false), searchPlaceholder   SearchText: searchText
//   SearchActive: searchActive (true = expanded and focused); install sends these three only with a search
"#;

        /// Every key template [`KOTLIN_KEY_HEADER`] names.
        const KEY_TEMPLATES: [&str; 38] = [
            "tabCount",
            "tab<i>Title",
            "tab<i>Enabled",
            "tab<i>Badge",
            "tab<i>IconRev",
            "selectedIndex",
            "hasTop",
            "title",
            "subtitle",
            "leading",
            "largeTitle",
            "actionCount",
            "action<i>Title",
            "action<i>HasIcon",
            "action<i>MenuCount",
            "action<i>Badge",
            "action<i>Prominent",
            "menu<i>_<j>Title",
            "menu<i>_<j>Destructive",
            "menu<i>_<j>Enabled",
            "toolbarCount",
            "toolbarPrimary",
            "toolbar<i>Title",
            "toolbar<i>HasIcon",
            "toolbar<i>MenuCount",
            "toolbar<i>Badge",
            "toolbar<i>Prominent",
            "toolbarMenu<i>_<j>Title",
            "toolbarMenu<i>_<j>Destructive",
            "toolbarMenu<i>_<j>Enabled",
            "toolbarStyle",
            "toolbarPrimaryLabel",
            "dark",
            "hasSearch",
            "searchPlaceholder",
            "searchText",
            "searchActive",
            // Not a spec key: an apply's field list.
            "changed",
        ];

        fn bytes(content: &[u8]) -> Arc<[u8]> {
            Arc::from(content.to_vec().into_boxed_slice())
        }

        /// A spec using none of the badge, prominent or symbol-with-bytes
        /// API: two tabs (one byte icon, one badge), a top bar with an icon
        /// action and a two-item menu action, and a toolbar with a menu item
        /// and a byte-icon primary.
        fn old_shape_spec() -> ChromeSpec {
            ChromeSpec {
                tabs: vec![
                    TabItem::new("home", "Home", TabIcon::Bytes(bytes(&[1]))).badge("3"),
                    TabItem::new("me", "Me", TabIcon::AppleSymbol("person".into())).enabled(false),
                ],
                selected: Some(TabId::new("me")),
                top: Some(TopBarSpec {
                    title: "Inbox".into(),
                    subtitle: Some("2 new".into()),
                    leading: Leading::Image(bytes(&[7])),
                    actions: vec![
                        ActionItem::new("add", "Add").icon(TabIcon::Bytes(bytes(&[2]))),
                        ActionItem::new("more", "More").menu(vec![
                            MenuItem::new("share", "Share"),
                            MenuItem::new("delete", "Delete")
                                .destructive(true)
                                .enabled(false),
                        ]),
                    ],
                    large_title: true,
                }),
                toolbar: Some(
                    ToolbarSpec::new()
                        .item(
                            ActionItem::new("sort", "Sort")
                                .menu(vec![MenuItem::new("new", "Newest").enabled(false)]),
                        )
                        .primary(
                            ActionItem::new("comment", "Comment").icon(TabIcon::Bytes(bytes(&[5]))),
                        ),
                ),
                scroll: None,
                dark: true,
                search: None,
            }
        }

        /// A spec exercising every key: [`old_shape_spec`] with a badged,
        /// prominent top-bar action, a prominent toolbar item, a dot-badged
        /// primary, the floating style, a labelled primary and an active
        /// search item.
        fn full_spec() -> ChromeSpec {
            let mut spec = old_shape_spec();
            spec.search = Some(
                SearchSpec::new()
                    .placeholder("Find \"it\"")
                    .text("iss")
                    .active(true),
            );
            let top = spec.top.as_mut().unwrap();
            top.actions[0] = top.actions[0].clone().badge("4").prominent(true);
            let toolbar = spec.toolbar.take().unwrap();
            let mut toolbar = toolbar.style(ToolbarStyle::Floating).primary_label(true);
            toolbar.items[0] = toolbar.items[0].clone().prominent(true);
            toolbar.primary = toolbar.primary.take().map(|primary| primary.badge(""));
            spec.toolbar = Some(toolbar);
            spec
        }

        /// `key` with its first digit run spelled `<i>` and its second `<j>`.
        fn template(key: &str) -> String {
            let mut out = String::new();
            let mut runs = 0;
            let mut in_run = false;
            for c in key.chars() {
                if c.is_ascii_digit() {
                    if !in_run {
                        out.push_str(if runs == 0 { "<i>" } else { "<j>" });
                        runs += 1;
                        in_run = true;
                    }
                } else {
                    in_run = false;
                    out.push(c);
                }
            }
            out
        }

        /// Whether `token` appears in `text` as a whole word.
        fn names(text: &str, token: &str) -> bool {
            text.match_indices(token).any(|(at, _)| {
                let word = |c: char| c.is_ascii_alphanumeric() || c == '<' || c == '>';
                let before = text[..at].chars().next_back().is_none_or(|c| !word(c));
                let after = text[at + token.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| !word(c) && c != '_');
                before && after
            })
        }

        /// The JSON keys of a flat object as the test parser sees them:
        /// every string literal followed by `:`, honouring escapes.
        fn json_keys(json: &str) -> Vec<String> {
            let mut keys = Vec::new();
            let mut chars = json.chars().peekable();
            while let Some(c) = chars.next() {
                if c != '"' {
                    continue;
                }
                let mut text = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => {
                            chars.next();
                        }
                        '"' => break,
                        c => text.push(c),
                    }
                }
                if chars.peek() == Some(&':') {
                    keys.push(text);
                }
            }
            keys
        }

        #[test]
        fn install_round_trips_the_kotlin_key_table() {
            for template in KEY_TEMPLATES {
                assert!(
                    template == "changed" || names(KOTLIN_KEY_HEADER, template),
                    "{template} is not in the Kotlin header copy"
                );
            }
            let mut icons = IconTable::new();
            icons.sync(&full_spec());
            let json = install_json(&full_spec(), &icons);
            let encoded: BTreeSet<String> = json_keys(&json).iter().map(|k| template(k)).collect();
            let expected: BTreeSet<String> = KEY_TEMPLATES
                .iter()
                .filter(|key| **key != "changed")
                .map(|key| (*key).to_string())
                .collect();
            assert_eq!(encoded, expected, "{json}");
        }

        #[test]
        fn an_apply_carries_its_fields_and_the_changed_spellings() {
            let mut icons = IconTable::new();
            icons.sync(&full_spec());
            let json = apply_json(
                &full_spec(),
                &[ChromeField::Title, ChromeField::Brightness],
                &icons,
            )
            .expect("JSON fields");
            assert_eq!(
                json,
                r#"{"title":"Inbox","dark":true,"changed":["Title","Brightness"]}"#
            );
            for field in INSTALL_FIELDS {
                assert_eq!(field_name(field), format!("{field:?}"));
            }
            assert_eq!(field_name(ChromeField::Scroll), "Scroll");
            let keys = json_keys(&apply_json(&full_spec(), &INSTALL_FIELDS, &icons).unwrap());
            assert!(
                keys.iter()
                    .all(|key| names(KOTLIN_KEY_HEADER, &template(key)) || key == "changed")
            );
        }

        #[test]
        fn scroll_never_travels_as_json_and_tabs_carry_the_selection() {
            let icons = IconTable::new();
            assert_eq!(
                apply_json(&full_spec(), &[ChromeField::Scroll], &icons),
                None
            );
            assert_eq!(
                wire_fields(&[ChromeField::Tabs, ChromeField::Scroll]),
                [ChromeField::Tabs, ChromeField::Selected]
            );
            let json = apply_json(&full_spec(), &[ChromeField::Tabs], &icons).unwrap();
            assert!(json.contains(r#""selectedIndex":1"#), "{json}");
            assert!(
                json.ends_with(r#""changed":["Tabs","Selected"]}"#),
                "{json}"
            );
        }

        #[test]
        fn values_escape_and_pack_by_index() {
            let mut spec = full_spec();
            spec.top.as_mut().unwrap().title = "say \"hi\"\\\n\u{1}".into();
            spec.top.as_mut().unwrap().subtitle = None;
            spec.tabs[1] = spec.tabs[1].clone().badge("");
            let mut icons = IconTable::new();
            icons.sync(&spec);
            let json = install_json(&spec, &icons);
            for needle in [
                r#""title":"say \"hi\"\\\n\u0001""#,
                r#""subtitle":null"#,
                r#""tabCount":2"#,
                r#""tab0Title":"Home""#,
                r#""tab0Badge":"3""#,
                r#""tab0IconRev":1"#,
                r#""tab1Enabled":false"#,
                r#""tab1Badge":"""#,
                r#""tab1IconRev":0"#,
                r#""selectedIndex":1"#,
                r#""hasTop":true"#,
                r#""leading":"image""#,
                r#""actionCount":2"#,
                r#""action0HasIcon":true"#,
                r#""action0MenuCount":0"#,
                r#""action1HasIcon":false"#,
                r#""action1MenuCount":2"#,
                r#""menu1_0Title":"Share""#,
                r#""menu1_1Destructive":true"#,
                r#""menu1_1Enabled":false"#,
                r#""largeTitle":true"#,
                r#""toolbarCount":2"#,
                r#""toolbarPrimary":true"#,
                r#""toolbar0Title":"Sort""#,
                r#""toolbar0HasIcon":false"#,
                r#""toolbar0MenuCount":1"#,
                r#""toolbarMenu0_0Title":"Newest""#,
                r#""toolbarMenu0_0Enabled":false"#,
                r#""toolbar1Title":"Comment""#,
                r#""toolbar1HasIcon":true"#,
                r#""dark":true"#,
            ] {
                assert!(json.contains(needle), "{needle} missing from {json}");
            }
            assert!(!json.contains("changed"));
            assert!(json.starts_with('{') && json.ends_with('}'));
        }

        #[test]
        fn absent_top_and_unknown_selection_encode_empty() {
            let spec = ChromeSpec {
                selected: Some(TabId::new("gone")),
                ..ChromeSpec::default()
            };
            assert_eq!(
                install_json(&spec, &IconTable::new()),
                r#"{"tabCount":0,"selectedIndex":-1,"hasTop":false,"title":"","subtitle":null,"leading":"none","actionCount":0,"toolbarCount":0,"toolbarPrimary":false,"largeTitle":false,"dark":false}"#
            );
            assert_eq!(leading_name(&Leading::Symbol("person".into())), "none");
            assert_eq!(leading_name(&Leading::Back), "back");
        }

        #[test]
        fn icons_are_sent_once_per_content_and_cleared_when_gone() {
            let mut icons = IconTable::new();
            let first = icons.sync(&full_spec());
            assert_eq!(
                first
                    .iter()
                    .map(|up| (up.slot, up.index, up.bytes.as_deref().map(<[u8]>::to_vec)))
                    .collect::<Vec<_>>(),
                [
                    (ICON_SLOT_TAB, 0, Some(vec![1])),
                    (ICON_SLOT_ACTION, 0, Some(vec![2])),
                    (ICON_SLOT_LEADING, 0, Some(vec![7])),
                    (ICON_SLOT_TOOLBAR, 1, Some(vec![5])),
                ]
            );
            // Same content in fresh Arcs: nothing to send, revisions kept.
            assert!(icons.sync(&full_spec()).is_empty());
            assert_eq!(icons.rev(ICON_SLOT_TAB, 0), 1);
            // Changed bytes re-send at a fresh revision; a gone icon clears.
            let mut next = full_spec();
            next.tabs[0] = TabItem::new("home", "Home", TabIcon::Bytes(bytes(&[9])));
            next.top.as_mut().unwrap().leading = Leading::Back;
            let uploads = icons.sync(&next);
            assert_eq!(
                uploads,
                [
                    IconUpload {
                        slot: ICON_SLOT_LEADING,
                        index: 0,
                        bytes: None
                    },
                    IconUpload {
                        slot: ICON_SLOT_TAB,
                        index: 0,
                        bytes: Some(bytes(&[9]))
                    },
                ]
            );
            assert_eq!(icons.rev(ICON_SLOT_TAB, 0), 5);
            assert_eq!(icons.rev(ICON_SLOT_LEADING, 0), 0);
            // An Apple symbol is title-only: no bytes, no revision.
            assert_eq!(icons.rev(ICON_SLOT_TAB, 1), 0);
            icons.clear();
            assert_eq!(icons.sync(&next).len(), 3, "a cleared table re-sends all");
        }

        /// [`old_shape_spec`]'s install payload as the encoder wrote it
        /// before the badge, prominent and symbol-with-bytes keys existed.
        const OLD_SHAPE_INSTALL: &str = concat!(
            r#"{"tabCount":2,"tab0Title":"Home","tab0Enabled":true,"tab0Badge":"3","#,
            r#""tab0IconRev":1,"tab1Title":"Me","tab1Enabled":false,"tab1Badge":null,"#,
            r#""tab1IconRev":0,"selectedIndex":1,"hasTop":true,"title":"Inbox","#,
            r#""subtitle":"2 new","leading":"image","actionCount":2,"action0Title":"Add","#,
            r#""action0HasIcon":true,"action0MenuCount":0,"action1Title":"More","#,
            r#""action1HasIcon":false,"action1MenuCount":2,"menu1_0Title":"Share","#,
            r#""menu1_0Destructive":false,"menu1_0Enabled":true,"menu1_1Title":"Delete","#,
            r#""menu1_1Destructive":true,"menu1_1Enabled":false,"toolbarCount":2,"#,
            r#""toolbarPrimary":true,"toolbar0Title":"Sort","toolbar0HasIcon":false,"#,
            r#""toolbar0MenuCount":1,"toolbarMenu0_0Title":"Newest","#,
            r#""toolbarMenu0_0Destructive":false,"toolbarMenu0_0Enabled":false,"#,
            r#""toolbar1Title":"Comment","toolbar1HasIcon":true,"toolbar1MenuCount":0,"#,
            r#""largeTitle":true,"dark":true}"#,
        );

        #[test]
        fn a_spec_without_the_appended_api_encodes_byte_identically() {
            let mut icons = IconTable::new();
            icons.sync(&old_shape_spec());
            assert_eq!(install_json(&old_shape_spec(), &icons), OLD_SHAPE_INSTALL);
            let json = apply_json(&old_shape_spec(), &[ChromeField::Actions], &icons).unwrap();
            for key in ["Badge", "Prominent"] {
                assert!(!json_keys(&json).iter().any(|k| k.contains(key)), "{json}");
            }
        }

        #[test]
        fn the_appended_action_keys_round_trip() {
            let mut icons = IconTable::new();
            icons.sync(&full_spec());
            let json = install_json(&full_spec(), &icons);
            for needle in [
                r#""action0MenuCount":0,"action0Badge":"4","action0Prominent":true,"#,
                r#""toolbar0MenuCount":1,"toolbar0Prominent":true,"#,
                r#""toolbar1MenuCount":0,"toolbar1Badge":"","#,
                r#""toolbarStyle":"floating","toolbarPrimaryLabel":true,"largeTitle""#,
            ] {
                assert!(json.contains(needle), "{needle} missing from {json}");
            }
            assert!(!json.contains("action1Badge"), "an absent badge is no key");
            assert!(!json.contains("action1Prominent"), "plain is no key");
            // The old payload plus exactly the appended keys.
            let old: BTreeSet<String> = json_keys(OLD_SHAPE_INSTALL).into_iter().collect();
            let new: BTreeSet<String> = json_keys(&json).into_iter().collect();
            let added: Vec<&String> = new.difference(&old).collect();
            assert_eq!(
                added,
                [
                    "action0Badge",
                    "action0Prominent",
                    "hasSearch",
                    "searchActive",
                    "searchPlaceholder",
                    "searchText",
                    "toolbar0Prominent",
                    "toolbar1Badge",
                    "toolbarPrimaryLabel",
                    "toolbarStyle"
                ]
            );
            assert!(old.is_subset(&new));
        }

        #[test]
        fn a_search_item_appends_its_keys_after_every_other_one() {
            let icons = IconTable::new();
            let json = install_json(&full_spec(), &icons);
            assert!(
                json.ends_with(concat!(
                    r#""dark":true,"hasSearch":true,"searchPlaceholder":"Find \"it\"","#,
                    r#""searchText":"iss","searchActive":true}"#
                )),
                "{json}"
            );
            // Without one, none of its keys: the old payload exactly.
            let mut icons = IconTable::new();
            icons.sync(&old_shape_spec());
            let bare = install_json(&old_shape_spec(), &icons);
            assert_eq!(bare, OLD_SHAPE_INSTALL);
            assert!(!bare.contains("Search") && !bare.contains("search"));
        }

        #[test]
        fn a_search_apply_carries_only_the_changed_search_keys() {
            let icons = IconTable::new();
            let spec = full_spec();
            let text = apply_json(&spec, &[ChromeField::SearchText], &icons).unwrap();
            assert_eq!(text, r#"{"searchText":"iss","changed":["SearchText"]}"#);
            let active = apply_json(&spec, &[ChromeField::SearchActive], &icons).unwrap();
            assert_eq!(
                active,
                r#"{"searchActive":true,"changed":["SearchActive"]}"#
            );
            let both = apply_json(
                &spec,
                &[ChromeField::Search, ChromeField::SearchActive],
                &icons,
            )
            .unwrap();
            assert_eq!(
                both,
                concat!(
                    r#"{"hasSearch":true,"searchPlaceholder":"Find \"it\"","#,
                    r#""searchActive":true,"changed":["Search","SearchActive"]}"#
                )
            );
            // The item going away says so, with nothing else.
            let gone = apply_json(&old_shape_spec(), &[ChromeField::Search], &icons).unwrap();
            assert_eq!(gone, r#"{"hasSearch":false,"changed":["Search"]}"#);
            for field in SEARCH_FIELDS {
                assert_eq!(field_name(field), format!("{field:?}"));
            }
        }

        #[test]
        fn the_toolbar_style_and_primary_label_keys_are_written_only_when_set() {
            let icons = IconTable::new();
            let with = |toolbar: ToolbarSpec| ChromeSpec {
                toolbar: Some(toolbar),
                ..old_shape_spec()
            };
            let base = old_shape_spec().toolbar.unwrap();
            // The defaults spelled out encode exactly as the old shape.
            let explicit = with(
                base.clone()
                    .style(ToolbarStyle::Buttons)
                    .primary_label(false),
            );
            let mut synced = IconTable::new();
            synced.sync(&explicit);
            assert_eq!(install_json(&explicit, &synced), OLD_SHAPE_INSTALL);
            // Each key alone, in the Actions group, after the entries.
            let floating = with(base.clone().style(ToolbarStyle::Floating));
            let json = apply_json(&floating, &[ChromeField::Actions], &icons).unwrap();
            assert!(
                json.ends_with(
                    r#""toolbar1MenuCount":0,"toolbarStyle":"floating","changed":["Actions"]}"#
                ),
                "{json}"
            );
            assert!(!json.contains("toolbarPrimaryLabel"), "{json}");
            let labelled = with(base.primary_label(true));
            let json = apply_json(&labelled, &[ChromeField::Actions], &icons).unwrap();
            assert!(
                json.ends_with(
                    r#""toolbar1MenuCount":0,"toolbarPrimaryLabel":true,"changed":["Actions"]}"#
                ),
                "{json}"
            );
            assert!(!json.contains("toolbarStyle"), "{json}");
            assert_eq!(toolbar_style_name(ToolbarStyle::Buttons), "buttons");
            assert_eq!(toolbar_style_name(ToolbarStyle::Floating), "floating");
            // An absent toolbar writes neither.
            let bare = install_json(&ChromeSpec::default(), &icons);
            assert!(!bare.contains("toolbarStyle") && !bare.contains("toolbarPrimaryLabel"));
        }

        #[test]
        fn a_badges_apply_carries_every_entrys_badge_and_nothing_else() {
            let icons = IconTable::new();
            let json = apply_json(&full_spec(), &[ChromeField::ActionBadges], &icons).unwrap();
            assert_eq!(
                json,
                concat!(
                    r#"{"action0Badge":"4","action1Badge":null,"toolbar0Badge":null,"#,
                    r#""toolbar1Badge":"","changed":["ActionBadges"]}"#
                )
            );
            assert!(
                json_keys(&json)
                    .iter()
                    .all(|key| names(KOTLIN_KEY_HEADER, &template(key)) || key == "changed")
            );
            // Beside a rebuild it is dropped: the rebuild carries the badges.
            assert_eq!(
                wire_fields(&[ChromeField::Actions, ChromeField::ActionBadges]),
                [ChromeField::Actions]
            );
            assert_eq!(
                wire_fields(&[ChromeField::Title, ChromeField::ActionBadges]),
                [ChromeField::Title, ChromeField::ActionBadges]
            );
            assert_eq!(field_name(ChromeField::ActionBadges), "ActionBadges");
        }

        #[test]
        fn symbol_with_bytes_icons_send_their_bytes_and_a_leading_icon_encodes_icon() {
            let mut spec = old_shape_spec();
            spec.tabs[1] = TabItem::new(
                "me",
                "Me",
                TabIcon::apple_symbol_with_bytes("person", bytes(&[8])),
            );
            let top = spec.top.as_mut().unwrap();
            top.actions[1] = top.actions[1]
                .clone()
                .icon(TabIcon::apple_symbol_with_bytes("ellipsis", bytes(&[3])));
            top.leading = Leading::Icon(TabIcon::apple_symbol_with_bytes("bell", bytes(&[6])));
            let mut icons = IconTable::new();
            let uploads: Vec<(i32, usize)> = icons
                .sync(&spec)
                .iter()
                .map(|up| (up.slot, up.index))
                .collect();
            assert_eq!(
                uploads,
                [
                    (ICON_SLOT_TAB, 0),
                    (ICON_SLOT_TAB, 1),
                    (ICON_SLOT_ACTION, 0),
                    (ICON_SLOT_ACTION, 1),
                    (ICON_SLOT_LEADING, 0),
                    (ICON_SLOT_TOOLBAR, 1),
                ]
            );
            let json = install_json(&spec, &icons);
            for needle in [
                r#""tab1IconRev":2"#,
                r#""action1HasIcon":true"#,
                r#""leading":"icon""#,
            ] {
                assert!(json.contains(needle), "{needle} missing from {json}");
            }
            let symbol_only = Leading::Icon(TabIcon::AppleSymbol("bell".into()));
            assert_eq!(leading_name(&symbol_only), "none");
            assert_eq!(
                leading_name(&Leading::Icon(TabIcon::Bytes(bytes(&[1])))),
                "icon"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::host::{ChromeField, HostCall, RECORDED_CALLS_CAP};
    use super::*;
    use std::sync::Mutex as StdMutex;

    fn bytes(content: &[u8]) -> Arc<[u8]> {
        Arc::from(content.to_vec().into_boxed_slice())
    }

    fn spec() -> ChromeSpec {
        ChromeSpec {
            tabs: vec![
                TabItem::new("home", "Home", TabIcon::AppleSymbol("house".into())),
                TabItem::new("inbox", "Inbox", TabIcon::Bytes(bytes(&[1, 2, 3]))),
            ],
            selected: Some(TabId::new("home")),
            top: Some(TopBarSpec {
                title: "Home".into(),
                subtitle: None,
                leading: Leading::Back,
                actions: vec![
                    ActionItem::new("add", "Add").icon(TabIcon::AppleSymbol("plus".into())),
                    ActionItem::new("more", "More").menu(vec![
                        MenuItem::new("share", "Share"),
                        MenuItem::new("delete", "Delete").destructive(true),
                    ]),
                ],
                large_title: true,
            }),
            toolbar: None,
            scroll: None,
            dark: false,
            search: None,
        }
    }

    /// [`spec`] with a search item showing `text`, expanded when `active`.
    fn searching(text: &str, active: bool) -> ChromeSpec {
        ChromeSpec {
            search: Some(
                SearchSpec::new()
                    .placeholder("Search")
                    .text(text)
                    .active(active),
            ),
            ..spec()
        }
    }

    /// A slot installed with [`searching`]`("", false)` and a sink recording
    /// every event.
    fn search_slot() -> (
        ChromeSlot<RecordingChromeHost>,
        ChromeGeneration,
        Arc<StdMutex<Vec<ChromeEvent>>>,
    ) {
        let mut slot = slot();
        let generation = slot
            .publish(None, searching("", false))
            .generation()
            .unwrap();
        slot.host_mut().take_calls();
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let sink_seen = Arc::clone(&seen);
        assert!(slot.set_sink(
            generation,
            Arc::new(move |event| sink_seen.lock().unwrap().push(event))
        ));
        (slot, generation, seen)
    }

    /// Route `event` from the field and hand it to the sink, as `deliver` does.
    fn report_search(
        slot: &mut ChromeSlot<RecordingChromeHost>,
        generation: ChromeGeneration,
        event: SearchEvent,
    ) -> bool {
        match slot.route_search(generation.get(), event) {
            Some((event, sink)) => {
                sink(event);
                true
            }
            None => false,
        }
    }

    #[test]
    fn every_search_event_round_trips_its_kind_and_text() {
        for event in [
            SearchEvent::Changed("issues".into()),
            SearchEvent::Changed(String::new()),
            SearchEvent::Submitted("pull \u{65e5}".into()),
            SearchEvent::Began,
            SearchEvent::Ended,
        ] {
            let (kind, text) = encode_search(&event);
            assert!((14..=17).contains(&kind), "{event:?} kind {kind}");
            assert_eq!(decode_search(kind, text), Some(event.clone()));
        }
        // Began/Ended ignore any text; other kinds are no search report.
        assert_eq!(
            decode_search(EVENT_KIND_CHROME_SEARCH_BEGAN, "x"),
            Some(SearchEvent::Began)
        );
        for kind in [EVENT_KIND_CHROME_BACK, 13, 18, 0] {
            assert_eq!(decode_search(kind, "x"), None, "{kind}");
        }
        // A search event has no (kind, detail) form, and the int path never
        // decodes a search kind.
        let spec = searching("", false);
        let began = ChromeEvent::Search(SearchEvent::Began);
        assert_eq!(encode_event(&spec, &began), None);
        assert_eq!(decode_event(&spec, EVENT_KIND_CHROME_SEARCH_BEGAN, 0), None);
    }

    #[test]
    fn the_search_fields_are_reported_on_their_own() {
        let base = searching("is", false);
        let cases: [(ChromeField, ChromeSpec); 5] = [
            (ChromeField::Search, spec()),
            (ChromeField::Search, {
                let mut next = base.clone();
                next.search.as_mut().unwrap().placeholder = "Find".into();
                next
            }),
            (ChromeField::Search, {
                let mut next = base.clone();
                next.search.as_mut().unwrap().placement = SearchPlacement::NavigationBar;
                next
            }),
            (ChromeField::SearchText, searching("iss", false)),
            (ChromeField::SearchActive, searching("is", true)),
        ];
        for (field, next) in cases {
            let diff = diff_specs(&base, &next);
            if next.search.is_none() {
                // Going away also resets the text it showed.
                assert_eq!(diff.changed, [ChromeField::Search, ChromeField::SearchText]);
            } else {
                assert_eq!(diff.changed, [field], "{field:?}");
            }
        }
        // A search item appearing beside a title change, in declaration order.
        let mut both = searching("", true);
        both.top.as_mut().unwrap().title.push('!');
        assert_eq!(
            diff_specs(&spec(), &both).changed,
            [
                ChromeField::Title,
                ChromeField::Search,
                ChromeField::SearchActive
            ]
        );
    }

    #[test]
    fn writing_back_the_typed_text_reaches_the_field_never() {
        let (mut slot, generation, seen) = search_slot();
        for typed in ["i", "is", "iss"] {
            assert!(report_search(
                &mut slot,
                generation,
                SearchEvent::Changed(typed.into())
            ));
            // The app confirms what it was told: an echo, no host call.
            assert_eq!(
                slot.publish(Some(generation), searching(typed, false)),
                PublishOutcome::Unchanged(generation),
                "{typed}"
            );
        }
        assert_eq!(slot.host().calls().count(), 0, "no wire update for an echo");
        assert_eq!(seen.lock().unwrap().len(), 3);
        // The field repeating itself is not delivered again.
        assert!(!report_search(
            &mut slot,
            generation,
            SearchEvent::Changed("iss".into())
        ));
        assert_eq!(seen.lock().unwrap().len(), 3);
    }

    #[test]
    fn an_app_change_of_the_text_is_applied_once() {
        let (mut slot, generation, _) = search_slot();
        assert!(report_search(
            &mut slot,
            generation,
            SearchEvent::Changed("iss".into())
        ));
        slot.publish(Some(generation), searching("iss", false));
        // The app clears it: applied, and the field now holds it.
        let cleared = searching("", false);
        assert_eq!(
            slot.publish(Some(generation), cleared.clone()),
            PublishOutcome::Applied(generation)
        );
        assert_eq!(
            slot.host_mut().take_calls(),
            [HostCall::Apply {
                changed: vec![ChromeField::SearchText],
                spec: cleared.clone(),
            }]
        );
        assert_eq!(
            slot.publish(Some(generation), cleared),
            PublishOutcome::Unchanged(generation)
        );
        // An app that never writes the text back leaves the typing alone.
        assert!(report_search(
            &mut slot,
            generation,
            SearchEvent::Changed("pu".into())
        ));
        assert_eq!(
            slot.publish(Some(generation), searching("", false)),
            PublishOutcome::Unchanged(generation),
            "its value did not change, so nothing is pushed over the typing"
        );
        assert_eq!(
            slot.field(generation).map(|field| field.text.as_str()),
            Some("pu")
        );
    }

    #[test]
    fn begin_and_end_are_confirmed_without_an_echo_and_applied_from_the_app() {
        let (mut slot, generation, seen) = search_slot();
        assert!(report_search(&mut slot, generation, SearchEvent::Began));
        assert!(
            !report_search(&mut slot, generation, SearchEvent::Began),
            "a second begin says nothing new"
        );
        assert_eq!(
            slot.publish(Some(generation), searching("", true)),
            PublishOutcome::Unchanged(generation)
        );
        assert!(report_search(
            &mut slot,
            generation,
            SearchEvent::Changed("is".into())
        ));
        assert!(report_search(
            &mut slot,
            generation,
            SearchEvent::Submitted("is".into())
        ));
        assert!(
            report_search(&mut slot, generation, SearchEvent::Submitted("is".into())),
            "a submit is always delivered"
        );
        assert_eq!(
            slot.publish(Some(generation), searching("is", true)),
            PublishOutcome::Unchanged(generation)
        );
        // The app ends the search (a tab switch): applied, and it empties the field.
        let ended = searching("", false);
        assert_eq!(
            slot.publish(Some(generation), ended.clone()),
            PublishOutcome::Applied(generation)
        );
        assert_eq!(
            slot.host_mut().take_calls(),
            [HostCall::Apply {
                changed: vec![ChromeField::SearchActive],
                spec: ended,
            }],
            "the text it cleared is not sent again"
        );
        assert_eq!(slot.field(generation), Some(&FieldState::default()));
        assert!(
            !report_search(&mut slot, generation, SearchEvent::Ended),
            "the field's own end after the app's collapse is an echo"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            [
                ChromeEvent::Search(SearchEvent::Began),
                ChromeEvent::Search(SearchEvent::Changed("is".into())),
                ChromeEvent::Search(SearchEvent::Submitted("is".into())),
                ChromeEvent::Search(SearchEvent::Submitted("is".into())),
            ]
        );
    }

    #[test]
    fn a_search_report_needs_a_live_search_item() {
        let (mut slot, generation, seen) = search_slot();
        slot.publish(Some(generation), spec());
        assert_eq!(slot.field(generation), None);
        assert!(!report_search(&mut slot, generation, SearchEvent::Began));
        assert!(
            slot.route_search(generation.get() + 1, SearchEvent::Began)
                .is_none()
        );
        // Coming back starts the field fresh.
        slot.publish(Some(generation), searching("", false));
        assert_eq!(slot.field(generation), Some(&FieldState::default()));
        assert!(report_search(&mut slot, generation, SearchEvent::Began));
        assert_eq!(seen.lock().unwrap().len(), 1);
        // The id-based path routes a search event the same way.
        let event = ChromeEvent::Search(SearchEvent::Changed("x".into()));
        let routed = slot
            .route_event(generation.get(), &event)
            .map(|(event, _)| event);
        assert_eq!(routed, Some(event));
    }

    #[test]
    fn the_process_wide_slot_delivers_a_search_report_by_kind_and_text() {
        let _guard = slot_test_guard();
        let generation = publish(None, searching("", false))
            .generation()
            .expect("installed");
        assert_eq!(live_search_generation(), Some(generation.get()));
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let sink_seen = Arc::clone(&seen);
        assert!(set_sink(
            generation,
            Arc::new(move |event| sink_seen.lock().unwrap().push(event))
        ));
        assert!(deliver_search(
            generation.get(),
            EVENT_KIND_CHROME_SEARCH_CHANGED,
            "\u{65e5}\u{672c}"
        ));
        assert!(
            !deliver_search(generation.get(), EVENT_KIND_CHROME_SEARCH_ENDED, ""),
            "not begun"
        );
        assert!(!deliver_search(
            generation.get(),
            EVENT_KIND_CHROME_BACK,
            ""
        ));
        assert_eq!(
            search_field(generation).map(|field| field.text),
            Some("\u{65e5}\u{672c}".to_string())
        );
        assert_eq!(
            *seen.lock().unwrap(),
            [ChromeEvent::Search(SearchEvent::Changed(
                "\u{65e5}\u{672c}".into()
            ))]
        );
        assert!(withdraw(generation));
        assert!(!deliver_search(
            generation.get(),
            EVENT_KIND_CHROME_SEARCH_BEGAN,
            ""
        ));
    }

    #[test]
    fn a_fallback_generation_tracks_its_field_too() {
        let mut slot = ChromeSlot::new(RecordingChromeHost::refusing(ChromeError::Refused(
            "theme".into(),
        )));
        let generation = slot
            .publish(None, searching("", false))
            .generation()
            .unwrap();
        assert!(slot.set_sink(generation, Arc::new(|_| {})));
        assert!(
            slot.route_search(generation.get(), SearchEvent::Changed("ab".into()))
                .is_some()
        );
        slot.publish(Some(generation), searching("", false));
        assert_eq!(
            slot.field(generation).map(|field| field.text.clone()),
            Some("ab".to_string()),
            "the frust-drawn field keeps the typing an app does not write back"
        );
        slot.publish(Some(generation), searching("preset", false));
        assert_eq!(
            slot.field(generation).map(|field| field.text.clone()),
            Some("preset".to_string())
        );
    }

    /// [`spec`] with a toolbar: a plain item, a menu item and a primary.
    fn with_toolbar() -> ChromeSpec {
        ChromeSpec {
            toolbar: Some(
                ToolbarSpec::new()
                    .item(ActionItem::new("info", "Info"))
                    .item(ActionItem::new("sort", "Sort").menu(vec![
                        MenuItem::new("new", "Newest"),
                        MenuItem::new("old", "Oldest").enabled(false),
                    ]))
                    .primary(ActionItem::new("comment", "Comment")),
            ),
            ..spec()
        }
    }

    fn slot() -> ChromeSlot<RecordingChromeHost> {
        ChromeSlot::new(RecordingChromeHost::new())
    }

    fn installed(slot: &mut ChromeSlot<RecordingChromeHost>) -> ChromeGeneration {
        let outcome = slot.publish(None, spec());
        slot.host_mut().take_calls();
        outcome.generation().expect("first publish installs")
    }

    #[test]
    fn the_first_publish_installs_with_a_nonzero_generation() {
        let mut slot = slot();
        let outcome = slot.publish(None, spec());
        let PublishOutcome::Installed(generation) = outcome else {
            panic!("expected Installed, got {outcome:?}");
        };
        assert_ne!(generation.get(), 0);
        assert_eq!(
            slot.host_mut().take_calls(),
            [HostCall::Install {
                generation,
                spec: spec()
            }]
        );
    }

    #[test]
    fn a_brightness_only_publish_is_an_applied_brightness_diff() {
        let mut slot = slot();
        let generation = installed(&mut slot);
        let mut next = spec();
        next.dark = !next.dark;
        assert_eq!(
            slot.publish(Some(generation), next.clone()),
            PublishOutcome::Applied(generation)
        );
        assert_eq!(
            slot.host_mut().take_calls(),
            [HostCall::Apply {
                changed: vec![ChromeField::Brightness],
                spec: next,
            }]
        );
    }

    #[test]
    fn a_diff_applies_only_the_changed_fields() {
        let mut slot = slot();
        let generation = installed(&mut slot);
        let mut next = spec();
        next.selected = Some(TabId::new("inbox"));
        next.top.as_mut().unwrap().title = "Inbox".into();
        next.scroll = Some(ScrollState {
            offset: 12.0,
            max_offset: 400.0,
            viewport: 600.0,
        });
        assert_eq!(
            slot.publish(Some(generation), next.clone()),
            PublishOutcome::Applied(generation)
        );
        assert_eq!(
            slot.host_mut().take_calls(),
            [HostCall::Apply {
                changed: vec![
                    ChromeField::Selected,
                    ChromeField::Title,
                    ChromeField::Scroll
                ],
                spec: next.clone(),
            }]
        );
        assert_eq!(slot.spec(), Some(&next), "the new spec is retained");
    }

    #[test]
    fn every_field_is_reported_on_its_own() {
        let base = spec();
        type Edit = (ChromeField, fn(&mut ChromeSpec));
        let edits: [Edit; 10] = [
            (ChromeField::Tabs, |s| s.tabs.truncate(1)),
            (ChromeField::Selected, |s| s.selected = None),
            (ChromeField::Title, |s| {
                s.top.as_mut().unwrap().title.push('!')
            }),
            (ChromeField::Subtitle, |s| {
                s.top.as_mut().unwrap().subtitle = Some("2 new".into());
            }),
            (ChromeField::Leading, |s| {
                s.top.as_mut().unwrap().leading = Leading::Symbol("person".into());
            }),
            (ChromeField::Actions, |s| {
                s.top.as_mut().unwrap().actions.pop();
            }),
            (ChromeField::LargeTitle, |s| {
                s.top.as_mut().unwrap().large_title = false;
            }),
            (ChromeField::Scroll, |s| {
                s.scroll = Some(ScrollState::default())
            }),
            (ChromeField::Brightness, |s| s.dark = !s.dark),
            (ChromeField::ActionBadges, |s| {
                let actions = &mut s.top.as_mut().unwrap().actions;
                actions[0] = actions[0].clone().badge("2");
            }),
        ];
        for (field, edit) in edits {
            let mut next = base.clone();
            edit(&mut next);
            let diff = ChromeDiff::between(&base, &next);
            assert_eq!(diff.changed, [field], "{field:?}");
            assert!(diff.contains(field));
        }
        // An empty bar appearing changes presence alone.
        let bare = ChromeSpec::default();
        let shown = ChromeSpec {
            top: Some(TopBarSpec::default()),
            ..ChromeSpec::default()
        };
        assert_eq!(
            ChromeDiff::between(&bare, &shown).changed,
            [ChromeField::TopBar]
        );
        assert!(ChromeDiff::between(&base, &base.clone()).is_empty());
    }

    #[test]
    fn a_toolbar_change_keeps_earlier_and_later_fields_in_declaration_order() {
        let base = spec();
        let mut new = with_toolbar();
        new.selected = Some(TabId::from("other"));
        new.dark = !new.dark;
        assert_eq!(
            diff_specs(&base, &new).changed,
            [
                ChromeField::Selected,
                ChromeField::Actions,
                ChromeField::Brightness
            ]
        );
    }

    #[test]
    fn a_toolbar_style_or_primary_label_change_is_an_actions_change() {
        let shown = with_toolbar();
        let restyle = |change: fn(ToolbarSpec) -> ToolbarSpec| {
            let mut new = shown.clone();
            new.toolbar = new.toolbar.take().map(change);
            diff_specs(&shown, &new).changed
        };
        assert_eq!(
            restyle(|t| t.style(ToolbarStyle::Floating)),
            [ChromeField::Actions],
            "a style change rebuilds the toolbar"
        );
        assert_eq!(
            restyle(|t| t.primary_label(true)),
            [ChromeField::Actions],
            "a primary-label change rebuilds the toolbar"
        );
        assert!(restyle(|t| t.style(ToolbarStyle::Buttons)).is_empty());
        // Beside a badge change it is still a rebuild, which carries the badge.
        let mut both = shown.clone();
        let mut toolbar = both.toolbar.take().unwrap().style(ToolbarStyle::Floating);
        toolbar.items[0] = toolbar.items[0].clone().badge("1");
        both.toolbar = Some(toolbar);
        assert_eq!(diff_specs(&shown, &both).changed, [ChromeField::Actions]);
    }

    #[test]
    fn a_toolbar_change_is_an_actions_change_in_declaration_order() {
        let base = spec();
        let shown = with_toolbar();
        assert_eq!(
            diff_specs(&base, &shown).changed,
            [ChromeField::Actions],
            "a toolbar appearing"
        );
        let mut both = shown.clone();
        both.top.as_mut().unwrap().title.push('!');
        both.dark = !both.dark;
        assert_eq!(
            diff_specs(&base, &both).changed,
            [
                ChromeField::Title,
                ChromeField::Actions,
                ChromeField::Brightness
            ]
        );
        let mut actions_too = shown.clone();
        actions_too.top.as_mut().unwrap().actions.pop();
        assert_eq!(
            diff_specs(&base, &actions_too).changed,
            [ChromeField::Actions],
            "reported once"
        );
        assert!(diff_specs(&shown, &with_toolbar()).is_empty());

        let mut slot = slot();
        let generation = installed(&mut slot);
        assert_eq!(
            slot.publish(Some(generation), shown.clone()),
            PublishOutcome::Applied(generation)
        );
        assert_eq!(
            slot.host_mut().take_calls(),
            [HostCall::Apply {
                changed: vec![ChromeField::Actions],
                spec: shown,
            }]
        );
    }

    #[test]
    fn a_badge_change_is_an_in_place_item_update_not_a_rebuild() {
        let base = with_toolbar();
        let badged = |spec: &ChromeSpec, badge: &str| {
            let mut next = spec.clone();
            let actions = &mut next.top.as_mut().unwrap().actions;
            actions[1] = actions[1].clone().badge(badge);
            next
        };
        let top = badged(&base, "3");
        assert_eq!(diff_specs(&base, &top).changed, [ChromeField::ActionBadges]);
        assert_eq!(
            diff_specs(&top, &badged(&base, "")).changed,
            [ChromeField::ActionBadges],
            "a count becoming a dot"
        );
        assert_eq!(
            diff_specs(&top, &base).changed,
            [ChromeField::ActionBadges],
            "a badge going away"
        );
        let mut toolbar = base.clone();
        let entries = toolbar.toolbar.as_mut().unwrap();
        entries.primary = entries.primary.take().map(|primary| primary.badge("9"));
        assert_eq!(
            diff_specs(&base, &toolbar).changed,
            [ChromeField::ActionBadges],
            "a toolbar entry's badge alone"
        );
        let mut both = toolbar.clone();
        both.top.as_mut().unwrap().title.push('!');
        both.dark = !both.dark;
        assert_eq!(
            diff_specs(&top, &both).changed,
            [
                ChromeField::Title,
                ChromeField::Brightness,
                ChromeField::ActionBadges
            ]
        );
        // Anything beyond a badge rebuilds, and the rebuild carries the badges.
        let mut prominent = top.clone();
        let actions = &mut prominent.top.as_mut().unwrap().actions;
        actions[0] = actions[0].clone().prominent(true);
        assert_eq!(
            diff_specs(&base, &prominent).changed,
            [ChromeField::Actions]
        );
        let mut reordered = badged(&base, "3");
        reordered.top.as_mut().unwrap().actions.reverse();
        assert_eq!(
            diff_specs(&base, &reordered).changed,
            [ChromeField::Actions]
        );
        let mut moved = toolbar.clone();
        let entries = moved.toolbar.as_mut().unwrap();
        let primary = entries.primary.take().unwrap();
        entries.items.push(primary);
        assert_eq!(
            diff_specs(&base, &moved).changed,
            [ChromeField::Actions],
            "a primary turned item is a rebuild"
        );
        let mut rebuilt_toolbar = top.clone();
        rebuilt_toolbar.toolbar = None;
        assert_eq!(
            diff_specs(&base, &rebuilt_toolbar).changed,
            [ChromeField::Actions],
            "a top-bar badge beside a toolbar rebuild is carried by the rebuild"
        );

        let mut slot = slot();
        let generation = slot.publish(None, base).generation().unwrap();
        slot.host_mut().take_calls();
        assert_eq!(
            slot.publish(Some(generation), top.clone()),
            PublishOutcome::Applied(generation)
        );
        assert_eq!(
            slot.host_mut().take_calls(),
            [HostCall::Apply {
                changed: vec![ChromeField::ActionBadges],
                spec: top.clone(),
            }]
        );
        assert_eq!(
            slot.publish(Some(generation), top),
            PublishOutcome::Unchanged(generation)
        );
    }

    #[test]
    fn badges_and_prominence_do_not_change_what_an_action_reports() {
        let mut spec = with_toolbar();
        let actions = &mut spec.top.as_mut().unwrap().actions;
        actions[0] = actions[0].clone().badge("").prominent(true);
        for event in [
            ChromeEvent::ActionTapped(ActionId::new("add")),
            ChromeEvent::MenuItemChosen(ActionId::new("more"), MenuItemId::new("share")),
        ] {
            let (kind, detail) = encode_event(&spec, &event).expect("encodable");
            assert_eq!(decode_event(&spec, kind, detail), Some(event));
        }
        let item = ActionItem::new("bell", "Bell");
        assert!(item.same_but_badge(&item.clone().badge("1")));
        assert!(!item.same_but_badge(&item.clone().prominent(true)));
        assert_eq!(item.badge, None);
        assert!(!item.prominent);
    }

    #[test]
    fn toolbar_entries_follow_the_top_bars_actions_in_one_index_space() {
        let spec = with_toolbar();
        assert_eq!(toolbar_offset(&spec), 2);
        let ids: Vec<&str> = chrome_actions(&spec).map(|a| a.id().as_str()).collect();
        assert_eq!(ids, ["add", "more", "info", "sort", "comment"]);
        for event in [
            ChromeEvent::ActionTapped(ActionId::new("info")),
            ChromeEvent::ActionTapped(ActionId::new("comment")),
            ChromeEvent::MenuItemChosen(ActionId::new("sort"), MenuItemId::new("new")),
        ] {
            let (kind, detail) = encode_event(&spec, &event).expect("encodable");
            assert_eq!(decode_event(&spec, kind, detail), Some(event.clone()));
        }
        assert_eq!(
            encode_event(&spec, &ChromeEvent::ActionTapped(ActionId::new("comment"))),
            Some((EVENT_KIND_CHROME_ACTION, 4))
        );
        assert_eq!(
            resolve_tap(&spec, ChromeTap::Action(2)),
            Some(ChromeEvent::ActionTapped(ActionId::new("info")))
        );
        assert_eq!(resolve_tap(&spec, ChromeTap::Menu(3, 1)), None, "disabled");
        assert_eq!(resolve_tap(&spec, ChromeTap::Action(5)), None);
        let bare = ChromeSpec {
            top: None,
            ..with_toolbar()
        };
        assert_eq!(toolbar_offset(&bare), 0);
        assert_eq!(
            resolve_tap(&bare, ChromeTap::Action(0)),
            Some(ChromeEvent::ActionTapped(ActionId::new("info")))
        );
    }

    #[test]
    fn an_empty_toolbar_shows_none() {
        assert_eq!(shown_toolbar(&spec()), None);
        let empty = ChromeSpec {
            toolbar: Some(ToolbarSpec::new()),
            ..spec()
        };
        assert_eq!(shown_toolbar(&empty), None);
        let primary = ToolbarSpec::new().primary(ActionItem::new("go", "Go"));
        assert!(!primary.is_empty());
        assert_eq!(
            primary
                .entries()
                .map(|a| a.id().as_str())
                .collect::<Vec<_>>(),
            ["go"]
        );
    }

    #[test]
    fn an_identical_publish_makes_no_host_call() {
        let mut slot = slot();
        let generation = installed(&mut slot);
        assert_eq!(
            slot.publish(Some(generation), spec()),
            PublishOutcome::Unchanged(generation)
        );
        assert_eq!(slot.host().calls().count(), 0);
    }

    #[test]
    fn icon_bytes_compare_by_content_not_identity() {
        let mut slot = slot();
        let generation = installed(&mut slot);
        let mut rebuilt = spec();
        rebuilt.tabs[1] = TabItem::new("inbox", "Inbox", TabIcon::Bytes(bytes(&[1, 2, 3])));
        rebuilt.top.as_mut().unwrap().leading = Leading::Back;
        assert_eq!(
            slot.publish(Some(generation), rebuilt.clone()),
            PublishOutcome::Unchanged(generation),
            "a fresh Arc with the same bytes is the same spec"
        );
        let a = Leading::Image(bytes(&[9, 9]));
        assert_eq!(a, Leading::Image(bytes(&[9, 9])));
        assert_ne!(a, Leading::Image(bytes(&[9, 8])));
        rebuilt.tabs[1] = TabItem::new("inbox", "Inbox", TabIcon::Bytes(bytes(&[1, 2, 4])));
        assert_eq!(
            slot.publish(Some(generation), rebuilt),
            PublishOutcome::Applied(generation)
        );
    }

    #[test]
    fn a_second_publisher_is_refused_busy() {
        let mut slot = slot();
        let generation = installed(&mut slot);
        assert_eq!(
            slot.publish(None, spec()),
            PublishOutcome::Refused(ChromeError::Busy)
        );
        let stale = ChromeGeneration(generation.get() + 7);
        assert_eq!(
            slot.publish(Some(stale), spec()),
            PublishOutcome::Refused(ChromeError::Busy)
        );
        assert!(!slot.withdraw(stale), "a refused scaffold cannot withdraw");
        assert_eq!(slot.generation(), Some(generation));
        assert_eq!(slot.host().calls().count(), 0);
    }

    #[test]
    fn a_withdrawn_generation_is_stale_and_only_a_fresh_publisher_installs() {
        let mut slot = slot();
        let first = installed(&mut slot);
        assert!(slot.withdraw(first));
        assert!(!slot.withdraw(first), "withdraw is once");
        assert_eq!(slot.generation(), None);
        slot.host_mut().take_calls();
        assert_eq!(slot.publish(Some(first), spec()), PublishOutcome::Stale);
        assert_eq!(
            slot.generation(),
            None,
            "a stale publisher installs nothing"
        );
        assert_eq!(slot.host().calls().count(), 0);
        let outcome = slot.publish(None, spec());
        let PublishOutcome::Installed(second) = outcome else {
            panic!("expected Installed, got {outcome:?}");
        };
        assert!(second > first, "generations increase");
        assert_eq!(
            slot.host_mut().take_calls(),
            [HostCall::Install {
                generation: second,
                spec: spec()
            }]
        );
    }

    #[test]
    fn a_no_host_install_is_retried_on_the_next_publish() {
        let mut slot = ChromeSlot::new(RecordingChromeHost::refusing(ChromeError::NoHost));
        let outcome = slot.publish(None, spec());
        let PublishOutcome::Fallback(generation, ChromeError::NoHost) = outcome else {
            panic!("expected Fallback, got {outcome:?}");
        };
        slot.host_mut().take_calls();
        let mut next = spec();
        next.selected = None;
        assert_eq!(
            slot.publish(Some(generation), next.clone()),
            PublishOutcome::Fallback(generation, ChromeError::NoHost)
        );
        assert_eq!(
            slot.spec(),
            Some(&next),
            "fallback chrome still reads the spec"
        );
        assert_eq!(
            slot.host_mut().take_calls(),
            [HostCall::Install {
                generation,
                spec: next.clone()
            }],
            "a NoHost install is retried under the same generation"
        );
        assert_eq!(
            slot.publish(None, spec()),
            PublishOutcome::Refused(ChromeError::Busy)
        );
        slot.host_mut().set_refusal(None);
        assert_eq!(
            slot.publish(Some(generation), spec()),
            PublishOutcome::Installed(generation)
        );
        assert_eq!(
            slot.publish(Some(generation), spec()),
            PublishOutcome::Unchanged(generation)
        );
        assert!(slot.withdraw(generation));
        assert_eq!(
            slot.host_mut().take_calls(),
            [
                HostCall::Install {
                    generation,
                    spec: spec()
                },
                HostCall::Dispose
            ]
        );
    }

    #[test]
    fn a_refused_install_pins_the_fallback_without_host_calls() {
        let refusal = ChromeError::Refused("window already re-rooted".into());
        let mut slot = ChromeSlot::new(RecordingChromeHost::refusing(refusal.clone()));
        let outcome = slot.publish(None, spec());
        let PublishOutcome::Fallback(generation, error) = outcome else {
            panic!("expected Fallback, got {outcome:?}");
        };
        assert_eq!(error, refusal);
        slot.host_mut().take_calls();
        slot.host_mut().set_refusal(None);
        let mut next = spec();
        next.selected = None;
        assert_eq!(
            slot.publish(Some(generation), next.clone()),
            PublishOutcome::Fallback(generation, refusal)
        );
        assert_eq!(slot.spec(), Some(&next));
        assert!(slot.withdraw(generation));
        assert_eq!(
            slot.host().calls().count(),
            0,
            "never retried, nothing installed, nothing disposed"
        );
    }

    #[test]
    fn a_deferred_no_host_failure_falls_back_then_retries() {
        let mut slot = slot();
        let generation = installed(&mut slot);
        slot.host_mut()
            .fail_deferred(generation, ChromeError::NoHost);
        let mut next = spec();
        next.selected = None;
        assert_eq!(
            slot.publish(Some(generation), next.clone()),
            PublishOutcome::Fallback(generation, ChromeError::NoHost)
        );
        assert_eq!(slot.spec(), Some(&next), "the fallback reads the new spec");
        assert_eq!(slot.host().calls().count(), 0, "no apply to a dead install");
        assert_eq!(
            slot.publish(Some(generation), spec()),
            PublishOutcome::Installed(generation),
            "the next publish retries the install"
        );
        assert_eq!(
            slot.host_mut().take_calls(),
            [HostCall::Install {
                generation,
                spec: spec()
            }]
        );
    }

    #[test]
    fn a_deferred_refusal_pins_the_fallback() {
        let mut slot = slot();
        let generation = installed(&mut slot);
        let refusal = ChromeError::Refused("a native chrome step panicked".into());
        slot.host_mut().fail_deferred(generation, refusal.clone());
        for _ in 0..2 {
            assert_eq!(
                slot.publish(Some(generation), spec()),
                PublishOutcome::Fallback(generation, refusal.clone())
            );
        }
        assert!(slot.withdraw(generation));
        assert_eq!(
            slot.host().calls().count(),
            0,
            "never retried, and the failed install is not disposed"
        );
    }

    #[test]
    fn a_failure_withdrawn_before_any_publish_is_not_disposed() {
        let mut slot = slot();
        let generation = installed(&mut slot);
        slot.host_mut()
            .fail_deferred(generation, ChromeError::NoHost);
        assert!(slot.withdraw(generation));
        assert_eq!(slot.host().calls().count(), 0);
    }

    #[test]
    fn an_older_generations_failure_leaves_a_newer_one_alone() {
        let mut slot = slot();
        let first = installed(&mut slot);
        assert!(slot.withdraw(first));
        let second = installed(&mut slot);
        slot.host_mut().fail_deferred(first, ChromeError::NoHost);
        assert_eq!(
            slot.publish(Some(second), spec()),
            PublishOutcome::Unchanged(second)
        );
        let mut next = spec();
        next.dark = !next.dark;
        assert_eq!(
            slot.publish(Some(second), next),
            PublishOutcome::Applied(second)
        );
        assert!(slot.withdraw(second));
        assert_eq!(
            slot.host_mut().take_calls().last(),
            Some(&HostCall::Dispose),
            "the newer install is still disposed"
        );
    }

    #[test]
    fn every_chrome_event_round_trips_the_codec() {
        let spec = spec();
        for event in [
            ChromeEvent::TabSelected(TabId::new("inbox")),
            ChromeEvent::TabReselected(TabId::new("home")),
            ChromeEvent::ActionTapped(ActionId::new("add")),
            ChromeEvent::ActionTapped(ActionId::new("more")),
            ChromeEvent::MenuItemChosen(ActionId::new("more"), MenuItemId::new("delete")),
            ChromeEvent::BackPressed,
        ] {
            let (kind, detail) = encode_event(&spec, &event).expect("encodable");
            assert_eq!(
                decode_event(&spec, kind, detail),
                Some(event.clone()),
                "{event:?}"
            );
        }
        assert_eq!(
            encode_event(
                &spec,
                &ChromeEvent::MenuItemChosen(ActionId::new("more"), MenuItemId::new("delete"))
            ),
            Some((EVENT_KIND_CHROME_MENU, 1001))
        );
    }

    #[test]
    fn an_undecodable_report_is_none() {
        let spec = spec();
        for (kind, detail) in [
            (EVENT_KIND_CHROME_TAB, 2),
            (EVENT_KIND_CHROME_ACTION, 2),
            (EVENT_KIND_CHROME_MENU, 2),
            (EVENT_KIND_CHROME_MENU, 1002),
            (EVENT_KIND_CHROME_BACK, 1),
            (crate::events::EVENT_KIND_CLICK, 0),
        ] {
            assert_eq!(decode_event(&spec, kind, detail), None, "{kind}/{detail}");
        }
        assert_eq!(
            encode_event(&spec, &ChromeEvent::TabSelected(TabId::new("nope"))),
            None
        );
    }

    #[test]
    fn a_disabled_tab_or_menu_item_decodes_to_none() {
        let mut spec = spec();
        spec.tabs[1] = spec.tabs[1].clone().enabled(false);
        let actions = &mut spec.top.as_mut().unwrap().actions;
        actions[1] = ActionItem::new("more", "More").menu(vec![
            MenuItem::new("share", "Share"),
            MenuItem::new("delete", "Delete").enabled(false),
        ]);
        let tab = pack_chrome_tab(1, false).expect("packable");
        assert_eq!(decode_event(&spec, EVENT_KIND_CHROME_TAB, tab), None);
        let delete = pack_chrome_menu(1, 1).expect("packable");
        assert_eq!(decode_event(&spec, EVENT_KIND_CHROME_MENU, delete), None);
        assert_eq!(
            decode_event(&spec, EVENT_KIND_CHROME_TAB, 0),
            Some(ChromeEvent::TabSelected(TabId::new("home"))),
            "the enabled siblings still decode"
        );
        assert_eq!(
            decode_event(&spec, EVENT_KIND_CHROME_MENU, 1000),
            Some(ChromeEvent::MenuItemChosen(
                ActionId::new("more"),
                MenuItemId::new("share")
            ))
        );
    }

    #[test]
    fn a_tap_resolves_against_the_confirmed_selection() {
        let spec = spec();
        let cases = [
            (
                ChromeTap::Tab(0),
                Some(ChromeEvent::TabReselected(TabId::new("home"))),
            ),
            (
                ChromeTap::Tab(1),
                Some(ChromeEvent::TabSelected(TabId::new("inbox"))),
            ),
            (ChromeTap::Tab(2), None),
            (
                ChromeTap::Action(0),
                Some(ChromeEvent::ActionTapped(ActionId::new("add"))),
            ),
            (ChromeTap::Action(2), None),
            (
                ChromeTap::Menu(1, 1),
                Some(ChromeEvent::MenuItemChosen(
                    ActionId::new("more"),
                    MenuItemId::new("delete"),
                )),
            ),
            (ChromeTap::Menu(0, 0), None),
            (ChromeTap::Menu(1, 1000), None),
            (ChromeTap::Back, Some(ChromeEvent::BackPressed)),
        ];
        for (tap, event) in cases {
            assert_eq!(resolve_tap(&spec, tap), event, "{tap:?}");
        }
        let mut unconfirmed = spec.clone();
        unconfirmed.selected = Some(TabId::new("gone"));
        assert_eq!(
            resolve_tap(&unconfirmed, ChromeTap::Tab(0)),
            Some(ChromeEvent::TabSelected(TabId::new("home"))),
            "no confirmed tab: every tap is a selection"
        );
        unconfirmed.top.as_mut().unwrap().leading = Leading::Symbol("person".into());
        assert_eq!(resolve_tap(&unconfirmed, ChromeTap::Back), None);
        assert_eq!(resolve_tap(&ChromeSpec::default(), ChromeTap::Back), None);
    }

    #[test]
    fn a_resolved_event_is_rerouted_by_id_against_the_live_spec() {
        let mut slot = slot();
        let generation = installed(&mut slot);
        assert!(slot.set_sink(generation, Arc::new(|_| {})));
        let shown = spec();
        let tapped = resolve_tap(&shown, ChromeTap::Action(0)).expect("resolved");
        let mut reordered = spec();
        reordered.top.as_mut().unwrap().actions.reverse();
        slot.publish(Some(generation), reordered.clone());
        let routed = slot
            .route_event(generation.get(), &tapped)
            .map(|(event, _)| event);
        assert_eq!(routed, Some(tapped.clone()), "the id survives the reorder");
        let mut gone = reordered;
        gone.top
            .as_mut()
            .unwrap()
            .actions
            .retain(|action| action.id().as_str() != "add");
        gone.tabs[1] = gone.tabs[1].clone().enabled(false);
        slot.publish(Some(generation), gone);
        assert!(slot.route_event(generation.get(), &tapped).is_none());
        let inbox = ChromeEvent::TabSelected(TabId::new("inbox"));
        assert!(
            slot.route_event(generation.get(), &inbox).is_none(),
            "disabled since"
        );
        assert!(
            slot.route_event(generation.get() + 1, &ChromeEvent::BackPressed)
                .is_none()
        );
    }

    #[test]
    fn routing_drops_a_withdrawn_generation_and_needs_a_sink() {
        let mut slot = slot();
        let generation = installed(&mut slot);
        assert!(
            slot.route(generation.get(), EVENT_KIND_CHROME_BACK, 0)
                .is_none(),
            "no sink"
        );
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let sink_seen = Arc::clone(&seen);
        assert!(slot.set_sink(
            generation,
            Arc::new(move |event| sink_seen.lock().unwrap().push(event))
        ));
        let (event, sink) = slot
            .route(
                generation.get(),
                EVENT_KIND_CHROME_TAB,
                pack_chrome_tab(1, false).expect("packable"),
            )
            .expect("routed");
        sink(event);
        assert_eq!(
            *seen.lock().unwrap(),
            [ChromeEvent::TabSelected(TabId::new("inbox"))]
        );
        assert!(
            slot.route(generation.get() + 1, EVENT_KIND_CHROME_BACK, 0)
                .is_none()
        );
        assert!(slot.withdraw(generation));
        assert!(
            slot.route(generation.get(), EVENT_KIND_CHROME_BACK, 0)
                .is_none()
        );
        assert!(!slot.set_sink(generation, Arc::new(|_| {})));
    }

    #[test]
    fn the_recording_log_is_bounded() {
        let mut slot = slot();
        let generation = installed(&mut slot);
        for offset in 0..(RECORDED_CALLS_CAP + 10) {
            let mut next = spec();
            next.scroll = Some(ScrollState {
                offset: offset as f64,
                ..ScrollState::default()
            });
            slot.publish(Some(generation), next);
        }
        assert_eq!(slot.host().calls().count(), RECORDED_CALLS_CAP);
    }

    #[test]
    fn the_process_wide_slot_delivers_outside_its_lock() {
        let _guard = slot_test_guard();
        let generation = publish(None, spec()).generation().expect("installed");
        assert_eq!(
            publish(None, spec()),
            PublishOutcome::Refused(ChromeError::Busy)
        );
        let republished = Arc::new(StdMutex::new(None));
        let seen = Arc::clone(&republished);
        assert!(set_sink(
            generation,
            Arc::new(move |event| {
                // A sink may publish again: deliver released the lock first.
                *seen.lock().unwrap() = Some((event, publish(Some(generation), spec())));
            })
        ));
        assert!(deliver(generation.get(), EVENT_KIND_CHROME_BACK, 0));
        assert_eq!(
            republished.lock().unwrap().take(),
            Some((
                ChromeEvent::BackPressed,
                PublishOutcome::Unchanged(generation)
            ))
        );
        assert!(deliver_event(generation.get(), &ChromeEvent::BackPressed));
        assert_eq!(
            republished.lock().unwrap().take().map(|(event, _)| event),
            Some(ChromeEvent::BackPressed)
        );
        assert!(withdraw(generation));
        assert!(!deliver(generation.get(), EVENT_KIND_CHROME_BACK, 0));
        assert!(!deliver_event(generation.get(), &ChromeEvent::BackPressed));
    }
}
