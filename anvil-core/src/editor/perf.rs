//! Opt-in timing spans for diagnosing editor latency.
//!
//! Set `ANVIL_PERF=1` before launching the editor. Timings are written to
//! stderr only when enabled, keeping normal builds free of logging traffic.

use std::sync::LazyLock;
use std::time::Instant;

static ENABLED: LazyLock<bool> = LazyLock::new(|| {
    std::env::var_os("ANVIL_PERF")
        .map(|value| value != "0")
        .unwrap_or(false)
});

pub(crate) struct PerfSpan {
    stage: &'static str,
    started: Instant,
}

impl Drop for PerfSpan {
    fn drop(&mut self) {
        eprintln!(
            "anvil_perf stage={} duration_us={}",
            self.stage,
            self.started.elapsed().as_micros()
        );
    }
}

#[inline]
pub(crate) fn span(stage: &'static str) -> Option<PerfSpan> {
    (*ENABLED).then(|| PerfSpan {
        stage,
        started: Instant::now(),
    })
}
