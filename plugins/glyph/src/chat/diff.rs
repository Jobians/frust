//! [`diff_view`]: the edit a tool made to one file, shown as a unified line
//! diff that [`compute_diff`] works out here from the old and new text (the
//! `imara-diff` Histogram algorithm, then its line postprocessing).
//!
//! # Contract
//!
//! - **Controlled.** `.collapsed(bool)` and `.on_toggle(..)` work like
//!   `crate::accordion`: the widget reports a header or show-all press and
//!   never flips its own state. Only a diff with more rows than
//!   `.collapse_above(..)` collapses (to its header, totals and one show-all
//!   row); a smaller one always shows every row.
//! - **Computed once per input.** The model is rebuilt only when the old or
//!   new text (or the context) differs from the previous build.
//! - **Line semantics.** A trailing `\r` is ignored in comparison and never
//!   drawn, and a final line without a newline equals the same line with one,
//!   so an edit that only adds or drops the last newline reads as no change.
//!   Above [`DIFF_MAX_BYTES`] or [`DIFF_MAX_LINES`] on either side nothing is
//!   diffed: the model is flagged `too_large` and carries line counts only.
//! - **One horizontal offset.** Rows never wrap; a horizontal drag or wheel
//!   scrolls every row's text together while the gutter stays put. A drag
//!   that turns vertical first is declined, so the enclosing transcript
//!   scrolls instead.
//! - **Not colour alone.** Added and removed rows carry a `+`/`-` marker
//!   column as well as their wash.

use std::rc::Rc;

use frust::authoring::text::{TextContext, TextLayout, TextStyle};
use frust::authoring::{
    Action, BoxConstraints, Brush, BuildCtx, ChangeFlags, Color, ErasedCallback, EventCtx,
    EventResult, InputEvent, LayoutCtx, PaintCtx, PaintScene, Point, PointerPhase, Rect, Role,
    RoundedRect, ScrollDelta, SemanticsCtx, Shape, Size, View, Widget, erase_callback,
};
use frust::input::{TOUCH_SLOP, WHEEL_LINE_PX};
use frust::{ColorScheme, StatusPalette, Theme};
use imara_diff::{Algorithm, Diff, Hunk, InternedInput};

use super::model::DiffInput;
use super::style;
use crate::badge::LayoutGuard;
use crate::press::presses;

/// Unchanged lines kept around each change when no `.context(..)` is given.
pub const DIFF_DEFAULT_CONTEXT: usize = 3;
/// Rows above which a collapsed diff shows only its header, totals and a
/// show-all row, when no `.collapse_above(..)` is given.
pub const DIFF_DEFAULT_COLLAPSE_ABOVE: usize = 40;
/// Largest side, in bytes, that [`compute_diff`] still diffs.
pub const DIFF_MAX_BYTES: usize = 512 * 1024;
/// Largest side, in lines, that [`compute_diff`] still diffs.
pub const DIFF_MAX_LINES: usize = 5_000;

/// Gap between the gutter, the marker column and the row text.
const COLUMN_GAP: f64 = style::PAD_SMALL;
/// Vertical padding inside one row.
const ROW_PAD_Y: f64 = 1.0;
/// Gap between the header's path and its totals, and between the totals.
const TOTALS_GAP: f64 = style::BLOCK_GAP;
/// Spaces a tab expands to on screen.
const TAB_WIDTH: usize = 4;
/// Width used when the incoming constraints are unbounded.
const FALLBACK_WIDTH: f64 = 480.0;
/// Corner-rounding tolerance for the outline stroke.
const PATH_TOLERANCE: f64 = 0.1;
/// Middle-elision mark for a long path (present in the bundled mono face).
const ELLIPSIS: char = '\u{2026}';

const DARK: ColorScheme = crate::tokens::color::dark();
const DARK_STATUS: StatusPalette = crate::tokens::color::status();
/// Unthemed row text.
const FALLBACK_TEXT: Color = DARK.on_surface;
/// Unthemed added marker and total.
const FALLBACK_ADDED_FG: Color = DARK_STATUS.dark.success;
/// Unthemed removed marker and total.
const FALLBACK_REMOVED_FG: Color = DARK.error;

/// What one diff row is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiffLineKind {
    /// Unchanged, shown for context.
    Context,
    /// Present only in the new text.
    Added,
    /// Present only in the old text.
    Removed,
}

impl DiffLineKind {
    /// The marker column's character: `+`, `-`, or a space.
    pub fn marker(self) -> char {
        match self {
            Self::Context => ' ',
            Self::Added => '+',
            Self::Removed => '-',
        }
    }
}

/// One line of a hunk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    /// What the line is.
    pub kind: DiffLineKind,
    /// 1-based line number in the old text; `None` for an added line.
    pub old_line: Option<u32>,
    /// 1-based line number in the new text; `None` for a removed line.
    pub new_line: Option<u32>,
    /// The line without its line ending.
    pub text: String,
}

/// A run of changes with their surrounding context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffHunk {
    /// First old line covered (the line before it when `old_count` is 0).
    pub old_start: u32,
    /// Old lines covered.
    pub old_count: u32,
    /// First new line covered (the line before it when `new_count` is 0).
    pub new_start: u32,
    /// New lines covered.
    pub new_count: u32,
    /// The rows, in order.
    pub lines: Vec<DiffLine>,
}

impl DiffHunk {
    /// The unified-diff range line, `@@ -a,b +c,d @@`.
    pub fn header(&self) -> String {
        format!(
            "@@ -{},{} +{},{} @@",
            self.old_start, self.old_count, self.new_start, self.new_count
        )
    }
}

/// A line diff of one file, from [`compute_diff`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiffModel {
    /// The hunks, in file order; empty for identical texts or a guarded input.
    pub hunks: Vec<DiffHunk>,
    /// Added lines in total.
    pub added: usize,
    /// Removed lines in total.
    pub removed: usize,
    /// Lines in the old text.
    pub old_lines: usize,
    /// Lines in the new text.
    pub new_lines: usize,
    /// The change creates the file.
    pub new_file: bool,
    /// A side exceeded [`DIFF_MAX_BYTES`] or [`DIFF_MAX_LINES`]: no hunks and
    /// no added/removed totals, only the line counts.
    pub too_large: bool,
}

impl DiffModel {
    /// Whether there is any row to show.
    pub fn has_changes(&self) -> bool {
        !self.hunks.is_empty()
    }

    /// Rows of the full presentation: one separator per hunk plus its lines.
    pub fn row_count(&self) -> usize {
        self.hunks.iter().map(|h| h.lines.len() + 1).sum()
    }

    /// Lines across every hunk.
    pub fn line_count(&self) -> usize {
        self.hunks.iter().map(|h| h.lines.len()).sum()
    }
}

/// The lines of `text` without endings; a trailing newline ends the last line
/// rather than starting an empty one.
fn split_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let body = text.strip_suffix('\n').unwrap_or(text);
    body.split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect()
}

/// [`split_lines`]' length, without allocating.
fn count_lines(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let breaks = text.bytes().filter(|&b| b == b'\n').count();
    breaks + usize::from(!text.ends_with('\n'))
}

