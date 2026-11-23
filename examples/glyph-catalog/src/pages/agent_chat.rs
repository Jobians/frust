//! Agent chat section: a scripted agent conversation played through every
//! `frust_glyph::chat` component. It is the demonstration, the device-gate
//! target and the reference composition for app authors.
//!
//! The page is a [`Component`] with its own [`ChatPage`] state: the transcript
//! ([`ChatScript`], plain data whose rows carry stable ids used as list keys),
//! the draft, the scroller's at-end flag and a frame clock. Every view is
//! composed in [`AgentChat`]'s `build`.
//!
//! # The scripted driver
//!
//! [`ChatScript`] is a timed queue of steps (append a row, stream text into
//! it, move a tool or plan entry on, wait for approvals) advanced by the frame
//! clock: a zero-size pump republishes `PaintCtx::frame_time` and asks for the
//! next frame while a turn runs, and `build` steps the script once per frame,
//! so every stream chunk that arrived since the last frame lands in one state
//! update. No thread, no timer, no network; idle, the page requests no frames.
//!
//! The 30 s approval countdown is an `ApprovalDeadline` made once when the
//! request arrives; the script also expires the card on its own clock, so a
//! headless run is deterministic. Links and copies only raise a toast: the
//! page opens nothing, and copy is the components' own clipboard write.
//! Leaving the section tears the component down and restarts the transcript.

use std::cell::Cell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;

use frust::authoring::{
    BoxConstraints, BuildCtx, ChangeFlags, LayoutCtx, PaintCtx, PaintScene, Size, View, Widget,
};
use frust::{
    ButtonStyle, ChildKey, Component, CrossAxisAlignment, EdgeInsets, EitherOf5, FrameTime,
    Padding, RwSignal, ScrollController, Theme, Update, button, column, component, row, text,
    use_context,
};
use frust_glyph::chat::{
    ApprovalDeadline, ApprovalOption, ApprovalOptionKind, ApprovalSelection, ApprovalState,
    ChatRole, DiffInput, NoticeTone, PlanEntry, PlanEntryStatus, ToolContent, ToolKind, ToolStatus,
    UsageLabel, approval_card, composer, message_row, message_scroller, notice_row, plan_block,
    tool_call, usage_meter,
};

use crate::CatalogState;

/// The context window the usage meter measures against.
pub const CONTEXT_SIZE: u64 = 200_000;
/// Tokens already used when the page opens; two turns cross the 80 % warning.
pub const INITIAL_USED: u64 = 104_000;
/// The synthetic transcript the "Load 5,000 rows" control installs.
pub const SYNTHETIC_ROWS: usize = 5_000;
/// The prompt the "Play turn" control sends.
pub const DEMO_PROMPT: &str = "The parser accepts a trailing comma. Fix it and run the tests.";

/// Characters per simulated stream chunk, and the time between chunks.
const CHUNK_CHARS: usize = 6;
const CHUNK_MS: u128 = 30;
/// How long the reply holds with its code fence still open.
const FENCE_PAUSE: Duration = Duration::from_millis(1_500);
/// The interactive approval's countdown, and the scripted expiring one's.
const ASK_TIMEOUT: Duration = Duration::from_secs(30);
const CLEAN_TIMEOUT: Duration = Duration::from_secs(4);
/// The longest step one frame advances the script, so a page resumed after a
/// long idle gap does not jump a whole turn in one frame.
const MAX_STEP: Duration = Duration::from_millis(50);
/// Reading width of a message on a wide window.
const MAX_READING_WIDTH: f64 = 760.0;

// ---------------------------------------------------------------------------
// The transcript model
// ---------------------------------------------------------------------------

/// What a transcript row shows, one variant per chat component it maps to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// A user message.
    User,
    /// An assistant message (markdown).
    Assistant,
    /// A thought row.
    Thought,
    /// A tool call card.
    Tool,
    /// The plan block.
    Plan,
    /// An approval card.
    Approval,
    /// A notice row.
    Notice,
}

#[derive(Clone, Debug)]
struct Row {
    id: u64,
    body: Body,
}

#[derive(Clone, Debug)]
enum Body {
    Message {
        role: ChatRole,
        text: String,
        streaming: bool,
        open: bool,
    },
    Tool(Tool),
    Plan(Vec<PlanEntry>),
    Approval(Approval),
    Notice {
        text: String,
        tone: NoticeTone,
        working: bool,
    },
}

#[derive(Clone, Debug)]
struct Tool {
    kind: ToolKind,
    title: String,
    status: ToolStatus,
    contents: Vec<ToolContent>,
    open: bool,
    output_expanded: bool,
    diff_expanded: bool,
}

#[derive(Clone, Debug)]
struct Approval {
    title: String,
    detail: String,
    options: Vec<ApprovalOption>,
    state: ApprovalState,
    selected: Option<String>,
    deadline: Option<ApprovalDeadline>,
    /// When the script expires the request, on the script's own clock.
    expires_at: Option<Duration>,
}

impl Body {
    fn kind(&self) -> RowKind {
        match self {
            Body::Message { role, .. } => match role {
                ChatRole::User => RowKind::User,
                ChatRole::Thought => RowKind::Thought,
                _ => RowKind::Assistant,
            },
            Body::Tool(_) => RowKind::Tool,
            Body::Plan(_) => RowKind::Plan,
            Body::Approval(_) => RowKind::Approval,
            Body::Notice { .. } => RowKind::Notice,
        }
    }
}

