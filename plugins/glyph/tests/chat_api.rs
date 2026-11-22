//! The public surface of `frust_glyph::chat`, named path by path: removing
//! or renaming a builder, a builder method, a type or a constant fails this
//! file. The second half is the family's composition example: a transcript
//! of message, tool-call, plan, approval and notice rows above a composer.
#![cfg(feature = "chat")]

use std::any::Any;
use std::fmt::Debug;
use std::rc::Rc;
use std::time::Duration;

use frust::FrameTime;
use frust::authoring::text::TextContext;
use frust::authoring::{AnyView, DiscardScene, Role, Size, View, any};
use frust::{ChildKey, EdgeInsets, EitherOf5, ScrollController, column, flexible, text};
use frust_core::RenderRoot;
use frust_glyph::chat::{
    ApprovalCardView, ApprovalDeadline, ApprovalOption, ApprovalOptionKind, ApprovalState,
    ChatRole, CodeBlockView, CodeSpan, CodeTone, ComposerView, DEFAULT_ESTIMATED_ROW_EXTENT,
    DIFF_DEFAULT_COLLAPSE_ABOVE, DIFF_DEFAULT_CONTEXT, DIFF_MAX_BYTES, DIFF_MAX_LINES, DiffHunk,
    DiffInput, DiffLine, DiffLineKind, DiffModel, DiffView, MarkdownView, MessageRowView,
    MessageScrollerView, NoticeRowView, NoticeTone, PlanBlockView, PlanEntry, PlanEntryStatus,
    ToolCallView, ToolContent, ToolKind, ToolStatus, UsageLabel, UsageMeterView, approval_card,
    code_block, composer, compute_diff, diff_view, markdown, message_row, message_scroller,
    notice_row, plan_block, tool_call, usage_meter,
};

/// Compiles only for a `View` that is also `Debug`.
fn is_view<V: View<()> + Debug>(_view: &V) {}

fn option(id: &str, kind: ApprovalOptionKind) -> ApprovalOption {
    ApprovalOption {
        id: id.to_owned(),
        label: format!("{id} label"),
        kind,
    }
}

fn edit() -> DiffInput {
    DiffInput {
        path: "src/lib.rs".to_owned(),
        old_text: Some("a\nb\n".to_owned()),
        new_text: "a\nc\n".to_owned(),
    }
}

