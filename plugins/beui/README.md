# frust-beui

The beUI design-system catalog for [Frust](https://frust.dev). It is a port of
beUI v2 (`github.com/starc007/ui-components`) onto Frust's `View`/`Widget`
tree: the vendored token tables folded onto the baseline `ColorScheme`, a
`BeuiTokens` extension, three glass recipes, catalog motion tokens, an overlay
hosting seam, and the component catalog, together with the bundled Geist and
Geist Mono fonts. An optional `gpu-effects` feature adds a GPU perspective
substrate for a few 3D component variants.

It is built on the `frust` facade's public `authoring` surface only. Applications
depend on `frust-beui` directly, next to `frust`; the facade does not re-export it.

Documentation and project home: <https://frust.dev>. Source: <https://github.com/frust-rs/frust>.

## Phones

beUI's own sizes are desktop-first. For a phone, install the touch density in
`frust::app!`'s `setup` block with
`frust_beui::install_with_density(frust_beui::Density::Touch)`, or build the
theme with `frust_beui::theme_with_density(Density::Touch)` and apply it
yourself (for example through `frust::set_app_theme`). `install()` and
`theme()` keep the comfortable sizes.

- **Which parts follow it.** The density is carried as a `BeuiMetrics` theme
  extension, and only the parts that read it change size: the components
  `alert`, `animated_badge`, `animated_toast_stack`, `button`, `card`, `chip`,
  `grouped_list`, `input` and `key_value`, and the agents `message`,
  `message_bubble` and `prompt_input`. Under `Touch` their controls keep a
  minimum target of 44 px on Apple targets and 48 px elsewhere, and their text
  steps grow. Every other part keeps its comfortable size.
- **Text scale.** beUI text follows the OS text-size setting
  (`Theme::text_scale`) in both densities; nothing changes where the scale is
  1.0.
- **A select inside a modal.** A `select` or `multi_select` inside a modal or
  a clipping container needs `.floating(true)`, which paints the option panel
  in the root overlay layer. While a floated panel is open it shows no row
  hover highlight, masks the app's shortcuts and closes on Tab.
- **Device-tested once.** The gallery's phone shell, the touch density and
  the text scale were walked on a Pixel 5 (Android 14) and an iPhone 13 mini
  (iOS 27.2); the checks, the results and the date are in the gallery's
  [README](https://github.com/frust-rs/frust/blob/main/examples/beui-demo/README.md)
  under "Device gates".

Limitations that matter on a phone, by their id in the repository's
[docs/LIMITATIONS.md](https://github.com/frust-rs/frust/blob/main/docs/LIMITATIONS.md):

- `beui-density-partial-coverage`: the parts outside the list above, the
  button's state icon and `theme_toggle` keep comfortable sizes.
- `beui-overlay-seam`: the default inline panel needs bounded constraints, so
  a select inside a modal needs `.floating(true)`.
- `overlay-portal-v1-scope` (item 3): what a floated panel costs.
- `beui-sidebar-no-mobile-drawer`: `animated_sidebar` has no phone drawer; a
  phone navigation layout is the app's.
- `beui-stack-modal-no-back-press`: a modal, drawer or sheet mounted in a
  `Stack` does not receive the system back press; wire a `frust::BackFallback`.

## License

The crate's SPDX expression is `(MIT OR Apache-2.0) AND OFL-1.1`: the Frust
sources are dual-licensed under [LICENSE-MIT](LICENSE-MIT) or
[LICENSE-APACHE](LICENSE-APACHE), and the bundled fonts are under the SIL Open
Font License 1.1.

Third-party material shipped in this package:

- beUI v2, MIT, Copyright (c) 2026 Saurabh Chauhan: [LICENSE-beui.md](LICENSE-beui.md).
- Geist and Geist Mono, OFL-1.1, Copyright 2024 The Geist Project Authors
  (<https://github.com/vercel/geist-font>): [fonts/geist/OFL.txt](fonts/geist/OFL.txt),
  [fonts/geist-mono/OFL.txt](fonts/geist-mono/OFL.txt).
