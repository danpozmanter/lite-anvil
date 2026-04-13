//! Markdown parser producing a block tree for the preview renderer.
//!
//! Thin wrapper over `pulldown-cmark` that folds the event stream into a
//! flat block list whose inline spans carry only the style attributes the
//! renderer consumes. Task-list items are tagged with their checked state
//! and the source byte offset so the preview can toggle the `[ ]` / `[x]`
//! marker when the user clicks them.

use pulldown_cmark::{
    Alignment as CmAlignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd,
};

/// A single styled run of text within a block.
#[derive(Debug, Clone)]
pub struct Span {
    pub text: String,
    pub code: bool,
    pub bold: bool,
    pub italic: bool,
    pub strikethrough: bool,
    pub href: Option<String>,
}

impl Span {
    fn from_text(text: impl Into<String>, style: &InlineStyle) -> Self {
        Self {
            text: text.into(),
            code: false,
            bold: style.bold,
            italic: style.italic,
            strikethrough: style.strikethrough,
            href: style.href.clone(),
        }
    }

    fn inline_code(text: impl Into<String>, style: &InlineStyle) -> Self {
        Self {
            text: text.into(),
            code: true,
            bold: false,
            italic: false,
            strikethrough: false,
            href: style.href.clone(),
        }
    }
}

/// One list item. `task` is `Some(checked)` for `[ ]` / `[x]` items.
#[derive(Debug, Clone, Default)]
pub struct ListItem {
    pub spans: Vec<Span>,
    pub task: Option<bool>,
    /// Byte offset of the list-item start in the source markdown. Used by
    /// the preview to locate the checkbox on click so it can be toggled.
    pub source_start: Option<usize>,
}

/// Column alignment in a table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alignment {
    None,
    Left,
    Center,
    Right,
}

impl From<CmAlignment> for Alignment {
    fn from(a: CmAlignment) -> Self {
        match a {
            CmAlignment::None => Alignment::None,
            CmAlignment::Left => Alignment::Left,
            CmAlignment::Center => Alignment::Center,
            CmAlignment::Right => Alignment::Right,
        }
    }
}

/// A document block.
#[derive(Debug, Clone)]
pub enum Block {
    Heading {
        level: u8,
        inlines: Vec<Span>,
    },
    Paragraph {
        inlines: Vec<Span>,
    },
    Code {
        #[allow(dead_code)]
        lang: Option<String>,
        text: String,
    },
    Rule,
    Quote {
        blocks: Vec<Block>,
    },
    List {
        ordered: bool,
        start: u64,
        items: Vec<ListItem>,
    },
    Table {
        #[allow(dead_code)]
        alignments: Vec<Alignment>,
        head: Vec<Vec<Span>>,
        rows: Vec<Vec<Vec<Span>>>,
    },
}

#[derive(Clone, Default)]
struct InlineStyle {
    bold: bool,
    italic: bool,
    strikethrough: bool,
    href: Option<String>,
}

enum Frame {
    Root {
        blocks: Vec<Block>,
    },
    Heading {
        level: u8,
        spans: Vec<Span>,
    },
    Paragraph {
        spans: Vec<Span>,
    },
    CodeBlock {
        lang: Option<String>,
        text: String,
    },
    Quote {
        blocks: Vec<Block>,
    },
    List {
        ordered: bool,
        start: u64,
        items: Vec<ListItem>,
    },
    Item {
        spans: Vec<Span>,
        task: Option<bool>,
        source_start: Option<usize>,
    },
}

struct TableState {
    alignments: Vec<Alignment>,
    in_head: bool,
    head: Vec<Vec<Span>>,
    rows: Vec<Vec<Vec<Span>>>,
    current_row: Vec<Vec<Span>>,
    current_cell: Vec<Span>,
}

/// Parse `text` as CommonMark-with-extensions and return the block tree.
pub fn parse(text: &str) -> Vec<Block> {
    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS;
    let parser = Parser::new_ext(text, opts).into_offset_iter();

    let mut stack: Vec<Frame> = vec![Frame::Root { blocks: vec![] }];
    let mut style_stack: Vec<InlineStyle> = vec![InlineStyle::default()];
    let mut table: Option<TableState> = None;

    for (event, range) in parser {
        match event {
            Event::Start(tag) => {
                handle_start(tag, range.start, &mut stack, &mut style_stack, &mut table)
            }
            Event::End(tag) => handle_end(tag, &mut stack, &mut style_stack, &mut table),
            Event::Text(text) => {
                let style = style_stack.last().unwrap();
                push_span(&mut stack, &mut table, Span::from_text(text.as_ref(), style));
            }
            Event::Code(text) => {
                let style = style_stack.last().unwrap();
                push_span(&mut stack, &mut table, Span::inline_code(text.as_ref(), style));
            }
            Event::SoftBreak => {
                let style = style_stack.last().unwrap();
                push_span(&mut stack, &mut table, Span::from_text(" ", style));
            }
            Event::HardBreak => {
                push_span(
                    &mut stack,
                    &mut table,
                    Span::from_text("\n", &InlineStyle::default()),
                );
            }
            Event::Rule => push_block(&mut stack, Block::Rule),
            Event::TaskListMarker(checked) => {
                for frame in stack.iter_mut().rev() {
                    if let Frame::Item { task, .. } = frame {
                        *task = Some(checked);
                        break;
                    }
                }
            }
            _ => {}
        }
    }

    match stack.pop() {
        Some(Frame::Root { blocks }) => blocks,
        _ => vec![],
    }
}

