//! The monitoring engine: one thread blocked on a command channel with a timeout equal to the next due probe.
//! Probes run on short-lived worker threads (bounded parallelism). Idle cost = wake-ups at probe times only.
use crate::config::{self, Config, Paths, Server};
use crate::health::{Context, Outcome, Tracker};
use crate::history::History;
use crate::model::*;
use crate::notify::{self, Limiter, Notifier};
use crate::probe::{self, ProbeResult, ProbeSpec};
use crate::{localnet, sshconfig, tailscale, util};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub type Reply = Option<Sender<Result<(), String>>>;

pub enum Cmd {
    ProbeDone { name: String, generation: u64, host: Option<String>, ts: TsState, outcome: Outcome },
    /// Probe now: one server or all enabled.
    CheckNow(Option<String>),
    SetEnabled(String, bool, Reply),
    Add(Server, Reply),
    Remove(String, Reply),
    Reload,
    /// Mute notifications for N seconds; None clears.
    Mute(Option<u64>),
    Snapshot(Sender<Snapshot>),
    Shutdown,
}

#[derive(Clone)]
pub struct Handle {
    tx: Sender<Cmd>,
}

impl Handle {
    pub fn send(&self, c: Cmd) {
        let _ = self.tx.send(c);
    }
    fn call(&self, make: impl FnOnce(Reply) -> Cmd) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.tx.send(make(Some(tx))).map_err(|_| "engine stopped".to_string())?;
        rx.recv_timeout(Duration::from_secs(5)).map_err(|_| "engine did not answer".to_string())?
    }
    pub fn set_enabled(&self, name: &str, on: bool) -> Result<(), String> {
        self.call(|r| Cmd::SetEnabled(name.into(), on, r))
    }
    pub fn add(&self, s: Server) -> Result<(), String> {
        self.call(|r| Cmd::Add(s, r))
    }
    pub fn remove(&self, name: &str) -> Result<(), String> {
        self.call(|r| Cmd::Remove(name.into(), r))
    }
    pub fn check_now(&self, name: Option<&str>) {
        self.send(Cmd::CheckNow(name.map(String::from)));
    }
    pub fn snapshot(&self) -> Option<Snapshot> {
        let (tx, rx) = mpsc::channel();
        self.tx.send(Cmd::Snapshot(tx)).ok()?;
        rx.recv_timeout(Duration::from_secs(5)).ok()
    }
    pub fn shutdown(&self) {
        self.send(Cmd::Shutdown);
    }
}

pub type Sink = Box<dyn FnMut(&Snapshot) + Send>;
pub type EventSink = Box<dyn FnMut(&Event) + Send>;

struct Slot {
    server: Server,
    tracker: Tracker,
    generation: u64,
    due: Instant,
    inflight: bool,
    host: Option<String>,
}

#[derive(Default)]
struct TsCache {
    inner: Mutex<Option<(Instant, tailscale::Status)>>,
}

impl TsCache {
    fn get(&self) -> tailscale::Status {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((t, s)) = g.as_ref() {
            if t.elapsed() < Duration::from_secs(5) {
                return s.clone();
            }
        }
        let s = tailscale::fetch(std::path::Path::new(tailscale::SOCKET), Duration::from_secs(2));
        *g = Some((Instant::now(), s.clone()));
        s
    }
}

pub struct Engine {
    cfg: Config,
    paths: Paths,
    slots: Vec<Slot>,
    rx: Receiver<Cmd>,
    tx: Sender<Cmd>,
    history: History,
    limiter: Limiter,
    notifier: Option<Notifier>,
    sinks: Vec<Sink>,
    event_sinks: Vec<EventSink>,
    ts_cache: Arc<TsCache>,
    next_gen: u64,
    inflight: usize,
    muted_until: i64,
    grace_until: Option<Instant>,
    last_drift: f64,
    local_net_ok: bool,
    last_published: Option<Snapshot>,
    persist_config: bool,
}

const MAX_WAIT: Duration = Duration::from_secs(30);
const RESUME_DRIFT_SECS: f64 = 10.0;
const RESUME_GRACE: Duration = Duration::from_secs(30);

fn drift() -> f64 {
    let get = |clk| {
        let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: valid pointer to a timespec; clock ids are constants.
        unsafe { libc::clock_gettime(clk, &mut ts) };
        ts.tv_sec as f64 + ts.tv_nsec as f64 / 1e9
    };
    get(libc::CLOCK_BOOTTIME) - get(libc::CLOCK_MONOTONIC)
}

