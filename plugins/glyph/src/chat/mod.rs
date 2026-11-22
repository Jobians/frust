//! Chat components for agent-style conversation UIs, behind the non-default
//! `chat` feature and reached as `frust_glyph::chat::*` (not re-exported at
//! the crate root, where `message` or `plan` would be too generic).
//!
//! | Component | Shows |
//! |---|---|
//! | [`message_scroller`] | the virtualized transcript, stuck to its live end |
//! | [`message_row`] | a user, assistant or thought message |
//! | [`markdown`] | assistant markdown, one widget per block |
//! | [`code_block`] / [`diff_view`] | code, and a file edit as a line diff |
//! | [`tool_call`] / [`plan_block`] | a tool call card, the agent's plan |
//! | [`approval_card`] | a permission request and its answer |
//! | [`notice_row`] / [`usage_meter`] | a status line, a context meter |
//! | [`composer`] | the prompt input |
//!
//! **Controlled, non-reactive:** each takes data and callbacks, holds no
//! signals and never applies a toggle itself; the app owns the transcript.
//! **Untrusted content:** markdown emits no HTML and fetches no image; a
//! link's URL is attacker-controlled and may differ from its label, so show
//! or confirm it before opening; approval titles, details and options are
//! agent-supplied. **Performance:** markdown re-parses per change and keeps
//! unchanged blocks, so coalesce stream chunks to one update per frame; key
//! scroller rows by a stable message id. Composed example: `tests/chat_api.rs`.

mod approval;
mod code_block;
mod composer;
mod diff;
mod markdown;
mod markdown_parse;
mod message;
mod model;
mod notice;
mod plan;
mod scroller;
pub(crate) mod style;
mod table;
mod tool_call;
mod usage;

// The widget structs stay `pub` inside these private modules (a public
// view's `Element` cannot name a crate-private type) and are not listed
// here, so no path outside the crate reaches them.
pub use approval::{ApprovalCardView, ApprovalDeadline, approval_card};
pub use code_block::{CodeBlockView, CodeSpan, CodeTone, code_block};
pub use composer::{ComposerView, composer};
pub use diff::{
    DIFF_DEFAULT_COLLAPSE_ABOVE, DIFF_DEFAULT_CONTEXT, DIFF_MAX_BYTES, DIFF_MAX_LINES, DiffHunk,
    DiffLine, DiffLineKind, DiffModel, DiffView, compute_diff, diff_view,
};
pub use markdown::{MARKDOWN_MAX_SOURCE_BYTES, MarkdownView, markdown};
pub use message::{MessageRowView, message_row};
pub use model::{
    ApprovalOption, ApprovalOptionKind, ApprovalState, ChatRole, DiffInput, PlanEntry,
    PlanEntryStatus, ToolKind, ToolStatus,
};
pub use notice::{NoticeRowView, NoticeTone, notice_row};
pub use plan::{PlanBlockView, plan_block};
pub use scroller::{DEFAULT_ESTIMATED_ROW_EXTENT, MessageScrollerView, message_scroller};
pub use tool_call::{ToolCallView, ToolContent, tool_call};
pub use usage::{UsageLabel, UsageMeterView, usage_meter};
