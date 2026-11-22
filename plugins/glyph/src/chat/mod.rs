//! Chat components for agent-style conversation UIs: transcript messages,
//! markdown, code and diff blocks, tool calls, plans, approvals, a composer.
//!
//! Behind the non-default `chat` cargo feature; reach items as
//! `frust_glyph::chat::*` (they are deliberately not re-exported at the crate
//! root, where names like `message` and `plan` would be too generic).
//!
//! # Contract
//!
//! - **Controlled and non-reactive.** Every component takes its data and
//!   callbacks from the caller and holds no signals; the app owns the
//!   transcript and re-builds the views when it changes.
//! - **Token-driven.** Colors, radii and the monospace family resolve from
//!   the live `Theme` (with an unthemed Glyph-dark fallback per value), never
//!   by matching on a design language. Shared values live in `style`.
//! - **Facade-only.** Authored against `frust::authoring`; no other framework
//!   crate is named.
//! - **Untrusted content.** Message text is untrusted. Markdown never emits
//!   raw HTML, images are never fetched (alt text is shown instead), and a
//!   link is only ever reported through the app's callback, never opened by
//!   the component.
//!
//! [`model`] holds the plain data types shared by the components.

mod approval;
mod code_block;
mod composer;
mod diff;
mod markdown;
mod markdown_parse;
mod message;
pub mod model;
mod notice;
mod plan;
mod scroller;
pub(crate) mod style;
mod table;
mod tool_call;
mod usage;

// The stub modules below are empty until their cards land, and a glob
// import of an empty module warns; the last card of the plan removes every
// `allow(unused_imports)` here.
#[allow(unused_imports)]
pub use approval::*;
#[allow(unused_imports)]
pub use code_block::*;
#[allow(unused_imports)]
pub use composer::*;
#[allow(unused_imports)]
pub use diff::*;
#[allow(unused_imports)]
pub use markdown::*;
#[allow(unused_imports)]
pub use markdown_parse::*;
#[allow(unused_imports)]
pub use message::*;
pub use model::*;
#[allow(unused_imports)]
pub use notice::*;
#[allow(unused_imports)]
pub use plan::*;
#[allow(unused_imports)]
pub use scroller::*;
#[allow(unused_imports)]
pub use table::*;
#[allow(unused_imports)]
pub use tool_call::*;
#[allow(unused_imports)]
pub use usage::*;