/// Diff `old` against `new` line by line, keeping `context` unchanged lines
/// around each change and merging hunks whose context would touch. `old =
/// None` is a new file: every line added, in one hunk. See the module docs
/// for line-ending handling and the size guard.
pub fn compute_diff(old: Option<&str>, new: &str, context: usize) -> DiffModel {
    let old_text = old.unwrap_or("");
    let mut model = DiffModel {
        old_lines: count_lines(old_text),
        new_lines: count_lines(new),
        new_file: old.is_none(),
        ..DiffModel::default()
    };
    model.too_large = old_text.len() > DIFF_MAX_BYTES
        || new.len() > DIFF_MAX_BYTES
        || model.old_lines > DIFF_MAX_LINES
        || model.new_lines > DIFF_MAX_LINES;
    if model.too_large {
        return model;
    }
    let before = split_lines(old_text);
    let after = split_lines(new);
    let changes: Vec<Hunk> = if model.new_file {
        if after.is_empty() {
            Vec::new()
        } else {
            vec![Hunk {
                before: 0..0,
                after: 0..after.len() as u32,
            }]
        }
    } else {
        let mut input = InternedInput::default();
        input.reserve(before.len() as u32, after.len() as u32);
        input.update_before(before.iter().copied());
        input.update_after(after.iter().copied());
        let mut diff = Diff::compute(Algorithm::Histogram, &input);
        diff.postprocess_lines(&input);
        diff.hunks().collect()
    };
    let mut first = 0;
    while first < changes.len() {
        let mut last = first;
        while last + 1 < changes.len()
            && (changes[last + 1].before.start - changes[last].before.end) as usize <= 2 * context
        {
            last += 1;
        }
        model
            .hunks
            .push(build_hunk(&changes[first..=last], &before, &after, context));
        first = last + 1;
    }
    for line in model.hunks.iter().flat_map(|h| &h.lines) {
        match line.kind {
            DiffLineKind::Added => model.added += 1,
            DiffLineKind::Removed => model.removed += 1,
            DiffLineKind::Context => {}
        }
    }
    model
}

/// One output hunk from a group of nearby changes. Between two changes, and
/// before the first, the unchanged stretch has the same length on both sides.
fn build_hunk(group: &[Hunk], before: &[&str], after: &[&str], context: usize) -> DiffHunk {
    let line = |kind, old: Option<usize>, new: Option<usize>, text: &str| DiffLine {
        kind,
        old_line: old.map(|i| i as u32 + 1),
        new_line: new.map(|i| i as u32 + 1),
        text: text.to_owned(),
    };
    let lead = (group[0].before.start as usize).min(context);
    let mut old_i = group[0].before.start as usize - lead;
    let mut new_i = group[0].after.start as usize - lead;
    let (old_from, new_from) = (old_i, new_i);
    let mut lines = Vec::new();
    for change in group {
        while old_i < change.before.start as usize {
            lines.push(line(
                DiffLineKind::Context,
                Some(old_i),
                Some(new_i),
                before[old_i],
            ));
            old_i += 1;
            new_i += 1;
        }
        let (old_end, new_end) = (change.before.end as usize, change.after.end as usize);
        for (i, text) in before.iter().enumerate().take(old_end).skip(old_i) {
            lines.push(line(DiffLineKind::Removed, Some(i), None, text));
        }
        for (i, text) in after.iter().enumerate().take(new_end).skip(new_i) {
            lines.push(line(DiffLineKind::Added, None, Some(i), text));
        }
        old_i = old_end;
        new_i = new_end;
    }
    let trail = before.len().saturating_sub(old_i).min(context);
    for _ in 0..trail {
        lines.push(line(
            DiffLineKind::Context,
            Some(old_i),
            Some(new_i),
            before[old_i],
        ));
        old_i += 1;
        new_i += 1;
    }
    let start = |from: usize, count: usize| (if count == 0 { from } else { from + 1 }) as u32;
    DiffHunk {
        old_start: start(old_from, old_i - old_from),
        old_count: (old_i - old_from) as u32,
        new_start: start(new_from, new_i - new_from),
        new_count: (new_i - new_from) as u32,
        lines,
    }
}

/// `path` cut to at most `max_chars` characters by replacing its middle with
/// an ellipsis; the tail (the file name) keeps the larger half.
fn elide_middle(path: &str, max_chars: usize) -> String {
    let count = path.chars().count();
    if count <= max_chars {
        return path.to_owned();
    }
    if max_chars <= 1 {
        return ELLIPSIS.to_string();
    }
    let keep = max_chars - 1;
    let head = keep / 2;
    let mut out: String = path.chars().take(head).collect();
    out.push(ELLIPSIS);
    out.extend(path.chars().skip(count - (keep - head)));
    out
}

fn expand_tabs(text: &str) -> String {
    if text.contains('\t') {
        text.replace('\t', &" ".repeat(TAB_WIDTH))
    } else {
        text.to_owned()
    }
}

type OnToggle<State> = Rc<dyn Fn(&mut State)>;

/// A declarative diff view. See the [module docs](self).
pub struct DiffView<State: 'static> {
    input: DiffInput,
    collapsed: bool,
    collapse_above: usize,
    context: usize,
    on_toggle: Option<OnToggle<State>>,
}

/// Show `input` as a unified diff. Starts collapsed (which only matters above
/// [`DIFF_DEFAULT_COLLAPSE_ABOVE`] rows) with [`DIFF_DEFAULT_CONTEXT`] lines of
/// context.
pub fn diff_view<State: 'static>(input: DiffInput) -> DiffView<State> {
    DiffView {
        input,
        collapsed: true,
        collapse_above: DIFF_DEFAULT_COLLAPSE_ABOVE,
        context: DIFF_DEFAULT_CONTEXT,
        on_toggle: None,
    }
}

impl<State: 'static> DiffView<State> {
    /// Set the (app-owned) collapsed state.
    pub fn collapsed(mut self, collapsed: bool) -> Self {
        self.collapsed = collapsed;
        self
    }

    /// Fire on a header or show-all press of a collapsible diff. The widget
    /// never flips `collapsed` itself; the app feeds the new value back in.
    pub fn on_toggle<F: Fn(&mut State) + 'static>(mut self, on_toggle: F) -> Self {
        self.on_toggle = Some(Rc::new(on_toggle));
        self
    }

    /// Rows above which the collapsed presentation applies.
    pub fn collapse_above(mut self, rows: usize) -> Self {
        self.collapse_above = rows;
        self
    }

    /// Unchanged lines kept around each change.
    pub fn context(mut self, lines: usize) -> Self {
        self.context = lines;
        self
    }

    fn model(&self) -> DiffModel {
        compute_diff(
            self.input.old_text.as_deref(),
            &self.input.new_text,
            self.context,
        )
    }
}

/// One displayed row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    /// A hunk's `@@` separator.
    Hunk(usize),
    /// Line `.1` of hunk `.0`.
    Line(usize, usize),
    /// The collapsed presentation's show-all row.
    ShowAll,
    /// The no-changes, empty-file or too-large summary.
    Notice,
}

