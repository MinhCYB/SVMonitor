//! One probe = one noninteractive `ssh` invocation with a hard deadline.
//! Host key policy, identities, ProxyJump, Include, ports, agent: all come from the
//! user's OpenSSH configuration; we only add flags that make the probe safe and quiet.
use crate::config::Server;
use crate::metrics::{self, RawSample};
use crate::model::FailKind;
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct ProbeSpec {
    pub ssh_bin: String,
    pub server: Server,
    pub connect_timeout: Duration,
    pub deadline: Duration,
    /// Ask the remote script for a 1 s CPU sample (used when there is no baseline yet).
    pub sample_cpu: bool,
}

#[derive(Debug)]
pub enum ProbeResult {
    Ok(Box<RawSample>),
    Fail { kind: FailKind, detail: String },
}

const MAX_STDOUT: u64 = 256 * 1024;
const MAX_STDERR: u64 = 32 * 1024;

pub fn build_args(spec: &ProbeSpec) -> Vec<String> {
    let ct = spec.connect_timeout.as_secs().max(1);
    let mut a: Vec<String> = [
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        &format!("ConnectTimeout={ct}"),
        "-o",
        "ServerAliveInterval=5",
        "-o",
        "ServerAliveCountMax=2",
        "-o",
        "ClearAllForwardings=yes",
        "-o",
        "ForwardAgent=no",
        "-o",
        "ForwardX11=no",
        "-o",
        "PermitLocalCommand=no",
        "-o",
        "RemoteCommand=none",
        "-o",
        "RequestTTY=no",
        // Reuse an existing multiplexed master if the user has one, never start one.
        "-o",
        "ControlMaster=no",
        "-o",
        "LogLevel=ERROR",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    a.extend(spec.server.extra_args());
    a.push("--".into());
    a.push(spec.server.destination());
    a.push("sh".into());
    a.push("-s".into());
    a
}

pub fn classify_stderr(stderr: &str) -> FailKind {
    let s = stderr.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| s.contains(n));
    if has(&[
        "host key verification failed",
        "remote host identification has changed",
        "no ed25519 host key is known",
        "no rsa host key is known",
        "no ecdsa host key is known",
        "host key for",
    ]) {
        FailKind::HostKey
    } else if has(&[
        "permission denied",
        "too many authentication failures",
        "no more authentication methods",
        "authentication failed",
        "agent refused",
    ]) {
        FailKind::Auth
    } else if has(&[
        "could not resolve hostname",
        "name or service not known",
        "temporary failure in name resolution",
        "nodename nor servname",
    ]) {
        FailKind::Dns
    } else if has(&["timed out", "timeout"]) {
        FailKind::Timeout
    } else if has(&["connection refused"]) {
        FailKind::Refused
    } else if has(&["no route to host", "network is unreachable", "host is down"]) {
        FailKind::NoRoute
    } else if has(&[
        "connection reset",
        "connection closed by",
        "kex_exchange_identification",
        "broken pipe",
        "connection lost",
    ]) {
        FailKind::Reset
    } else {
        FailKind::Other
    }
}

fn first_line(s: &str) -> String {
    crate::util::sanitize_text(
        s.lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or(""),
        200,
    )
}

/// Interpret a finished ssh run. Separated from process handling so it is unit-testable.
pub fn interpret(exit_code: Option<i32>, stdout: &str, stderr: &str, ts: i64) -> ProbeResult {
    if exit_code == Some(0) {
        return match metrics::parse(stdout, ts) {
            Ok(r) => ProbeResult::Ok(Box::new(r)),
            Err(e) => ProbeResult::Fail {
                kind: FailKind::Remote,
                detail: e.to_string(),
            },
        };
    }
    let detail = first_line(stderr);
    if exit_code == Some(255) || exit_code.is_none() {
        let kind = classify_stderr(stderr);
        let detail = if detail.is_empty() {
            kind.label().to_string()
        } else {
            detail
        };
        return ProbeResult::Fail { kind, detail };
    }
    // Non-zero, non-255: the remote command itself failed (e.g. no `sh`).
    let detail = if detail.is_empty() {
        format!("remote exited with status {}", exit_code.unwrap_or(-1))
    } else {
        detail
    };
    ProbeResult::Fail {
        kind: FailKind::Remote,
        detail,
    }
}

