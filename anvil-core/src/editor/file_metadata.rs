use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// One label/value row of a file's metadata, ordered for display.
#[derive(Debug, Clone, PartialEq)]
pub struct MetadataField {
    pub label: &'static str,
    pub value: String,
}

/// Metadata rows describing the file at `path`, in display order.
pub fn describe(path: &Path) -> std::io::Result<Vec<MetadataField>> {
    let meta = fs::metadata(path)?;

    let mut fields = Vec::new();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    fields.push(field("Name", name));
    if let Some(parent) = path.parent() {
        let dir = parent.to_string_lossy();
        if !dir.is_empty() {
            fields.push(field("Location", dir.into_owned()));
        }
    }

    fields.push(field("Size", format_size(meta.len())));
    fields.push(field("Modified", format_time(meta.modified().ok())));
    fields.push(field("Permissions", permissions(&meta)));
    for (label, value) in ownership(&meta) {
        fields.push(field(label, value));
    }
    Ok(fields)
}

fn field(label: &'static str, value: String) -> MetadataField {
    MetadataField { label, value }
}

/// Byte count rendered in the largest unit that keeps the number above 1,
/// with the exact byte count alongside it.
pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    const TB: u64 = GB * 1024;
    let exact = format!("{} bytes", group_digits(bytes));
    let scaled =
        |unit: u64, suffix: &str| format!("{:.2} {suffix} ({exact})", bytes as f64 / unit as f64);
    if bytes >= TB {
        scaled(TB, "TB")
    } else if bytes >= GB {
        scaled(GB, "GB")
    } else if bytes >= MB {
        scaled(MB, "MB")
    } else if bytes >= KB {
        scaled(KB, "KB")
    } else {
        exact
    }
}

/// Decimal digits with thousands separators, so large byte counts stay readable.
fn group_digits(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn format_time(time: Option<SystemTime>) -> String {
    let Some(time) = time else {
        return "unavailable".to_string();
    };
    let secs = match time.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    };
    match local_datetime(secs) {
        Some((y, mo, d, h, mi, s)) => {
            format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
        }
        None => "unavailable".to_string(),
    }
}

/// Seconds since the Unix epoch broken into local (year, month, day, hour,
/// minute, second).
#[cfg(unix)]
fn local_datetime(secs: i64) -> Option<(i64, i64, i64, i64, i64, i64)> {
    let t = secs as libc::time_t;
    let mut out: libc::tm = unsafe { std::mem::zeroed() };
    // localtime_r writes through `out` and returns it; null means the
    // timestamp falls outside the range the platform can represent.
    let filled = unsafe { !libc::localtime_r(&t, &mut out).is_null() };
    filled.then(|| from_tm(&out))
}

#[cfg(windows)]
fn local_datetime(secs: i64) -> Option<(i64, i64, i64, i64, i64, i64)> {
    let t = secs as libc::time_t;
    let mut out: libc::tm = unsafe { std::mem::zeroed() };
    // localtime_s writes through `out` and returns 0 on success; a non-zero
    // code means the timestamp falls outside the range the CRT can represent.
    let rc = unsafe { libc::localtime_s(&mut out, &t) };
    (rc == 0).then(|| from_tm(&out))
}

/// UTC fallback for targets with neither a POSIX nor a CRT local-time call.
#[cfg(not(any(unix, windows)))]
fn local_datetime(secs: i64) -> Option<(i64, i64, i64, i64, i64, i64)> {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, mo, d) = civil_from_days(days);
    Some((y, mo, d, rem / 3600, (rem % 3600) / 60, rem % 60))
}

#[cfg(any(unix, windows))]
fn from_tm(tm: &libc::tm) -> (i64, i64, i64, i64, i64, i64) {
    (
        tm.tm_year as i64 + 1900,
        tm.tm_mon as i64 + 1,
        tm.tm_mday as i64,
        tm.tm_hour as i64,
        tm.tm_min as i64,
        tm.tm_sec as i64,
    )
}

