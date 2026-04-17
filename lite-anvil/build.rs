// SDL3 is now compiled from source and statically linked via `sdl3-sys`'s
// `build-from-source-static` feature (see `anvil-core/Cargo.toml`), so the
// binary has no runtime libSDL3 dependency. No RPATH or link-search setup
// is required on any platform.
fn main() {}
