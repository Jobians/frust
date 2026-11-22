//! The read-only geometry of a laid-out `TextLayout` through the public API:
//! range rectangles (one line, a soft wrap, a newline, right-to-left and mixed
//! direction text, two font sizes on one line, an empty range, a `max_lines`
//! cut), line boxes, point-to-position round trips, word ranges and agreement
//! with `TextEditor`'s own selection and caret geometry.
//!
//! Latin text shapes in the host's system face, as the crate's other layout
//! tests do, so the assertions are structural (order, adjacency, containment,
//! equality with the editor over the same text) rather than pixel values. The
//! Arabic and Thai text shapes in the bundled faces.

use frust_text::{
    EditOp, EditingStateBytes, TextAffinity, TextContext, TextEditor, TextLayout, TextOverflow,
    TextPosition, TextSpan, TextStyle,
};
use kurbo::{Point, Rect};

const ARABIC_FACE: &[u8] = include_bytes!("../../../testing/fonts/NotoSansArabic-Subset.ttf");
const THAI_FACE: &[u8] = include_bytes!("../../../testing/fonts/NotoSansThai-Subset.ttf");

/// The Arabic word the bundled Arabic face covers.
const MARHABA: &str = "\u{645}\u{631}\u{62d}\u{628}\u{627}";
/// The Thai sentence the bundled Thai face covers.
const THAI: &str = "\u{e2a}\u{e27}\u{e31}\u{e2a}\u{e14}\u{e35}\u{e04}\u{e23}\u{e31}\u{e1a}\u{e44}\u{e01}\u{e48}\u{e15}\u{e49}\u{e21}\u{e22}\u{e33}";

fn context() -> TextContext {
    let mut cx = TextContext::new();
    for face in [ARABIC_FACE, THAI_FACE] {
        cx.register_fonts(face.to_vec())
            .expect("bundled face registers");
    }
    cx
}

fn style(size: f32) -> TextStyle {
    TextStyle::default().with_size(size)
}

fn down(index: usize) -> TextPosition {
    TextPosition::new(index, TextAffinity::Downstream)
}

fn up(index: usize) -> TextPosition {
    TextPosition::new(index, TextAffinity::Upstream)
}

/// The index of the line whose box holds `y`.
fn line_at(layout: &TextLayout, y: f64) -> usize {
    (0..layout.line_count())
        .find(|&l| {
            layout
                .line_bounds(l)
                .is_some_and(|(top, bottom)| y >= top && y < bottom)
        })
        .expect("y lies inside the layout")
}

fn mid_y(rect: &Rect) -> f64 {
    (rect.y0 + rect.y1) / 2.0
}

/// The first byte index of each visual line, from the caret rects of every
/// char boundary (downstream, so a soft-wrap boundary draws on the next line).
fn line_starts(layout: &TextLayout, text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    let mut line = 0;
    for (i, _) in text.char_indices().skip(1) {
        let at = line_at(layout, mid_y(&layout.caret_rect(down(i), 0.0)));
        if at != line {
            line = at;
            starts.push(i);
        }
    }
    starts
}

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-3
}

/// An editor over `text` wrapped at `width`, selecting `range`.
fn editor_selecting(
    cx: &mut TextContext,
    text: &str,
    style: &TextStyle,
    width: Option<f32>,
    range: std::ops::Range<usize>,
) -> TextEditor {
    let mut ed = TextEditor::new(style);
    ed.set_wrap_width(width, cx);
    ed.apply(
        EditOp::ApplyEditingState(EditingStateBytes {
            text: text.to_owned(),
            base: range.start,
            extent: range.end,
            composing: None,
        }),
        cx,
    );
    ed
}

#[test]
fn a_range_inside_one_line_is_one_rect_between_its_carets() {
    let mut cx = context();
    let text = "Hello wide world";
    let layout = cx.layout(text, &style(20.0), None);
    let rects = layout.range_rects(6..10);
    assert_eq!(rects.len(), 1, "{rects:?}");
    let r = rects[0];
    let start = layout.caret_rect(down(6), 0.0);
    let end = layout.caret_rect(up(10), 0.0);
    assert!(
        approx(r.x0, start.x0) && approx(r.x1, end.x0),
        "{r:?} {start:?} {end:?}"
    );
    assert!(r.x0 > 0.0 && r.x1 < layout.size().width);
    assert_eq!((r.y0, r.y1), (start.y0, start.y1), "the caret box");
}

