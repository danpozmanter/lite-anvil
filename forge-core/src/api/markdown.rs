use mlua::prelude::*;
use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

// ── Public entry point ────────────────────────────────────────────────────────

/// Build the `markdown` Lua table exposing `markdown.parse(text) -> blocks`.
pub fn make_module(lua: &Lua) -> LuaResult<LuaTable> {
    let t = lua.create_table()?;
    t.set(
        "parse",
        lua.create_function(|lua, text: String| parse_markdown(lua, &text))?,
    )?;
    Ok(t)
}

// ── Intermediate representation ───────────────────────────────────────────────

/// Accumulated style for inline spans from nested Start/End tags.
#[derive(Clone, Default)]
struct Style {
    bold: bool,
    italic: bool,
    strikethrough: bool,
    href: Option<String>,
}

/// A single styled run of text within a block.
struct Span {
    text: String,
    code: bool,
    bold: bool,
    italic: bool,
    strikethrough: bool,
    href: Option<String>,
}

impl Span {
    fn from_text(text: impl Into<String>, style: &Style) -> Self {
        Self {
            text: text.into(),
            code: false,
            bold: style.bold,
            italic: style.italic,
            strikethrough: style.strikethrough,
            href: style.href.clone(),
        }
    }

    fn inline_code(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            code: true,
            bold: false,
            italic: false,
            strikethrough: false,
            href: None,
        }
    }
}

/// A document block.
enum Block {
    Heading {
        level: u8,
        inlines: Vec<Span>,
    },
    Paragraph {
        inlines: Vec<Span>,
    },
    Code {
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
        items: Vec<Vec<Span>>,
    },
    /// head: header-row cells; rows: body rows → cells → spans.
    Table {
        alignments: Vec<String>,
        head: Vec<Vec<Span>>,
        rows: Vec<Vec<Vec<Span>>>,
    },
}

// ── Parser state ──────────────────────────────────────────────────────────────

/// A frame on the block-building stack.
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
        items: Vec<Vec<Span>>,
    },
    /// Simplified: collect only inline spans from the item (first-paragraph flatten).
    Item {
        spans: Vec<Span>,
    },
}

/// State while parsing a table (separate because tables have 3-level nesting).
struct TableState {
    alignments: Vec<String>,
    in_head: bool,
    head: Vec<Vec<Span>>,
    rows: Vec<Vec<Vec<Span>>>,
    current_row: Vec<Vec<Span>>,
    current_cell: Vec<Span>,
}

// ── Parsing ───────────────────────────────────────────────────────────────────

fn parse_markdown(lua: &Lua, text: &str) -> LuaResult<LuaTable> {
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    let parser = Parser::new_ext(text, opts);

    let mut stack: Vec<Frame> = vec![Frame::Root { blocks: vec![] }];
    let mut style_stack: Vec<Style> = vec![Style::default()];
    let mut table: Option<TableState> = None;

    for event in parser {
        match event {
            Event::Start(tag) => handle_start(tag, &mut stack, &mut style_stack, &mut table),
            Event::End(tag) => handle_end(tag, &mut stack, &mut style_stack, &mut table),

            Event::Text(text) => {
                let style = style_stack.last().unwrap();
                let span = Span::from_text(text.as_ref(), style);
                push_span(&mut stack, &mut table, span);
            }
            Event::Code(text) => {
                let span = Span::inline_code(text.as_ref());
                push_span(&mut stack, &mut table, span);
            }
            Event::SoftBreak => {
                let style = style_stack.last().unwrap();
                push_span(&mut stack, &mut table, Span::from_text(" ", style));
            }
            Event::HardBreak => {
                push_span(
                    &mut stack,
                    &mut table,
                    Span::from_text("\n", &Style::default()),
                );
            }
            Event::Rule => push_block(&mut stack, Block::Rule),
            _ => {}
        }
    }

    let blocks = match stack.pop() {
        Some(Frame::Root { blocks }) => blocks,
        _ => vec![],
    };
    blocks_to_lua(lua, blocks)
}

