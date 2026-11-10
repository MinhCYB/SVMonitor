//! Tiny command channel from UI helpers (dashboard popup) to the running monitor.
//!
//! No server socket lives in the daemon: a UI writes one small JSON file into
//! `$XDG_RUNTIME_DIR/svmonitor/cmd/` and sends `SIGUSR1`; the daemon drains and validates the
//! files. The directory is private (0700) and every file is checked for ownership and size, so
//! another local user cannot inject commands.
use crate::config::{Paths, validate_name};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const MAX_FILE_BYTES: u64 = 4096;
/// Commands consumed per signal; the rest stay queued for the next drain.
const MAX_PER_DRAIN: usize = 64;
/// Maximum spooled command files before `send` rejects new requests.
const MAX_QUEUED_COMMANDS: usize = 256;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum UiCommand {
    /// Probe one server (or every enabled one when `server` is None) as soon as possible.
    CheckNow {
        server: Option<String>,
    },
    SetEnabled {
        server: String,
        enabled: bool,
    },
    Remove {
        server: String,
    },
    /// Start monitoring an SSH alias.
    Add {
        server: String,
    },
    /// Mute notifications for N seconds; None clears the mute.
    Mute {
        secs: Option<u64>,
    },
    Reload,
}

impl UiCommand {
    /// Server names reach ssh argv and the config file: reject anything `validate_name` refuses.
    pub fn is_valid(&self) -> bool {
        match self {
            UiCommand::CheckNow { server: Some(s) }
            | UiCommand::SetEnabled { server: s, .. }
            | UiCommand::Remove { server: s }
            | UiCommand::Add { server: s } => validate_name(s).is_ok(),
            UiCommand::Mute { secs: Some(n) } => *n <= 7 * 24 * 3600,
            _ => true,
        }
    }

    /// The engine command that carries this request out. Replies are not awaited: the UI watches
    /// the next snapshot instead, so the daemon never blocks on a UI helper.
    pub fn into_cmd(self) -> crate::engine::Cmd {
        use crate::engine::Cmd;
        match self {
            UiCommand::CheckNow { server } => Cmd::CheckNow(server),
            UiCommand::SetEnabled { server, enabled } => Cmd::SetEnabled(server, enabled, None),
            UiCommand::Remove { server } => Cmd::Remove(server, None),
            UiCommand::Add { server } => Cmd::Add(crate::config::Server::alias(&server), None),
            UiCommand::Mute { secs } => Cmd::Mute(secs),
            UiCommand::Reload => Cmd::Reload,
        }
    }
}

pub fn cmd_dir(paths: &Paths) -> PathBuf {
    paths.runtime_dir.join("cmd")
}

/// Discard commands from an earlier daemon before publishing a new PID.
pub fn purge(paths: &Paths) {
    if let Ok(entries) = std::fs::read_dir(cmd_dir(paths)) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".json") && !n.starts_with('.'))
            {
                let _ = std::fs::remove_file(p);
            }
        }
    }
}

/// PID of the running monitor, verified against /proc so a stale state file (crash + PID reuse)
/// can never make us signal an unrelated process.
pub fn running_pid(paths: &Paths) -> Option<u32> {
    let snap: crate::model::Snapshot =
        serde_json::from_slice(&std::fs::read(paths.snapshot()).ok()?).ok()?;
    is_svmonitor(snap.pid).then_some(snap.pid)
}

pub fn is_svmonitor(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .map(|c| c.trim() == "svmonitor")
        .unwrap_or(false)
}

/// Send `sig` to the running monitor. False when none is running.
pub fn signal_monitor(paths: &Paths, sig: i32) -> bool {
    match running_pid(paths) {
        // SAFETY: plain syscall on a PID we just verified.
        Some(pid) => unsafe { libc::kill(pid as i32, sig) == 0 },
        None => false,
    }
}

/// Queue `cmd` for the daemon and wake it. Err when no monitor is running (nothing is queued, so a
/// later start cannot replay a stale click).
pub fn send(paths: &Paths, cmd: &UiCommand) -> std::io::Result<()> {
    let snapshot: crate::model::Snapshot =
        serde_json::from_slice(&std::fs::read(paths.snapshot())?).map_err(std::io::Error::other)?;
    if !snapshot.command_capable || !is_svmonitor(snapshot.pid) {
        return Err(std::io::Error::other(
            "restart SVMonitor after upgrading to enable dashboard actions",
        ));
    }
    if !cmd.is_valid() {
        return Err(std::io::Error::other("invalid dashboard command"));
    }
    let dir = cmd_dir(paths);
    crate::util::ensure_private_dir(&paths.runtime_dir)?;
    crate::util::ensure_private_dir(&dir)?;
    if let Ok(rd) = std::fs::read_dir(&dir) {
        let count = rd
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| n.ends_with(".json") && !n.starts_with('.'))
            })
            .count();
        if count >= MAX_QUEUED_COMMANDS {
            return Err(std::io::Error::other("command queue full"));
        }
    }
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let name = format!(
        "{nanos:020}-{}-{}.json",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let body = serde_json::to_vec(cmd).map_err(std::io::Error::other)?;
    let path = dir.join(name);
    crate::util::atomic_write(&path, &body)?;
    if !signal_monitor(paths, libc::SIGUSR1) {
        let _ = std::fs::remove_file(path);
        return Err(std::io::Error::other(
            "monitor stopped before receiving command",
        ));
    }
    Ok(())
}

