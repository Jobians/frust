//! Sanitizing markdown parser: untrusted, often half-streamed assistant text in,
//! a small block model out. pulldown-cmark parses with tables, strikethrough and
//! task lists on; its events never leave this module.
//!
//! Untrusted content: raw HTML, block or inline, becomes literal text; an image
//! the text `alt (url)`, never an image; a link's `url` is its trimmed
//! destination (an email autolink gains `mailto:`), `safe` only for http, https
//! and mailto with no control character. Lists and quotes nested past
//! [`MAX_NESTING_DEPTH`] flatten to text, spans past [`MAX_INLINE_DEPTH`] lose
//! their styling, and cells past [`MAX_TABLE_COLUMNS`] join the last column.
//!
//! Streaming: every prefix parses without panicking. A fence is `closed` once
//! it can gain nothing (a closing fence line, or its list item or quote ended
//! before the source did); an open one holds everything after it. Half-written
//! emphasis, links and table rows stay literal text. A block followed by
//! another is final (the renderer keeps it as is), with these exceptions:
//! - while the source's last line is unterminated, the block before the last
//!   can still change: `#x`, `***x` or `<divx` typed under a paragraph or list
//!   is lazy continuation text, not the heading, rule or HTML block its first
//!   characters started;
//! - a paragraph using `[label]` links changes when a later reference
//!   definition resolves them;
//! - only the last block can turn from paragraph into setext heading or table.
//!   The whole source is re-parsed on every call.

use std::ops::Range;

use pulldown_cmark::{Alignment, CodeBlockKind, Event, LinkType, Options, Parser, Tag, TagEnd};

/// Lists and block quotes nested deeper than this flatten to one text paragraph.
pub(crate) const MAX_NESTING_DEPTH: usize = 10;
/// Emphasis, strong, strikethrough and link spans nested deeper than this keep
/// their text but drop the span (a dropped link appends ` (url)`).
pub(crate) const MAX_INLINE_DEPTH: usize = 32;
/// Table columns past this merge into the last column.
pub(crate) const MAX_TABLE_COLUMNS: usize = 32;

/// One block of parsed markdown.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub(crate) enum MdBlock {
    /// A paragraph; also the literal text of a raw HTML block, one line per
    /// [`MdInline::HardBreak`].
    Paragraph(Vec<MdInline>),
    /// An ATX or setext heading, `level` 1 to 6.
    Heading {
        /// Heading level, 1 to 6.
        level: u8,
        /// Heading text.
        content: Vec<MdInline>,
    },
    /// A fenced or indented code block.
    CodeBlock {
        /// First word of the fence's info string, if any.
        language: Option<String>,
        /// The code, without its final line break.
        code: String,
        /// False while a fence has no closing fence yet (streaming).
        closed: bool,
    },
    /// A bullet (`ordered == false`, `start == 1`) or numbered list.
    List {
        /// Whether the items are numbered.
        ordered: bool,
        /// Number of the first item.
        start: u64,
        /// The items, in order.
        items: Vec<MdListItem>,
    },
    /// A block quote.
    BlockQuote(Vec<MdBlock>),
    /// A table; `header` and every row have at most [`MAX_TABLE_COLUMNS`] cells.
    Table {
        /// Per-column alignment.
        alignments: Vec<MdAlignment>,
        /// Header cells.
        header: Vec<Vec<MdInline>>,
        /// Body rows of cells; the last may be incomplete while streaming.
        rows: Vec<Vec<Vec<MdInline>>>,
    },
    /// A thematic break.
    Rule,
}

/// One list item.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct MdListItem {
    /// `Some(checked)` for a task-list item, `None` otherwise.
    pub checked: Option<bool>,
    /// The item's content; text of a tight item is wrapped in a paragraph.
    pub blocks: Vec<MdBlock>,
}

/// Horizontal alignment of a table column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum MdAlignment {
    /// No alignment given.
    None,
    /// `:--`
    Left,
    /// `:-:`
    Center,
    /// `--:`
    Right,
}

/// Inline content of a paragraph, heading or table cell.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub(crate) enum MdInline {
    /// Plain text, shown as is; raw HTML and images arrive here too.
    Text(String),
    /// `*emphasis*`
    Emphasis(Vec<MdInline>),
    /// `**strong**`
    Strong(Vec<MdInline>),
    /// `~~strikethrough~~`
    Strikethrough(Vec<MdInline>),
    /// `` `code` ``
    Code(String),
    /// A link. Show an unsafe link as plain text plus its `url`; never open it.
    Link {
        /// The destination, outer ASCII whitespace trimmed (an email autolink
        /// gains `mailto:`). For a safe link, exactly the string checked.
        url: String,
        /// True only for an http, https or mailto scheme with no control
        /// character anywhere.
        safe: bool,
        /// The link text.
        content: Vec<MdInline>,
    },
    /// A line break that renders as a space.
    SoftBreak,
    /// A forced line break.
    HardBreak,
}

/// Parses untrusted markdown into blocks. Never panics, on any input or prefix.
pub(crate) fn parse_markdown(source: &str) -> Vec<MdBlock> {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut builder = Builder::new(source);
    for (event, range) in Parser::new_ext(source, options).into_offset_iter() {
        builder.event(event, range);
    }
    builder.finish()
}

/// Number of leading blocks equal in both slices: the blocks a renderer can keep
/// when the source grows from the one `blocks_prev` was parsed from.
#[cfg(test)]
pub(crate) fn stable_prefix_len(blocks_prev: &[MdBlock], blocks_next: &[MdBlock]) -> usize {
    blocks_prev
        .iter()
        .zip(blocks_next)
        .take_while(|(a, b)| a == b)
        .count()
}

/// Whether a (trimmed) link destination may be offered to the app as a
/// tappable link: checked as is, so the app receives exactly this string.
fn is_safe_url(url: &str) -> bool {
    if url.chars().any(char::is_control) {
        return false;
    }
    let Some((scheme, _)) = url.split_once(':') else {
        return false;
    };
    ["http", "https", "mailto"]
        .iter()
        .any(|safe| scheme.eq_ignore_ascii_case(safe))
}

enum ContainerKind {
    Root,
    Quote,
    Item { checked: Option<bool> },
}

enum Frame {
    Container {
        kind: ContainerKind,
        blocks: Vec<MdBlock>,
    },
    List {
        ordered: bool,
        start: u64,
        items: Vec<MdListItem>,
    },
    Table {
        alignments: Vec<MdAlignment>,
        header: Vec<Vec<MdInline>>,
        rows: Vec<Vec<Vec<MdInline>>>,
        row: Vec<Vec<MdInline>>,
    },
}

enum Leaf {
    Paragraph {
        implicit: bool,
    },
    Heading(u8),
    Cell,
    Html(String),
    Code {
        language: Option<String>,
        fenced: bool,
        code: String,
        content_end: usize,
        closed: bool,
    },
}