/// Days since the Unix epoch to (year, month, day). Howard Hinnant's
/// `civil_from_days` (public domain).
#[cfg(not(any(unix, windows)))]
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(unix)]
fn permissions(meta: &fs::Metadata) -> String {
    use std::os::unix::fs::PermissionsExt;
    let mode = meta.permissions().mode();
    format!("{} ({:04o})", mode_symbolic(mode), mode & 0o7777)
}

/// The classic nine-character `rwxrwxrwx` rendering, with the setuid,
/// setgid, and sticky bits folded into the execute positions.
#[cfg(unix)]
fn mode_symbolic(mode: u32) -> String {
    let mut out = String::with_capacity(9);
    // Triples run owner, group, other; each special bit replaces the execute
    // character of its own triple.
    let special = [
        (mode & 0o4000, 's', 'S'),
        (mode & 0o2000, 's', 'S'),
        (mode & 0o1000, 't', 'T'),
    ];
    for (i, (special_bit, set_char, unset_char)) in special.iter().enumerate() {
        let shift = 6 - i * 3;
        let bits = (mode >> shift) & 0o7;
        out.push(if bits & 0o4 != 0 { 'r' } else { '-' });
        out.push(if bits & 0o2 != 0 { 'w' } else { '-' });
        let executable = bits & 0o1 != 0;
        out.push(match (*special_bit != 0, executable) {
            (true, true) => *set_char,
            (true, false) => *unset_char,
            (false, true) => 'x',
            (false, false) => '-',
        });
    }
    out
}

#[cfg(windows)]
fn permissions(meta: &fs::Metadata) -> String {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x0000_0002;
    const FILE_ATTRIBUTE_SYSTEM: u32 = 0x0000_0004;
    const FILE_ATTRIBUTE_ARCHIVE: u32 = 0x0000_0020;
    let attrs = meta.file_attributes();
    let mut parts = vec![if meta.permissions().readonly() {
        "Read-only"
    } else {
        "Read/write"
    }];
    if attrs & FILE_ATTRIBUTE_HIDDEN != 0 {
        parts.push("Hidden");
    }
    if attrs & FILE_ATTRIBUTE_SYSTEM != 0 {
        parts.push("System");
    }
    if attrs & FILE_ATTRIBUTE_ARCHIVE != 0 {
        parts.push("Archive");
    }
    parts.join(", ")
}

#[cfg(not(any(unix, windows)))]
fn permissions(meta: &fs::Metadata) -> String {
    if meta.permissions().readonly() {
        "Read-only".to_string()
    } else {
        "Read/write".to_string()
    }
}

/// Owner and group rows. Windows exposes neither through `std`, so the popup
/// simply omits them there.
#[cfg(unix)]
fn ownership(meta: &fs::Metadata) -> Vec<(&'static str, String)> {
    use std::os::unix::fs::MetadataExt;
    let uid = meta.uid();
    let gid = meta.gid();
    vec![
        ("Owner", labelled_id(user_name(uid), uid)),
        ("Group", labelled_id(group_name(gid), gid)),
    ]
}

#[cfg(not(unix))]
fn ownership(_meta: &fs::Metadata) -> Vec<(&'static str, String)> {
    Vec::new()
}

#[cfg(unix)]
fn labelled_id(name: Option<String>, id: u32) -> String {
    match name {
        Some(name) => format!("{name} ({id})"),
        None => id.to_string(),
    }
}

/// Largest `getpwuid_r` / `getgrgid_r` scratch buffer to grow to before
/// falling back to the numeric id. Directory backends can report huge or
/// unknown suggested sizes, so the growth is bounded.
#[cfg(unix)]
const MAX_NAME_BUF: usize = 64 * 1024;

#[cfg(unix)]
fn user_name(uid: u32) -> Option<String> {
    with_growing_buf(|buf| {
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let rc = unsafe {
            libc::getpwuid_r(
                uid as libc::uid_t,
                &mut pwd,
                buf.as_mut_ptr(),
                buf.len(),
                &mut result,
            )
        };
        // `pwd` is only initialised when the call succeeds and finds an entry.
        (
            rc,
            (!result.is_null()).then(|| unsafe { c_string(pwd.pw_name) }),
        )
    })
}