/// Shaped text of one row.
#[derive(Default)]
struct RowRuns {
    gutter: Option<TextLayout>,
    body: Option<TextLayout>,
}

/// Where a press started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Header,
    ShowAll,
    Body,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Gesture {
    Idle,
    Pending { start: Point, target: Target },
    Scrolling { start_x: f64, start_offset: f64 },
    Declined,
}

/// Every text style, resolved together so a theme change reshapes everything.
#[derive(Clone, PartialEq)]
struct Styles {
    body: TextStyle,
    muted: TextStyle,
    added: TextStyle,
    removed: TextStyle,
}

impl Styles {
    fn resolve(theme: Option<&Theme>) -> Self {
        let family = style::mono_family(theme);
        let make =
            |color| TextStyle::new(style::CODE_FONT_SIZE as f32, color).with_family(family.clone());
        Self {
            body: make(theme.map_or(FALLBACK_TEXT, |t| t.scheme().on_surface)),
            muted: make(style::muted(theme)),
            added: make(added_fg(theme)),
            removed: make(removed_fg(theme)),
        }
    }
}

/// The added marker and total: the success hue.
fn added_fg(theme: Option<&Theme>) -> Color {
    theme.map_or(FALLBACK_ADDED_FG, |t| {
        t.extension::<StatusPalette>()
            .map_or(FALLBACK_ADDED_FG, |s| s.colors(t.brightness).success)
    })
}

/// The removed marker and total: the error hue.
fn removed_fg(theme: Option<&Theme>) -> Color {
    theme.map_or(FALLBACK_REMOVED_FG, |t| t.scheme().error)
}

/// The retained widget for a [`DiffView`].
pub struct DiffWidget {
    path: String,
    model: DiffModel,
    collapsed: bool,
    collapse_above: usize,
    on_toggle: Option<ErasedCallback>,
    rows: Vec<Row>,
    runs: Vec<RowRuns>,
    styles: Option<Styles>,
    guard: LayoutGuard,
    header_path: Option<(usize, TextLayout)>,
    header_totals: Option<(Option<TextLayout>, Option<TextLayout>)>,
    markers: Option<(TextLayout, TextLayout)>,
    char_size: Size,
    width: f64,
    row_h: f64,
    header_h: f64,
    marker_x: f64,
    content_x: f64,
    content_w: f64,
    h_offset: f64,
    max_h_offset: f64,
    gesture: Gesture,
    #[cfg(test)]
    computations: usize,
}

impl DiffWidget {
    fn set_model(&mut self, model: DiffModel) {
        self.model = model;
        self.header_totals = None;
        self.refresh_rows();
        #[cfg(test)]
        {
            self.computations += 1;
        }
    }

    fn collapsible(&self) -> bool {
        !self.model.too_large && self.model.row_count() > self.collapse_above
    }

    fn refresh_rows(&mut self) {
        self.rows = if self.model.too_large || !self.model.has_changes() {
            vec![Row::Notice]
        } else if self.collapsed && self.collapsible() {
            vec![Row::ShowAll]
        } else {
            let mut rows = Vec::with_capacity(self.model.row_count());
            for (h, hunk) in self.model.hunks.iter().enumerate() {
                rows.push(Row::Hunk(h));
                rows.extend((0..hunk.lines.len()).map(|l| Row::Line(h, l)));
            }
            rows
        };
        self.runs = self.rows.iter().map(|_| RowRuns::default()).collect();
    }

    fn line(&self, hunk: usize, line: usize) -> &DiffLine {
        &self.model.hunks[hunk].lines[line]
    }

    fn digits(&self) -> usize {
        self.model
            .old_lines
            .max(self.model.new_lines)
            .max(1)
            .to_string()
            .len()
    }

    /// Characters in the line-number gutter: one column for a new file, else
    /// old and new numbers separated by a space.
    fn gutter_chars(&self) -> usize {
        if self.model.new_file {
            self.digits()
        } else {
            self.digits() * 2 + 1
        }
    }

    fn gutter_text(&self, line: &DiffLine) -> String {
        let w = self.digits();
        let num = |n: Option<u32>| n.map_or_else(String::new, |n| n.to_string());
        if self.model.new_file {
            format!("{:>w$}", num(line.new_line))
        } else {
            format!("{:>w$} {:>w$}", num(line.old_line), num(line.new_line))
        }
    }

    fn notice_text(&self) -> String {
        let m = &self.model;
        match (m.too_large, m.new_file) {
            (true, true) => format!("new file too large to show ({} lines)", m.new_lines),
            (true, false) => format!(
                "diff too large to show ({} -> {} lines)",
                m.old_lines, m.new_lines
            ),
            (false, true) => "empty file".to_owned(),
            (false, false) => "no changes".to_owned(),
        }
    }

    fn show_all_text(&self) -> String {
        format!("show all {} lines", self.model.line_count())
    }

    /// A row's text for screen readers: the marker leads a changed line.
    fn row_label(&self, row: Row) -> String {
        match row {
            Row::Hunk(h) => self.model.hunks[h].header(),
            Row::Line(h, l) => {
                let line = self.line(h, l);
                format!("{} {}", line.kind.marker(), line.text)
            }
            Row::ShowAll => self.show_all_text(),
            Row::Notice => self.notice_text(),
        }
    }

    fn summary_label(&self) -> String {
        let m = &self.model;
        if m.too_large {
            format!("{}, {}", self.path, self.notice_text())
        } else if m.new_file {
            format!("{}, new file, {} added", self.path, m.added)
        } else {
            format!("{}, {} added, {} removed", self.path, m.added, m.removed)
        }
    }

    fn body_top(&self) -> f64 {
        self.header_h + style::ROW_GAP
    }

    /// Invalidate every shaped run when the styles (or the locale) moved.
    fn sync_styles(&mut self, styles: Styles) {
        if self.styles.as_ref() == Some(&styles) && self.guard.matches(&styles.body, None) {
            return;
        }
        let stamp = LayoutGuard::stamp(&styles.body);
        self.guard.record(&styles.body, None, stamp);
        self.styles = Some(styles);
        self.header_path = None;
        self.header_totals = None;
        self.markers = None;
        self.char_size = Size::ZERO;
        self.runs.iter_mut().for_each(|r| *r = RowRuns::default());
    }

    fn shape_rows(&mut self, ctx: &mut LayoutCtx, styles: &Styles) {
        let mut content_w: f64 = 0.0;
        for i in 0..self.rows.len() {
            let row = self.rows[i];
            if self.runs[i].body.is_none() {
                let (gutter, body, style) = match row {
                    Row::Line(h, l) => {
                        let line = self.line(h, l);
                        (
                            Some(self.gutter_text(line)),
                            expand_tabs(&line.text),
                            &styles.body,
                        )
                    }
                    other => (None, self.row_label(other), &styles.muted),
                };
                let text = ctx.text_context::<TextContext>();
                self.runs[i].gutter = gutter.map(|g| text.layout(&g, &styles.muted, None));
                self.runs[i].body = Some(text.layout(&body, style, None));
            }
            if let (Row::Line(..), Some(body)) = (row, &self.runs[i].body) {
                content_w = content_w.max(body.size().width);
            }
        }
        self.content_w = content_w;
    }