fn jitter(name: &str, interval: u64) -> Duration {
    let mut h = DefaultHasher::new();
    name.hash(&mut h);
    let max_ms = (interval * 1000 / 10).clamp(500, 10_000);
    Duration::from_millis(h.finish() % max_ms)
}

impl Engine {
    pub fn new(cfg: Config, paths: Paths, with_notifications: bool) -> (Engine, Handle) {
        let (tx, rx) = mpsc::channel();
        let history = History::new(paths.events(), paths.samples(), cfg.general.keep_samples);
        let mut e = Engine {
            notifier: (with_notifications && cfg.general.notifications).then(Notifier::spawn),
            cfg,
            paths,
            slots: Vec::new(),
            rx,
            tx: tx.clone(),
            history,
            limiter: Limiter::default(),
            sinks: Vec::new(),
            event_sinks: Vec::new(),
            ts_cache: Arc::new(TsCache::default()),
            next_gen: 1,
            inflight: 0,
            muted_until: 0,
            grace_until: None,
            last_drift: drift(),
            local_net_ok: true,
            last_published: None,
            persist_config: true,
        };
        let servers = e.cfg.servers.clone();
        for s in servers {
            e.add_slot(s);
        }
        (e, Handle { tx })
    }

    /// Tests: do not write the config file.
    pub fn without_persistence(mut self) -> Engine {
        self.persist_config = false;
        self
    }

    pub fn add_sink(&mut self, s: Sink) {
        self.sinks.push(s);
    }

    pub fn add_event_sink(&mut self, s: EventSink) {
        self.event_sinks.push(s);
    }

    fn add_slot(&mut self, server: Server) {
        let t = Tracker::new(&server.name, &server.destination(), server.enabled);
        let generation = self.next_gen;
        self.next_gen += 1;
        let interval = self.cfg.interval_for(&server);
        let due = Instant::now() + jitter(&server.name, interval);
        let host = server.host.clone();
        self.slots.push(Slot { server, tracker: t, generation, due, inflight: false, host });
    }

