use anyhow::{Context, Result};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

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
        // Canonicalize so that relative paths from current_exe() (macOS
        // returns whatever argv[0] was) become absolute before we derive
        // data_dir / user_dir from them. Strip Windows' `\\?\` extended
        // path prefix afterwards; that form rejects forward-slash
        // separators, which trips up any code that later does
        // `format!("{path}/sub")` (theme/syntax loaders did, and the
        // resulting `\\?\C:\...\data/assets/themes/...` fails to open).
        let exe_file = std::fs::canonicalize(&exe_file)
            .map(strip_unc_prefix)
            .unwrap_or(exe_file);
        let exe_dir = exe_file
            .parent()
            .context("executable has no parent directory")?
            .to_path_buf();
        let data_dir = find_data_dir(&exe_file, &exe_dir);
        let path_sep = std::path::MAIN_SEPARATOR;
        let user_dir = find_user_dir(&exe_file, &exe_dir, path_sep);
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

    /// User config directory as a string.
    pub fn user_dir_str(&self) -> String {
        self.user_dir.to_string_lossy().into_owned()
    }

    /// Data directory as a string.
    pub fn data_dir_str(&self) -> String {
        self.data_dir.to_string_lossy().into_owned()
    }

    /// HiDPI scale factor.
    pub fn scale(&self) -> f64 {
        self.scale
    }

    /// Platform name string.
    pub fn platform_name(&self) -> &'static str {
        platform_name()
    }
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

fn find_data_dir(exe_file: &Path, exe_dir: &Path) -> PathBuf {
    fn is_data_dir(candidate: &Path) -> bool {
        candidate.join("fonts").join("Lilex-Regular.ttf").exists()
    }

    let app_name = match exe_file.file_stem().and_then(OsStr::to_str) {
        Some("nano-anvil") => "nano-anvil",
        Some("note-anvil") => "note-anvil",
        _ => "lite-anvil",
    };

    if let Some(prefix) = std::env::var_os("LITE_PREFIX") {
        return PathBuf::from(prefix).join("share").join(app_name);
    }

    if exe_dir.file_name() == Some(OsStr::new("bin"))
        && let Some(prefix) = exe_dir.parent()
    {
        for name in [app_name, "lite-anvil"] {
            let candidate = prefix.join("share").join(name).join("data");
            if is_data_dir(&candidate) {
                return candidate;
            }
            let candidate = prefix.join("share").join(name);
            if is_data_dir(&candidate) {
                return candidate;
            }
        }
    }

    // macOS app bundle: exe is at .app/Contents/MacOS/lite-anvil.
    // Data may live in .app/Contents/Resources/data/.
    if exe_dir.file_name() == Some(OsStr::new("MacOS"))
        && let Some(contents) = exe_dir.parent()
    {
        let candidate = contents.join("Resources").join("data");
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

fn find_user_dir(exe_file: &Path, exe_dir: &Path, _path_sep: char) -> PathBuf {
    let bundled = exe_dir.join("user");
    if bundled.exists() {
        return bundled;
    }

    if let Some(user_dir) = std::env::var_os("LITE_USERDIR") {
        return PathBuf::from(user_dir);
    }

    // Per-app subdirectory so nano-anvil and note-anvil keep their own
    // session / recent-files / project state and don't pollute
    // lite-anvil's. Lite-anvil keeps the historical "lite-anvil" name
    // for backward compatibility with existing installs.
    let app_dir = match exe_file.file_stem().and_then(OsStr::to_str) {
        Some("nano-anvil") => "nano-anvil",
        Some("note-anvil") => "note-anvil",
        _ => "lite-anvil",
    };

    // Windows: use %APPDATA% (`C:\Users\<user>\AppData\Roaming`) per
    // Microsoft's user-data guidance, rather than dropping a folder
    // directly under the user's profile root.
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata).join(app_dir);
        }
        if let Some(home) = std::env::var_os("USERPROFILE") {
            return PathBuf::from(home)
                .join("AppData")
                .join("Roaming")
                .join(app_dir);
        }
    }

    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join(app_dir);
    }

    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".config").join(app_dir);
    }

    bundled
}

/// Strip the Windows `\\?\` extended-length path prefix, if present.
/// No-op on non-Windows platforms and for paths that do not use the
/// prefix. Callers use this after `fs::canonicalize` so downstream
/// string-based path building can keep mixing `/` and `\` without the
/// kernel rejecting the resulting path.
fn strip_unc_prefix(p: PathBuf) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        let s = p.to_string_lossy();
        if let Some(stripped) = s.strip_prefix(r"\\?\") {
            return PathBuf::from(stripped.to_string());
        }
    }
    p
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