    fn shape_header(&mut self, ctx: &mut LayoutCtx, styles: &Styles) {
        let text = ctx.text_context::<TextContext>();
        if self.header_totals.is_none() {
            let m = &self.model;
            let added =
                (!m.too_large).then(|| text.layout(&format!("+{}", m.added), &styles.added, None));
            let removed = (!m.too_large && !m.new_file)
                .then(|| text.layout(&format!("-{}", m.removed), &styles.removed, None));
            self.header_totals = Some((added, removed));
        }
        let totals_w = self.totals_width();
        let avail = self.width - style::PAD * 2.0 - totals_w;
        let max_chars = (avail / self.char_size.width.max(1.0)).floor().max(1.0) as usize;
        if self
            .header_path
            .as_ref()
            .is_none_or(|(n, _)| *n != max_chars)
        {
            let shown = elide_middle(&self.path, max_chars);
            self.header_path = Some((max_chars, text.layout(&shown, &styles.body, None)));
        }
    }

    fn totals_width(&self) -> f64 {
        let Some((added, removed)) = &self.header_totals else {
            return 0.0;
        };
        let w = |l: &Option<TextLayout>| l.as_ref().map_or(0.0, |l| l.size().width + TOTALS_GAP);
        w(added) + w(removed)
    }

    fn hit(&self, p: Point) -> Option<Target> {
        if p.y < self.header_h {
            return (self.collapsible() && self.on_toggle.is_some()).then_some(Target::Header);
        }
        if self.rows == [Row::ShowAll] {
            return self.on_toggle.is_some().then_some(Target::ShowAll);
        }
        (self.max_h_offset > 0.0).then_some(Target::Body)
    }

    fn set_offset(&mut self, ctx: &mut EventCtx, offset: f64) -> bool {
        let offset = offset.clamp(0.0, self.max_h_offset);
        if offset == self.h_offset {
            return false;
        }
        self.h_offset = offset;
        ctx.request_redraw();
        true
    }

    fn toggle(&mut self, ctx: &mut EventCtx) {
        if let Some(cb) = self.on_toggle.as_mut() {
            cb(ctx);
        }
    }
}

