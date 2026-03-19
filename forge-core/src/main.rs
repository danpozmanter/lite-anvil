mod api;
mod lua_vm;
#[cfg(feature = "sdl")]
mod renderer;
mod runtime;
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
    let mut restarted = false;
    loop {
        if !lua_vm::run(args, restarted)? {
            return Ok(());
        }
        log::info!("restarting Lua VM");
        restarted = true;
        // Release any FontRef arcs held by the previous frame's draw commands
        // before the new Lua VM loads fonts on the same FT_Library.
        #[cfg(feature = "sdl")]
        crate::renderer::reset_cache();
        #[cfg(feature = "sdl")]
        window::prepare_restart();
    }
}
