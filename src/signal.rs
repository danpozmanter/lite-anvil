use std::sync::atomic::{AtomicBool, Ordering};

static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Returns true if a SIGINT or SIGTERM has been received.
pub fn shutdown_requested() -> bool {
    SHUTDOWN_REQUESTED.load(Ordering::SeqCst)
}

/// Resets the shutdown flag so a VM restart starts with a clean state.
pub fn clear_shutdown() {
    SHUTDOWN_REQUESTED.store(false, Ordering::SeqCst);
}

/// Installs process-wide signal handlers for graceful shutdown.
#[cfg(unix)]
pub fn install_handlers() {
    // SAFETY: libc::signal is safe to call with a valid sighandler_t.
    // handle_signal only sets an atomic flag, which is async-signal-safe.
    unsafe {
        libc::signal(
            libc::SIGINT,
            handle_signal as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            handle_signal as *const () as libc::sighandler_t,
        );
    }
}

#[cfg(not(unix))]
pub fn install_handlers() {}

#[cfg(unix)]
extern "C" fn handle_signal(_sig: libc::c_int) {
    SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
}
