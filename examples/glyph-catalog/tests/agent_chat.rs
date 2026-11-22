//! The Agent chat page, headless: the page builds and rests without frame
//! requests, the scripted driver produces every row kind, the 5,000-row load
//! materializes only the visible window, and the reply's unsafe link, raw HTML
//! and image reference reach the screen as text. Same `RenderRoot` harness
//! shape as `tests/smoke.rs`.

use std::any::Any;
use std::time::Duration;

use frust::accesskit::Role;
use frust::{AnyView, Brightness, GetUntracked, Theme, provide_context};
use frust_core::{FrameTime, InputEvent, PaintOutcome, PointerButton, PointerEvent, PointerPhase};
use frust_core::{PaintScene, RenderRoot};
use frust_glyph::chat::{ApprovalState, PlanEntryStatus, ToolStatus};
use frust_reactive::ReactiveRuntime;
use frust_scene::GlyphRun;
use frust_text::TextContext;
use kurbo::{BezPath, Point, Rect, Size};
use peniko::{Brush, Color};
use reactive_graph::owner::Owner;

use glyphcatalog::CatalogState;
use glyphcatalog::pages::agent_chat::{
    ChatScript, DEMO_PROMPT, INITIAL_USED, RowKind, SYNTHETIC_ROWS,
};
use glyphcatalog::pages::{self, AGENT_CHAT};

/// One frame of script or shell time.
const FRAME_MS: u64 = 50;

/// A paint target that records nothing; the tests read semantics.
struct NullScene;

impl PaintScene for NullScene {
    fn fill_rect(&mut self, _origin: Point, _size: Size, _color: Color) {}
    fn draw_text(&mut self, _origin: Point, _text: &str) {}
    fn fill_rounded_rect(&mut self, _origin: Point, _size: Size, _radius: f64, _color: Color) {}
    fn fill_rounded_rect_brush(
        &mut self,
        _origin: Point,
        _size: Size,
        _radius: f64,
        _brush: &Brush,
    ) {
    }
    fn draw_glyph_run(&mut self, _run: GlyphRun) {}
    fn draw_image(&mut self, _data: &peniko::ImageData, _dest: Rect) {}
    fn fill_path(&mut self, _origin: Point, _path: &BezPath, _brush: &Brush) {}
    fn stroke_path(&mut self, _origin: Point, _path: &BezPath, _width: f64, _brush: &Brush) {}
}

/// One semantics node, flattened: role, label, value and centre point.
#[derive(Debug)]
struct Node {
    role: Role,
    label: String,
    value: String,
    centre: Option<Point>,
}

impl Node {
    fn text(&self) -> String {
        format!("{} {}", self.label, self.value)
    }
}

/// The Agent chat page mounted on its own under the Glyph theme, at a
/// virtual frame clock the harness advances by [`FRAME_MS`] per frame.
struct Page {
    _owner: Owner,
    root: RenderRoot<CatalogState, AnyView<CatalogState>>,
    tcx: TextContext,
    state: CatalogState,
    size: Size,
    t_ms: u64,
}

impl Page {
    fn mount(width: f64, height: f64) -> Self {
        let _ = ReactiveRuntime::init(std::sync::Arc::new(|| {}));
        let owner = Owner::new();
        owner.set();
        let theme = Theme::builder(frust_glyph::baseline())
            .brightness(Brightness::Dark)
            .build();
        provide_context(theme.clone());
        let mut root = RenderRoot::new();
        root.set_theme(Box::new(theme));
        let mut page = Page {
            _owner: owner,
            root,
            tcx: TextContext::new(),
            state: CatalogState::new(),
            size: Size::new(width, height),
            t_ms: 0,
        };
        page.frame();
        page
    }

    /// Rebuild, lay out and paint one frame, then advance the clock.
    fn frame(&mut self) -> PaintOutcome {
        let mut logic = |s: &mut CatalogState| pages::current(AGENT_CHAT, s);
        self.root.rebuild(&mut logic, &mut self.state);
        let tcx: &mut dyn Any = &mut self.tcx;
        self.root.layout_with_text(self.size, tcx);
        let outcome = self
            .root
            .paint(&mut NullScene, FrameTime::from_nanos(self.t_ms * 1_000_000));
        self.t_ms += FRAME_MS;
        outcome
    }