#[test]
fn line_boxes_tile_the_layout_and_hold_their_caret_boxes() {
    let mut cx = context();
    let text = "Hello wide world, the quick brown fox jumps over the lazy dog";
    let layout = cx.layout(text, &style(18.0), Some(90.0));
    let lines = layout.line_count();
    assert!(lines > 2, "{lines} lines");
    assert_eq!(layout.line_bounds(0).map(|b| b.0), Some(0.0));
    for line in 1..lines {
        assert_eq!(
            layout.line_bounds(line - 1).map(|b| b.1),
            layout.line_bounds(line).map(|b| b.0),
            "line {line} starts where the previous ends"
        );
    }
    assert_eq!(
        layout.line_bounds(lines - 1).map(|b| b.1),
        Some(layout.size().height)
    );
    assert_eq!(layout.line_bounds(lines), None);
    // Each caret box sits within a pixel of its line box.
    for (line, start) in line_starts(&layout, text).into_iter().enumerate() {
        let caret = layout.caret_rect(down(start), 0.0);
        let (top, bottom) = layout.line_bounds(line).expect("line exists");
        assert!((caret.y0 - top).abs() <= 1.0 && (caret.y1 - bottom).abs() <= 1.0);
    }
}

#[test]
fn a_range_across_a_soft_wrap_is_one_rect_per_line() {
    let mut cx = context();
    let text = "alpha beta gamma delta epsilon zeta";
    let layout = cx.layout(text, &style(18.0), Some(100.0));
    let starts = line_starts(&layout, text);
    assert!(starts.len() >= 3, "the text wraps: {starts:?}");
    // From inside line 0 to inside line 2.
    let range = 2..starts[2] + 2;
    let rects = layout.range_rects(range.clone());
    assert_eq!(rects.len(), 3, "{rects:?}");
    let lines: Vec<usize> = rects.iter().map(|r| line_at(&layout, mid_y(r))).collect();
    assert_eq!(lines, [0, 1, 2]);
    assert!(approx(rects[0].x0, layout.caret_rect(down(2), 0.0).x0));
    // The middle line is covered from its start.
    assert!(approx(
        rects[1].x0,
        layout.caret_rect(down(starts[1]), 0.0).x0
    ));
    assert!(approx(
        rects[2].x1,
        layout.caret_rect(up(range.end), 0.0).x0
    ));
}

#[test]
fn a_range_across_a_newline_covers_it_with_a_stub() {
    let mut cx = context();
    let text = "Hello\nworld";
    let layout = cx.layout(text, &style(20.0), None);
    let rects = layout.range_rects(2..9);
    assert_eq!(rects.len(), 2, "{rects:?}");
    assert_eq!(line_at(&layout, mid_y(&rects[0])), 0);
    assert_eq!(line_at(&layout, mid_y(&rects[1])), 1);
    let line_end = layout.caret_rect(down(5), 0.0).x0;
    assert!(rects[0].x1 > line_end, "the newline's stub: {rects:?}");
    assert!(approx(rects[1].x0, layout.caret_rect(down(6), 0.0).x0));
    // Ending before the newline adds no stub.
    let before = layout.range_rects(2..5);
    assert_eq!(before.len(), 1);
    assert!(approx(before[0].x1, line_end));
}

#[test]
fn a_right_to_left_range_lies_at_the_right_of_its_line() {
    let mut cx = context();
    let text = format!("{MARHABA} {MARHABA}");
    let layout = cx.layout(&text, &style(24.0), None);
    let first = layout.range_rects(0..MARHABA.len());
    let second = layout.range_rects(MARHABA.len() + 1..text.len());
    assert_eq!((first.len(), second.len()), (1, 1), "{first:?} {second:?}");
    assert!(
        first[0].x0 > second[0].x1,
        "the first word reads rightmost: {first:?} {second:?}"
    );
    // A caret downstream of the first word's start sits at its right edge.
    assert!(approx(layout.caret_rect(down(0), 0.0).x0, first[0].x1));
}

#[test]
fn a_range_across_a_bidi_boundary_splits_on_its_line() {
    let mut cx = context();
    let text = format!("Hello {MARHABA} Hello");
    let layout = cx.layout(&text, &style(24.0), None);
    let arabic_start = "Hello ".len();
    // "lo " plus the first two Arabic letters: logically contiguous, visually
    // two pieces (the Latin end, and the right end of the Arabic run).
    let range = 3..arabic_start + 4;
    let rects = layout.range_rects(range);
    assert_eq!(rects.len(), 2, "{rects:?}");
    let (a, b) = (rects[0], rects[1]);
    assert_eq!(a.y0, b.y0, "one line");
    assert!(a.x1 <= b.x0 || b.x1 <= a.x0, "disjoint: {rects:?}");
    let arabic = layout.range_rects(arabic_start..arabic_start + MARHABA.len());
    assert_eq!(arabic.len(), 1);
    // One piece is the Arabic run's right end.
    assert!(
        rects.iter().any(|r| approx(r.x1, arabic[0].x1)),
        "{rects:?} {arabic:?}"
    );
}

