//! End-to-end engine tests using a fake `ssh` binary whose behaviour is switched through a file.
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};
use svmonitor::config::{Config, Paths, Server};
use svmonitor::engine::{Engine, Handle};
use svmonitor::history;
use svmonitor::model::*;

const FAKE: &str = r#"#!/bin/sh
# usage: fake-ssh <opts...> -- dest sh -s ; behaviour from a per-test mode file
mode=$(cat "@MODE_FILE@")
cat >/dev/null
case "$mode" in
  ok) printf 'svm=1\nload=0.10 0.10 0.10\nprocs=100\nuptime=5000.5\ncpu=100 0 100 800 0 0 0 0\ncpu2=110 0 110 880 0 0 0 0\ncores=2\nmem_total=1000\nmem_avail=900\nswap_total=0\nswap_free=0\ndisk=1000|100|/\nsvm_end=1\n';;
  timeout) echo "ssh: connect to host x port 22: Connection timed out" >&2; exit 255;;
  auth) echo "x@y: Permission denied (publickey)." >&2; exit 255;;
  hang) sleep 60;;
esac
"#;

struct Rig {
    handle: Handle,
    dir: tempfile::TempDir,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn mode(dir: &Path, m: &str) {
    std::fs::write(dir.join("mode"), m).unwrap();
}

fn start(servers: &[&str], tweak: impl FnOnce(&mut Config)) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("fake-ssh");
    std::fs::write(&fake, FAKE.replace("@MODE_FILE@", dir.path().join("mode").to_str().unwrap())).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    mode(dir.path(), "ok");
    let mut cfg = Config::default();
    cfg.general.ssh_binary = fake.to_str().unwrap().into();
    cfg.general.interval_secs = 5;
    cfg.general.retry_interval_secs = 1;
    cfg.general.down_interval_max_secs = 2;
    cfg.general.failure_threshold = 2;
    cfg.general.probe_timeout_secs = 3;
    cfg.general.check_tailscale = false;
    cfg.general.notifications = false;
    for s in servers {
        let mut sv = Server::alias(s);
        sv.host = Some(format!("{s}.invalid"));
        cfg.add(sv).unwrap();
    }
    tweak(&mut cfg);
    let paths = Paths { config: dir.path().join("config.toml"), state_dir: dir.path().join("state"), runtime_dir: dir.path().join("run") };
    let (engine, handle) = Engine::new(cfg, paths, false);
    let engine = engine.without_persistence();
    let thread = Some(std::thread::spawn(move || engine.run()));
    Rig { handle, dir, thread }
}

impl Rig {
    fn wait(&self, name: &str, want: Overall, secs: u64) -> ServerView {
        let end = Instant::now() + Duration::from_secs(secs);
        loop {
            let snap = self.handle.snapshot().unwrap();
            let v = snap.servers.into_iter().find(|s| s.name == name).unwrap();
            if v.overall == want {
                return v;
            }
            assert!(Instant::now() < end, "timed out waiting for {want:?}, last {:?} detail={:?}", v.overall, v.detail);
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    fn mode(&self, m: &str) {
        mode(self.dir.path(), m);
    }
    fn events(&self) -> Vec<Event> {
        history::read_events(&self.dir.path().join("state/events.jsonl"), None, 100)
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.handle.shutdown();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[test]
fn healthy_server_goes_online_with_metrics_and_cpu() {
    let r = start(&["a"], |_| {});
    let v = r.wait("a", Overall::Online, 10);
    let m = v.metrics.unwrap();
    assert_eq!(m.cores, Some(2));
    assert!((m.cpu_pct.unwrap() - 20.0).abs() < 0.1, "{:?}", m.cpu_pct);
    assert_eq!(v.ssh, SshState::Available);
}

#[test]
fn outage_is_debounced_then_reported_then_recovers_once() {
    let r = start(&["a"], |_| {});
    r.wait("a", Overall::Online, 10);
    r.mode("timeout");
    r.handle.check_now(Some("a"));
    let v = r.wait("a", Overall::Down, 15);
    assert_eq!(v.ssh, SshState::Unavailable(FailKind::Timeout));
    assert!(v.down_since.is_some());
    r.mode("ok");
    r.handle.check_now(Some("a"));
    r.wait("a", Overall::Online, 15);
    let kinds: Vec<_> = r.events().iter().map(|e| e.kind).filter(|k| matches!(k, EventKind::Down | EventKind::Recovered)).collect();
    assert_eq!(kinds, vec![EventKind::Down, EventKind::Recovered]);
}

#[test]
fn auth_failure_is_attention_not_offline() {
    let r = start(&["a"], |_| {});
    r.mode("auth");
    let v = r.wait("a", Overall::Attention, 15);
    assert_eq!(v.ssh, SshState::Unavailable(FailKind::Auth));
}

#[test]
fn hung_ssh_is_killed_at_deadline() {
    let r = start(&["a"], |c| {
        c.general.probe_timeout_secs = 3;
        c.general.failure_threshold = 1;
    });
    r.mode("hang");
    let t = Instant::now();
    r.wait("a", Overall::Down, 15);
    assert!(t.elapsed() < Duration::from_secs(8), "took {:?}", t.elapsed());
}

#[test]
fn disabled_server_is_never_probed_and_state_is_clean() {
    let r = start(&["a", "b"], |c| c.server_mut("b").unwrap().enabled = false);
    r.wait("a", Overall::Online, 10);
    let snap = r.handle.snapshot().unwrap();
    let b = snap.servers.iter().find(|s| s.name == "b").unwrap();
    assert_eq!(b.overall, Overall::Disabled);
    assert!(b.last_check.is_none());
    r.handle.set_enabled("b", true).unwrap();
    r.wait("b", Overall::Online, 10);
    r.handle.set_enabled("b", false).unwrap();
    assert_eq!(r.wait("b", Overall::Disabled, 2).name, "b");
    assert!(r.handle.set_enabled("nope", true).is_err());
}

#[test]
fn add_and_remove_at_runtime() {
    let r = start(&["a"], |_| {});
    let mut s = Server::alias("c");
    s.host = Some("c.invalid".into());
    r.handle.add(s.clone()).unwrap();
    assert!(r.handle.add(s).is_err(), "duplicate rejected");
    r.wait("c", Overall::Online, 10);
    r.handle.remove("c").unwrap();
    assert!(r.handle.snapshot().unwrap().servers.iter().all(|s| s.name != "c"));
    assert!(r.handle.add(Server::alias("-oProxyCommand=evil")).is_err());
}

#[test]
fn never_more_than_max_parallel_probes() {
    let r = start(&["a", "b", "c", "d"], |c| {
        c.general.max_parallel_probes = 1;
        c.general.probe_timeout_secs = 3;
    });
    r.mode("hang");
    std::thread::sleep(Duration::from_millis(1500));
    let probing = r.handle.snapshot().unwrap().servers.iter().filter(|s| s.probing).count();
    assert_eq!(probing, 1);
}