fn handle_start(
    tag: Tag,
    start_offset: usize,
    stack: &mut Vec<Frame>,
    style_stack: &mut Vec<InlineStyle>,
    table: &mut Option<TableState>,
) {
    match tag {
        Tag::Heading { level, .. } => {
            stack.push(Frame::Heading {
                level: level_u8(level),
                spans: vec![],
            });
        }
        Tag::Paragraph => {
            if !in_item(stack) {
                stack.push(Frame::Paragraph { spans: vec![] });
            }
        }
        Tag::CodeBlock(kind) => {
            let lang = match kind {
                CodeBlockKind::Fenced(info) => {
                    let s = info.split_whitespace().next().unwrap_or("").to_string();
                    if s.is_empty() { None } else { Some(s) }
                }
                CodeBlockKind::Indented => None,
            };
            stack.push(Frame::CodeBlock {
                lang,
                text: String::new(),
            });
        }
        Tag::BlockQuote(_) => {
            stack.push(Frame::Quote { blocks: vec![] });
        }
        Tag::List(start) => {
            stack.push(Frame::List {
                ordered: start.is_some(),
                start: start.unwrap_or(1),
                items: vec![],
            });
        }
        Tag::Item => {
            stack.push(Frame::Item {
                spans: vec![],
                task: None,
                source_start: Some(start_offset),
            });
        }
        Tag::Table(alignments) => {
            *table = Some(TableState {
                alignments: alignments.iter().map(|a| Alignment::from(*a)).collect(),
                in_head: false,
                head: vec![],
                rows: vec![],
                current_row: vec![],
                current_cell: vec![],
            });
        }
        Tag::TableHead => {
            if let Some(t) = table.as_mut() {
                t.in_head = true;
            }
        }
        Tag::TableRow | Tag::TableCell => {}
        Tag::Emphasis => {
            let mut s = style_stack.last().cloned().unwrap_or_default();
            s.italic = true;
            style_stack.push(s);
        }
        Tag::Strong => {
            let mut s = style_stack.last().cloned().unwrap_or_default();
            s.bold = true;
            style_stack.push(s);
        }
        Tag::Strikethrough => {
            let mut s = style_stack.last().cloned().unwrap_or_default();
            s.strikethrough = true;
            style_stack.push(s);
        }
        Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => {
            let mut s = style_stack.last().cloned().unwrap_or_default();
            s.href = Some(dest_url.to_string());
            style_stack.push(s);
        }
        _ => {}
    }
}

fn handle_end(
    tag: TagEnd,
    stack: &mut Vec<Frame>,
    style_stack: &mut Vec<InlineStyle>,
    table: &mut Option<TableState>,
) {
    match tag {
        TagEnd::Heading(_) => {
            if let Some(Frame::Heading { level, spans }) = stack.pop() {
                push_block(
                    stack,
                    Block::Heading {
                        level,
                        inlines: spans,
                    },
                );
            }
        }
        TagEnd::Paragraph => {
            if !in_item(stack) {
                if let Some(Frame::Paragraph { spans }) = stack.pop() {
                    push_block(stack, Block::Paragraph { inlines: spans });
                }
            }
        }
        TagEnd::CodeBlock => {
            if let Some(Frame::CodeBlock { lang, text }) = stack.pop() {
                let text = text.strip_suffix('\n').unwrap_or(&text).to_string();
                push_block(stack, Block::Code { lang, text });
            }
        }
        TagEnd::BlockQuote(_) => {
            if let Some(Frame::Quote { blocks }) = stack.pop() {
                push_block(stack, Block::Quote { blocks });
            }
        }
        TagEnd::List(_) => {
            if let Some(Frame::List {
                ordered,
                start,
                items,
            }) = stack.pop()
            {
                push_block(
                    stack,
                    Block::List {
                        ordered,
                        start,
                        items,
                    },
                );
            }
        }
        TagEnd::Item => {
            if let Some(Frame::Item {
                spans,
                task,
                source_start,
            }) = stack.pop()
            {
                for frame in stack.iter_mut().rev() {
                    if let Frame::List { items, .. } = frame {
                        items.push(ListItem {
                            spans,
                            task,
                            source_start,
                        });
                        break;
                    }
                }
            }
        }
        TagEnd::TableCell => {
            if let Some(t) = table.as_mut() {
                t.current_row.push(std::mem::take(&mut t.current_cell));
            }
        }
        TagEnd::TableRow => {
            if let Some(t) = table.as_mut() {
                let row = std::mem::take(&mut t.current_row);
                if !row.is_empty() {
                    t.rows.push(row);
                }
            }
        }
        TagEnd::TableHead => {
            // pulldown-cmark does NOT wrap header cells in a TableRow, so
            // flush `current_row` into `head` here.
            if let Some(t) = table.as_mut() {
                let row = std::mem::take(&mut t.current_row);
                t.head = row;
                t.in_head = false;
            }
        }
        TagEnd::Table => {
            if let Some(ts) = table.take() {
                push_block(
                    stack,
                    Block::Table {
                        alignments: ts.alignments,
                        head: ts.head,
                        rows: ts.rows,
                    },
                );
            }
        }
        TagEnd::Emphasis
        | TagEnd::Strong
        | TagEnd::Strikethrough
        | TagEnd::Link
        | TagEnd::Image => {
            if style_stack.len() > 1 {
                style_stack.pop();
            }
        }
        _ => {}
    }
}

