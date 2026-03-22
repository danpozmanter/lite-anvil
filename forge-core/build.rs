use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo::rerun-if-env-changed=VCPKG_ROOT");
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os != "windows" || target_env != "msvc" {
        return;
    }

    let Some(vcpkg_root) = env::var_os("VCPKG_ROOT") else {
        return;
    };

    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_else(|_| "x86_64".to_string());
    let triplet = match target_arch.as_str() {
        "x86_64" => "x64-windows",
        "x86" => "x86-windows",
        "aarch64" => "arm64-windows",
        _ => return,
    };

    let lib_dir = PathBuf::from(vcpkg_root)
        .join("installed")
        .join(triplet)
        .join("lib");
    if lib_dir.exists() {
        println!("cargo::rustc-link-search=native={}", lib_dir.display());
    }
}

