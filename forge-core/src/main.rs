mod api;
mod lua_vm;
#[cfg(feature = "sdl")]
mod renderer;
mod time;
#[cfg(feature = "sdl")]
mod window;

fn main() {
    env_logger::init();
    let args: Vec<String> = std::env::args().collect();
    if let Err(e) = run(&args) {
        eprintln!("Fatal: {e:#}");
        std::process::exit(1);
    }
}

fn run(args: &[String]) -> anyhow::Result<()> {
    #[cfg(feature = "sdl")]
    window::init()?;

    let result = run_loop(args);

    #[cfg(feature = "sdl")]
    window::shutdown();

    result
}

fn run_loop(args: &[String]) -> anyhow::Result<()> {
    loop {
        if !lua_vm::run(args)? {
            return Ok(());
        }
        log::info!("restarting Lua VM");
        #[cfg(feature = "sdl")]
        window::prepare_restart();
    }
}
