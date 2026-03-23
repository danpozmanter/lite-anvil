use mlua::prelude::*;

/// Require a module by name, returning the loaded table.
fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

/// calc_signature(positioned_tokens) -> integer hash
fn build_calc_signature(lua: &Lua) -> LuaResult<LuaFunction> {
    lua.create_function(|lua, positioned: LuaValue| {
        let tbl = match positioned {
            LuaValue::Table(t) => t,
            _ => return Ok(LuaValue::Integer(0)),
        };
        let len = tbl.raw_len();
        if len == 0 {
            return Ok(LuaValue::Integer(0));
        }

        let string_format: LuaFunction = lua.globals().get::<LuaTable>("string")?.get("format")?;
        let string_byte: LuaFunction = lua.globals().get::<LuaTable>("string")?.get("byte")?;

        let mut hash: i64 = 5381;
        for i in 1..=len {
            let token: LuaTable = tbl.raw_get(i)?;
            let ttype: LuaValue = token.get("type")?;
            let pos: LuaValue = token.get("pos")?;
            let tlen: LuaValue = token.get("len")?;
            let part: LuaString = string_format.call(("%s:%d:%d|", ttype, pos, tlen))?;
            let part_len = part.as_bytes().len();
            for j in 1..=part_len {
                let b: i64 = string_byte.call((part.clone(), j))?;
                hash = ((hash * 33) + b) % 2_147_483_647;
            }
        }
        Ok(LuaValue::Integer(hash))
    })
}

/// pair_tokens_to_positioned(tokens) -> positioned table
fn build_pair_tokens_to_positioned(lua: &Lua) -> LuaResult<LuaFunction> {
    lua.create_function(|lua, tokens: LuaTable| {
        let positioned = lua.create_table()?;
        let len = tokens.raw_len();
        let mut pos: i64 = 0;
        let mut idx = 1;
        let mut out_idx = 1;
        while idx <= len {
            let token_type: LuaValue = tokens.raw_get(idx)?;
            let text: LuaValue = tokens.raw_get(idx + 1)?;
            let text_len: i64 = match text {
                LuaValue::String(ref s) => {
                    let string_tbl: LuaTable = lua.globals().get("string")?;
                    let ulen: LuaFunction = string_tbl.get("ulen")?;
                    let result: LuaValue = ulen.call(s.clone())?;
                    match result {
                        LuaValue::Integer(n) => n,
                        _ => s.as_bytes().len() as i64,
                    }
                }
                _ => 0,
            };
            let entry = lua.create_table()?;
            entry.set("type", token_type)?;
            entry.set("pos", pos)?;
            entry.set("len", text_len)?;
            positioned.raw_set(out_idx, entry)?;
            pos += text_len;
            idx += 2;
            out_idx += 1;
        }
        Ok(positioned)
    })
}

/// positioned_to_pair_tokens(positioned, full_text) -> pair token table
fn build_positioned_to_pair_tokens(lua: &Lua) -> LuaResult<LuaFunction> {
    lua.create_function(|lua, (positioned, full_text): (LuaTable, LuaString)| {
        let pair_tokens = lua.create_table()?;
        let len = positioned.raw_len();
        let string_tbl: LuaTable = lua.globals().get("string")?;
        let usub: LuaFunction = string_tbl.get("usub")?;
        let mut out_idx = 1;
        for i in 1..=len {
            let token: LuaTable = positioned.raw_get(i)?;
            let token_type: LuaValue = token.get("type")?;
            let token_pos: i64 = token.get("pos")?;
            let token_len: i64 = token.get("len")?;
            let start_char = token_pos + 1;
            let end_char = token_pos + token_len;
            let text: LuaValue = usub.call((full_text.clone(), start_char, end_char))?;
            if let LuaValue::String(ref s) = text {
                if !s.as_bytes().is_empty() {
                    pair_tokens.raw_set(out_idx, token_type)?;
                    pair_tokens.raw_set(out_idx + 1, text)?;
                    out_idx += 2;
                }
            }
        }
        Ok(pair_tokens)
    })
}

/// clone_positioned(positioned) -> deep copy
fn build_clone_positioned(lua: &Lua) -> LuaResult<LuaFunction> {
    lua.create_function(|lua, positioned: LuaTable| {
        let copy = lua.create_table()?;
        let len = positioned.raw_len();
        for i in 1..=len {
            let token: LuaTable = positioned.raw_get(i)?;
            let entry = lua.create_table()?;
            entry.set("type", token.get::<LuaValue>("type")?)?;
            entry.set("pos", token.get::<LuaValue>("pos")?)?;
            entry.set("len", token.get::<LuaValue>("len")?)?;
            copy.raw_set(i, entry)?;
        }
        Ok(copy)
    })
}