impl<State: 'static> View<State> for DiffView<State> {
    type Element = DiffWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> DiffWidget {
        let mut widget = DiffWidget {
            path: self.input.path.clone(),
            model: DiffModel::default(),
            collapsed: self.collapsed,
            collapse_above: self.collapse_above,
            on_toggle: self.on_toggle.as_ref().map(erase_callback),
            rows: Vec::new(),
            runs: Vec::new(),
            styles: None,
            guard: LayoutGuard::default(),
            header_path: None,
            header_totals: None,
            markers: None,
            char_size: Size::ZERO,
            width: 0.0,
            row_h: 0.0,
            header_h: 0.0,
            marker_x: 0.0,
            content_x: 0.0,
            content_w: 0.0,
            h_offset: 0.0,
            max_h_offset: 0.0,
            gesture: Gesture::Idle,
            #[cfg(test)]
            computations: 0,
        };
        widget.set_model(self.model());
        widget
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut DiffWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = ChangeFlags::NONE;
        if prev.input.old_text != self.input.old_text
            || prev.input.new_text != self.input.new_text
            || prev.context != self.context
        {
            element.set_model(self.model());
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        if prev.input.path != self.input.path {
            element.path = self.input.path.clone();
            element.header_path = None;
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        if prev.collapsed != self.collapsed || prev.collapse_above != self.collapse_above {
            element.collapsed = self.collapsed;
            element.collapse_above = self.collapse_above;
            element.refresh_rows();
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        element.on_toggle = self.on_toggle.as_ref().map(erase_callback);
        flags
    }
}

impl Widget for DiffWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let styles = Styles::resolve(Theme::from_layout_ctx(ctx));
        self.sync_styles(styles.clone());
        self.width = if bc.max().width.is_finite() {
            bc.max().width
        } else {
            FALLBACK_WIDTH
        };
        if self.char_size == Size::ZERO {
            let text = ctx.text_context::<TextContext>();
            self.char_size = text.layout("0", &styles.body, None).size();
            self.markers = Some((
                text.layout("+", &styles.added, None),
                text.layout("-", &styles.removed, None),
            ));
        }
        self.row_h = self.char_size.height + ROW_PAD_Y * 2.0;
        self.header_h = self.char_size.height + style::PAD_SMALL * 2.0;
        self.shape_header(ctx, &styles);
        self.shape_rows(ctx, &styles);

        let numbers_w = self.gutter_chars() as f64 * self.char_size.width;
        self.marker_x = style::PAD_SMALL + numbers_w + COLUMN_GAP;
        self.content_x = self.marker_x + self.char_size.width + COLUMN_GAP;
        let viewport_w = (self.width - self.content_x - style::PAD_SMALL).max(0.0);
        self.max_h_offset = (self.content_w - viewport_w).max(0.0);
        self.h_offset = self.h_offset.clamp(0.0, self.max_h_offset);

        let height = self.body_top() + self.rows.len() as f64 * self.row_h + style::RADIUS;
        bc.constrain(Size::new(self.width, height))
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let theme = Theme::from_paint_ctx(ctx);
        let o = ctx.origin();
        let size = ctx.size();
        scene.fill_rounded_rect(o, size, style::RADIUS, style::surface(theme));
        scene.push_clip_rounded(o, size, style::RADIUS);

        let text_y = |top: f64, h: f64| top + (h - self.char_size.height) / 2.0;
        if let Some((_, path)) = &self.header_path {
            paint_layout(
                path,
                Point::new(o.x + style::PAD, text_y(o.y, self.header_h)),
                scene,
            );
        }
        if let Some((added, removed)) = &self.header_totals {
            let mut x = o.x + size.width - style::PAD;
            for total in [removed, added].into_iter().flatten() {
                x -= total.size().width;
                paint_layout(total, Point::new(x, text_y(o.y, self.header_h)), scene);
                x -= TOTALS_GAP;
            }
        }
        let divider_y = o.y + self.header_h;
        scene.stroke_line(
            Point::new(o.x, divider_y),
            Point::new(o.x + size.width, divider_y),
            style::BORDER_WIDTH,
            style::border(theme),
        );

        let top = o.y + self.body_top();
        let (first, last) = match ctx.visible_rect() {
            Some(v) if self.row_h > 0.0 => (
                ((v.y0 - top) / self.row_h).floor().max(0.0) as usize,
                (((v.y1 - top) / self.row_h).ceil().max(0.0) as usize).min(self.rows.len()),
            ),
            _ => (0, self.rows.len()),
        };
        let first = first.min(last);
        for i in first..last {
            let y = top + i as f64 * self.row_h;
            match self.rows[i] {
                Row::Line(h, l) => {
                    let kind = self.line(h, l).kind;
                    let wash = match kind {
                        DiffLineKind::Added => Some(style::added_bg(theme)),
                        DiffLineKind::Removed => Some(style::removed_bg(theme)),
                        DiffLineKind::Context => None,
                    };
                    if let Some(wash) = wash {
                        scene.fill_rect(
                            Point::new(o.x, y),
                            Size::new(size.width, self.row_h),
                            wash,
                        );
                    }
                    if let Some(gutter) = &self.runs[i].gutter {
                        let at = Point::new(o.x + style::PAD_SMALL, text_y(y, self.row_h));
                        paint_layout(gutter, at, scene);
                    }
                    let marker = self.markers.as_ref().and_then(|(add, rem)| match kind {
                        DiffLineKind::Added => Some(add),
                        DiffLineKind::Removed => Some(rem),
                        DiffLineKind::Context => None,
                    });
                    if let Some(marker) = marker {
                        let at = Point::new(o.x + self.marker_x, text_y(y, self.row_h));
                        paint_layout(marker, at, scene);
                    }
                }
                Row::Hunk(h) => {
                    if h > 0 {
                        scene.stroke_line(
                            Point::new(o.x, y),
                            Point::new(o.x + size.width, y),
                            style::BORDER_WIDTH,
                            style::border(theme),
                        );
                    }
                    if let Some(body) = &self.runs[i].body {
                        let at = Point::new(o.x + style::PAD_SMALL, text_y(y, self.row_h));
                        paint_layout(body, at, scene);
                    }
                }
                Row::ShowAll | Row::Notice => {
                    if let Some(body) = &self.runs[i].body {
                        let at = Point::new(o.x + style::PAD_SMALL, text_y(y, self.row_h));
                        paint_layout(body, at, scene);
                    }
                }
            }
        }

        let clip_x = o.x + self.content_x;
        let clip_w = (size.width - self.content_x - style::PAD_SMALL).max(0.0);
        scene.push_clip(
            Point::new(clip_x, top),
            Size::new(clip_w, (size.height - self.body_top()).max(0.0)),
        );
        for i in first..last {
            if let (Row::Line(..), Some(body)) = (self.rows[i], &self.runs[i].body) {
                let y = top + i as f64 * self.row_h;
                let at = Point::new(clip_x - self.h_offset, text_y(y, self.row_h));
                paint_layout(body, at, scene);
            }
        }
        scene.pop_clip();
        scene.pop_clip();

        let outline =
            RoundedRect::from_rect(Rect::from_origin_size(Point::ORIGIN, size), style::RADIUS);
        scene.stroke_path(
            o,
            &outline.to_path(PATH_TOLERANCE),
            style::BORDER_WIDTH,
            &Brush::Solid(style::border(theme)),
        );
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        if event.is_broadcast() {
            return EventResult::Ignored;
        }
        if let InputEvent::Scroll { position, delta } = event {
            let dx = match delta {
                ScrollDelta::Lines(x, _) => x * WHEEL_LINE_PX,
                ScrollDelta::Pixels(x, _) => *x,
            };
            if position.y < self.header_h || dx == 0.0 {
                return EventResult::Ignored;
            }
            return if self.set_offset(ctx, self.h_offset + dx) {
                EventResult::Handled
            } else {
                EventResult::Ignored
            };
        }
        let InputEvent::Pointer(p) = event else {
            return EventResult::Ignored;
        };
        match p.phase {
            PointerPhase::Down => {
                let Some(target) = self.hit(p.position).filter(|_| presses(p)) else {
                    return EventResult::Ignored;
                };
                self.gesture = Gesture::Pending {
                    start: p.position,
                    target,
                };
                ctx.capture_pointer();
                EventResult::Handled
            }
            PointerPhase::Move => match self.gesture {
                Gesture::Pending { start, target } => {
                    let dx = p.position.x - start.x;
                    let dy = p.position.y - start.y;
                    if dy.abs() > TOUCH_SLOP && dy.abs() >= dx.abs() {
                        // A vertical intent belongs to the enclosing scroll
                        // surface for the rest of the gesture.
                        self.gesture = Gesture::Declined;
                        return EventResult::Ignored;
                    }
                    if dx.abs() > TOUCH_SLOP {
                        if target == Target::Body {
                            self.gesture = Gesture::Scrolling {
                                start_x: start.x,
                                start_offset: self.h_offset,
                            };
                            self.set_offset(ctx, self.h_offset - dx);
                        } else {
                            self.gesture = Gesture::Declined;
                            return EventResult::Ignored;
                        }
                    }
                    EventResult::Handled
                }
                Gesture::Scrolling {
                    start_x,
                    start_offset,
                } => {
                    self.set_offset(ctx, start_offset - (p.position.x - start_x));
                    EventResult::Handled
                }
                Gesture::Idle | Gesture::Declined => EventResult::Ignored,
            },
            PointerPhase::Up => {
                let gesture = std::mem::replace(&mut self.gesture, Gesture::Idle);
                match gesture {
                    Gesture::Pending { target, .. } => {
                        if target != Target::Body && self.hit(p.position) == Some(target) {
                            self.toggle(ctx);
                        }
                        EventResult::Handled
                    }
                    Gesture::Scrolling { .. } => EventResult::Handled,
                    Gesture::Idle | Gesture::Declined => EventResult::Ignored,
                }
            }
            PointerPhase::Cancel => {
                let active = matches!(
                    self.gesture,
                    Gesture::Pending { .. } | Gesture::Scrolling { .. }
                );
                self.gesture = Gesture::Idle;
                if active {
                    EventResult::Handled
                } else {
                    EventResult::Ignored
                }
            }
        }
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        let label = self.summary_label();
        ctx.push_container(
            Role::Group,
            |node| node.set_label(label.as_str()),
            |ctx| {
                let origin = ctx.origin();
                let width = ctx.size().width;
                let bounds = |y0: f64, y1: f64| frust::accesskit::Rect {
                    x0: origin.x,
                    y0: origin.y + y0,
                    x1: origin.x + width,
                    y1: origin.y + y1,
                };
                if self.collapsible() {
                    ctx.push_node(Role::Button, |node| {
                        node.set_label(self.path.as_str());
                        node.set_expanded(!self.collapsed);
                        node.add_action(Action::Click);
                        node.set_bounds(bounds(0.0, self.header_h));
                    });
                }
                for (i, row) in self.rows.iter().enumerate() {
                    let y0 = self.body_top() + i as f64 * self.row_h;
                    let text = self.row_label(*row);
                    let role = if *row == Row::ShowAll {
                        Role::Button
                    } else {
                        Role::Label
                    };
                    ctx.push_node(role, |node| {
                        node.set_label(text.as_str());
                        if role == Role::Button {
                            node.add_action(Action::Click);
                        }
                        node.set_bounds(bounds(y0, y0 + self.row_h));
                    });
                }
            },
        );
    }
}