    fn nodes(&mut self) -> Vec<Node> {
        self.root
            .semantics()
            .nodes
            .iter()
            .map(|(_, n)| Node {
                role: n.role(),
                label: n.label().unwrap_or_default().to_string(),
                value: n.value().unwrap_or_default().to_string(),
                centre: n
                    .bounds()
                    .map(|b| Point::new((b.x0 + b.x1) / 2.0, (b.y0 + b.y1) / 2.0)),
            })
            .collect()
    }

    fn has(&mut self, role: Role, label: &str) -> bool {
        self.nodes()
            .iter()
            .any(|n| n.role == role && n.label == label)
    }

    fn any_text(&mut self, needle: &str) -> bool {
        self.nodes().iter().any(|n| n.text().contains(needle))
    }

    /// Tap the first button labelled `label`; reports whether one was found.
    fn tap(&mut self, label: &str) -> bool {
        let Some(at) = self
            .nodes()
            .into_iter()
            .find(|n| n.role == Role::Button && n.label == label)
            .and_then(|n| n.centre)
        else {
            return false;
        };
        for phase in [PointerPhase::Down, PointerPhase::Up] {
            let event = InputEvent::Pointer(PointerEvent {
                phase,
                position: at,
                button: PointerButton::Primary,
            });
            self.root.event(&mut self.state, &event);
        }
        true
    }

    fn toasts(&self) -> Vec<String> {
        self.state.toasts.get_untracked()
    }
}

#[test]
fn the_page_builds_and_rests_without_frame_requests() {
    for (w, h) in [(390.0, 844.0), (1280.0, 800.0)] {
        let mut page = Page::mount(w, h);
        let outcome = page.frame();
        assert!(!outcome.needs_frame, "an idle page requests no frames");
        assert!(page.has(Role::Log, "Messages"), "the transcript region");
        assert!(page.has(Role::Button, "Send"), "the composer");
        for control in ["Play turn", "Load 5,000 rows", "Restart"] {
            assert!(page.has(Role::Button, control), "{control} control");
        }
        assert!(page.any_text("Scripted agent"), "the opening notice");
        assert!(page.any_text("Agent chat"), "the header title");
    }
}

/// Step `script` one frame at a time until its turn ends, answering the first
/// approval it raises with `answer`. Returns the frames taken and the longest
/// run of consecutive frames the reply's code fence was open.
fn play(script: &mut ChatScript, answer: &str) -> (usize, usize) {
    let mut answered = false;
    let mut open_frames = 0;
    let mut longest_open = 0;
    for frame in 0..4_000 {
        if !script.advance(Duration::from_millis(FRAME_MS)) {
            return (frame, longest_open);
        }
        if !answered && let Some(&id) = script.pending_approvals().first() {
            assert!(script.answer(id, answer), "the first request is answerable");
            answered = true;
        }
        let fences = script
            .assistant_text()
            .map_or(0, |text| text.matches("```").count());
        if fences % 2 == 1 {
            open_frames += 1;
            longest_open = longest_open.max(open_frames);
        } else {
            open_frames = 0;
        }
    }
    panic!("the turn never finished");
}

#[test]
fn driving_the_script_produces_every_row_kind() {
    let mut script = ChatScript::new();
    assert_eq!(script.kinds(), [RowKind::Notice]);
    assert!(script.submit("Fix the parser"));
    assert!(!script.submit("A second prompt"), "one turn at a time");

    // The first frames already show the turn's state notice and thought.
    for _ in 0..20 {
        script.advance(Duration::from_millis(FRAME_MS));
    }
    let early = script.kinds();
    assert_eq!(
        &early[..4],
        [
            RowKind::Notice,
            RowKind::User,
            RowKind::Notice,
            RowKind::Thought
        ]
    );

    let (frames, fence_open) = play(&mut script, "allow-once");
    assert!(frames > 100, "a turn streams over many frames ({frames})");
    assert!(
        fence_open >= 20,
        "the code fence stays open for a while ({fence_open} frames)"
    );
    assert!(!script.is_running());

    let kinds = script.kinds();
    for kind in [
        RowKind::User,
        RowKind::Assistant,
        RowKind::Thought,
        RowKind::Tool,
        RowKind::Plan,
        RowKind::Approval,
        RowKind::Notice,
    ] {
        assert!(kinds.contains(&kind), "{kind:?} in {kinds:?}");
    }
    assert_eq!(
        script.tool_statuses(),
        [
            ToolStatus::Completed,
            ToolStatus::Completed,
            ToolStatus::Completed,
            ToolStatus::Failed
        ]
    );
    assert_eq!(
        script.approval_states(),
        [
            ApprovalState::Allowed,
            ApprovalState::Expired,
            ApprovalState::AnsweredElsewhere
        ]
    );
    assert!(
        script
            .plan_statuses()
            .iter()
            .all(|s| *s == PlanEntryStatus::Completed)
    );
    let reply = script.assistant_text().expect("the reply");
    assert_eq!(reply.matches("```").count(), 2, "the fence closed");
    assert!(reply.ends_with("before the tests."));
    assert!(script.used() > INITIAL_USED);

    // Ids only grow, so every row has its own list key.
    assert!(script.ids().windows(2).all(|w| w[0] < w[1]));
}