fn trusted_file(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .ok()?;
    let md = file.metadata().ok()?;
    // SAFETY: getuid has no preconditions.
    if !md.is_file() || md.uid() != unsafe { libc::getuid() } || md.len() > MAX_FILE_BYTES {
        return None;
    }
    let mut buf = Vec::with_capacity(md.len().min(MAX_FILE_BYTES) as usize);
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut buf).ok()?;
    if buf.len() > MAX_FILE_BYTES as usize {
        return None;
    }
    Some(buf)
}

/// Consume queued commands, oldest first. Unreadable, foreign, oversized, malformed or invalid
/// files are deleted without effect.
pub fn drain(paths: &Paths) -> Vec<UiCommand> {
    let dir = cmd_dir(paths);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return vec![];
    };
    let mut files: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".json") && !n.starts_with('.'))
        })
        .collect();
    files.sort();
    files.truncate(MAX_PER_DRAIN);
    let mut out = Vec::new();
    for f in files {
        let parsed = trusted_file(&f).and_then(|b| serde_json::from_slice::<UiCommand>(&b).ok());
        let _ = std::fs::remove_file(&f);
        if let Some(c) = parsed.filter(UiCommand::is_valid) {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(dir: &Path) -> Paths {
        Paths {
            config: dir.join("config.toml"),
            state_dir: dir.join("state"),
            runtime_dir: dir.join("run"),
        }
    }

    /// A monitor "running" as this test process: snapshot with our pid and a matching comm is
    /// impossible (comm is the test binary), so write files directly like `send` would.
    fn put(p: &Paths, name: &str, body: &[u8]) {
        crate::util::ensure_private_dir(&p.runtime_dir).unwrap();
        let d = cmd_dir(p);
        crate::util::ensure_private_dir(&d).unwrap();
        crate::util::atomic_write(&d.join(name), body).unwrap();
    }

    #[test]
    fn drains_in_order_and_consumes_files() {
        let t = tempfile::tempdir().unwrap();
        let p = paths(t.path());
        put(&p, "002.json", br#"{"cmd":"reload"}"#);
        put(&p, "001.json", br#"{"cmd":"check_now","server":"catplus"}"#);
        assert_eq!(
            drain(&p),
            vec![
                UiCommand::CheckNow {
                    server: Some("catplus".into())
                },
                UiCommand::Reload
            ]
        );
        assert!(drain(&p).is_empty(), "files are consumed exactly once");
    }

    #[test]
    fn hostile_files_are_dropped_not_executed() {
        let t = tempfile::tempdir().unwrap();
        let p = paths(t.path());
        put(&p, "1.json", b"not json");
        put(
            &p,
            "2.json",
            br#"{"cmd":"remove","server":"-oProxyCommand=x"}"#,
        );
        put(&p, "3.json", br#"{"cmd":"mute","secs":99999999999}"#);
        put(&p, "4.json", &vec![b' '; 5000]);
        put(&p, "5.json", br#"{"cmd":"launch_missiles"}"#);
        std::os::unix::fs::symlink("/etc/hostname", cmd_dir(&p).join("6.json")).unwrap();
        put(
            &p,
            "7.json",
            br#"{"cmd":"set_enabled","server":"ok","enabled":false}"#,
        );
        assert_eq!(
            drain(&p),
            vec![UiCommand::SetEnabled {
                server: "ok".into(),
                enabled: false
            }]
        );
        assert!(
            std::fs::read_dir(cmd_dir(&p)).unwrap().next().is_none(),
            "rejected files are removed too"
        );
    }

    #[test]
    fn snapshot_deserializes_missing_command_capable_as_false() {
        let raw = br#"{"ts":100,"pid":1,"local_net_ok":true,"muted":false,"servers":[]}"#;
        let snap: crate::model::Snapshot = serde_json::from_slice(raw).unwrap();
        assert!(!snap.command_capable);
    }

    #[test]
    fn purge_removes_stale_spool_files() {
        let t = tempfile::tempdir().unwrap();
        let p = paths(t.path());
        put(&p, "001.json", br#"{"cmd":"reload"}"#);
        put(&p, "002.json", br#"{"cmd":"check_now"}"#);
        purge(&p);
        let dir = cmd_dir(&p);
        assert!(!dir.join("001.json").exists());
        assert!(!dir.join("002.json").exists());
    }

    #[test]
    fn drain_burst_clears_multiple_chunks() {
        let t = tempfile::tempdir().unwrap();
        let p = paths(t.path());
        for i in 0..70 {
            put(&p, &format!("{i:04}.json"), br#"{"cmd":"reload"}"#);
        }
        let chunk1 = drain(&p);
        assert_eq!(chunk1.len(), MAX_PER_DRAIN);
        let chunk2 = drain(&p);
        assert_eq!(chunk2.len(), 6);
        assert!(drain(&p).is_empty());
    }

    #[test]
    fn trusted_file_rejects_symlink_to_valid_file() {
        let t = tempfile::tempdir().unwrap();
        let target = t.path().join("target.json");
        std::fs::write(&target, br#"{"cmd":"reload"}"#).unwrap();
        let link = t.path().join("link.json");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(
            trusted_file(&link).is_none(),
            "O_NOFOLLOW must reject symlinks"
        );
    }

    #[test]
    fn send_requires_a_running_monitor() {
        let t = tempfile::tempdir().unwrap();
        let p = paths(t.path());
        assert!(send(&p, &UiCommand::Reload).is_err());
        assert!(
            !cmd_dir(&p).exists(),
            "a refused command must not be queued for a later daemon"
        );
    }
}