fn paint_layout(layout: &TextLayout, origin: Point, scene: &mut dyn PaintScene) {
    for run in layout.to_scene_runs(origin) {
        scene.draw_glyph_run(run);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use frust::authoring::scene::GlyphRun;
    use std::any::Any;
    use std::time::{Duration, Instant};

    fn kinds(hunk: &DiffHunk) -> String {
        hunk.lines.iter().map(|l| l.kind.marker()).collect()
    }

    fn numbered(n: usize) -> String {
        (1..=n).map(|i| format!("line {i}\n")).collect()
    }

    // ---- Model -------------------------------------------------------------

    #[test]
    fn a_modified_line_is_a_removal_then_an_addition_with_context() {
        let m = compute_diff(Some("a\nb\nc\n"), "a\nB\nc\n", DIFF_DEFAULT_CONTEXT);
        assert_eq!(m.hunks.len(), 1);
        let h = &m.hunks[0];
        assert_eq!(kinds(h), " -+ ");
        assert_eq!(
            (h.old_start, h.old_count, h.new_start, h.new_count),
            (1, 3, 1, 3)
        );
        assert_eq!(h.header(), "@@ -1,3 +1,3 @@");
        assert_eq!((h.lines[1].old_line, h.lines[1].new_line), (Some(2), None));
        assert_eq!((h.lines[2].old_line, h.lines[2].new_line), (None, Some(2)));
        assert_eq!(
            (h.lines[3].old_line, h.lines[3].new_line),
            (Some(3), Some(3))
        );
        assert_eq!((m.added, m.removed), (1, 1));
        assert!(!m.new_file && !m.too_large);
    }

    #[test]
    fn context_bounds_a_hunk_and_far_changes_split() {
        let old = numbered(30);
        let new = old
            .replace("line 5\n", "five\n")
            .replace("line 25\n", "twenty-five\n");
        let m = compute_diff(Some(&old), &new, 3);
        assert_eq!(m.hunks.len(), 2);
        assert_eq!(m.hunks[0].header(), "@@ -2,7 +2,7 @@");
        assert_eq!(kinds(&m.hunks[0]), "   -+   ");
        assert_eq!(m.hunks[1].old_start, 22);
        let narrow = compute_diff(Some(&old), &new, 1);
        assert_eq!(narrow.hunks[0].lines.len(), 4);
    }

    #[test]
    fn adjacent_hunks_merge() {
        let old = numbered(20);
        let new = old
            .replace("line 5\n", "five\n")
            .replace("line 11\n", "eleven\n");
        // Five unchanged lines between the changes: within 2 x 3 of context.
        let m = compute_diff(Some(&old), &new, 3);
        assert_eq!(m.hunks.len(), 1);
        assert_eq!(kinds(&m.hunks[0]), "   -+     -+   ");
        assert_eq!(m.row_count(), 16);
    }

    #[test]
    fn a_new_file_is_one_hunk_of_additions() {
        let m = compute_diff(None, "x\ny\nz", DIFF_DEFAULT_CONTEXT);
        assert!(m.new_file);
        assert_eq!(m.hunks.len(), 1);
        assert_eq!(kinds(&m.hunks[0]), "+++");
        assert!(m.hunks[0].lines.iter().all(|l| l.old_line.is_none()));
        assert_eq!(m.hunks[0].header(), "@@ -0,0 +1,3 @@");
        assert_eq!((m.added, m.removed, m.new_lines), (3, 0, 3));
        assert!(!compute_diff(None, "", 3).has_changes());
    }

    #[test]
    fn a_missing_trailing_newline_is_not_a_change() {
        assert!(!compute_diff(Some("a\nb"), "a\nb\n", 3).has_changes());
        let m = compute_diff(Some("a\nb"), "a\nc", 3);
        assert_eq!(kinds(&m.hunks[0]), " -+");
        assert_eq!(m.hunks[0].lines[2].text, "c");
    }

    #[test]
    fn crlf_compares_and_displays_without_the_cr() {
        assert!(!compute_diff(Some("a\r\nb\r\n"), "a\nb\n", 3).has_changes());
        let m = compute_diff(Some("a\r\nb\r\n"), "a\r\nc\r\n", 3);
        assert_eq!(kinds(&m.hunks[0]), " -+");
        assert!(m.hunks[0].lines.iter().all(|l| !l.text.contains('\r')));
    }

    #[test]
    fn empty_sides() {
        let added = compute_diff(Some(""), "a\nb\n", 3);
        assert_eq!(kinds(&added.hunks[0]), "++");
        assert!(!added.new_file);
        let removed = compute_diff(Some("a\nb\n"), "", 3);
        assert_eq!(kinds(&removed.hunks[0]), "--");
        assert_eq!(removed.hunks[0].header(), "@@ -1,2 +0,0 @@");
        assert!(!compute_diff(Some(""), "", 3).has_changes());
    }

    #[test]
    fn identical_texts_have_no_hunks() {
        let m = compute_diff(Some("same\ntext\n"), "same\ntext\n", 3);
        assert!(m.hunks.is_empty());
        assert_eq!((m.added, m.removed), (0, 0));
    }

    #[test]
    fn a_guarded_input_returns_promptly_with_line_counts_only() {
        let big = "x\n".repeat(DIFF_MAX_BYTES);
        let started = Instant::now();
        let m = compute_diff(Some(&big), "y\n", 3);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(m.too_large && m.hunks.is_empty());
        assert_eq!(
            (m.old_lines, m.new_lines, m.added, m.removed),
            (DIFF_MAX_BYTES, 1, 0, 0)
        );
        let many = "\n".repeat(DIFF_MAX_LINES + 1);
        assert!(compute_diff(None, &many, 3).too_large);
        assert!(!compute_diff(None, &"\n".repeat(DIFF_MAX_LINES), 3).too_large);
    }

    #[test]
    fn middle_elision_keeps_both_ends() {
        assert_eq!(elide_middle("src/lib.rs", 20), "src/lib.rs");
        let cut = elide_middle("crates/frust-core/src/widget.rs", 12);
        assert_eq!(cut.chars().count(), 12);
        assert!(cut.starts_with("crate") && cut.ends_with("get.rs"));
        assert!(cut.contains(ELLIPSIS));
        assert_eq!(elide_middle("abc", 1), ELLIPSIS.to_string());
    }

    // ---- Widget harness ----------------------------------------------------

    #[derive(Default)]
    struct Recorder {
        rects: Vec<(Point, Size, Color)>,
        runs: usize,
    }

    impl PaintScene for Recorder {
        fn fill_rect(&mut self, o: Point, s: Size, c: Color) {
            self.rects.push((o, s, c));
        }
        fn draw_text(&mut self, _o: Point, _t: &str) {}
        fn draw_glyph_run(&mut self, _run: GlyphRun) {
            self.runs += 1;
        }
    }

    fn input(path: &str, old: Option<&str>, new: &str) -> DiffInput {
        DiffInput {
            path: path.into(),
            old_text: old.map(str::to_owned),
            new_text: new.into(),
        }
    }

    fn build<S: 'static>(view: &DiffView<S>) -> DiffWidget {
        let mut counter = 0u64;
        View::<S>::build(view, &mut BuildCtx::new(&mut counter))
    }

    fn rebuild<S: 'static>(next: &DiffView<S>, prev: &DiffView<S>, w: &mut DiffWidget) {
        let mut counter = 0u64;
        View::<S>::rebuild(next, prev, w, &mut BuildCtx::new(&mut counter));
    }

    fn layout(w: &mut DiffWidget, width: f64, theme: Option<&Theme>) -> Size {
        let mut tcx = TextContext::new();
        let mut lctx = match theme {
            Some(t) => {
                LayoutCtx::with_resources(Some(&mut tcx as &mut dyn Any), Some(t as &dyn Any))
            }
            None => LayoutCtx::with_text_context(&mut tcx as &mut dyn Any),
        };
        w.layout(
            &mut lctx,
            &BoxConstraints::new(Size::ZERO, Size::new(width, f64::INFINITY)),
        )
    }

    fn paint(w: &mut DiffWidget, size: Size, theme: Option<&Theme>) -> Recorder {
        let mut rec = Recorder::default();
        let mut ctx = match theme {
            Some(t) => PaintCtx::new(Point::ZERO, size).with_theme(t),
            None => PaintCtx::new(Point::ZERO, size),
        };
        w.paint(&mut ctx, &mut rec);
        rec
    }

    fn pointer(
        w: &mut DiffWidget,
        state: &mut dyn Any,
        phase: PointerPhase,
        at: Point,
    ) -> EventResult {
        let event = InputEvent::Pointer(frust_core::PointerEvent {
            phase,
            position: at,
            button: frust_core::PointerButton::Primary,
        });
        let mut ctx = EventCtx::new(state, Point::ZERO, Size::new(320.0, 400.0));
        w.event(&mut ctx, &event)
    }

    fn wheel(w: &mut DiffWidget, at: Point, delta: ScrollDelta) -> EventResult {
        let mut state = ();
        let mut ctx = EventCtx::new(
            &mut state as &mut dyn Any,
            Point::ZERO,
            Size::new(320.0, 400.0),
        );
        w.event(
            &mut ctx,
            &InputEvent::Scroll {
                position: at,
                delta,
            },
        )
    }

    fn long_edit() -> DiffInput {
        let old = numbered(60);
        let new = old.replace("line 3\n", &format!("{}\n", "wide ".repeat(40)));
        input("src/long.rs", Some(&old), &new)
    }

    // ---- Widget ------------------------------------------------------------

    #[test]
    fn an_equal_input_is_not_recomputed() {
        let a: DiffView<()> = diff_view(input("a.rs", Some("a\n"), "b\n"));
        let mut w = build(&a);
        assert_eq!(w.computations, 1);
        let same: DiffView<()> = diff_view(input("a.rs", Some("a\n"), "b\n")).collapsed(false);
        rebuild(&same, &a, &mut w);
        assert_eq!(w.computations, 1, "equal input, new collapsed flag");
        let renamed: DiffView<()> = diff_view(input("b.rs", Some("a\n"), "b\n")).collapsed(false);
        rebuild(&renamed, &same, &mut w);
        assert_eq!(w.computations, 1, "a path change only re-labels");
        let edited: DiffView<()> = diff_view(input("b.rs", Some("a\n"), "c\n")).collapsed(false);
        rebuild(&edited, &renamed, &mut w);
        assert_eq!(w.computations, 2);
        let wider: DiffView<()> = diff_view(input("b.rs", Some("a\n"), "c\n"))
            .collapsed(false)
            .context(5);
        rebuild(&wider, &edited, &mut w);
        assert_eq!(w.computations, 3);
    }

    #[test]
    fn collapsed_shows_one_show_all_row_above_the_threshold_only() {
        let edit = long_edit();
        let rows = compute_diff(edit.old_text.as_deref(), &edit.new_text, 3).row_count();
        let collapsed: DiffView<()> = diff_view(edit.clone()).collapse_above(rows - 1);
        let w = build(&collapsed);
        assert_eq!(w.rows, [Row::ShowAll]);
        let expanded: DiffView<()> = diff_view(edit.clone())
            .collapse_above(rows - 1)
            .collapsed(false);
        assert_eq!(build(&expanded).rows.len(), rows);
        let small: DiffView<()> = diff_view(edit).collapse_above(rows);
        assert_eq!(
            build(&small).rows.len(),
            rows,
            "at the threshold nothing collapses"
        );
    }

    #[test]
    fn identical_and_guarded_inputs_show_one_notice_row() {
        let same: DiffView<()> = diff_view(input("a.rs", Some("x\n"), "x\n"));
        let w = build(&same);
        assert_eq!(w.rows, [Row::Notice]);
        assert_eq!(w.row_label(Row::Notice), "no changes");
        let big = "x\n".repeat(DIFF_MAX_LINES + 1);
        let guarded: DiffView<()> = diff_view(input("a.rs", Some("x\n"), &big));
        let w = build(&guarded);
        assert_eq!(w.rows, [Row::Notice]);
        assert!(w.row_label(Row::Notice).contains("too large"));
        assert!(!w.collapsible());
    }

    #[test]
    fn show_all_reports_a_toggle_and_never_self_expands() {
        #[derive(Default)]
        struct S {
            toggles: u32,
        }
        let view: DiffView<S> = diff_view(long_edit())
            .collapse_above(4)
            .on_toggle(|s: &mut S| s.toggles += 1);
        let mut w = build(&view);
        layout(&mut w, 320.0, None);
        let mut s = S::default();
        let at = Point::new(40.0, w.body_top() + w.row_h / 2.0);
        assert_eq!(
            pointer(&mut w, &mut s, PointerPhase::Down, at),
            EventResult::Handled
        );
        pointer(&mut w, &mut s, PointerPhase::Up, at);
        assert_eq!(s.toggles, 1);
        assert!(w.collapsed);
        let header = Point::new(40.0, w.header_h / 2.0);
        pointer(&mut w, &mut s, PointerPhase::Down, header);
        pointer(&mut w, &mut s, PointerPhase::Up, header);
        assert_eq!(s.toggles, 2, "the header toggles a collapsible diff too");
    }

    #[test]
    fn the_new_file_presentation_has_one_gutter_column_and_no_removed_total() {
        let _slot = frust_widgets::test_support::LocaleSlotGuard::hold();
        let new: DiffView<()> = diff_view(input("new.rs", None, "a\nb\n"));
        let mut w = build(&new);
        layout(&mut w, 320.0, None);
        assert_eq!(w.gutter_chars(), 1);
        assert!(matches!(w.header_totals, Some((Some(_), None))));
        assert_eq!(w.summary_label(), "new.rs, new file, 2 added");
        let edit: DiffView<()> = diff_view(input("old.rs", Some("a\n"), "b\n"));
        let mut e = build(&edit);
        layout(&mut e, 320.0, None);
        assert_eq!(e.gutter_chars(), 3);
        assert!(e.marker_x > w.marker_x);
        assert!(matches!(e.header_totals, Some((Some(_), Some(_)))));
    }

    #[test]
    fn a_long_path_is_elided_in_the_header_but_whole_in_semantics() {
        let _slot = frust_widgets::test_support::LocaleSlotGuard::hold();
        let path = format!("{}/file.rs", "deeply/nested/directory".repeat(6));
        let view: DiffView<()> = diff_view(input(&path, Some("a\n"), "b\n"));
        let mut w = build(&view);
        let size = layout(&mut w, 260.0, None);
        let (chars, laid) = w.header_path.as_ref().expect("header shaped");
        assert!(*chars < path.chars().count());
        let right = style::PAD + laid.size().width + w.totals_width() + style::PAD;
        assert!(right <= size.width + 1.0, "{right} > {}", size.width);
        assert!(w.summary_label().starts_with(&path));
    }

    #[test]
    fn layout_stays_within_the_width_and_long_lines_scroll() {
        let _slot = frust_widgets::test_support::LocaleSlotGuard::hold();
        let view: DiffView<()> = diff_view(long_edit()).collapsed(false);
        let mut w = build(&view);
        let size = layout(&mut w, 240.0, None);
        assert_eq!(size.width, 240.0);
        assert!(w.max_h_offset > 0.0, "a 200-column line overflows 240 px");
        let rows = w.rows.len() as f64;
        assert!((size.height - (w.body_top() + rows * w.row_h + style::RADIUS)).abs() < 1e-9);
        let bounded = layout(&mut w, f64::INFINITY, None);
        assert_eq!(bounded.width, FALLBACK_WIDTH);
    }

    #[test]
    fn a_horizontal_drag_scrolls_every_row_and_a_vertical_one_is_declined() {
        let _slot = frust_widgets::test_support::LocaleSlotGuard::hold();
        let view: DiffView<()> = diff_view(long_edit()).collapsed(false);
        let mut w = build(&view);
        layout(&mut w, 240.0, None);
        let mut s = ();
        let start = Point::new(150.0, w.body_top() + 3.0 * w.row_h);
        pointer(&mut w, &mut s, PointerPhase::Down, start);
        let moved = Point::new(start.x - 80.0, start.y + 2.0);
        assert_eq!(
            pointer(&mut w, &mut s, PointerPhase::Move, moved),
            EventResult::Handled
        );
        assert_eq!(w.h_offset, 80.0);
        pointer(&mut w, &mut s, PointerPhase::Up, moved);

        let before = w.h_offset;
        pointer(&mut w, &mut s, PointerPhase::Down, start);
        let down = Point::new(start.x - 4.0, start.y + 40.0);
        assert_eq!(
            pointer(&mut w, &mut s, PointerPhase::Move, down),
            EventResult::Ignored
        );
        let sideways = Point::new(start.x - 120.0, start.y + 40.0);
        assert_eq!(
            pointer(&mut w, &mut s, PointerPhase::Move, sideways),
            EventResult::Ignored
        );
        assert_eq!(w.h_offset, before, "a declined gesture never scrolls");
        pointer(&mut w, &mut s, PointerPhase::Up, sideways);
    }

    #[test]
    fn a_horizontal_wheel_scrolls_and_a_vertical_one_passes_through() {
        let _slot = frust_widgets::test_support::LocaleSlotGuard::hold();
        let view: DiffView<()> = diff_view(long_edit()).collapsed(false);
        let mut w = build(&view);
        layout(&mut w, 240.0, None);
        let at = Point::new(100.0, w.body_top() + w.row_h);
        assert_eq!(
            wheel(&mut w, at, ScrollDelta::Pixels(0.0, 40.0)),
            EventResult::Ignored
        );
        assert_eq!(
            wheel(&mut w, at, ScrollDelta::Pixels(30.0, 0.0)),
            EventResult::Handled
        );
        assert_eq!(w.h_offset, 30.0);
        assert_eq!(
            wheel(&mut w, at, ScrollDelta::Pixels(-90.0, 0.0)),
            EventResult::Handled
        );
        assert_eq!(w.h_offset, 0.0);
        assert_eq!(
            wheel(&mut w, at, ScrollDelta::Pixels(-10.0, 0.0)),
            EventResult::Ignored,
            "a notch that cannot move passes outward"
        );
    }

    #[test]
    fn a_short_diff_does_not_capture_a_press() {
        let view: DiffView<()> = diff_view(input("a.rs", Some("a\n"), "b\n"));
        let mut w = build(&view);
        layout(&mut w, 320.0, None);
        let mut s = ();
        let at = Point::new(40.0, w.body_top() + w.row_h);
        assert_eq!(
            pointer(&mut w, &mut s, PointerPhase::Down, at),
            EventResult::Ignored
        );
    }

    #[test]
    fn changed_rows_get_a_full_width_wash_themed_and_unthemed() {
        let theme = crate::baseline();
        for theme in [None, Some(&theme)] {
            let view: DiffView<()> = diff_view(input("a.rs", Some("a\nb\nc\n"), "a\nB\nc\n"));
            let mut w = build(&view);
            let size = layout(&mut w, 300.0, theme);
            let rec = paint(&mut w, size, theme);
            let washes: Vec<_> = rec
                .rects
                .iter()
                .filter(|r| r.1.width == size.width)
                .collect();
            assert_eq!(washes.len(), 2);
            assert_eq!(washes[0].2, style::removed_bg(theme));
            assert_eq!(washes[1].2, style::added_bg(theme));
            assert!(rec.runs > 0);
        }
    }

    #[test]
    fn every_presentation_paints_without_panicking() {
        let theme = crate::baseline();
        let big = "x\n".repeat(DIFF_MAX_LINES + 1);
        let inputs = [
            long_edit(),
            input("same.rs", Some("a\n"), "a\n"),
            input("new.rs", None, "a\n\tb\n"),
            input("empty.rs", None, ""),
            input("big.rs", None, &big),
        ];
        for theme in [None, Some(&theme)] {
            for edit in &inputs {
                for collapsed in [true, false] {
                    let view: DiffView<()> = diff_view(edit.clone())
                        .collapse_above(4)
                        .collapsed(collapsed);
                    let mut w = build(&view);
                    let size = layout(&mut w, 200.0, theme);
                    paint(&mut w, size, theme);
                }
            }
        }
    }

    #[test]
    fn semantics_names_the_file_and_reads_rows_with_their_markers() {
        fn logic(_s: &mut ()) -> DiffView<()> {
            diff_view(DiffInput {
                path: "src/a.rs".into(),
                old_text: Some("a\nb\n".into()),
                new_text: "a\nc\n".into(),
            })
        }
        let mut root: frust_core::RenderRoot<(), DiffView<()>> = frust_core::RenderRoot::new();
        root.rebuild(&mut logic, &mut ());
        let mut tcx = TextContext::new();
        root.layout_with_text(Size::new(320.0, 400.0), &mut tcx as &mut dyn Any);
        let update = root.semantics();
        let label = |role: Role, text: &str| {
            update
                .nodes
                .iter()
                .any(|(_, n)| n.role() == role && n.label() == Some(text))
        };
        assert!(label(Role::Group, "src/a.rs, 1 added, 1 removed"));
        assert!(label(Role::Label, "- b"));
        assert!(label(Role::Label, "+ c"));
        assert!(label(Role::Label, "@@ -1,2 +1,2 @@"));
    }
}