#[test]
fn usage_rises_across_turns_and_stop_interrupts_the_stream() {
    let mut script = ChatScript::new();
    let mut used = script.used();
    for turn in 0..2 {
        assert!(script.submit(format!("turn {turn}")));
        play(&mut script, "deny");
        assert!(script.used() > used);
        used = script.used();
    }
    assert!(
        used as f64 / glyphcatalog::pages::agent_chat::CONTEXT_SIZE as f64 >= 0.8,
        "two turns reach the warning tone ({used})"
    );

    assert!(script.submit("one more"));
    for _ in 0..40 {
        script.advance(Duration::from_millis(FRAME_MS));
    }
    assert!(script.stop());
    assert!(!script.is_running());
    assert!(!script.advance(Duration::from_millis(FRAME_MS)));
    assert!(script.pending_approvals().is_empty());
    assert!(script.tool_statuses().iter().all(|s| s.is_finished()));
    assert_eq!(script.kinds().last(), Some(&RowKind::Notice));
}

#[test]
fn the_five_thousand_row_load_builds_only_the_visible_window() {
    let mut page = Page::mount(390.0, 844.0);
    assert!(page.tap("Load 5,000 rows"));
    page.frame();
    page.frame();
    assert!(page.any_text(&format!("{SYNTHETIC_ROWS} rows, idle")));

    let nodes = page.nodes();
    let synthetic = nodes
        .iter()
        .filter(|n| n.text().contains("Synthetic"))
        .count();
    assert!(synthetic > 0, "the newest rows are on screen");
    assert!(
        synthetic < 200,
        "only the visible window is built ({synthetic} synthetic nodes)"
    );
    assert!(
        nodes.len() < 1_000,
        "the tree stays small ({} nodes)",
        nodes.len()
    );
    assert!(
        nodes
            .iter()
            .any(|n| n.text().contains("Synthetic checkpoint 4999")),
        "stuck to the end"
    );
    assert!(
        !nodes
            .iter()
            .any(|n| n.text().trim() == "Synthetic prompt 0"),
        "the first row is not built"
    );
}

#[test]
fn the_reply_renders_unsafe_links_html_and_images_as_text() {
    let mut page = Page::mount(390.0, 4_000.0);
    assert!(page.tap("Play turn"));
    page.frame();
    // A second prompt while the turn runs is refused with a toast.
    assert!(page.tap("Play turn"));
    assert!(
        page.toasts().iter().any(|t| t.contains("press stop first")),
        "{:?}",
        page.toasts()
    );

    let mut answered = false;
    for _ in 0..2_000 {
        page.frame();
        if !answered && page.tap("Allow once") {
            answered = true;
        }
        if page.any_text("rows, idle") {
            break;
        }
    }
    assert!(answered, "the approval card was answered by a tap");
    assert!(page.any_text("rows, idle"), "the turn finished");
    assert!(
        !page.frame().needs_frame,
        "a finished turn requests no frames"
    );

    let nodes = page.nodes();
    let shows = |needle: &str| nodes.iter().any(|n| n.text().contains(needle));
    assert!(
        shows("run this (javascript:alert(1))"),
        "unsafe link as text"
    );
    assert!(
        !nodes
            .iter()
            .any(|n| n.role == Role::Link && n.text().contains("run this")),
        "the unsafe link is not a link"
    );
    assert!(
        nodes
            .iter()
            .any(|n| n.role == Role::Link && n.text().contains("grammar notes")),
        "the safe link is a link"
    );
    assert!(
        shows(r#"<div onclick="steal()">raw HTML stays text</div>"#),
        "HTML as text"
    );
    assert!(
        shows("parse tree diagram (https://example.com/tree.png)"),
        "image reference as text"
    );
    assert!(
        shows("Approved: cargo fmt --all ran."),
        "the answer reported"
    );
    assert!(shows(DEMO_PROMPT), "the prompt the control sent");
    assert!(!page.toasts().iter().any(|t| t.starts_with("Link")));
}
