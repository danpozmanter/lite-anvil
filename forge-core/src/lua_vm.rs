use anyhow::{Context, Result};
use mlua::prelude::*;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Lua bootstrap executed after start.lua has run.
///
/// start.lua sets VERSION to a build-time meson placeholder; we override it
/// with FORGE_VERSION (set from Rust) immediately after start.lua loads.
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

/// Initialise one Lua VM lifecycle. Returns true if the editor requested a restart.
pub fn run(args: &[String]) -> Result<bool> {
    // SAFETY: the debug library is required for debug.traceback in error handlers.
    let lua = unsafe { Lua::unsafe_new() };

    let exe_file = std::env::current_exe().context("could not resolve executable path")?;
    let exe_dir = exe_file
        .parent()
        .context("executable has no parent directory")?;
    let data_dir = find_data_dir(exe_dir);
    let start_lua = data_dir.join("core").join("start.lua");

    set_globals(&lua, args, &exe_file, &data_dir)?;
    crate::api::register_stubs(&lua)?;

    let source = std::fs::read_to_string(&start_lua)
        .with_context(|| format!("could not read {}", start_lua.display()))?;

    lua.load(&source).set_name("start.lua").exec()?;

    // start.lua writes the meson placeholder "@PROJECT_VERSION@"; replace it.
    lua.globals().set("VERSION", env!("CARGO_PKG_VERSION"))?;

    let restart: bool = lua
        .load(BOOTSTRAP)
        .set_name("bootstrap")
        .eval()
        .unwrap_or(false);

    Ok(restart)
}

/// Locate the data/ directory using the same priority as start.lua, plus a
/// dev-mode fallback that walks up from the executable to find the repo root.
fn find_data_dir(exe_dir: &Path) -> PathBuf {
    if let Some(prefix) = std::env::var_os("LITE_PREFIX") {
        return PathBuf::from(prefix).join("share").join("lite-anvil");
    }

    if exe_dir.file_name() == Some(OsStr::new("bin")) {
        if let Some(prefix) = exe_dir.parent() {
            let d = prefix.join("share").join("lite-anvil");
            if d.join("core").join("start.lua").exists() {
                return d;
            }
        }
    }

    // Dev layout: walk up from the binary (e.g. target/debug/) to find data/.
    let mut dir = exe_dir.to_path_buf();
    for _ in 0..6 {
        let candidate = dir.join("data");
        if candidate.join("core").join("start.lua").exists() {
            return candidate;
        }
        if !dir.pop() {
            break;
        }
    }

    exe_dir.join("data")
}

fn set_globals(lua: &Lua, args: &[String], exe_file: &Path, data_dir: &Path) -> Result<()> {
    let globals = lua.globals();

    let args_table = lua.create_table()?;
    for (i, arg) in args.iter().enumerate() {
        args_table.set(i as i64 + 1, arg.as_str())?;
    }
    globals.set("ARGS", args_table)?;

    globals.set("PLATFORM", platform_name())?;
    globals.set("ARCH", arch_tuple())?;
    globals.set("RESTARTED", false)?;
    globals.set("EXEFILE", exe_file.to_str().unwrap_or(""))?;

    // FORGE_VERSION is read by the bootstrap to restore VERSION after start.lua.
    globals.set("FORGE_VERSION", env!("CARGO_PKG_VERSION"))?;

    // MACOS_RESOURCES is checked first by start.lua when computing DATADIR.
    // Setting it here forces start.lua to use our pre-computed path, which
    // is necessary in dev builds where the binary lives in target/debug/ and
    // data/ is several directories above it.
    globals.set("MACOS_RESOURCES", data_dir.to_str().unwrap_or(""))?;

    // HOME is referenced as a global in start.lua before it calls os.getenv.
    let home_key = if cfg!(target_os = "windows") {
        "USERPROFILE"
    } else {
        "HOME"
    };
    if let Ok(home) = std::env::var(home_key) {
        globals.set("HOME", home)?;
    }

    Ok(())
}

fn platform_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "macOS"
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
