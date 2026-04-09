pub(crate) mod editor;
#[cfg(feature = "sdl")]
#[allow(dead_code)]
mod renderer;
#[allow(dead_code)]
mod runtime;
mod signal;
#[allow(dead_code)]
mod time;
#[cfg(feature = "sdl")]
#[allow(dead_code)]
mod window;

fn main() {
    env_logger::init();
    signal::install_handlers();
    let args: Vec<String> = std::env::args().collect();
    if let Err(e) = run(&args) {
        eprintln!("Fatal: {e:#}");
        std::process::exit(1);
    }
}

fn run(args: &[String]) -> anyhow::Result<()> {
    let verbose = args.iter().any(|a| a == "-v" || a == "--verbose");

    #[cfg(feature = "sdl")]
    window::init()?;

    let runtime = runtime::RuntimeContext::discover()?;
    let mut config = editor::config::NativeConfig::load_or_default(
        &runtime.user_dir_str(),
        runtime.scale(),
        runtime.platform_name(),
        &runtime.data_dir_str(),
    );
    config.verbose = verbose;
    editor::native_loop::run(config, args, &runtime.data_dir_str(), &runtime.user_dir_str());

    #[cfg(feature = "sdl")]
    window::shutdown();

    Ok(())
}