/// merge_adjacent(positioned) -> merged positioned table
fn build_merge_adjacent(lua: &Lua) -> LuaResult<LuaFunction> {
    lua.create_function(|lua, positioned: LuaTable| {
        let merged = lua.create_table()?;
        let len = positioned.raw_len();
        let mut merged_len: i64 = 0;
        for i in 1..=len {
            let token: LuaTable = positioned.raw_get(i)?;
            let token_len: i64 = token.get("len")?;
            if token_len > 0 {
                if merged_len > 0 {
                    let prev: LuaTable = merged.raw_get(merged_len)?;
                    let prev_type: LuaString = prev.get("type")?;
                    let token_type: LuaString = token.get("type")?;
                    let prev_pos: i64 = prev.get("pos")?;
                    let prev_len: i64 = prev.get("len")?;
                    let token_pos: i64 = token.get("pos")?;
                    if prev_type == token_type && prev_pos + prev_len == token_pos {
                        prev.set("len", prev_len + token_len)?;
                        continue;
                    }
                }
                let entry = lua.create_table()?;
                entry.set("type", token.get::<LuaValue>("type")?)?;
                entry.set("pos", token.get::<LuaValue>("pos")?)?;
                entry.set("len", token.get::<LuaValue>("len")?)?;
                merged_len += 1;
                merged.raw_set(merged_len, entry)?;
            }
        }
        Ok(merged)
    })
}

/// overlay_positioned(base_tokens, overlay_tokens) -> merged result
fn build_overlay_positioned(lua: &Lua) -> LuaResult<LuaFunction> {
    let clone_pos = build_clone_positioned(lua)?;
    let clone_key = lua.create_registry_value(clone_pos)?;
    let merge_adj = build_merge_adjacent(lua)?;
    let merge_key = lua.create_registry_value(merge_adj)?;

    lua.create_function(move |lua, (base_tokens, overlay_tokens): (LuaTable, LuaValue)| {
        let clone_positioned: LuaFunction = lua.registry_value(&clone_key)?;
        let merge_adjacent: LuaFunction = lua.registry_value(&merge_key)?;

        let overlay_tbl = match overlay_tokens {
            LuaValue::Table(ref t) if t.raw_len() > 0 => t.clone(),
            _ => {
                let cloned: LuaTable = clone_positioned.call(base_tokens)?;
                return Ok(cloned);
            }
        };

        let result = lua.create_table()?;
        let mut overlay_idx: i64 = 1;
        let overlay_len = overlay_tbl.raw_len() as i64;
        let base_len = base_tokens.raw_len();
        let mut result_len: i64 = 0;

        for i in 1..=base_len {
            let base: LuaTable = base_tokens.raw_get(i)?;
            let base_pos: i64 = base.get("pos")?;
            let base_token_len: i64 = base.get("len")?;
            let base_end = base_pos + base_token_len;
            let mut cursor = base_pos;

            // Advance overlay_idx past tokens that end before cursor
            while overlay_idx <= overlay_len {
                let ov: LuaTable = overlay_tbl.raw_get(overlay_idx)?;
                let ov_pos: i64 = ov.get("pos")?;
                let ov_len: i64 = ov.get("len")?;
                if ov_pos + ov_len <= cursor {
                    overlay_idx += 1;
                } else {
                    break;
                }
            }

            let mut scan_idx = overlay_idx;
            while cursor < base_end {
                if scan_idx > overlay_len {
                    let entry = lua.create_table()?;
                    entry.set("type", base.get::<LuaValue>("type")?)?;
                    entry.set("pos", cursor)?;
                    entry.set("len", base_end - cursor)?;
                    result_len += 1;
                    result.raw_set(result_len, entry)?;
                    cursor = base_end;
                    continue;
                }

                let overlay: LuaTable = overlay_tbl.raw_get(scan_idx)?;
                let ov_pos: i64 = overlay.get("pos")?;
                let ov_len: i64 = overlay.get("len")?;

                if ov_pos >= base_end {
                    let entry = lua.create_table()?;
                    entry.set("type", base.get::<LuaValue>("type")?)?;
                    entry.set("pos", cursor)?;
                    entry.set("len", base_end - cursor)?;
                    result_len += 1;
                    result.raw_set(result_len, entry)?;
                    cursor = base_end;
                } else if ov_pos > cursor {
                    let entry = lua.create_table()?;
                    entry.set("type", base.get::<LuaValue>("type")?)?;
                    entry.set("pos", cursor)?;
                    entry.set("len", ov_pos - cursor)?;
                    result_len += 1;
                    result.raw_set(result_len, entry)?;
                    cursor = ov_pos;
                } else {
                    let overlay_end = (base_end).min(ov_pos + ov_len);
                    if overlay_end > cursor {
                        let entry = lua.create_table()?;
                        entry.set("type", overlay.get::<LuaValue>("type")?)?;
                        entry.set("pos", cursor)?;
                        entry.set("len", overlay_end - cursor)?;
                        result_len += 1;
                        result.raw_set(result_len, entry)?;
                        cursor = overlay_end;
                    } else {
                        scan_idx += 1;
                    }
                }
            }
        }

        let merged: LuaTable = merge_adjacent.call(result)?;
        Ok(merged)
    })
}