fn in_item(stack: &[Frame]) -> bool {
    for frame in stack.iter().rev() {
        match frame {
            Frame::Item { .. } => return true,
            Frame::List { .. } => return false,
            _ => {}
        }
    }
    false
}

fn push_span(stack: &mut [Frame], table: &mut Option<TableState>, span: Span) {
    if let Some(ts) = table.as_mut() {
        ts.current_cell.push(span);
        return;
    }
    for frame in stack.iter_mut().rev() {
        match frame {
            Frame::CodeBlock { text, .. } => {
                text.push_str(&span.text);
                return;
            }
            Frame::Heading { spans, .. } | Frame::Paragraph { spans } => {
                spans.push(span);
                return;
            }
            Frame::Item { spans, .. } => {
                spans.push(span);
                return;
            }
            _ => {}
        }
    }
}

fn push_block(stack: &mut [Frame], block: Block) {
    for frame in stack.iter_mut().rev() {
        match frame {
            Frame::Root { blocks } | Frame::Quote { blocks } => {
                blocks.push(block);
                return;
            }
            _ => {}
        }
    }
}

fn level_u8(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_heading_and_paragraph() {
        let blocks = parse("# Hello\n\nworld\n");
        assert_eq!(blocks.len(), 2);
        match &blocks[0] {
            Block::Heading { level, inlines } => {
                assert_eq!(*level, 1);
                assert_eq!(inlines[0].text, "Hello");
            }
            _ => panic!("expected heading"),
        }
        match &blocks[1] {
            Block::Paragraph { inlines } => {
                assert_eq!(inlines[0].text, "world");
            }
            _ => panic!("expected paragraph"),
        }
    }

    #[test]
    fn parses_link_href() {
        let blocks = parse("[click here](https://example.com)\n");
        match &blocks[0] {
            Block::Paragraph { inlines } => {
                assert_eq!(inlines[0].href.as_deref(), Some("https://example.com"));
            }
            _ => panic!("expected paragraph"),
        }
    }

    #[test]
    fn parses_inline_styles() {
        let blocks = parse("**bold** *italic* `code` ~~strike~~\n");
        match &blocks[0] {
            Block::Paragraph { inlines } => {
                assert!(inlines.iter().any(|s| s.bold));
                assert!(inlines.iter().any(|s| s.italic));
                assert!(inlines.iter().any(|s| s.code));
                assert!(inlines.iter().any(|s| s.strikethrough));
            }
            _ => panic!("expected paragraph"),
        }
    }

    #[test]
    fn parses_task_list_checked_unchecked() {
        let src = "- [ ] first\n- [x] second\n";
        let blocks = parse(src);
        match &blocks[0] {
            Block::List { items, .. } => {
                assert_eq!(items.len(), 2);
                assert_eq!(items[0].task, Some(false));
                assert_eq!(items[1].task, Some(true));
                assert!(items[0].source_start.unwrap() < src.len());
                assert!(items[1].source_start.unwrap() < src.len());
            }
            _ => panic!("expected list"),
        }
    }

    #[test]
    fn parses_table_with_header() {
        let src = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n";
        let blocks = parse(src);
        match &blocks[0] {
            Block::Table { head, rows, .. } => {
                assert_eq!(head.len(), 2);
                assert_eq!(head[0][0].text, "a");
                assert_eq!(rows.len(), 2);
                assert_eq!(rows[0].len(), 2);
                assert_eq!(rows[1][1][0].text, "4");
            }
            _ => panic!("expected table"),
        }
    }

    #[test]
    fn parses_fenced_code_block() {
        let src = "```rust\nfn main() {}\n```\n";
        let blocks = parse(src);
        match &blocks[0] {
            Block::Code { lang, text } => {
                assert_eq!(lang.as_deref(), Some("rust"));
                assert_eq!(text, "fn main() {}");
            }
            _ => panic!("expected code block"),
        }
    }

    #[test]
    fn parses_blockquote() {
        let blocks = parse("> quoted\n");
        match &blocks[0] {
            Block::Quote { blocks } => {
                assert_eq!(blocks.len(), 1);
            }
            _ => panic!("expected blockquote"),
        }
    }

    #[test]
    fn parses_rule() {
        let blocks = parse("---\n");
        assert!(matches!(blocks[0], Block::Rule));
    }
}