    pub fn run(mut self) {
        let now_u = util::now_unix();
        self.history.record_event(&Event { ts: now_u, server: String::new(), kind: EventKind::Started, message: "svmonitor started".into(), key: String::new(), notify: false });
        self.publish(true);
        loop {
            self.detect_resume();
            self.dispatch();
            let wait = self.next_wait();
            match self.rx.recv_timeout(wait) {
                Ok(cmd) => {
                    if !self.handle(cmd) {
                        break;
                    }
                    while let Ok(cmd) = self.rx.try_recv() {
                        if !self.handle(cmd) {
                            return self.finish();
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            self.publish(false);
        }
        self.finish();
    }

    fn finish(&mut self) {
        let _ = std::fs::remove_file(self.paths.snapshot());
    }

    fn next_wait(&self) -> Duration {
        let now = Instant::now();
        let mut wait = MAX_WAIT;
        for s in &self.slots {
            if s.tracker.enabled && !s.inflight {
                wait = wait.min(s.due.saturating_duration_since(now));
            }
        }
        if let Some(g) = self.grace_until {
            wait = wait.min(g.saturating_duration_since(now).max(Duration::from_millis(50)));
        }
        wait
    }

    fn detect_resume(&mut self) {
        let d = drift();
        if d - self.last_drift > RESUME_DRIFT_SECS {
            let now = Instant::now();
            self.grace_until = Some(now + RESUME_GRACE);
            for s in &mut self.slots {
                s.tracker.forgive();
                s.due = now + Duration::from_secs(3) + jitter(&s.server.name, 30);
            }
        }
        self.last_drift = d;
        if self.grace_until.is_some_and(|g| Instant::now() >= g) {
            self.grace_until = None;
        }
    }

    fn dispatch(&mut self) {
        let now = Instant::now();
        let max = self.cfg.general.max_parallel_probes;
        let mut order: Vec<usize> = (0..self.slots.len()).filter(|&i| self.slots[i].tracker.enabled && !self.slots[i].inflight && self.slots[i].due <= now).collect();
        order.sort_by_key(|&i| self.slots[i].due);
        for i in order {
            if self.inflight >= max {
                break;
            }
            self.start_probe(i);
        }
    }

    fn start_probe(&mut self, i: usize) {
        let g = &self.cfg.general;
        let slot = &mut self.slots[i];
        slot.inflight = true;
        self.inflight += 1;
        let spec = ProbeSpec {
            ssh_bin: g.ssh_binary.clone(),
            server: slot.server.clone(),
            connect_timeout: Duration::from_secs(g.connect_timeout_secs),
            deadline: Duration::from_secs(g.probe_timeout_secs),
            sample_cpu: slot.tracker.needs_cpu_baseline(),
        };
        let name = slot.server.name.clone();
        let generation = slot.generation;
        let known_host = slot.host.clone();
        let check_ts = g.check_tailscale;
        let cache = self.ts_cache.clone();
        let tx = self.tx.clone();
        let spawn = std::thread::Builder::new().name("svm-probe".into()).stack_size(256 * 1024).spawn({
            let name = name.clone();
            move || {
                let host = known_host.or_else(|| sshconfig::resolve(&spec.ssh_bin, &spec.server.name).map(|r| r.hostname));
                let ts = match (&host, check_ts) {
                    (Some(h), true) => tailscale::classify(h, &cache.get()),
                    _ => TsState::NotApplicable,
                };
                let outcome = if ts.is_local_problem() {
                    Outcome::Fail { kind: FailKind::Timeout, detail: String::new() }
                } else {
                    match probe::run(&spec, util::now_unix()) {
                        ProbeResult::Ok(s) => Outcome::Ok(s),
                        ProbeResult::Fail { kind, detail } => Outcome::Fail { kind, detail },
                    }
                };
                let _ = tx.send(Cmd::ProbeDone { name, generation, host, ts, outcome });
            }
        });
        if spawn.is_err() {
            // Out of threads: report as a failure of this probe so scheduling continues.
            let _ = self.tx.send(Cmd::ProbeDone { name, generation, host: None, ts: TsState::Unknown, outcome: Outcome::Fail { kind: FailKind::Spawn, detail: "cannot start probe thread".into() } });
        }
    }

    /// Returns false to stop.
    fn handle(&mut self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::ProbeDone { name, generation, host, ts, outcome } => self.on_probe_done(&name, generation, host, ts, outcome),
            Cmd::CheckNow(name) => {
                let now = Instant::now();
                for s in &mut self.slots {
                    if s.tracker.enabled && name.as_deref().is_none_or(|n| n == s.server.name) {
                        s.due = now;
                    }
                }
            }
            Cmd::SetEnabled(name, on, reply) => {
                let r = self.set_enabled(&name, on);
                if let Some(r_) = reply {
                    let _ = r_.send(r);
                }
            }
            Cmd::Add(s, reply) => {
                let r = self.add_server(s);
                if let Some(r_) = reply {
                    let _ = r_.send(r);
                }
            }
            Cmd::Remove(name, reply) => {
                let r = self.remove_server(&name);
                if let Some(r_) = reply {
                    let _ = r_.send(r);
                }
            }
            Cmd::Reload => self.reload(),
            Cmd::Mute(secs) => {
                self.muted_until = secs.map(|s| util::now_unix() + s as i64).unwrap_or(0);
            }
            Cmd::Snapshot(tx) => {
                let _ = tx.send(self.snapshot());
            }
            Cmd::Shutdown => return false,
        }
        true
    }

    fn on_probe_done(&mut self, name: &str, generation: u64, host: Option<String>, ts: TsState, outcome: Outcome) {
        let Some(i) = self.slots.iter().position(|s| s.server.name == name && s.generation == generation) else {
            // Removed or replaced while in flight.
            self.inflight = self.inflight.saturating_sub(1);
            return;
        };
        self.inflight = self.inflight.saturating_sub(1);
        let now_u = util::now_unix();
        // Cheap local check at completion time.
        let was_ok = self.local_net_ok;
        self.local_net_ok = localnet::has_default_route();
        let slot = &mut self.slots[i];
        slot.inflight = false;
        if slot.host.is_none() {
            slot.host = host;
        }
        if !was_ok && self.local_net_ok {
            for s in &mut self.slots {
                s.tracker.forgive();
            }
        }
        let slot = &mut self.slots[i];
        let in_grace = self.grace_until.is_some();
        if let (true, Outcome::Fail { kind, .. }) = (in_grace, &outcome) {
            if kind.is_connectivity() || *kind == FailKind::Dns {
                // Just resumed from suspend: the network is probably still coming up.
                slot.due = Instant::now() + Duration::from_secs(5);
                return;
            }
        }
        let ctx = Context { now: now_u, ts, local_net_ok: self.local_net_ok };
        let events = slot.tracker.apply(outcome, &ctx, &self.cfg.thresholds, &self.cfg.general);
        let interval = self.cfg.interval_for(&slot.server);
        let delay = slot.tracker.next_delay(&self.cfg.general, interval);
        slot.due = Instant::now() + Duration::from_secs(delay) + if delay >= interval { jitter(&slot.server.name, interval) } else { Duration::ZERO };
        if slot.tracker.ssh == SshState::Available {
            if let Some(m) = &slot.tracker.metrics {
                self.history.record_sample(&slot.server.name, m);
            }
        }
        self.emit(events);
    }

    fn emit(&mut self, events: Vec<Event>) {
        for e in events {
            self.history.record_event(&e);
            for s in &mut self.event_sinks {
                s(&e);
            }
            let muted = util::now_unix() < self.muted_until;
            if let Some(n) = &self.notifier {
                if !muted && self.limiter.allow(&e, self.cfg.general.notify_cooldown_secs) {
                    n.show(notify::toast_for(&e));
                }
            }
        }
    }

    fn save(&self) -> Result<(), String> {
        if self.persist_config {
            config::save(&self.paths.config, &self.cfg).map_err(|e| format!("{e:#}"))?;
        }
        Ok(())
    }

    fn set_enabled(&mut self, name: &str, on: bool) -> Result<(), String> {
        let Some(slot) = self.slots.iter_mut().find(|s| s.server.name == name) else {
            return Err(format!("unknown server {name:?}"));
        };
        slot.server.enabled = on;
        if let Some(s) = self.cfg.server_mut(name) {
            s.enabled = on;
        }
        if let Some(ev) = slot.tracker.set_enabled(on, util::now_unix()) {
            if on {
                slot.due = Instant::now();
            } else {
                self.limiter.forget(name);
            }
            self.history.record_event(&ev);
        }
        self.save()
    }

    fn add_server(&mut self, s: Server) -> Result<(), String> {
        self.cfg.add(s.clone()).map_err(|e| format!("{e:#}"))?;
        if let Err(e) = self.save() {
            self.cfg.remove(&s.name);
            return Err(e);
        }
        self.add_slot(s);
        Ok(())
    }

    fn remove_server(&mut self, name: &str) -> Result<(), String> {
        if !self.cfg.remove(name) {
            return Err(format!("unknown server {name:?}"));
        }
        self.slots.retain(|s| s.server.name != name);
        self.limiter.forget(name);
        if let Some(n) = &self.notifier {
            n.close_for(name);
        }
        self.save()
    }

    fn reload(&mut self) {
        let new = match config::load(&self.paths.config) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("svmonitor: config reload failed, keeping previous: {e:#}");
                return;
            }
        };
        if new == self.cfg {
            return;
        }
        self.cfg = new;
        let wanted = self.cfg.servers.clone();
        self.slots.retain(|s| wanted.iter().any(|w| w.name == s.server.name));
        for w in wanted {
            match self.slots.iter().position(|s| s.server.name == w.name) {
                Some(i) => {
                    let slot = &mut self.slots[i];
                    let changed = slot.server != w;
                    let was = slot.server.enabled;
                    slot.server = w.clone();
                    if changed {
                        slot.host = w.host.clone();
                        slot.tracker.target = w.destination();
                        slot.due = Instant::now();
                    }
                    if was != w.enabled {
                        if let Some(ev) = slot.tracker.set_enabled(w.enabled, util::now_unix()) {
                            self.history.record_event(&ev);
                            if w.enabled {
                                slot.due = Instant::now();
                            }
                        }
                    }
                }
                None => self.add_slot(w),
            }
        }
        // keep config order
        let order: Vec<&str> = self.cfg.servers.iter().map(|s| s.name.as_str()).collect();
        self.slots.sort_by_key(|s| order.iter().position(|n| *n == s.server.name).unwrap_or(usize::MAX));
        self.history = History::new(self.paths.events(), self.paths.samples(), self.cfg.general.keep_samples);
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            ts: util::now_unix(),
            pid: std::process::id(),
            local_net_ok: self.local_net_ok,
            muted: util::now_unix() < self.muted_until,
            servers: self.slots.iter().map(|s| s.tracker.view(&self.cfg.general, s.inflight)).collect(),
        }
    }

    fn publish(&mut self, force: bool) {
        let snap = self.snapshot();
        let changed = match &self.last_published {
            Some(p) => p.servers != snap.servers || p.muted != snap.muted || p.local_net_ok != snap.local_net_ok,
            None => true,
        };
        if !(changed || force) {
            return;
        }
        if let Ok(json) = serde_json::to_vec(&snap) {
            let _ = util::ensure_private_dir(&self.paths.runtime_dir);
            let _ = util::atomic_write(&self.paths.snapshot(), &json);
        }
        for s in &mut self.sinks {
            s(&snap);
        }
        self.last_published = Some(snap);
    }
}
