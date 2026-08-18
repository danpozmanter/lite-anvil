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

/// Whether opt-in timings and reports are enabled.
#[inline]
pub(crate) fn enabled() -> bool {
    *ENABLED
}

/// Report what the open documents are retaining, one line per document plus a
/// total. Answers "which tab is holding the memory" without a profiler, and is
/// the evidence any future cache budget should be set from.
pub(crate) fn report_memory(docs: &[crate::editor::open_doc::OpenDoc]) {
    if !enabled() {
        return;
    }
    let mut total = 0u64;
    for doc in docs {
        let memory = crate::editor::open_doc::doc_memory(doc);
        total += memory.total();
        eprintln!(
            "anvil_perf memory doc={} total_kb={} text_kb={} history_kb={} search_kb={} \
tokens_kb={} render_kb={} preview_kb={}",
            if doc.name.is_empty() {
                "untitled"
            } else {
                &doc.name
            },
            memory.total() / 1024,
            memory.text / 1024,
            memory.history / 1024,
            memory.search_subject / 1024,
            memory.token_cache / 1024,
            memory.render_cache / 1024,
            memory.preview / 1024,
        );
    }
    eprintln!(
        "anvil_perf memory docs={} total_kb={}",
        docs.len(),
        total / 1024
    );
}

#[inline]
pub(crate) fn span(stage: &'static str) -> Option<PerfSpan> {
    (*ENABLED).then(|| PerfSpan {
        stage,
        started: Instant::now(),
    })
}
