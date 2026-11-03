//! Markdown documents as a flat list of blocks, ready to draw one at a time.
//!
//! Containers are flattened: a block carries its quote and list depth, and the first block of a list item carries the
//! marker. A table is a run of row blocks. Every piece of selectable text has a number that follows reading order, so
//! a selection is two positions that compare directly.

use std::ops::Range;
use std::sync::Arc;

use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

/// Inline styles of a run of text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MdStyle {
    pub bold: bool,
    pub italic: bool,
    pub strikethrough: bool,
    pub code: bool,
    /// Source shown as it is: inline HTML, footnote references, and the alt text of images inside a sentence.
    pub muted: bool,
}

/// A styled run within a text. Bytes outside every span are plain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MdSpan {
    pub range: Range<usize>,
    pub style: MdStyle,
    pub link: Option<Arc<str>>,
}

/// A selectable piece of text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MdText {
    /// Position in reading order among all texts of the document.
    pub id: u32,
    pub text: Arc<str>,
    pub spans: Vec<MdSpan>,
}

impl MdText {
    /// The link at a byte of the text.
    pub fn link_at(&self, index: usize) -> Option<(&Arc<str>, &Range<usize>)> {
        self.spans.iter().find_map(|span| {
            span.range
                .contains(&index)
                .then_some(span.link.as_ref().map(|link| (link, &span.range)))
                .flatten()
        })
    }
}