enum Span {
    Root,
    Emphasis,
    Strong,
    Strikethrough,
    Link {
        url: String,
        safe: bool,
    },
    Image {
        url: String,
    },
    /// A span past the depth limit (or inside an image): children splice into
    /// the parent, followed by `suffix`.
    Plain {
        suffix: Option<String>,
    },
}

struct SpanFrame {
    span: Span,
    children: Vec<MdInline>,
}

struct Builder<'s> {
    source: &'s str,
    frames: Vec<Frame>,
    leaf: Option<Leaf>,
    spans: Vec<SpanFrame>,
    nesting: usize,
    styled_depth: usize,
    image_depth: usize,
    /// Open tags inside a flattened region (0 outside one).
    flatten: usize,
    flatten_urls: Vec<String>,
}

impl<'s> Builder<'s> {
    fn new(source: &'s str) -> Self {
        Self {
            source,
            frames: vec![Frame::Container {
                kind: ContainerKind::Root,
                blocks: Vec::new(),
            }],
            leaf: None,
            spans: Vec::new(),
            nesting: 0,
            styled_depth: 0,
            image_depth: 0,
            flatten: 0,
            flatten_urls: Vec::new(),
        }
    }

    fn event(&mut self, event: Event<'_>, range: Range<usize>) {
        if self.flatten > 0 {
            self.flatten_event(event);
            return;
        }
        match event {
            Event::Start(tag) => self.start(tag, range),
            Event::End(end) => self.end(end, range),
            Event::Text(text) => self.text(&text, range.end),
            Event::Code(code) => self.inline(MdInline::Code(code.into_string())),
            Event::Html(html) | Event::InlineHtml(html) => self.text(&html, range.end),
            Event::InlineMath(math) | Event::DisplayMath(math) => self.text(&math, range.end),
            Event::FootnoteReference(label) => self.text(&format!("[^{label}]"), range.end),
            Event::SoftBreak => self.inline(MdInline::SoftBreak),
            Event::HardBreak => self.inline(MdInline::HardBreak),
            Event::Rule => {
                self.close_implicit();
                self.push_block(MdBlock::Rule);
            }
            Event::TaskListMarker(checked) => {
                let item = self.frames.iter_mut().rev().find_map(|frame| match frame {
                    Frame::Container {
                        kind: ContainerKind::Item { checked },
                        ..
                    } => Some(checked),
                    _ => None,
                });
                if let Some(slot) = item {
                    *slot = Some(checked);
                }
            }
        }
    }

    fn start(&mut self, tag: Tag<'_>, range: Range<usize>) {
        match tag {
            Tag::Emphasis => self.open_span(Span::Emphasis),
            Tag::Strong => self.open_span(Span::Strong),
            Tag::Strikethrough => self.open_span(Span::Strikethrough),
            Tag::Superscript | Tag::Subscript => self.open_span(Span::Plain { suffix: None }),
            Tag::Link {
                link_type,
                dest_url,
                ..
            } => {
                let mut url = dest_url
                    .trim_matches(|c: char| c.is_ascii_whitespace())
                    .to_owned();
                if link_type == LinkType::Email
                    && !url
                        .get(..7)
                        .is_some_and(|s| s.eq_ignore_ascii_case("mailto:"))
                {
                    url.insert_str(0, "mailto:");
                }
                let safe = is_safe_url(&url);
                self.open_span(Span::Link { url, safe });
            }
            Tag::Image { dest_url, .. } => self.open_span(Span::Image {
                url: dest_url.into_string(),
            }),
            Tag::Paragraph => self.open_leaf(Leaf::Paragraph { implicit: false }),
            Tag::Heading { level, .. } => self.open_leaf(Leaf::Heading(level as u8)),
            Tag::HtmlBlock => self.open_leaf(Leaf::Html(String::new())),
            Tag::CodeBlock(kind) => {
                let (language, fenced) = match kind {
                    CodeBlockKind::Fenced(info) => {
                        (info.split_whitespace().next().map(str::to_owned), true)
                    }
                    CodeBlockKind::Indented => (None, false),
                };
                let block = self.source.get(range.clone()).unwrap_or_default();
                let content_end = block
                    .find('\n')
                    .map_or(range.end, |at| range.start + at + 1);
                let leaf = Leaf::Code {
                    language,
                    fenced,
                    code: String::new(),
                    content_end,
                    closed: !fenced,
                };
                self.open_leaf(leaf);
            }
            Tag::List(_) | Tag::BlockQuote(_) if self.nesting >= MAX_NESTING_DEPTH => {
                self.open_leaf(Leaf::Paragraph { implicit: false });
                self.flatten = 1;
            }
            Tag::List(first) => {
                self.close_implicit();
                self.nesting += 1;
                self.frames.push(Frame::List {
                    ordered: first.is_some(),
                    start: first.unwrap_or(1),
                    items: Vec::new(),
                });
            }
            Tag::BlockQuote(_) => {
                self.close_implicit();
                self.nesting += 1;
                self.frames.push(Frame::Container {
                    kind: ContainerKind::Quote,
                    blocks: Vec::new(),
                });
            }
            Tag::Item => {
                self.close_implicit();
                self.frames.push(Frame::Container {
                    kind: ContainerKind::Item { checked: None },
                    blocks: Vec::new(),
                });
            }
            Tag::Table(alignments) => {
                self.close_implicit();
                let alignments = alignments
                    .iter()
                    .take(MAX_TABLE_COLUMNS)
                    .map(|a| convert_alignment(*a))
                    .collect();
                self.frames.push(Frame::Table {
                    alignments,
                    header: Vec::new(),
                    rows: Vec::new(),
                    row: Vec::new(),
                });
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some(Frame::Table { row, .. }) = self.frames.last_mut() {
                    row.clear();
                }
            }
            Tag::TableCell => self.open_leaf(Leaf::Cell),
            Tag::FootnoteDefinition(_)
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::MetadataBlock(_) => self.close_implicit(),
        }
    }

