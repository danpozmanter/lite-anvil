#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

use anvil_core::editor::subsystems::EditorSubsystems;
use std::path::PathBuf;

fn main() {
    env_logger::init();
    anvil_core::signal::install_handlers();
    let args: Vec<String> = std::env::args().collect();
    if let Err(e) = run(&args) {
        eprintln!("Fatal: {e:#}");
        std::process::exit(1);
    }
}

fn run(args: &[String]) -> anyhow::Result<()> {
    let verbose = args.iter().any(|a| a == "-v" || a == "--verbose");

    anvil_core::window::set_app_icon_bytes(include_bytes!(
        "../../resources/icons/note-anvil.png"
    ));
    anvil_core::window::set_app_metadata("Note Anvil", "note-anvil");
    anvil_core::window::init()?;

    let runtime = anvil_core::runtime::RuntimeContext::discover()?;
    let mut config = anvil_core::editor::config::NativeConfig::load_or_default(
        &runtime.user_dir_str(),
        runtime.scale(),
        runtime.platform_name(),
        &runtime.data_dir_str(),
    );
    config.verbose = verbose;

    let notes_folder = resolve_notes_folder(args);
    std::fs::create_dir_all(&notes_folder).ok();

    let subsystems = EditorSubsystems::notes(notes_folder.to_string_lossy().to_string());

    // No need to inject the notes folder as a CLI arg — main_loop reads
    // it from `subsystems.notes_folder()` and opens it as the project
    // root automatically when notes-mode is set. Forwarding the user's
    // own args lets `--verbose` and similar still apply.
    anvil_core::editor::main_loop::run(
        config,
        args,
        &runtime.data_dir_str(),
        &runtime.user_dir_str(),
        subsystems,
    );

    anvil_core::window::shutdown();

    Ok(())
}

/// Resolve the notes folder, with precedence:
///   1. `--notes-folder=<path>` arg
///   2. `NOTE_ANVIL_DIR` env var
///   3. `~/local-notes/` (matches NoteSquirrel's default)
fn resolve_notes_folder(args: &[String]) -> PathBuf {
    for a in args.iter().skip(1) {
        if let Some(path) = a.strip_prefix("--notes-folder=") {
            return PathBuf::from(path);
        }
    }
    if let Some(env_dir) = std::env::var_os("NOTE_ANVIL_DIR") {
        return PathBuf::from(env_dir);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join("local-notes")
}
