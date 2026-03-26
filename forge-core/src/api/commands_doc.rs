use mlua::prelude::*;

type SelectionRange = (usize, usize, usize, usize, usize);

fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

fn sort_positions(
    line1: usize,
    col1: usize,
    line2: usize,
    col2: usize,
) -> (usize, usize, usize, usize, bool) {
    if line1 > line2 || (line1 == line2 && col1 > col2) {
        (line2, col2, line1, col1, true)
    } else {
        (line1, col1, line2, col2, false)
    }
}

fn selection_count(doc: &LuaTable) -> LuaResult<usize> {
    Ok(doc.get::<LuaTable>("selections")?.raw_len() / 4)
}

fn doc_selections(doc: &LuaTable) -> LuaResult<Vec<usize>> {
    let selections: LuaTable = doc.get("selections")?;
    selections.sequence_values::<usize>().collect()
}

fn selection_ranges(
    doc: &LuaTable,
    sort: bool,
    reverse: bool,
) -> LuaResult<Vec<SelectionRange>> {
    let selections = doc_selections(doc)?;
    let mut out = Vec::with_capacity(selections.len() / 4);
    for (n, chunk) in selections.chunks_exact(4).enumerate() {
        let idx = n + 1;
        let (line1, col1, line2, col2, _) = if sort {
            sort_positions(chunk[0], chunk[1], chunk[2], chunk[3])
        } else {
            (chunk[0], chunk[1], chunk[2], chunk[3], false)
        };
        out.push((idx, line1, col1, line2, col2));
    }
    if reverse {
        out.reverse();
    }
    Ok(out)
}

fn multiline_selection_ranges(
    doc: &LuaTable,
    sort: bool,
) -> LuaResult<Vec<SelectionRange>> {
    let lines: LuaTable = doc.get("lines")?;
    let mut out = Vec::new();
    for (idx, line1, col1, mut line2, mut col2) in selection_ranges(doc, sort, false)? {
        if line2 > line1 && col2 == 1 {
            line2 -= 1;
            col2 = lines.get::<Option<String>>(line2)?.map(|s| s.len()).unwrap_or(1);
        }
        out.push((idx, line1, col1, line2, col2));
    }
    Ok(out)
}

fn append_line_if_last_line(doc: &LuaTable, line: usize) -> LuaResult<()> {
    let lines: LuaTable = doc.get("lines")?;
    if line >= lines.raw_len() {
        doc.call_method::<()>("insert", (line, f64::INFINITY, "\n"))?;
    }
    Ok(())
}

fn set_primary_selection(lua: &Lua, doc: &LuaTable) -> LuaResult<()> {
    let platform: String = lua.globals().get("PLATFORM")?;
    if platform != "Windows" {
        let system: LuaTable = lua.globals().get("system")?;
        let text: String = doc.call_method("get_selection_text", ())?;
        system.call_function::<()>("set_primary_selection", text)?;
    }
    Ok(())
}

fn line_indent(text: &str) -> String {
    text.chars()
        .take_while(|c| *c == '\t' || *c == ' ')
        .collect()
}

fn active_docview(lua: &Lua) -> LuaResult<LuaTable> {
    require_table(lua, "core")?.get("active_view")
}

fn active_doc(lua: &Lua) -> LuaResult<LuaTable> {
    active_docview(lua)?.get("doc")
}

fn save_with_filename(lua: &Lua, filename: Option<String>) -> LuaResult<()> {
    let doc = active_doc(lua)?;
    let core = require_table(lua, "core")?;
    let system: LuaTable = lua.globals().get("system")?;

    let mut normalized = filename;
    let mut abs_filename = None::<String>;
    if let Some(name) = normalized.clone() {
        let project = core.call_function::<Option<LuaTable>>("root_project", ())?;
        normalized = Some(if let Some(project) = &project {
            project.call_method("normalize_path", name)?
        } else {
            name
        });
        abs_filename = Some(if let Some(project) = &project {
            project.call_method("absolute_path", normalized.clone().unwrap())?
        } else {
            system.call_function("absolute_path", normalized.clone().unwrap())?
        });
    }

    match doc.call_method::<()>("save", (normalized.clone(), abs_filename.clone())) {
        Ok(()) => {
            let saved_filename: String = doc.get("filename")?;
            if doc.get::<Option<String>>("abs_filename")?.is_some()
                && core.get::<Option<LuaFunction>>("update_recent_file")?.is_some()
            {
                let update_recent: LuaFunction = core.get("update_recent_file")?;
                update_recent.call::<()>(doc.get::<String>("abs_filename")?)?;
            }
            core.call_function::<()>("log", format!("Saved \"{saved_filename}\""))?;
        }
        Err(err) => {
            core.call_function::<()>("error", err.to_string())?;
            let nag_view: LuaTable = core.get("nag_view")?;
            let spec = vec![
                {
                    let t = lua.create_table()?;
                    t.set("text", "Yes")?;
                    t.set("default_yes", true)?;
                    t
                },
                {
                    let t = lua.create_table()?;
                    t.set("text", "No")?;
                    t.set("default_no", true)?;
                    t
                },
            ];
            nag_view.call_method::<()>(
                "show",
                (
                    "Saving failed",
                    format!(
                        "Couldn't save file \"{}\". Do you want to save to another location?",
                        doc.get::<Option<String>>("filename")?
                            .unwrap_or_else(|| "unsaved".to_string())
                    ),
                    spec,
                    lua.create_function(|lua, item: LuaTable| {
                        if item.get::<String>("text")? == "Yes" {
                            let core = require_table(lua, "core")?;
                            let command = require_table(lua, "core.command")?;
                            let thunk = lua.create_function(move |lua, ()| {
                                require_table(lua, "core.command")?
                                    .call_function::<bool>("perform", "doc:save-as")?;
                                Ok(())
                            })?;
                            let _ = command;
                            core.call_function::<()>("add_thread", thunk)?;
                        }
                        Ok(())
                    })?,
                ),
            )?;
        }
    }

    Ok(())
}

fn insert_paste(doc: &LuaTable, value: String, whole_line: bool, idx: usize) -> LuaResult<()> {
    if whole_line {
        let (line1, col1, _, _, _): (usize, usize, usize, usize, Option<bool>) =
            doc.call_method("get_selection_idx", idx)?;
        doc.call_method::<()>("insert", (line1, 1, format!("{}\n", value.replace('\r', ""))))?;
        if col1 == 1 {
            doc.call_method::<()>("move_to_cursor", (idx, value.len() + 1))?;
        }
    } else {
        doc.call_method::<()>("text_input", (value.replace('\r', ""), idx))?;
    }
    Ok(())
}

