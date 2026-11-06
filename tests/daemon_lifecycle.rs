//! Integration regression tests for daemon lifecycle, signals, and reload.
//!
//! Tests real daemon execution with notifications enabled and isolated XDG directories.
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use svmonitor::config::Paths;
use svmonitor::model::{ServerView, Snapshot};

const FAKE_SSH: &str = r#"#!/bin/sh
mode=$(cat "@MODE_FILE@")
cat >/dev/null
case "$*" in
  *alpha-new*)
    uptime=9999.0
    ;;
  *alpha-old*)
    uptime=1111.0
    ;;
  *)
    uptime=5000.0
    ;;
esac
case "$mode" in
  delay)
    sleep 2
    printf "svm=1\nload=0.20 0.20 0.20\nprocs=100\nuptime=${uptime}\ncpu=100 0 100 800 0 0 0 0\ncpu2=110 0 110 880 0 0 0 0\ncores=2\nmem_total=1000\nmem_avail=900\nswap_total=0\nswap_free=0\ndisk=1000|100|/\nsvm_end=1\n"
    ;;
  fail)
    echo "ssh: connect to host x port 22: Connection refused" >&2
    exit 255
    ;;
  *)
    printf "svm=1\nload=0.10 0.10 0.10\nprocs=100\nuptime=${uptime}\ncpu=100 0 100 800 0 0 0 0\ncpu2=110 0 110 880 0 0 0 0\ncores=2\nmem_total=1000\nmem_avail=900\nswap_total=0\nswap_free=0\ndisk=1000|100|/\nsvm_end=1\n"
    ;;
esac
"#;

struct TestDaemon {
    child: Child,
    dir: tempfile::TempDir,
    paths: Paths,
    mode_file: PathBuf,
    stderr_buf: Arc<Mutex<String>>,
}

impl TestDaemon {
    fn spawn(notifications_enabled: bool, initial_config: &str) -> Self {
        let dir = tempfile::tempdir().expect("failed to create tempdir");
        let fake_ssh = dir.path().join("fake-ssh");
        let mode_file = dir.path().join("mode");
        fs::write(&mode_file, "ok").expect("failed to write mode file");

        let script = FAKE_SSH.replace("@MODE_FILE@", mode_file.to_str().unwrap());
        fs::write(&fake_ssh, script).expect("failed to write fake-ssh");
        fs::set_permissions(&fake_ssh, fs::Permissions::from_mode(0o755))
            .expect("failed to chmod fake-ssh");

        let cfg_dir = dir.path().join("config").join("svmonitor");
        let state_dir = dir.path().join("state").join("svmonitor");
        let run_base = dir.path().join("run");
        let run_dir = run_base.join("svmonitor");

        fs::create_dir_all(&cfg_dir).expect("create cfg_dir");
        fs::create_dir_all(&state_dir).expect("create state_dir");
        fs::create_dir_all(&run_base).expect("create run_base");
        fs::set_permissions(&run_base, fs::Permissions::from_mode(0o700))
            .expect("set run_base perms");

        let cfg_path = cfg_dir.join("config.toml");
        let config_content = initial_config.replace("@FAKE_SSH@", fake_ssh.to_str().unwrap());
        fs::write(&cfg_path, config_content).expect("write config.toml");

        let paths = Paths {
            config: cfg_path,
            state_dir,
            runtime_dir: run_dir,
        };

        let bin = env!("CARGO_BIN_EXE_svmonitor");
        let mut cmd = Command::new(bin);
        cmd.arg("run").arg("--headless");
        if !notifications_enabled {
            cmd.arg("--no-notify");
        }
        cmd.env("XDG_CONFIG_HOME", dir.path().join("config"))
            .env("XDG_STATE_HOME", dir.path().join("state"))
            .env("XDG_RUNTIME_DIR", &run_base)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().expect("failed to spawn svmonitor daemon");

        let stderr_buf = Arc::new(Mutex::new(String::new()));
        let stderr_clone = stderr_buf.clone();
        if let Some(mut err_pipe) = child.stderr.take() {
            std::thread::Builder::new()
                .name("test-stderr-drain".into())
                .spawn(move || {
                    let mut text = String::new();
                    let _ = err_pipe.read_to_string(&mut text);
                    if let Ok(mut lock) = stderr_clone.lock() {
                        *lock = text;
                    }
                })
                .expect("spawn stderr reader");
        }

        let mut daemon = Self {
            child,
            dir,
            paths,
            mode_file,
            stderr_buf,
        };

        // Wait until state.json snapshot is published and pid is set
        daemon.wait_snapshot(Duration::from_secs(5));
        daemon
    }