/// How a list item or footnote is marked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MdMarker {
    Bullet,
    Number(u64),
    Task { checked: bool },
    Footnote(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MdAlign {
    Left,
    Center,
    Right,
}

/// What kind of source a [`MdBlockKind::Source`] block shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MdSourceKind {
    Html,
    /// The metadata block at the top of a file.
    Metadata,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MdBlockKind {
    Heading {
        /// 1 to 6.
        level: u8,
        text: MdText,
    },
    Paragraph(MdText),
    Code {
        language: Option<String>,
        text: MdText,
    },
    Source {
        kind: MdSourceKind,
        text: MdText,
    },
    /// One row of a table. Rows of a table share their column layout.
    TableRow {
        cells: Vec<MdText>,
        head: bool,
        last: bool,
        alignments: Arc<[MdAlign]>,
        /// Share of the table's width per column. Sums to 1.
        widths: Arc<[f32]>,
    },
    /// An image standing alone in its paragraph.
    Image {
        url: String,
        alt: String,
    },
    Rule,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MdBlock {
    pub kind: MdBlockKind,
    pub quote_depth: u16,
    pub list_depth: u16,
    /// Marker of the list item or footnote this block starts.
    pub marker: Option<MdMarker>,
}

impl MdBlock {
    /// The block's selectable texts, in reading order.
    pub fn texts(&self) -> &[MdText] {
        match &self.kind {
            MdBlockKind::Heading { text, .. }
            | MdBlockKind::Code { text, .. }
            | MdBlockKind::Source { text, .. } => std::slice::from_ref(text),
            MdBlockKind::Paragraph(text) => std::slice::from_ref(text),
            MdBlockKind::TableRow { cells, .. } => cells,
            MdBlockKind::Image { .. } | MdBlockKind::Rule => &[],
        }
    }
}

/// A position in the document's text: the text's number and a byte within it.
pub type MdPosition = (u32, usize);

/// A parsed Markdown file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MarkdownDocument {
    pub blocks: Vec<MdBlock>,
    text_count: u32,
}

impl MarkdownDocument {
    /// Parses CommonMark with tables, task lists, strikethrough, footnotes, math and a metadata block at the top.
    /// Math stays in its source form. HTML is shown as source, never interpreted.
    pub fn parse(source: &str) -> Self {
        let normalized;
        let source = if source.contains('\r') {
            normalized = source.replace("\r\n", "\n").replace('\r', "\n");
            normalized.as_str()
        } else {
            source
        };
        let options = Options::ENABLE_TABLES
            | Options::ENABLE_FOOTNOTES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_MATH
            | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS;
        let mut builder = Builder::default();
        for event in Parser::new_ext(source, options) {
            builder.event(event);
        }
        builder.flush_inline();
        Self {
            blocks: builder.blocks,
            text_count: builder.next_text,
        }
    }

    /// First and last positions of the document's text, or `None` when it has none.
    pub fn full_range(&self) -> Option<(MdPosition, MdPosition)> {
        let mut texts = self.blocks.iter().flat_map(|block| block.texts());
        let first = texts.next()?;
        let last = texts.last().unwrap_or(first);
        Some(((first.id, 0), (last.id, last.text.len())))
    }

    /// The text between two positions, in reading order. Blocks are separated by a blank line, list items and table
    /// rows by a line break and table cells by a tab.
    pub fn text_between(&self, start: MdPosition, end: MdPosition) -> String {
        let mut out = String::new();
        let mut last: Option<(usize, usize, bool)> = None;
        for (block_ix, block) in self.blocks.iter().enumerate() {
            for (text_ix, text) in block.texts().iter().enumerate() {
                if text.id < start.0 || text.id > end.0 {
                    continue;
                }
                let from = if text.id == start.0 { start.1 } else { 0 };
                let to = if text.id == end.0 {
                    end.1
                } else {
                    text.text.len()
                };
                if let Some((last_block, _, last_tight)) = last {
                    out.push_str(if last_block == block_ix {
                        "\t"
                    } else if last_tight && is_tight(block) {
                        "\n"
                    } else {
                        "\n\n"
                    });
                }
                out.push_str(&text.text[from.min(to)..to]);
                last = Some((block_ix, text_ix, is_tight(block)));
            }
        }
        out
    }

    /// Number of selectable texts.
    pub fn text_count(&self) -> u32 {
        self.text_count
    }
}

/// List items and table rows follow each other without a blank line when copied.
fn is_tight(block: &MdBlock) -> bool {
    block.list_depth > 0 || matches!(block.kind, MdBlockKind::TableRow { .. })
}

#[derive(Default)]
struct Inline {
    text: String,
    spans: Vec<MdSpan>,
    bold: u32,
    italic: u32,
    strikethrough: u32,
    links: Vec<Arc<str>>,
    /// Images of this text, finished ones with their alt text's range.
    images: Vec<(String, Range<usize>)>,
    /// Open images, innermost last: destination and where the alt text starts.
    open_images: Vec<(String, usize)>,
}

impl Inline {
    fn is_empty(&self) -> bool {
        self.text.is_empty() && self.images.is_empty()
    }

    fn push(&mut self, text: &str, code: bool, muted: bool) {
        if text.is_empty() {
            return;
        }
        let start = self.text.len();
        self.text.push_str(text);
        let style = MdStyle {
            bold: self.bold > 0,
            italic: self.italic > 0,
            strikethrough: self.strikethrough > 0,
            code,
            muted: muted || !self.open_images.is_empty(),
        };
        let link = self.links.last().cloned();
        if style == MdStyle::default() && link.is_none() {
            return;
        }
        if let Some(last) = self.spans.last_mut()
            && last.range.end == start
            && last.style == style
            && last.link == link
        {
            last.range.end = self.text.len();
            return;
        }
        self.spans.push(MdSpan {
            range: start..self.text.len(),
            style,
            link,
        });
    }
}

#[derive(Default)]
struct TableBuilder {
    alignments: Vec<MdAlign>,
    /// Rows so far; the first is the head.
    rows: Vec<Vec<MdText>>,
    row: Vec<MdText>,
}

/// Text collected for a block whose content is not inline Markdown.
enum Verbatim {
    Code(Option<String>),
    Source(MdSourceKind),
}

#[derive(Default)]
struct Builder {
    blocks: Vec<MdBlock>,
    next_text: u32,
    quote_depth: u16,
    /// Open lists and footnotes, with the next number of ordered lists.
    lists: Vec<Option<u64>>,
    marker: Option<MdMarker>,
    inline: Inline,
    heading: Option<u8>,
    table: Option<TableBuilder>,
    verbatim: Option<(Verbatim, String)>,
}

impl Builder {
    fn text(&mut self, text: String, spans: Vec<MdSpan>) -> MdText {
        let id = self.next_text;
        self.next_text += 1;
        MdText {
            id,
            text: text.into(),
            spans,
        }
    }

    fn push_block(&mut self, kind: MdBlockKind) {
        self.blocks.push(MdBlock {
            kind,
            quote_depth: self.quote_depth,
            list_depth: self.lists.len() as u16,
            marker: self.marker.take(),
        });
    }

    /// Ends the running paragraph. Tight list items hold inline content without a paragraph around it.
    fn flush_inline(&mut self) {
        if self.inline.is_empty() {
            return;
        }
        let inline = std::mem::take(&mut self.inline);
        // A paragraph of nothing but images shows the images themselves.
        let mut rest = inline.text.as_str().to_string();
        for (_, range) in inline.images.iter().rev() {
            rest.replace_range(range.clone(), "");
        }
        if !inline.images.is_empty() && rest.trim().is_empty() {
            for (url, range) in &inline.images {
                let alt = inline.text[range.clone()].to_string();
                self.push_block(MdBlockKind::Image {
                    url: url.clone(),
                    alt,
                });
            }
            return;
        }
        let text = self.text(inline.text, inline.spans);
        self.push_block(MdBlockKind::Paragraph(text));
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => self.flush_inline(),
            Tag::Heading { level, .. } => {
                self.flush_inline();
                self.heading = Some(level as u8);
            }
            Tag::BlockQuote(_) => {
                self.flush_inline();
                self.quote_depth += 1;
            }
            Tag::CodeBlock(kind) => {
                self.flush_inline();
                let language = match kind {
                    CodeBlockKind::Fenced(info) => info
                        .split_whitespace()
                        .next()
                        .map(str::to_string)
                        .filter(|language| !language.is_empty()),
                    CodeBlockKind::Indented => None,
                };
                self.verbatim = Some((Verbatim::Code(language), String::new()));
            }
            Tag::HtmlBlock => {
                self.flush_inline();
                self.verbatim = Some((Verbatim::Source(MdSourceKind::Html), String::new()));
            }
            Tag::MetadataBlock(_) => {
                self.flush_inline();
                self.verbatim = Some((Verbatim::Source(MdSourceKind::Metadata), String::new()));
            }
            Tag::List(first) => {
                self.flush_inline();
                self.lists.push(first);
            }
            Tag::Item => {
                self.flush_inline();
                self.marker = Some(match self.lists.last_mut() {
                    Some(Some(number)) => {
                        let marker = MdMarker::Number(*number);
                        *number += 1;
                        marker
                    }
                    _ => MdMarker::Bullet,
                });
            }
            Tag::FootnoteDefinition(label) => {
                self.flush_inline();
                self.lists.push(None);
                self.marker = Some(MdMarker::Footnote(label.to_string()));
            }
            Tag::Table(alignments) => {
                self.flush_inline();
                self.table = Some(TableBuilder {
                    alignments: alignments
                        .iter()
                        .map(|alignment| match alignment {
                            Alignment::Center => MdAlign::Center,
                            Alignment::Right => MdAlign::Right,
                            Alignment::None | Alignment::Left => MdAlign::Left,
                        })
                        .collect(),
                    ..TableBuilder::default()
                });
            }
            Tag::Emphasis => self.inline.italic += 1,
            Tag::Strong => self.inline.bold += 1,
            Tag::Strikethrough => self.inline.strikethrough += 1,
            Tag::Link { dest_url, .. } => self.inline.links.push(dest_url.as_ref().into()),
            Tag::Image { dest_url, .. } => {
                let start = self.inline.text.len();
                self.inline.open_images.push((dest_url.to_string(), start));
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => self.flush_inline(),
            TagEnd::Heading(_) => {
                let inline = std::mem::take(&mut self.inline);
                let level = self.heading.take().unwrap_or(1);
                let text = self.text(inline.text, inline.spans);
                self.push_block(MdBlockKind::Heading { level, text });
            }
            TagEnd::BlockQuote(_) => {
                self.flush_inline();
                self.quote_depth = self.quote_depth.saturating_sub(1);
            }
            TagEnd::CodeBlock | TagEnd::HtmlBlock | TagEnd::MetadataBlock(_) => {
                if let Some((kind, mut text)) = self.verbatim.take() {
                    if text.ends_with('\n') {
                        text.pop();
                    }
                    let text = self.text(text, Vec::new());
                    self.push_block(match kind {
                        Verbatim::Code(language) => MdBlockKind::Code { language, text },
                        Verbatim::Source(kind) => MdBlockKind::Source { kind, text },
                    });
                }
            }
            TagEnd::List(_) => {
                self.flush_inline();
                self.lists.pop();
            }
            TagEnd::Item | TagEnd::FootnoteDefinition => {
                self.flush_inline();
                // An empty item still shows its marker.
                if self.marker.is_some() {
                    let text = self.text(String::new(), Vec::new());
                    self.push_block(MdBlockKind::Paragraph(text));
                }
                if tag == TagEnd::FootnoteDefinition {
                    self.lists.pop();
                }
            }
            TagEnd::TableCell => {
                let inline = std::mem::take(&mut self.inline);
                let text = self.text(inline.text, inline.spans);
                if let Some(table) = &mut self.table {
                    table.row.push(text);
                }
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                if let Some(table) = &mut self.table {
                    let row = std::mem::take(&mut table.row);
                    table.rows.push(row);
                }
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    self.push_table(table);
                }
            }
            TagEnd::Emphasis => self.inline.italic = self.inline.italic.saturating_sub(1),
            TagEnd::Strong => self.inline.bold = self.inline.bold.saturating_sub(1),
            TagEnd::Strikethrough => {
                self.inline.strikethrough = self.inline.strikethrough.saturating_sub(1);
            }
            TagEnd::Link => {
                self.inline.links.pop();
            }
            TagEnd::Image => {
                if let Some((url, start)) = self.inline.open_images.pop() {
                    let end = self.inline.text.len();
                    self.inline.images.push((url, start..end));
                }
            }
            _ => {}
        }
    }

    fn push_table(&mut self, table: TableBuilder) {
        let alignments: Arc<[MdAlign]> = table.alignments.into();
        let widths: Arc<[f32]> = column_widths(&table.rows, alignments.len()).into();
        let count = table.rows.len();
        for (ix, cells) in table.rows.into_iter().enumerate() {
            self.push_block(MdBlockKind::TableRow {
                cells,
                head: ix == 0,
                last: ix + 1 == count,
                alignments: alignments.clone(),
                widths: widths.clone(),
            });
        }
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => match &mut self.verbatim {
                Some((_, buffer)) => buffer.push_str(&text),
                None => self.inline.push(&text, false, false),
            },
            Event::Code(text) => self.inline.push(&text, true, false),
            Event::InlineMath(text) => self.inline.push(&format!("${text}$"), true, false),
            Event::DisplayMath(text) => self.inline.push(&format!("$${text}$$"), true, false),
            Event::Html(text) => match &mut self.verbatim {
                Some((_, buffer)) => buffer.push_str(&text),
                None => self.inline.push(&text, false, true),
            },
            Event::InlineHtml(text) => self.inline.push(&text, false, true),
            Event::FootnoteReference(label) => {
                self.inline.push(&format!("[{label}]"), false, true);
            }
            Event::SoftBreak => self.inline.push(" ", false, false),
            Event::HardBreak => self.inline.push("\n", false, false),
            Event::Rule => {
                self.flush_inline();
                self.push_block(MdBlockKind::Rule);
            }
            Event::TaskListMarker(checked) => self.marker = Some(MdMarker::Task { checked }),
        }
    }
}

