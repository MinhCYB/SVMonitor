//! XDG autostart entry (`~/.config/autostart/svmonitor.desktop`), plus the application launcher entry.
use crate::util::atomic_write;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub fn autostart_path() -> PathBuf {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"),
    };
    base.join("autostart/svmonitor.desktop")
}

fn quote_exec(p: &str) -> String {
    // Desktop Entry spec: quote args containing spaces/specials, escape " ` $ \ inside quotes.
    if p.chars().any(|c| c.is_whitespace() || "\"'\\><~|&;$*?#()`".contains(c)) {
        let esc: String = p.chars().flat_map(|c| if "\"`$\\".contains(c) { vec!['\\', c] } else { vec![c] }).collect();
        format!("\"{esc}\"")
    } else {
        p.to_string()
    }
}

pub fn desktop_entry(exe: &str, delay_secs: u32) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=SVMonitor\nComment=Lightweight SSH server monitor\nExec={} run\nIcon=network-server\nTerminal=false\nCategories=Network;Monitor;System;\nX-GNOME-Autostart-enabled=true\nX-GNOME-Autostart-Delay={delay_secs}\nStartupNotify=false\n",
        quote_exec(exe)
    )
}

pub fn enable(path: &Path, exe: &str) -> Result<()> {
    atomic_write(path, desktop_entry(exe, 5).as_bytes()).with_context(|| format!("writing {}", path.display()))
}

pub fn disable(path: &Path) -> Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
    }
}

pub fn is_enabled(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_quoting() {
        assert_eq!(quote_exec("/usr/bin/svmonitor"), "/usr/bin/svmonitor");
        assert_eq!(quote_exec("/opt/my app/svm"), "\"/opt/my app/svm\"");
        assert_eq!(quote_exec("/a$b"), "\"/a\\$b\"");
    }

    #[test]
    fn enable_disable_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("autostart/svmonitor.desktop");
        assert!(!is_enabled(&p));
        enable(&p, "/usr/bin/svmonitor").unwrap();
        assert!(is_enabled(&p));
        let t = std::fs::read_to_string(&p).unwrap();
        assert!(t.contains("Exec=/usr/bin/svmonitor run\n"));
        assert!(t.starts_with("[Desktop Entry]"));
        assert!(disable(&p).unwrap());
        assert!(!disable(&p).unwrap());
    }
}