    fn stderr_text(&self) -> String {
        self.stderr_buf
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    fn set_mode(&self, mode: &str) {
        fs::write(&self.mode_file, mode).expect("failed to set mode");
    }

    fn snapshot_path(&self) -> PathBuf {
        self.paths.snapshot()
    }

    fn try_read_snapshot(&self) -> Option<Snapshot> {
        let bytes = fs::read(self.snapshot_path()).ok()?;
        serde_json::from_slice::<Snapshot>(&bytes).ok()
    }

    fn wait_snapshot(&mut self, timeout: Duration) -> Snapshot {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Ok(Some(st)) = self.child.try_wait() {
                let err = self.stderr_text();
                panic!(
                    "daemon exited early with status {st:?}; stderr: {err}\nsnapshot_path: {}",
                    self.snapshot_path().display()
                );
            }
            if let Some(snap) = self.try_read_snapshot()
                && snap.pid != 0
            {
                return snap;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let err = self.stderr_text();
        panic!(
            "timed out waiting for snapshot at {} (pid={}); stderr: {err}",
            self.snapshot_path().display(),
            self.child.id()
        );
    }

    fn wait_for_server(
        &mut self,
        name: &str,
        timeout: Duration,
        predicate: impl Fn(&ServerView) -> bool,
    ) -> ServerView {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Ok(Some(st)) = self.child.try_wait() {
                let err = self.stderr_text();
                panic!(
                    "daemon died with status {st:?} while waiting for server {name}; stderr: {err}"
                );
            }
            if let Some(snap) = self.try_read_snapshot()
                && let Some(s) = snap.servers.into_iter().find(|s| s.name == name)
                && predicate(&s)
            {
                return s;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let err = self.stderr_text();
        panic!(
            "timed out waiting for server {name} to satisfy predicate (pid={}); stderr: {err}",
            self.child.id()
        );
    }

    fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn send_signal(&mut self, sig: libc::c_int) {
        let pid = self.child.id() as i32;
        // SAFETY: pid is our spawned child process.
        unsafe {
            libc::kill(pid, sig);
        }
    }

    fn cli_cmd(&self, args: &[&str]) -> std::process::Output {
        let bin = env!("CARGO_BIN_EXE_svmonitor");
        let mut cmd = Command::new(bin);
        cmd.arg("--config").arg(&self.paths.config);
        for a in args {
            cmd.arg(a);
        }
        cmd.env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env("XDG_STATE_HOME", self.dir.path().join("state"))
            .env("XDG_RUNTIME_DIR", self.paths.runtime_dir.parent().unwrap())
            .output()
            .expect("failed to run cli command")
    }

    fn update_config(&self, new_content: &str) {
        let fake_ssh = self.dir.path().join("fake-ssh");
        let rendered = new_content.replace("@FAKE_SSH@", fake_ssh.to_str().unwrap());
        fs::write(&self.paths.config, rendered).expect("failed to update config.toml");
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        if self.is_running() {
            let pid = self.child.id() as i32;
            // SAFETY: terminating child on drop.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
            let _ = self.child.wait();
        }
    }
}

const DEFAULT_CONFIG: &str = r#"
[general]
ssh_binary = "@FAKE_SSH@"
interval_secs = 5
probe_timeout_secs = 3
notifications = true
check_tailscale = false

[[server]]
name = "alpha"
host = "alpha.test"
"#;

#[test]
fn notifications_enabled_survives_repeated_sighup() {
    let mut daemon = TestDaemon::spawn(true, DEFAULT_CONFIG);
    let original_pid = daemon.child.id();

    // Verify initial snapshot exists and monitoring works
    let snap = daemon.wait_snapshot(Duration::from_secs(3));
    assert_eq!(snap.pid, original_pid);
    assert_eq!(snap.servers.len(), 1);
    assert_eq!(snap.servers[0].name, "alpha");

    // Verify PID is live via kill(pid, 0)
    // SAFETY: pid check
    let alive = unsafe { libc::kill(original_pid as i32, 0) == 0 };
    assert!(alive, "daemon pid {original_pid} must be running");

    // Send SIGHUP 3 times in succession with small pauses
    for i in 1..=3 {
        daemon.send_signal(libc::SIGHUP);
        std::thread::sleep(Duration::from_millis(150));
        let running = daemon.is_running();
        if !running {
            let status = daemon.child.try_wait();
            let err = daemon.stderr_text();
            panic!(
                "daemon must survive SIGHUP #{i} with notifications enabled, but died with status {status:?}; stderr: {err}"
            );
        }
        assert_eq!(
            daemon.child.id(),
            original_pid,
            "daemon PID must remain constant across reload #{i}"
        );
    }

    // Also verify CLI reload command succeeds and daemon stays alive
    let out = daemon.cli_cmd(&["reload"]);
    assert!(out.status.success(), "cli reload should succeed");
    std::thread::sleep(Duration::from_millis(150));
    assert!(daemon.is_running(), "daemon must survive CLI reload");
    assert_eq!(daemon.child.id(), original_pid);

    // Verify SIGTERM cleanly shuts down and removes snapshot
    daemon.send_signal(libc::SIGTERM);
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut exited = false;
    while Instant::now() < deadline {
        if !daemon.is_running() {
            exited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(exited, "daemon should have exited after SIGTERM");
    assert!(
        !daemon.snapshot_path().exists(),
        "snapshot file state.json must be deleted on clean shutdown"
    );
}

#[test]
fn no_notify_daemon_survives_sighup() {
    let mut daemon = TestDaemon::spawn(false, DEFAULT_CONFIG);
    let original_pid = daemon.child.id();

    let snap = daemon.wait_snapshot(Duration::from_secs(3));
    assert_eq!(snap.pid, original_pid);

    daemon.send_signal(libc::SIGHUP);
    std::thread::sleep(Duration::from_millis(150));
    assert!(daemon.is_running(), "no-notify daemon must survive SIGHUP");
    assert_eq!(daemon.child.id(), original_pid);

    daemon.send_signal(libc::SIGTERM);
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && daemon.is_running() {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!daemon.is_running());
    assert!(!daemon.snapshot_path().exists());
}

#[test]
fn reload_applies_server_mutations() {
    let mut daemon = TestDaemon::spawn(true, DEFAULT_CONFIG);
    let original_pid = daemon.child.id();

    // 1. Disable server via CLI
    let out = daemon.cli_cmd(&["disable", "alpha"]);
    assert!(out.status.success(), "cli disable should succeed");

    daemon.wait_for_server("alpha", Duration::from_secs(3), |s| !s.enabled);
    assert_eq!(daemon.child.id(), original_pid);

    // 2. Enable server via CLI
    let out = daemon.cli_cmd(&["enable", "alpha"]);
    assert!(out.status.success(), "cli enable should succeed");

    daemon.wait_for_server("alpha", Duration::from_secs(3), |s| s.enabled);
    assert_eq!(daemon.child.id(), original_pid);

    // 3. Add server via CLI
    let out = daemon.cli_cmd(&["add", "beta", "--host", "beta.test"]);
    assert!(out.status.success(), "cli add should succeed");

    daemon.wait_for_server("beta", Duration::from_secs(3), |s| s.name == "beta");
    assert_eq!(daemon.child.id(), original_pid);

    // 4. Remove server via CLI
    let out = daemon.cli_cmd(&["remove", "beta"]);
    assert!(out.status.success(), "cli remove should succeed");

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut removed = false;
    while Instant::now() < deadline {
        if let Some(snap) = daemon.try_read_snapshot()
            && !snap.servers.iter().any(|s| s.name == "beta")
        {
            removed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(removed, "beta should be removed from snapshot");
    assert_eq!(daemon.child.id(), original_pid);

    daemon.send_signal(libc::SIGTERM);
}

#[test]
fn invalid_config_reload_preserves_prior_valid_monitoring() {
    let mut daemon = TestDaemon::spawn(true, DEFAULT_CONFIG);
    let original_pid = daemon.child.id();

    daemon.wait_for_server("alpha", Duration::from_secs(3), |s| s.enabled);

    // Corrupt the config with invalid syntax
    daemon.update_config("INVALID SYNTAX [[[[ not toml");

    // Send SIGHUP to trigger reload
    daemon.send_signal(libc::SIGHUP);
    std::thread::sleep(Duration::from_millis(200));

    // Daemon must remain alive with same PID and keep previous valid configuration
    assert!(
        daemon.is_running(),
        "daemon must not crash when reloaded with invalid config"
    );
    assert_eq!(daemon.child.id(), original_pid);

    // Snapshot should still be valid and monitoring alpha
    let snap = daemon.try_read_snapshot().expect("snapshot should exist");
    assert_eq!(snap.pid, original_pid);
    assert!(
        snap.servers.iter().any(|s| s.name == "alpha"),
        "alpha should still be monitored"
    );

    daemon.send_signal(libc::SIGTERM);
}

#[test]
fn in_flight_probe_does_not_overwrite_changed_server_identity() {
    let initial_config = r#"
[general]
ssh_binary = "@FAKE_SSH@"
interval_secs = 5
probe_timeout_secs = 3
notifications = true
check_tailscale = false

[[server]]
name = "alpha"
host = "alpha-old.test"
"#;
    let mut daemon = TestDaemon::spawn(true, initial_config);
    let original_pid = daemon.child.id();

    // Wait for initial check against alpha-old (uptime 1111)
    daemon.wait_for_server("alpha", Duration::from_secs(3), |s| {
        s.target == "alpha-old.test" && s.metrics.as_ref().map(|m| m.uptime_secs) == Some(1111)
    });

    // Set probe mode to delay so next probe hangs in flight for 2 seconds
    daemon.set_mode("delay");

    // Trigger check now to launch delayed in-flight probe to alpha-old
    let _ = daemon.cli_cmd(&["check"]);

    // While probe to alpha-old is hanging in flight, update config to alpha-new and reload
    let updated_config = r#"
[general]
ssh_binary = "@FAKE_SSH@"
interval_secs = 5
probe_timeout_secs = 3
notifications = true
check_tailscale = false

[[server]]
name = "alpha"
host = "alpha-new.test"
"#;
    daemon.update_config(updated_config);
    daemon.send_signal(libc::SIGHUP);

    // Switch mode back to ok for new probes
    daemon.set_mode("ok");

    // Wait for in-flight probe to finish and alpha-new to be probed (uptime 9999)
    let s = daemon.wait_for_server("alpha", Duration::from_secs(6), |s| {
        s.target == "alpha-new.test" && s.metrics.as_ref().map(|m| m.uptime_secs) == Some(9999)
    });
    assert_eq!(s.target, "alpha-new.test");
    assert_eq!(s.metrics.as_ref().unwrap().uptime_secs, 9999);
    assert_eq!(daemon.child.id(), original_pid);

    // Sleep past any residual old probe time to confirm stale alpha-old (uptime 1111) never overwrites
    std::thread::sleep(Duration::from_millis(500));
    let final_s = daemon.wait_for_server("alpha", Duration::from_secs(1), |s| {
        s.target == "alpha-new.test"
    });
    assert_eq!(final_s.target, "alpha-new.test");
    assert_eq!(final_s.metrics.as_ref().unwrap().uptime_secs, 9999);
    assert_eq!(daemon.child.id(), original_pid);
    daemon.send_signal(libc::SIGTERM);
}

#[test]
fn sigterm_terminates_and_cleans_up_runtime() {
    let mut daemon = TestDaemon::spawn(true, DEFAULT_CONFIG);
    let snap_path = daemon.snapshot_path();
    let lock_path = daemon.paths.lock();

    assert!(daemon.is_running());
    assert!(snap_path.exists());
    assert!(lock_path.exists());

    daemon.send_signal(libc::SIGTERM);

    let deadline = Instant::now() + Duration::from_secs(4);
    let mut exited = false;
    while Instant::now() < deadline {
        if !daemon.is_running() {
            exited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(exited, "daemon must terminate on SIGTERM");
    assert!(
        !snap_path.exists(),
        "state.json must be cleaned up on SIGTERM"
    );

    // Lock file must be unlocked so another daemon can start immediately
    let mut second = TestDaemon::spawn(true, DEFAULT_CONFIG);
    assert!(
        second.is_running(),
        "second daemon should acquire lock and run"
    );
    second.send_signal(libc::SIGTERM);
}