fn cut_or_copy(lua: &Lua, delete: bool) -> LuaResult<()> {
    let doc = active_doc(lua)?;
    let core = require_table(lua, "core")?;
    let system: LuaTable = lua.globals().get("system")?;
    let cursor_clipboard = lua.create_table()?;
    let whole_line = lua.create_table()?;
    let mut full_text = String::new();

    for (idx, line1, col1, line2, col2) in selection_ranges(&doc, true, true)? {
        let text = if line1 != line2 || col1 != col2 {
            let text: String = doc.call_method("get_text", (line1, col1, line2, col2))?;
            whole_line.set(idx, false)?;
            if delete {
                doc.call_method::<()>("delete_to_cursor", (idx, 0))?;
            }
            text
        } else {
            let lines: LuaTable = doc.get("lines")?;
            let line = lines.get::<String>(line1)?;
            let text = line[..line.len().saturating_sub(1)].to_string();
            whole_line.set(idx, true)?;
            if delete {
                let lines_len = lines.raw_len();
                if line1 < lines_len {
                    doc.call_method::<()>("remove", (line1, 1, line1 + 1, 1))?;
                } else if lines_len == 1 {
                    doc.call_method::<()>("remove", (line1, 1, line1, f64::INFINITY))?;
                } else {
                    doc.call_method::<()>("remove", (line1 - 1, f64::INFINITY, line1, f64::INFINITY))?;
                }
                doc.call_method::<()>("set_selections", (idx, line1, col1, line2, col2))?;
            }
            text
        };

        full_text = if full_text.is_empty() {
            if whole_line.get::<bool>(idx)? {
                format!("{text}\n")
            } else {
                text.clone()
            }
        } else if whole_line.get::<bool>(idx)? {
            format!("{text}\n{full_text}")
        } else {
            format!("{text} {full_text}")
        };
        cursor_clipboard.set(idx, text)?;
    }

    if delete {
        doc.call_method::<()>("merge_cursors", ())?;
    }
    cursor_clipboard.set("full", full_text.clone())?;
    core.set("cursor_clipboard", cursor_clipboard)?;
    core.set("cursor_clipboard_whole_line", whole_line)?;
    system.call_function::<()>("set_clipboard", full_text)?;
    Ok(())
}

fn split_cursor(lua: &Lua, dv: &LuaTable, direction: i64) -> LuaResult<()> {
    let doc: LuaTable = dv.get("doc")?;
    let docview = require_table(lua, "core.docview")?;
    let translate: LuaTable = docview.get("translate")?;
    let translate_fn: LuaFunction = if direction < 0 {
        translate.get("previous_line")?
    } else {
        translate.get("next_line")?
    };
    let lines: LuaTable = doc.get("lines")?;
    let lines_len = lines.raw_len() as i64;
    let mut new_cursors = Vec::new();
    for (_, line1, col1, _, _) in selection_ranges(&doc, false, false)? {
        if (line1 as i64 + direction) >= 1 && (line1 as i64 + direction) <= lines_len {
            let (line, col): (usize, usize) =
                translate_fn.call((doc.clone(), line1, col1, dv.clone()))?;
            new_cursors.push((line, col));
        }
    }
    if direction < 0 {
        new_cursors.reverse();
    }
    for (line, col) in new_cursors {
        doc.call_method::<()>("add_selection", (line, col))?;
    }
    require_table(lua, "core")?.call_function::<()>("blink_reset", ())?;
    Ok(())
}

fn set_cursor(lua: &Lua, dv: &LuaTable, x: f64, y: f64, snap_type: &str) -> LuaResult<()> {
    let (line, col): (usize, usize) = dv.call_method("resolve_screen_position", (x, y))?;
    let doc: LuaTable = dv.get("doc")?;
    doc.call_method::<()>("set_selection", (line, col, line, col))?;
    if snap_type == "word" || snap_type == "lines" {
        require_table(lua, "core.command")?
            .call_function::<bool>("perform", format!("doc:select-{snap_type}"))?;
    }
    let mouse_selecting = lua.create_table()?;
    mouse_selecting.set(1, line)?;
    mouse_selecting.set(2, col)?;
    mouse_selecting.set(3, snap_type)?;
    dv.set("mouse_selecting", mouse_selecting)?;
    require_table(lua, "core")?.call_function::<()>("blink_reset", ())?;
    Ok(())
}

fn line_comment(
    doc: &LuaTable,
    comment: LuaValue,
    line1: usize,
    col1: usize,
    line2: usize,
    col2: usize,
) -> LuaResult<(usize, usize, usize, usize)> {
    let (open, close) = match comment {
        LuaValue::Table(t) => (t.get::<String>(1)?, Some(t.get::<String>(2)?)),
        LuaValue::String(s) => (s.to_str()?.to_string(), None),
        _ => return Err(LuaError::runtime("invalid comment spec")),
    };

    let start_comment = format!("{open} ");
    let end_comment = close.clone().map(|c| format!(" {c}"));
    let lines: LuaTable = doc.get("lines")?;
    let mut uncomment = true;
    let mut start_offset = usize::MAX;

    for line in line1..=line2 {
        let text = lines.get::<String>(line)?;
        if let Some(s) = text.find(|c: char| !c.is_whitespace()).map(|i| i + 1) {
            if !text[s - 1..].starts_with(&start_comment) {
                uncomment = false;
            }
            start_offset = start_offset.min(s);
        }
    }

    let end_line = col2 == lines.get::<String>(line2)?.len();
    for line in line1..=line2 {
        let text = lines.get::<String>(line)?;
        if let Some(s) = text.find(|c: char| !c.is_whitespace()).map(|i| i + 1) {
            if uncomment {
                if let Some(end_comment) = &end_comment {
                    if text[..text.len().saturating_sub(1)].ends_with(end_comment) {
                        doc.call_method::<()>(
                            "remove",
                            (line, text.len() - end_comment.len(), line, text.len()),
                        )?;
                    }
                }
                if let Some(cs) = text[s - 1..].find(&start_comment).map(|i| s + i) {
                    doc.call_method::<()>("remove", (line, cs, line, cs + start_comment.len()))?;
                }
            } else {
                doc.call_method::<()>("insert", (line, start_offset, start_comment.clone()))?;
                if let Some(close) = &close {
                    let updated = lines.get::<String>(line)?;
                    doc.call_method::<()>("insert", (line, updated.len(), format!(" {close}")))?;
                }
            }
        }
    }

    let delta = if uncomment {
        -(start_comment.len() as isize)
    } else {
        start_comment.len() as isize
    };
    let mut out_col1 = col1 as isize;
    let mut out_col2 = col2 as isize;
    if col1 > start_offset {
        out_col1 += delta;
    }
    if col2 > start_offset {
        out_col2 += delta;
    }
    if let Some(end_comment) = end_comment {
        if end_line {
            out_col2 += if uncomment {
                -(end_comment.len() as isize)
            } else {
                end_comment.len() as isize
            };
        }
    }
    Ok((line1, out_col1.max(1) as usize, line2, out_col2.max(1) as usize))
}

