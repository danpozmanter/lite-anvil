use mlua::prelude::*;

fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

fn check_autorestart(lua: &Lua, this: &LuaTable) -> LuaResult<()> {
    let abs_filename: Option<String> = this.get("abs_filename")?;
    let path = match abs_filename {
        Some(p) => p,
        None => return Ok(()),
    };

    let userdir: String = lua.globals().get("USERDIR")?;
    let pathsep: String = lua.globals().get("PATHSEP")?;

    let core = require_table(lua, "core")?;
    let project_path: Option<String> = (|| -> LuaResult<Option<String>> {
        let root_project: Option<LuaFunction> = core.get("root_project")?;
        if let Some(f) = root_project {
            let proj: Option<LuaTable> = f.call(())?;
            if let Some(p) = proj {
                return p.get("path");
            }
        }
        Ok(None)
    })()?;

    let affordance = require_table(lua, "affordance_model")?;
    let proj_arg: LuaValue = match project_path {
        Some(p) => lua.create_string(p)?.into_lua(lua)?,
        None => LuaValue::Nil,
    };
    let should_restart: bool =
        affordance.call_function("should_autorestart", (path, userdir, pathsep, proj_arg))?;

    if should_restart {
        let command = require_table(lua, "core.command")?;
        let _: LuaValue = command.call_function("perform", "core:restart")?;
    }
    Ok(())
}

fn patch_doc_save(lua: &Lua) -> LuaResult<()> {
    let doc = require_table(lua, "core.doc")?;
    let old_save: LuaFunction = doc.get("save")?;
    let old_key = lua.create_registry_value(old_save)?;

    doc.set(
        "save",
        lua.create_function(move |lua, (this, args): (LuaTable, LuaMultiValue)| {
            let old: LuaFunction = lua.registry_value(&old_key)?;
            let mut call_args = LuaMultiValue::new();
            call_args.push_back(LuaValue::Table(this.clone()));
            call_args.extend(args.iter().cloned());
            let res: LuaMultiValue = old.call(call_args)?;

            // Equivalent to Lua pcall: catch errors and report via core.error.
            if let Err(e) = check_autorestart(lua, &this) {
                let name: String = this.call_method("get_name", ()).unwrap_or_default();
                if let Ok(core) = require_table(lua, "core") {
                    let _: LuaResult<()> = core.call_function(
                        "error",
                        format!("Post-save autorestart hook failed for {}: {}", name, e),
                    );
                }
            }

            Ok(res)
        })?,
    )?;
    Ok(())
}

/// Registers `plugins.autorestart`: patches `Doc.save` to trigger `core:restart`
/// when a config file matching `affordance_model.should_autorestart` is saved.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;
    preload.set(
        "plugins.autorestart",
        lua.create_function(|lua, ()| {
            patch_doc_save(lua)?;
            Ok(LuaValue::Boolean(true))
        })?,
    )
}
