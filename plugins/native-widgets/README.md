# frust-native-widgets

Render **real OS controls** from pure Rust — `native_button("Save")`,
`native_label(...)`, `native_switch(checked)`, `native_slider(...)`,
`native_progress(...)`, `native_image(bytes)`, `native_spinner(animating)`,
`native_date_picker(date)`, `native_segmented(labels, selected)`,
`native_stepper(value, min, max)` (iOS/macOS only),
`native_tab_bar(items, selected)` (iOS/iPadOS only) — composed straight
into a frust `View` tree like any other widget. Each control is backed by a
genuine Android `View` (`Button`/`TextView`/`Switch`/`SeekBar`/`ProgressBar`/`ImageView`/a
circular-style `ProgressBar`/`DatePicker`), UIKit view (`UIButton`/`UILabel`/`UISwitch`/
`UISlider`/`UIProgressView`/`UIImageView`/`UIActivityIndicatorView`/`UIDatePicker`/
`UISegmentedControl`/`UIStepper`/a bare `UITabBar`), or
AppKit view on macOS (`NSButton`/`NSTextField`/`NSSwitch`/`NSSlider`/
`NSProgressIndicator`/`NSImageView`/an `NSProgressIndicator` in its
`Spinning` style/`NSDatePicker`/`NSSegmentedControl`/`NSStepper`), hosted as a **platform-view slot**
(`docs/ARCHITECTURE.md`'s Platform-view flow) — frust paints nothing for it,
the OS composites it in place.

Like every frust **platform plugin**, this crate is added to your app's own
`Cargo.toml` alongside `frust` (the pubspec model) — the `frust` facade does
not re-export it.

> **Templates stay clean.** A generated frust project ships **no**
> native-widgets code, permissions, or plist keys. You add exactly the lines
> below by hand — or let `frust plugin add native-widgets` (or the frust TUI's
> **Add Plugin** dialog) apply them for you; it automates every step in this
> document, including the Android Material 3 theme parent. On Android the plugin's
> Kotlin ships as its own Gradle library module rather than as files copied
> into your app, so there is nothing to keep in sync by hand; on iOS there is
> nothing to add at all beyond the dependency.

**Platform support:** Android, iOS and macOS. Other targets (desktop preview, web) have no native-control backend.

More about Frust: <https://frust.dev> and <https://github.com/frust-rs/frust>.

---

## 1. What you get

Eleven controls, one Rust API, no Kotlin or Swift to write for any of them:

| Builder | Android view | iOS view | macOS view |
|---|---|---|---|
| `native_button(text)` | `Button` | `UIButton` | `NSButton` |
| `native_label(text)` | `TextView` | `UILabel` | `NSTextField` (label) |
| `native_switch(checked)` | `Switch` | `UISwitch` | `NSSwitch` |
| `native_slider(value, min, max)` | `SeekBar` | `UISlider` | `NSSlider` |
| `native_progress(value, min, max)` | `ProgressBar` | `UIProgressView` | `NSProgressIndicator` |
| `native_image(bytes)` | `ImageView` | `UIImageView` | `NSImageView` |
| `native_spinner(animating)` | `ProgressBar` (circular style) | `UIActivityIndicatorView` | `NSProgressIndicator` (`Spinning` style) |
| `native_date_picker(date)` | `DatePicker` | `UIDatePicker` (date mode) | `NSDatePicker` (year/month/day) |
| `native_segmented(labels, selected)` | `MaterialButtonToggleGroup` (Material3 theme required) | `UISegmentedControl` | `NSSegmentedControl` (select-one) |
| `native_stepper(value, min, max)` | **none — renders a refusal banner** (see below) | `UIStepper` | `NSStepper` |
| `native_tab_bar(items, selected)` | **none — renders a refusal banner** (see below) | a bare `UITabBar` (no `UITabBarController`) | **none — renders a refusal banner** |

`native_segmented` runs on iOS/iPadOS, macOS and Android (Material's
`MaterialButtonToggleGroup`, equal-width single-line segments; it needs a
Material 3 app theme, which `frust plugin add` sets up (§3) — under a stock
theme construction is logged and the slot stays empty, never a crash; under a
Material 3 Expressive theme it draws separate pills with clipped labels). `native_stepper` is **iOS/iPadOS and macOS only**:
Android's framework has no increment/decrement stepper control, so there (and
on every desktop-preview host) the builder is resolved **at compile time** to
the same frust-drawn warning banner the translucency-refused fallback uses,
naming the missing arm, rather than an empty slot; a composite stepper Android
arm (two `ImageButton`s plus a `TextView`) is a follow-up plan.
`native_segmented` is controlled like `native_switch`:
`.on_select(|index| …)` reports the *requested* segment, and the app confirms
it by feeding `selected` back; `.momentary(true)` makes a tap flash its
segment instead of selecting it (no persistent check is ever shown). The selected segment wears the theme's
`accent_fill`. `native_stepper` is controlled like `native_slider`:
`.on_change(|value| …)` reports the *requested* value on a tap, and the app
confirms it by feeding `value` back; `.step(n)` sets the increment (default
`1`) and `.wraps(true)` wraps the value from `max` back to `min` instead of
clamping. The control wears the theme's `accent_ink` as its tint on iOS only
— `NSStepper` has no tint property AppKit exposes at all, so the fold is a
silent no-op there, logged at debug (same shape as the AppKit tint gaps in §4
below).

