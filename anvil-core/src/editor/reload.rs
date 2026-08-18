//! Off-UI-thread reading of a watched file, for deciding whether an external
//! change should be adopted.
//!
//! A watcher event is only a hint that a file may have changed. Answering it
//! means reading, decoding, and hashing the file, which is unbounded work the
//! user did not ask for. That work happens here, on a worker, and the result
//! is applied by the caller against whatever the document looks like when it
//! arrives.

use std::collections::HashMap;
use std::sync::LazyLock;

use parking_lot::Mutex;

use crate::editor::buffer;

/// A watched file as read from disk.
pub(crate) struct DiskContents {
    /// The path as requested, which is how the caller matches it to a tab.
    pub path: String,
    pub lines: Vec<String>,
    pub signature: u32,
}

static PENDING: LazyLock<Mutex<Vec<DiskContents>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// Paths being read, each carrying whether another hint arrived while the read
/// was running. A hint that lands mid-read is answered by one more read once
/// the current one finishes, so a file written twice in quick succession still
/// converges on its final contents.
static READING: LazyLock<Mutex<HashMap<String, bool>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Read a file and fingerprint it. Returns `None` if it cannot be read, which
/// is the normal outcome for a path that a rename or delete has just moved.
pub(crate) fn read_disk_contents(path: &str) -> Option<DiskContents> {
    let _perf = crate::editor::perf::span("external_reload_read");
    let mut state = buffer::default_buffer_state();
    buffer::load_file(&mut state, path).ok()?;
    let signature = buffer::content_signature(&state.lines);
    Some(DiskContents {
        path: path.to_string(),
        lines: std::mem::take(&mut state.lines),
        signature,
    })
}

/// Read `path` on a worker thread for later comparison against the open
/// document. Hints for a path already being read collapse into one more read.
pub(crate) fn probe(path: &str) {
    {
        let mut reading = READING.lock();
        if let Some(again) = reading.get_mut(path) {
            *again = true;
            return;
        }
        reading.insert(path.to_string(), false);
    }
    let path = path.to_string();
    std::thread::spawn(move || {
        loop {
            if let Some(contents) = read_disk_contents(&path) {
                PENDING.lock().push(contents);
            }
            let mut reading = READING.lock();
            match reading.get_mut(&path) {
                Some(again) if *again => *again = false,
                _ => {
                    reading.remove(&path);
                    return;
                }
            }
        }
    });
}

/// Take the reads that have finished.
pub(crate) fn drain() -> Vec<DiskContents> {
    let mut pending = PENDING.lock();
    if pending.is_empty() {
        return Vec::new();
    }
    std::mem::take(&mut *pending)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reading_a_file_reports_its_lines_and_signature() {
        let path = std::env::temp_dir().join("liteanvil_test_reload_read.txt");
        std::fs::write(&path, b"one\ntwo\n").unwrap();
        let path = path.to_string_lossy().to_string();

        let contents = read_disk_contents(&path).expect("file is readable");

        assert_eq!(contents.path, path);
        assert_eq!(
            contents.lines,
            vec!["one\n".to_string(), "two\n".to_string()]
        );
        assert_eq!(
            contents.signature,
            buffer::content_signature(&contents.lines)
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn reading_a_missing_file_reports_nothing() {
        assert!(read_disk_contents("/no/such/path/liteanvil_missing.txt").is_none());
    }

    #[test]
    fn a_hint_during_a_read_is_answered_by_one_more_read() {
        let path = "/watched/busy.rs";
        READING.lock().insert(path.to_string(), false);

        probe(path);
        probe(path);

        assert_eq!(
            READING.lock().get(path),
            Some(&true),
            "hints arriving during a read must schedule exactly one more"
        );
        READING.lock().remove(path);
    }
}