fn block_comment(
    doc: &LuaTable,
    comment: LuaTable,
    line1: usize,
    col1: usize,
    line2: usize,
    col2: usize,
) -> LuaResult<(usize, usize, usize, usize)> {
    let open: String = comment.get(1)?;
    let close: String = comment.get(2)?;

    let word_start = doc
        .call_method::<String>("get_text", (line1, col1, line1, f64::INFINITY))?
        .find(|c: char| !c.is_whitespace())
        .map(|i| i + 1)
        .unwrap_or(0);
    let word_end = doc
        .call_method::<String>("get_text", (line2, 1, line2, col2))?
        .rfind(|c: char| !c.is_whitespace())
        .map(|i| i + 1)
        .unwrap_or(col2);
    let col1 = col1 + word_start.saturating_sub(1);
    let col2 = word_end;

    let block_start: String = doc.call_method("get_text", (line1, col1, line1, col1 + open.len()))?;
    let block_end: String = doc.call_method(
        "get_text",
        (line2, col2.saturating_sub(close.len()), line2, col2),
    )?;

    if block_start == open && block_end == close {
        let mut start_len = open.len();
        let mut stop_len = close.len();
        let after: String =
            doc.call_method("get_text", (line1, col1 + open.len(), line1, col1 + open.len() + 1))?;
        if after.chars().last().is_some_and(|ch| ch.is_whitespace()) {
            start_len += 1;
        }
        let before: String = doc.call_method(
            "get_text",
            (line2, col2.saturating_sub(close.len() + 1), line2, col2),
        )?;
        if before.chars().next().is_some_and(|ch| ch.is_whitespace()) {
            stop_len += 1;
        }

        doc.call_method::<()>("remove", (line1, col1, line1, col1 + start_len))?;
        let adj_col2 = col2.saturating_sub(if line1 == line2 { start_len } else { 0 });
        doc.call_method::<()>("remove", (line2, adj_col2 - stop_len, line2, adj_col2))?;
        Ok((line1, col1, line2, adj_col2 - stop_len))
    } else {
        doc.call_method::<()>("insert", (line1, col1, format!("{open} ")))?;
        let adj_col2 = col2 + if line1 == line2 { open.len() + 1 } else { 0 };
        doc.call_method::<()>("insert", (line2, adj_col2, format!(" {close}")))?;
        Ok((line1, col1, line2, adj_col2 + close.len() + 1))
    }
}

fn add_command(map: &LuaTable, name: &str, func: LuaFunction) -> LuaResult<()> {
    map.set(name, func)
}

fn register_text_transform(lua: &Lua, commands: &LuaTable, name: &str, transform: &str) -> LuaResult<()> {
    let command_name = name.to_string();
    let transform_name = transform.to_string();
    add_command(
        commands,
        &command_name,
        lua.create_function(move |lua, dv: LuaTable| {
            let doc: LuaTable = dv.get("doc")?;
            let string_mod: LuaTable = lua.globals().get("string")?;
            let func: LuaFunction = string_mod.get(transform_name.as_str())?;
            doc.call_method::<()>("replace", func)?;
            Ok(())
        })?,
    )
}

fn register_translate_command_set(
    lua: &Lua,
    commands: &LuaTable,
    name: &str,
    source: &str,
) -> LuaResult<()> {
    let move_name = format!("doc:move-to-{name}");
    let select_name = format!("doc:select-to-{name}");
    let delete_name = format!("doc:delete-to-{name}");
    let method = name.replace('-', "_");
    let source_name = source.to_string();

    add_command(
        commands,
        &move_name,
        lua.create_function({
            let method = method.clone();
            let source_name = source_name.clone();
            move |lua, dv: LuaTable| {
                let doc: LuaTable = dv.get("doc")?;
                let source = require_table(lua, source_name.as_str())?;
                let func: LuaFunction = source.get(method.as_str())?;
                doc.call_method::<()>("move_to", (func, dv))?;
                Ok(())
            }
        })?,
    )?;

    add_command(
        commands,
        &select_name,
        lua.create_function({
            let method = method.clone();
            let source_name = source_name.clone();
            move |lua, dv: LuaTable| {
                let doc: LuaTable = dv.get("doc")?;
                let source = require_table(lua, source_name.as_str())?;
                let func: LuaFunction = source.get(method.as_str())?;
                doc.call_method::<()>("select_to", (func, dv.clone()))?;
                set_primary_selection(lua, &doc)?;
                Ok(())
            }
        })?,
    )?;

    add_command(
        commands,
        &delete_name,
        lua.create_function(move |lua, dv: LuaTable| {
            let doc: LuaTable = dv.get("doc")?;
            let source = require_table(lua, source_name.as_str())?;
            let func: LuaFunction = source.get(method.as_str())?;
            doc.call_method::<()>("delete_to", (func, dv))?;
            Ok(())
        })?,
    )
}

fn register_docview_translate_command_set(
    lua: &Lua,
    commands: &LuaTable,
    name: &str,
) -> LuaResult<()> {
    let move_name = format!("doc:move-to-{name}");
    let select_name = format!("doc:select-to-{name}");
    let delete_name = format!("doc:delete-to-{name}");
    let method = name.replace('-', "_");

    add_command(
        commands,
        &move_name,
        lua.create_function({
            let method = method.clone();
            move |lua, dv: LuaTable| {
                let doc: LuaTable = dv.get("doc")?;
                let translate: LuaTable = require_table(lua, "core.docview")?.get("translate")?;
                let func: LuaFunction = translate.get(method.as_str())?;
                doc.call_method::<()>("move_to", (func, dv))?;
                Ok(())
            }
        })?,
    )?;

    add_command(
        commands,
        &select_name,
        lua.create_function({
            let method = method.clone();
            move |lua, dv: LuaTable| {
                let doc: LuaTable = dv.get("doc")?;
                let translate: LuaTable = require_table(lua, "core.docview")?.get("translate")?;
                let func: LuaFunction = translate.get(method.as_str())?;
                doc.call_method::<()>("select_to", (func, dv.clone()))?;
                set_primary_selection(lua, &doc)?;
                Ok(())
            }
        })?,
    )?;

    add_command(
        commands,
        &delete_name,
        lua.create_function(move |lua, dv: LuaTable| {
            let doc: LuaTable = dv.get("doc")?;
            let translate: LuaTable = require_table(lua, "core.docview")?.get("translate")?;
            let func: LuaFunction = translate.get(method.as_str())?;
            doc.call_method::<()>("delete_to", (func, dv))?;
            Ok(())
        })?,
    )
}