#[test]
fn a_rich_line_with_two_font_sizes_shares_one_box() {
    let mut cx = context();
    let small = style(12.0);
    let big = style(30.0);
    let spans = [TextSpan::new("small ", &small), TextSpan::new("BIG", &big)];
    let layout = cx.layout_rich(&spans, None);
    assert_eq!(layout.line_count(), 1);
    let a = layout.range_rects(0..6);
    let b = layout.range_rects(6..9);
    assert_eq!((a.len(), b.len()), (1, 1));
    assert_eq!((a[0].y0, a[0].y1), (b[0].y0, b[0].y1), "one line box");
    assert!(approx(a[0].x1, b[0].x0), "the spans touch");
    assert!(b[0].width() > a[0].width() / 2.0, "the big span is wide");
    let small_alone = cx.layout("small ", &small, None);
    assert!(
        a[0].height() > small_alone.range_rects(0..6)[0].height(),
        "the big span's line is taller than the small text's own"
    );
    let both = layout.range_rects(0..9);
    assert_eq!(both, vec![Rect::new(a[0].x0, a[0].y0, b[0].x1, a[0].y1)]);
}

#[test]
fn an_empty_range_and_an_empty_text_cover_nothing() {
    let mut cx = context();
    let layout = cx.layout("Hello", &style(20.0), None);
    assert!(layout.range_rects(2..2).is_empty());
    let empty = cx.layout("", &style(20.0), None);
    assert!(empty.range_rects(0..5).is_empty());
    assert_eq!(empty.position_at_point(Point::new(10.0, 5.0)).index, 0);
    assert_eq!(empty.word_range_at(down(0)), 0..0);
}

#[test]
fn a_range_cut_by_max_lines_covers_only_the_kept_lines() {
    let mut cx = context();
    let text = "alpha beta gamma delta epsilon zeta eta theta";
    let full = cx.layout(text, &style(18.0), Some(100.0));
    let starts = line_starts(&full, text);
    assert!(starts.len() >= 3, "{starts:?}");
    for overflow in [TextOverflow::Clip, TextOverflow::Ellipsis] {
        let cut = cx.layout_bounded(text, &style(18.0), Some(100.0), Some(2), overflow);
        assert_eq!(cut.line_count(), 2);
        let rects = cut.range_rects(2..text.len());
        assert!(!rects.is_empty());
        let bottom = cut.line_bounds(1).expect("two lines").1;
        assert!(
            rects.iter().all(|r| mid_y(r) < bottom),
            "{overflow:?}: {rects:?} below {bottom}"
        );
        assert!(
            cut.range_rects(starts[2] + 4..text.len()).is_empty(),
            "{overflow:?}: a range wholly past the cut covers nothing"
        );
        if overflow == TextOverflow::Ellipsis {
            assert!(cut.is_ellipsized());
            // No rect reaches over the ellipsis glyph at the line's end.
            let last = rects.last().expect("rects");
            assert!(last.x1 <= cut.size().width + 1e-3, "{last:?}");
        }
    }
}

#[test]
fn an_ellipsis_on_a_right_to_left_line_keeps_the_text_beside_it() {
    let mut cx = context();
    let text = [MARHABA; 6].join(" ");
    let cut = cx.layout_bounded(
        &text,
        &style(24.0),
        Some(150.0),
        Some(1),
        TextOverflow::Ellipsis,
    );
    assert!(cut.is_ellipsized());
    // The ellipsis draws at the line's left (its end, right to left), so it
    // is the leftmost glyph; the kept text's rects start right of it.
    let leftmost = cut
        .to_scene_runs(Point::ORIGIN)
        .iter()
        .flat_map(|run| run.glyphs.iter().map(|g| f64::from(g.x)))
        .fold(f64::INFINITY, f64::min);
    let rects = cut.range_rects(0..text.len());
    assert!(!rects.is_empty());
    let left = rects.iter().map(|r| r.x0).fold(f64::INFINITY, f64::min);
    assert!(
        left > leftmost + 1.0,
        "{rects:?} vs the ellipsis at {leftmost}"
    );
    let right = rects.iter().map(|r| r.x1).fold(f64::NEG_INFINITY, f64::max);
    assert!(
        approx(cut.caret_rect(down(0), 0.0).x0, right),
        "the text starts at the right"
    );
}

