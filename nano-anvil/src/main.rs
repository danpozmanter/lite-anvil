#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]
// See lite-anvil/src/main.rs for why we detach from the console on
// release Windows builds.

use anvil_core::editor::subsystems::EditorSubsystems;

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

    anvil_core::window::set_app_icon_bytes(include_bytes!("../../resources/icons/nano-anvil.png"));
    anvil_core::window::set_app_metadata("Nano Anvil", "nano-anvil");
    anvil_core::window::init()?;

    let runtime = anvil_core::runtime::RuntimeContext::discover()?;
    let mut config = anvil_core::editor::config::NativeConfig::load_or_default(
        &runtime.user_dir_str(),
        runtime.scale(),
        runtime.platform_name(),
        &runtime.data_dir_str(),
    );
    config.verbose = verbose;

    let subsystems = EditorSubsystems::none();
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