#[test]
fn every_builder_and_builder_method_is_reachable() {
    let card: ApprovalCardView<()> =
        approval_card("Run it?", vec![option("y", ApprovalOptionKind::AllowOnce)])
            .detail("ls -la")
            .state(ApprovalState::Pending)
            .expires(ApprovalDeadline::after(Duration::from_secs(30)))
            .on_select(|_: &mut (), _id: &str| {})
            .selected("y")
            .busy(false)
            .expired_hint("Ask again");
    is_view(&card);

    let code: CodeBlockView<()> = code_block("let x = 1;")
        .language("rust")
        .spans(vec![CodeSpan {
            range: 0..3,
            style: CodeTone::Keyword,
        }])
        .on_copy(|_: &mut (), _code: &str| {})
        .max_visible_lines(12)
        .expanded(false)
        .on_toggle(|_: &mut ()| {})
        .streaming(false);
    is_view(&code);

    let input: ComposerView<()> = composer("draft")
        .on_change(|_: &mut (), _text: String| {})
        .on_submit(|_: &mut (), _text: String| {})
        .on_stop(|_: &mut ()| {})
        .running(false)
        .disabled("Sign in to chat")
        .placeholder("Ask anything")
        .max_visible_lines(6)
        .submit_on_enter(true)
        .trailing_action(text("mic"));
    is_view(&input);

    let diff: DiffView<()> = diff_view(edit())
        .expanded(false)
        .on_toggle(|_: &mut ()| {})
        .collapse_above(DIFF_DEFAULT_COLLAPSE_ABOVE)
        .context(DIFF_DEFAULT_CONTEXT);
    is_view(&diff);

    let md: MarkdownView<()> = markdown("**hi**")
        .streaming(false)
        .on_link(|_: &mut (), _url: &str| {})
        .selectable(true)
        .on_copy(|_: &mut (), _code: &str| {});
    is_view(&md);

    let row: MessageRowView<()> = message_row(ChatRole::Assistant, "hi")
        .streaming(false)
        .on_link(|_: &mut (), _url: &str| {})
        .on_copy(|_: &mut (), _text: &str| {})
        .thought_open(false)
        .on_thought_toggle(|_: &mut ()| {})
        .max_width(720.0)
        .selectable(true);
    is_view(&row);

    let notice: NoticeRowView<()> = notice_row("Context trimmed")
        .tone(NoticeTone::Warning)
        .working(false);
    is_view(&notice);

    let plan: PlanBlockView<()> = plan_block(vec![PlanEntry {
        text: "write the parser".to_owned(),
        status: PlanEntryStatus::InProgress,
    }])
    .title("Plan");
    is_view(&plan);

    let scroller: MessageScrollerView<()> =
        message_scroller(0, |i| ChildKey::new(i as u64), |_| text("row"))
            .controller(ScrollController::new())
            .at_end(true)
            .on_at_end_change(|_: &mut (), _at_end: bool| {})
            .unread(0)
            .row_gap(8.0)
            .padding(EdgeInsets::all(0.0))
            .empty(text("No messages"))
            .estimated_row_extent(DEFAULT_ESTIMATED_ROW_EXTENT);
    is_view(&scroller);

    let call: ToolCallView<()> = tool_call(ToolKind::Execute, "cargo test", ToolStatus::Completed)
        .open(true)
        .on_toggle(|_: &mut ()| {})
        .content("ok")
        .contents([
            ToolContent::Text("more".to_owned()),
            ToolContent::Diff(edit()),
        ])
        .on_copy(|_: &mut (), _text: &str| {})
        .max_visible_lines(12)
        .output_expanded(false)
        .on_output_toggle(|_: &mut ()| {})
        .diff_expanded(false)
        .on_diff_toggle(|_: &mut ()| {});
    is_view(&call);

    let meter: UsageMeterView<()> = usage_meter(42_000, 200_000)
        .label(UsageLabel::Counts)
        .warn_at(0.8)
        .critical_at(0.95)
        .compact(false)
        .semantics_label("context usage");
    is_view(&meter);
}

#[test]
fn the_data_types_and_constants_are_reachable() {
    let model: DiffModel = compute_diff(Some("a\nb\n"), "a\nc\n", DIFF_DEFAULT_CONTEXT);
    assert!(model.has_changes());
    assert_eq!(model.line_count(), 3);
    assert_eq!(model.row_count(), 4);
    let hunk: &DiffHunk = &model.hunks[0];
    assert_eq!(hunk.header(), "@@ -1,2 +1,2 @@");
    let line: &DiffLine = &hunk.lines[1];
    assert_eq!(line.kind, DiffLineKind::Removed);
    assert_eq!(DiffLineKind::Added.marker(), '+');
    let limits: [usize; 2] = [DIFF_MAX_BYTES, DIFF_MAX_LINES];
    assert!(limits.iter().all(|&n| n > 0));

    assert!(edit().old_text.is_some() && !edit().is_new_file());
    assert!(ToolStatus::Failed.is_finished());
    assert!(ApprovalOptionKind::AllowAlways.is_allow());
    assert!(ApprovalState::Pending.is_pending());
    let deadline = ApprovalDeadline::after(Duration::from_secs(5));
    assert_eq!(deadline.total(), Duration::from_secs(5));
    assert!(deadline.remaining() <= deadline.total());
    let _ = (
        ChatRole::User,
        ChatRole::Thought,
        ToolKind::Read,
        CodeTone::Plain,
        NoticeTone::Error,
        UsageLabel::Percent,
        PlanEntryStatus::Completed,
        ToolContent::from("text"),
    );
    fn debug<T: Debug>() {}
    debug::<(
        ApprovalDeadline,
        ApprovalOption,
        CodeSpan,
        DiffModel,
        DiffInput,
        PlanEntry,
    )>();
}

// ---------------------------------------------------------------------------
// Composition: a transcript of every row kind above a composer.
// ---------------------------------------------------------------------------

/// One transcript entry, as an app stores it.
enum Entry {
    Message(ChatRole, String),
    Tool(ToolKind, String, ToolStatus),
    Plan(Vec<PlanEntry>),
    Approval(String, Vec<ApprovalOption>),
    Notice(String),
}