    fn end(&mut self, end: TagEnd, range: Range<usize>) {
        match end {
            TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::Superscript
            | TagEnd::Subscript
            | TagEnd::Link
            | TagEnd::Image => self.close_span(),
            TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::HtmlBlock | TagEnd::TableCell => {
                self.close_leaf()
            }
            TagEnd::CodeBlock => {
                // A fence is closed iff a closing fence line follows the last content line.
                if let Some(Leaf::Code {
                    fenced: true,
                    content_end,
                    closed,
                    ..
                }) = &mut self.leaf
                {
                    let tail = self.source.get(*content_end..range.end).unwrap_or_default();
                    *closed = tail.contains(['`', '~']);
                }
                self.close_leaf();
            }
            TagEnd::List(_) => {
                self.close_implicit();
                if let Some(Frame::List {
                    ordered,
                    start,
                    items,
                }) = self.pop_frame(|frame| matches!(frame, Frame::List { .. }))
                {
                    self.nesting = self.nesting.saturating_sub(1);
                    self.push_block(MdBlock::List {
                        ordered,
                        start,
                        items,
                    });
                }
            }
            TagEnd::BlockQuote(_) => {
                self.close_implicit();
                let quote = |frame: &Frame| {
                    matches!(
                        frame,
                        Frame::Container {
                            kind: ContainerKind::Quote,
                            ..
                        }
                    )
                };
                if let Some(Frame::Container { mut blocks, .. }) = self.pop_frame(quote) {
                    self.nesting = self.nesting.saturating_sub(1);
                    self.seal_fence(&mut blocks, &range);
                    self.push_block(MdBlock::BlockQuote(blocks));
                }
            }
            TagEnd::Item => {
                self.close_implicit();
                let item = |frame: &Frame| {
                    matches!(
                        frame,
                        Frame::Container {
                            kind: ContainerKind::Item { .. },
                            ..
                        }
                    )
                };
                if let Some(Frame::Container {
                    kind: ContainerKind::Item { checked },
                    mut blocks,
                }) = self.pop_frame(item)
                {
                    self.seal_fence(&mut blocks, &range);
                    match self.frames.last_mut() {
                        Some(Frame::List { items, .. }) => {
                            items.push(MdListItem { checked, blocks })
                        }
                        _ => blocks.into_iter().for_each(|block| self.push_block(block)),
                    }
                }
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                if let Some(Frame::Table {
                    header, rows, row, ..
                }) = self.frames.last_mut()
                {
                    let cells = std::mem::take(row);
                    if end == TagEnd::TableHead {
                        *header = cells;
                    } else {
                        rows.push(cells);
                    }
                }
            }
            TagEnd::Table => {
                if let Some(Frame::Table {
                    alignments,
                    header,
                    rows,
                    ..
                }) = self.pop_frame(|frame| matches!(frame, Frame::Table { .. }))
                {
                    self.push_block(MdBlock::Table {
                        alignments,
                        header,
                        rows,
                    });
                }
            }
            TagEnd::FootnoteDefinition
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::MetadataBlock(_) => self.close_implicit(),
        }
    }

    /// A fence still open when its list item or quote (`range`) ends can gain
    /// nothing more once anything but whitespace or a quote marker follows
    /// the container, so it is final and reads as closed. A fence open at the
    /// end of the source stays unclosed.
    fn seal_fence(&self, blocks: &mut [MdBlock], range: &Range<usize>) {
        fn seal(blocks: &mut [MdBlock]) {
            match blocks.last_mut() {
                Some(MdBlock::CodeBlock { closed, .. }) => *closed = true,
                Some(MdBlock::BlockQuote(inner)) => seal(inner),
                Some(MdBlock::List { items, .. }) => {
                    if let Some(item) = items.last_mut() {
                        seal(&mut item.blocks);
                    }
                }
                _ => {}
            }
        }
        let followed = !range.is_empty()
            && self
                .source
                .get(range.end..)
                .is_some_and(|rest| rest.chars().any(|c| !c.is_whitespace() && c != '>'));
        if followed {
            seal(blocks);
        }
    }