fn handle_start(
    tag: Tag,
    stack: &mut Vec<Frame>,
    style_stack: &mut Vec<Style>,
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
            // Inside a list item we collect spans directly; no separate Paragraph frame needed.
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
            stack.push(Frame::Item { spans: vec![] });
        }
        Tag::Table(alignments) => {
            *table = Some(TableState {
                alignments: alignments
                    .iter()
                    .map(|a| alignment_str(*a).to_string())
                    .collect(),
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
        Tag::TableRow | Tag::TableCell => {} // cell/row reset happens in handle_end
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
    style_stack: &mut Vec<Style>,
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
            // Inside an item: paragraph end is a no-op; spans already in Item frame.
        }
        TagEnd::CodeBlock => {
            if let Some(Frame::CodeBlock { lang, text }) = stack.pop() {
                // Strip final newline that pulldown-cmark appends to fenced blocks.
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
            if let Some(Frame::Item { spans }) = stack.pop() {
                for frame in stack.iter_mut().rev() {
                    if let Frame::List { items, .. } = frame {
                        items.push(spans);
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
                if t.in_head {
                    t.head = row; // single header row
                } else {
                    t.rows.push(row);
                }
            }
        }
        TagEnd::TableHead => {
            if let Some(t) = table.as_mut() {
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

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Returns true when the stack currently has an Item frame above the nearest List.
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

/// Push a span to the innermost span-accepting frame or the active table cell.
fn push_span(stack: &mut [Frame], table: &mut Option<TableState>, span: Span) {
    if let Some(ts) = table.as_mut() {
        ts.current_cell.push(span);
        return;
    }
    for frame in stack.iter_mut().rev() {
        match frame {
            Frame::CodeBlock { text, .. } => {
                // Code blocks accumulate raw text rather than styled spans.
                text.push_str(&span.text);
                return;
            }
            Frame::Heading { spans, .. } | Frame::Paragraph { spans } | Frame::Item { spans } => {
                spans.push(span);
                return;
            }
            _ => {}
        }
    }
}

/// Push a finished block to the nearest block-accepting ancestor (Root or Quote).
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

fn alignment_str(a: Alignment) -> &'static str {
    match a {
        Alignment::Left => "left",
        Alignment::Center => "center",
        Alignment::Right => "right",
        Alignment::None => "none",
    }
}

// ── Lua conversion ────────────────────────────────────────────────────────────

fn spans_to_lua(lua: &Lua, spans: Vec<Span>) -> LuaResult<LuaTable> {
    let t = lua.create_table_with_capacity(spans.len(), 0)?;
    for (i, s) in spans.into_iter().enumerate() {
        let st = lua.create_table_with_capacity(0, 6)?;
        st.set("text", s.text)?;
        st.set("bold", s.bold)?;
        st.set("italic", s.italic)?;
        st.set("code", s.code)?;
        st.set("strikethrough", s.strikethrough)?;
        if let Some(href) = s.href {
            st.set("href", href)?;
        }
        t.raw_set(i + 1, st)?;
    }
    Ok(t)
}

fn block_to_lua(lua: &Lua, block: Block) -> LuaResult<LuaTable> {
    let t = lua.create_table()?;
    match block {
        Block::Heading { level, inlines } => {
            t.set("type", "heading")?;
            t.set("level", level)?;
            t.set("inlines", spans_to_lua(lua, inlines)?)?;
        }
        Block::Paragraph { inlines } => {
            t.set("type", "paragraph")?;
            t.set("inlines", spans_to_lua(lua, inlines)?)?;
        }
        Block::Code { lang, text } => {
            t.set("type", "code_block")?;
            if let Some(l) = lang {
                t.set("lang", l)?;
            }
            t.set("text", text)?;
        }
        Block::Rule => {
            t.set("type", "rule")?;
        }
        Block::Quote { blocks } => {
            t.set("type", "blockquote")?;
            t.set("blocks", blocks_to_lua(lua, blocks)?)?;
        }
        Block::List {
            ordered,
            start,
            items,
        } => {
            t.set("type", "list")?;
            t.set("ordered", ordered)?;
            t.set("start", start)?;
            let items_lua = lua.create_table_with_capacity(items.len(), 0)?;
            for (i, item_spans) in items.into_iter().enumerate() {
                items_lua.raw_set(i + 1, spans_to_lua(lua, item_spans)?)?;
            }
            t.set("items", items_lua)?;
        }
        Block::Table {
            alignments,
            head,
            rows,
        } => {
            t.set("type", "table")?;
            let aligns_lua = lua.create_table_with_capacity(alignments.len(), 0)?;
            for (i, a) in alignments.iter().enumerate() {
                aligns_lua.raw_set(i + 1, a.as_str())?;
            }
            t.set("alignments", aligns_lua)?;
            // head: array of cells
            let head_lua = lua.create_table_with_capacity(head.len(), 0)?;
            for (i, cell) in head.into_iter().enumerate() {
                head_lua.raw_set(i + 1, spans_to_lua(lua, cell)?)?;
            }
            t.set("head", head_lua)?;
            // rows: array of rows, each row is array of cells
            let rows_lua = lua.create_table_with_capacity(rows.len(), 0)?;
            for (i, row) in rows.into_iter().enumerate() {
                let row_lua = lua.create_table_with_capacity(row.len(), 0)?;
                for (j, cell) in row.into_iter().enumerate() {
                    row_lua.raw_set(j + 1, spans_to_lua(lua, cell)?)?;
                }
                rows_lua.raw_set(i + 1, row_lua)?;
            }
            t.set("rows", rows_lua)?;
        }
    }
    Ok(t)
}

fn blocks_to_lua(lua: &Lua, blocks: Vec<Block>) -> LuaResult<LuaTable> {
    let t = lua.create_table_with_capacity(blocks.len(), 0)?;
    for (i, b) in blocks.into_iter().enumerate() {
        t.raw_set(i + 1, block_to_lua(lua, b)?)?;
    }
    Ok(t)
}