/// Which disclosure a press on a row toggles.
#[derive(Clone, Copy, Debug)]
enum Toggle {
    Thought,
    Tool,
    Output,
    Diff,
}

/// An index into a turn's rows, fixed by the turn script.
type Slot = usize;

/// One scripted step. Instant steps apply and fall through to the next one in
/// the same frame; `Wait`, `Stream` and `Await` hold the queue.
enum Step {
    Push(Slot, Body),
    /// Give a pushed approval its countdown, starting now.
    Arm(Slot, Duration),
    /// Append text to a message at the chunk rate.
    Stream(Slot, &'static str),
    Finish(Slot),
    Wait(Duration),
    Status(Slot, ToolStatus),
    ToolDone(Slot, ToolStatus, Vec<ToolContent>),
    PlanEntry(Slot, usize, PlanEntryStatus),
    /// Resolve an approval that is still pending.
    Resolve(Slot, ApprovalState),
    /// Hold until none of these approvals is pending.
    Await(Vec<Slot>),
    /// Turn a notice into the outcome of an approval.
    Report {
        approval: Slot,
        notice: Slot,
    },
    Notice(Slot, String, NoticeTone, bool),
    Usage(u64),
}

/// A turn in flight.
struct Run {
    steps: VecDeque<Step>,
    slots: Vec<Option<u64>>,
    /// Script time since the turn began, and when the front step began.
    clock: Duration,
    step_start: Duration,
    /// Byte length of the streamed message before the current `Stream` step.
    stream_base: Option<usize>,
}

/// The conversation the page owns: its rows (oldest first, each with a stable
/// id), the context usage, and the scripted turn in flight, if any.
///
/// Plain data with no widget and no clock of its own: [`advance`](Self::advance)
/// is stepped by the caller, once per frame on the page.
pub struct ChatScript {
    rows: Rc<Vec<Rc<Row>>>,
    next_id: u64,
    turn: u64,
    used: u64,
    run: Option<Run>,
}

impl Default for ChatScript {
    fn default() -> Self {
        Self::new()
    }
}

impl ChatScript {
    /// The opening transcript: one notice explaining the page.
    pub fn new() -> Self {
        Self::opening(1)
    }

    /// The opening transcript with ids from `first_id` on.
    fn opening(first_id: u64) -> Self {
        let mut script = ChatScript {
            rows: Rc::new(Vec::new()),
            next_id: first_id,
            turn: 0,
            used: INITIAL_USED,
            run: None,
        };
        script.push(notice(
            "Scripted agent: send any prompt, or press Play turn, to stream a turn.",
            NoticeTone::Info,
            false,
        ));
        script
    }

    /// The number of rows.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether the transcript has no rows.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The kind of every row, oldest first.
    pub fn kinds(&self) -> Vec<RowKind> {
        self.rows.iter().map(|row| row.body.kind()).collect()
    }

    /// Every row's id, oldest first.
    pub fn ids(&self) -> Vec<u64> {
        self.rows.iter().map(|row| row.id).collect()
    }

    /// The status of every tool call, oldest first.
    pub fn tool_statuses(&self) -> Vec<ToolStatus> {
        self.bodies()
            .filter_map(|body| match body {
                Body::Tool(tool) => Some(tool.status),
                _ => None,
            })
            .collect()
    }

    /// The state of every approval, oldest first.
    pub fn approval_states(&self) -> Vec<ApprovalState> {
        self.bodies()
            .filter_map(|body| match body {
                Body::Approval(approval) => Some(approval.state),
                _ => None,
            })
            .collect()
    }