    /// Inside a flattened region every tag is dropped and only its text kept,
    /// block boundaries becoming soft breaks and link or image URLs ` (url)`.
    fn flatten_event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => {
                self.flatten += 1;
                match tag {
                    Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => {
                        self.flatten_urls.push(dest_url.into_string());
                    }
                    Tag::Emphasis
                    | Tag::Strong
                    | Tag::Strikethrough
                    | Tag::Superscript
                    | Tag::Subscript => {}
                    _ => self.flatten_break(),
                }
            }
            Event::End(end) => {
                self.flatten -= 1;
                match end {
                    TagEnd::Link | TagEnd::Image => {
                        let url = self.flatten_urls.pop().unwrap_or_default();
                        self.inline(MdInline::Text(format!(" ({url})")));
                    }
                    TagEnd::Emphasis
                    | TagEnd::Strong
                    | TagEnd::Strikethrough
                    | TagEnd::Superscript
                    | TagEnd::Subscript => {}
                    _ => self.flatten_break(),
                }
                if self.flatten == 0 {
                    if let Some(SpanFrame { children, .. }) = self.spans.first_mut() {
                        while children.last() == Some(&MdInline::SoftBreak) {
                            children.pop();
                        }
                    }
                    self.close_leaf();
                }
            }
            Event::Text(text)
            | Event::Code(text)
            | Event::Html(text)
            | Event::InlineHtml(text)
            | Event::InlineMath(text)
            | Event::DisplayMath(text) => {
                for (index, line) in text.split('\n').enumerate() {
                    if index > 0 {
                        self.flatten_break();
                    }
                    self.inline(MdInline::Text(line.to_owned()));
                }
            }
            Event::FootnoteReference(label) => self.inline(MdInline::Text(format!("[^{label}]"))),
            Event::TaskListMarker(checked) => {
                self.inline(MdInline::Text(
                    if checked { "[x] " } else { "[ ] " }.to_owned(),
                ));
            }
            Event::SoftBreak | Event::HardBreak | Event::Rule => self.flatten_break(),
        }
    }

    fn flatten_break(&mut self) {
        if let Some(SpanFrame { children, .. }) = self.spans.last()
            && !children.is_empty()
            && children.last() != Some(&MdInline::SoftBreak)
        {
            self.inline(MdInline::SoftBreak);
        }
    }

    fn text(&mut self, text: &str, end: usize) {
        match &mut self.leaf {
            Some(Leaf::Code {
                code, content_end, ..
            }) => {
                code.push_str(text);
                *content_end = end;
            }
            Some(Leaf::Html(html)) => html.push_str(text),
            _ => self.inline(MdInline::Text(text.to_owned())),
        }
    }

    fn inline(&mut self, inline: MdInline) {
        if self.leaf.is_none() {
            self.open_leaf(Leaf::Paragraph { implicit: true });
        }
        if let Some(frame) = self.spans.last_mut() {
            push_inline(&mut frame.children, inline);
        }
    }

    fn open_span(&mut self, span: Span) {
        if self.leaf.is_none() {
            self.open_leaf(Leaf::Paragraph { implicit: true });
        }
        let span = match span {
            Span::Image { .. } => {
                self.image_depth += 1;
                span
            }
            Span::Link { url, .. }
                if self.image_depth > 0 || self.styled_depth >= MAX_INLINE_DEPTH =>
            {
                Span::Plain {
                    suffix: Some(format!(" ({url})")),
                }
            }
            Span::Plain { .. } => span,
            _ if self.image_depth > 0 || self.styled_depth >= MAX_INLINE_DEPTH => {
                Span::Plain { suffix: None }
            }
            _ => {
                self.styled_depth += 1;
                span
            }
        };
        self.spans.push(SpanFrame {
            span,
            children: Vec::new(),
        });
    }

    fn close_span(&mut self) {
        if self.spans.len() < 2 {
            return;
        }
        let Some(SpanFrame { span, children }) = self.spans.pop() else {
            return;
        };
        let Some(parent) = self.spans.last_mut() else {
            return;
        };
        let inline = match span {
            Span::Emphasis => MdInline::Emphasis(children),
            Span::Strong => MdInline::Strong(children),
            Span::Strikethrough => MdInline::Strikethrough(children),
            Span::Link { url, safe } => MdInline::Link {
                url,
                safe,
                content: children,
            },
            Span::Image { url } => {
                self.image_depth = self.image_depth.saturating_sub(1);
                let mut alt = String::new();
                plain_text(&children, &mut alt);
                let text = if alt.is_empty() {
                    format!("({url})")
                } else {
                    format!("{alt} ({url})")
                };
                push_inline(&mut parent.children, MdInline::Text(text));
                return;
            }
            Span::Root | Span::Plain { suffix: None } => {
                children
                    .into_iter()
                    .for_each(|child| push_inline(&mut parent.children, child));
                return;
            }
            Span::Plain {
                suffix: Some(suffix),
            } => {
                children
                    .into_iter()
                    .for_each(|child| push_inline(&mut parent.children, child));
                push_inline(&mut parent.children, MdInline::Text(suffix));
                return;
            }
        };
        self.styled_depth = self.styled_depth.saturating_sub(1);
        push_inline(&mut parent.children, inline);
    }

    fn open_leaf(&mut self, leaf: Leaf) {
        self.close_implicit();
        if self.leaf.is_some() {
            self.close_leaf();
        }
        self.leaf = Some(leaf);
        self.spans.push(SpanFrame {
            span: Span::Root,
            children: Vec::new(),
        });
    }

    fn close_implicit(&mut self) {
        if matches!(self.leaf, Some(Leaf::Paragraph { implicit: true })) {
            self.close_leaf();
        }
    }

    fn close_leaf(&mut self) {
        while self.spans.len() > 1 {
            self.close_span();
        }
        let content = self
            .spans
            .pop()
            .map(|frame| frame.children)
            .unwrap_or_default();
        let Some(leaf) = self.leaf.take() else {
            return;
        };
        let block = match leaf {
            Leaf::Paragraph { .. } => MdBlock::Paragraph(content),
            Leaf::Heading(level) => MdBlock::Heading { level, content },
            Leaf::Html(html) => {
                let mut lines = Vec::new();
                for (index, line) in html.trim_end_matches(['\n', '\r']).split('\n').enumerate() {
                    if index > 0 {
                        lines.push(MdInline::HardBreak);
                    }
                    push_inline(
                        &mut lines,
                        MdInline::Text(line.trim_end_matches('\r').to_owned()),
                    );
                }
                MdBlock::Paragraph(lines)
            }
            Leaf::Code {
                language,
                mut code,
                closed,
                ..
            } => {
                if code.ends_with('\n') {
                    code.pop();
                }
                MdBlock::CodeBlock {
                    language,
                    code,
                    closed,
                }
            }
            Leaf::Cell => {
                if let Some(Frame::Table { row, .. }) = self.frames.last_mut() {
                    if row.len() < MAX_TABLE_COLUMNS {
                        row.push(content);
                    } else if let Some(last) = row.last_mut() {
                        push_inline(last, MdInline::Text(" | ".to_owned()));
                        content
                            .into_iter()
                            .for_each(|inline| push_inline(last, inline));
                    }
                    return;
                }
                MdBlock::Paragraph(content)
            }
        };
        self.push_block(block);
    }

    /// Pops the innermost frame when `wanted` accepts it.
    fn pop_frame(&mut self, wanted: impl Fn(&Frame) -> bool) -> Option<Frame> {
        if self.frames.len() > 1 && self.frames.last().is_some_and(wanted) {
            self.frames.pop()
        } else {
            None
        }
    }

    /// Appends a finished block to the innermost container (root, quote or item).
    fn push_block(&mut self, block: MdBlock) {
        let container = self.frames.iter_mut().rev().find_map(|frame| match frame {
            Frame::Container { blocks, .. } => Some(blocks),
            _ => None,
        });
        if let Some(blocks) = container {
            blocks.push(block);
        }
    }

    fn finish(mut self) -> Vec<MdBlock> {
        if self.leaf.is_some() {
            self.close_leaf();
        }
        // pulldown-cmark balances its tags; this only guards against a frame left open.
        while self.frames.len() > 1 {
            let before = self.frames.len();
            match self.frames.last() {
                Some(Frame::List { .. }) => self.end(TagEnd::List(false), 0..0),
                Some(Frame::Table { .. }) => self.end(TagEnd::Table, 0..0),
                Some(Frame::Container {
                    kind: ContainerKind::Quote,
                    ..
                }) => self.end(TagEnd::BlockQuote(None), 0..0),
                _ => self.end(TagEnd::Item, 0..0),
            }
            if self.frames.len() == before {
                self.frames.pop();
            }
        }
        match self.frames.pop() {
            Some(Frame::Container { blocks, .. }) => blocks,
            _ => Vec::new(),
        }
    }
}

fn convert_alignment(alignment: Alignment) -> MdAlignment {
    match alignment {
        Alignment::None => MdAlignment::None,
        Alignment::Left => MdAlignment::Left,
        Alignment::Center => MdAlignment::Center,
        Alignment::Right => MdAlignment::Right,
    }
}

/// Pushes `inline`, merging it into a preceding text run; empty text is dropped.
fn push_inline(children: &mut Vec<MdInline>, inline: MdInline) {
    match (children.last_mut(), inline) {
        (_, MdInline::Text(text)) if text.is_empty() => {}
        (Some(MdInline::Text(last)), MdInline::Text(text)) => last.push_str(&text),
        (_, inline) => children.push(inline),
    }
}

