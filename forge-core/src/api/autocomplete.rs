use mlua::prelude::*;
use std::sync::Arc;

fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

fn make_weak_table(lua: &Lua) -> LuaResult<LuaTable> {
    let t = lua.create_table()?;
    let mt = lua.create_table()?;
    mt.set("__mode", "k")?;
    t.set_metatable(Some(mt))?;
    Ok(t)
}

fn font_get_height(font: &LuaValue) -> LuaResult<f64> {
    match font {
        LuaValue::Table(t) => t.call_method("get_height", ()),
        LuaValue::UserData(ud) => ud.call_method("get_height", ()),
        _ => Err(LuaError::RuntimeError("expected font".into())),
    }
}

fn font_get_width(font: &LuaValue, text: &str) -> LuaResult<f64> {
    match font {
        LuaValue::Table(t) => t.call_method("get_width", text.to_owned()),
        LuaValue::UserData(ud) => ud.call_method("get_width", text.to_owned()),
        _ => Err(LuaError::RuntimeError("expected font".into())),
    }
}

/// Returns `config.plugins.autocomplete.<key>` as the given type, or a default.
fn ac_config_get<T: FromLua>(lua: &Lua, key: &str) -> LuaResult<T> {
    let config: LuaTable = require_table(lua, "core.config")?;
    let plugins: LuaTable = config.get("plugins")?;
    let ac: LuaTable = plugins.get("autocomplete")?;
    ac.get(key)
}

fn current_mode(lua: &Lua) -> LuaResult<String> {
    ac_config_get::<Option<String>>(lua, "mode")
        .map(|o| o.unwrap_or_else(|| "off".to_owned()))
}

fn suggestion_scope(lua: &Lua) -> LuaResult<Option<String>> {
    let mode = current_mode(lua)?;
    Ok(match mode.as_str() {
        "in_document" => Some("local".to_owned()),
        "totally_on" => Some("global".to_owned()),
        _ => None,
    })
}

/// Returns the active view if it is a DocView.
fn get_active_docview(lua: &Lua) -> LuaResult<Option<LuaTable>> {
    let core: LuaTable = require_table(lua, "core")?;
    let av: LuaValue = core.get("active_view")?;
    if let LuaValue::Table(v) = av {
        let has_doc: bool = !matches!(v.get::<LuaValue>("doc")?, LuaValue::Nil);
        if has_doc {
            return Ok(Some(v));
        }
    }
    Ok(None)
}

fn get_partial_symbol(lua: &Lua) -> LuaResult<String> {
    let core: LuaTable = require_table(lua, "core")?;
    let av: LuaTable = core.get("active_view")?;
    let doc: LuaTable = av.get("doc")?;
    let translate: LuaTable = require_table(lua, "core.doc.translate")?;
    let (line2, col2): (i64, i64) = doc.call_method("get_selection", ())?;
    let start_of_word: LuaFunction = translate.get("start_of_word")?;
    let (line1, col1): (i64, i64) =
        doc.call_method("position_offset", (line2, col2, start_of_word))?;
    doc.call_method("get_text", (line1, col1, line2, col2))
}

fn reset_suggestions(lua: &Lua, state: &LuaTable, ac: &LuaTable) -> LuaResult<()> {
    state.set("suggestions_offset", 1i64)?;
    state.set("suggestions_idx", 1i64)?;
    state.set("suggestions", lua.create_table()?)?;
    state.set("provider_items", LuaValue::Nil)?;
    let req_id: i64 = state.get("provider_request_id")?;
    state.set("provider_request_id", req_id + 1)?;
    state.set("triggered_manually", false)?;

    let on_close: LuaValue = ac.get("on_close")?;
    if let LuaValue::Function(f) = on_close {
        let core: LuaTable = require_table(lua, "core")?;
        let av: LuaValue = core.get("active_view")?;
        let doc: LuaValue = if let LuaValue::Table(v) = &av { v.get("doc")? } else { LuaValue::Nil };
        let suggestions: LuaTable = state.get("suggestions")?;
        let idx: i64 = state.get("suggestions_idx")?;
        let item: LuaValue = suggestions.get(idx)?;
        f.call::<()>((doc, item))?;
        ac.set("on_close", LuaValue::Nil)?;
    }
    Ok(())
}

/// Convert a completion map item to a suggestion entry table.
fn item_from_entry(lua: &Lua, text: &str, info: LuaValue) -> LuaResult<LuaTable> {
    let mt = lua.create_table()?;
    let text_str = lua.create_string(text)?;
    mt.set("__tostring", lua.create_function(|_lua, t: LuaTable| {
        t.get::<String>("text")
    })?)?;

    let item = lua.create_table()?;
    item.set("text", text_str.clone())?;
    item.set_metatable(Some(mt))?;

    match &info {
        LuaValue::Table(info_t) => {
            let info_str: Option<String> = info_t.get("info")?;
            item.set("info", info_str)?;
            let icon: LuaValue = info_t.get("icon")?;
            item.set("icon", icon)?;
            let desc: LuaValue = info_t.get("desc")?;
            item.set("desc", desc)?;
            let onhover: LuaValue = info_t.get("onhover")?;
            item.set("onhover", onhover)?;
            let onselect: LuaValue = info_t.get("onselect")?;
            item.set("onselect", onselect)?;
            let data: LuaValue = info_t.get("data")?;
            item.set("data", data)?;
        }
        LuaValue::Nil => {}
        _ => {
            // info is a plain value — use as info string
            let s = match &info {
                LuaValue::String(s) => Some(s.to_str()?.to_owned()),
                LuaValue::Integer(n) => Some(n.to_string()),
                LuaValue::Number(n) => Some(n.to_string()),
                _ => None,
            };
            item.set("info", s)?;
        }
    }
    Ok(item)
}

fn items_from_completion_map(lua: &Lua, t: &LuaTable) -> LuaResult<LuaTable> {
    let items_map: LuaTable = t.get("items")?;
    let out = lua.create_table()?;
    for pair in items_map.pairs::<LuaValue, LuaValue>() {
        let (key, value) = pair?;
        let text = match &key {
            LuaValue::String(s) => s.to_str()?.to_owned(),
            LuaValue::Integer(n) => n.to_string(),
            LuaValue::Number(n) => n.to_string(),
            _ => continue,
        };
        let item = item_from_entry(lua, &text, value)?;
        out.push(item)?;
    }
    Ok(out)
}

