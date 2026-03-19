use anyhow::{Context, Result};
use mlua::StdLib;
use mlua::prelude::*;

/// Lua bootstrap executed after the Rust runtime has prepared the Lua environment.
/// The bootstrap mirrors the xpcall wrapper in the original main.c.
const BOOTSTRAP: &str = r#"
local os_exit = os.exit
os.exit = function(code, close)
    os_exit(code, close == nil and true or close)
end

local core
xpcall(function()
    core = require(os.getenv("LITE_ANVIL_RUNTIME") or "core")
    core.init()
    core.run()
end, function(err)
    io.stderr:write("Error: " .. tostring(err) .. "\n")
    io.stderr:write(debug.traceback(nil, 2) .. "\n")
    if core and core.on_error then
        pcall(core.on_error, err)
    end
end)

return core and core.restart_request
"#;

fn install_debug_shim(lua: &Lua) -> LuaResult<()> {
    let debug = lua.create_table()?;

    debug.set(
        "traceback",
        lua.create_function(|lua, (msg, level): (Option<String>, Option<usize>)| {
            lua.traceback(msg.as_deref(), level.unwrap_or(1))
        })?,
    )?;

    debug.set(
        "getinfo",
        lua.create_function(|lua, (level, _what): (usize, Option<String>)| {
            let info = lua.create_table()?;
            if let Some((short_src, currentline)) = lua.inspect_stack(level, |debug| {
                let short_src = debug
                    .source()
                    .short_src
                    .map(|src| src.into_owned())
                    .unwrap_or_else(|| "[C]".to_string());
                let currentline = debug.current_line().unwrap_or(0);
                (short_src, currentline)
            }) {
                info.set("short_src", short_src)?;
                info.set("currentline", currentline)?;
            } else {
                info.set("short_src", "[C]")?;
                info.set("currentline", 0)?;
            }
            Ok(info)
        })?,
    )?;

    lua.globals().set("debug", debug)
}

/// Initialise one Lua VM lifecycle. Returns true if the editor requested a restart.
pub fn run(args: &[String], restarted: bool) -> Result<bool> {
    let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default())
        .map_err(anyhow::Error::from)
        .context("could not create safe Lua VM")?;

    crate::api::register_stubs(&lua)?;
    let runtime = crate::runtime::RuntimeContext::discover()?;
    runtime.configure_lua(&lua, args, restarted)?;
    install_debug_shim(&lua)?;

    let restart: bool = lua
        .load(BOOTSTRAP)
        .set_name("bootstrap")
        .eval()
        .unwrap_or(false);

    Ok(restart)
}

#[cfg(test)]
mod tests {
    use super::install_debug_shim;
    use mlua::{Lua, LuaOptions, StdLib};

    #[test]
    fn safe_vm_debug_shim_supports_traceback_and_getinfo() {
        let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default()).expect("lua");
        install_debug_shim(&lua).expect("debug shim");
        lua.load(
            r#"
            local info = debug.getinfo(1, "Sl")
            assert(type(info.short_src) == "string")
            assert(type(info.currentline) == "number")
            local tb = debug.traceback("", 1)
            assert(type(tb) == "string")
        "#,
        )
        .exec()
        .expect("lua exec");
    }
}