/// Width of a text in cells, with wide characters as two.
fn text_cells(text: &str) -> f32 {
    text.chars()
        .map(|ch| if (ch as u32) < 0x1100 { 1. } else { 2. })
        .sum()
}

/// Splits a table's width by the longest text of each column, so a narrow column of short values stays narrow and a
/// column of sentences gets the room. Long sentences wrap, so they count only up to a limit.
fn column_widths(rows: &[Vec<MdText>], columns: usize) -> Vec<f32> {
    const MIN_CELLS: f32 = 6.;
    const MAX_CELLS: f32 = 60.;
    let mut cells = vec![MIN_CELLS; columns];
    for row in rows {
        for (column, text) in row.iter().enumerate().take(columns) {
            cells[column] = cells[column].max(text_cells(&text.text).min(MAX_CELLS));
        }
    }
    let total: f32 = cells.iter().sum();
    cells.iter().map(|cells| cells / total).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str) -> Vec<MdBlock> {
        MarkdownDocument::parse(source).blocks
    }

    fn plain(block: &MdBlock) -> &str {
        &block.texts()[0].text
    }

    #[test]
    fn styles_inline_runs() {
        let blocks = parse("a **b *c*** `d` ~~e~~ [f](http://x)");
        let MdBlockKind::Paragraph(text) = &blocks[0].kind else {
            panic!("expected a paragraph");
        };
        assert_eq!(&*text.text, "a b c d e f");
        let styled = |span: &MdSpan| text.text[span.range.clone()].to_string();
        assert_eq!(styled(&text.spans[0]), "b ");
        assert!(text.spans[0].style.bold && !text.spans[0].style.italic);
        assert_eq!(styled(&text.spans[1]), "c");
        assert!(text.spans[1].style.bold && text.spans[1].style.italic);
        assert_eq!(styled(&text.spans[2]), "d");
        assert!(text.spans[2].style.code);
        assert_eq!(styled(&text.spans[3]), "e");
        assert!(text.spans[3].style.strikethrough);
        assert_eq!(styled(&text.spans[4]), "f");
        assert_eq!(text.spans[4].link.as_deref(), Some("http://x"));
        assert_eq!(
            text.link_at(text.text.len() - 1).map(|(link, _)| &**link),
            Some("http://x")
        );
        assert!(text.link_at(0).is_none());
    }

    #[test]
    fn soft_breaks_are_spaces_and_hard_breaks_are_newlines() {
        let blocks = parse("한 줄\n다음 줄  \n끝");
        assert_eq!(plain(&blocks[0]), "한 줄 다음 줄\n끝");
    }

    #[test]
    fn flattens_lists_and_quotes() {
        let blocks = parse("> 인용\n>\n> - 하나\n>   - 둘\n>\n>     이어짐\n\n3. 셋\n4. [x] 넷\n");
        let summary: Vec<_> = blocks
            .iter()
            .map(|block| {
                (
                    block.quote_depth,
                    block.list_depth,
                    block.marker.clone(),
                    plain(block).to_string(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                (1, 0, None, "인용".to_string()),
                (1, 1, Some(MdMarker::Bullet), "하나".to_string()),
                (1, 2, Some(MdMarker::Bullet), "둘".to_string()),
                (1, 2, None, "이어짐".to_string()),
                (0, 1, Some(MdMarker::Number(3)), "셋".to_string()),
                (
                    0,
                    1,
                    Some(MdMarker::Task { checked: true }),
                    "넷".to_string()
                ),
            ]
        );
    }

    #[test]
    fn keeps_code_as_written() {
        let blocks = parse("```rust title\nfn main() {\n\tlet x = 1;\n\n}\n```\n\n    indented\n");
        let MdBlockKind::Code { language, text } = &blocks[0].kind else {
            panic!("expected code");
        };
        assert_eq!(language.as_deref(), Some("rust"));
        assert_eq!(&*text.text, "fn main() {\n\tlet x = 1;\n\n}");
        let MdBlockKind::Code { language, text } = &blocks[1].kind else {
            panic!("expected code");
        };
        assert_eq!(*language, None);
        assert_eq!(&*text.text, "indented");
    }

    #[test]
    fn splits_tables_into_rows_with_a_shared_layout() {
        let blocks = parse("| 이름 | 설명 |\n|:--|--:|\n| a | 긴 설명입니다 |\n| b | **c** |\n");
        assert_eq!(blocks.len(), 3);
        let MdBlockKind::TableRow {
            cells,
            head,
            last,
            alignments,
            widths,
        } = &blocks[0].kind
        else {
            panic!("expected a table row");
        };
        assert!(*head && !*last);
        assert_eq!(&*cells[1].text, "설명");
        assert_eq!(&**alignments, [MdAlign::Left, MdAlign::Right]);
        assert!(widths[1] > widths[0]);
        assert!((widths.iter().sum::<f32>() - 1.).abs() < 1e-5);
        let MdBlockKind::TableRow { cells, last, .. } = &blocks[2].kind else {
            panic!("expected a table row");
        };
        assert!(*last);
        assert!(cells[1].spans[0].style.bold);
    }

    #[test]
    fn images_alone_become_blocks_and_others_stay_in_the_sentence() {
        let blocks = parse("![그림](a.png)\n![다음](b.png)\n\n앞 ![그림](a.png) 뒤\n");
        assert_eq!(
            blocks[0].kind,
            MdBlockKind::Image {
                url: "a.png".into(),
                alt: "그림".into()
            }
        );
        assert_eq!(
            blocks[1].kind,
            MdBlockKind::Image {
                url: "b.png".into(),
                alt: "다음".into()
            }
        );
        let MdBlockKind::Paragraph(text) = &blocks[2].kind else {
            panic!("expected a paragraph");
        };
        assert_eq!(&*text.text, "앞 그림 뒤");
        assert!(text.spans[0].style.muted);
    }

    #[test]
    fn shows_html_metadata_math_and_footnotes_as_source() {
        let blocks = parse(
            "---\ntitle: 제목\n---\n\n<div>\n<b>굵게</b>\n</div>\n\n값 $x_1$ 과 <i>기울임</i>[^n]\n\n[^n]: 주석\n",
        );
        assert_eq!(
            blocks[0].kind,
            MdBlockKind::Source {
                kind: MdSourceKind::Metadata,
                text: MdText {
                    id: 0,
                    text: "title: 제목".into(),
                    spans: Vec::new()
                }
            }
        );
        let MdBlockKind::Source { kind, text } = &blocks[1].kind else {
            panic!("expected HTML source");
        };
        assert_eq!(*kind, MdSourceKind::Html);
        assert!(text.text.contains("<b>굵게</b>"));
        assert_eq!(plain(&blocks[2]), "값 $x_1$ 과 <i>기울임</i>[n]");
        assert_eq!(blocks[3].marker, Some(MdMarker::Footnote("n".into())));
        assert_eq!(plain(&blocks[3]), "주석");
    }

    #[test]
    fn keeps_double_brackets_as_text() {
        assert_eq!(plain(&parse("[[다른 문서]]")[0]), "[[다른 문서]]");
    }

    #[test]
    fn numbers_texts_in_reading_order() {
        let doc = MarkdownDocument::parse(
            "# 제목\n\n본문\n\n| a | b |\n|---|---|\n| c | d |\n\n```\ne\n```\n",
        );
        let ids: Vec<u32> = doc
            .blocks
            .iter()
            .flat_map(|block| block.texts())
            .map(|text| text.id)
            .collect();
        assert_eq!(ids, [0, 1, 2, 3, 4, 5, 6]);
        assert_eq!(doc.text_count(), 7);
    }

    #[test]
    fn copies_text_between_positions() {
        let doc = MarkdownDocument::parse(
            "첫 문단\n\n- 하나\n- 둘\n\n| a | b |\n|---|---|\n| c | d |\n\n끝\n",
        );
        let (start, end) = doc.full_range().unwrap();
        assert_eq!(
            doc.text_between(start, end),
            "첫 문단\n\n하나\n둘\na\tb\nc\td\n\n끝"
        );
        // A selection inside one text, and one from the middle of a text to the middle of another.
        assert_eq!(doc.text_between((0, 4), (0, 7)), "문");
        assert_eq!(doc.text_between((1, 3), (2, 3)), "나\n둘");
    }

    #[test]
    fn reads_empty_documents_and_line_endings() {
        assert!(parse("").is_empty());
        assert_eq!(MarkdownDocument::parse("").full_range(), None);
        let blocks = parse("a\r\nb\r\n\r\n```\r\nc\r\nd\r\n```\r\n");
        assert_eq!(plain(&blocks[0]), "a b");
        assert_eq!(plain(&blocks[1]), "c\nd");
    }
}
