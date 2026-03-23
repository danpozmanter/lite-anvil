use mlua::prelude::*;

use std::sync::Arc;

/// Require a module by name, returning the loaded table.
fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

fn set_config_defaults(lua: &Lua) -> LuaResult<()> {
    let config = require_table(lua, "core.config")?;
    let plugins: LuaTable = config.get("plugins")?;
    let common = require_table(lua, "core.common")?;

    let userdir: String = lua.globals().get("USERDIR")?;
    let pathsep: String = lua.globals().get("PATHSEP")?;

    let defaults = lua.create_table()?;
    defaults.set("mount_root", format!("{userdir}{pathsep}remote-ssh"))?;
    defaults.set("sshfs_binary", "sshfs")?;
    let opts = lua.create_sequence_from([
        "reconnect",
        "ServerAliveInterval=15",
        "ServerAliveCountMax=3",
        "auto_cache",
        "follow_symlinks",
    ])?;
    defaults.set("sshfs_options", opts)?;
    defaults.set("mount_timeout", 30)?;
    defaults.set("unmount_timeout", 15)?;

    let merged: LuaTable =
        common.call_function("merge", (defaults, plugins.get::<LuaValue>("remotessh")?))?;
    plugins.set("remotessh", merged)?;
    Ok(())
}

fn trim(text: &str) -> &str {
    text.trim()
}

fn parse_remote_spec(spec: &str) -> Result<String, String> {
    let trimmed = trim(spec);
    if trimmed.is_empty() {
        return Err("remote path is empty".to_string());
    }
    // Check format: something:something
    if !trimmed.contains(':') || trimmed.starts_with(':') || trimmed.ends_with(':') {
        return Err("expected format user@host:/absolute/path".to_string());
    }
    // Ensure the part after : is non-empty
    let parts: Vec<&str> = trimmed.splitn(2, ':').collect();
    if parts.len() != 2 || parts[1].is_empty() {
        return Err("expected format user@host:/absolute/path".to_string());
    }
    Ok(trimmed.to_string())
}

fn sanitize_mount_name(spec: &str) -> String {
    let s = spec.strip_prefix("ssh://").unwrap_or(spec);
    let mut result = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch.is_alphanumeric() || ch == '.' || ch == '-' {
            result.push(ch);
        } else if result.ends_with('_') {
            // Skip consecutive underscores
        } else {
            result.push('_');
        }
    }
    result
}