/// Appends the visible text of `inlines` to `out`, breaks as spaces.
pub(crate) fn plain_text(inlines: &[MdInline], out: &mut String) {
    for inline in inlines {
        match inline {
            MdInline::Text(text) | MdInline::Code(text) => out.push_str(text),
            MdInline::Emphasis(children)
            | MdInline::Strong(children)
            | MdInline::Strikethrough(children) => {
                plain_text(children, out);
            }
            MdInline::Link { content, .. } => plain_text(content, out),
            MdInline::SoftBreak | MdInline::HardBreak => out.push(' '),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> MdInline {
        MdInline::Text(s.to_owned())
    }

    fn para(s: &str) -> MdBlock {
        MdBlock::Paragraph(vec![text(s)])
    }

    /// Everything a reader can see: text, code, link targets, code languages,
    /// table cells, and checked task markers as `[x]`.
    fn visible(blocks: &[MdBlock], out: &mut String) {
        for block in blocks {
            match block {
                MdBlock::Paragraph(content) | MdBlock::Heading { content, .. } => {
                    visible_inline(content, out)
                }
                MdBlock::CodeBlock { language, code, .. } => {
                    out.push_str(language.as_deref().unwrap_or_default());
                    out.push(' ');
                    out.push_str(code);
                }
                MdBlock::List { items, .. } => {
                    for item in items {
                        if item.checked == Some(true) {
                            out.push_str("[x]");
                        }
                        visible(&item.blocks, out);
                    }
                }
                MdBlock::BlockQuote(blocks) => visible(blocks, out),
                MdBlock::Table { header, rows, .. } => {
                    for cell in header.iter().chain(rows.iter().flatten()) {
                        visible_inline(cell, out);
                        out.push(' ');
                    }
                }
                MdBlock::Rule => {}
            }
            out.push(' ');
        }
    }

    fn visible_inline(inlines: &[MdInline], out: &mut String) {
        for inline in inlines {
            match inline {
                MdInline::Text(s) | MdInline::Code(s) => out.push_str(s),
                MdInline::Emphasis(c) | MdInline::Strong(c) | MdInline::Strikethrough(c) => {
                    visible_inline(c, out)
                }
                MdInline::Link { url, content, .. } => {
                    visible_inline(content, out);
                    out.push_str(url);
                }
                MdInline::SoftBreak | MdInline::HardBreak => out.push(' '),
            }
        }
    }

    fn visible_of(source: &str) -> String {
        let mut out = String::new();
        visible(&parse_markdown(source), &mut out);
        out
    }

    fn non_space(s: &str) -> String {
        s.chars().filter(|c| !c.is_whitespace()).collect()
    }

    /// Whether every alphabetic character of `needle` appears in `hay` in order.
    fn alphabetic_subsequence(needle: &str, hay: &str) -> bool {
        let mut hay = hay.chars().filter(|c| c.is_alphabetic());
        needle
            .chars()
            .filter(|c| c.is_alphabetic())
            .all(|c| hay.any(|h| h == c))
    }

    fn char_prefixes(source: &str) -> impl Iterator<Item = &str> {
        (0..=source.len())
            .filter(|&at| source.is_char_boundary(at))
            .map(|at| &source[..at])
    }

    const FIXTURE: &str = "# Plan för 日本 🚀\n\
        \n\
        Some *emphasis*, **strong**, ~~gone~~ and `code` with a [link](https://example.com/a) \
        and <span>raw html</span>.\n\
        Second line of the paragraph\\\n\
        after a hard break.\n\
        \n\
        - first item\n\
        - [x] checked task\n\
        - [ ] open task\n\
        \x20 - nested item\n\
        \n\
        3. third\n\
        4. fourth\n\
        \n\
        > quoted *text*\n\
        > still quoted\n\
        \n\
        | name | kind |\n\
        | :--- | ---: |\n\
        | alpha | beta |\n\
        | gamma | delta |\n\
        \n\
        ```rust\n\
        fn main() {\n\
        \x20   println!(\"hi\");\n\
        }\n\
        ```\n\
        \n\
        <div class=\"x\">\n\
        block html\n\
        </div>\n\
        \n\
        ![diagram alt](https://example.com/d.png)\n\
        \n\
        ---\n\
        \n\
        Setext heading\n\
        ==============\n\
        \n\
        Final words.\n";

    #[test]
    fn parses_every_block_kind() {
        let blocks = parse_markdown(FIXTURE);
        assert_eq!(
            blocks[0],
            MdBlock::Heading {
                level: 1,
                content: vec![text("Plan för 日本 🚀")]
            }
        );
        let MdBlock::Paragraph(content) = &blocks[1] else {
            panic!("{:?}", blocks[1])
        };
        assert_eq!(content[1], MdInline::Emphasis(vec![text("emphasis")]));
        assert_eq!(content[3], MdInline::Strong(vec![text("strong")]));
        assert_eq!(content[5], MdInline::Strikethrough(vec![text("gone")]));
        assert_eq!(content[7], MdInline::Code("code".to_owned()));
        assert_eq!(
            content[9],
            MdInline::Link {
                url: "https://example.com/a".to_owned(),
                safe: true,
                content: vec![text("link")]
            }
        );
        assert_eq!(content[10], text(" and <span>raw html</span>."));
        assert!(content.contains(&MdInline::SoftBreak) && content.contains(&MdInline::HardBreak));

        let MdBlock::List {
            ordered: false,
            start: 1,
            items,
        } = &blocks[2]
        else {
            panic!("{:?}", blocks[2])
        };
        assert_eq!(
            items[0],
            MdListItem {
                checked: None,
                blocks: vec![para("first item")]
            }
        );
        assert_eq!(items[1].checked, Some(true));
        assert_eq!(items[2].checked, Some(false));
        let MdBlock::List { items: nested, .. } = &items[2].blocks[1] else {
            panic!("{:?}", items[2])
        };
        assert_eq!(nested[0].blocks, vec![para("nested item")]);

        assert!(
            matches!(&blocks[3], MdBlock::List { ordered: true, start: 3, items } if items.len() == 2)
        );
        let MdBlock::BlockQuote(quoted) = &blocks[4] else {
            panic!("{:?}", blocks[4])
        };
        assert_eq!(quoted.len(), 1);

        let MdBlock::Table {
            alignments,
            header,
            rows,
        } = &blocks[5]
        else {
            panic!("{:?}", blocks[5])
        };
        assert_eq!(alignments, &[MdAlignment::Left, MdAlignment::Right]);
        assert_eq!(header, &[vec![text("name")], vec![text("kind")]]);
        assert_eq!(rows[1], vec![vec![text("gamma")], vec![text("delta")]]);

        assert_eq!(
            blocks[6],
            MdBlock::CodeBlock {
                language: Some("rust".to_owned()),
                code: "fn main() {\n    println!(\"hi\");\n}".to_owned(),
                closed: true
            }
        );
        assert_eq!(
            blocks[7],
            MdBlock::Paragraph(vec![
                text("<div class=\"x\">"),
                MdInline::HardBreak,
                text("block html"),
                MdInline::HardBreak,
                text("</div>")
            ])
        );
        assert_eq!(blocks[8], para("diagram alt (https://example.com/d.png)"));
        assert_eq!(blocks[9], MdBlock::Rule);
        assert_eq!(
            blocks[10],
            MdBlock::Heading {
                level: 1,
                content: vec![text("Setext heading")]
            }
        );
        assert_eq!(blocks[11], para("Final words."));
        assert_eq!(blocks.len(), 12);
    }

    #[test]
    fn indented_code_is_closed_without_language() {
        assert_eq!(
            parse_markdown("    let x = 1;\n"),
            vec![MdBlock::CodeBlock {
                language: None,
                code: "let x = 1;".to_owned(),
                closed: true
            }]
        );
    }

    #[test]
    fn raw_html_is_literal_text() {
        assert_eq!(
            parse_markdown("a <b onclick=\"x()\">b</b> c"),
            vec![para("a <b onclick=\"x()\">b</b> c")]
        );
        assert_eq!(
            parse_markdown("<!-- hidden? -->"),
            vec![para("<!-- hidden? -->")]
        );
        assert_eq!(
            parse_markdown("<script>\nalert(1)\n</script>"),
            vec![MdBlock::Paragraph(vec![
                text("<script>"),
                MdInline::HardBreak,
                text("alert(1)"),
                MdInline::HardBreak,
                text("</script>")
            ])]
        );
        let MdBlock::List { items, .. } = &parse_markdown("- <img src=x onerror=y>")[0] else {
            panic!()
        };
        assert_eq!(items[0].blocks, vec![para("<img src=x onerror=y>")]);
    }

    #[test]
    fn images_become_alt_text_and_url() {
        assert_eq!(
            parse_markdown("![a *cat*](http://x/c.png \"t\")"),
            vec![para("a cat (http://x/c.png)")]
        );
        assert_eq!(
            parse_markdown("![](data:image/png;base64,AA)"),
            vec![para("(data:image/png;base64,AA)")]
        );
        assert_eq!(
            parse_markdown("[![badge](http://i)](https://t)"),
            vec![MdBlock::Paragraph(vec![MdInline::Link {
                url: "https://t".to_owned(),
                safe: true,
                content: vec![text("badge (http://i)")]
            }])]
        );
    }

    fn first_link(source: &str) -> (String, bool) {
        let blocks = parse_markdown(source);
        let Some(MdBlock::Paragraph(content)) = blocks.first() else {
            panic!("{source:?}: {blocks:?}")
        };
        content
            .iter()
            .find_map(|inline| match inline {
                MdInline::Link { url, safe, .. } => Some((url.clone(), *safe)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{source:?}: {blocks:?}"))
    }

    #[test]
    fn link_safety_by_scheme() {
        for (source, url, safe) in [
            ("[a](http://x)", "http://x", true),
            ("[a](HTTPS://x/y?q=1)", "HTTPS://x/y?q=1", true),
            ("[a](MailTo:me@x)", "MailTo:me@x", true),
            ("[a](<\u{1}https://x>)", "\u{1}https://x", false),
            ("[a](<  https://x  >)", "https://x", true),
            ("[a](/relative)", "/relative", false),
            ("[a](relative/path)", "relative/path", false),
            ("[a](#frag)", "#frag", false),
            ("[a](javascript:alert(1))", "javascript:alert(1)", false),
            ("[a](JaVaScRiPt:alert(1))", "JaVaScRiPt:alert(1)", false),
            ("[a](javascript&#58;alert(1))", "javascript:alert(1)", false),
            ("[a](data:text/html,x)", "data:text/html,x", false),
            ("[a](file:///etc/passwd)", "file:///etc/passwd", false),
            ("[a](myapp://open)", "myapp://open", false),
            ("[a](<java\tscript:x>)", "java\tscript:x", false),
            ("[a](<http://x\u{7}y>)", "http://x\u{7}y", false),
            ("[a](ht&#x0;tp://x)", "ht\u{fffd}tp://x", false),
            ("<https://auto.link/p>", "https://auto.link/p", true),
            ("<someone@example.com>", "mailto:someone@example.com", true),
        ] {
            assert_eq!(first_link(source), (url.to_owned(), safe), "{source:?}");
        }
    }

    #[test]
    fn link_keeps_text_and_target() {
        assert_eq!(
            parse_markdown("[**click** me](javascript:x)"),
            vec![MdBlock::Paragraph(vec![MdInline::Link {
                url: "javascript:x".to_owned(),
                safe: false,
                content: vec![MdInline::Strong(vec![text("click")]), text(" me")]
            }])]
        );
    }

    fn nesting_depth(blocks: &[MdBlock]) -> usize {
        blocks
            .iter()
            .map(|block| match block {
                MdBlock::List { items, .. } => {
                    1 + items
                        .iter()
                        .map(|i| nesting_depth(&i.blocks))
                        .max()
                        .unwrap_or(0)
                }
                MdBlock::BlockQuote(inner) => 1 + nesting_depth(inner),
                _ => 0,
            })
            .max()
            .unwrap_or(0)
    }

    fn inline_depth(inlines: &[MdInline]) -> usize {
        inlines
            .iter()
            .map(|inline| match inline {
                MdInline::Emphasis(c) | MdInline::Strong(c) | MdInline::Strikethrough(c) => {
                    1 + inline_depth(c)
                }
                MdInline::Link { content, .. } => 1 + inline_depth(content),
                _ => 0,
            })
            .max()
            .unwrap_or(0)
    }

    fn max_inline_depth(blocks: &[MdBlock]) -> usize {
        blocks
            .iter()
            .map(|block| match block {
                MdBlock::Paragraph(c) | MdBlock::Heading { content: c, .. } => inline_depth(c),
                MdBlock::List { items, .. } => items
                    .iter()
                    .map(|i| max_inline_depth(&i.blocks))
                    .max()
                    .unwrap_or(0),
                MdBlock::BlockQuote(inner) => max_inline_depth(inner),
                MdBlock::Table { header, rows, .. } => header
                    .iter()
                    .chain(rows.iter().flatten())
                    .map(|c| inline_depth(c))
                    .max()
                    .unwrap_or(0),
                _ => 0,
            })
            .max()
            .unwrap_or(0)
    }

    fn max_columns(blocks: &[MdBlock]) -> usize {
        blocks
            .iter()
            .map(|block| match block {
                MdBlock::Table {
                    alignments,
                    header,
                    rows,
                } => rows
                    .iter()
                    .map(Vec::len)
                    .chain([alignments.len(), header.len()])
                    .max()
                    .unwrap_or(0),
                MdBlock::List { items, .. } => items
                    .iter()
                    .map(|i| max_columns(&i.blocks))
                    .max()
                    .unwrap_or(0),
                MdBlock::BlockQuote(inner) => max_columns(inner),
                _ => 0,
            })
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn deep_nesting_flattens_to_text() {
        let quotes = format!("{} deep [x](http://u) *e*\n", ">".repeat(40));
        let blocks = parse_markdown(&quotes);
        assert_eq!(nesting_depth(&blocks), MAX_NESTING_DEPTH);
        assert!(non_space(&visible_of(&quotes)).contains("deepx(http://u)e"));

        let mut lists = String::new();
        for level in 0..30 {
            lists.push_str(&format!("{}- level{level}\n", "  ".repeat(level)));
        }
        let blocks = parse_markdown(&lists);
        assert_eq!(nesting_depth(&blocks), MAX_NESTING_DEPTH);
        let shown = visible_of(&lists);
        assert!(
            (0..30).all(|level| shown.contains(&format!("level{level}"))),
            "{shown}"
        );
    }

    #[test]
    fn deep_spans_keep_text_without_styling() {
        let source = format!("{}core{}", "*".repeat(200), "*".repeat(200));
        let blocks = parse_markdown(&source);
        assert!(max_inline_depth(&blocks) <= MAX_INLINE_DEPTH);
        assert!(visible_of(&source).contains("core"));

        let links = format!("{}x{}", "**[".repeat(40), "](http://u)**".repeat(40));
        assert!(max_inline_depth(&parse_markdown(&links)) <= MAX_INLINE_DEPTH);
        assert!(alphabetic_subsequence(&links, &visible_of(&links)));
    }

    #[test]
    fn wide_tables_merge_extra_columns() {
        let columns = 100;
        let header: String = (0..columns).map(|c| format!("| h{c} ")).collect();
        let delimiter = "| - ".repeat(columns);
        let row: String = (0..columns).map(|c| format!("| r{c} ")).collect();
        let source = format!("{header}|\n{delimiter}|\n{row}|\n");
        let blocks = parse_markdown(&source);
        assert_eq!(max_columns(&blocks), MAX_TABLE_COLUMNS);
        let MdBlock::Table { header: cells, .. } = &blocks[0] else {
            panic!("{blocks:?}")
        };
        let merged = format!("h{} | h{} | ", MAX_TABLE_COLUMNS - 1, MAX_TABLE_COLUMNS);
        assert!(
            matches!(cells.last().map(Vec::as_slice), Some([MdInline::Text(t)]) if t.starts_with(&merged))
        );
        let shown = visible_of(&source);
        assert!(
            (0..columns)
                .all(|c| shown.contains(&format!("h{c}")) && shown.contains(&format!("r{c}")))
        );
    }

    #[test]
    fn open_fence_holds_everything_after_it() {
        let source = "intro\n\n```rust\nfn main() {\n    x();\n}\n```\n\nafter";
        let fence = source.find("```").unwrap_or_default();
        let body_start = fence + "```rust\n".len();
        let closer = source.rfind("```").unwrap_or_default();
        for prefix in char_prefixes(source) {
            let blocks = parse_markdown(prefix);
            let at = prefix.len();
            if at < fence + 3 {
                continue;
            }
            let code_block = blocks
                .get(1)
                .unwrap_or_else(|| panic!("{prefix:?}: {blocks:?}"));
            let MdBlock::CodeBlock {
                language,
                code,
                closed,
            } = code_block
            else {
                panic!("{prefix:?}: {blocks:?}")
            };
            assert_eq!(
                language.as_deref(),
                "rust"
                    .get(..at.min(body_start - 1) - fence - 3)
                    .filter(|l| !l.is_empty())
            );
            if at < closer + 3 {
                let body = prefix.get(body_start..).unwrap_or_default();
                // A partial line of only indentation is not yet content.
                assert_eq!(
                    (code.trim_end(), *closed),
                    (body.trim_end(), false),
                    "{prefix:?}"
                );
            } else {
                assert_eq!(
                    (code.as_str(), *closed),
                    ("fn main() {\n    x();\n}", true),
                    "{prefix:?}"
                );
            }
        }
    }

    #[test]
    fn unclosed_fence_inside_quote_and_list() {
        assert_eq!(
            parse_markdown("> ```\n> code"),
            vec![MdBlock::BlockQuote(vec![MdBlock::CodeBlock {
                language: None,
                code: "code".to_owned(),
                closed: false
            }])]
        );
        assert!(matches!(
            &parse_markdown("> ```\n> code\n> ```")[0],
            MdBlock::BlockQuote(inner) if matches!(inner[0], MdBlock::CodeBlock { closed: true, .. })
        ));
        assert!(matches!(
            &parse_markdown("```\ncode\n    ```")[0],
            MdBlock::CodeBlock { closed: false, .. }
        ));
        assert!(matches!(
            &parse_markdown("~~~~\nx\n~~~~  \n")[0],
            MdBlock::CodeBlock { closed: true, .. }
        ));
    }

    #[test]
    fn half_written_inlines_stay_text() {
        assert_eq!(parse_markdown("so **bold"), vec![para("so **bold")]);
        assert_eq!(parse_markdown("a _b ~~c"), vec![para("a _b ~~c")]);
        assert_eq!(
            parse_markdown("see [docs](https://ex"),
            vec![para("see [docs](https://ex")]
        );
        assert_eq!(parse_markdown("see [docs"), vec![para("see [docs")]);
        assert_eq!(parse_markdown("run `cargo"), vec![para("run `cargo")]);
        assert_eq!(parse_markdown("a <b"), vec![para("a <b")]);
        assert_eq!(
            parse_markdown("![alt](http://x"),
            vec![para("![alt](http://x")]
        );
    }

    #[test]
    fn half_written_table_rows_keep_their_cells() {
        let source = "| left | right |\n| --- | --- |\n| one | two |\n| three | fo";
        for prefix in char_prefixes(source) {
            assert!(
                alphabetic_subsequence(prefix, &visible_of(prefix)),
                "{prefix:?}"
            );
        }
        let MdBlock::Table { rows, .. } = &parse_markdown(source)[0] else {
            panic!()
        };
        assert_eq!(rows[1], vec![vec![text("three")], vec![text("fo")]]);
    }

    #[test]
    fn every_prefix_parses_keeps_text_and_settles_earlier_blocks() {
        let full = parse_markdown(FIXTURE);
        let mut previous = Vec::new();
        for prefix in char_prefixes(FIXTURE) {
            let blocks = parse_markdown(prefix);
            assert!(
                alphabetic_subsequence(prefix, &visible_of(prefix)),
                "{prefix:?}: {blocks:?}"
            );
            let settled = if prefix.ends_with('\n') {
                blocks.len().saturating_sub(1)
            } else {
                blocks.len().saturating_sub(2)
            };
            assert_eq!(blocks[..settled], full[..settled], "{prefix:?}");
            assert!(
                stable_prefix_len(&previous, &blocks) + 2 >= previous.len(),
                "{prefix:?}"
            );
            previous = blocks;
        }
    }

    #[test]
    fn documented_exceptions() {
        // An unterminated last line can still fold into the block before it.
        assert_eq!(parse_markdown("para\n#").len(), 2);
        assert_eq!(
            parse_markdown("para\n#x"),
            vec![MdBlock::Paragraph(vec![
                text("para"),
                MdInline::SoftBreak,
                text("#x")
            ])]
        );
        for (started, folded) in [
            ("para\n***", "para\n***x"),
            ("para\n<div", "para\n<divx"),
            ("- a\n#", "- a\n#x"),
        ] {
            assert_eq!(
                (parse_markdown(started).len(), parse_markdown(folded).len()),
                (2, 1),
                "{started:?}"
            );
        }
        // A later reference definition turns earlier `[label]` text into a link.
        assert_eq!(parse_markdown("[ref]\n\nmore")[0], para("[ref]"));
        assert!(matches!(
            &parse_markdown("[ref]\n\n[ref]: https://x")[0],
            MdBlock::Paragraph(content) if matches!(content[0], MdInline::Link { safe: true, .. })
        ));
    }

    /// The first code block anywhere in `blocks`, depth first.
    fn first_fence(blocks: &[MdBlock]) -> Option<(String, bool)> {
        blocks.iter().find_map(|block| match block {
            MdBlock::CodeBlock { code, closed, .. } => Some((code.clone(), *closed)),
            MdBlock::BlockQuote(inner) => first_fence(inner),
            MdBlock::List { items, .. } => items.iter().find_map(|i| first_fence(&i.blocks)),
            _ => None,
        })
    }

    #[test]
    fn a_fence_its_container_ended_is_closed_at_every_prefix() {
        // (fixture, prefix that ends inside the still-open fence)
        let fixtures = [
            (
                "- ```\n  code\n  more\n- second\n",
                "- ```\n  code\n  more\n",
            ),
            (
                "> ```\n> code\n> more\n\nafter\n",
                "> ```\n> code\n> more\n",
            ),
        ];
        for (fixture, open) in fixtures {
            let full = first_fence(&parse_markdown(fixture)).expect("a fence");
            assert_eq!(full, ("code\nmore".to_owned(), true), "{fixture:?}");
            let still_open = first_fence(&parse_markdown(open)).expect("a fence");
            assert!(!still_open.1, "open at the end of {open:?}");
            let mut sealed: Option<(usize, String)> = None;
            for end in 0..=fixture.len() {
                let prefix = &fixture[..end];
                let Some((code, closed)) = first_fence(&parse_markdown(prefix)) else {
                    continue;
                };
                if let Some((at, sealed_code)) = &sealed {
                    assert!(closed, "{prefix:?} reopened the fence closed at {at}");
                    assert_eq!(&code, sealed_code, "{prefix:?} grew a final fence");
                } else if closed {
                    sealed = Some((end, code));
                }
            }
            assert!(sealed.is_some(), "{fixture:?} never closed its fence");
        }
    }

    #[test]
    fn stable_prefix_len_counts_equal_leading_blocks() {
        let a = parse_markdown("one\n\ntwo\n\nthr");
        let b = parse_markdown("one\n\ntwo\n\nthree");
        assert_eq!(stable_prefix_len(&a, &b), 2);
        assert_eq!(stable_prefix_len(&b, &b), 3);
        assert_eq!(stable_prefix_len(&[], &b), 0);
        assert_eq!(stable_prefix_len(&a, &parse_markdown("changed")), 0);
    }

    const HTML_CORPUS: &[&str] = &[
        "<div>hi</div>",
        "<script>alert('x')</script>",
        "text <b>bold</b> <i>it</i> tail",
        "<!-- comment --> after",
        "<iframe src=\"https://evil\"></iframe>",
        "<?php echo 1; ?>",
        "<![CDATA[ data ]]>",
        "<!DOCTYPE html>",
        "<a href=\"javascript:x\">click</a>",
        "<table><tr><td>cell</td></tr></table>",
        "<pre>\nkeep *this*\n\nand this\n</pre>",
        "<style>\nbody { display: none }\n</style>",
        "<textarea>\n# not a heading\n</textarea>",
        "<custom-element attr=\"v\">\n\ncontent\n",
    ];

    const IMAGE_CORPUS: &[(&str, &str)] = &[
        ("![cat](http://x/c.png)", "cat(http://x/c.png)"),
        ("![a **b** `c`](u)", "abc(u)"),
        ("![](https://track.er/p.gif)", "(https://track.er/p.gif)"),
        (
            "![alt][ref]\n\n[ref]: https://x/i.png",
            "alt(https://x/i.png)",
        ),
        (
            "![multi\nline](data:image/png;base64,AA)",
            "multiline(data:image/png;base64,AA)",
        ),
        ("- ![in list](file:///etc/x)", "inlist(file:///etc/x)"),
    ];

    #[test]
    fn html_and_images_lose_no_visible_characters() {
        for source in HTML_CORPUS {
            let quoted: Vec<String> = source.split('\n').map(|line| format!("> {line}")).collect();
            let listed: Vec<String> = source.split('\n').map(|line| format!("  {line}")).collect();
            let listed = format!("-{}", listed.join("\n").get(1..).unwrap_or_default());
            for variant in [source.to_string(), quoted.join("\n"), listed] {
                let blocks = parse_markdown(&variant);
                assert_eq!(
                    non_space(&visible_of(&variant)),
                    non_space(source),
                    "{variant:?}: {blocks:?}"
                );
            }
        }
        for (source, expected) in IMAGE_CORPUS {
            let blocks = parse_markdown(source);
            assert_eq!(
                non_space(&visible_of(source)),
                *expected,
                "{source:?}: {blocks:?}"
            );
            assert_eq!(
                max_inline_depth(&blocks),
                0,
                "an image never becomes a link: {blocks:?}"
            );
        }
    }

    #[test]
    fn large_hostile_document_respects_limits() {
        let mut source = String::new();
        while source.len() < 120 * 1024 {
            source.push_str(FIXTURE);
            source.push('\n');
        }
        source.push_str(&">".repeat(20_000));
        source.push_str(" bottom\n\n");
        source.push_str(&"*".repeat(10_000));
        source.push_str("core");
        source.push_str(&"*".repeat(10_000));
        source.push_str("\n\n");
        source.push_str(&"[".repeat(10_000));
        source.push_str("\n\n");
        for level in 0..200 {
            source.push_str(&format!("{}- deep{level}\n", "  ".repeat(level)));
        }
        source.push('\n');
        source.push_str(&format!(
            "{}|\n{}|\n",
            "| h ".repeat(500),
            "| - ".repeat(500)
        ));
        source.push_str(&"<div>".repeat(2_000));
        assert!(source.len() >= 200 * 1024, "{}", source.len());

        let blocks = parse_markdown(&source);
        assert!(nesting_depth(&blocks) <= MAX_NESTING_DEPTH);
        assert!(max_inline_depth(&blocks) <= MAX_INLINE_DEPTH);
        assert!(max_columns(&blocks) <= MAX_TABLE_COLUMNS);
        let shown = visible_of(&source);
        assert!(shown.contains("bottom") && shown.contains("core") && shown.contains("deep199"));
    }
}