fn update_suggestions(lua: &Lua, state: &LuaTable, ac: &LuaTable) -> LuaResult<()> {
    let core: LuaTable = require_table(lua, "core")?;
    let av: LuaTable = core.get("active_view")?;
    let doc: LuaTable = av.get("doc")?;
    let filename: String = doc.get("filename").unwrap_or_default();

    let triggered_manually: bool = state.get("triggered_manually")?;
    let map: LuaTable = if triggered_manually {
        ac.get("map_manually")?
    } else {
        ac.get("map")?
    };

    let common: LuaTable = require_table(lua, "core.common")?;
    let provider_items: LuaValue = state.get("provider_items")?;
    let partial: String = state.get("partial")?;

    let mut items: Vec<LuaTable> = Vec::new();
    let mut assigned_sym: Vec<String> = Vec::new();

    if let LuaValue::Table(pi) = &provider_items {
        for entry in pi.sequence_values::<LuaTable>() {
            let e = entry?;
            let text: String = e.get("text")?;
            assigned_sym.push(text);
            items.push(e);
        }
    } else {
        for pair in map.pairs::<LuaValue, LuaTable>() {
            let (_, v) = pair?;
            let files: String = v.get("files").unwrap_or_else(|_| ".*".to_owned());
            let matched: bool = common.call_function("match_pattern", (filename.clone(), files))?;
            if matched {
                let v_items: LuaTable = v.get("items")?;
                for entry in v_items.sequence_values::<LuaTable>() {
                    let e = entry?;
                    let text: String = e.get("text")?;
                    assigned_sym.push(text);
                    items.push(e);
                }
            }
        }
    }

    let scope = suggestion_scope(lua)?;
    if !triggered_manually && matches!(provider_items, LuaValue::Nil) {
        if let Some(scope_str) = &scope {
            let cache: LuaTable = state.get("cache")?;
            let symbol_index: LuaTable = require_table(lua, "symbol_index")?;

            match scope_str.as_str() {
                "global" => {
                    let docs: LuaTable = core.get("docs")?;
                    let mut doc_ids: Vec<i64> = Vec::new();
                    for d in docs.sequence_values::<LuaTable>() {
                        let d = d?;
                        let entry: LuaValue = cache.raw_get(d)?;
                        if let LuaValue::Table(ce) = entry {
                            let syms: LuaValue = ce.get("symbols")?;
                            if matches!(syms, LuaValue::Boolean(true)) {
                                let doc_id: i64 = ce.get("doc_id")?;
                                doc_ids.push(doc_id);
                            }
                        }
                    }
                    let doc_ids_tbl = lua.create_sequence_from(doc_ids.iter().copied())?;
                    let text_symbols: LuaTable = symbol_index.call_function("collect", doc_ids_tbl)?;
                    for sym in text_symbols.sequence_values::<String>() {
                        let sym = sym?;
                        if !assigned_sym.contains(&sym) {
                            let item = item_from_entry(lua, &sym, LuaValue::String(lua.create_string("normal")?))?;
                            items.push(item);
                        }
                    }
                }
                "local" => {
                    let entry: LuaValue = cache.raw_get(doc.clone())?;
                    if let LuaValue::Table(ce) = entry {
                        let syms: LuaValue = ce.get("symbols")?;
                        match syms {
                            LuaValue::Boolean(true) => {
                                let doc_id: i64 = ce.get("doc_id")?;
                                let syms: LuaTable = symbol_index.call_function("get_doc_symbols", doc_id)?;
                                for sym in syms.sequence_values::<String>() {
                                    let sym = sym?;
                                    if !assigned_sym.contains(&sym) {
                                        let item = item_from_entry(lua, &sym, LuaValue::String(lua.create_string("normal")?))?;
                                        items.push(item);
                                    }
                                }
                            }
                            LuaValue::Table(sym_t) => {
                                for pair in sym_t.pairs::<String, LuaValue>() {
                                    let (sym, _) = pair?;
                                    if !assigned_sym.contains(&sym) {
                                        let item = item_from_entry(lua, &sym, LuaValue::String(lua.create_string("normal")?))?;
                                        items.push(item);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // Fuzzy match items against partial
    let items_tbl = lua.create_table()?;
    for item in &items {
        items_tbl.push(item.clone())?;
    }
    let matched: LuaTable = common.call_function("fuzzy_match", (items_tbl, partial))?;

    let max_suggestions: i64 = ac_config_get(lua, "max_suggestions")?;
    let suggestions = lua.create_table()?;
    let mut j = 1i64;
    for i in 1..=max_suggestions {
        let item: LuaValue = matched.get(j)?;
        if matches!(item, LuaValue::Nil) { break; }
        let item: LuaTable = match item { LuaValue::Table(t) => t, _ => break };
        let item_text: String = item.get("text")?;
        let merged = lua.create_table()?;
        // Copy fields from item
        for pair in item.pairs::<LuaValue, LuaValue>() {
            let (k, v) = pair?;
            merged.set(k, v)?;
        }
        // Merge duplicates' info
        j += 1;
        loop {
            let next: LuaValue = matched.get(j)?;
            if matches!(next, LuaValue::Nil) { break; }
            if let LuaValue::Table(next_t) = next {
                let next_text: String = next_t.get("text")?;
                if next_text != item_text { break; }
                let merged_info: LuaValue = merged.get("info")?;
                if matches!(merged_info, LuaValue::Nil) {
                    let next_info: LuaValue = next_t.get("info")?;
                    merged.set("info", next_info)?;
                }
                j += 1;
            } else {
                break;
            }
        }
        suggestions.set(i, merged)?;
    }

    state.set("suggestions", suggestions)?;
    state.set("suggestions_idx", 1i64)?;
    state.set("suggestions_offset", 1i64)?;
    Ok(())
}

fn show_autocomplete(lua: &Lua, state: &LuaTable, ac: &LuaTable) -> LuaResult<()> {
    let av = match get_active_docview(lua)? {
        Some(v) => v,
        None => return Ok(()),
    };

    let triggered_manually: bool = state.get("triggered_manually")?;
    if !triggered_manually && current_mode(lua)?.as_str() == "off" {
        reset_suggestions(lua, state, ac)?;
        return Ok(());
    }

    let partial = get_partial_symbol(lua)?;
    state.set("partial", partial.clone())?;

    let min_len: i64 = ac_config_get(lua, "min_len")?;
    if (partial.len() as i64) >= min_len || triggered_manually {
        let mode = current_mode(lua)?;
        let providers: LuaTable = ac.get("providers")?;
        let provider: LuaValue = if !triggered_manually {
            providers.get(mode.clone())?
        } else {
            LuaValue::Nil
        };

        if let LuaValue::Function(pf) = provider {
            state.set("provider_items", LuaValue::Nil)?;
            let req_id: i64 = state.get("provider_request_id")?;
            let new_req_id = req_id + 1;
            state.set("provider_request_id", new_req_id)?;

            let doc: LuaTable = av.get("doc")?;
            let (line, col): (i64, i64) = doc.call_method("get_selection", ())?;
            let change_id: LuaValue = doc.call_method("get_change_id", ())?;
            let request_partial = partial.clone();

            let state_key = lua.create_registry_value(state.clone())?;
            let ac_key = lua.create_registry_value(ac.clone())?;
            let doc_key = lua.create_registry_value(doc)?;

            let respond = lua.create_function(move |lua, completions: LuaValue| {
                let state: LuaTable = lua.registry_value(&state_key)?;
                let ac: LuaTable = lua.registry_value(&ac_key)?;
                let doc: LuaTable = lua.registry_value(&doc_key)?;

                let cur_req_id: i64 = state.get("provider_request_id")?;
                if cur_req_id != new_req_id { return Ok(()); }

                let av = match get_active_docview(lua)? {
                    Some(v) => v,
                    None => return Ok(()),
                };
                let av_doc: LuaTable = av.get("doc")?;
                if av_doc.to_pointer() != doc.to_pointer() { return Ok(()); }

                let (cur_line, cur_col): (i64, i64) = doc.call_method("get_selection", ())?;
                if cur_line != line || cur_col != col { return Ok(()); }

                let cur_change_id: LuaValue = doc.call_method("get_change_id", ())?;
                // Compare change IDs — if they differ, skip
                let change_mismatch = match (&change_id, &cur_change_id) {
                    (LuaValue::Integer(a), LuaValue::Integer(b)) => a != b,
                    _ => false,
                };
                if change_mismatch { return Ok(()); }

                let cur_partial = get_partial_symbol(lua)?;
                if cur_partial != request_partial { return Ok(()); }

                let new_items = match completions {
                    LuaValue::Table(t) => {
                        let items = items_from_completion_map(lua, &t)?;
                        LuaValue::Table(items)
                    }
                    _ => LuaValue::Table(lua.create_table()?),
                };
                state.set("provider_items", new_items)?;
                update_suggestions(lua, &state, &ac)
            })?;

            let ctx = lua.create_table()?;
            ctx.set("doc", av.get::<LuaValue>("doc")?)?;
            ctx.set("line", line)?;
            ctx.set("col", col)?;
            ctx.set("partial", partial.clone())?;
            ctx.set("manually", triggered_manually)?;

            let completions: LuaValue = pf.call((ctx, respond.clone()))?;
            if !matches!(completions, LuaValue::Nil) {
                respond.call::<()>(completions)?;
            }
        } else {
            update_suggestions(lua, state, ac)?;
        }

        if !triggered_manually {
            let doc: LuaTable = av.get("doc")?;
            let (line, col): (i64, i64) = doc.call_method("get_selection", ())?;
            state.set("last_line", line)?;
            state.set("last_col", col)?;
        } else {
            let doc: LuaTable = av.get("doc")?;
            let (line, col): (i64, i64) = doc.call_method("get_selection", ())?;
            let last_line: LuaValue = state.get("last_line")?;
            let last_col: LuaValue = state.get("last_col")?;
            let last_col_i: i64 = match &last_col { LuaValue::Integer(n) => *n, _ => col };
            if line != match &last_line { LuaValue::Integer(n) => *n, _ => line } {
                reset_suggestions(lua, state, ac)?;
                return Ok(());
            }
            let char_before: String = doc.call_method("get_char", (line, col - 1, line, col - 1))?;
            let is_space = char_before.chars().next().map(|c| c.is_whitespace()).unwrap_or(false);
            let is_punct_moved = char_before.chars().next().map(|c| c.is_ascii_punctuation()).unwrap_or(false)
                && col != last_col_i;
            if is_space || is_punct_moved {
                reset_suggestions(lua, state, ac)?;
                return Ok(());
            }
        }

        // Scroll if suggestions rect is out of view
        let drawing: LuaTable = require_table(lua, "plugins.autocomplete.drawing")?;
        let ctx = make_ctx(lua, state, ac)?;
        let rect: LuaMultiValue = drawing.call_function("get_suggestions_rect", (ctx, av.clone()))?;
        let mut r_iter = rect.into_iter();
        let _rx: f64 = match r_iter.next() { Some(LuaValue::Number(n)) => n, _ => return Ok(()) };
        let ry: f64 = match r_iter.next() { Some(LuaValue::Number(n)) => n, _ => return Ok(()) };
        let _rw: f64 = match r_iter.next() { Some(LuaValue::Number(n)) => n, _ => return Ok(()) };
        let rh: f64 = match r_iter.next() { Some(LuaValue::Number(n)) => n, _ => return Ok(()) };

        let position: LuaTable = av.get("position")?;
        let pos_y: f64 = position.get("y")?;
        let size: LuaTable = av.get("size")?;
        let size_y: f64 = size.get("y")?;
        let limit = pos_y + size_y;
        if ry + rh > limit {
            let scroll: LuaTable = av.get("scroll")?;
            let scroll_to: LuaTable = scroll.get("to")?;
            let cur_y: f64 = scroll.get("y")?;
            scroll_to.set("y", cur_y + ry + rh - limit)?;
        }
    } else {
        reset_suggestions(lua, state, ac)?;
    }
    Ok(())
}

fn make_ctx(lua: &Lua, state: &LuaTable, ac: &LuaTable) -> LuaResult<LuaTable> {
    let ctx = lua.create_table()?;
    let suggestions: LuaValue = state.get("suggestions")?;
    let suggestions_idx: i64 = state.get("suggestions_idx")?;
    let suggestions_offset: i64 = state.get("suggestions_offset")?;
    let partial: String = state.get("partial")?;
    let icons: LuaValue = ac.get("icons")?;
    ctx.set("suggestions", suggestions)?;
    ctx.set("suggestions_idx", suggestions_idx)?;
    ctx.set("suggestions_offset", suggestions_offset)?;
    ctx.set("partial", partial)?;
    ctx.set("icons", icons)?;
    Ok(ctx)
}

fn set_config_defaults(lua: &Lua) -> LuaResult<()> {
    let config: LuaTable = require_table(lua, "core.config")?;
    let plugins: LuaTable = config.get("plugins")?;
    let user_ac: LuaValue = plugins.get("autocomplete")?;
    let user_mode = if let LuaValue::Table(ref t) = user_ac {
        t.get::<Option<String>>("mode")?
    } else {
        None
    };

    let common: LuaTable = require_table(lua, "core.common")?;

    let defaults = lua.create_table()?;
    defaults.set("mode", user_mode.as_deref().unwrap_or("lsp"))?;
    defaults.set("min_len", 3i64)?;
    defaults.set("max_height", 6i64)?;
    defaults.set("max_suggestions", 100i64)?;
    defaults.set("max_symbols", 4000i64)?;
    defaults.set("suggestions_scope", "global")?;
    defaults.set("desc_font_size", 12i64)?;
    defaults.set("hide_icons", false)?;
    defaults.set("icon_position", "left")?;
    defaults.set("hide_info", false)?;

    let spec = lua.create_table()?;
    spec.set("name", "Autocomplete")?;

    let add_entry = |spec: &LuaTable, label: &str, desc: &str, path: &str, typ: &str| -> LuaResult<()> {
        let e = lua.create_table()?;
        e.set("label", label)?;
        e.set("description", desc)?;
        e.set("path", path)?;
        e.set("type", typ)?;
        spec.push(e)?;
        Ok(())
    };

    {
        let e = lua.create_table()?;
        e.set("label", "Mode")?;
        e.set("description", "Choose whether autocomplete uses document symbols, LSP, all known symbols, or stays off.")?;
        e.set("path", "mode")?;
        e.set("type", "selection")?;
        e.set("default", user_mode.as_deref().unwrap_or("lsp"))?;
        let vals = lua.create_table()?;
        vals.push(lua.create_sequence_from(["Off", "off"])?)?;
        vals.push(lua.create_sequence_from(["In Document", "in_document"])?)?;
        vals.push(lua.create_sequence_from(["Via LSP", "lsp"])?)?;
        vals.push(lua.create_sequence_from(["Totally On", "totally_on"])?)?;
        e.set("values", vals)?;
        spec.push(e)?;
    }

    {
        let e = lua.create_table()?;
        e.set("label", "Minimum Length")?;
        e.set("description", "Amount of characters that need to be written for autocomplete to popup.")?;
        e.set("path", "min_len")?;
        e.set("type", "number")?;
        e.set("default", 3i64)?;
        e.set("min", 1i64)?;
        e.set("max", 5i64)?;
        spec.push(e)?;
    }

    add_entry(&spec, "Maximum Height", "The maximum amount of visible items.", "max_height", "number")?;
    add_entry(&spec, "Maximum Suggestions", "The maximum amount of scrollable items.", "max_suggestions", "number")?;
    add_entry(&spec, "Maximum Symbols", "Maximum amount of symbols to cache per document.", "max_symbols", "number")?;
    add_entry(&spec, "Description Font Size", "Font size of the description box.", "desc_font_size", "number")?;
    add_entry(&spec, "Hide Icons", "Do not show icons on the suggestions list.", "hide_icons", "toggle")?;

    {
        let e = lua.create_table()?;
        e.set("label", "Icons Position")?;
        e.set("description", "Position to display icons on the suggestions list.")?;
        e.set("path", "icon_position")?;
        e.set("type", "selection")?;
        e.set("default", "left")?;
        let vals = lua.create_table()?;
        vals.push(lua.create_sequence_from(["Left", "left"])?)?;
        vals.push(lua.create_sequence_from(["Right", "Right"])?)?;
        e.set("values", vals)?;
        spec.push(e)?;
    }

    add_entry(&spec, "Hide Items Info", "Do not show the additional info related to each suggestion.", "hide_info", "toggle")?;

    defaults.set("config_spec", spec)?;

    let merged: LuaTable = common.call_function("merge", (defaults, user_ac))?;
    plugins.set("autocomplete", merged)?;
    Ok(())
}

fn add_thread(lua: &Lua, state_key: Arc<LuaRegistryKey>, ac_key: Arc<LuaRegistryKey>) -> LuaResult<()> {
    let core: LuaTable = require_table(lua, "core")?;
    // One tick: scan all docs for stale caches, update symbols.
    // Returns 0.0 when work was done (resume soon) or 1.0 when all caches are
    // fresh (sleep 1 s before next scan).
    // coroutine.yield cannot be called from a Rust C function, so the
    // loop+yield live in a thin Lua wrapper below.
    let tick = lua.create_function(move |lua, (): ()| -> LuaResult<f64> {
        let symbol_index: LuaTable = require_table(lua, "symbol_index")?;
        let state: LuaTable = lua.registry_value(&state_key)?;
        let ac: LuaTable = lua.registry_value(&ac_key)?;
        let core: LuaTable = require_table(lua, "core")?;
        let docs: LuaTable = core.get("docs")?;
        let cache: LuaTable = state.get("cache")?;
        let mut did_work = false;

        for doc in docs.sequence_values::<LuaTable>() {
            let doc = doc?;
            let entry: LuaValue = cache.raw_get(doc.clone())?;
            let change_id: LuaValue = doc.call_method("get_change_id", ())?;
            let cache_valid = if let LuaValue::Table(ce) = &entry {
                let cached_id: LuaValue = ce.get("last_change_id")?;
                match (&cached_id, &change_id) {
                    (LuaValue::Integer(a), LuaValue::Integer(b)) => a == b,
                    _ => false,
                }
            } else {
                false
            };

            if !cache_valid {
                did_work = true;
                let doc_id: i64 = if let LuaValue::Table(ce) = &entry {
                    ce.get("doc_id")?
                } else {
                    let id: i64 = state.get("next_doc_id")?;
                    state.set("next_doc_id", id + 1)?;
                    id
                };

                let syntax: LuaValue = doc.get("syntax")?;
                if let LuaValue::Table(syn) = &syntax {
                    let syn_name: String = syn.get("name").unwrap_or_default();
                    let map_key = format!("language_{}", syn_name);
                    let map: LuaTable = ac.get("map")?;
                    let already: LuaValue = map.get(map_key.clone())?;
                    if matches!(already, LuaValue::Nil) {
                        if let LuaValue::Table(sym_t) = syn.get::<LuaValue>("symbols")? {
                            let items = lua.create_table()?;
                            for pair in sym_t.pairs::<String, LuaValue>() {
                                let (name, _) = pair?;
                                items.set(name, LuaValue::Boolean(true))?;
                            }
                            let entry = lua.create_table()?;
                            entry.set("files", syn.get::<LuaValue>("files")?)?;
                            entry.set("items", items)?;
                            map.set(map_key, entry)?;
                            ac.set("map", map)?;
                        }
                    }
                }

                let max_symbols: i64 = ac_config_get(lua, "max_symbols").unwrap_or(4000);
                let result = if let LuaValue::Table(lines_t) = doc.get::<LuaValue>("lines")? {
                    symbol_index.call_function::<LuaTable>("set_doc_symbols", (doc_id as u64, lines_t, max_symbols as usize, LuaValue::Nil))?
                } else {
                    lua.create_table()?
                };
                let exceeded: bool = result.get("exceeded").unwrap_or(false);

                let new_entry = lua.create_table()?;
                new_entry.set("doc_id", doc_id)?;
                new_entry.set("last_change_id", change_id)?;
                new_entry.set("symbols", !exceeded)?;
                cache.raw_set(doc.clone(), new_entry)?;
            }

            if suggestion_scope(lua)?.as_deref() == Some("global") {
                let entry: LuaValue = cache.raw_get(doc.clone())?;
                if let LuaValue::Table(ce) = entry {
                    if matches!(ce.get::<LuaValue>("symbols")?, LuaValue::Boolean(true)) {
                        let doc_id: i64 = ce.get("doc_id")?;
                        let doc_syms: LuaTable = symbol_index.call_function("get_doc_symbols", doc_id)?;
                        let global: LuaTable = state.get("global_symbols")?;
                        for sym in doc_syms.sequence_values::<String>() {
                            global.set(sym?, true)?;
                        }
                    }
                }
            }
        }

        // Return 0 when work was done so the scheduler resumes quickly;
        // return 1.0 when everything is fresh so we sleep before re-scanning.
        Ok(if did_work { 0.0 } else { 1.0 })
    })?;

    // Lua wrapper: loops and yields — only Lua functions may yield in Lua 5.4.
    let thread_fn: LuaFunction = lua.load(
        "local t = ...; return function() while true do coroutine.yield(t()) end end"
    ).call::<LuaFunction>(tick)?;

    core.get::<LuaFunction>("add_thread")?.call::<()>(thread_fn)
}

fn patch_methods(
    lua: &Lua,
    state_key: Arc<LuaRegistryKey>,
    ac_key: Arc<LuaRegistryKey>,
) -> LuaResult<()> {
    // RootView.on_text_input
    {
        let root_view: LuaTable = require_table(lua, "core.rootview")?;
        let old: LuaFunction = root_view.get("on_text_input")?;
        let old_key = Arc::new(lua.create_registry_value(old)?);
        let sk = Arc::clone(&state_key);
        let ak = Arc::clone(&ac_key);
        root_view.set("on_text_input", lua.create_function(move |lua, args: LuaMultiValue| {
            let old: LuaFunction = lua.registry_value(&old_key)?;
            old.call::<()>(args)?;
            let state: LuaTable = lua.registry_value(&sk)?;
            let ac: LuaTable = lua.registry_value(&ak)?;
            let triggered: bool = state.get("triggered_manually")?;
            if triggered || current_mode(lua)? != "lsp" {
                show_autocomplete(lua, &state, &ac)?;
            }
            Ok(())
        })?)?;
    }

    // Doc.remove
    {
        let doc_class: LuaTable = require_table(lua, "core.doc")?;
        let old: LuaFunction = doc_class.get("remove")?;
        let old_key = Arc::new(lua.create_registry_value(old)?);
        let sk = Arc::clone(&state_key);
        let ak = Arc::clone(&ac_key);
        doc_class.set("remove", lua.create_function(move |lua, (this, line1, col1, line2, col2): (LuaTable, i64, i64, i64, i64)| {
            let old: LuaFunction = lua.registry_value(&old_key)?;
            old.call::<()>((this, line1, col1, line2, col2))?;
            let state: LuaTable = lua.registry_value(&sk)?;
            let ac: LuaTable = lua.registry_value(&ak)?;
            let triggered: bool = state.get("triggered_manually")?;
            if triggered && line1 == line2 {
                let last_col: i64 = state.get("last_col").unwrap_or(col1);
                if last_col >= col1 {
                    reset_suggestions(lua, &state, &ac)?;
                } else {
                    show_autocomplete(lua, &state, &ac)?;
                }
            }
            Ok(())
        })?)?;
    }

    // RootView.update
    {
        let root_view: LuaTable = require_table(lua, "core.rootview")?;
        let old: LuaFunction = root_view.get("update")?;
        let old_key = Arc::new(lua.create_registry_value(old)?);
        let sk = Arc::clone(&state_key);
        let ak = Arc::clone(&ac_key);
        root_view.set("update", lua.create_function(move |lua, args: LuaMultiValue| {
            let old: LuaFunction = lua.registry_value(&old_key)?;
            old.call::<()>(args)?;
            let state: LuaTable = lua.registry_value(&sk)?;
            let ac: LuaTable = lua.registry_value(&ak)?;
            if let Some(av) = get_active_docview(lua)? {
                let doc: LuaTable = av.get("doc")?;
                let (line, col): (i64, i64) = doc.call_method("get_selection", ())?;
                let triggered: bool = state.get("triggered_manually")?;
                let last_line: i64 = state.get("last_line").unwrap_or(line);
                let last_col: i64 = state.get("last_col").unwrap_or(col);
                let should_reset = if !triggered {
                    line != last_line || col != last_col
                } else {
                    line != last_line || col < last_col
                };
                if should_reset {
                    reset_suggestions(lua, &state, &ac)?;
                }
            }
            Ok(())
        })?)?;
    }

    // RootView.draw
    {
        let root_view: LuaTable = require_table(lua, "core.rootview")?;
        let old: LuaFunction = root_view.get("draw")?;
        let old_key = Arc::new(lua.create_registry_value(old)?);
        let sk = Arc::clone(&state_key);
        let ak = Arc::clone(&ac_key);
        root_view.set("draw", lua.create_function(move |lua, args: LuaMultiValue| {
            let old: LuaFunction = lua.registry_value(&old_key)?;
            old.call::<()>(args)?;
            let state: LuaTable = lua.registry_value(&sk)?;
            let ac: LuaTable = lua.registry_value(&ak)?;
            if let Some(av) = get_active_docview(lua)? {
                let core: LuaTable = require_table(lua, "core")?;
                let root_view: LuaTable = core.get("root_view")?;
                let ctx = make_ctx(lua, &state, &ac)?;
                let drawing: LuaTable = require_table(lua, "plugins.autocomplete.drawing")?;
                let draw_fn: LuaFunction = drawing.get("draw_suggestions_box")?;
                root_view.call_method::<()>("defer_draw", (draw_fn, ctx, av))?;
            }
            Ok(())
        })?)?;
    }

    // Doc.on_close
    {
        let doc_class: LuaTable = require_table(lua, "core.doc")?;
        let old: LuaFunction = doc_class.get("on_close")?;
        let old_key = Arc::new(lua.create_registry_value(old)?);
        let sk = Arc::clone(&state_key);
        doc_class.set("on_close", lua.create_function(move |lua, (this, rest): (LuaTable, LuaMultiValue)| {
            let state: LuaTable = lua.registry_value(&sk)?;
            let cache: LuaTable = state.get("cache")?;
            let entry: LuaValue = cache.raw_get(this.clone())?;
            if let LuaValue::Table(ce) = entry {
                let doc_id: Option<i64> = ce.get("doc_id")?;
                if let Some(id) = doc_id {
                    let symbol_index: LuaTable = require_table(lua, "symbol_index")?;
                    symbol_index.call_function::<()>("remove_doc", id as u64)?;
                    let shrink: LuaValue = symbol_index.get("shrink")?;
                    if let LuaValue::Function(f) = shrink {
                        f.call::<()>(())?;
                    }
                }
            }
            let old: LuaFunction = lua.registry_value(&old_key)?;
            let mut args = vec![LuaValue::Table(this)];
            for v in rest.into_iter() { args.push(v); }
            old.call::<()>(LuaMultiValue::from_vec(args))
        })?)?;
    }

    Ok(())
}

fn register_commands(
    lua: &Lua,
    state_key: Arc<LuaRegistryKey>,
    ac_key: Arc<LuaRegistryKey>,
) -> LuaResult<()> {
    let command: LuaTable = require_table(lua, "core.command")?;

    let predicate = {
        let sk = Arc::clone(&state_key);
        lua.create_function(move |lua, ()| -> LuaResult<LuaMultiValue> {
            let state: LuaTable = lua.registry_value(&sk)?;
            let av = get_active_docview(lua)?;
            let suggestions: LuaTable = state.get("suggestions")?;
            let len = suggestions.raw_len();
            match av {
                Some(dv) if len > 0 => Ok(LuaMultiValue::from_vec(vec![
                    LuaValue::Table(dv.clone()),
                    LuaValue::Table(dv),
                ])),
                _ => Ok(LuaMultiValue::from_vec(vec![LuaValue::Boolean(false)])),
            }
        })?
    };

    let cmds = lua.create_table()?;

    // autocomplete:complete
    {
        let sk = Arc::clone(&state_key);
        let ak = Arc::clone(&ac_key);
        cmds.set("autocomplete:complete", lua.create_function(move |lua, dv: LuaTable| {
            let state: LuaTable = lua.registry_value(&sk)?;
            let ac: LuaTable = lua.registry_value(&ak)?;
            let doc: LuaTable = dv.get("doc")?;
            let suggestions: LuaTable = state.get("suggestions")?;
            let idx: i64 = state.get("suggestions_idx")?;
            let item: LuaTable = suggestions.get(idx)?;

            let mut inserted = false;
            let onselect: LuaValue = item.get("onselect")?;
            if let LuaValue::Function(f) = onselect {
                let result: LuaValue = f.call((idx, item.clone()))?;
                inserted = matches!(result, LuaValue::Boolean(true));
            }

            if !inserted {
                let current_partial = get_partial_symbol(lua)?;
                let sz = current_partial.len();
                let item_text: String = item.get("text")?;

                // Iterate selections to remove partial and insert completion
                let sel_results: LuaMultiValue = doc.call_method("get_selections", (true,))?;
                let mut sel_iter = sel_results.into_iter();
                let iter_fn: LuaFunction = match sel_iter.next() {
                    Some(LuaValue::Function(f)) => f,
                    _ => {
                        doc.call_method::<()>("text_input", item_text.clone())?;
                        return reset_suggestions(lua, &state, &ac);
                    }
                };
                let sel_state = sel_iter.next().unwrap_or(LuaValue::Nil);
                let mut control = sel_iter.next().unwrap_or(LuaValue::Nil);

                loop {
                    let res: LuaMultiValue = iter_fn.call((sel_state.clone(), control.clone()))?;
                    let mut vals = res.into_iter();
                    let first = vals.next();
                    match first {
                        None | Some(LuaValue::Nil) => break,
                        Some(v) => {
                            control = v;
                            let line1: i64 = match &control { LuaValue::Integer(n) => *n, _ => break };
                            let col1: i64 = match vals.next() { Some(LuaValue::Integer(n)) => n, _ => break };
                            let line2: i64 = match vals.next() { Some(LuaValue::Integer(n)) => n, _ => break };
                            let _col2: LuaValue = vals.next().unwrap_or(LuaValue::Nil);

                            let n = col1 - 1;
                            let line_text: String = {
                                let lines: LuaTable = doc.get("lines")?;
                                lines.get(line1).unwrap_or_default()
                            };
                            for i in 1..=(sz as i64 + 1) {
                                let j = sz as i64 - i;
                                let subline_start = (n - j).max(0) as usize;
                                let subline_end = n as usize;
                                let subline = if subline_start <= line_text.len() && subline_end <= line_text.len() {
                                    &line_text[subline_start..subline_end]
                                } else {
                                    ""
                                };
                                let subpartial_start = (i - 1) as usize;
                                let subpartial = if subpartial_start <= current_partial.len() {
                                    &current_partial[subpartial_start..]
                                } else {
                                    ""
                                };
                                if subpartial == subline {
                                    doc.call_method::<()>("remove", (line1, col1, line2, n - j))?;
                                    break;
                                }
                            }
                        }
                    }
                }
                doc.call_method::<()>("text_input", item_text)?;
            }

            reset_suggestions(lua, &state, &ac)
        })?)?;
    }

    // autocomplete:previous
    {
        let sk = Arc::clone(&state_key);
        cmds.set("autocomplete:previous", lua.create_function(move |lua, ()| {
            let state: LuaTable = lua.registry_value(&sk)?;
            let suggestions: LuaTable = state.get("suggestions")?;
            let len = suggestions.raw_len() as i64;
            let idx: i64 = state.get("suggestions_idx")?;
            let new_idx = (idx - 2).rem_euclid(len) + 1;
            state.set("suggestions_idx", new_idx)?;
            let ah: i64 = ac_config_get(lua, "max_height")?;
            let ah = ah.min(len);
            let offset: i64 = state.get("suggestions_offset")?;
            let new_offset = if offset > new_idx {
                new_idx
            } else if offset + ah < new_idx + 1 {
                new_idx - ah + 1
            } else {
                offset
            };
            state.set("suggestions_offset", new_offset)
        })?)?;
    }

    // autocomplete:next
    {
        let sk = Arc::clone(&state_key);
        cmds.set("autocomplete:next", lua.create_function(move |lua, ()| {
            let state: LuaTable = lua.registry_value(&sk)?;
            let suggestions: LuaTable = state.get("suggestions")?;
            let len = suggestions.raw_len() as i64;
            let idx: i64 = state.get("suggestions_idx")?;
            let new_idx = idx.rem_euclid(len) + 1;
            state.set("suggestions_idx", new_idx)?;
            let ah: i64 = ac_config_get(lua, "max_height")?;
            let ah = ah.min(len);
            let offset: i64 = state.get("suggestions_offset")?;
            let new_offset = if offset + ah < new_idx + 1 {
                new_idx - ah + 1
            } else if offset > new_idx {
                new_idx
            } else {
                offset
            };
            state.set("suggestions_offset", new_offset)
        })?)?;
    }

    // autocomplete:cycle
    {
        let sk = Arc::clone(&state_key);
        cmds.set("autocomplete:cycle", lua.create_function(move |lua, ()| {
            let state: LuaTable = lua.registry_value(&sk)?;
            let suggestions: LuaTable = state.get("suggestions")?;
            let len = suggestions.raw_len() as i64;
            let idx: i64 = state.get("suggestions_idx")?;
            let new_idx = if idx + 1 > len { 1 } else { idx + 1 };
            state.set("suggestions_idx", new_idx)
        })?)?;
    }

    // autocomplete:cancel
    {
        let sk = Arc::clone(&state_key);
        let ak = Arc::clone(&ac_key);
        cmds.set("autocomplete:cancel", lua.create_function(move |lua, ()| {
            let state: LuaTable = lua.registry_value(&sk)?;
            let ac: LuaTable = lua.registry_value(&ak)?;
            reset_suggestions(lua, &state, &ac)
        })?)?;
    }

    command.call_function::<()>("add", (LuaValue::Function(predicate), cmds))?;

    let keymap: LuaTable = require_table(lua, "core.keymap")?;
    let bindings = lua.create_table()?;
    bindings.set("return", "autocomplete:complete")?;
    bindings.set("keypad enter", "autocomplete:complete")?;
    bindings.set("tab", "autocomplete:complete")?;
    bindings.set("up", "autocomplete:previous")?;
    bindings.set("down", "autocomplete:next")?;
    bindings.set("escape", "autocomplete:cancel")?;
    keymap.call_function::<()>("add", bindings)?;

    Ok(())
}

fn build_autocomplete_module(
    lua: &Lua,
    state_key: Arc<LuaRegistryKey>,
) -> LuaResult<LuaTable> {
    let ac = lua.create_table()?;
    ac.set("map", lua.create_table()?)?;
    ac.set("map_manually", lua.create_table()?)?;
    ac.set("on_close", LuaValue::Nil)?;
    ac.set("icons", lua.create_table()?)?;
    ac.set("providers", lua.create_table()?)?;

    let ac_key = Arc::new(lua.create_registry_value(ac.clone())?);

    // autocomplete.add(t, manually_triggered)
    {
        let ak = Arc::clone(&ac_key);
        ac.set("add", lua.create_function(move |lua, (t, manually): (LuaTable, LuaValue)| {
            let ac: LuaTable = lua.registry_value(&ak)?;
            let manually = matches!(manually, LuaValue::Boolean(true));
            let name: String = t.get("name")?;
            let files: String = t.get("files").unwrap_or_else(|_| ".*".to_owned());
            let items = items_from_completion_map(lua, &t)?;
            let entry = lua.create_table()?;
            entry.set("files", files)?;
            entry.set("items", items)?;
            if !manually {
                let map: LuaTable = ac.get("map")?;
                map.set(name, entry)?;
            } else {
                let map: LuaTable = ac.get("map_manually")?;
                map.set(name, entry)?;
            }
            Ok(())
        })?)?;
    }

    // autocomplete.open(on_close)
    {
        let sk = Arc::clone(&state_key);
        let ak = Arc::clone(&ac_key);
        ac.set("open", lua.create_function(move |lua, on_close: LuaValue| {
            let state: LuaTable = lua.registry_value(&sk)?;
            let ac: LuaTable = lua.registry_value(&ak)?;
            state.set("triggered_manually", true)?;
            if !matches!(on_close, LuaValue::Nil) {
                ac.set("on_close", on_close)?;
            }
            if let Some(_av) = get_active_docview(lua)? {
                let partial = get_partial_symbol(lua)?;
                state.set("partial", partial)?;
                let core: LuaTable = require_table(lua, "core")?;
                let av: LuaTable = core.get("active_view")?;
                let doc: LuaTable = av.get("doc")?;
                let (line, col): (i64, i64) = doc.call_method("get_selection", ())?;
                state.set("last_line", line)?;
                state.set("last_col", col)?;
                update_suggestions(lua, &state, &ac)?;
            }
            Ok(())
        })?)?;
    }

    // autocomplete.close()
    {
        let sk = Arc::clone(&state_key);
        let ak = Arc::clone(&ac_key);
        ac.set("close", lua.create_function(move |lua, ()| {
            let state: LuaTable = lua.registry_value(&sk)?;
            let ac: LuaTable = lua.registry_value(&ak)?;
            reset_suggestions(lua, &state, &ac)
        })?)?;
    }

    // autocomplete.is_open()
    {
        let sk = Arc::clone(&state_key);
        ac.set("is_open", lua.create_function(move |lua, ()| {
            let state: LuaTable = lua.registry_value(&sk)?;
            let suggestions: LuaTable = state.get("suggestions")?;
            Ok(suggestions.raw_len() > 0)
        })?)?;
    }

    // autocomplete.complete(completions, on_close)
    {
        let sk = Arc::clone(&state_key);
        let ak = Arc::clone(&ac_key);
        ac.set("complete", lua.create_function(move |lua, (completions, on_close): (LuaTable, LuaValue)| {
            let state: LuaTable = lua.registry_value(&sk)?;
            let ac: LuaTable = lua.registry_value(&ak)?;
            reset_suggestions(lua, &state, &ac)?;
            let empty = lua.create_table()?;
            ac.set("map_manually", empty)?;
            let open: LuaFunction = ac.get("add")?;
            open.call::<()>((completions, LuaValue::Boolean(true)))?;
            let open_fn: LuaFunction = ac.get("open")?;
            open_fn.call::<()>(on_close)
        })?)?;
    }

    // autocomplete.can_complete()
    {
        let sk = Arc::clone(&state_key);
        ac.set("can_complete", lua.create_function(move |lua, ()| {
            let state: LuaTable = lua.registry_value(&sk)?;
            if current_mode(lua)? == "off" { return Ok(false); }
            let partial: String = state.get("partial")?;
            let min_len: i64 = ac_config_get(lua, "min_len")?;
            Ok((partial.len() as i64) >= min_len)
        })?)?;
    }

    // autocomplete.register_provider(name, provider)
    {
        let ak = Arc::clone(&ac_key);
        ac.set("register_provider", lua.create_function(move |lua, (name, provider): (String, LuaFunction)| {
            let ac: LuaTable = lua.registry_value(&ak)?;
            let providers: LuaTable = ac.get("providers")?;
            providers.set(name, provider)
        })?)?;
    }

    // autocomplete.set_default_mode(mode)
    {
        let sk = Arc::clone(&state_key);
        ac.set("set_default_mode", lua.create_function(move |lua, mode: String| {
            let state: LuaTable = lua.registry_value(&sk)?;
            let mode_explicitly_set: bool = state.get("mode_explicitly_set")?;
            if !mode_explicitly_set && current_mode(lua)? == "lsp" {
                let config: LuaTable = require_table(lua, "core.config")?;
                let plugins: LuaTable = config.get("plugins")?;
                let ac: LuaTable = plugins.get("autocomplete")?;
                ac.set("mode", mode)?;
            }
            Ok(())
        })?)?;
    }

    // autocomplete.add_icon(name, character, font, color)
    {
        let ak = Arc::clone(&ac_key);
        ac.set("add_icon", lua.create_function(move |lua, (name, character, font, color): (String, String, LuaValue, LuaValue)| {
            let ac: LuaTable = lua.registry_value(&ak)?;
            let icons: LuaTable = ac.get("icons")?;
            let entry = lua.create_table()?;
            entry.set("char", character)?;
            let font = if matches!(font, LuaValue::Nil) {
                let style: LuaTable = require_table(lua, "core.style")?;
                style.get("code_font")?
            } else {
                font
            };
            entry.set("font", font)?;
            let color = if matches!(color, LuaValue::Nil) {
                LuaValue::String(lua.create_string("keyword")?)
            } else {
                color
            };
            entry.set("color", color)?;
            icons.set(name, entry)
        })?)?;
    }

    // Add built-in syntax symbol icons (one for each style.syntax key)
    {
        let ak = Arc::clone(&ac_key);
        let ac_clone = ac.clone();
        let style: LuaTable = require_table(lua, "core.style")?;
        let syntax: LuaTable = style.get("syntax")?;
        let icon_font: LuaValue = style.get("icon_font")?;
        let add_icon_fn: LuaFunction = ac_clone.get("add_icon")?;
        for pair in syntax.pairs::<String, LuaValue>() {
            let (name, _) = pair?;
            add_icon_fn.call::<()>((
                name.clone(),
                "M".to_owned(),
                icon_font.clone(),
                LuaValue::String(lua.create_string(&name)?),
            ))?;
        }
        let _ = ak;
    }

    // Patch methods and add thread
    patch_methods(lua, Arc::clone(&state_key), Arc::clone(&ac_key))?;
    add_thread(lua, Arc::clone(&state_key), Arc::clone(&ac_key))?;
    register_commands(lua, Arc::clone(&state_key), Arc::clone(&ac_key))?;

    Ok(ac)
}

fn init_autocomplete(lua: &Lua) -> LuaResult<LuaValue> {
    set_config_defaults(lua)?;

    // Build internal state
    let state = lua.create_table()?;
    state.set("cache", make_weak_table(lua)?)?;
    state.set("global_symbols", lua.create_table()?)?;
    state.set("next_doc_id", 1i64)?;
    state.set("partial", "")?;
    state.set("suggestions_offset", 1i64)?;
    state.set("suggestions_idx", 1i64)?;
    state.set("suggestions", lua.create_table()?)?;
    state.set("last_line", LuaValue::Nil)?;
    state.set("last_col", LuaValue::Nil)?;
    state.set("triggered_manually", false)?;
    state.set("provider_items", LuaValue::Nil)?;
    state.set("provider_request_id", 0i64)?;

    let user_ac: LuaValue = {
        let config: LuaTable = require_table(lua, "core.config")?;
        let plugins: LuaTable = config.get("plugins")?;
        plugins.get("autocomplete")?
    };
    let mode_explicitly_set = if let LuaValue::Table(ref t) = user_ac {
        !matches!(t.get::<LuaValue>("mode")?, LuaValue::Nil)
    } else {
        false
    };
    state.set("mode_explicitly_set", mode_explicitly_set)?;

    let state_key = Arc::new(lua.create_registry_value(state)?);
    let ac = build_autocomplete_module(lua, state_key)?;
    Ok(LuaValue::Table(ac))
}

// ─── Drawing module ──────────────────────────────────────────────────────────

fn init_drawing(lua: &Lua) -> LuaResult<LuaValue> {
    let draw_state = lua.create_table()?;
    draw_state.set("last_max_width", 0.0f64)?;
    draw_state.set("desc_font", LuaValue::Nil)?;
    draw_state.set("previous_scale", {
        let scale: LuaValue = lua.globals().get("SCALE")?;
        scale
    })?;
    let ds_key = Arc::new(lua.create_registry_value(draw_state)?);

    let m = lua.create_table()?;

    // M.get_suggestions_rect(ctx, av) -> x, y, w, h[, has_icons]
    {
        let dsk = Arc::clone(&ds_key);
        m.set("get_suggestions_rect", lua.create_function(move |lua, (ctx, av): (LuaTable, LuaTable)| {
            let suggestions: LuaTable = ctx.get("suggestions")?;
            let suggestions_idx: i64 = ctx.get("suggestions_idx")?;
            let partial: String = ctx.get("partial")?;
            let icons: LuaTable = ctx.get("icons")?;

            let len = suggestions.raw_len() as i64;
            let ds: LuaTable = lua.registry_value(&dsk)?;
            if len == 0 {
                ds.set("last_max_width", 0.0f64)?;
                return Ok(LuaMultiValue::from_vec(vec![
                    LuaValue::Number(0.0), LuaValue::Number(0.0),
                    LuaValue::Number(0.0), LuaValue::Number(0.0),
                ]));
            }

            let doc: LuaTable = av.get("doc")?;
            let (line, col): (i64, i64) = doc.call_method("get_selection", ())?;
            let partial_len = partial.chars().count() as i64;
            let (mut x, mut y): (f64, f64) = av.call_method("get_line_screen_position", (line, col - partial_len))?;
            let lh_av: f64 = av.call_method("get_line_height", ())?;
            let style: LuaTable = require_table(lua, "core.style")?;
            let padding: LuaTable = style.get("padding")?;
            let px: f64 = padding.get("x")?;
            let py: f64 = padding.get("y")?;
            y += lh_av + py;

            let av_font: LuaValue = av.call_method("get_font", ())?;
            let th = font_get_height(&av_font)?;
            let mut has_icons = false;
            let hide_info: bool = ac_config_get(lua, "hide_info")?;
            let hide_icons: bool = ac_config_get(lua, "hide_icons")?;
            let ah: i64 = ac_config_get(lua, "max_height")?;
            let show_count = len.min(ah);
            let start_idx = (suggestions_idx - (ah - 1)).max(1);

            let mut max_width = 0.0f64;
            let mut max_l_icon_width = 0.0f64;
            let style_font: LuaValue = style.get("font")?;

            for i in start_idx..start_idx + show_count {
                let s: LuaValue = suggestions.get(i)?;
                let s = match s { LuaValue::Table(t) => t, _ => continue };
                let text: String = s.get("text")?;
                let mut w = font_get_width(&av_font, &text)?;
                let info: LuaValue = s.get("info")?;
                if !matches!(info, LuaValue::Nil) && !hide_info {
                    if let LuaValue::String(is) = &info {
                        w += font_get_width(&style_font, &is.to_str()?)? + px;
                    }
                }
                let icon: LuaValue = s.get("icon").unwrap_or_else(|_| LuaValue::Nil);
                let icon_key = if !matches!(icon, LuaValue::Nil) { icon } else { info };
                if !hide_icons {
                    if let LuaValue::String(ik) = &icon_key {
                        let icon_data: LuaValue = icons.get(ik.to_str()?)?;
                        if let LuaValue::Table(id) = icon_data {
                            let ifont: LuaValue = id.get("font")?;
                            let ichar: String = id.get("char")?;
                            let iw = font_get_width(&ifont, &ichar)?;
                            let icon_pos: String = ac_config_get(lua, "icon_position")?;
                            if icon_pos == "left" {
                                max_l_icon_width = max_l_icon_width.max(iw + px / 2.0);
                            }
                            w += iw + px / 2.0;
                            has_icons = true;
                        }
                    }
                }
                max_width = max_width.max(w);
            }

            let last_max: f64 = ds.get("last_max_width")?;
            max_width = max_width.max(last_max);
            ds.set("last_max_width", max_width)?;
            max_width += px * 2.0;
            x -= px + max_l_icon_width;

            let max_items = len.min(ah) + 1;

            let core: LuaTable = require_table(lua, "core")?;
            let root_view: LuaTable = core.get("root_view")?;
            let rv_size: LuaTable = root_view.get("size")?;
            let rv_w: f64 = rv_size.get("x")?;

            if max_width > rv_w { max_width = rv_w; }
            let scale: f64 = lua.globals().get::<LuaValue>("SCALE")
                .map(|v| match v { LuaValue::Number(n) => n, LuaValue::Integer(n) => n as f64, _ => 1.0 })
                .unwrap_or(1.0);
            if max_width < 150.0 * scale { max_width = 150.0 * scale; }

            let av_size: LuaTable = av.get("size")?;
            let av_pos: LuaTable = av.get("position")?;
            let av_sx: f64 = av_size.get("x")?;
            let av_px: f64 = av_pos.get("x")?;
            if x + max_width > rv_w {
                x = av_sx + av_px - max_width;
            }

            let h = max_items as f64 * (th + py) + py;
            Ok(LuaMultiValue::from_vec(vec![
                LuaValue::Number(x),
                LuaValue::Number(y - py),
                LuaValue::Number(max_width),
                LuaValue::Number(h),
                LuaValue::Boolean(has_icons),
            ]))
        })?)?;
    }

    // M.draw_suggestions_box(ctx, av)
    {
        let dsk = Arc::clone(&ds_key);
        m.set("draw_suggestions_box", lua.create_function(move |lua, (ctx, av): (LuaTable, LuaTable)| {
            let suggestions: LuaTable = ctx.get("suggestions")?;
            let suggestions_idx: i64 = ctx.get("suggestions_idx")?;
            let suggestions_offset: i64 = ctx.get("suggestions_offset")?;
            let icons: LuaTable = ctx.get("icons")?;
            let len = suggestions.raw_len() as i64;
            if len <= 0 { return Ok(()); }

            let style: LuaTable = require_table(lua, "core.style")?;
            let ah: i64 = ac_config_get(lua, "max_height")?;
            let m_tbl: LuaTable = require_table(lua, "plugins.autocomplete.drawing")?;
            let rect: LuaMultiValue = m_tbl.call_function("get_suggestions_rect", (ctx.clone(), av.clone()))?;
            let mut rv = rect.into_iter();
            let rx: f64 = match rv.next() { Some(LuaValue::Number(n)) => n, _ => return Ok(()) };
            let ry: f64 = match rv.next() { Some(LuaValue::Number(n)) => n, _ => return Ok(()) };
            let rw: f64 = match rv.next() { Some(LuaValue::Number(n)) => n, _ => return Ok(()) };
            let rh: f64 = match rv.next() { Some(LuaValue::Number(n)) => n, _ => return Ok(()) };
            let has_icons: bool = match rv.next() { Some(LuaValue::Boolean(b)) => b, _ => false };

            let renderer: LuaTable = lua.globals().get("renderer")?;
            let bg3: LuaValue = style.get("background3")?;
            renderer.call_function::<()>("draw_rect", (rx, ry, rw, rh, bg3.clone()))?;

            let av_font: LuaValue = av.call_method("get_font", ())?;
            let padding: LuaTable = style.get("padding")?;
            let py: f64 = padding.get("y")?;
            let px: f64 = padding.get("x")?;
            let th = font_get_height(&av_font)? + py;
            let mut y = ry + py / 2.0;
            let show_count = len.min(ah);
            let hide_info: bool = ac_config_get(lua, "hide_info")?;
            let color_accent: LuaValue = style.get("accent")?;
            let color_text: LuaValue = style.get("text")?;
            let color_dim: LuaValue = style.get("dim")?;
            let style_font: LuaValue = style.get("font")?;
            let common: LuaTable = require_table(lua, "core.common")?;
            let hide_icons: bool = ac_config_get(lua, "hide_icons")?;
            let icon_position: String = ac_config_get(lua, "icon_position")?;
            let core: LuaTable = require_table(lua, "core")?;

            let ds: LuaTable = lua.registry_value(&dsk)?;
            // Update desc font if needed
            let cur_scale: f64 = lua.globals().get::<LuaValue>("SCALE")
                .map(|v| match v { LuaValue::Number(n) => n, LuaValue::Integer(n) => n as f64, _ => 1.0 })
                .unwrap_or(1.0);
            let prev_scale: f64 = ds.get("previous_scale").unwrap_or(cur_scale);
            let desc_font: LuaValue = ds.get("desc_font")?;
            let desc_font = if matches!(desc_font, LuaValue::Nil) || (prev_scale - cur_scale).abs() > 1e-6 {
                let code_font: LuaValue = style.get("code_font")?;
                let desc_size: i64 = ac_config_get(lua, "desc_font_size")?;
                let new_font: LuaValue = match &code_font {
                    LuaValue::Table(t) => t.call_method("copy", (desc_size as f64 * cur_scale,))?,
                    LuaValue::UserData(ud) => ud.call_method("copy", (desc_size as f64 * cur_scale,))?,
                    _ => code_font.clone(),
                };
                ds.set("desc_font", new_font.clone())?;
                ds.set("previous_scale", cur_scale)?;
                new_font
            } else {
                desc_font
            };

            let mut desc_item: Option<(i64, LuaTable)> = None;

            for i in suggestions_offset..suggestions_offset + show_count {
                let s: LuaValue = suggestions.get(i)?;
                let s = match s { LuaValue::Table(t) => t, _ => break };
                let mut icon_l_padding = 0.0f64;
                let mut icon_r_padding = 0.0f64;

                if has_icons && !hide_icons {
                    let icon: LuaValue = s.get("icon").unwrap_or(LuaValue::Nil);
                    let info: LuaValue = s.get("info").unwrap_or(LuaValue::Nil);
                    let icon_key = if !matches!(icon, LuaValue::Nil) { icon } else { info };
                    if let LuaValue::String(ik) = &icon_key {
                        let icon_data: LuaValue = icons.get(ik.to_str()?)?;
                        if let LuaValue::Table(id) = icon_data {
                            let ifont: LuaValue = id.get("font")?;
                            let itext: String = id.get("char")?;
                            let mut icolor: LuaValue = id.get("color")?;
                            if i == suggestions_idx {
                                icolor = color_accent.clone();
                            } else if let LuaValue::String(cs) = &icolor {
                                let syn_color: LuaValue = {
                                    let syntax: LuaTable = style.get("syntax")?;
                                    syntax.get(cs.to_str()?)?
                                };
                                icolor = syn_color;
                            }
                            let iw = font_get_width(&ifont, &itext)?;
                            if icon_position == "left" {
                                common.call_function::<()>("draw_text", (ifont.clone(), icolor, itext.clone(), "left", rx + px, y, rw, th))?;
                                icon_l_padding = iw + px / 2.0;
                            } else {
                                common.call_function::<()>("draw_text", (ifont.clone(), icolor, itext.clone(), "right", rx, y, rw - px, th))?;
                                icon_r_padding = iw + px / 2.0;
                            }
                        }
                    }
                }

                let info: LuaValue = s.get("info").unwrap_or(LuaValue::Nil);
                let info_size = if !matches!(info, LuaValue::Nil) && !hide_info {
                    if let LuaValue::String(is) = &info {
                        font_get_width(&style_font, &is.to_str()?)? + px
                    } else { 0.0 }
                } else { 0.0 };

                let color = if i == suggestions_idx { color_accent.clone() } else { color_text.clone() };
                let text: String = s.get("text")?;

                core.call_function::<()>("push_clip_rect", (
                    rx + icon_l_padding + px, y,
                    rw - info_size - icon_l_padding - icon_r_padding - px, th,
                ))?;
                let x_adv: f64 = common.call_function("draw_text", (
                    av_font.clone(), color.clone(), text.clone(), "left",
                    rx + icon_l_padding + px, y, rw, th,
                ))?;
                core.call_function::<()>("pop_clip_rect", ())?;

                if x_adv > rx + rw - info_size - icon_r_padding {
                    let ellipsis_size = font_get_width(&av_font, "…")?;
                    let ell_x = rx + rw - info_size - icon_r_padding - ellipsis_size;
                    renderer.call_function::<()>("draw_rect", (ell_x, y, ellipsis_size, th, bg3.clone()))?;
                    common.call_function::<()>("draw_text", (av_font.clone(), color.clone(), "…", "left", ell_x, y, ellipsis_size, th))?;
                }

                if !matches!(info, LuaValue::Nil) && !hide_info {
                    let info_color = if i == suggestions_idx { color_text.clone() } else { color_dim.clone() };
                    common.call_function::<()>("draw_text", (
                        style_font.clone(), info_color, info, "right",
                        rx, y, rw - icon_r_padding - px, th,
                    ))?;
                }

                y += th;

                if suggestions_idx == i {
                    // Handle onhover
                    let onhover: LuaValue = s.get("onhover")?;
                    if let LuaValue::Function(f) = onhover {
                        f.call::<()>((suggestions_idx, s.clone()))?;
                        s.set("onhover", LuaValue::Nil)?;
                    }
                    // Check for description
                    let desc: LuaValue = s.get("desc")?;
                    if let LuaValue::String(ds_str) = &desc {
                        if !ds_str.to_str()?.is_empty() {
                            desc_item = Some((i, s.clone()));
                        }
                    }
                }
            }

            // Footer
            let caret: LuaValue = style.get("caret")?;
            let bg: LuaValue = style.get("background")?;
            renderer.call_function::<()>("draw_rect", (rx, y, rw, 2.0, caret))?;
            renderer.call_function::<()>("draw_rect", (rx, y + 2.0, rw, th, bg))?;
            common.call_function::<()>("draw_text", (style_font.clone(), color_accent.clone(), "Items", "left", rx + px, y, rw, th))?;
            common.call_function::<()>("draw_text", (
                style_font.clone(), color_accent.clone(),
                format!("{}/{}", suggestions_idx, len), "right",
                rx, y, rw - px, th,
            ))?;

            // Description box
            if let Some((_i, s)) = desc_item {
                let desc: String = s.get("desc")?;
                let av_size: LuaTable = av.get("size")?;
                let av_pos: LuaTable = av.get("position")?;
                let av_sx: f64 = av_size.get("x")?;
                let av_px: f64 = av_pos.get("x")?;
                let av_py: f64 = av_pos.get("y")?;

                // draw_description_box inline
                let font = &desc_font;
                let lh = font_get_height(font)?;
                let desc_y = ry + py;
                let desc_x = rx + rw + px / 4.0;
                let char_width = font_get_width(font, " ")?;

                let max_chars_right = (av_sx + av_px - desc_x) / char_width - 5.0;
                let max_chars_left = (rx - av_px - px / 4.0 - 10.0) / char_width - 5.0;
                let draw_left = (rx - av_px) < (av_sx - (rx - av_px) - rw);
                let max_chars = if draw_left { max_chars_left } else { max_chars_right };

                let lines: Vec<String> = desc.split('\n')
                    .flat_map(|line| wrap_line(line, max_chars.max(1.0) as usize))
                    .collect();
                let width: f64 = lines.iter()
                    .map(|l| font_get_width(font, l).unwrap_or(0.0))
                    .fold(0.0f64, f64::max);

                let box_x = if draw_left { rx - px / 4.0 - width - px * 2.0 } else { desc_x };
                let box_h = lines.len() as f64 * lh;
                renderer.call_function::<()>("draw_rect", (box_x, ry, width + px * 2.0, box_h + py * 2.0, bg3.clone()))?;
                let mut dy = ry + py;
                for line in &lines {
                    common.call_function::<()>("draw_text", (font.clone(), color_text.clone(), line.as_str(), "left", box_x + px, dy, width, lh))?;
                    dy += lh;
                }
                let _ = (av_py, desc_y, desc_x);
            }

            Ok(())
        })?)?;
    }

    Ok(LuaValue::Table(m))
}

/// Wrap a single line to at most max_chars width, splitting at word boundaries.
fn wrap_line(line: &str, max_chars: usize) -> Vec<String> {
    if line.len() <= max_chars {
        return vec![line.to_owned()];
    }
    let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut prev_char = ' ';
    let line_chars: Vec<char> = line.chars().collect();
    let total = line_chars.len();
    for (pos, &ch) in line_chars.iter().enumerate() {
        if current.len() < max_chars {
            current.push(ch);
            prev_char = ch;
            if pos + 1 >= total {
                lines.push(current.clone());
            }
        } else {
            let next_ch = if pos + 1 < total { line_chars[pos + 1] } else { ' ' };
            if !prev_char.is_whitespace() && !next_ch.is_whitespace() && pos + 1 < total {
                current.push('-');
            }
            lines.push(current.clone());
            current = format!("{}{}", indent, ch);
            prev_char = ch;
        }
    }
    if !current.is_empty() && lines.last().map(|l: &String| l.as_str()) != Some(&current) {
        lines.push(current);
    }
    lines
}

/// Registers `plugins.autocomplete` and `plugins.autocomplete.drawing`.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;

    preload.set(
        "plugins.autocomplete",
        lua.create_function(|lua, ()| init_autocomplete(lua))?,
    )?;

    preload.set(
        "plugins.autocomplete.drawing",
        lua.create_function(|lua, ()| init_drawing(lua))?,
    )
}
