# frust-glyph

The Glyph design-system catalog for [Frust](https://frust.dev): a terminal-native,
dark-first, monospace-led design language. The crate holds the Glyph tokens
(color schemes, type, shape, elevation, motion and glass scales), the assembled
theme, two motion patterns, and the widget catalog (badges, alerts, loaders,
toast, navigation chrome, page dots (`page_dots`), cards, terminal block, command palette, a floating
action button (`fab`), a card-switcher skin with its action bar and chips (`card_switcher`,
`switcher_bar`, `switcher_chip`), and its own `toggle` switch). The bundled monospace fonts are behind
the default-on `bundled-fonts` feature.

It is built on the `frust` facade's public `authoring` surface only. Applications
depend on `frust-glyph` directly, next to `frust`; the facade does not re-export it.

## Chat components (`chat` feature)

The opt-in `chat` feature adds `frust_glyph::chat`, a family for agent-conversation UIs. It is
off by default: a default build compiles none of it and gains no dependency. Enable it with
`frust-glyph = { version = "..", features = ["chat"] }`. The feature turns on three optional
dependencies: `pulldown-cmark` (markdown parsing), `imara-diff` (line diffs) and `web-time`
(the monotonic clock the countdown and animations read).

Components, each reached as `frust_glyph::chat::<name>`:

- `message_scroller`: the virtualized transcript, stuck to its live end, with a jump-to-latest control.
- `message_row`: a user, assistant or thought message.
- `markdown`: assistant markdown, one widget per block.
- `code_block`: monospace code that scrolls sideways, with a copy control.
- `diff_view` (and `compute_diff`): a file edit as a line diff.
- `tool_call`: a tool call card with its output and diff.
- `plan_block`: the agent's plan entries.
- `approval_card`: a permission request and its answer, with an optional countdown.
- `notice_row`: a status line.
- `usage_meter`: a context-window meter.
- `composer`: the prompt input with send and stop.

**Controlled and non-reactive.** Each component takes data and callbacks, holds no signals and never
applies a toggle or an answer itself: the app owns the transcript and every open or answered state.

**Untrusted content.** Markdown emits no HTML and fetches no image. A link's URL is
attacker-controlled and may differ from its label, so show or confirm it before opening; `on_link`
receives only a validated `http`, `https` or `mailto` URL. Approval titles, details and options are
supplied by the agent: do not treat them as the app's own words. Key each approval card per request
so one request's answer cannot carry to the next, and make an `ApprovalDeadline` once, when the
request arrives, and store it, or the countdown restarts on every rebuild.

**Performance.** Markdown re-parses its whole source on each change and keeps unchanged blocks, so
coalesce stream chunks to one update per frame. Key `message_scroller` rows by a stable message id.
A markdown source over `MARKDOWN_MAX_SOURCE_BYTES` (256 KiB) is shown as plain text.

The module documentation of `src/chat/mod.rs` holds the contract. `tests/chat_api.rs` composes a
transcript (message, tool-call, plan, approval and notice rows above a composer), and the catalog's
**Agent chat** page ([`examples/glyph-catalog`](../../examples/glyph-catalog)) plays a scripted turn
through every component. Known limits are in the `glyph-chat-*` entries of
[`docs/LIMITATIONS.md`](../../docs/LIMITATIONS.md). The family has been tested headlessly on Linux
only.

Documentation and project home: <https://frust.dev>. Source: <https://github.com/frust-rs/frust>.

## License

The crate's SPDX expression is `(MIT OR Apache-2.0) AND OFL-1.1`: the Frust
sources are dual-licensed under [LICENSE-MIT](LICENSE-MIT) or
[LICENSE-APACHE](LICENSE-APACHE), and the bundled fonts are under the SIL Open
Font License 1.1.

Third-party material shipped in this package:

- Space Mono, OFL-1.1, Copyright 2016 The Space Mono Project Authors
  (<https://github.com/googlefonts/spacemono>): [fonts/space-mono/OFL.txt](fonts/space-mono/OFL.txt).
- IBM Plex Mono, OFL-1.1, Copyright (c) 2017 IBM Corp. with Reserved Font Name
  "Plex" (<https://github.com/IBM/plex>): [fonts/ibm-plex-mono/OFL.txt](fonts/ibm-plex-mono/OFL.txt).
