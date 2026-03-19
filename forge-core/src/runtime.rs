use anyhow::{Context, Result};
use mlua::prelude::*;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

const LUA_RUNTIME_SETUP: &str = r#"
package.native_plugins = {}
package.searchers = { package.searchers[1], package.searchers[2], function(modname)
  local path, err = package.searchpath(modname, package.cpath)
  if not path then return err end
  return system.load_native_plugin, path
end }

table.pack = table.pack or pack or function(...) return {...} end
table.unpack = table.unpack or unpack

local lua_require = require
local require_stack = { "" }

function require(modname, ...)
  if modname then
    local level, rel_path = string.match(modname, "^(%.*)(.*)")
    level = #(level or "")
    if level > 0 then
      if #require_stack == 0 then
        return error("Require stack underflowed.")
      else
        local base_path = require_stack[#require_stack]
        while level > 1 do
          base_path = string.match(base_path, "^(.*)%.") or ""
          level = level - 1
        end
        modname = base_path
        if #base_path > 0 then
          modname = modname .. "."
        end
        modname = modname .. rel_path
      end
    end
  end

  table.insert(require_stack, modname)
  local ok, result, loaderdata = pcall(lua_require, modname, ...)
  table.remove(require_stack)

  if not ok then
    return error(result)
  end
  return result, loaderdata
end

function get_current_require_path()
  return require_stack[#require_stack]
end

require "core.utf8string"
require "core.process"
"#;

pub struct RuntimeContext {
    exe_file: PathBuf,
    exe_dir: PathBuf,
    data_dir: PathBuf,
    user_dir: PathBuf,
    path_sep: char,
    scale: f64,
}

impl RuntimeContext {
    pub fn discover() -> Result<Self> {
        apply_appimage_workdir_fix()?;

        let exe_file = std::env::current_exe().context("could not resolve executable path")?;
        let exe_dir = exe_file
            .parent()
            .context("executable has no parent directory")?
            .to_path_buf();
        let data_dir = find_data_dir(&exe_dir);
        let path_sep = std::path::MAIN_SEPARATOR;
        let user_dir = find_user_dir(&exe_dir, path_sep);
        let scale = std::env::var("LITE_SCALE")
            .ok()
            .or_else(|| std::env::var("GDK_SCALE").ok())
            .or_else(|| std::env::var("QT_SCALE_FACTOR").ok())
            .and_then(|raw| raw.parse::<f64>().ok())
            .unwrap_or(1.0);

        Ok(Self {
            exe_file,
            exe_dir,
            data_dir,
            user_dir,
            path_sep,
            scale,
        })
    }

    pub fn configure_lua(&self, lua: &Lua, args: &[String], restarted: bool) -> Result<()> {
        let globals = lua.globals();

        let args_table = lua.create_table()?;
        for (i, arg) in args.iter().enumerate() {
            args_table.set(i as i64 + 1, arg.as_str())?;
        }
        globals.set("ARGS", args_table)?;

        globals.set("PLATFORM", platform_name())?;
        globals.set("ARCH", arch_tuple())?;
        globals.set("RESTARTED", restarted)?;
        globals.set("VERSION", env!("CARGO_PKG_VERSION"))?;
        globals.set("MOD_VERSION_MAJOR", 4)?;
        globals.set("MOD_VERSION_MINOR", 0)?;
        globals.set("MOD_VERSION_PATCH", 0)?;
        globals.set("MOD_VERSION_STRING", "4.0.0")?;
        globals.set("EXEFILE", self.exe_file.to_string_lossy().as_ref())?;
        globals.set("EXEDIR", self.exe_dir.to_string_lossy().as_ref())?;
        globals.set("DATADIR", self.data_dir.to_string_lossy().as_ref())?;
        globals.set("USERDIR", self.user_dir.to_string_lossy().as_ref())?;
        globals.set("PATHSEP", self.path_sep.to_string())?;
        globals.set("SCALE", self.scale)?;

        let home_key = if cfg!(target_os = "windows") {
            "USERPROFILE"
        } else {
            "HOME"
        };
        if let Ok(home) = std::env::var(home_key) {
            globals.set("HOME", home)?;
        }

        let package: LuaTable = globals.get("package")?;
        package.set("path", lua_package_path(&self.data_dir, &self.user_dir))?;
        package.set("cpath", lua_package_cpath(&self.data_dir, &self.user_dir))?;

        lua.load(LUA_RUNTIME_SETUP)
            .set_name("rust_runtime_setup")
            .exec()?;

        Ok(())
    }
}

fn lua_package_path(data_dir: &Path, user_dir: &Path) -> String {
    let data = data_dir.to_string_lossy();
    let user = user_dir.to_string_lossy();
    format!("{data}/?.lua;{data}/?/init.lua;{user}/?.lua;{user}/?/init.lua;")
}

fn lua_package_cpath(data_dir: &Path, user_dir: &Path) -> String {
    let suffix = if cfg!(target_os = "windows") {
        "dll"
    } else {
        "so"
    };
    let arch = arch_tuple();
    let data = data_dir.to_string_lossy();
    let user = user_dir.to_string_lossy();
    format!(
        "{user}/?.{arch}.{suffix};\
{user}/?/init.{arch}.{suffix};\
{user}/?.{suffix};\
{user}/?/init.{suffix};\
{data}/?.{arch}.{suffix};\
{data}/?/init.{arch}.{suffix};\
{data}/?.{suffix};\
{data}/?/init.{suffix};"
    )
}

fn apply_appimage_workdir_fix() -> Result<()> {
    if std::env::var_os("APPIMAGE").is_some()
        && let Some(owd) = std::env::var_os("OWD")
    {
        std::env::set_current_dir(&owd).with_context(|| {
            format!(
                "could not restore AppImage cwd to {}",
                PathBuf::from(&owd).display()
            )
        })?;
    }
    Ok(())
}

fn find_data_dir(exe_dir: &Path) -> PathBuf {
    fn is_data_dir(candidate: &Path) -> bool {
        candidate.join("core").join("utf8string.lua").exists()
            && candidate.join("colors").join("default.lua").exists()
    }

    if let Some(prefix) = std::env::var_os("LITE_PREFIX") {
        return PathBuf::from(prefix).join("share").join("lite-anvil");
    }

    if exe_dir.file_name() == Some(OsStr::new("bin"))
        && let Some(prefix) = exe_dir.parent()
    {
        let candidate = prefix.join("share").join("lite-anvil");
        if is_data_dir(&candidate) {
            return candidate;
        }
    }

    let mut dir = exe_dir.to_path_buf();
    for _ in 0..6 {
        let candidate = dir.join("data");
        if is_data_dir(&candidate) {
            return candidate;
        }
        if !dir.pop() {
            break;
        }
    }

    exe_dir.join("data")
}

fn find_user_dir(exe_dir: &Path, path_sep: char) -> PathBuf {
    let bundled = exe_dir.join("user");
    if bundled.exists() {
        return bundled;
    }

    if let Some(user_dir) = std::env::var_os("LITE_USERDIR") {
        return PathBuf::from(user_dir);
    }

    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("lite-anvil");
    }

    if let Some(home) = std::env::var_os(if cfg!(target_os = "windows") {
        "USERPROFILE"
    } else {
        "HOME"
    }) {
        let mut path = PathBuf::from(home);
        if path_sep == '\\' {
            path.push("lite-anvil");
        } else {
            path.push(".config");
            path.push("lite-anvil");
        }
        return path;
    }

    bundled
}

fn platform_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "Mac OS X"
    } else if cfg!(target_os = "linux") {
        "Linux"
    } else if cfg!(target_os = "freebsd") {
        "FreeBSD"
    } else if cfg!(target_os = "openbsd") {
        "OpenBSD"
    } else if cfg!(target_os = "netbsd") {
        "NetBSD"
    } else {
        "Unknown"
    }
}

fn arch_tuple() -> String {
    let cpu = std::env::consts::ARCH;
    let os = match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "darwin",
        "windows" => "windows",
        "freebsd" => "freebsd",
        "openbsd" => "openbsd",
        "netbsd" => "netbsd",
        o => o,
    };
    format!("{cpu}-{os}")
}