fn register_commands(lua: &Lua) -> LuaResult<()> {
    let command = require_table(lua, "core.command")?;
    let commands = lua.create_table()?;

    add_command(
        &commands,
        "doc:select-none",
        lua.create_function(|_, dv: LuaTable| {
            let doc: LuaTable = dv.get("doc")?;
            type Sel = (Option<usize>, Option<usize>, Option<usize>, Option<usize>, Option<bool>);
            let (mut l1, mut c1, _, _, _): Sel =
                doc.call_method("get_selection_idx", doc.get::<usize>("last_selection")?)?;
            if l1.is_none() {
                let vals: Sel = doc.call_method("get_selection_idx", 1)?;
                l1 = vals.0;
                c1 = vals.1;
            }
            doc.call_method::<()>("set_selection", (l1.unwrap_or(1), c1.unwrap_or(1)))?;
            Ok(())
        })?,
    )?;
    add_command(&commands, "doc:cut", lua.create_function(|lua, ()| cut_or_copy(lua, true))?)?;
    add_command(&commands, "doc:copy", lua.create_function(|lua, ()| cut_or_copy(lua, false))?)?;
    add_command(
        &commands,
        "doc:undo",
        lua.create_function(|_, dv: LuaTable| dv.get::<LuaTable>("doc")?.call_method::<()>("undo", ()))?,
    )?;
    add_command(
        &commands,
        "doc:redo",
        lua.create_function(|_, dv: LuaTable| dv.get::<LuaTable>("doc")?.call_method::<()>("redo", ()))?,
    )?;
    add_command(
        &commands,
        "doc:paste",
        lua.create_function(|lua, dv: LuaTable| {
            let system: LuaTable = lua.globals().get("system")?;
            let clipboard: String = system.call_function("get_clipboard", ())?;
            if clipboard.is_empty() {
                return Ok(());
            }

            let core = require_table(lua, "core")?;
            let doc: LuaTable = dv.get("doc")?;
            let cursor_clipboard = core
                .get::<Option<LuaTable>>("cursor_clipboard")?
                .unwrap_or(lua.create_table()?);
            if cursor_clipboard.get::<Option<String>>("full")?.as_deref() != Some(&clipboard) {
                core.set("cursor_clipboard", lua.create_table()?)?;
                core.set("cursor_clipboard_whole_line", lua.create_table()?)?;
                for (idx, _, _, _, _) in selection_ranges(&doc, false, false)? {
                    insert_paste(&doc, clipboard.clone(), false, idx)?;
                }
                return Ok(());
            }

            let whole_line = core
                .get::<Option<LuaTable>>("cursor_clipboard_whole_line")?
                .unwrap_or(lua.create_table()?);
            let mut only_whole_lines = true;
            for value in whole_line.pairs::<LuaValue, bool>() {
                let (_, is_whole) = value?;
                if !is_whole {
                    only_whole_lines = false;
                    break;
                }
            }

            if whole_line.raw_len() == selection_count(&doc)? {
                for (idx, _, _, _, _) in selection_ranges(&doc, false, false)? {
                    insert_paste(
                        &doc,
                        cursor_clipboard.get::<String>(idx)?,
                        only_whole_lines,
                        idx,
                    )?;
                }
            } else {
                let mut new_selections = Vec::new();
                for (idx, _, _, _, _) in selection_ranges(&doc, false, false)? {
                    if only_whole_lines {
                        for cb_idx in 1..=whole_line.raw_len() {
                            insert_paste(
                                &doc,
                                cursor_clipboard.get::<String>(cb_idx)?,
                                true,
                                idx,
                            )?;
                        }
                        new_selections.push(
                            doc.call_method::<(usize, usize, usize, usize, Option<bool>)>(
                                "get_selection_idx",
                                idx,
                            )?,
                        );
                    } else {
                        for cb_idx in 1..=whole_line.raw_len() {
                            insert_paste(
                                &doc,
                                cursor_clipboard.get::<String>(cb_idx)?,
                                false,
                                idx,
                            )?;
                            new_selections.push(
                                doc.call_method::<(usize, usize, usize, usize, Option<bool>)>(
                                    "get_selection_idx",
                                    idx,
                                )?,
                            );
                        }
                    }
                }

                let mut first = true;
                for (l1, c1, l2, c2, _) in new_selections {
                    if first {
                        doc.call_method::<()>("set_selection", (l1, c1, l2, c2))?;
                        first = false;
                    } else {
                        doc.call_method::<()>("add_selection", (l1, c1, l2, c2))?;
                    }
                }
            }
            Ok(())
        })?,
    )?;
    add_command(
        &commands,
        "doc:paste-primary-selection",
        lua.create_function(|lua, (dv, x, y): (LuaTable, Option<f64>, Option<f64>)| {
            if let (Some(x), Some(y)) = (x, y) {
                set_cursor(lua, &dv, x, y, "set")?;
                dv.set("mouse_selecting", LuaValue::Nil)?;
            }
            let system: LuaTable = lua.globals().get("system")?;
            let text: Option<String> = system.call_function("get_primary_selection", ())?;
            dv.get::<LuaTable>("doc")?
                .call_method::<()>("text_input", text.unwrap_or_default())?;
            Ok(())
        })?,
    )?;
    add_command(
        &commands,
        "doc:newline",
        lua.create_function(|lua, dv: LuaTable| {
            let doc: LuaTable = dv.get("doc")?;
            let lines: LuaTable = doc.get("lines")?;
            let config = require_table(lua, "core.config")?;
            let keep_ws: bool = config.get("keep_newline_whitespace")?;
            for (idx, line, col, _, _) in selection_ranges(&doc, false, true)? {
                let line_text = lines.get::<String>(line)?;
                let mut indent = line_indent(&line_text);
                if col <= indent.len() {
                    indent = indent[indent.len() + 1 - col..].to_string();
                }
                if !keep_ws && line_text.trim().is_empty() {
                    doc.call_method::<()>("remove", (line, 1, line, f64::INFINITY))?;
                }
                doc.call_method::<()>("text_input", (format!("\n{indent}"), idx))?;
            }
            Ok(())
        })?,
    )?;
    add_command(
        &commands,
        "doc:newline-below",
        lua.create_function(|_, dv: LuaTable| {
            let doc: LuaTable = dv.get("doc")?;
            let lines: LuaTable = doc.get("lines")?;
            for (idx, line, _, _, _) in selection_ranges(&doc, false, true)? {
                let indent = line_indent(&lines.get::<String>(line)?);
                doc.call_method::<()>("insert", (line, f64::INFINITY, format!("\n{indent}")))?;
                doc.call_method::<()>("set_selections", (idx, line + 1, f64::INFINITY))?;
            }
            Ok(())
        })?,
    )?;
    add_command(
        &commands,
        "doc:newline-above",
        lua.create_function(|_, dv: LuaTable| {
            let doc: LuaTable = dv.get("doc")?;
            let lines: LuaTable = doc.get("lines")?;
            for (idx, line, _, _, _) in selection_ranges(&doc, false, true)? {
                let indent = line_indent(&lines.get::<String>(line)?);
                doc.call_method::<()>("insert", (line, 1, format!("{indent}\n")))?;
                doc.call_method::<()>("set_selections", (idx, line, f64::INFINITY))?;
            }
            Ok(())
        })?,
    )?;
    add_command(
        &commands,
        "doc:delete",
        lua.create_function(|lua, dv: LuaTable| {
            let doc: LuaTable = dv.get("doc")?;
            let translate = require_table(lua, "core.doc.translate")?;
            let next_char: LuaFunction = translate.get("next_char")?;
            let lines: LuaTable = doc.get("lines")?;
            for (idx, line1, col1, line2, col2) in selection_ranges(&doc, true, true)? {
                if line1 == line2
                    && col1 == col2
                    && lines.get::<String>(line1)?[col1.saturating_sub(1)..].trim().is_empty()
                {
                    doc.call_method::<()>("remove", (line1, col1, line1, f64::INFINITY))?;
                }
                doc.call_method::<()>("delete_to_cursor", (idx, next_char.clone()))?;
            }
            Ok(())
        })?,
    )?;
    add_command(
        &commands,
        "doc:backspace",
        lua.create_function(|lua, dv: LuaTable| {
            let doc: LuaTable = dv.get("doc")?;
            let (_, indent_size): (LuaValue, usize) = doc.call_method("get_indent_info", ())?;
            let translate = require_table(lua, "core.doc.translate")?;
            let previous_char: LuaFunction = translate.get("previous_char")?;
            for (idx, line1, col1, line2, col2) in selection_ranges(&doc, true, true)? {
                if line1 == line2 && col1 == col2 {
                    let text: String = doc.call_method("get_text", (line1, 1, line1, col1))?;
                    if text.len() >= indent_size && text.chars().all(|c| c == ' ') {
                        doc.call_method::<()>("delete_to_cursor", (idx, 0, -(indent_size as i64)))?;
                        continue;
                    }
                }
                doc.call_method::<()>("delete_to_cursor", (idx, previous_char.clone()))?;
            }
            Ok(())
        })?,
    )?;
    add_command(
        &commands,
        "doc:select-all",
        lua.create_function(|lua, dv: LuaTable| {
            let doc: LuaTable = dv.get("doc")?;
            doc.call_method::<()>("set_selection", (1, 1, f64::INFINITY, f64::INFINITY))?;
            set_primary_selection(lua, &doc)?;
            let lines: LuaTable = doc.get("lines")?;
            dv.set("last_line1", 1)?;
            dv.set("last_col1", 1)?;
            dv.set("last_line2", lines.raw_len())?;
            dv.set("last_col2", lines.get::<String>(lines.raw_len())?.len())?;
            Ok(())
        })?,
    )?;
    add_command(
        &commands,
        "doc:select-lines",
        lua.create_function(|lua, dv: LuaTable| {
            let doc: LuaTable = dv.get("doc")?;
            for (idx, line1, _, line2, _) in selection_ranges(&doc, true, false)? {
                append_line_if_last_line(&doc, line2)?;
                doc.call_method::<()>("set_selections", (idx, line2 + 1, 1, line1, 1))?;
            }
            set_primary_selection(lua, &doc)
        })?,
    )?;
    add_command(
        &commands,
        "doc:select-word",
        lua.create_function(|lua, dv: LuaTable| {
            let doc: LuaTable = dv.get("doc")?;
            let translate = require_table(lua, "core.doc.translate")?;
            let start_of_word: LuaFunction = translate.get("start_of_word")?;
            let end_of_word: LuaFunction = translate.get("end_of_word")?;
            for (idx, line1, col1, _, _) in selection_ranges(&doc, true, false)? {
                let (line1, col1): (usize, usize) = start_of_word.call((doc.clone(), line1, col1))?;
                let (line2, col2): (usize, usize) = end_of_word.call((doc.clone(), line1, col1))?;
                doc.call_method::<()>("set_selections", (idx, line2, col2, line1, col1))?;
            }
            set_primary_selection(lua, &doc)
        })?,
    )?;
    add_command(
        &commands,
        "doc:join-lines",
        lua.create_function(|_, dv: LuaTable| {
            let doc: LuaTable = dv.get("doc")?;
            for (idx, line1, col1, mut line2, col2) in selection_ranges(&doc, true, false)? {
                if line1 == line2 {
                    line2 += 1;
                }
                let text: String = doc.call_method("get_text", (line1, 1, line2, f64::INFINITY))?;
                let mut joined = String::new();
                let mut first = true;
                for segment in text.split('\n') {
                    let trimmed = segment.trim_start_matches(['\t', ' ']);
                    if first {
                        joined.push_str(segment);
                        first = false;
                    } else if segment.trim().is_empty() {
                        joined.push_str(trimmed);
                    } else {
                        joined.push(' ');
                        joined.push_str(trimmed);
                    }
                }
                doc.call_method::<()>("insert", (line1, 1, joined.clone()))?;
                doc.call_method::<()>("remove", (line1, joined.len() + 1, line2, f64::INFINITY))?;
                if line1 != line2 || col1 != col2 {
                    doc.call_method::<()>("set_selections", (idx, line1, f64::INFINITY))?;
                }
            }
            Ok(())
        })?,
    )?;
    add_command(
        &commands,
        "doc:indent",
        lua.create_function(|_, dv: LuaTable| {
            let doc: LuaTable = dv.get("doc")?;
            for (idx, line1, col1, line2, col2) in multiline_selection_ranges(&doc, true)? {
                let result: (Option<usize>, Option<usize>, Option<usize>, Option<usize>) =
                    doc.call_method("indent_text", (false, line1, col1, line2, col2))?;
                if let (Some(l1), Some(c1), Some(l2), Some(c2)) = result {
                    doc.call_method::<()>("set_selections", (idx, l1, c1, l2, c2))?;
                }
            }
            Ok(())
        })?,
    )?;
    add_command(
        &commands,
        "doc:unindent",
        lua.create_function(|_, dv: LuaTable| {
            let doc: LuaTable = dv.get("doc")?;
            for (idx, line1, col1, line2, col2) in multiline_selection_ranges(&doc, true)? {
                let result: (Option<usize>, Option<usize>, Option<usize>, Option<usize>) =
                    doc.call_method("indent_text", (true, line1, col1, line2, col2))?;
                if let (Some(l1), Some(c1), Some(l2), Some(c2)) = result {
                    doc.call_method::<()>("set_selections", (idx, l1, c1, l2, c2))?;
                }
            }
            Ok(())
        })?,
    )?;
    add_command(
        &commands,
        "doc:duplicate-lines",
        lua.create_function(|_, dv: LuaTable| {
            let doc: LuaTable = dv.get("doc")?;
            for (idx, line1, col1, line2, col2) in multiline_selection_ranges(&doc, true)? {
                append_line_if_last_line(&doc, line2)?;
                let text: String = doc.call_method("get_text", (line1, 1, line2 + 1, 1))?;
                doc.call_method::<()>("insert", (line2 + 1, 1, text))?;
                let n = line2 - line1 + 1;
                doc.call_method::<()>("set_selections", (idx, line1 + n, col1, line2 + n, col2))?;
            }
            Ok(())
        })?,
    )?;
    add_command(
        &commands,
        "doc:delete-lines",
        lua.create_function(|_, dv: LuaTable| {
            let doc: LuaTable = dv.get("doc")?;
            for (idx, line1, col1, line2, _) in multiline_selection_ranges(&doc, true)? {
                append_line_if_last_line(&doc, line2)?;
                doc.call_method::<()>("remove", (line1, 1, line2 + 1, 1))?;
                doc.call_method::<()>("set_selections", (idx, line1, col1))?;
            }
            Ok(())
        })?,
    )?;
    add_command(
        &commands,
        "doc:move-lines-up",
        lua.create_function(|_, dv: LuaTable| {
            let doc: LuaTable = dv.get("doc")?;
            let lines: LuaTable = doc.get("lines")?;
            for (idx, line1, col1, line2, col2) in multiline_selection_ranges(&doc, true)? {
                append_line_if_last_line(&doc, line2)?;
                if line1 > 1 {
                    let text = lines.get::<String>(line1 - 1)?;
                    doc.call_method::<()>("insert", (line2 + 1, 1, text))?;
                    doc.call_method::<()>("remove", (line1 - 1, 1, line1, 1))?;
                    doc.call_method::<()>("set_selections", (idx, line1 - 1, col1, line2 - 1, col2))?;
                }
            }
            Ok(())
        })?,
    )?;
    add_command(
        &commands,
        "doc:move-lines-down",
        lua.create_function(|_, dv: LuaTable| {
            let doc: LuaTable = dv.get("doc")?;
            let lines: LuaTable = doc.get("lines")?;
            for (idx, line1, col1, line2, col2) in multiline_selection_ranges(&doc, true)? {
                append_line_if_last_line(&doc, line2 + 1)?;
                if line2 < lines.raw_len() {
                    let text = lines.get::<String>(line2 + 1)?;
                    doc.call_method::<()>("remove", (line2 + 1, 1, line2 + 2, 1))?;
                    doc.call_method::<()>("insert", (line1, 1, text))?;
                    doc.call_method::<()>("set_selections", (idx, line1 + 1, col1, line2 + 1, col2))?;
                }
            }
            Ok(())
        })?,
    )?;
    add_command(
        &commands,
        "doc:toggle-block-comments",
        lua.create_function(|lua, dv: LuaTable| {
            let doc: LuaTable = dv.get("doc")?;
            let tokenizer = require_table(lua, "core.tokenizer")?;
            let command = require_table(lua, "core.command")?;
            for (idx, line1, mut col1, line2, mut col2) in multiline_selection_ranges(&doc, true)? {
                let mut current_syntax: LuaTable = doc.get("syntax")?;
                if line1 > 1 {
                    let highlighter: LuaTable = doc.get("highlighter")?;
                    let prev_line: LuaTable = highlighter.call_method("get_line", line1 - 1)?;
                    let state: LuaValue = prev_line.get("state")?;
                    let syntaxes: LuaTable =
                        tokenizer.call_function("extract_subsyntaxes", (current_syntax.clone(), state))?;
                    for syntax in syntaxes.sequence_values::<LuaTable>() {
                        let syntax = syntax?;
                        if syntax.get::<Option<LuaValue>>("block_comment")?.is_some() {
                            current_syntax = syntax;
                            break;
                        }
                    }
                }

                let comment = current_syntax.get::<Option<LuaTable>>("block_comment")?;
                let Some(comment) = comment else {
                    if doc.get::<LuaTable>("syntax")?
                        .get::<Option<LuaValue>>("comment")?
                        .is_some()
                    {
                        command.call_function::<bool>("perform", "doc:toggle-line-comments")?;
                    }
                    return Ok(());
                };

                if line1 == line2 && col1 == col2 {
                    let lines: LuaTable = doc.get("lines")?;
                    col1 = 1;
                    col2 = lines.get::<Option<String>>(line2)?.map(|s| s.len()).unwrap_or(1);
                }

                let (l1, c1, l2, c2) = block_comment(&doc, comment, line1, col1, line2, col2)?;
                doc.call_method::<()>("set_selections", (idx, l1, c1, l2, c2))?;
            }
            Ok(())
        })?,
    )?;
    add_command(
        &commands,
        "doc:toggle-line-comments",
        lua.create_function(|lua, dv: LuaTable| {
            let doc: LuaTable = dv.get("doc")?;
            let tokenizer = require_table(lua, "core.tokenizer")?;
            for (idx, line1, col1, line2, col2) in multiline_selection_ranges(&doc, true)? {
                let mut current_syntax: LuaTable = doc.get("syntax")?;
                if line1 > 1 {
                    let highlighter: LuaTable = doc.get("highlighter")?;
                    let prev_line: LuaTable = highlighter.call_method("get_line", line1 - 1)?;
                    let state: LuaValue = prev_line.get("state")?;
                    let syntaxes: LuaTable =
                        tokenizer.call_function("extract_subsyntaxes", (current_syntax.clone(), state))?;
                    for syntax in syntaxes.sequence_values::<LuaTable>() {
                        let syntax = syntax?;
                        if syntax.get::<Option<LuaValue>>("comment")?.is_some()
                            || syntax.get::<Option<LuaValue>>("block_comment")?.is_some()
                        {
                            current_syntax = syntax;
                            break;
                        }
                    }
                }

                let comment = if let Some(comment) = current_syntax.get::<Option<LuaValue>>("comment")? {
                    Some(comment)
                } else {
                    current_syntax.get::<Option<LuaValue>>("block_comment")?
                };
                if let Some(comment) = comment {
                    let (l1, c1, l2, c2) = line_comment(&doc, comment, line1, col1, line2, col2)?;
                    doc.call_method::<()>("set_selections", (idx, l1, c1, l2, c2))?;
                }
            }
            Ok(())
        })?,
    )?;
    register_text_transform(lua, &commands, "doc:upper-case", "uupper")?;
    register_text_transform(lua, &commands, "doc:lower-case", "ulower")?;
    add_command(
        &commands,
        "doc:go-to-line",
        lua.create_function(|lua, dv: LuaTable| {
            let core = require_table(lua, "core")?;
            let command_view: LuaTable = core.get("command_view")?;
            let dv_submit = dv.clone();
            let dv_suggest = dv.clone();
            command_view.call_method::<()>(
                "enter",
                (
                    "Go To Line",
                    {
                        let spec = lua.create_table()?;
                        spec.set(
                            "submit",
                            lua.create_function(move |lua, (text, item): (String, Option<LuaTable>)| {
                                let line = if let Some(item) = item {
                                    item.get::<usize>("line")?
                                } else if text.is_empty() {
                                    0
                                } else {
                                    text.parse::<usize>().unwrap_or(0)
                                };
                                if line == 0 {
                                    require_table(lua, "core")?.call_function::<()>(
                                        "error",
                                        "Invalid line number or unmatched string",
                                    )?;
                                    return Ok(());
                                }
                                let doc: LuaTable = dv_submit.get("doc")?;
                                doc.call_method::<()>("set_selection", (line, 1))?;
                                dv_submit.call_method::<()>("scroll_to_line", (line, true))?;
                                Ok(())
                            })?,
                        )?;
                        spec.set(
                            "suggest",
                            lua.create_function(move |lua, text: String| -> LuaResult<LuaValue> {
                                if text.chars().all(|ch| ch.is_ascii_digit()) {
                                    return Ok(LuaValue::Nil);
                                }

                                let doc: LuaTable = dv_suggest.get("doc")?;
                                let lines: LuaTable = doc.get("lines")?;
                                let items = lua.create_table()?;
                                let mt = lua.create_table()?;
                                mt.set(
                                    "__tostring",
                                    lua.create_function(|_, item: LuaTable| item.get::<String>("text"))?,
                                )?;
                                for idx in 1..=lines.raw_len() {
                                    let item = lua.create_table()?;
                                    item.set("text", lines.get::<String>(idx)?.trim_end_matches('\n'))?;
                                    item.set("line", idx)?;
                                    item.set("info", format!("line: {idx}"))?;
                                    item.set_metatable(Some(mt.clone()))?;
                                    items.set(idx, item)?;
                                }

                                let common = require_table(lua, "core.common")?;
                                common.call_function("fuzzy_match", (items, text))
                            })?,
                        )?;
                        spec
                    },
                ),
            )
        })?,
    )?;
    add_command(
        &commands,
        "doc:toggle-line-ending",
        lua.create_function(|_, dv: LuaTable| {
            let doc: LuaTable = dv.get("doc")?;
            let crlf: bool = doc.get("crlf")?;
            doc.set("crlf", !crlf)
        })?,
    )?;
    add_command(
        &commands,
        "doc:toggle-overwrite",
        lua.create_function(|lua, dv: LuaTable| {
            let doc: LuaTable = dv.get("doc")?;
            let overwrite: bool = doc.get("overwrite")?;
            doc.set("overwrite", !overwrite)?;
            require_table(lua, "core")?.call_function::<()>("blink_reset", ())?;
            Ok(())
        })?,
    )?;
    add_command(
        &commands,
        "doc:save-as",
        lua.create_function(|lua, dv: LuaTable| {
            let doc: LuaTable = dv.get("doc")?;
            let core = require_table(lua, "core")?;
            let status_view: LuaTable = core.get("status_view")?;
            let mut text = doc.get::<Option<String>>("filename")?;
            if text.is_none() {
                if let Some(last_active) = core.get::<Option<LuaTable>>("last_active_view")? {
                    let last_doc: LuaTable = last_active.get("doc")?;
                    if let Some(abs) = last_doc.get::<Option<String>>("abs_filename")? {
                        if let Some(project) = core.call_function::<Option<LuaTable>>("root_project", ())? {
                            if let Some((dirname, _)) = abs.rsplit_once(['/', '\\']) {
                                let normalized: String =
                                    project.call_method("normalize_path", dirname.to_string())?;
                                let project_path: String = project.get("path")?;
                                let pathsep: String = lua.globals().get("PATHSEP")?;
                                text = Some(if normalized == project_path {
                                    String::new()
                                } else {
                                    format!("{normalized}{pathsep}")
                                });
                            }
                        }
                    }
                }
            }

            let command_view: LuaTable = core.get("command_view")?;
            let doc_for_suggest = doc.clone();
            command_view.call_method::<()>(
                "enter",
                (
                    "Save As",
                    {
                        let spec = lua.create_table()?;
                        if let Some(text) = text {
                            spec.set("text", text)?;
                        }
                        spec.set(
                            "suggest",
                            lua.create_function(move |lua, input: String| -> LuaResult<LuaValue> {
                                let common = require_table(lua, "core.common")?;
                                let core = require_table(lua, "core")?;
                                let project = core.call_function::<Option<LuaTable>>("root_project", ())?;
                                let home_expand: String = common.call_function("home_expand", input.clone())?;
                                let abs = if let Some(project) = project {
                                    project.call_method::<String>(
                                        "absolute_path",
                                        project.call_method::<String>("normalize_path", home_expand.clone())?,
                                    )?
                                } else {
                                    lua.globals()
                                        .get::<LuaTable>("system")?
                                        .call_function::<String>("absolute_path", home_expand.clone())?
                                };
                                status_view.call_method::<()>(
                                    "show_tooltip",
                                    format!(
                                        "{} -> {}",
                                        doc_for_suggest.call_method::<String>("get_name", ())?,
                                        common.call_function::<String>("home_encode", abs)?
                                    ),
                                )?;
                                let suggestions: LuaValue =
                                    common.call_function("path_suggest", home_expand)?;
                                common.call_function("home_encode_list", suggestions)
                            })?,
                        )?;
                        spec.set(
                            "submit",
                            lua.create_function(move |lua, filename: String| {
                                require_table(lua, "core")?
                                    .get::<LuaTable>("status_view")?
                                    .call_method::<()>("remove_tooltip", ())?;
                                let common = require_table(lua, "core.common")?;
                                save_with_filename(
                                    lua,
                                    Some(common.call_function("home_expand", filename)?),
                                )
                            })?,
                        )?;
                        spec.set(
                            "cancel",
                            lua.create_function(move |lua, ()| {
                                require_table(lua, "core")?
                                    .get::<LuaTable>("status_view")?
                                    .call_method::<()>("remove_tooltip", ())?;
                                Ok(())
                            })?,
                        )?;
                        spec
                    },
                ),
            )
        })?,
    )?;
    add_command(
        &commands,
        "doc:save",
        lua.create_function(|lua, dv: LuaTable| {
            let doc: LuaTable = dv.get("doc")?;
            if doc.get::<Option<String>>("filename")?.is_some() {
                save_with_filename(lua, None)
            } else {
                require_table(lua, "core.command")?.call_function::<bool>("perform", "doc:save-as")?;
                Ok(())
            }
        })?,
    )?;
    add_command(
        &commands,
        "doc:reload",
        lua.create_function(|_, dv: LuaTable| dv.get::<LuaTable>("doc")?.call_method::<()>("reload", ()))?,
    )?;
    add_command(
        &commands,
        "file:rename",
        lua.create_function(|lua, dv: LuaTable| {
            let core = require_table(lua, "core")?;
            let status_view: LuaTable = core.get("status_view")?;
            let doc: LuaTable = dv.get("doc")?;
            let Some(old_filename) = doc.get::<Option<String>>("filename")? else {
                core.call_function::<()>("error", "Cannot rename unsaved doc")?;
                return Ok(());
            };
            let old_filename_submit = old_filename.clone();
            let old_filename_suggest = old_filename.clone();
            let command_view: LuaTable = core.get("command_view")?;
            command_view.call_method::<()>(
                "enter",
                (
                    "Rename",
                    {
                        let spec = lua.create_table()?;
                        spec.set("text", old_filename.clone())?;
                        spec.set(
                            "suggest",
                            lua.create_function(move |lua, input: String| -> LuaResult<LuaValue> {
                                let common = require_table(lua, "core.common")?;
                                let core = require_table(lua, "core")?;
                                let target: String = common.call_function("home_expand", input.clone())?;
                                let project = core.call_function::<Option<LuaTable>>("root_project", ())?;
                                let abs = if let Some(project) = project {
                                    project.call_method::<String>(
                                        "absolute_path",
                                        project.call_method::<String>("normalize_path", target.clone())?,
                                    )?
                                } else {
                                    lua.globals()
                                        .get::<LuaTable>("system")?
                                        .call_function::<String>("absolute_path", target.clone())?
                                };
                                status_view.call_method::<()>(
                                    "show_tooltip",
                                    format!(
                                        "{} -> {}",
                                        old_filename_suggest,
                                        common.call_function::<String>("home_encode", abs)?
                                    ),
                                )?;
                                common.call_function(
                                    "home_encode_list",
                                    common.call_function::<LuaValue>("path_suggest", target)?,
                                )
                            })?,
                        )?;
                        spec.set(
                            "submit",
                            lua.create_function(move |lua, filename: String| {
                                require_table(lua, "core")?
                                    .get::<LuaTable>("status_view")?
                                    .call_method::<()>("remove_tooltip", ())?;
                                let common = require_table(lua, "core.common")?;
                                let expanded: String = common.call_function("home_expand", filename.clone())?;
                                save_with_filename(lua, Some(expanded.clone()))?;
                                require_table(lua, "core")?.call_function::<()>(
                                    "log",
                                    format!("Renamed \"{old_filename_submit}\" to \"{expanded}\""),
                                )?;
                                if expanded != old_filename_submit {
                                    let os: LuaTable = lua.globals().get("os")?;
                                    let _ = os.call_function::<LuaMultiValue>("remove", old_filename_submit.clone())?;
                                }
                                Ok(())
                            })?,
                        )?;
                        spec.set(
                            "cancel",
                            lua.create_function(move |lua, ()| {
                                require_table(lua, "core")?
                                    .get::<LuaTable>("status_view")?
                                    .call_method::<()>("remove_tooltip", ())?;
                                Ok(())
                            })?,
                        )?;
                        spec
                    },
                ),
            )
        })?,
    )?;
    add_command(
        &commands,
        "file:delete",
        lua.create_function(|lua, dv: LuaTable| {
            let core = require_table(lua, "core")?;
            let doc: LuaTable = dv.get("doc")?;
            let Some(filename) = doc.get::<Option<String>>("abs_filename")? else {
                core.call_function::<()>("error", "Cannot remove unsaved doc")?;
                return Ok(());
            };
            let views: LuaTable = core.call_function("get_views_referencing_doc", doc.clone())?;
            let root_view: LuaTable = core.get("root_view")?;
            let root_node: LuaTable = root_view.get("root_node")?;
            for view in views.sequence_values::<LuaTable>() {
                let view = view?;
                let node: LuaTable = root_node.call_method("get_node_for_view", view.clone())?;
                node.call_method::<()>("close_view", (root_node.clone(), view))?;
            }
            let os: LuaTable = lua.globals().get("os")?;
            let _ = os.call_function::<LuaMultiValue>("remove", filename.clone())?;
            core.call_function::<()>("log", format!("Removed \"{filename}\""))?;
            Ok(())
        })?,
    )?;
    add_command(
        &commands,
        "doc:select-to-cursor",
        lua.create_function(|lua, (dv, x, y, _clicks): (LuaTable, f64, f64, Option<i64>)| {
            let doc: LuaTable = dv.get("doc")?;
            let (_, _, line1, col1): (usize, usize, usize, usize) =
                doc.call_method("get_selection", ())?;
            let (line2, col2): (usize, usize) = dv.call_method("resolve_screen_position", (x, y))?;
            let mouse_selecting = lua.create_table()?;
            mouse_selecting.set(1, line1)?;
            mouse_selecting.set(2, col1)?;
            mouse_selecting.set(3, LuaValue::Nil)?;
            dv.set("mouse_selecting", mouse_selecting)?;
            doc.call_method::<()>("set_selection", (line2, col2, line1, col1))?;
            set_primary_selection(lua, &doc)?;
            Ok(())
        })?,
    )?;
    add_command(
        &commands,
        "doc:create-cursor-previous-line",
        lua.create_function(|lua, dv: LuaTable| {
            let doc: LuaTable = dv.get("doc")?;
            split_cursor(lua, &dv, -1)?;
            doc.call_method::<()>("merge_cursors", ())?;
            Ok(())
        })?,
    )?;
    add_command(
        &commands,
        "doc:create-cursor-next-line",
        lua.create_function(|lua, dv: LuaTable| {
            let doc: LuaTable = dv.get("doc")?;
            split_cursor(lua, &dv, 1)?;
            doc.call_method::<()>("merge_cursors", ())?;
            Ok(())
        })?,
    )?;

    for name in [
        "previous-char",
        "next-char",
        "previous-word-start",
        "next-word-end",
        "previous-block-start",
        "next-block-end",
        "start-of-doc",
        "end-of-doc",
        "start-of-line",
        "end-of-line",
        "start-of-word",
        "start-of-indentation",
        "end-of-word",
    ] {
        register_translate_command_set(lua, &commands, name, "core.doc.translate")?;
    }
    for name in ["previous-line", "next-line", "previous-page", "next-page"] {
        register_docview_translate_command_set(lua, &commands, name)?;
    }

    add_command(
        &commands,
        "doc:move-to-previous-char",
        lua.create_function(|lua, dv: LuaTable| {
            let doc: LuaTable = dv.get("doc")?;
            let translate = require_table(lua, "core.doc.translate")?;
            let previous_char: LuaFunction = translate.get("previous_char")?;
            for (idx, line1, col1, line2, col2) in selection_ranges(&doc, true, false)? {
                if line1 != line2 || col1 != col2 {
                    doc.call_method::<()>("set_selections", (idx, line1, col1))?;
                } else {
                    doc.call_method::<()>("move_to_cursor", (idx, previous_char.clone()))?;
                }
            }
            doc.call_method::<()>("merge_cursors", ())?;
            Ok(())
        })?,
    )?;
    add_command(
        &commands,
        "doc:move-to-next-char",
        lua.create_function(|lua, dv: LuaTable| {
            let doc: LuaTable = dv.get("doc")?;
            let translate = require_table(lua, "core.doc.translate")?;
            let next_char: LuaFunction = translate.get("next_char")?;
            for (idx, line1, col1, line2, col2) in selection_ranges(&doc, true, false)? {
                if line1 != line2 || col1 != col2 {
                    doc.call_method::<()>("set_selections", (idx, line2, col2))?;
                } else {
                    doc.call_method::<()>("move_to_cursor", (idx, next_char.clone()))?;
                }
            }
            doc.call_method::<()>("merge_cursors", ())?;
            Ok(())
        })?,
    )?;

    command.call_function::<()>("add", ("core.docview", commands))?;

    let mouse_predicate = lua.create_function(|lua, (x, y): (Option<f64>, Option<f64>)| {
        let (Some(x), Some(y)) = (x, y) else {
            return Ok(LuaMultiValue::from_vec(vec![LuaValue::Boolean(false)]));
        };
        let core = require_table(lua, "core")?;
        let active_view: LuaTable = core.get("active_view")?;
        let docview = require_table(lua, "core.docview")?;
        if !active_view.call_method::<bool>("extends", docview)? {
            return Ok(LuaMultiValue::from_vec(vec![LuaValue::Boolean(false)]));
        }
        let x1 = active_view.get::<LuaTable>("position")?.get::<f64>("x")?;
        let y1 = active_view.get::<LuaTable>("position")?.get::<f64>("y")?;
        let x2 = x1 + active_view.get::<LuaTable>("size")?.get::<f64>("x")?;
        let y2 = y1 + active_view.get::<LuaTable>("size")?.get::<f64>("y")?;
        let gutter: f64 = active_view.call_method("get_gutter_width", ())?;
        let ok = x >= x1 + gutter && x < x2 && y >= y1 && y < y2;
        let mut out = LuaMultiValue::new();
        out.push_back(LuaValue::Boolean(ok));
        if ok {
            out.push_back(LuaValue::Table(active_view));
            out.push_back(LuaValue::Number(x));
            out.push_back(LuaValue::Number(y));
        }
        Ok(out)
    })?;

    let mouse_commands = lua.create_table()?;
    add_command(
        &mouse_commands,
        "doc:set-cursor",
        lua.create_function(|lua, (dv, x, y): (LuaTable, f64, f64)| set_cursor(lua, &dv, x, y, "set"))?,
    )?;
    add_command(
        &mouse_commands,
        "doc:set-cursor-word",
        lua.create_function(|lua, (dv, x, y): (LuaTable, f64, f64)| set_cursor(lua, &dv, x, y, "word"))?,
    )?;
    add_command(
        &mouse_commands,
        "doc:set-cursor-line",
        lua.create_function(|lua, (dv, x, y): (LuaTable, f64, f64)| set_cursor(lua, &dv, x, y, "lines"))?,
    )?;
    add_command(
        &mouse_commands,
        "doc:split-cursor",
        lua.create_function(|lua, (dv, x, y): (LuaTable, f64, f64)| {
            let doc: LuaTable = dv.get("doc")?;
            let (line, col): (usize, usize) = dv.call_method("resolve_screen_position", (x, y))?;
            let mut removal_target = None;
            for (idx, line1, col1, _, _) in selection_ranges(&doc, true, false)? {
                if line1 == line && col1 == col && selection_count(&doc)? > 1 {
                    removal_target = Some(idx);
                }
            }
            if let Some(idx) = removal_target {
                doc.call_method::<()>("remove_selection", idx)?;
            } else {
                doc.call_method::<()>("add_selection", (line, col, line, col))?;
            }
            let mouse_selecting = lua.create_table()?;
            mouse_selecting.set(1, line)?;
            mouse_selecting.set(2, col)?;
            mouse_selecting.set(3, "set")?;
            dv.set("mouse_selecting", mouse_selecting)?;
            Ok(())
        })?,
    )?;
    command.call_function::<()>("add", (mouse_predicate, mouse_commands))?;

    Ok(())
}

pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let package: LuaTable = lua.globals().get("package")?;
    let preload: LuaTable = package.get("preload")?;
    preload.set(
        "core.commands.doc",
        lua.create_function(|lua, ()| {
            register_commands(lua)?;
            Ok(LuaValue::Boolean(true))
        })?,
    )?;
    Ok(())
}