`native_tab_bar` is **iOS/iPadOS only**: macOS has no bottom-tab-bar idiom,
and the bare control has no Android arm (Android's bottom navigation ships
with `native_scaffold`, §1c) — both render the refusal banner, naming both
reasons. It is a
**bare** `UITabBar`, never a `UITabBarController`: frust keeps screen
ownership and routing, so a tap never navigates by itself. Each `TabItem`
carries a stable app-chosen `TabId`, a title, an icon — `TabIcon::AppleSymbol`
(an SF Symbol name), `TabIcon::Bytes` (encoded image bytes, normalized to a
25pt box; supply 75×75px for a crisp 3x icon) or
`TabIcon::apple_symbol_with_bytes(symbol, bytes)` (`TabIcon::AppleSymbolWithBytes`:
the symbol on Apple platforms, the bytes elsewhere; this bar shows the symbol)
— and optionally a
`.selected_icon(…)`, a `.badge(…)` and `.enabled(false)`. `.on_select(|id| …)`
reports the *requested* tab: route there and feed the id back as `selected`
(controlled, like `native_segmented`). `.on_reselect(|id| …)` fires when the
tab the app last confirmed was tapped again — the conventional "scroll to
top / pop to root" — not merely the one UIKit is highlighting: a tap you
reject (no write-back) retapped still reports as another `.on_select`, even
though the OS has already moved the highlight to it. The same tracking works
in reverse too: tap back on the tab the app still confirms and that retap
fires `.on_reselect`, even though the OS's own highlight had just moved to
the rejected tab a moment before, not to the one you tapped back — an app
that pops to root on `.on_reselect` should feed the confirmed selection back
promptly so its highlight and `selected` never keep diverging. The bar
sizes itself to 49pt plus the window's bottom safe-area inset,
so its background runs under the home indicator: put it last in a `Column`
docked to the bottom edge, and if you wrap it in `frust::safe_area` for
horizontal cutouts use `.top(false).bottom(false)` (the way `examples/huddle`
docks Material's `navigation_bar`); `.safe_area(false)` gives the bare 49pt
for a bar that isn't docked to the bottom. The selected item wears the
theme's `accent_ink`, unselected items its `on_surface_variant`, and the bar
its `surface` colour (an opaque appearance, set for both the standard and the
scroll-edge state so iOS 15+ never shows a transparent bar). On iPadOS the
bare bar stays at the bottom and gets no Liquid Glass — the iPadOS 18 top tab
bar and the floating glass bar are `UITabBarController` features.

```rust
// `Shell` is the app's `Component`; `ShellState` is its `State` (app code, not this crate).
use frust::{Get, RouteNavigator, RwSignal, Set, View};
use frust_native_widgets::{TabIcon, TabId, TabItem, native_tab_bar};

impl Shell {
    // The router owns navigation; the tab bar only reports the request.
    fn bottom_tabs(nav: RouteNavigator, tab: RwSignal<String>) -> impl View<ShellState> {
        native_tab_bar(
            vec![
                TabItem::new("home", "Home", TabIcon::AppleSymbol("house".into()))
                    .selected_icon(TabIcon::AppleSymbol("house.fill".into())),
                TabItem::new("inbox", "Inbox", TabIcon::AppleSymbol("tray".into())).badge("3"),
            ],
            TabId::new(tab.get()),
        )
        .on_select(move |id| {
            tab.set(id.to_string()); // feed the confirmed id back next rebuild
            nav.go(format!("/{id}")); // Router::go through its Send + Sync navigator
        })
        .on_reselect(|id| log::info!("scroll {id} to top"))
    }
}
```

`native_date_picker` picks a **date only** (no time) and is controlled like
`native_switch`: `.on_change(|date| …)` reports the *requested* `CivilDate`
(`{ year, month, day }`, month 1-based, validated — build one with
`CivilDate::new(2026, 9, 29)`), and the app confirms it by feeding it back
as `date`. `.min(date)`/`.max(date)` bound the range (a `date` outside it is
shown and reported clamped), and `.style(NativeDatePickerStyle::…)` picks the
presentation: `Compact` (default — iOS's compact button, a macOS text field
with a click-to-open calendar, Android's spinner mode), `Wheels` (iOS wheels,
the macOS text field and stepper, Android's spinner mode) or `Inline` (an
always-visible calendar on all three). Android fixes the mode when the picker
is created, so a later `.style(…)` change applies on iOS/macOS only (logged
once on Android). The picker wears the theme's `accent_ink` as its tint on iOS
and the theme's body-text colour as its text colour on macOS; Android's
`DatePicker` has no tint or text-colour API — its colours come only from the
(light/dark) theme it is built against — and `NSDatePicker` has no tint, so
those folds are silent no-ops, logged at debug.

…plus `native_component`, the generic mounting seam for a control this crate
does not ship: `impl NativeComponent` (with your own typed `Props`) →
`register_component::<C>(KIND)` → `native_component(KIND, c, props)`, mounted
through the same one factory and the same runtime the built-in builders use,
with no per-component Kotlin or Swift anywhere in it. See the `api::mount`
module docs.

A component hears its own views the way the built-in builders do: it attaches the
platform's one listener to any view it built with
`ComponentCtx::attach_listener(view, ListenerKinds::CLICK)` (or `TOGGLED` /
`VALUE_CHANGED`), its `NativeComponent::on_event` receives each event, and
whatever that answers reaches the app on the mounted view's `.on_event(...)`
hook — the same events-as-signals idiom as `.on_press`:

```rust
native_component(KIND, MyCard, props)
    .interactive()
    .on_event(move |event| if event.is_click() { taps.set(taps.get_untracked() + 1) })
```

> **One limit on that seam — read it before you plan around it.** An app
> crate cannot implement `NativeComponent` today (it needs raw
> `jni`/`objc2-ui-kit` dependencies this crate does not re-export). It is
> spelled out in §5, and does not apply to the built-in builders above.

Every control follows frust's **controlled-component** convention where the
platform allows it: a switch/slider reports the *requested* value through its
`on_...` callback, and the value you pass back down on the next rebuild is
what actually shows — the same contract `Checkbox`/`Slider` follow in
`frust-widgets` (`docs/CODE_STANDARDS.md`'s Interaction Semantics).

A native control's tap/toggle/drag bypasses `frust-core`'s `EventCtx`
entirely — it fires straight from the platform's own listener
(`View.OnClickListener` on Android, target-action on iOS) into a registered
Rust callback. That means none of `frust-core`'s interaction contract
(capture, focus, fire-on-up-inside, `Cancel`-never-mutates-state) applies to
a hosted native control — this is inherent to hosting real platform widgets,
not a gap.

```rust
// `Editor` is the app's `Component`; `EditorState` is its `State` (app code, not this crate).
use frust::{RwSignal, Set, View};
use frust_native_widgets::native_button;

impl Editor {
    // A native control's callback takes no app-state parameter — it fires
    // straight from the platform's own listener, so it closes over a signal
    // instead (the events-as-signals idiom every builder follows):
    fn save_button(saved: RwSignal<bool>) -> impl View<EditorState> {
        native_button("Save")
            .size(160.0, 48.0)
            .on_press(move || saved.set(true))
    }
}
```

`.style(NativeButtonStyle::…)` picks the button's platform style. A style
change on a live button restyles it in place, and the restyled button looks
like a fresh one in the new style:

| `NativeButtonStyle` | Android | iOS | macOS |
|---|---|---|---|
| `Plain` (default) | Material text button, themed by the app theme | system button, themed | push button, themed |
| `Prominent` | filled button | filled configuration | accent-tinted push button |
| `Destructive` | filled, error colour | filled, system red | red push button, destructive flag |
| `Glass` | tonal button | glass configuration (iOS 26; plain before) | push button |
| `ProminentGlass` | filled button | prominent glass (iOS 26; filled before) | accent-tinted push button |

Only `Plain` folds the app theme's accent into the fill and caption; the other
styles take the platform's colours. On Android the themed `Plain` button keeps
the Material ripple. The iOS configurations are looked up at run time, so an
older iOS gets the fallback named in the table. The iOS and macOS mappings
have not yet been run on a device.

```rust
native_button("Delete").style(NativeButtonStyle::Destructive)
```

---

## 1c. App chrome (`native_scaffold`)

`native_scaffold(body)` puts the host's own bars around your body: a top bar
(title, subtitle, leading item, actions with badges, pull-down menus, a large
title, a search field), a bottom tab bar and
an optional bottom toolbar, published as one spec to the platform host (or, on
a target with no chrome arm, to frust-drawn fallback bars). It is one declarative tree inside
`Component::build`. The scaffold resizes its body to avoid the keyboard by
default (`.resize_to_avoid_keyboard(false)` to opt out), so a bottom-pinned
composer sits on the keyboard; on Android the bottom navigation hides while the
keyboard is up:

```rust
use frust::{Component, Get, RwSignal, Set, View, text};
use frust_native_widgets::{
    ActionItem, Leading, TabIcon, TabId, TabItem, ToolbarSpec, native_scaffold,
};

#[derive(Default)]
struct Root;

struct RootState {
    tab: RwSignal<String>,
}

impl Root {
    fn body(tab: String) -> impl View<RootState> {
        text(format!("Showing {tab}"))
    }

    fn tabs() -> Vec<TabItem> {
        vec![
            TabItem::new("home", "Home", TabIcon::AppleSymbol("house".into())),
            TabItem::new("inbox", "Inbox", TabIcon::AppleSymbol("tray".into())),
        ]
    }
}

impl Component for Root {
    type State = RootState;

    fn init(&self) -> RootState {
        RootState { tab: RwSignal::new("home".to_string()) }
    }

    fn build(&self, state: &mut RootState) -> impl View<RootState> {
        let tab = state.tab;
        native_scaffold(Self::body(tab.get()))
            .tabs(Self::tabs(), tab.get())
            .title("Home")
            .leading(Leading::Back)
            .actions(vec![ActionItem::new("add", "Add")])
            .toolbar(ToolbarSpec::new().primary(ActionItem::new("comment", "Comment")))
            .on_tab_select(move |id: TabId| tab.set(id.to_string()))
    }
}

frust::app!(Root);
```

**You route, the bars report.** A tap reports (`.on_tab_select`,
`.on_tab_reselect`, `.on_action`, `.on_menu`, `.on_back`, or every event through
`.events_into(signal)`); you route, and the confirmed `selected` comes back as
the next build's props. Handlers take no `&mut State` — write a signal or drive
a `NavigatorController`. iOS tab selection is native and optimistic: UIKit
selects on the tap and the frust surface moves into the tapped page, then the
app has about 250 ms to confirm by publishing that tab; otherwise the bar snaps
back to the tab the spec names (chained taps converge on the spec). An
app-driven `selected` change is applied without animation.
`.scroll(controller)` feeds Android's lift-on-scroll app bar and the fallback
bars (changed values only); iOS does not minimize its tab bar on scroll. On iOS the bars float
over a full-bleed body, so put the safe area INSIDE the scroller
(`scroll_view(safe_area(page))`) for content to scroll under them; the scaffold
paints a static edge fade in the theme surface colour under each bar:
`.edge_fade(EdgeFade::default().color(..).bar_alpha(..))` tunes colour, edge and
bar-line alpha and tail, `EdgeFade::strong()` is a heavier preset and
`.no_edge_fade()` turns it off. On Android the Material 3 bars (`MaterialToolbar` in an `AppBarLayout`, a
`BottomNavigationView`) are siblings of the frust surface inside the Activity's
content view, which is padded to sit between them — the surface is never
re-parented — so do not pad for them yourself. The Material bars need a
Material activity theme, which `frust plugin add native-widgets` gives a
generated app (§3); an app whose theme is not Material (generated before that,
or reverted to a stock theme) gets the frust fallback bars instead, with the
fix logged once, and `frust doctor` names it. The same requirement covers the
Android sheet, segmented control and toolbar buttons.
A second live scaffold
publishes nothing and shows a one-line refusal above its body — one
`native_scaffold` per app.

**Toolbar:** `.toolbar(ToolbarSpec::new().items(..).primary(..))` adds a bottom
action strip — iOS navigation-controller toolbar items (the primary trailing,
prominent), Android floating action buttons (the primary a FAB 16dp above the
bottom navigation, the items stacked above it), a frust-drawn row above the
fallback tab row. Its taps report through `.on_action`/`.on_menu` like the top
bar's, so action ids must be unique across both: a toolbar entry repeating an
earlier id is dropped and a one-line banner says so. On iOS the tab bar hides
while a toolbar is shown (the toolbar takes the bottom spot, as in the GitHub
app), animated only for a push/pop inside a tab, and returns with the toolbar.
`.style(ToolbarStyle::Floating)` gathers the items into one floating pill with
the primary beside it: a Material 3 floating toolbar centred above the bottom
navigation on Android, hidden while the keyboard shows, and one pill in the
frust-drawn row. Its items sit in a single row with no overflow, so keep a
floating toolbar to a few short items on a phone. `.primary_label(true)` shows
an icon primary's title beside its icon (Android's extended floating action
button); an icon primary otherwise shows its icon alone, with the title as its
accessibility label. iOS ignores both settings and keeps its system toolbar.

```rust
// `comment_icon` is a `TabIcon` the app built.
ToolbarSpec::new()
    .style(ToolbarStyle::Floating)
    .items(vec![ActionItem::new("info", "Info")])
    .primary(ActionItem::new("comment", "Comment").icon(comment_icon))
    .primary_label(true)
```

**Top-bar items.** `ActionItem::icon(TabIcon)` gives an action an icon (its
title becomes the accessibility label), `.menu(items)` makes it a pull-down,
`.badge(text)` puts a badge on it (a count, `"new"`, or `""` for a bare dot; a
change of badge alone updates the shown item in place) and `.prominent(true)`
emphasises it. `Leading::Back` reports `.on_back`; `Leading::Image(bytes)` is
an avatar; `Leading::Icon(TabIcon)` is a plain, untappable icon; and
`Leading::Symbol(name)` is an SF Symbol. Phones have no shared icon
vocabulary, so give every icon as `TabIcon::apple_symbol_with_bytes(symbol,
bytes)`: iOS shows the symbol (the bytes for a name it does not know), Android
and the frust-drawn bars show the bytes. A symbol alone draws nothing on
Android (`native-chrome-android-bitmap-icons` in `docs/LIMITATIONS.md`).

**Large title.** `.large_title(true)` shows the iOS large title, and on Android
Material 3's medium collapsing top bar with the title and subtitle. On Android
the bar collapses and expands from the `.scroll(controller)` state, animated:
it snaps between the two heights rather than following the finger. The
frust-drawn bars ignore it.

**Search.** `.search(SearchSpec)` adds a search item to the top bar (it needs
one: set a title or another top-bar item) and `.on_search(..)` hears each
`SearchEvent`: `Began` (the field opened), `Changed(text)` (the whole text,
after every edit), `Submitted(text)` (the keyboard's search key) and `Ended`
(the field closed; a field that held text reports `Changed("")` first). The
same events also arrive as `ChromeEvent::Search` through `.events_into`. The
app owns the query: show the results in the body, and feed the text and the
open state back with `SearchSpec::text` and `SearchSpec::active`. The plugin
applies them only when they differ from what the field holds, so writing the
typed text back never disturbs the field, and flipping `active` opens or closes
it from the app. Focusing the native field ends a frust text field's focus, and
focusing a frust text field takes the keyboard back and leaves the search open.

```rust
// `query` and `searching` are app `RwSignal`s; `ChatState` is the app's `State`.
native_scaffold(results(query.get()))
    .title("Chats")
    .search(
        SearchSpec::new()
            .placeholder("Search chats")
            .text(query.get())
            .active(searching.get()),
    )
    .on_search(move |event| match event {
        SearchEvent::Began => searching.set(true),
        SearchEvent::Changed(text) => query.set(text),
        SearchEvent::Ended => searching.set(false),
        _ => {}
    })
```

| | iOS / iPadOS | Android | frust-drawn bars |
|---|---|---|---|
| Action badge | `UIBarButtonItem.badge`, iOS 26 and later, top bar only | Material badge, top bar and toolbar | a dot or a label |
| Prominent action | `UIBarButtonItemStyle::Prominent` | tinted with the theme's primary colour | a primary-coloured pill |
| Large title | the navigation bar's large title | medium collapsing bar, snaps on scroll | ignored |
| `ToolbarStyle::Floating`, `primary_label` | ignored | floating toolbar, extended primary | one pill; the label is ignored |
| Search field | a `UISearchController` on each page's navigation item | a search action that expands into a field across the top bar | a text-field row with a close button under the top bar |
| `SearchPlacement` | `NavigationBar` asks for the integrated placement (iOS 16 and later) | no effect | no effect |
| Search ends | cancel | back, or the field's collapse | the close button or Escape; a blur ends nothing |

There is no bottom search placement. Nothing in the iOS column has been run on
a device yet.

---

## 1b. Native presentations (alerts, sheets, toasts)

A native **alert** is not a control in the tree: it is a request answered by
exactly one outcome — the chosen action's id, `Cancelled`, `Dismissed` (you
took it down with `present::dismiss(&handle)`) or `HostLost` (the presenting
host went away first — the iOS/iPadOS window scene, the macOS presenting
window closing, or the Android hosting `Activity` being destroyed). Nothing
ever blocks waiting for the user.

**Two slots: one sheet, and one alert above it.** An alert requested while a
sheet is live is presented above that sheet (the confirmation a sheet row
asks for, below). When the sheet ends while its alert is still up, the alert
is taken down first and resolves `Cancelled`, then the sheet resolves its own
outcome. Every other overlap is refused `PresentError::Busy` at once, never
queued and never replacing the live one:

| Request | No presentation live | A sheet live | An alert live (with or without a sheet) |
|---|---|---|---|
| alert | shown | shown above the sheet | `Busy` |
| sheet | shown | `Busy` | `Busy` |
| toast | shown | shown | shown |

A toast takes no slot (§ Native toasts below). The alert over a sheet has
been run on an Android phone; on iOS it is not yet run on a device.

| Platform | Today |
|---|---|
| iOS / iPadOS | `UIAlertController` — `AlertStyle::Alert` centered, `AlertStyle::ActionSheet` from the bottom (iPhone) or as a popover pointing at `anchor` (iPad; required there, see below) |
| macOS | `NSAlert` presented as a window sheet on the key/main window — Return resolves the first action, Escape resolves the `Cancel`-role action; no click-away, so `Cancelled` is never produced; `style`/`anchor` ignored |
| Android | framework `android.app.AlertDialog` over the resumed `Activity` — the back key / an outside tap resolve `Cancelled`, only when `cancelable`; `style`/`anchor` ignored |
| desktop preview, web, every other target | `PresentError::Unsupported` |

Up to three actions, each with a unique non-empty `id` and a role —
`ActionRole::Default`, `Cancel` (at most one; UIKit places it itself) or
`Destructive`: iOS styles it red, macOS sets `NSButton.hasDestructiveAction`,
and Android has no severity styling for it — plain, sharing `Default`'s
button-slot preference. Two shapes:

```rust
use frust::{RwSignal, Set, spawn_local};
use frust_native_widgets::{
    ActionRole, AlertOutcome, AlertSpec, AlertStyle, AnchorRect, native_button, show_native_alert,
    show_native_alert_into,
};

fn delete_spec() -> AlertSpec {
    AlertSpec::new("Delete draft?", "This cannot be undone.")
        .with_action("keep", "Keep", ActionRole::Cancel)
        .with_action("delete", "Delete", ActionRole::Destructive)
}

// 1. Awaitable — from any async context (`spawn_local` on the UI thread);
//    `deleted` is an app `RwSignal<bool>`:
spawn_local(async move {
    if let Ok(AlertOutcome::Action(id)) = show_native_alert(delete_spec()).await
        && id == "delete"
    {
        deleted.set(true);
    }
});

// 2. Into a signal — no async block. Writes `Some(outcome)` exactly once;
//    you read it on a later rebuild and reset it to `None` yourself:
let outcome: RwSignal<Option<AlertOutcome>> = RwSignal::new(None);
native_button("Delete").on_press(move || {
    if let Err(err) = show_native_alert_into(delete_spec(), outcome) {
        log::warn!("no alert: {err}"); // Busy, InvalidSpec, Unsupported…
    }
})
```

`show_native_alert_into` returns synchronous refusals (`Busy`, an invalid
spec, `Unsupported`) as `Err` and never writes the signal for them; an error
found only later on the main thread (`NoHost`) is logged, not written — await
`show_native_alert` when you need to tell those apart. Dropping the awaited
future frees the alert slot but leaves the alert on screen until the user
answers it or a newer request takes it down (that answer is discarded).

**The iPad anchor rule.** An action sheet on iPad is a popover and must point
at something: `AlertStyle::ActionSheet` with no `anchor` is refused
`PresentError::InvalidSpec` on iPad before anything is shown (UIKit would
crash). Pass an `AnchorRect` in **logical window coordinates** — frust's
logical pixels are iOS points, so the rect a native control's
`platform_view` slot paints at, or any frust widget's window-space rect, is
already right; a zero-size rect points at a tap location. iPhone ignores the
anchor.

```rust
let mut spec = delete_spec();
spec.style = AlertStyle::ActionSheet;
spec.anchor = Some(AnchorRect { x: 24.0, y: 600.0, width: 120.0, height: 44.0 });
```

`cancelable` matters only where the platform has a non-action dismissal.
**iOS:** on iPad, `cancelable: false` stops an outside tap from closing the
action-sheet popover; UIKit alerts (and iPhone action sheets) have none, so
there it removes nothing. On an iPad popover with a `Cancel`-role action,
UIKit hides that button and an outside tap reports it (`Action("keep")`
above) rather than `Cancelled`. **Android:** `cancelable` gates both the back
key and an outside tap, either resolving `Cancelled`. **macOS:** no analog —
a window sheet has no click-away dismissal, so `cancelable` is ignored there.

### Native sheets (iOS / iPadOS / Android)

A **sheet** is the same kind of request: a `SheetSpec` answered by exactly
one `SheetOutcome` — `Action(id)` (the user tapped a row; the sheet has
already slid away), `Dismissed(DismissReason::User)` (swiped down),
`Dismissed(DismissReason::Programmatic)` (you called `handle.dismiss()`) or
`HostLost`. It holds the sheet slot: a second sheet, or a sheet while an alert
is up, is refused `PresentError::Busy`; an alert while it is up is shown above
it (the table above).

| Platform | Today |
|---|---|
| iOS / iPadOS | a page sheet under `UISheetPresentationController`: detents, grabber, swipe-to-dismiss; an alert over it is presented from the sheet's view controller |
| Android | Material `BottomSheetDialog` (Material 3 theme required, §3): detents map to half-expanded/expanded states, one half ratio at a time; `largest_undimmed` and `scrolling_expands` are ignored; an alert over it is an alert dialog above the bottom sheet dialog |
| macOS, desktop preview, web | `PresentError::Unsupported` (an `NSPopover` arm is follow-up work) |

The content is a **constrained native schema**, not frust widgets: an
optional title, an optional message, optional image bytes and up to three
action rows (system buttons — `Default` wears the tint, `Destructive` red,
`Cancel` the secondary label colour), stacked top to bottom. An empty sheet
is refused `InvalidSpec`, as are more than three actions or a
`Detent::Custom(f)` outside `0 < f <= 1`.

```rust
use frust::{RwSignal, Set, spawn_local};
use frust_native_widgets::{
    ActionRole, Detent, SheetContent, SheetOutcome, SheetSpec, native_button, show_native_sheet,
    show_native_sheet_into,
};

fn share_spec() -> SheetSpec {
    SheetSpec::new(
        SheetContent::new()
            .with_title("Share draft")
            .with_message("Anyone with the link can read it.")
            .with_action("copy", "Copy link", ActionRole::Default)
            .with_action("stop", "Stop sharing", ActionRole::Destructive),
    )
    .with_detents([Detent::Medium, Detent::Large])
    .with_theme(&theme) // tint + light/dark from the active frust theme
}

// Awaitable, with the handle taken first (moves it, or takes it down):
let presentation = show_native_sheet(share_spec().on_detent(move |detent| {
    expanded.set(detent == Detent::Large); // user drags stream here; not outcomes
}));
let handle = presentation.sheet_handle();
spawn_local(async move {
    if let Ok(SheetOutcome::Action(id)) = presentation.await {
        chosen.set(Some(id));
    }
});
// later: handle.map(|h| h.select_detent(Detent::Large));

// Into a signal — the same controlled contract as the alert form:
let outcome: RwSignal<Option<SheetOutcome>> = RwSignal::new(None);
native_button("Share").on_press(move || {
    if let Err(err) = show_native_sheet_into(share_spec(), outcome) {
        log::warn!("no sheet: {err}"); // Busy, InvalidSpec, Unsupported…
    }
})
```

`dismissible: false` (`.with_dismissible(false)`) stops the swipe-down, so
only a row or `handle.dismiss()` ends the sheet. `Detent::Custom` needs
iOS 16; on iOS 15 it becomes the nearest of medium/large (logged once).

**Event rows.** `SheetSpec::on_row_tap(id, listener)` turns the action row
`id` into an event: a tap calls `listener` on the UI thread and the sheet
stays up. The sheet still resolves exactly once, through another row, a
swipe-down, a dismissal or its host going away. A row that asks for
confirmation opens an alert from its listener, and the alert is shown above
the sheet:

```rust
// `stop_answer` is an app `RwSignal<Option<AlertOutcome>>`.
let spec = share_spec().on_row_tap("stop", move || {
    let confirm = AlertSpec::new("Stop sharing?", "The link stops working.")
        .with_action("keep", "Keep", ActionRole::Cancel)
        .with_action("stop", "Stop", ActionRole::Destructive);
    if let Err(err) = show_native_alert_into(confirm, stop_answer) {
        log::warn!("no confirmation: {err}");
    }
});
```

An `id` that names no action row is refused `InvalidSpec` when the sheet is
submitted.

**iPad in regular width ignores detents.** UIKit presents a page sheet in a
full-width (or large Split View) iPad window as a centered form sheet at a
fixed size — no medium height, no detent changes; only an edge-attached sheet
(iPhone, or an iPad window in compact width such as Slide Over) rests at its
detents. Design the content to read well at both sizes; never rely on detent
parity across idioms.

### Native toasts (Android / iOS / iPadOS)

A **toast** is a short message above the app's own native chrome, optionally
with one action: `show_native_toast(ToastSpec)` (awaitable) or
`show_native_toast_into(ToastSpec, signal)` (into an `RwSignal`, with the alert
form's contract). It resolves exactly one `ToastOutcome`: `TimedOut`,
`Action` (the user tapped its action) or `Dismissed` (swiped away, replaced by
a newer toast, taken down with `present::dismiss(&handle)`, or its host went
away). It takes neither slot: it is never refused `Busy` and never waits for
an alert or a sheet. A new toast replaces the current one, which resolves
`Dismissed`.

`ToastDuration::Short` is 1.5 s, `Long` 2.75 s, and `Custom(d)` is clamped to
1-10 s; Android may lengthen it for accessibility. An empty message or action
label is refused `InvalidSpec`.

```rust
use frust::{RwSignal, Set, spawn_local};
use frust_native_widgets::{ToastDuration, ToastOutcome, ToastSpec, show_native_toast};

// `restore` is an app `RwSignal<bool>`.
let spec = ToastSpec::new("Message archived")
    .with_action("Undo")
    .with_duration(ToastDuration::Long);
spawn_local(async move {
    if let Ok(ToastOutcome::Action) = show_native_toast(spec).await {
        restore.set(true);
    }
});
```

| Platform | Today |
|---|---|
| Android | a Material `Snackbar` above the plugin's bottom navigation or toolbar when one shows, else above the keyboard and the navigation bar; a toast shown while a sheet or an alert is up draws behind that dialog; `glass` ignored |
| iOS / iPadOS | a plugin-owned capsule above the tab and tool bars and the keyboard; `.with_glass(true)` asks for Liquid Glass on iOS 26 and a blur before; not yet run on a device |
| macOS, desktop preview, web | `PresentError::Unsupported` |

---

## 1d. What stays frust-drawn

This plugin wraps the native views listed above and nothing more. These stay
frust-drawn (`frust-widgets` or a design-system plugin) on every platform:

- **Text fields.** `text_input` is frust's own; only the scaffold's search
  field is native.
- **Context menus.** Only the top bar's and toolbar's pull-down menus
  (`ActionItem::menu`) are native.
- **Pull-to-refresh.**
- **Sheet content beyond the schema.** A native sheet shows a title, a
  message, an image and up to three rows; anything else is a frust-drawn
  sheet. Hosting frust-drawn content inside a native sheet has been explored,
  not shipped.

An app cannot add another native control from its own crate (§5, the
`NativeComponent` limit); request a missing control or option upstream.

---

## 2. Add the dependency (always)

```toml
# app Cargo.toml — [dependencies]
frust-native-widgets = "0.6"
```

`<frust>` is the path to your frust checkout — derive it from the `frust = {
path = "…" }` line the scaffold already wrote.

This is the **only** step iOS needs — see §4.

---

## 3. Android setup — the Gradle module and a Material 3 theme

**Two additions: this plugin's Android library module, and a Material 3
activity theme.** The module carries the plugin's Kotlin classes —
`dev.frust.nativewidgets`'s `FrustNativeControlFactory` (the ONE platform-view
factory every control resolves through), `FrustNativeListener` (the ONE
listener class every control's events funnel through), `FrustNativePresenter`
(alerts, sheets and toasts) and `FrustNativeChrome` (the app chrome) — plus
the R8 keep rules they need, in its own `consumer-rules.pro`. It depends on
Material Components (`api` scope), whose views (the chrome, the sheet, the
segmented control, the toolbar buttons) need a Material activity theme.
`proguard-rules.pro` stays untouched, and there is no Kotlin file to copy, so
nothing can drift.

`frust plugin add native-widgets` (or the TUI's Add Plugin dialog, `frust tui`
→ Add Plugin → native-widgets) makes all three edits below for you,
idempotently. By hand:

1. `android/settings.gradle.kts` — include the module, and redirect
   its build directory under your app:

   ```kotlin
   include(":frust-native-widgets")
   project(":frust-native-widgets").projectDir = frustLocalDir("frust.plugin.frust-native-widgets.dir")

   gradle.lifecycle.beforeProject {
       if (path == ":frust-native-widgets") {
           layout.buildDirectory.set(rootDir.resolve("../build/android/frust-native-widgets"))
       }
   }
   ```

   The module's directory is machine-local: its key sits in the gitignored
   `android/local.properties`, written by `frust run`/`frust build` from the
   crate cargo resolves, and read by the generated `frustLocalDir` helper.

2. `android/app/build.gradle.kts` — depend on it:

   ```kotlin
   dependencies {
       implementation(project(":frust-native-widgets"))
   }
   ```

3. `android/app/src/main/res/values/themes.xml` — give the app-owned
   `FrustAppTheme` style a Material 3 parent:

   ```xml
   <style name="FrustAppTheme" parent="Theme.Material3.DayNight.NoActionBar" />
   ```

   A generated app's `AndroidManifest.xml` names `@style/FrustAppTheme` on
   both its `<application>` and its `<activity>`, and the template ships the
   style with the platform parent `android:Theme.Material.NoActionBar`; the add
   rewrites only that `parent` attribute.

The theme step leaves an app alone when it is already fine: an activity theme
in the `Theme.Material3`, `Theme.Material3Expressive` or
`Theme.MaterialComponents` family needs nothing, and neither does a
`FrustAppTheme` whose parent is one of those or an app style in `themes.xml`
that leads to one. A parent style the add cannot follow (one defined outside
`themes.xml`) is trusted, not checked; if it is not Material after all, the
chrome falls back to the frust-drawn bars at run time.

A `FrustAppTheme` whose parent is an app style that ends in a platform theme
(`FrustAppTheme` → `Brand` → `android:Theme.Material.Light`) **is** rewritten:
its `parent` becomes `Theme.Material3.DayNight.NoActionBar`, which takes
`Brand` and its items out of the activity theme. Re-parent `Brand` onto a
Material 3 theme first to keep them.

**An app generated before the template defined `FrustAppTheme`** (its
activity names neither that style nor a Material theme) is refused before
anything is written, with the two manual edits named: add
`<style name="FrustAppTheme" parent="Theme.Material3.DayNight.NoActionBar" />`
inside `<resources>` in `android/app/src/main/res/values/themes.xml` (create
the file if it is missing), set `android:theme="@style/FrustAppTheme"` on both
the `<application>` and the `<activity>` element of
`android/app/src/main/AndroidManifest.xml`, then re-run
`frust plugin add native-widgets`. The add reads the activity theme by name: a
style of your own on the activity (`@style/MyTheme`) is refused the same way
even when it descends from Material 3, and `frust doctor` keeps reporting the
theme edit as pending, although the Material views work at run time. Keep the
activity on `@style/FrustAppTheme` and make your style its parent.

**Checking the wiring.** `frust plugin list` marks the plugin `<n> pending`
while an edit is missing, and inside the project `frust doctor` prints a
`Plugins` block naming it (informational; it never changes the exit code):

```text
Plugins
[!] native-widgets
    pending: Android theme `FrustAppTheme` parent `Theme.Material3.DayNight.NoActionBar`
    see limitation: native-chrome-android-material3-theme-required
    fix: frust plugin add native-widgets
```

A missing Gradle include shows as a `pending` row naming the Gradle module
`:frust-native-widgets`; for an app the add would refuse, `fix` lists the two
manual edits first.

No permission is required: hosting an `android.widget` view needs none.

**Both class names are hard contracts.**
`dev.frust.nativewidgets.FrustNativeControlFactory` is the platform-view
`viewType` string this crate publishes (the embedding's `FrustViewHost`
resolves it reflectively through the application classloader, which is also
why the package keeps its `dev.frust.` prefix), and both class names are
baked into this plugin's four JNI export symbols
(`Java_dev_frust_nativewidgets_*`). If the module isn't wired in, the host
cannot resolve the factory and every native control renders as an empty slot.

**Integrated this plugin before the module existed?** Both class names and all
four export symbols changed — see §7 before you rebuild.

---

## 4. iOS and macOS setup — nothing either

Neither Apple platform needs anything beyond §2's Cargo dependency: no Swift
package reference, no Xcode project edit, no `Info.plist` key, and on macOS
no explicit "install" call either.

### iOS — zero Swift, by design

The factory that would be a Swift class on every other plugin's Apple arm is
instead a Rust `objc2` `define_class!` type, registered straight into the
Objective-C runtime from this crate's own code and resolved by
`NSClassFromString` under the bare runtime name `FrustNativeControlFactory`
(`docs/CODE_STANDARDS.md`'s Naming Conventions: iOS factories carry no
package prefix — the opposite convention from Android's fully-qualified
FQCN). This has held for every control added since the first.

### macOS — the desktop Mode-A host registers the factory lazily

The desktop preview/bundle needs no app-side wiring either: this crate's
AppKit factory (a Rust `frust_plugin::desktop::DesktopViewFactory`) registers
itself with the shell's platform-view registry the first time any builder in
this crate encodes a slot's params — there is nothing for your app to call.

Two things differ from the mobile arms, both by design:

- **The theme ladder follows the app, not the Mac.** Every control's
  `NSAppearance` is pinned to the active `frust::Theme`'s brightness
  (`DarkAqua`/`Aqua`), re-applied on every update — so a Mac running Dark
  Mode still draws a Light-themed app's controls light, and vice versa.
- **A handful of AppKit gaps are logged, not papered over**: `Fit::Cover`
  has no true fill-and-crop on `NSImageView`, so it letterboxes like
  `Fit::Contain` rather than cropping; a slider emits no drag-start/drag-end
  event (AppKit target-action carries no gesture phase, only the final
  value); `NSSwitch`'s track and `NSProgressIndicator`'s fill have no tint
  API at all, so `thumbTint`/`trackTint`/`progressTint` on `native_switch`/
  `native_progress` — the spinner's own tint on `native_spinner`, which
  also draws through `NSProgressIndicator` — and `native_stepper`'s tint,
  since `NSStepper` exposes no tint property either — and `native_date_picker`'s
  tint, `NSDatePicker` having none (its calendar draws in the system accent
  colour) — are silent no-ops
  logged at debug (`NSSwitch`/`NSProgressIndicator` still draw in the system
  accent colour regardless; `NSStepper`'s own `-`/`+` glyphs simply keep
  their default appearance);
  `native_image`'s tint marks the image as a template and sets
  `NSImageView.contentTintColor` — a silhouette in the tint colour, like Android's SRC_IN and iOS's template rendering — and clearing
  the tint restores the original image; and `native_image` decodes through
  ImageIO, so a shell environment whose `DYLD_LIBRARY_PATH` shadows one of
  ImageIO's private codec dylibs (e.g. Homebrew's `/opt/homebrew/lib` on a
  machine with `libpng`/`libjpeg` installed) leaves every image slot empty
  with one logged warning rather than decoding or crashing.

---

## 5. Caveats

- **`frust create --overwrite` destroys these additions.** `--overwrite`
  re-renders the generated project wholesale, silently dropping the Gradle
  include and the module dependency and putting `FrustAppTheme`'s platform
  parent back. All three additions are **idempotent** — just re-run
  `frust plugin add native-widgets` (or the Add Plugin dialog, or re-apply §3)
  to restore them. The Kotlin classes themselves live in the plugin's own
  module, so they survive — but a project that no longer includes the module
  can't reach them.
- **A wrong or missing wiring fails at runtime, not at build time.**
  Nothing in the Rust build references the Kotlin classes, so a missing
  `include(...)` compiles cleanly and surfaces on device as a blank slot
  where the control should be (the host logs an unresolvable factory), and a
  stock activity theme surfaces as the frust-drawn fallback bars. A stale
  pre-module hand copy produces the same blank slot — see §7. Run
  `frust doctor` in the project: its `Plugins` block names a missing include
  or a stock theme before you reach a device (§3).
- **No cargo gate compiles Kotlin.** `cargo check --target
  aarch64-linux-android -p frust-native-widgets` type-checks the Rust half
  only. In this repository a separate CI lane, `scripts/ci/kotlin-compile.sh`
  (the `kotlin-compile` job, advisory and outside the merge gate), scaffolds a
  throwaway app, adds every built-in plugin with an Android module and
  compiles each module's Kotlin without building Rust; in your app a typo in
  the module's `.kt` files still surfaces only in a real Gradle build
  (`docs/DEVELOPMENT.md`). `plugins/native-widgets/tests/
  kotlin_conformance.rs` covers the one thing a source scan can: that the
  Kotlin listener's `KIND_*` constants and value-packing arithmetic still
  agree with `src/events.rs`'s.
- **Events bypass frust's event pipeline** — see §1. A native control is not
  reachable by frust's pointer capture/focus machinery, and a frust widget
  drawn over an interactive slot needs an explicit input `shield(...)`
  (`docs/ARCHITECTURE.md`'s Platform-view flow).
- **An app crate cannot implement `NativeComponent` today** (§1's
  `native_component` seam only). `create` has to construct real native views,
  which means naming `jni::objects::JObject` on Android and `objc2-ui-kit`'s
  classes on iOS *in your own crate*; this plugin re-exports neither FFI
  crate, and `docs/CODE_STANDARDS.md` sanctions a
  `frust-core`/`kurbo`/`peniko` escape hatch only for an `examples/*` app. So
  the practical audience today is **plugin authors, not app authors** — the
  only implementor in this repo is this crate's own non-default
  `demo-components` composite. Closing the gap (re-exporting a curated
  view-construction surface, or the FFI crates themselves) is a separate,
  unscheduled decision. The built-in builders in §1 are unaffected: they are
  ordinary Rust calls needing no FFI dependency of yours.
- **A `NativeComponent`'s events come only from listeners it attached.**
  `ComponentCtx::attach_listener` binds the platform's one listener class to
  the component's own slot id — which the context never hands the component —
  and the runtime's bridge delivers only the event families the slot attached,
  so a view the component attached nothing to still behaves natively (a
  button highlights) but reports nothing. Two children attached for the same
  family are indistinguishable in `on_event`: a click carries its slot, not
  which child fired, so give each child its own family or its own slot. On
  macOS an `NSControl` carries one target/action pair, so it takes exactly one
  family (`CLICK`, `TOGGLED` or `VALUE_CHANGED`), and AppKit reports no drag
  edges. The listener is released when the `ListenerHandle` the component
  keeps in its state drops.

---

## 6. `frust plugin add` automates all of this

Everything in §2 and §3 — the Cargo.toml dependency, the
`:frust-native-widgets` Gradle include plus its app-module dependency, and the
`FrustAppTheme` parent — is applied for you, idempotently, by
`frust plugin add native-widgets` or the frust TUI's **Add Plugin** dialog
(select `native-widgets`). This README is the manual contract both encode.

---

## 7. Migrating from the v1 hand copy (breaking, one time)

Before this plugin shipped its own Gradle module, its two generic Kotlin
classes lived in the **bare `dev.frust` package** and the documented
integration was to **hand-copy** `FrustNativeControlFactory.kt` and
`FrustNativeListener.kt` into your own app module, under
`app/src/main/kotlin/dev/frust/`. That shape is gone, and the rename it
required is breaking:

| | v1 (hand copy) | now |
|---|---|---|
| Kotlin package | `dev.frust` | `dev.frust.nativewidgets` |
| factory `viewType` | `dev.frust.FrustNativeControlFactory` | `dev.frust.nativewidgets.FrustNativeControlFactory` |
| JNI export symbols | `Java_dev_frust_FrustNative*` | `Java_dev_frust_nativewidgets_FrustNative*` |
| how the Kotlin ships | copied into your app module | this plugin's own Gradle library module (§3) |

**Why.** The bare `dev.frust` package belongs exclusively to the embedding
module (`docs/CODE_STANDARDS.md`'s Plugin Conventions); this plugin held a
time-boxed exception only because a Kotlin package is baked verbatim into a
JNI export's mangled symbol name. Shipping a real Gradle library module — the
shape every other plugin already uses — ended the exception, and moving the
package *had* to move all four export symbols with it.

**What to do**, in this order:

1. **Delete your copies** of `FrustNativeControlFactory.kt` and
   `FrustNativeListener.kt` (plus the `dev/frust/` directory they sat in, if
   nothing else of yours lives there). They are dead weight now, and worse
   than dead — see the symptom below.
2. **Apply §3's module wiring**: the `include(":frust-native-widgets")` +
   `projectDir` lines in `android/settings.gradle.kts` and the
   `implementation(project(":frust-native-widgets"))` line in
   `android/app/build.gradle.kts`. The TUI's Add Plugin dialog makes both edits
   idempotently.
3. Rebuild. Nothing of yours ever referenced either class by name, so there is
   nothing else to change — no manifest edit, and no keep rule to add (the
   module ships its own `consumer-rules.pro` for the new names). A keep rule
   *you* added for the old `dev.frust.FrustNative*` classes can go too; it
   matches nothing once the copies are deleted.

iOS is unaffected: there was never anything to copy there (§4).

**The symptom if you skip this.** Nothing fails at build time — Kotlin happily
compiles an `external fun` whose native symbol does not exist, and no cargo
gate compiles Kotlin at all (§5) — so it lands on device the first time a
native control is created, and it looks like **a blank slot with no obvious
cause**:

- **Copies kept, module not wired:** the host resolves the factory by the new
  FQCN, your app only has the old `dev.frust.` one, so `Class.forName` fails —
  `logcat` (tag `frust`) shows `platform-view factory
  'dev.frust.nativewidgets.FrustNativeControlFactory' failed to resolve` and
  the slot is marked dead. Every native control is an empty hole.
- **Anything that still reaches a stale copy** dies with `UnsatisfiedLinkError:
  No implementation found for … Java_dev_frust_FrustNativeControlFactory_nativeCreateControl`:
  the JVM resolves a `native`/`external` method by mangled name alone, and that
  name is no longer in the `.so`. The host wraps every factory call in `catch
  (Throwable)`, so this too becomes a dead slot plus one `logcat` warning
  rather than a crash — which is exactly why the cause is not obvious from the
  screen.

Both failures are silent by design (a misbehaving platform-view factory must
never take down the frame loop), so `adb logcat -s frust` is the place to
confirm which one you have.

## License

Licensed under either of MIT or Apache-2.0 (SPDX: `MIT OR Apache-2.0`), at your
option. See `LICENSE-MIT` and `LICENSE-APACHE` beside this README.
