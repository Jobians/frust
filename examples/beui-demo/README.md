# beui-demo

The `frust-beui` design-system plugin's gallery: twenty pages that show
every ported component interactively — a home page with the upstream
attribution, a Content page for the phone-shaped parts, nine Motion pages (text effects, buttons, controls, selection,
overlays, navigation, surfaces, data, and the WGSL shader backgrounds), three
Agents pages (chat primitives, agent-output panels, the assembled chat app
over a mock streaming driver), four Blocks pages (command surfaces, morphing
tabs and stacks, forms and upload, showcase blocks), a Theming page, and a
GPU Effects page.

A **standalone workspace** (its own `[workspace]` root and `Cargo.lock`,
excluded from the root one — see `Cargo.toml`'s header), shaped like
`examples/material3-demo`: one tree drives the desktop preview and the
generated `android/` and `ios/` host projects. The package is `beuidemo` (no
hyphen) even though the directory keeps one: the crate name is the `.so`/`.a`
leaf the Android and iOS shells load by identifier. The manifest adds a
single non-default `gpu-effects` feature row (see "GPU Effects" below)
forwarding to `frust-beui/gpu-effects`; the default build carries none of it.

## Run

```bash
cd examples/beui-demo
cargo run                # desktop preview
frust run -d <device>    # Android / iOS device or simulator
```

The desktop preview opens a real winit window titled "beUI Gallery".
`frust_beui::install_with_density` runs in `frust::app!`'s `setup` block, so
the app starts themed with beUI's own token tables rather than the
framework's neutral fallback (see "Phones" below for the density). The
window opens at the desktop shell's own default size (800×600 logical px);
set `FRUST_WINDOW_SIZE=<width>x<height>` before running for a roomier
gallery view of the sidebar-plus-content shell, or a width under 720 for the
phone shell.

It gates from its own directory rather than with `-p` from the repo root:

```bash
cd examples/beui-demo
cargo test \
  && cargo clippy --all-targets -- -D warnings \
  && cargo clippy --features gpu-effects --all-targets -- -D warnings \
  && cargo fmt --check
```

## Nav shell

This section is the wide shell, at a layout width of 720 logical px or more;
"Phones" below covers the narrower one. The rail is `frust_beui::components::animated_sidebar` — the catalog's own
motion-driven sidebar, collapsible via the top bar's
Collapse/Expand button. `animated_sidebar` ports upstream's panel-and-menu,
not a labelled-group header slot (see that module's own doc comment), so this
scaffold keeps the rail as one flat, springing list and folds each grouped
page's section into its own label (`Motion · Text`, `Agents · Chat`, ...)
rather than inventing a group-header widget here. `nav.rs`'s module docs carry
the same note; the upgrade to a genuinely grouped rail is future work for
whichever task next touches `animated_sidebar` itself.

Twenty pages, in nav order:

- **Home** — title, upstream attribution (MIT, rev
  `10c283e433a8f4f0ac0736684d4426ab612b9f55`), and a category-overview card
  per section, each linking straight into that section's first page.
- **Content** — the parts a phone screen is built from, in one column at most
  480 logical px wide and centred on a wider window: a hero card over a 2×2
  grid of stat tiles, a grouped list (avatar rows, a pressable counter row, a
  destructive row), chips and every status-badge tone, avatars in three sizes,
  every alert tone (a banner and a dismissible one among them), key-value rows
  and a three-step timeline, every button tone and size, a text field, a
  floating select and a toast trigger. Two chips at the top switch the
  density between Comfortable and Touch (`frust_beui::theme_with_density`,
  pushed through `frust::set_app_theme` with the current brightness kept);
  the light/dark toggle keeps the chosen density.
- **Motion** (9 pages: `text`, `buttons`, `controls`, `selection`, `overlays`,
  `navigation`, `surfaces`, `data`, `shader`) — the 41 motion components with
  every registry variant, plus the five `shader_background` variants on the
  engine's `ShaderQuad` seam.
- **Agents** (3 pages: `primitives`, `panels`, `chat`) — the 17 agent
  components, headlined by the chat app over its mock streaming driver.
- **Blocks** (4 pages: `command`, `morph`, `forms`, `showcase`) — the 22
  blocks, each with the port's premise corrections in its caption.
- **Theming** — the beUI palette across both brightnesses (`BEUI_LIGHT`,
  `BEUI_DARK`), a live light/dark toggle via `frust_beui`'s own
  [`theme_toggle`](../../plugins/beui/src/components/theme_toggle.rs), and a
  Geist/Geist Mono typography specimen. The motion-token visualizer is a
  stub — it names `frust_beui::tokens::motion`'s curve/spring surface without
  wiring a live timeline against it yet.
- **GPU Effects** — the non-default `gpu-effects` feature, live: five 2D/3D
  pairs (`tilt_card`, `wheel_picker`, `cylinder_carousel`, `project_folder`,
  `wallet_card`), each the same component twice — its ordinary 2D path next
  to the true-3D opt-in the feature adds — plus a three-tile
  `shader_background` showcase (unconditional; not behind this feature). See
  "GPU Effects" below.

Content swaps under `frust::motion::switcher::pattern_switcher` with the
`FadeThrough` pattern — non-directional, the idiomatic choice for a
destination change with no spatial relationship. The top bar carries a second,
persistent copy of `theme_toggle` (reachable from every page, not just
Theming) alongside the active page's title.

## Theming today vs. TODO

- **Wired today:** `frust_beui::install_with_density` seeds the beUI theme
  as the app default; every `theme_toggle` instance (top bar, Theming and Content pages)
  flips brightness live through `frust::set_app_theme`, keeping the density
  the Content page's chips chose; every swatch, button, and
  text run on Home/Theming resolves through beUI's own tokens
  (`frust_beui::palette`/`sans_family`/`mono_family`) rather than a literal
  color authored in this crate.
- **TODO:** the motion-token visualizer (Theming page) is a named stub, not a
  live timeline; the nav rail is a flat list rather than a grouped/labelled
  one (see "Nav shell" above); every section page hosts its state in a
  page-local component, so a page starts fresh each time it is opened.

## Phones

The shell follows the layout width, not the target OS: below 720 logical px
(`nav::COMPACT_BREAKPOINT`) a phone shell replaces the rail, so a narrow
desktop window shows it too.

- **Top bar:** a Menu button, the page title and the light/dark toggle, each
  control at least the touch minimum (44 px on Apple targets, 48 px
  elsewhere) in either density.
- **Page list:** the Menu button opens every page in a left `drawer`, grouped
  under the Motion, Agents and Blocks headers as beUI list rows. Choosing a
  page shows it and closes the list; a backdrop tap, Escape and the Android
  back press close it too. The drawer is mounted in the shell's stack, so the
  back press reaches it through a `frust::BackFallback` that claims the press
  only while the list is open; with the list closed, back leaves the app as
  usual.
- **Safe area and keyboard:** the whole shell sits in a `frust::safe_area`
  with `.keyboard(true)`, so the top bar starts below the status bar or notch
  and the page ends above the home indicator, or above the soft keyboard
  while it is open. The page's scroll view then keeps a focused field's caret
  in view, which is what keeps the Chat page's composer and the Content
  page's text field above the keyboard.
- **Density:** Android and iOS start under `Density::Touch`
  (`frust_beui::install_with_density`), every other target under
  `Density::Comfortable`. The Content page's density chips read the live
  theme rather than a state of their own, so they open on whichever density
  the app started with.

Fifteen of the twenty pages still lay out wider than a 390 px phone in
places (fixed-width stages, rows that do not wrap, wide specimens); the
shell's own test records which ones. Content, Agents · Primitives,
Agents · Panels, Blocks · Morph and Theming fit.

## Device gates

Walks of this gallery on real phones, in light and dark. A row is a single
run on the build named below, not a standing guarantee.

2026-11-16, Android: Pixel 5, Android 14, 1080×2340 at 440 dpi, 90 Hz;
`frust build apk --profile --target-platform android-arm64` at `4f82ba9a`.

| Check | Device and OS | Result | Note |
|-------|---------------|--------|------|
| 1. Touch targets on Content | Pixel 5, Android 14 | PASS | Small button 66×48 dp, list row 56 dp tall, other buttons 48 to 56 dp. Below 48 dp: the page's inline theme toggle (32 dp) and the select (40 dp). |
| 2. OS font size, default and 1.3 | Pixel 5, Android 14 | PASS | beUI text grows and rows wrap, nothing clips or overlaps; button labels fit. The gallery's own headings, page descriptions and top-bar title keep their size. |
| 3. Status and navigation bars | Pixel 5, Android 14 | PASS | Portrait and landscape; landscape is wide enough for the rail shell and clears the display cutout. |
| 4. Keyboard | Pixel 5, Android 14 | FAIL | The Content field stays visible while typing. The Chat composer is revealed only to its caret line: the send button sits under the keyboard until the page is scrolled, and the composer can be scrolled back under it. |
| 5. Toast | Pixel 5, Android 14 | PASS | Appears above the content and the navigation bar; closes from its button. It has no timeout of its own. |
| 6. Scrolling Content | Pixel 5, Android 14 | PASS | Trace over six flings: frame time p50 6 to 11 ms, p95 13 ms, no skipped frame; scene encode 2 to 3.5 ms. |
| 7. Log | Pixel 5, Android 14 | PASS | No panic, no crash and no GPU-effects warning; one process for the whole walk. |
| 8. Navigation | Pixel 5, Android 14 | PASS | All 20 pages open. Back closes the page list; with the list closed it leaves the app from any page. Pages are wider than the phone in places, as listed under "Phones". |
| 9. Draft dialog on Overlays | Pixel 5, Android 14 | PASS | Veto on: back keeps the dialog. Veto off: back closes it with its exit animation. Focus return on: focus goes back to Composer (the keyboard reopens after Discard, not after back). Off: neither returns. |

2026-11-16, iOS: iPhone 13 mini, iOS 27.2, 375×812 pt at 3×, 60 Hz;
`frust build ios --profile` at `4f82ba9a`, driven by an XCUITest runner. The
generated Xcode project built and signed unchanged. `cargo check --target
aarch64-apple-ios` and `--target aarch64-apple-ios-sim` both pass.

| Check | Device and OS | Result | Note |
|-------|---------------|--------|------|
| 1. Touch targets on Content | iPhone 13 mini, iOS 27.2 | PASS | Small button 66×44 pt, list row 58 pt tall, other buttons 44 to 52 pt. Below 44 pt: the page's inline theme toggle (32 pt) and the select (40 pt). |
| 2. Text size, default and largest standard | iPhone 13 mini, iOS 27.2 | PASS | Largest standard size set with the content-size launch argument. beUI text grows and rows wrap, nothing overlaps; three labels end in an ellipsis (the card's date line, the "Orders" tile label, a list row's email). The gallery's own headings and descriptions keep their size. |
| 3. Notch and home indicator | iPhone 13 mini, iOS 27.2 | PASS | Portrait and landscape; landscape uses the rail shell. |
| 4. Keyboard | iPhone 13 mini, iOS 27.2 | FAIL | Same as Android: the Content field stays visible, the Chat composer is revealed only to its caret line and its send button sits under the keyboard until the page is scrolled. |
| 5. Toast | iPhone 13 mini, iOS 27.2 | PASS | Appears above the content and the home indicator; closes from its button. |
| 6. Scrolling Content | iPhone 13 mini, iOS 27.2 | PASS | Trace over eight flings: frames paced at the 60 Hz period (p95 17 to 18 ms including the wait for the drawable), no skipped frame; scene encode p95 2 ms. |
| 7. Log | iPhone 13 mini, iOS 27.2 | PASS | Device console: no panic and no GPU-effects warning. |
| 8. Navigation | iPhone 13 mini, iOS 27.2 | PASS | All 20 pages open; the page list closes on a backdrop tap and on choosing a page. |
| 9. Draft dialog on Overlays | iPhone 13 mini, iOS 27.2 | PASS | Veto on: a backdrop tap keeps the dialog. Veto off: it closes. Focus return on: focus and the keyboard go back to Composer. Off: neither returns. |

## GPU Effects

```bash
cd examples/beui-demo
cargo run --features gpu-effects
```

Compiles the catalog's `gpu_fx` substrate in and turns on the five true-3D
opt-ins the GPU Effects page pairs against their 2D path
(`TiltCardView::gpu_face`, `WalletCardView::gpu_fan`, `WheelPicker::gpu_drum`,
`CylinderCarouselView::gpu_cylinder`, `ProjectFolderView::gpu_fan`). The
default build (`cargo run`, no flag) never compiles `gpu_fx` at
all — the page still shows, with a placeholder in every 3D pane naming this
command.

Compiling the feature in is not the same as rendering 3D: a device is only
reachable once the desktop shell has opened its first surface (beUI keeps the
3D path off on Android and iOS), and `FRUST_BEUI_NO_GPU_FX=1` turns the whole path back off
regardless of the build. Every component already carries that degrade in its
own contract (so a phone, where beUI keeps the 3D path off, renders every
variant's 2D path), so a `--features gpu-effects` build with no reachable device
renders exactly the default build's 2D panes — the GPU Effects page states
which of the three cases the current run is in.

The `gpu_fx` substrate itself is GPU-verified on Vulkan against a T400
(`plugins/beui`'s own `#[ignore]`d cases — see that crate's `gpu_fx` module
docs). This page's own run is owed: the dockur Linux desktop rig this gallery
is otherwise verified on (`run-on-desktop.sh`, which runs the release binary
headless and captures a screenshot) has not yet run the `--features
gpu-effects` build. Metal is expected to behave the same once a Mac runs it —
the substrate reaches the GPU only through `frust::gpu`, which carries no
backend-specific code of its own — but that run is owed too.

## Verifying it

This dev rig is headless — the binary is never run here. `cargo build` (from
this directory) is the runtime check, `cargo check --features gpu-effects`
covers the feature-gated build compiling, and `cargo ndk -t arm64-v8a check`
covers the Android compile; a desktop container (dockur linux-native,
`run-on-desktop.sh`) runs `cargo run` (and, for the GPU Effects page,
`cargo run --features gpu-effects`) and captures the screenshot separately.