/// Registers `plugins.remotessh`: mounts remote directories via sshfs and opens them as projects.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;
    preload.set(
        "plugins.remotessh",
        lua.create_function(|lua, ()| {
            set_config_defaults(lua)?;

            // State: mounts_by_spec, mounts_by_path, mount_counter
            let state = lua.create_table()?;
            state.set("mounts_by_spec", lua.create_table()?)?;
            state.set("mounts_by_path", lua.create_table()?)?;
            state.set("mount_counter", 0i64)?;
            let state_key = Arc::new(lua.create_registry_value(state)?);

            // run_command(argv, timeout) -> stdout or nil, err
            let run_command = lua.create_function(|lua, (argv, timeout): (LuaTable, f64)| {
                let process: LuaTable = lua.globals().get("process")?;
                let proc: LuaTable = process.call_function("start", argv)?;
                let exit_code: Option<i64> = proc.call_method("wait", timeout)?;
                let stdout_stream: LuaTable = proc.get("stdout")?;
                let stderr_stream: LuaTable = proc.get("stderr")?;
                let stdout: String =
                    stdout_stream.call_method::<Option<String>>("read", "all")?
                    .unwrap_or_default();
                let stderr: String =
                    stderr_stream.call_method::<Option<String>>("read", "all")?
                    .unwrap_or_default();
                if exit_code != Some(0) {
                    let msg = if !stderr.trim().is_empty() {
                        stderr.trim().to_string()
                    } else if !stdout.trim().is_empty() {
                        stdout.trim().to_string()
                    } else {
                        "command failed".to_string()
                    };
                    Ok((LuaValue::Nil, Some(msg)))
                } else {
                    Ok((LuaValue::String(lua.create_string(&stdout)?), None))
                }
            })?;
            let run_command_key = Arc::new(lua.create_registry_value(run_command)?);

            // mount_remote
            let sk = state_key.clone();
            let rck = Arc::clone(&run_command_key);
            let mount_remote = lua.create_function(move |lua, spec: String| {
                let state: LuaTable = lua.registry_value(&sk)?;
                let mounts_by_spec: LuaTable = state.get("mounts_by_spec")?;
                let mounts_by_path: LuaTable = state.get("mounts_by_path")?;

                // Already mounted?
                let existing: LuaValue = mounts_by_spec.get(spec.as_str())?;
                if !matches!(existing, LuaValue::Nil) {
                    return Ok((existing, LuaValue::Nil));
                }

                let common = require_table(lua, "core.common")?;
                let config = require_table(lua, "core.config")?;
                let plugins: LuaTable = config.get("plugins")?;
                let cfg: LuaTable = plugins.get("remotessh")?;
                let mount_root: String = cfg.get("mount_root")?;

                // Ensure mount root
                let result: LuaMultiValue = common.call_function("mkdirp", mount_root.as_str())?;
                let vals: Vec<LuaValue> = result.into_vec();
                if vals.len() >= 2 {
                    if let LuaValue::String(ref err) = vals[1] {
                        let err_str = err.to_str().map(|s| s.to_owned()).unwrap_or_default();
                        if !err_str.is_empty() && err_str != "path exists" {
                            let path_str = vals.get(2)
                                .and_then(|v| v.as_string().map(|s| s.to_string_lossy()))
                                .unwrap_or_else(|| mount_root.clone());
                            return Ok((
                                LuaValue::Nil,
                                LuaValue::String(
                                    lua.create_string(format!("{err_str}: {path_str}"))?,
                                ),
                            ));
                        }
                    }
                }

                // Make mountpoint
                let counter: i64 = state.get("mount_counter")?;
                let counter = counter + 1;
                state.set("mount_counter", counter)?;
                let pathsep: String = lua.globals().get("PATHSEP")?;
                let dirname = format!("{}-{:04}", sanitize_mount_name(&spec), counter);
                let mountpoint = format!("{mount_root}{pathsep}{dirname}");

                let _: LuaMultiValue =
                    common.call_function("mkdirp", mountpoint.as_str())?;

                // Build sshfs command
                let argv = lua.create_table()?;
                let sshfs_binary: String = cfg.get("sshfs_binary")?;
                argv.push(sshfs_binary)?;
                argv.push(spec.as_str())?;
                argv.push(mountpoint.as_str())?;
                let sshfs_options: LuaTable = cfg.get("sshfs_options")?;
                if sshfs_options.raw_len() > 0 {
                    argv.push("-o")?;
                    let table_concat: LuaFunction =
                        lua.globals().get::<LuaTable>("table")?.get("concat")?;
                    let opts_str: String = table_concat.call((sshfs_options, ","))?;
                    argv.push(opts_str)?;
                }

                let mount_timeout: f64 = cfg.get("mount_timeout")?;
                let run_cmd: LuaFunction = lua.registry_value(&rck)?;
                let (_, mount_err): (LuaValue, Option<String>) =
                    run_cmd.call((argv, mount_timeout))?;

                if let Some(err) = mount_err {
                    common.call_function::<()>("rm", (mountpoint.as_str(), false))?;
                    return Ok((
                        LuaValue::Nil,
                        LuaValue::String(lua.create_string(&err)?),
                    ));
                }

                mounts_by_spec.set(spec.as_str(), mountpoint.as_str())?;
                mounts_by_path.set(mountpoint.as_str(), spec.as_str())?;
                Ok((
                    LuaValue::String(lua.create_string(&mountpoint)?),
                    LuaValue::Nil,
                ))
            })?;
            let mount_remote_key = Arc::new(lua.create_registry_value(mount_remote)?);

            // unmount_remote_path
            let sk = state_key.clone();
            let rck = Arc::clone(&run_command_key);
            let unmount_remote_path = lua.create_function(move |lua, mountpoint: String| {
                let state: LuaTable = lua.registry_value(&sk)?;
                let mounts_by_path: LuaTable = state.get("mounts_by_path")?;
                let mounts_by_spec: LuaTable = state.get("mounts_by_spec")?;
                let common = require_table(lua, "core.common")?;

                let spec: LuaValue = mounts_by_path.get(mountpoint.as_str())?;
                if matches!(spec, LuaValue::Nil) {
                    return Ok((true, LuaValue::Nil));
                }

                let config = require_table(lua, "core.config")?;
                let plugins: LuaTable = config.get("plugins")?;
                let cfg: LuaTable = plugins.get("remotessh")?;
                let unmount_timeout: f64 = cfg.get("unmount_timeout")?;

                // Build unmount command
                let platform: String = lua.globals().get("PLATFORM")?;
                let argv = lua.create_table()?;
                if platform == "Mac OS X" {
                    argv.push("umount")?;
                    argv.push(mountpoint.as_str())?;
                } else {
                    let system: LuaTable = lua.globals().get("system")?;
                    let fm3: Option<LuaTable> =
                        system.call_function("get_file_info", "/usr/bin/fusermount3")?;
                    let fm: Option<LuaTable> =
                        system.call_function("get_file_info", "/usr/bin/fusermount")?;
                    if fm3.is_some() {
                        argv.push("/usr/bin/fusermount3")?;
                        argv.push("-u")?;
                    } else if fm.is_some() {
                        argv.push("/usr/bin/fusermount")?;
                        argv.push("-u")?;
                    } else {
                        argv.push("umount")?;
                    }
                    argv.push(mountpoint.as_str())?;
                }

                let run_cmd: LuaFunction = lua.registry_value(&rck)?;
                let (_, err): (LuaValue, Option<String>) =
                    run_cmd.call((argv, unmount_timeout))?;
                if let Some(err) = err {
                    return Ok((false, LuaValue::String(lua.create_string(&err)?)));
                }

                if let LuaValue::String(ref s) = spec {
                    mounts_by_spec.set(s.to_str()?, LuaValue::Nil)?;
                }
                mounts_by_path.set(mountpoint.as_str(), LuaValue::Nil)?;
                common.call_function::<()>("rm", (mountpoint.as_str(), false))?;
                Ok((true, LuaValue::Nil))
            })?;
            let unmount_key = Arc::new(lua.create_registry_value(unmount_remote_path)?);

            // attach_remote_project helper
            let attach_fn = lua.create_function(|lua, (project, spec): (LuaTable, String)| {
                let state_key_inner = lua; // just need lua
                let _ = state_key_inner;
                project.set("name", spec.as_str())?;
                project.set("remote_ssh_spec", spec)?;
                Ok(project)
            })?;
            let attach_key = Arc::new(lua.create_registry_value(attach_fn)?);

            // connect_remote_project
            let _sk = state_key.clone();
            let mrk = Arc::clone(&mount_remote_key);
            let ak = Arc::clone(&attach_key);
            let connect_fn = lua.create_function(move |lua, (spec, add_only): (String, bool)| {
                let core = require_table(lua, "core")?;
                let mount_fn: LuaFunction = lua.registry_value(&mrk)?;
                let attach: LuaFunction = lua.registry_value(&ak)?;
                let spec_clone = spec.clone();

                // Build thread body as Lua function wrapping Rust tick
                // (coroutine.yield cannot be called from Rust closures)
                let mount_key = lua.create_registry_value(mount_fn)?;
                let attach_key_inner = lua.create_registry_value(attach)?;
                let tick = lua.create_function(
                    move |lua, ()| -> LuaResult<()> {
                        let mount_fn: LuaFunction = lua.registry_value(&mount_key)?;
                        let (result, err): (LuaValue, LuaValue) =
                            mount_fn.call(spec_clone.as_str())?;
                        if matches!(result, LuaValue::Nil) {
                            let core = require_table(lua, "core")?;
                            let err_str = match &err {
                                LuaValue::String(s) => s.to_str()?.to_string(),
                                _ => "unknown error".to_string(),
                            };
                            core.call_function::<()>(
                                "error",
                                format!("Remote SSH mount failed: {err_str}"),
                            )?;
                            return Ok(());
                        }

                        let mountpoint = match &result {
                            LuaValue::String(s) => s.to_str()?.to_string(),
                            _ => return Ok(()),
                        };

                        let core = require_table(lua, "core")?;
                        let attach: LuaFunction = lua.registry_value(&attach_key_inner)?;
                        if add_only {
                            let project: LuaTable =
                                core.call_function("add_project", mountpoint.as_str())?;
                            attach.call::<()>((project, spec.as_str()))?;
                        } else {
                            let project: LuaTable =
                                core.call_function("set_project", mountpoint.as_str())?;
                            let root_view: LuaTable = core.get("root_view")?;
                            root_view.call_method::<()>("close_all_docviews", ())?;
                            attach.call::<()>((project, spec.as_str()))?;
                        }
                        core.call_function::<()>(
                            "log",
                            format!("Mounted remote project {:?}", spec),
                        )?;
                        Ok(())
                    },
                )?;

                core.call_function::<()>("add_thread", tick)?;
                Ok(())
            })?;
            let connect_key = Arc::new(lua.create_registry_value(connect_fn)?);

            // Patch core.add_project
            let core = require_table(lua, "core")?;
            {
                let old: LuaFunction = core.get("add_project")?;
                let old_key = lua.create_registry_value(old)?;
                let sk = state_key.clone();
                core.set(
                    "add_project",
                    lua.create_function(move |lua, project: LuaValue| {
                        let old: LuaFunction = lua.registry_value(&old_key)?;
                        let added: LuaTable = old.call(project)?;
                        let state: LuaTable = lua.registry_value(&sk)?;
                        let mounts_by_path: LuaTable = state.get("mounts_by_path")?;
                        let path: String = added.get("path")?;
                        let spec: LuaValue = mounts_by_path.get(path.as_str())?;
                        if let LuaValue::String(s) = spec {
                            added.set("name", s.to_str()?)?;
                            added.set("remote_ssh_spec", s.to_str()?)?;
                        }
                        Ok(added)
                    })?,
                )?;
            }

            // Patch core.set_project
            {
                let old: LuaFunction = core.get("set_project")?;
                let old_key = lua.create_registry_value(old)?;
                let sk = state_key.clone();
                core.set(
                    "set_project",
                    lua.create_function(move |lua, project: LuaValue| {
                        let old: LuaFunction = lua.registry_value(&old_key)?;
                        let set: LuaTable = old.call(project)?;
                        let state: LuaTable = lua.registry_value(&sk)?;
                        let mounts_by_path: LuaTable = state.get("mounts_by_path")?;
                        let path: String = set.get("path")?;
                        let spec: LuaValue = mounts_by_path.get(path.as_str())?;
                        if let LuaValue::String(s) = spec {
                            set.set("name", s.to_str()?)?;
                            set.set("remote_ssh_spec", s.to_str()?)?;
                        }
                        Ok(set)
                    })?,
                )?;
            }

            // Patch core.remove_project
            {
                let old: LuaFunction = core.get("remove_project")?;
                let old_key = lua.create_registry_value(old)?;
                let sk = state_key.clone();
                let uk = Arc::clone(&unmount_key);
                core.set(
                    "remove_project",
                    lua.create_function(move |lua, (project, force): (LuaValue, LuaValue)| {
                        let old: LuaFunction = lua.registry_value(&old_key)?;
                        let removed: LuaValue = old.call((project, force))?;
                        if let LuaValue::Table(ref removed_t) = removed {
                            let state: LuaTable = lua.registry_value(&sk)?;
                            let mounts_by_path: LuaTable = state.get("mounts_by_path")?;
                            let path: String = removed_t.get("path")?;
                            let spec: LuaValue = mounts_by_path.get(path.as_str())?;
                            if !matches!(spec, LuaValue::Nil) {
                                let unmount_fn: LuaFunction = lua.registry_value(&uk)?;
                                let (ok, err): (bool, LuaValue) = unmount_fn.call(path.as_str())?;
                                if !ok {
                                    let core = require_table(lua, "core")?;
                                    let remote_spec: String = removed_t
                                        .get::<Option<String>>("remote_ssh_spec")?
                                        .unwrap_or(path);
                                    let err_str = match &err {
                                        LuaValue::String(s) => {
                                            s.to_str().map(|s| s.to_owned()).unwrap_or_default()
                                        }
                                        _ => "unknown error".to_string(),
                                    };
                                    core.call_function::<()>(
                                        "warn",
                                        format!(
                                            "Remote SSH unmount failed for {:?}: {}",
                                            remote_spec, err_str
                                        ),
                                    )?;
                                }
                            }
                        }
                        Ok(removed)
                    })?,
                )?;
            }

            // Commands
            let command = require_table(lua, "core.command")?;
            let cmds = lua.create_table()?;

            let ck = Arc::clone(&connect_key);
            cmds.set(
                "remote-ssh:open-project",
                lua.create_function(move |lua, ()| {
                    let core = require_table(lua, "core")?;
                    let command_view: LuaTable = core.get("command_view")?;
                    let ck2 = Arc::clone(&ck);
                    let opts = lua.create_table()?;
                    opts.set(
                        "submit",
                        lua.create_function(move |lua, text: String| {
                            match parse_remote_spec(&text) {
                                Err(e) => {
                                    let core = require_table(lua, "core")?;
                                    core.call_function::<()>("error", e)?;
                                }
                                Ok(spec) => {
                                    let connect: LuaFunction = lua.registry_value(&ck2)?;
                                    connect.call::<()>((spec, false))?;
                                }
                            }
                            Ok(())
                        })?,
                    )?;
                    command_view.call_method::<()>("enter", ("Remote SSH Project", opts))
                })?,
            )?;

            let ck = Arc::clone(&connect_key);
            cmds.set(
                "remote-ssh:add-project",
                lua.create_function(move |lua, ()| {
                    let core = require_table(lua, "core")?;
                    let command_view: LuaTable = core.get("command_view")?;
                    let ck2 = Arc::clone(&ck);
                    let opts = lua.create_table()?;
                    opts.set(
                        "submit",
                        lua.create_function(move |lua, text: String| {
                            match parse_remote_spec(&text) {
                                Err(e) => {
                                    let core = require_table(lua, "core")?;
                                    core.call_function::<()>("error", e)?;
                                }
                                Ok(spec) => {
                                    let connect: LuaFunction = lua.registry_value(&ck2)?;
                                    connect.call::<()>((spec, true))?;
                                }
                            }
                            Ok(())
                        })?,
                    )?;
                    command_view.call_method::<()>("enter", ("Add Remote SSH Project", opts))
                })?,
            )?;

            command.call_function::<()>("add", (LuaValue::Nil, cmds))?;

            Ok(LuaValue::Boolean(true))
        })?,
    )
}