#[test]
fn every_caret_position_round_trips_through_its_point() {
    let mut cx = context();
    let text = "Hello wide world";
    let layout = cx.layout(text, &style(20.0), None);
    for (i, _) in text.char_indices().chain([(text.len(), ' ')]) {
        let caret = layout.caret_rect(down(i), 0.0);
        let at = layout.position_at_point(Point::new(caret.x0 + 0.5, mid_y(&caret)));
        assert_eq!(at.index, i, "a point just right of caret {i}");
        if i > 0 {
            let at = layout.position_at_point(Point::new(caret.x0 - 0.5, mid_y(&caret)));
            assert_eq!(at.index, i, "a point just left of caret {i}");
        }
    }
    // Past either end of the line, and above and below the layout.
    assert_eq!(layout.position_at_point(Point::new(-50.0, 5.0)).index, 0);
    let end = layout.position_at_point(Point::new(1e4, 5.0));
    assert_eq!(end.index, text.len());
    assert_eq!(layout.position_at_point(Point::new(-5.0, -50.0)).index, 0);
    assert_eq!(
        layout.position_at_point(Point::new(1e4, 500.0)).index,
        text.len()
    );
}

#[test]
fn a_wrapped_point_round_trips_on_every_line() {
    let mut cx = context();
    let text = "alpha beta gamma delta epsilon zeta";
    let layout = cx.layout(text, &style(18.0), Some(100.0));
    for start in line_starts(&layout, text) {
        let caret = layout.caret_rect(down(start), 0.0);
        let at = layout.position_at_point(Point::new(caret.x0 + 0.5, mid_y(&caret)));
        assert_eq!(at, down(start), "line start {start}");
    }
}

#[test]
fn a_word_range_spans_the_latin_word_around_a_position() {
    let mut cx = context();
    let text = "Hello wide world";
    let layout = cx.layout(text, &style(20.0), None);
    assert_eq!(layout.word_range_at(down(2)), 0..5);
    assert_eq!(layout.word_range_at(down(0)), 0..5);
    assert_eq!(
        layout.word_range_at(up(5)),
        0..5,
        "upstream of a word's end"
    );
    assert_eq!(
        layout.word_range_at(down(5)),
        5..6,
        "the space is its own word"
    );
    assert_eq!(layout.word_range_at(down(8)), 6..10);
    assert_eq!(layout.word_range_at(up(text.len())), 11..16);
    // Through a point: the word under it.
    let mid_wide = layout.range_rects(6..10)[0].center();
    assert_eq!(
        layout.word_range_at(layout.position_at_point(mid_wide)),
        6..10
    );
    // The right half of a word's last letter still selects that word.
    let o = layout.range_rects(4..5)[0];
    let right_of_o = Point::new(o.x1 - 0.5, o.center().y);
    assert_eq!(
        layout.word_range_at(layout.position_at_point(right_of_o)),
        0..5
    );
}

#[test]
fn an_unspaced_thai_sentence_splits_into_dictionary_words() {
    let mut cx = context();
    let layout = cx.layout(THAI, &style(20.0), None);
    let mut words = Vec::new();
    let mut at = 0;
    while at < THAI.len() {
        let word = layout.word_range_at(down(at));
        assert_eq!(word.start, at, "words tile the sentence");
        assert!(word.end > at);
        at = word.end;
        words.push(word);
    }
    assert!(
        words.len() > 1,
        "dictionary segmentation finds words: {words:?}"
    );
    assert!(words.iter().all(|w| THAI.is_char_boundary(w.start)));
}

#[test]
fn range_rects_agree_with_the_editors_selection_rects() {
    let mut cx = context();
    let text = "Hello wide world,\nthe quick brown fox jumps over the lazy dog";
    let sty = style(18.0);
    for width in [None, Some(120.0)] {
        let layout = cx.layout(text, &sty, width);
        for range in [0..5, 3..14, 14..20, 6..40, 0..text.len(), 17..18, 25..26] {
            let ed = editor_selecting(&mut cx, text, &sty, width, range.clone());
            assert_eq!(
                layout.range_rects(range.clone()),
                ed.selection_rects(),
                "{range:?} at {width:?}"
            );
        }
    }
}

#[test]
fn caret_rects_agree_with_the_editors_cursor_rect() {
    let mut cx = context();
    let text = "Hello wide world, the quick brown fox";
    let sty = style(18.0);
    let layout = cx.layout(text, &sty, Some(120.0));
    for i in [0, 3, 6, 17, 22, text.len()] {
        let ed = editor_selecting(&mut cx, text, &sty, Some(120.0), i..i);
        assert_eq!(
            Some(layout.caret_rect(down(i), 1.5)),
            ed.cursor_rect(1.5),
            "{i}"
        );
    }
}