/// Registers `core.doc.highlighter` as a pure Rust preload.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;

    // Pre-build helper functions and store in registry so closures can share them
    let calc_sig = build_calc_signature(lua)?;
    let calc_sig_key = lua.create_registry_value(calc_sig)?;

    let pair_to_pos = build_pair_tokens_to_positioned(lua)?;
    let pair_to_pos_key = lua.create_registry_value(pair_to_pos)?;

    let pos_to_pair = build_positioned_to_pair_tokens(lua)?;
    let pos_to_pair_key = lua.create_registry_value(pos_to_pair)?;

    let clone_pos = build_clone_positioned(lua)?;
    let clone_pos_key = lua.create_registry_value(clone_pos)?;

    let overlay_pos = build_overlay_positioned(lua)?;
    let overlay_pos_key = lua.create_registry_value(overlay_pos)?;

    preload.set(
        "core.doc.highlighter",
        lua.create_function(move |lua, ()| {
            let core: LuaTable = require_table(lua, "core")?;
            let common: LuaTable = require_table(lua, "core.common")?;
            let tokenizer: LuaTable = require_table(lua, "core.tokenizer")?;
            let object: LuaTable = require_table(lua, "core.object")?;
            let native_tokenizer: LuaTable = require_table(lua, "native_tokenizer")?;

            let calc_signature: LuaFunction = lua.registry_value(&calc_sig_key)?;
            let pair_tokens_to_positioned: LuaFunction = lua.registry_value(&pair_to_pos_key)?;
            let positioned_to_pair_tokens: LuaFunction = lua.registry_value(&pos_to_pair_key)?;
            let clone_positioned: LuaFunction = lua.registry_value(&clone_pos_key)?;
            let overlay_positioned: LuaFunction = lua.registry_value(&overlay_pos_key)?;

            let highlighter: LuaTable = object.call_method("extend", ())?;

            // Highlighter:__tostring()
            highlighter.set(
                "__tostring",
                lua.create_function(|_, _self: LuaTable| Ok("Highlighter"))?,
            )?;

            // Highlighter:new(doc)
            highlighter.set(
                "new",
                lua.create_function(|_, (self_tbl, doc): (LuaTable, LuaTable)| {
                    self_tbl.set("doc", doc)?;
                    self_tbl.set("running", false)?;
                    let reset: LuaFunction = self_tbl.get("reset")?;
                    reset.call::<()>(self_tbl)?;
                    Ok(())
                })?,
            )?;

            // Highlighter:start()
            // Uses coroutine.yield, so we build the loop body in Rust but wrap
            // the yielding loop in a thin Lua function.
            {
                let cs_key = lua.create_registry_value(calc_signature.clone())?;
                let cp_key = lua.create_registry_value(clone_positioned.clone())?;
                let op_key = lua.create_registry_value(overlay_positioned.clone())?;
                let p2p_key = lua.create_registry_value(positioned_to_pair_tokens.clone())?;
                let core_key = lua.create_registry_value(core.clone())?;

                // tick(self) -- one batch of retokenization, returns true when done
                let tick = lua.create_function(move |lua, self_tbl: LuaTable| -> LuaResult<bool> {
                    let calc_signature: LuaFunction = lua.registry_value(&cs_key)?;
                    let clone_positioned: LuaFunction = lua.registry_value(&cp_key)?;
                    let overlay_positioned: LuaFunction = lua.registry_value(&op_key)?;
                    let positioned_to_pair_tokens: LuaFunction = lua.registry_value(&p2p_key)?;
                    let core: LuaTable = lua.registry_value(&core_key)?;

                    let first_invalid: i64 = self_tbl.get("first_invalid_line")?;
                    let max_wanted: i64 = self_tbl.get("max_wanted_line")?;

                    if first_invalid > max_wanted {
                        self_tbl.set("max_wanted_line", 0i64)?;
                        self_tbl.set("running", false)?;
                        return Ok(true);
                    }

                    let max: i64 = (first_invalid + 40).min(max_wanted);
                    let lines: LuaTable = self_tbl.get("lines")?;
                    let doc: LuaTable = self_tbl.get("doc")?;
                    let doc_lines: LuaTable = doc.get("lines")?;

                    let tokenize_line: LuaFunction = self_tbl.get("tokenize_line")?;
                    let update_notify: LuaFunction = self_tbl.get("update_notify")?;

                    let mut retokenized_from: Option<i64> = None;
                    let mut should_yield = false;

                    for i in first_invalid..=max {
                        let state: LuaValue = if i > 1 {
                            let prev_line: LuaValue = lines.raw_get(i - 1)?;
                            match prev_line {
                                LuaValue::Table(ref t) => t.get("state")?,
                                _ => LuaValue::Nil,
                            }
                        } else {
                            LuaValue::Nil
                        };

                        let line: LuaValue = lines.raw_get(i)?;
                        let doc_line_text: LuaValue = doc_lines.raw_get(i)?;

                        // Clear resume if state or text changed
                        if let LuaValue::Table(ref line_tbl) = line {
                            let resume: LuaValue = line_tbl.get("resume")?;
                            if !matches!(resume, LuaValue::Nil | LuaValue::Boolean(false)) {
                                let init_state: LuaValue = line_tbl.get("init_state")?;
                                let text: LuaValue = line_tbl.get("text")?;
                                if init_state != state || text != doc_line_text {
                                    line_tbl.set("resume", LuaValue::Nil)?;
                                }
                            }
                        }

                        let needs_retokenize = match line {
                            LuaValue::Table(ref line_tbl) => {
                                let init_state: LuaValue = line_tbl.get("init_state")?;
                                let text: LuaValue = line_tbl.get("text")?;
                                let resume: LuaValue = line_tbl.get("resume")?;
                                let has_resume =
                                    !matches!(resume, LuaValue::Nil | LuaValue::Boolean(false));
                                !(init_state == state && text == doc_line_text && !has_resume)
                            }
                            _ => true,
                        };

                        if needs_retokenize {
                            if retokenized_from.is_none() {
                                retokenized_from = Some(i);
                            }
                            let resume: LuaValue = match line {
                                LuaValue::Table(ref t) => t.get("resume")?,
                                _ => LuaValue::Nil,
                            };
                            let resume_arg = match resume {
                                LuaValue::Nil | LuaValue::Boolean(false) => LuaValue::Nil,
                                v => v,
                            };

                            let new_line: LuaTable = tokenize_line.call((
                                self_tbl.clone(),
                                i,
                                state,
                                resume_arg,
                            ))?;

                            // Preserve semantic tokens from old line
                            if let LuaValue::Table(ref old_line) = line {
                                let sem: LuaValue = old_line.get("semantic_tokens")?;
                                if let LuaValue::Table(ref sem_tbl) = sem {
                                    let cloned: LuaTable =
                                        clone_positioned.call(sem_tbl.clone())?;
                                    new_line.set("semantic_tokens", cloned)?;
                                    let base_pos: LuaTable = new_line.get("base_positioned")?;
                                    let sem2: LuaTable = new_line.get("semantic_tokens")?;
                                    let overlaid: LuaTable =
                                        overlay_positioned.call((base_pos, sem2))?;
                                    new_line.set("positioned", overlaid.clone())?;
                                    let text: LuaValue = new_line.get("text")?;
                                    let tokens: LuaTable =
                                        positioned_to_pair_tokens.call((overlaid.clone(), text))?;
                                    new_line.set("tokens", tokens)?;
                                    let sig: LuaValue = calc_signature.call(overlaid)?;
                                    new_line.set("signature", sig)?;
                                }
                            }

                            let new_resume: LuaValue = new_line.get("resume")?;
                            lines.raw_set(i, new_line)?;

                            if !matches!(new_resume, LuaValue::Nil | LuaValue::Boolean(false)) {
                                self_tbl.set("first_invalid_line", i)?;
                                should_yield = true;
                                if let Some(from) = retokenized_from {
                                    update_notify.call::<()>((
                                        self_tbl.clone(),
                                        from,
                                        max - from,
                                    ))?;
                                }
                                break;
                            }
                        } else if retokenized_from.is_some() {
                            let from = retokenized_from.unwrap();
                            update_notify.call::<()>((
                                self_tbl.clone(),
                                from,
                                i - from - 1,
                            ))?;
                            retokenized_from = None;
                        }
                    }

                    if !should_yield {
                        self_tbl.set("first_invalid_line", max + 1)?;
                        if let Some(from) = retokenized_from {
                            update_notify.call::<()>((self_tbl.clone(), from, max - from))?;
                        }
                    }

                    core.set("redraw", true)?;
                    Ok(false)
                })?;

                // Lua wrapper: loops tick and yields -- only Lua functions may yield
                let thread_factory: LuaFunction = lua.load(
                    "local tick = ...; return function(self) while true do if tick(self) then return end; coroutine.yield(0) end end",
                ).call(tick)?;
                let thread_factory_key = lua.create_registry_value(thread_factory)?;

                let core_key2 = lua.create_registry_value(core.clone())?;
                highlighter.set(
                    "start",
                    lua.create_function(move |lua, self_tbl: LuaTable| {
                        let running: bool = self_tbl.get("running")?;
                        if running {
                            return Ok(());
                        }
                        self_tbl.set("running", true)?;
                        let core: LuaTable = lua.registry_value(&core_key2)?;
                        let add_thread: LuaFunction = core.get("add_thread")?;
                        let thread_fn: LuaFunction = lua.registry_value(&thread_factory_key)?;
                        let bound: LuaFunction = lua
                            .load("local f, s = ...; return function() f(s) end")
                            .call::<LuaFunction>((thread_fn, self_tbl.clone()))?;
                        add_thread.call::<()>((bound, self_tbl))?;
                        Ok(())
                    })?,
                )?;
            }

            // _set_max_wanted_lines(self, amount) -- internal helper
            highlighter.set(
                "_set_max_wanted_lines",
                lua.create_function(|_, (self_tbl, amount): (LuaTable, i64)| {
                    self_tbl.set("max_wanted_line", amount)?;
                    let first_invalid: i64 = self_tbl.get("first_invalid_line")?;
                    if first_invalid <= amount {
                        let start: LuaFunction = self_tbl.get("start")?;
                        start.call::<()>(self_tbl)?;
                    }
                    Ok(())
                })?,
            )?;

            // Highlighter:reset()
            highlighter.set(
                "reset",
                lua.create_function(|lua, self_tbl: LuaTable| {
                    self_tbl.set("lines", lua.create_table()?)?;
                    let soft_reset: LuaFunction = self_tbl.get("soft_reset")?;
                    soft_reset.call::<()>(self_tbl)?;
                    Ok(())
                })?,
            )?;

            // Highlighter:soft_reset()
            highlighter.set(
                "soft_reset",
                lua.create_function(|_, self_tbl: LuaTable| {
                    let lines: LuaTable = self_tbl.get("lines")?;
                    let len = lines.raw_len();
                    for i in 1..=len {
                        lines.raw_set(i, false)?;
                    }
                    self_tbl.set("first_invalid_line", 1)?;
                    self_tbl.set("max_wanted_line", 0)?;
                    Ok(())
                })?,
            )?;

            // Highlighter:invalidate(idx)
            highlighter.set(
                "invalidate",
                lua.create_function(|_, (self_tbl, idx): (LuaTable, i64)| {
                    let first_invalid: i64 = self_tbl.get("first_invalid_line")?;
                    self_tbl.set("first_invalid_line", first_invalid.min(idx))?;
                    let max_wanted: i64 = self_tbl.get("max_wanted_line")?;
                    let doc: LuaTable = self_tbl.get("doc")?;
                    let doc_lines: LuaTable = doc.get("lines")?;
                    let doc_lines_len: i64 = doc_lines.raw_len() as i64;
                    let new_max = max_wanted.min(doc_lines_len);
                    let set_max: LuaFunction = self_tbl.get("_set_max_wanted_lines")?;
                    set_max.call::<()>((self_tbl, new_max))?;
                    Ok(())
                })?,
            )?;

            // Highlighter:insert_notify(line, n)
            {
                let common_key = lua.create_registry_value(common.clone())?;
                highlighter.set(
                    "insert_notify",
                    lua.create_function(move |lua, (self_tbl, line, n): (LuaTable, i64, i64)| {
                        let invalidate: LuaFunction = self_tbl.get("invalidate")?;
                        invalidate.call::<()>((self_tbl.clone(), line))?;
                        let blanks = lua.create_table()?;
                        for i in 1..=n {
                            blanks.raw_set(i, false)?;
                        }
                        let common: LuaTable = lua.registry_value(&common_key)?;
                        let splice: LuaFunction = common.get("splice")?;
                        let lines: LuaTable = self_tbl.get("lines")?;
                        splice.call::<()>((lines, line, 0, blanks))?;
                        Ok(())
                    })?,
                )?;
            }

            // Highlighter:remove_notify(line, n)
            {
                let common_key2 = lua.create_registry_value(common)?;
                highlighter.set(
                    "remove_notify",
                    lua.create_function(move |lua, (self_tbl, line, n): (LuaTable, i64, i64)| {
                        let invalidate: LuaFunction = self_tbl.get("invalidate")?;
                        invalidate.call::<()>((self_tbl.clone(), line))?;
                        let common: LuaTable = lua.registry_value(&common_key2)?;
                        let splice: LuaFunction = common.get("splice")?;
                        let lines: LuaTable = self_tbl.get("lines")?;
                        splice.call::<()>((lines, line, n))?;
                        Ok(())
                    })?,
                )?;
            }

            // Highlighter:update_notify(line, n) -- no-op base
            highlighter.set(
                "update_notify",
                lua.create_function(|_, (_self, _line, _n): (LuaTable, i64, i64)| Ok(()))?,
            )?;

            // Highlighter:tokenize_line(idx, state, resume)
            {
                let cs_key2 = lua.create_registry_value(calc_signature.clone())?;
                let ptp_key = lua.create_registry_value(pair_tokens_to_positioned)?;
                let cp_key2 = lua.create_registry_value(clone_positioned.clone())?;
                let core_key3 = lua.create_registry_value(core)?;
                let nt_key = lua.create_registry_value(native_tokenizer)?;

                highlighter.set(
                    "tokenize_line",
                    lua.create_function(
                        move |lua,
                              (self_tbl, idx, state, resume): (
                            LuaTable,
                            i64,
                            LuaValue,
                            LuaValue,
                        )| {
                            let calc_signature: LuaFunction = lua.registry_value(&cs_key2)?;
                            let pair_tokens_to_positioned: LuaFunction =
                                lua.registry_value(&ptp_key)?;
                            let clone_positioned: LuaFunction = lua.registry_value(&cp_key2)?;
                            let core: LuaTable = lua.registry_value(&core_key3)?;
                            let native_tokenizer: LuaTable = lua.registry_value(&nt_key)?;

                            let res = lua.create_table()?;
                            res.set("init_state", state.clone())?;
                            let doc: LuaTable = self_tbl.get("doc")?;
                            let doc_lines: LuaTable = doc.get("lines")?;
                            let text: LuaValue = doc_lines.raw_get(idx)?;
                            res.set("text", text.clone())?;

                            let syntax: LuaValue = doc.get("syntax")?;
                            let syntax_name: LuaValue = match syntax {
                                LuaValue::Table(ref t) => t.get("name")?,
                                _ => LuaValue::Nil,
                            };

                            let native_resume = match resume {
                                LuaValue::Table(ref t) => {
                                    let nr: LuaValue = t.get("native_resume")?;
                                    if matches!(nr, LuaValue::Nil) {
                                        resume.clone()
                                    } else {
                                        nr
                                    }
                                }
                                ref v => v.clone(),
                            };

                            let pcall: LuaFunction = lua.globals().get("pcall")?;
                            let tokenize_fn: LuaFunction =
                                native_tokenizer.get("tokenize_line")?;
                            let result: LuaMultiValue = pcall.call((
                                tokenize_fn,
                                syntax_name.clone(),
                                text.clone(),
                                state.clone(),
                                native_resume,
                            ))?;
                            let vals: Vec<LuaValue> = result.into_vec();
                            let ok = matches!(vals.first(), Some(LuaValue::Boolean(true)));

                            if ok {
                                let tokens = vals.get(1).cloned().unwrap_or(LuaValue::Nil);
                                let next_state = vals.get(2).cloned().unwrap_or(LuaValue::Nil);
                                let next_resume = vals.get(3).cloned().unwrap_or(LuaValue::Nil);
                                res.set("tokens", tokens)?;
                                res.set("state", next_state)?;
                                let resume_val = if matches!(
                                    next_resume,
                                    LuaValue::Nil | LuaValue::Boolean(false)
                                ) {
                                    LuaValue::Nil
                                } else {
                                    let wrapper = lua.create_table()?;
                                    wrapper.set("native_resume", next_resume)?;
                                    LuaValue::Table(wrapper)
                                };
                                res.set("resume", resume_val)?;
                            } else {
                                let err_msg = vals.get(1).cloned().unwrap_or(LuaValue::Nil);
                                let error_fn: LuaFunction = core.get("error")?;
                                error_fn.call::<()>((
                                    "Native tokenizer error for %s: %s",
                                    syntax_name,
                                    err_msg,
                                ))?;
                                let fallback = lua.create_table()?;
                                fallback.raw_set(1, "normal")?;
                                fallback.raw_set(2, text)?;
                                res.set("tokens", fallback)?;
                                let fallback_state = match state {
                                    LuaValue::Nil => {
                                        LuaValue::String(lua.create_string("\0")?)
                                    }
                                    v => v,
                                };
                                res.set("state", fallback_state)?;
                            }

                            let tokens: LuaTable = res.get("tokens")?;
                            let base_positioned: LuaTable =
                                pair_tokens_to_positioned.call(tokens)?;
                            res.set("base_positioned", base_positioned.clone())?;
                            let positioned: LuaTable =
                                clone_positioned.call(base_positioned)?;
                            let sig: LuaValue = calc_signature.call(positioned.clone())?;
                            res.set("positioned", positioned)?;
                            res.set("signature", sig)?;

                            Ok(res)
                        },
                    )?,
                )?;
            }

            // Highlighter:merge_line(idx, overlay_tokens)
            {
                let cs_key3 = lua.create_registry_value(calc_signature.clone())?;
                let cp_key3 = lua.create_registry_value(clone_positioned.clone())?;
                let op_key2 = lua.create_registry_value(overlay_positioned.clone())?;
                let p2p_key2 = lua.create_registry_value(positioned_to_pair_tokens.clone())?;

                highlighter.set(
                    "merge_line",
                    lua.create_function(
                        move |lua,
                              (self_tbl, idx, overlay_tokens): (LuaTable, i64, LuaValue)| {
                            let calc_signature: LuaFunction = lua.registry_value(&cs_key3)?;
                            let clone_positioned: LuaFunction = lua.registry_value(&cp_key3)?;
                            let overlay_positioned: LuaFunction =
                                lua.registry_value(&op_key2)?;
                            let positioned_to_pair_tokens: LuaFunction =
                                lua.registry_value(&p2p_key2)?;

                            let get_line: LuaFunction = self_tbl.get("get_line")?;
                            let line: LuaTable = get_line.call((self_tbl.clone(), idx))?;

                            let sem = match overlay_tokens {
                                LuaValue::Table(ref t) => {
                                    let cloned: LuaTable =
                                        clone_positioned.call(t.clone())?;
                                    LuaValue::Table(cloned)
                                }
                                _ => LuaValue::Nil,
                            };
                            line.set("semantic_tokens", sem.clone())?;

                            let base_pos: LuaTable = line.get("base_positioned")?;
                            let positioned: LuaTable =
                                overlay_positioned.call((base_pos, sem))?;
                            line.set("positioned", positioned.clone())?;

                            let text: LuaValue = line.get("text")?;
                            let tokens: LuaTable =
                                positioned_to_pair_tokens.call((positioned.clone(), text))?;
                            line.set("tokens", tokens)?;

                            let sig: LuaValue = calc_signature.call(positioned)?;
                            line.set("signature", sig)?;

                            let update_notify: LuaFunction = self_tbl.get("update_notify")?;
                            update_notify.call::<()>((self_tbl, idx, 0))?;
                            Ok(())
                        },
                    )?,
                )?;
            }

            // Highlighter:get_line_signature(idx)
            highlighter.set(
                "get_line_signature",
                lua.create_function(|_, (self_tbl, idx): (LuaTable, i64)| {
                    let lines: LuaTable = self_tbl.get("lines")?;
                    let line: LuaValue = lines.raw_get(idx)?;
                    match line {
                        LuaValue::Table(t) => {
                            let sig: LuaValue = t.get("signature")?;
                            Ok(sig)
                        }
                        _ => Ok(LuaValue::Integer(0)),
                    }
                })?,
            )?;

            // Highlighter:get_line(idx)
            {
                let cs_key4 = lua.create_registry_value(calc_signature)?;
                let cp_key4 = lua.create_registry_value(clone_positioned)?;
                let op_key3 = lua.create_registry_value(overlay_positioned)?;
                let p2p_key3 = lua.create_registry_value(positioned_to_pair_tokens)?;

                highlighter.set(
                    "get_line",
                    lua.create_function(
                        move |lua, (self_tbl, idx): (LuaTable, i64)| {
                            let calc_signature: LuaFunction = lua.registry_value(&cs_key4)?;
                            let clone_positioned: LuaFunction = lua.registry_value(&cp_key4)?;
                            let overlay_positioned: LuaFunction =
                                lua.registry_value(&op_key3)?;
                            let positioned_to_pair_tokens: LuaFunction =
                                lua.registry_value(&p2p_key3)?;

                            let lines: LuaTable = self_tbl.get("lines")?;
                            let doc: LuaTable = self_tbl.get("doc")?;
                            let doc_lines: LuaTable = doc.get("lines")?;
                            let line: LuaValue = lines.raw_get(idx)?;
                            let doc_line_text: LuaValue = doc_lines.raw_get(idx)?;

                            let needs_retokenize = match line {
                                LuaValue::Table(ref t) => {
                                    let text: LuaValue = t.get("text")?;
                                    text != doc_line_text
                                }
                                _ => true,
                            };

                            let result_line = if needs_retokenize {
                                let prev: LuaValue = lines.raw_get(idx - 1)?;
                                let prev_state = match prev {
                                    LuaValue::Table(ref t) => t.get::<LuaValue>("state")?,
                                    _ => LuaValue::Nil,
                                };
                                let tokenize_line: LuaFunction =
                                    self_tbl.get("tokenize_line")?;
                                let new_line: LuaTable = tokenize_line.call((
                                    self_tbl.clone(),
                                    idx,
                                    prev_state,
                                ))?;

                                // Preserve semantic tokens from old line
                                if let LuaValue::Table(ref old_line) = line {
                                    let sem: LuaValue = old_line.get("semantic_tokens")?;
                                    if let LuaValue::Table(ref sem_tbl) = sem {
                                        let cloned: LuaTable =
                                            clone_positioned.call(sem_tbl.clone())?;
                                        new_line.set("semantic_tokens", cloned)?;
                                        let base_pos: LuaTable =
                                            new_line.get("base_positioned")?;
                                        let sem2: LuaTable =
                                            new_line.get("semantic_tokens")?;
                                        let overlaid: LuaTable =
                                            overlay_positioned.call((base_pos, sem2))?;
                                        new_line.set("positioned", overlaid.clone())?;
                                        let text: LuaValue = new_line.get("text")?;
                                        let tokens: LuaTable = positioned_to_pair_tokens
                                            .call((overlaid.clone(), text))?;
                                        new_line.set("tokens", tokens)?;
                                        let sig: LuaValue =
                                            calc_signature.call(overlaid)?;
                                        new_line.set("signature", sig)?;
                                    }
                                }

                                lines.raw_set(idx, new_line.clone())?;
                                let update_notify: LuaFunction =
                                    self_tbl.get("update_notify")?;
                                update_notify.call::<()>((self_tbl.clone(), idx, 0))?;
                                new_line
                            } else {
                                match line {
                                    LuaValue::Table(t) => t,
                                    _ => {
                                        return Err(LuaError::runtime(
                                            "unexpected non-table line",
                                        ))
                                    }
                                }
                            };

                            let max_wanted: i64 = self_tbl.get("max_wanted_line")?;
                            let new_max = max_wanted.max(idx);
                            let set_max: LuaFunction =
                                self_tbl.get("_set_max_wanted_lines")?;
                            set_max.call::<()>((self_tbl, new_max))?;

                            Ok(result_line)
                        },
                    )?,
                )?;
            }

            // Highlighter:each_token(idx)
            {
                let tokenizer_key = lua.create_registry_value(tokenizer)?;
                highlighter.set(
                    "each_token",
                    lua.create_function(move |lua, (self_tbl, idx): (LuaTable, i64)| {
                        let tokenizer: LuaTable = lua.registry_value(&tokenizer_key)?;
                        let get_line: LuaFunction = self_tbl.get("get_line")?;
                        let line: LuaTable = get_line.call((self_tbl, idx))?;
                        let tokens: LuaTable = line.get("tokens")?;
                        let each_token: LuaFunction = tokenizer.get("each_token")?;
                        each_token.call::<LuaMultiValue>(tokens)
                    })?,
                )?;
            }

            Ok(LuaValue::Table(highlighter))
        })?,
    )
}