#[derive(Default)]
struct Chat {
    transcript: Rc<Vec<(u64, Entry)>>,
    draft: String,
    sent: Vec<String>,
    answers: Vec<String>,
    at_end: bool,
}

impl Chat {
    fn screen(&self) -> impl View<Chat> + use<> {
        let transcript = Rc::clone(&self.transcript);
        let keys = Rc::clone(&self.transcript);
        let rows = message_scroller(
            transcript.len(),
            move |i| ChildKey::new(keys[i].0),
            move |i| Self::row(&transcript[i].1),
        )
        .at_end(self.at_end)
        .on_at_end_change(|chat: &mut Chat, at_end| chat.at_end = at_end)
        .padding(EdgeInsets::all(12.0));
        let input = composer(self.draft.clone())
            .on_change(|chat: &mut Chat, text| chat.draft = text)
            .on_submit(|chat: &mut Chat, text| chat.sent.push(text));
        column().push(flexible(1, rows)).child(input)
    }

    fn row(entry: &Entry) -> impl View<Chat> + use<> {
        match entry {
            Entry::Message(role, text) => EitherOf5::A(message_row(*role, text.as_str())),
            Entry::Tool(kind, title, status) => {
                EitherOf5::B(tool_call(*kind, title.as_str(), *status).content("ok"))
            }
            Entry::Plan(entries) => EitherOf5::C(plan_block(entries.clone())),
            Entry::Approval(title, options) => EitherOf5::D(
                approval_card(title.as_str(), options.clone())
                    .on_select(|chat: &mut Chat, id: &str| chat.answers.push(id.to_owned())),
            ),
            Entry::Notice(text) => EitherOf5::E(notice_row(text.as_str()).working(true)),
        }
    }
}

fn transcript() -> Vec<(u64, Entry)> {
    vec![
        (
            1,
            Entry::Message(ChatRole::User, "Fix the failing test".into()),
        ),
        (
            2,
            Entry::Message(ChatRole::Assistant, "Looking at `parse` first.".into()),
        ),
        (
            3,
            Entry::Tool(ToolKind::Execute, "cargo test".into(), ToolStatus::Failed),
        ),
        (
            4,
            Entry::Plan(vec![PlanEntry {
                text: "patch the parser".into(),
                status: PlanEntryStatus::InProgress,
            }]),
        ),
        (
            5,
            Entry::Approval(
                "Run cargo fmt?".into(),
                vec![
                    option("allow", ApprovalOptionKind::AllowOnce),
                    option("deny", ApprovalOptionKind::RejectOnce),
                ],
            ),
        ),
        (6, Entry::Notice("Waiting for approval".into())),
    ]
}

#[test]
fn a_transcript_of_every_row_kind_composes_above_the_composer() {
    let mut chat = Chat {
        transcript: Rc::new(transcript()),
        draft: "thanks".into(),
        at_end: true,
        ..Chat::default()
    };
    let mut root: RenderRoot<Chat, AnyView<Chat>> = RenderRoot::new();
    root.set_theme(Box::new(frust_glyph::baseline()));
    let mut text = TextContext::new();
    root.rebuild(&mut |chat: &mut Chat| any(chat.screen()), &mut chat);
    let window = Size::new(390.0, 2400.0);
    let laid = root.layout_with_text(window, &mut text as &mut dyn Any);
    assert!(laid.width <= window.width + 0.01);
    root.paint(&mut DiscardScene, FrameTime::ZERO);

    let update = root.semantics();
    let has = |role: Role, label: &str| {
        update
            .nodes
            .iter()
            .any(|(_, n)| n.role() == role && n.label() == Some(label))
    };
    assert!(has(Role::Log, "Messages"), "the transcript region");
    assert!(has(Role::Button, "Send"), "the composer's send control");
    assert!(has(Role::Button, "allow label"), "the approval's options");
    assert!(
        update.nodes.iter().any(|(_, n)| n.role() == Role::List),
        "the plan"
    );
    for (_, node) in &update.nodes {
        if let Some(b) = node.bounds() {
            assert!(
                b.x1 <= window.width + 0.5,
                "{:?} past the width",
                node.role()
            );
        }
    }
    assert!(chat.sent.is_empty() && chat.answers.is_empty());
}