fn read_capped<R: Read>(mut r: R, cap: u64) -> String {
    let mut buf = Vec::new();
    let _ = (&mut r).take(cap).read_to_end(&mut buf);
    // Keep draining so the child never blocks on a full pipe.
    let _ = std::io::copy(&mut r, &mut std::io::sink());
    String::from_utf8_lossy(&buf).into_owned()
}

pub fn run(spec: &ProbeSpec, ts: i64) -> ProbeResult {
    let mut cmd = Command::new(&spec.ssh_bin);
    cmd.args(build_args(spec))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("SSH_ASKPASS_REQUIRE")
        .process_group(0);
    // SAFETY: unblock signals in child process before exec so ssh has default signal semantics
    unsafe {
        cmd.pre_exec(|| {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigprocmask(libc::SIG_SETMASK, &set, std::ptr::null_mut());
            Ok(())
        });
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return ProbeResult::Fail {
                kind: FailKind::Spawn,
                detail: format!("{}: {e}", spec.ssh_bin),
            };
        }
    };
    let pgid = child.id() as i32;

    if let Some(mut stdin) = child.stdin.take() {
        let script = metrics::script(spec.sample_cpu);
        // Writer thread: a stuck remote must not block us past the deadline.
        std::thread::Builder::new()
            .name("svm-stdin".into())
            .stack_size(128 * 1024)
            .spawn(move || {
                let _ = stdin.write_all(script.as_bytes());
            })
            .ok();
    }
    let out = child.stdout.take().map(|o| {
        std::thread::Builder::new()
            .name("svm-out".into())
            .stack_size(128 * 1024)
            .spawn(move || read_capped(o, MAX_STDOUT))
    });
    let err = child.stderr.take().map(|e| {
        std::thread::Builder::new()
            .name("svm-err".into())
            .stack_size(128 * 1024)
            .spawn(move || read_capped(e, MAX_STDERR))
    });

    let start = Instant::now();
    let mut timed_out = false;
    let mut delay = Duration::from_millis(10);
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) => {}
            Err(_) => break None,
        }
        if start.elapsed() >= spec.deadline {
            timed_out = true;
            kill_group(pgid);
            break child.wait().ok();
        }
        std::thread::sleep(delay);
        delay = (delay * 2).min(Duration::from_millis(100));
    };
    // The whole group is gone or finished; kill stragglers (e.g. ProxyCommand children) so pipes close.
    kill_group(pgid);
    let stdout = out
        .and_then(|h| h.ok())
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    let stderr = err
        .and_then(|h| h.ok())
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    if timed_out {
        return ProbeResult::Fail {
            kind: FailKind::Timeout,
            detail: format!("no response within {}s", spec.deadline.as_secs()),
        };
    }
    let code = status.and_then(|s| s.code());
    interpret(code, &stdout, &stderr, ts)
}