#[cfg(unix)]
fn group_name(gid: u32) -> Option<String> {
    with_growing_buf(|buf| {
        let mut grp: libc::group = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::group = std::ptr::null_mut();
        let rc = unsafe {
            libc::getgrgid_r(
                gid as libc::gid_t,
                &mut grp,
                buf.as_mut_ptr(),
                buf.len(),
                &mut result,
            )
        };
        (
            rc,
            (!result.is_null()).then(|| unsafe { c_string(grp.gr_name) }),
        )
    })
}

/// Run a `_r`-suffixed name lookup, doubling its scratch buffer while the
/// call reports `ERANGE`. The closure returns the call's status code and the
/// name it found.
#[cfg(unix)]
fn with_growing_buf(
    mut lookup: impl FnMut(&mut [libc::c_char]) -> (libc::c_int, Option<String>),
) -> Option<String> {
    let mut size = 1024usize;
    loop {
        let mut buf = vec![0 as libc::c_char; size];
        let (rc, name) = lookup(&mut buf);
        if rc == libc::ERANGE && size < MAX_NAME_BUF {
            size *= 2;
            continue;
        }
        return if rc == 0 { name } else { None };
    }
}

/// Copy a NUL-terminated C string the lookup wrote into the scratch buffer.
///
/// # Safety
/// `ptr` must be a valid NUL-terminated string that outlives the call.
#[cfg(unix)]
unsafe fn c_string(ptr: *const libc::c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    unsafe { std::ffi::CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_size_reports_bytes_below_one_kilobyte() {
        assert_eq!(format_size(0), "0 bytes");
        assert_eq!(format_size(1023), "1,023 bytes");
    }

    #[test]
    fn format_size_scales_to_the_largest_whole_unit() {
        assert_eq!(format_size(1024), "1.00 KB (1,024 bytes)");
        assert_eq!(format_size(1024 * 1024), "1.00 MB (1,048,576 bytes)");
        assert_eq!(
            format_size(3 * 1024 * 1024 * 1024),
            "3.00 GB (3,221,225,472 bytes)"
        );
    }

    #[test]
    fn group_digits_separates_thousands() {
        assert_eq!(group_digits(1), "1");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1000), "1,000");
        assert_eq!(group_digits(1234567), "1,234,567");
    }

    #[cfg(unix)]
    #[test]
    fn mode_symbolic_renders_permission_triples() {
        assert_eq!(mode_symbolic(0o644), "rw-r--r--");
        assert_eq!(mode_symbolic(0o755), "rwxr-xr-x");
        assert_eq!(mode_symbolic(0o000), "---------");
        assert_eq!(mode_symbolic(0o4755), "rwsr-xr-x");
        assert_eq!(mode_symbolic(0o1777), "rwxrwxrwt");
        assert_eq!(mode_symbolic(0o2644), "rw-r-Sr--");
    }

    #[test]
    fn describe_reports_the_size_of_a_real_file() {
        let path = std::env::temp_dir().join("anvil-metadata-test.txt");
        std::fs::write(&path, b"hello").expect("write test file");
        let fields = describe(&path).expect("describe test file");
        let by_label = |label: &str| {
            fields
                .iter()
                .find(|f| f.label == label)
                .map(|f| f.value.clone())
        };
        assert_eq!(by_label("Name").as_deref(), Some("anvil-metadata-test.txt"));
        assert_eq!(by_label("Type"), None);
        assert_eq!(by_label("Size").as_deref(), Some("5 bytes"));
        assert!(by_label("Modified").is_some_and(|v| v.contains('-')));
        assert!(by_label("Permissions").is_some());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn describe_reports_a_missing_file_as_an_error() {
        let path = std::env::temp_dir().join("anvil-metadata-absent-file");
        let _ = std::fs::remove_file(&path);
        assert!(describe(&path).is_err());
    }
}
