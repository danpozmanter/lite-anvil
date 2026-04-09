use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Replace path separators in a storage key to avoid filesystem issues.
pub fn sanitize_key(key: &str) -> String {
    key.replace(['/', '\\'], "-")
}

/// Directory for a storage module under `base/storage/`.
pub fn module_dir(base: &Path, module: &str) -> PathBuf {
    base.join("storage").join(module)
}

/// Full path for a storage key file.
pub fn key_path(base: &Path, module: &str, key: &str) -> PathBuf {
    module_dir(base, module).join(sanitize_key(key))
}

/// Load text content from a storage key. Returns `None` if the file does not exist.
pub fn load_text(base: &Path, module: &str, key: &str) -> Result<Option<String>, std::io::Error> {
    let path = key_path(base, module, key);
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Save text content to a storage key, creating directories as needed.
pub fn save_text(base: &Path, module: &str, key: &str, text: &str) -> Result<(), std::io::Error> {
    let dir = module_dir(base, module);
    fs::create_dir_all(&dir)?;
    let path = key_path(base, module, key);
    fs::write(path, text)
}

/// List all keys in a storage module, sorted alphabetically.
pub fn list_keys(base: &Path, module: &str) -> Vec<String> {
    let dir = module_dir(base, module);
    let Ok(read_dir) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    for entry in read_dir.flatten() {
        if let Ok(name) = entry.file_name().into_string() {
            entries.push(name);
        }
    }
    entries.sort();
    entries
}

/// Clear a specific key or an entire module directory.
pub fn clear(base: &Path, module: &str, key: Option<&str>) -> Result<(), std::io::Error> {
    let path = match key {
        Some(key) => key_path(base, module, key),
        None => module_dir(base, module),
    };
    if !path.exists() {
        return Ok(());
    }
    if path.is_dir() {
        fs::remove_dir_all(&path)
    } else {
        fs::remove_file(&path)
    }
}

/// Write content to a temporary file, flush, then rename over the target (crash-safe).
pub fn write_atomic(path: &Path, content: &str) -> Result<(), std::io::Error> {
    let tmp = path.with_extension("tmp");
    let mut f = fs::File::create(&tmp)?;
    f.write_all(content.as_bytes())?;
    f.sync_all()?;
    fs::rename(&tmp, path)
}

/// Update a recent-items list: deduplicate, optionally add to front, truncate.
pub fn update_recent(items: &mut Vec<String>, entry: &str, add: bool, limit: usize) {
    items.retain(|item| item != entry);
    if add {
        items.insert(0, entry.to_string());
        items.truncate(limit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn sanitize_key_replaces_separators() {
        assert_eq!(sanitize_key("foo/bar\\baz"), "foo-bar-baz");
    }

    #[test]
    fn load_text_missing_returns_none() {
        let tmp = std::env::temp_dir().join("liteanvil_test_storage_missing");
        let result = load_text(&tmp, "mod", "nokey").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn save_and_load_round_trip() {
        let tmp = std::env::temp_dir().join("liteanvil_test_storage_rt");
        let _ = fs::remove_dir_all(&tmp);
        save_text(&tmp, "testmod", "mykey", "hello world").unwrap();
        let loaded = load_text(&tmp, "testmod", "mykey").unwrap();
        assert_eq!(loaded, Some("hello world".into()));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn list_keys_returns_sorted() {
        let tmp = std::env::temp_dir().join("liteanvil_test_storage_keys");
        let _ = fs::remove_dir_all(&tmp);
        save_text(&tmp, "km", "z_key", "1").unwrap();
        save_text(&tmp, "km", "a_key", "2").unwrap();
        let keys = list_keys(&tmp, "km");
        assert_eq!(keys, vec!["a_key", "z_key"]);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn clear_key_removes_file() {
        let tmp = std::env::temp_dir().join("liteanvil_test_storage_clear");
        let _ = fs::remove_dir_all(&tmp);
        save_text(&tmp, "cm", "k", "data").unwrap();
        assert!(load_text(&tmp, "cm", "k").unwrap().is_some());
        clear(&tmp, "cm", Some("k")).unwrap();
        assert!(load_text(&tmp, "cm", "k").unwrap().is_none());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn clear_module_removes_directory() {
        let tmp = std::env::temp_dir().join("liteanvil_test_storage_clearmod");
        let _ = fs::remove_dir_all(&tmp);
        save_text(&tmp, "dm", "k1", "1").unwrap();
        save_text(&tmp, "dm", "k2", "2").unwrap();
        clear(&tmp, "dm", None).unwrap();
        assert!(list_keys(&tmp, "dm").is_empty());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn write_atomic_creates_file() {
        let tmp = std::env::temp_dir().join("liteanvil_test_atomic");
        let path = tmp.join("test_atomic.txt");
        let _ = fs::create_dir_all(&tmp);
        write_atomic(&path, "atomic content").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "atomic content");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn update_recent_dedup_and_cap() {
        let mut items = vec!["b".into(), "a".into(), "c".into()];
        update_recent(&mut items, "a", true, 3);
        assert_eq!(items, vec!["a", "b", "c"]);

        update_recent(&mut items, "b", false, 3);
        assert_eq!(items, vec!["a", "c"]);
    }

    #[test]
    fn update_recent_truncates() {
        let mut items = vec!["a".into(), "b".into(), "c".into()];
        update_recent(&mut items, "d", true, 3);
        assert_eq!(items, vec!["d", "a", "b"]);
    }
}
