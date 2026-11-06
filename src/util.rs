//! Small shared helpers: time, atomic file writes, formatting.
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Write `data` to `path` atomically (temp file in the same directory + rename), mode 0600.
pub fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.tmp{}",
        path.file_name().and_then(|s| s.to_str()).unwrap_or("file"),
        std::process::id()
    ));
    let result = (|| {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(data)?;
        f.flush()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Create a directory (and parents) with mode 0700 for the leaf. Refuses a leaf that exists but is
/// not owned by the current user or is a symlink (matters for the /tmp fallback runtime dir).
pub fn ensure_private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    fs::create_dir_all(path)?;
    let md = fs::symlink_metadata(path)?;
    // SAFETY: getuid has no preconditions.
    if !md.is_dir() || md.uid() != unsafe { libc::getuid() } {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("{} is not a directory owned by you", path.display()),
        ));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

/// Make untrusted (server supplied) text safe to show in a terminal, menu or notification:
/// control characters (incl. ESC, newlines) become spaces, bidi overrides are dropped, length is capped.
pub fn sanitize_text(s: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(s.len().min(max_chars * 4));
    let mut n = 0;
    for c in s.chars() {
        if n >= max_chars {
            out.push('…');
            break;
        }
        let c = match c {
            c if c.is_control() => ' ',
            '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}' => continue,
            c => c,
        };
        out.push(c);
        n += 1;
    }
    out.trim().to_string()
}

/// Escape for the Notifications spec body markup subset.
pub fn escape_markup(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub fn fmt_duration(secs: u64) -> String {
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3600;
    let m = (secs % 3600) / 60;
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{secs}s")
    }
}

pub fn fmt_kib(kib: u64) -> String {
    let b = kib as f64 * 1024.0;
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i <= 1 {
        format!("{v:.0} {}", U[i])
    } else {
        format!("{v:.1} {}", U[i])
    }
}

/// "5m ago" style text for a unix timestamp.
pub fn fmt_ago(ts: i64, now: i64) -> String {
    let d = (now - ts).max(0) as u64;
    if d < 5 {
        "just now".into()
    } else {
        format!("{} ago", fmt_duration(d))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(fmt_duration(30), "30s");
        assert_eq!(fmt_duration(61), "1m");
        assert_eq!(fmt_duration(3 * 3600 + 120), "3h 2m");
        assert_eq!(fmt_duration(90_000), "1d 1h");
    }

    #[test]
    fn sanitizes_untrusted_text() {
        assert_eq!(
            sanitize_text("a\x1b[31mred\x07\nnext", 100),
            "a [31mred  next"
        );
        assert_eq!(sanitize_text("ev\u{202e}il", 100), "evil");
        assert_eq!(sanitize_text("abcdef", 3), "abc…");
        assert_eq!(escape_markup("<b>&</b>"), "&lt;b&gt;&amp;&lt;/b&gt;");
    }

    #[test]
    fn private_dir_rejects_symlink_and_sets_mode() {
        let d = tempfile::tempdir().unwrap();
        let real = d.path().join("real");
        fs::create_dir(&real).unwrap();
        let link = d.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(ensure_private_dir(&link).is_err());
        let ok = d.path().join("ok/sub");
        ensure_private_dir(&ok).unwrap();
        assert_eq!(
            fs::metadata(&ok).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn sizes() {
        assert_eq!(fmt_kib(512), "512 KiB");
        assert_eq!(fmt_kib(2048), "2.0 MiB");
        assert_eq!(fmt_kib(16 * 1024 * 1024), "16.0 GiB");
    }

    #[test]
    fn atomic_write_replaces_and_is_private() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("sub/a.txt");
        atomic_write(&p, b"one").unwrap();
        atomic_write(&p, b"two").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two");
        let mode = fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let leftovers: Vec<_> = fs::read_dir(p.parent().unwrap()).unwrap().collect();
        assert_eq!(leftovers.len(), 1);
    }
}