    /// The statuses of the latest plan's entries.
    pub fn plan_statuses(&self) -> Vec<PlanEntryStatus> {
        self.rows
            .iter()
            .rev()
            .find_map(|row| match &row.body {
                Body::Plan(entries) => Some(entries.iter().map(|e| e.status).collect()),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// The row ids of the approvals still waiting for an answer.
    pub fn pending_approvals(&self) -> Vec<u64> {
        self.rows
            .iter()
            .filter(|row| matches!(&row.body, Body::Approval(a) if a.state.is_pending()))
            .map(|row| row.id)
            .collect()
    }

    /// The latest assistant message's text so far.
    pub fn assistant_text(&self) -> Option<&str> {
        self.rows.iter().rev().find_map(|row| match &row.body {
            Body::Message {
                role: ChatRole::Assistant,
                text,
                ..
            } => Some(text.as_str()),
            _ => None,
        })
    }

    /// Tokens used in the context window.
    pub fn used(&self) -> u64 {
        self.used
    }

    /// Whether a scripted turn is in flight.
    pub fn is_running(&self) -> bool {
        self.run.is_some()
    }

    /// Send `text` as a user message and start a scripted turn. Refused (and
    /// reported `false`) for a blank prompt or while a turn runs.
    pub fn submit(&mut self, text: impl Into<String>) -> bool {
        let text = text.into();
        if text.trim().is_empty() || self.is_running() {
            return false;
        }
        self.push(message(ChatRole::User, text, false));
        self.turn += 1;
        let steps: VecDeque<Step> = turn_steps(self.turn).into();
        let slot_count = steps.iter().map(step_slot).max().unwrap_or(0) + 1;
        self.run = Some(Run {
            steps,
            slots: vec![None; slot_count],
            clock: Duration::ZERO,
            step_start: Duration::ZERO,
            stream_base: None,
        });
        true
    }

    /// Interrupt the turn: streaming stops where it is, unfinished tools fail,
    /// unanswered approvals expire, and a notice says so. Reports whether a
    /// turn was running.
    pub fn stop(&mut self) -> bool {
        let Some(run) = self.run.take() else {
            return false;
        };
        for id in run.slots.iter().flatten() {
            self.edit(*id, |body| match body {
                Body::Message { streaming, .. } => *streaming = false,
                Body::Tool(tool) if !tool.status.is_finished() => tool.status = ToolStatus::Failed,
                Body::Approval(a) if a.state.is_pending() => a.state = ApprovalState::Expired,
                Body::Notice { working, .. } => *working = false,
                _ => {}
            });
        }
        self.push(notice(
            "Stopped: the turn was interrupted.",
            NoticeTone::Warning,
            false,
        ));
        true
    }

    /// Start over: the opening transcript and usage, no turn in flight. Ids
    /// keep counting, so no new row reuses a key the scroller still holds.
    pub fn restart(&mut self) {
        *self = Self::opening(self.next_id);
    }

    /// Replace the transcript with `count` synthetic rows (user and assistant
    /// messages with a notice every 25th row), stopping any turn.
    pub fn load_synthetic(&mut self, count: usize) {
        self.run = None;
        let first = self.next_id;
        let rows = (0..count)
            .map(|i| {
                let body = if i % 25 == 24 {
                    notice(format!("Synthetic checkpoint {i}"), NoticeTone::Info, false)
                } else if i % 2 == 0 {
                    message(ChatRole::User, format!("Synthetic prompt {i}"), false)
                } else {
                    message(
                        ChatRole::Assistant,
                        format!("Synthetic reply {i}: **done**, see `row {i}`."),
                        false,
                    )
                };
                Rc::new(Row {
                    id: first + i as u64,
                    body,
                })
            })
            .collect();
        self.rows = Rc::new(rows);
        self.next_id = first + count as u64;
    }

    /// Answer the approval in row `id` with option `option_id`. Reports
    /// whether it was still pending.
    pub fn answer(&mut self, id: u64, option_id: &str) -> bool {
        let mut answered = false;
        self.edit(id, |body| {
            if let Body::Approval(a) = body
                && a.state.is_pending()
                && let Some(option) = a.options.iter().find(|o| o.id == option_id)
            {
                a.state = if option.kind.is_allow() {
                    ApprovalState::Allowed
                } else {
                    ApprovalState::Denied
                };
                a.selected = Some(option.id.clone());
                answered = true;
            }
        });
        answered
    }

    fn toggle(&mut self, id: u64, which: Toggle) {
        self.edit(id, |body| match (body, which) {
            (Body::Message { open, .. }, Toggle::Thought) => *open = !*open,
            (Body::Tool(tool), Toggle::Tool) => tool.open = !tool.open,
            (Body::Tool(tool), Toggle::Output) => tool.output_expanded = !tool.output_expanded,
            (Body::Tool(tool), Toggle::Diff) => tool.diff_expanded = !tool.diff_expanded,
            _ => {}
        });
    }

    /// Step the turn by `delta` of script time, applying every step that came
    /// due as one update. Returns whether the turn is still running.
    pub fn advance(&mut self, delta: Duration) -> bool {
        let Some(mut run) = self.run.take() else {
            return false;
        };
        run.clock += delta;
        self.expire_due(&run);
        while let Some(step) = run.steps.pop_front() {
            if let Some(held) = self.apply(&mut run, step) {
                run.steps.push_front(held);
                self.run = Some(run);
                return true;
            }
        }
        false
    }

    /// Expire every pending approval of the turn whose time is up, on the
    /// script clock or on the card's own deadline.
    fn expire_due(&mut self, run: &Run) {
        // Found first and edited only on expiry, so a quiet frame copies no row.
        let due: Vec<u64> = self
            .rows
            .iter()
            .filter(|row| run.slots.contains(&Some(row.id)))
            .filter(|row| match &row.body {
                Body::Approval(a) => {
                    a.state.is_pending()
                        && (a.expires_at.is_some_and(|at| run.clock >= at)
                            || a.deadline.as_ref().is_some_and(|d| d.remaining().is_zero()))
                }
                _ => false,
            })
            .map(|row| row.id)
            .collect();
        for id in due {
            self.edit(id, |body| {
                if let Body::Approval(a) = body {
                    a.state = ApprovalState::Expired;
                }
            });
        }
    }

    /// Apply `step`; hand it back when it still holds the queue.
    fn apply(&mut self, run: &mut Run, step: Step) -> Option<Step> {
        let slot_id = |run: &Run, slot: Slot| run.slots.get(slot).copied().flatten();
        match step {
            Step::Push(slot, body) => {
                let id = self.push(body);
                run.slots[slot] = Some(id);
            }
            Step::Arm(slot, timeout) => {
                let at = run.clock + timeout;
                if let Some(id) = slot_id(run, slot) {
                    self.edit(id, |body| {
                        if let Body::Approval(a) = body {
                            a.deadline = Some(ApprovalDeadline::after(timeout));
                            a.expires_at = Some(at);
                        }
                    });
                }
            }
            Step::Stream(slot, text) => {
                let id = slot_id(run, slot)?;
                let elapsed = run.clock.saturating_sub(run.step_start);
                let shown = (elapsed.as_millis() / CHUNK_MS + 1) as usize * CHUNK_CHARS;
                let end = text
                    .char_indices()
                    .nth(shown)
                    .map_or(text.len(), |(i, _)| i);
                let mut base = run.stream_base;
                self.edit(id, |body| {
                    if let Body::Message { text: current, .. } = body {
                        let start = *base.get_or_insert(current.len());
                        current.truncate(start);
                        current.push_str(&text[..end]);
                    }
                });
                if end < text.len() {
                    run.stream_base = base;
                    return Some(Step::Stream(slot, text));
                }
                run.stream_base = None;
                run.step_start = run.clock;
            }
            Step::Finish(slot) => {
                if let Some(id) = slot_id(run, slot) {
                    self.edit(id, |body| {
                        if let Body::Message { streaming, .. } = body {
                            *streaming = false;
                        }
                    });
                }
            }
            Step::Wait(duration) => {
                if run.clock.saturating_sub(run.step_start) < duration {
                    return Some(Step::Wait(duration));
                }
                run.step_start += duration;
            }
            Step::Status(slot, status) => {
                if let Some(id) = slot_id(run, slot) {
                    self.edit(id, |body| {
                        if let Body::Tool(tool) = body {
                            tool.status = status;
                        }
                    });
                }
            }
            Step::ToolDone(slot, status, contents) => {
                if let Some(id) = slot_id(run, slot) {
                    self.edit(id, |body| {
                        if let Body::Tool(tool) = body {
                            tool.status = status;
                            tool.contents = contents;
                        }
                    });
                }
            }
            Step::PlanEntry(slot, index, status) => {
                if let Some(id) = slot_id(run, slot) {
                    self.edit(id, |body| {
                        if let Body::Plan(entries) = body
                            && let Some(entry) = entries.get_mut(index)
                        {
                            entry.status = status;
                        }
                    });
                }
            }
            Step::Resolve(slot, state) => {
                if let Some(id) = slot_id(run, slot) {
                    self.edit(id, |body| {
                        if let Body::Approval(a) = body
                            && a.state.is_pending()
                        {
                            a.state = state;
                        }
                    });
                }
            }
            Step::Await(slots) => {
                let pending = slots.iter().any(|&slot| {
                    slot_id(run, slot).is_some_and(|id| {
                        self.rows.iter().any(|row| {
                            row.id == id
                                && matches!(&row.body, Body::Approval(a) if a.state.is_pending())
                        })
                    })
                });
                if pending {
                    return Some(Step::Await(slots));
                }
                run.step_start = run.clock;
            }
            Step::Report { approval, notice } => {
                let state = slot_id(run, approval)
                    .and_then(|id| self.rows.iter().find(|row| row.id == id))
                    .and_then(|row| match &row.body {
                        Body::Approval(a) => Some(a.state),
                        _ => None,
                    });
                let (line, tone) = match state {
                    Some(ApprovalState::Allowed) => {
                        ("Approved: cargo fmt --all ran.", NoticeTone::Success)
                    }
                    Some(ApprovalState::Denied) => {
                        ("Denied: formatting skipped.", NoticeTone::Warning)
                    }
                    _ => (
                        "No answer in time: formatting skipped.",
                        NoticeTone::Warning,
                    ),
                };
                if let Some(id) = slot_id(run, notice) {
                    self.set_notice(id, line.to_string(), tone, false);
                }
            }
            Step::Notice(slot, line, tone, working) => {
                if let Some(id) = slot_id(run, slot) {
                    self.set_notice(id, line, tone, working);
                }
            }
            Step::Usage(tokens) => self.used += tokens,
        }
        None
    }

    fn set_notice(&mut self, id: u64, line: String, new_tone: NoticeTone, now_working: bool) {
        self.edit(id, |body| {
            if let Body::Notice {
                text,
                tone,
                working,
            } = body
            {
                *text = line;
                *tone = new_tone;
                *working = now_working;
            }
        });
    }

    fn bodies(&self) -> impl Iterator<Item = &Body> {
        self.rows.iter().map(|row| &row.body)
    }

    fn push(&mut self, body: Body) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        Rc::make_mut(&mut self.rows).push(Rc::new(Row { id, body }));
        id
    }

    /// Edit row `id` in place. The search runs from the newest row, where a
    /// turn's rows are.
    fn edit(&mut self, id: u64, f: impl FnOnce(&mut Body)) {
        let rows = Rc::make_mut(&mut self.rows);
        if let Some(row) = rows.iter_mut().rev().find(|row| row.id == id) {
            f(&mut Rc::make_mut(row).body);
        }
    }
}

/// The highest slot a step names, so a run can size its slot table.
fn step_slot(step: &Step) -> Slot {
    match step {
        Step::Push(s, _)
        | Step::Arm(s, _)
        | Step::Stream(s, _)
        | Step::Finish(s)
        | Step::Status(s, _)
        | Step::ToolDone(s, _, _)
        | Step::PlanEntry(s, _, _)
        | Step::Resolve(s, _)
        | Step::Notice(s, _, _, _) => *s,
        Step::Report { approval, notice } => (*approval).max(*notice),
        Step::Await(slots) => slots.iter().copied().max().unwrap_or(0),
        Step::Wait(_) | Step::Usage(_) => 0,
    }
}

fn message(role: ChatRole, text: impl Into<String>, streaming: bool) -> Body {
    Body::Message {
        role,
        text: text.into(),
        streaming,
        open: false,
    }
}

fn notice(text: impl Into<String>, tone: NoticeTone, working: bool) -> Body {
    Body::Notice {
        text: text.into(),
        tone,
        working,
    }
}

fn tool(kind: ToolKind, title: &str, open: bool) -> Body {
    Body::Tool(Tool {
        kind,
        title: title.to_string(),
        status: ToolStatus::Pending,
        contents: Vec::new(),
        open,
        output_expanded: false,
        diff_expanded: false,
    })
}

fn approval(title: &str, detail: &str, options: Vec<ApprovalOption>) -> Body {
    Body::Approval(Approval {
        title: title.to_string(),
        detail: detail.to_string(),
        options,
        state: ApprovalState::Pending,
        selected: None,
        deadline: None,
        expires_at: None,
    })
}

fn option(id: &str, label: &str, kind: ApprovalOptionKind) -> ApprovalOption {
    ApprovalOption {
        id: id.to_string(),
        label: label.to_string(),
        kind,
    }
}

fn plan_entry(text: &str) -> PlanEntry {
    PlanEntry {
        text: text.to_string(),
        status: PlanEntryStatus::Pending,
    }
}

fn ms(millis: u64) -> Duration {
    Duration::from_millis(millis)
}

/// One scripted turn, numbered `turn`: a thought, a plan, four tool calls
/// (text output, an edit diff, a new-file diff, a failure), three approvals
/// (one to answer, one that expires, one answered elsewhere) and a markdown
/// reply whose code fence stays open for a while.
fn turn_steps(turn: u64) -> Vec<Step> {
    const STATE: Slot = 0;
    const THOUGHT: Slot = 1;
    const PLAN: Slot = 2;
    const READ: Slot = 3;
    const EDIT: Slot = 4;
    const CREATE: Slot = 5;
    const TEST: Slot = 6;
    const ASK: Slot = 7;
    const WAITING: Slot = 8;
    const CLEAN: Slot = 9;
    const PUSH: Slot = 10;
    const REPLY: Slot = 11;
    const DONE: Slot = 12;
    use Step::*;
    vec![
        Push(
            STATE,
            notice(format!("Turn {turn}: working"), NoticeTone::Info, true),
        ),
        Wait(ms(400)),
        Push(THOUGHT, message(ChatRole::Thought, "", true)),
        Stream(THOUGHT, THOUGHT_TEXT),
        Finish(THOUGHT),
        Push(
            PLAN,
            Body::Plan(vec![
                plan_entry("Read the parser"),
                plan_entry("Reject the trailing comma"),
                plan_entry("Move spans into src/span.rs"),
                plan_entry("Run the tests"),
            ]),
        ),
        Usage(2_400),
        PlanEntry(PLAN, 0, PlanEntryStatus::InProgress),
        Push(READ, tool(ToolKind::Read, "Read src/parser.rs", false)),
        Wait(ms(500)),
        Status(READ, ToolStatus::InProgress),
        Wait(ms(700)),
        ToolDone(
            READ,
            ToolStatus::Completed,
            vec![ToolContent::Text(READ_OUTPUT.to_string())],
        ),
        Usage(3_100),
        PlanEntry(PLAN, 0, PlanEntryStatus::Completed),
        PlanEntry(PLAN, 1, PlanEntryStatus::InProgress),
        Push(EDIT, tool(ToolKind::Edit, "Edit src/parser.rs", true)),
        Wait(ms(400)),
        Status(EDIT, ToolStatus::InProgress),
        Wait(ms(800)),
        ToolDone(
            EDIT,
            ToolStatus::Completed,
            vec![ToolContent::Diff(DiffInput {
                path: "src/parser.rs".to_string(),
                old_text: Some(PARSER_OLD.to_string()),
                new_text: PARSER_NEW.to_string(),
            })],
        ),
        PlanEntry(PLAN, 1, PlanEntryStatus::Completed),
        PlanEntry(PLAN, 2, PlanEntryStatus::InProgress),
        Push(CREATE, tool(ToolKind::Edit, "Create src/span.rs", true)),
        Wait(ms(400)),
        Status(CREATE, ToolStatus::InProgress),
        Wait(ms(600)),
        ToolDone(
            CREATE,
            ToolStatus::Completed,
            vec![ToolContent::Diff(DiffInput {
                path: "src/span.rs".to_string(),
                old_text: None,
                new_text: SPAN_NEW.to_string(),
            })],
        ),
        Usage(4_200),
        PlanEntry(PLAN, 2, PlanEntryStatus::Completed),
        PlanEntry(PLAN, 3, PlanEntryStatus::InProgress),
        Push(TEST, tool(ToolKind::Execute, "cargo test", true)),
        Wait(ms(300)),
        Status(TEST, ToolStatus::InProgress),
        Wait(ms(1_200)),
        ToolDone(
            TEST,
            ToolStatus::Failed,
            vec![ToolContent::Text(TEST_OUTPUT.to_string())],
        ),
        Push(
            ASK,
            approval(
                "Run cargo fmt --all?",
                "cargo fmt --all\nRewrites src/parser.rs and src/span.rs in place.",
                vec![
                    option("allow-once", "Allow once", ApprovalOptionKind::AllowOnce),
                    option(
                        "allow-always",
                        "Always allow cargo fmt",
                        ApprovalOptionKind::AllowAlways,
                    ),
                    option("deny", "Deny", ApprovalOptionKind::RejectOnce),
                ],
            ),
        ),
        Arm(ASK, ASK_TIMEOUT),
        Push(
            WAITING,
            notice("Waiting for your approval", NoticeTone::Info, true),
        ),
        Await(vec![ASK]),
        Report {
            approval: ASK,
            notice: WAITING,
        },
        Push(
            CLEAN,
            approval(
                "Delete target/ (2.1 GB)?",
                "rm -rf target/",
                vec![
                    option("allow-once", "Allow once", ApprovalOptionKind::AllowOnce),
                    option("deny", "Deny", ApprovalOptionKind::RejectOnce),
                ],
            ),
        ),
        Arm(CLEAN, CLEAN_TIMEOUT),
        Push(
            PUSH,
            approval(
                "Push to origin/main?",
                "git push origin main",
                vec![
                    option("allow-once", "Allow once", ApprovalOptionKind::AllowOnce),
                    option("deny", "Deny", ApprovalOptionKind::RejectOnce),
                ],
            ),
        ),
        Wait(ms(2_000)),
        Resolve(PUSH, ApprovalState::AnsweredElsewhere),
        Await(vec![CLEAN, PUSH]),
        PlanEntry(PLAN, 3, PlanEntryStatus::Completed),
        Push(REPLY, message(ChatRole::Assistant, "", true)),
        Stream(REPLY, REPLY_HEAD),
        Wait(FENCE_PAUSE),
        Stream(REPLY, REPLY_TAIL),
        Finish(REPLY),
        Usage(20_300),
        Notice(
            STATE,
            format!("Turn {turn}: started"),
            NoticeTone::Info,
            false,
        ),
        Push(
            DONE,
            notice(format!("Turn {turn} complete"), NoticeTone::Success, false),
        ),
    ]
}

const THOUGHT_TEXT: &str = "The failing case is parse(\"a,\"). The parser loops on Comma \
    without checking for a following item, so the trailing comma is accepted. I will read \
    the parser, reject the comma with its span, and move Span into its own module.";

const READ_OUTPUT: &str = r#"src/parser.rs (lines 40-52)
fn list(tokens: &mut Tokens) -> Result<Vec<Expr>, ParseError> {
    let mut items = vec![expr(tokens)?];
    while tokens.eat(Token::Comma) {
        if tokens.at_end() {
            break;
        }
        items.push(expr(tokens)?);
    }
    Ok(items)
}"#;

const PARSER_OLD: &str = r#"use crate::lexer::{Token, Tokens};

fn list(tokens: &mut Tokens) -> Result<Vec<Expr>, ParseError> {
    let mut items = vec![expr(tokens)?];
    while tokens.eat(Token::Comma) {
        if tokens.at_end() {
            break;
        }
        items.push(expr(tokens)?);
    }
    Ok(items)
}
"#;

const PARSER_NEW: &str = r#"use crate::lexer::{Token, Tokens};
use crate::span::Span;

fn list(tokens: &mut Tokens) -> Result<Vec<Expr>, ParseError> {
    let mut items = vec![expr(tokens)?];
    while let Some(comma) = tokens.eat_spanned(Token::Comma) {
        if tokens.at_end() {
            return Err(ParseError::trailing(comma));
        }
        items.push(expr(tokens)?);
    }
    Ok(items)
}
"#;

const SPAN_NEW: &str = r#"//! Source positions shared by the lexer and the parser.

/// A byte range in the input, with the column it starts at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub column: usize,
}

impl Span {
    pub fn new(start: usize, end: usize, column: usize) -> Self {
        Span { start, end, column }
    }
}
"#;

const TEST_OUTPUT: &str = r#"running 14 tests
test parser::tests::trailing_comma_is_rejected ... ok
test parser::tests::crlf_input_keeps_columns ... FAILED

failures:
---- parser::tests::crlf_input_keeps_columns stdout ----
assertion `left == right` failed
  left: 3
 right: 2

test result: FAILED. 13 passed; 1 failed; 0 ignored"#;

const REPLY_HEAD: &str = r#"## Parser fix

The parser now **rejects** a trailing comma and *keeps* the span of the token it saw, so `parse("a,")` reports column 2.

### What changed

- `Token::Comma` carries its span
- the error names the *expected* token
- [x] a test covers the trailing comma
- [ ] Windows line endings still need a fix

> The old parser accepted `a,` silently. One test still fails on CRLF input.

| File | Change | State |
|---|---:|---|
| src/parser.rs | +3 -2 | edited |
| src/span.rs | +16 | new |

The [grammar notes](https://example.com/grammar) have the full rules. A link to [run this](javascript:alert(1)) is shown as text and never opened.

<div onclick="steal()">raw HTML stays text</div>

![parse tree diagram](https://example.com/tree.png)

Build id: 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a089f86d081884c7d659a2feaa0c55ad015

```rust
pub fn parse(input: &str) -> Result<Ast, ParseError> {
    let mut tokens = Lexer::new(input).peekable();
"#;

const REPLY_TAIL: &str = r#"    let ast = expr(&mut tokens)?;
    if let Some(token) = tokens.next() {
        return Err(ParseError::trailing(token.span));
    }
    Ok(ast)
}
```

Approve the formatter next time and I will run it before the tests."#;

// ---------------------------------------------------------------------------
// The frame clock
// ---------------------------------------------------------------------------

/// The shell's frame time, shared between the pump in the view tree (which
/// writes it during paint) and the page's `build` (which reads it).
#[derive(Clone, Default)]
struct FrameClock {
    published: Rc<Cell<FrameTime>>,
    consumed: Option<FrameTime>,
}

impl FrameClock {
    /// Shell time since the previous tick, clamped to [`MAX_STEP`]; zero on
    /// the first tick and on a build the pump published nothing new for.
    fn tick(&mut self) -> Duration {
        let now = self.published.get();
        let delta = self
            .consumed
            .map_or(Duration::ZERO, |previous| now.saturating_sub(previous));
        self.consumed = Some(now);
        delta.min(MAX_STEP)
    }

    /// The zero-size view that keeps frames coming while `active`.
    fn pump(&self, active: bool) -> FramePump {
        FramePump {
            published: Rc::clone(&self.published),
            active,
        }
    }
}

/// See [`FrameClock::pump`].
struct FramePump {
    published: Rc<Cell<FrameTime>>,
    active: bool,
}

/// The pump's retained widget: zero-size, paints nothing.
struct FramePumpWidget {
    published: Rc<Cell<FrameTime>>,
    active: bool,
}

impl<State: 'static> View<State> for FramePump {
    type Element = FramePumpWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> FramePumpWidget {
        FramePumpWidget {
            published: Rc::clone(&self.published),
            active: self.active,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut FramePumpWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        element.published = Rc::clone(&self.published);
        if prev.active == self.active {
            return ChangeFlags::NONE;
        }
        element.active = self.active;
        // A pump just switched on needs one paint to ask for the next frame.
        ChangeFlags::PAINT
    }
}

impl Widget for FramePumpWidget {
    fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        bc.constrain(Size::ZERO)
    }

    fn paint(&mut self, ctx: &mut PaintCtx, _scene: &mut dyn PaintScene) {
        if self.active {
            self.published.set(ctx.frame_time());
            ctx.request_frame();
        }
    }
}

// ---------------------------------------------------------------------------
// The page
// ---------------------------------------------------------------------------

/// The page's own state (see the [module docs](self)).
pub struct ChatPage {
    script: ChatScript,
    draft: String,
    at_end: bool,
    /// The row count when the reader scrolled away from the end.
    unread_from: Option<usize>,
    controller: ScrollController,
    clock: FrameClock,
    toasts: RwSignal<Vec<String>>,
}

impl ChatPage {
    fn toast(&self, message: impl Into<String>) {
        let message = message.into();
        self.toasts.update(|queue| queue.push(message));
    }

    /// Send `text`: refused with a toast while a turn runs (Enter still
    /// submits then), otherwise the draft clears and the list follows the end.
    fn submit(&mut self, text: String) {
        if self.script.is_running() {
            self.toast("A turn is running: press stop first");
            return;
        }
        if self.script.submit(text) {
            self.draft.clear();
            self.follow_end();
        }
    }

    fn follow_end(&mut self) {
        if !self.at_end {
            self.controller.jump_to_end();
        }
    }
}

/// The Agent chat page's component; `toasts` is the catalog's toast queue.
pub struct AgentChat {
    toasts: RwSignal<Vec<String>>,
}

impl Component for AgentChat {
    type State = ChatPage;

    fn init(&self) -> ChatPage {
        ChatPage {
            script: ChatScript::new(),
            draft: String::new(),
            at_end: true,
            unread_from: None,
            controller: ScrollController::new(),
            clock: FrameClock::default(),
            toasts: self.toasts,
        }
    }

    fn build(&self, state: &mut ChatPage) -> impl View<ChatPage> {
        // One script step per frame: every chunk due since the last frame
        // lands in this single update.
        let delta = state.clock.tick();
        state.script.advance(delta);
        let running = state.script.is_running();
        let len = state.script.len();
        let unread = match (state.at_end, state.unread_from) {
            (false, Some(from)) => len.saturating_sub(from),
            _ => 0,
        };

        let theme = use_context::<Theme>().unwrap_or_else(frust_glyph::baseline);
        let scheme = theme.scheme();
        let status = if running {
            format!("turn {} running", state.script.turn)
        } else {
            format!("{len} rows, idle")
        };
        let header = Padding(
            EdgeInsets::symmetric(12.0, 8.0),
            row()
                .flex(
                    1,
                    column()
                        .child(text("Agent chat").size(15.0).color(scheme.primary))
                        .child(text(status).size(11.5).color(scheme.on_surface_variant)),
                )
                .child(
                    usage_meter(state.script.used(), CONTEXT_SIZE)
                        .label(UsageLabel::Counts)
                        .semantics_label("Context usage"),
                )
                .cross_axis(CrossAxisAlignment::Center),
        );

        let controls = Padding(
            EdgeInsets::symmetric(12.0, 0.0),
            row()
                .gap(8.0)
                .child(
                    button("Play turn", |s: &mut ChatPage| s.submit(DEMO_PROMPT.into()))
                        .style(ButtonStyle::Ghost)
                        .small(),
                )
                .child(
                    button("Load 5,000 rows", |s: &mut ChatPage| {
                        s.script.load_synthetic(SYNTHETIC_ROWS);
                        s.follow_end();
                    })
                    .style(ButtonStyle::Ghost)
                    .small(),
                )
                .child(
                    button("Restart", |s: &mut ChatPage| {
                        s.script.restart();
                        s.follow_end();
                    })
                    .style(ButtonStyle::Ghost)
                    .small(),
                ),
        );

        // Rows are keyed by their own stable id, so every approval request
        // gets its own card widget and a streaming row rebuilds alone; the
        // same id names the request, so each answer says which it was.
        let rows = Rc::clone(&state.script.rows);
        let keys = Rc::clone(&rows);
        let transcript = message_scroller(
            len,
            move |i| ChildKey::new(keys[i].id),
            move |i| {
                let id = rows[i].id;
                match &rows[i].body {
                    Body::Message {
                        role,
                        text,
                        streaming,
                        open,
                    } => EitherOf5::A(
                        message_row::<ChatPage>(*role, text.as_str())
                            .streaming(*streaming)
                            .thought_open(*open)
                            .on_thought_toggle(move |s: &mut ChatPage| {
                                s.script.toggle(id, Toggle::Thought)
                            })
                            .on_link(|s: &mut ChatPage, url: &str| s.toast(format!("Link: {url}")))
                            .on_copy(|s: &mut ChatPage, _: &str| s.toast("Copied to clipboard"))
                            .max_width(MAX_READING_WIDTH),
                    ),
                    Body::Tool(tool) => EitherOf5::B(
                        tool_call::<ChatPage>(tool.kind, tool.title.as_str(), tool.status)
                            .open(tool.open)
                            .on_toggle(move |s: &mut ChatPage| s.script.toggle(id, Toggle::Tool))
                            .contents(tool.contents.clone())
                            .on_copy(|s: &mut ChatPage, _: &str| s.toast("Copied to clipboard"))
                            .output_expanded(tool.output_expanded)
                            .on_output_toggle(move |s: &mut ChatPage| {
                                s.script.toggle(id, Toggle::Output)
                            })
                            .diff_expanded(tool.diff_expanded)
                            .on_diff_toggle(move |s: &mut ChatPage| {
                                s.script.toggle(id, Toggle::Diff)
                            }),
                    ),
                    Body::Plan(entries) => {
                        EitherOf5::C(plan_block::<ChatPage>(entries.clone()).title("Plan"))
                    }
                    Body::Approval(a) => {
                        let mut card =
                            approval_card::<ChatPage>(a.title.as_str(), a.options.clone())
                                .request_id(id.to_string())
                                .detail(a.detail.as_str())
                                .state(a.state)
                                .expired_hint("Send the prompt again to retry")
                                .on_select(move |s: &mut ChatPage, picked: &ApprovalSelection| {
                                    s.script.answer(id, &picked.option_id);
                                });
                        if let Some(deadline) = &a.deadline {
                            card = card.expires(deadline.clone());
                        }
                        if let Some(selected) = &a.selected {
                            card = card.selected(selected.as_str());
                        }
                        EitherOf5::D(card)
                    }
                    Body::Notice {
                        text,
                        tone,
                        working,
                    } => EitherOf5::E(
                        notice_row::<ChatPage>(text.as_str())
                            .tone(*tone)
                            .working(*working),
                    ),
                }
            },
        )
        .controller(state.controller.clone())
        .at_end(state.at_end)
        .on_at_end_change(|s: &mut ChatPage, at_end: bool| {
            s.at_end = at_end;
            s.unread_from = (!at_end).then(|| s.script.len());
        })
        .unread(unread)
        .padding(EdgeInsets::symmetric(12.0, 12.0));

        let input = Padding(
            EdgeInsets::symmetric(12.0, 8.0),
            composer::<ChatPage>(state.draft.clone())
                .placeholder("Ask the scripted agent")
                .submit_on_enter(true)
                .running(running)
                .on_change(|s: &mut ChatPage, draft: String| s.draft = draft)
                .on_submit(|s: &mut ChatPage, prompt: String| s.submit(prompt))
                .on_stop(|s: &mut ChatPage| {
                    s.script.stop();
                }),
        );

        column()
            .child(header)
            .child(controls)
            .flex(1, transcript)
            .child(input)
            .child(state.clock.pump(running))
            .cross_axis(CrossAxisAlignment::Stretch)
    }
}

/// See the page-fn contract in [`crate::pages`]. The page fills the section
/// body (the shell gives it no scroll view of its own).
pub fn page(state: &CatalogState) -> impl View<CatalogState> + use<> {
    component(AgentChat {
        toasts: state.toasts,
    })
}
