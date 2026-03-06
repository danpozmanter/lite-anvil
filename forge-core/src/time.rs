use std::sync::OnceLock;
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();

/// Seconds elapsed since first call (monotonic clock).
/// Equivalent to SDL_GetTicks / 1000.0 used by the C backend.
pub fn elapsed_secs() -> f64 {
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}