fn kill_group(pgid: i32) {
    // SAFETY: plain syscall; pgid is the process group we created. Errors (ESRCH) are expected.
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(bin: &str, server: Server, deadline: u64) -> ProbeSpec {
        ProbeSpec {
            ssh_bin: bin.into(),
            server,
            connect_timeout: Duration::from_secs(5),
            deadline: Duration::from_secs(deadline),
            sample_cpu: false,
        }
    }

    #[test]
    fn args_are_safe_and_noninteractive() {
        let a = build_args(&spec("ssh", Server::alias("web"), 10));
        let j = a.join(" ");
        assert!(j.contains("BatchMode=yes"));
        assert!(j.contains("RemoteCommand=none"));
        assert!(j.contains("ClearAllForwardings=yes"));
        assert!(
            !j.contains("StrictHostKeyChecking"),
            "host key policy must stay with the user"
        );
        assert!(!j.contains("UserKnownHostsFile"));
        let n = a.len();
        assert_eq!(&a[n - 4..], ["--", "web", "sh", "-s"]);
    }

    #[test]
    fn manual_server_args() {
        let s = Server {
            host: Some("h".into()),
            user: Some("u".into()),
            port: Some(2222),
            identity_file: Some("/k".into()),
            ..Server::alias("m")
        };
        let a = build_args(&spec("ssh", s, 10));
        let i = a.iter().position(|x| x == "-p").unwrap();
        assert_eq!(a[i + 1], "2222");
        assert!(a.contains(&"u@h".to_string()));
        assert_eq!(a.iter().filter(|x| x.as_str() == "--").count(), 1);
    }

    #[test]
    fn classifies_stderr() {
        assert_eq!(
            classify_stderr("Host key verification failed."),
            FailKind::HostKey
        );
        assert_eq!(
            classify_stderr("@@@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @@@"),
            FailKind::HostKey
        );
        assert_eq!(
            classify_stderr("user@h: Permission denied (publickey)."),
            FailKind::Auth
        );
        assert_eq!(
            classify_stderr("ssh: Could not resolve hostname x: Name or service not known"),
            FailKind::Dns
        );
        assert_eq!(
            classify_stderr("ssh: connect to host x port 22: Connection timed out"),
            FailKind::Timeout
        );
        assert_eq!(
            classify_stderr("ssh: connect to host x port 22: Connection refused"),
            FailKind::Refused
        );
        assert_eq!(
            classify_stderr("ssh: connect to host x port 22: No route to host"),
            FailKind::NoRoute
        );
        assert_eq!(
            classify_stderr("ssh: connect to host x port 22: Network is unreachable"),
            FailKind::NoRoute
        );
        assert_eq!(
            classify_stderr("Connection closed by 1.2.3.4 port 22"),
            FailKind::Reset
        );
        assert_eq!(classify_stderr("something odd"), FailKind::Other);
    }

    #[test]
    fn stderr_detail_is_sanitised() {
        match interpret(Some(255), "", "\x1b[2Jboom\x07\n", 0) {
            ProbeResult::Fail { detail, .. } => {
                assert!(!detail.chars().any(|c| c.is_control()), "{detail:?}")
            }
            _ => panic!(),
        }
    }

    #[test]
    fn interprets_exit_codes() {
        let ok = "svm=1\nload=1 1 1\nuptime=5\nmem_total=100\nmem_avail=50\nsvm_end=1\n";
        assert!(matches!(interpret(Some(0), ok, "", 0), ProbeResult::Ok(_)));
        assert!(matches!(
            interpret(Some(0), "garbage", "", 0),
            ProbeResult::Fail {
                kind: FailKind::Remote,
                ..
            }
        ));
        assert!(matches!(
            interpret(Some(127), "", "sh: not found", 0),
            ProbeResult::Fail {
                kind: FailKind::Remote,
                ..
            }
        ));
        assert!(matches!(
            interpret(Some(255), "", "Permission denied (publickey).", 0),
            ProbeResult::Fail {
                kind: FailKind::Auth,
                ..
            }
        ));
        assert!(matches!(
            interpret(None, "", "", 0),
            ProbeResult::Fail {
                kind: FailKind::Other,
                ..
            }
        ));
    }

    #[test]
    fn missing_binary_is_spawn_failure() {
        let r = run(&spec("/nonexistent/ssh", Server::alias("x"), 5), 0);
        assert!(matches!(
            r,
            ProbeResult::Fail {
                kind: FailKind::Spawn,
                ..
            }
        ));
    }
}
