use std::sync::Arc;

use mlua::prelude::*;

fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

fn install(lua: &Lua) -> LuaResult<()> {
    let core = require_table(lua, "core")?;
    let command = require_table(lua, "core.command")?;
    let keymap = require_table(lua, "core.keymap")?;

    let handled = lua.create_table()?;
    handled.set("keypressed", true)?;
    handled.set("keyreleased", true)?;
    handled.set("textinput", true)?;

    // Shared state: { state = "stopped", event_buffer = {}, modkeys = {} }
    let state_tbl = lua.create_table()?;
    state_tbl.set("state", "stopped")?;
    state_tbl.set("event_buffer", lua.create_table()?)?;
    state_tbl.set("modkeys", lua.create_table()?)?;
    let state_key = Arc::new(lua.create_registry_value(state_tbl)?);
    let handled_key = Arc::new(lua.create_registry_value(handled)?);

    // Patch core.on_event to record events during recording.
    let old_on_event: LuaFunction = core.get("on_event")?;
    let old_key = Arc::new(lua.create_registry_value(old_on_event)?);
    {
        let sk = state_key.clone();
        let hk = handled_key.clone();
        let ok = old_key.clone();
        core.set(
            "on_event",
            lua.create_function(move |lua, args: LuaMultiValue| {
                let old: LuaFunction = lua.registry_value(&ok)?;
                let res: LuaValue = old.call(args.clone())?;

                let st: LuaTable = lua.registry_value(&sk)?;
                let current: String = st.get("state")?;
                if current == "recording" {
                    let event_type = args.front().cloned().unwrap_or(LuaValue::Nil);
                    let type_str = match &event_type {
                        LuaValue::String(s) => {
                            s.to_str().map(|s| s.to_owned()).unwrap_or_default()
                        }
                        _ => String::new(),
                    };
                    let handled: LuaTable = lua.registry_value(&hk)?;
                    let is_handled: bool = handled.get(type_str.as_str()).unwrap_or(false);
                    if is_handled {
                        let buf: LuaTable = st.get("event_buffer")?;
                        let entry = lua.create_table()?;
                        for (idx, val) in args.into_iter().enumerate() {
                            entry.raw_set((idx + 1) as i64, val)?;
                        }
                        buf.push(entry)?;
                    }
                }
                Ok(res)
            })?,
        )?;
    }

    // Clone helper for shallow-copying keymap.modkeys.
    fn clone_table(lua: &Lua, src: &LuaTable) -> LuaResult<LuaTable> {
        let out = lua.create_table()?;
        for pair in src.pairs::<LuaValue, LuaValue>() {
            let (k, v) = pair?;
            out.set(k, v)?;
        }
        Ok(out)
    }

    // Predicate: allow commands only when not playing.
    let sk = state_key.clone();
    let predicate = lua.create_function(move |lua, ()| {
        let st: LuaTable = lua.registry_value(&sk)?;
        let current: String = st.get("state")?;
        Ok(current != "playing")
    })?;

    // Toggle-record command.
    let sk = state_key.clone();
    let toggle_record = lua.create_function(move |lua, ()| {
        let st: LuaTable = lua.registry_value(&sk)?;
        let core = require_table(lua, "core")?;
        let keymap = require_table(lua, "core.keymap")?;
        let current: String = st.get("state")?;
        if current == "stopped" {
            st.set("state", "recording")?;
            st.set("event_buffer", lua.create_table()?)?;
            let modkeys: LuaTable = keymap.get("modkeys")?;
            st.set("modkeys", clone_table(lua, &modkeys)?)?;
            core.call_function::<()>("log", "Recording macro...")?;
        } else {
            st.set("state", "stopped")?;
            let buf: LuaTable = st.get("event_buffer")?;
            let count = buf.len()?;
            core.call_function::<()>(
                "log",
                format!("Stopped recording macro ({count} events)"),
            )?;
        }
        Ok(())
    })?;

    // Play command.
    let sk = state_key.clone();
    let ok = old_key;
    let play = lua.create_function(move |lua, ()| {
        let st: LuaTable = lua.registry_value(&sk)?;
        st.set("state", "playing")?;

        let core = require_table(lua, "core")?;
        let keymap = require_table(lua, "core.keymap")?;
        let buf: LuaTable = st.get("event_buffer")?;
        let count = buf.len()?;
        core.call_function::<()>("log", format!("Playing macro... ({count} events)"))?;

        let orig_modkeys: LuaTable = keymap.get("modkeys")?;
        let saved_modkeys: LuaTable = st.get("modkeys")?;
        keymap.set("modkeys", clone_table(lua, &saved_modkeys)?)?;

        let on_event: LuaFunction = lua.registry_value(&ok)?;
        let root_view: LuaTable = core.get("root_view")?;
        for ev in buf.sequence_values::<LuaTable>() {
            let ev = ev?;
            let mut args = LuaMultiValue::new();
            for val in ev.sequence_values::<LuaValue>() {
                args.push_back(val?);
            }
            on_event.call::<()>(args)?;
            root_view.call_method::<()>("update", ())?;
        }

        keymap.set("modkeys", orig_modkeys)?;
        st.set("state", "stopped")?;
        Ok(())
    })?;

    let cmds = lua.create_table()?;
    cmds.set("macro:toggle-record", toggle_record)?;
    cmds.set("macro:play", play)?;
    command.call_function::<()>("add", (predicate, cmds))?;

    let bindings = lua.create_table()?;
    bindings.set("ctrl+shift+;", "macro:toggle-record")?;
    bindings.set("ctrl+;", "macro:play")?;
    keymap.call_function::<()>("add", bindings)?;

    Ok(())
}

/// Registers `plugins.macro`: event recording and playback with keybindings.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;
    preload.set(
        "plugins.macro",
        lua.create_function(|lua, ()| {
            install(lua)?;
            Ok(LuaValue::Boolean(true))
        })?,
    )
}
