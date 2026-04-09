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

fn find_data_dir(exe_dir: &Path) -> PathBuf {
    fn is_data_dir(candidate: &Path) -> bool {
        candidate.join("fonts").join("Lilex-Regular.ttf").exists()
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
