//! Pure per-server state machine: turns probe outcomes into state, metrics and events.
//! No I/O, no clocks (time is passed in) so every transition is unit-testable.
use crate::config::{General, Thresholds};
use crate::metrics::{self, RawSample};
use crate::model::*;
use std::collections::BTreeMap;

#[derive(Debug)]
pub enum Outcome {
    Ok(RawSample),
    Fail { kind: FailKind, detail: String },
}

#[derive(Debug, Clone, Copy)]
pub struct Context {
    pub now: i64,
    pub ts: TsState,
    pub local_net_ok: bool,
}

#[derive(Debug, Clone)]
pub struct Tracker {
    pub name: String,
    pub target: String,
    pub enabled: bool,
    pub overall: Overall,
    pub ssh: SshState,
    pub tailscale: TsState,
    pub metrics: Option<Metrics>,
    pub last_check: Option<i64>,
    pub last_ok: Option<i64>,
    pub down_since: Option<i64>,
    pub detail: String,
    pub fails: u32,
    prev_cpu: Option<[u64; 8]>,
    last_uptime: Option<u64>,
    breach_count: BTreeMap<&'static str, u32>,
    breach_active: BTreeMap<&'static str, f32>,
}

impl Tracker {
    pub fn new(name: &str, target: &str, enabled: bool) -> Tracker {
        Tracker {
            name: name.into(),
            target: target.into(),
            enabled,
            overall: if enabled { Overall::Unknown } else { Overall::Disabled },
            ssh: SshState::Unknown,
            tailscale: TsState::NotApplicable,
            metrics: None,
            last_check: None,
            last_ok: None,
            down_since: None,
            detail: String::new(),
            fails: 0,
            prev_cpu: None,
            last_uptime: None,
            breach_count: BTreeMap::new(),
            breach_active: BTreeMap::new(),
        }
    }

    pub fn set_enabled(&mut self, enabled: bool, now: i64) -> Option<Event> {
        if self.enabled == enabled {
            return None;
        }
        self.enabled = enabled;
        let (kind, msg) = if enabled { (EventKind::Enabled, "monitoring enabled") } else { (EventKind::Disabled, "monitoring disabled") };
        if enabled {
            self.overall = Overall::Unknown;
        } else {
            self.overall = Overall::Disabled;
            self.fails = 0;
            self.down_since = None;
            self.breach_count.clear();
            self.breach_active.clear();
            self.prev_cpu = None;
            self.detail.clear();
        }
        Some(Event { ts: now, server: self.name.clone(), kind, message: msg.into(), key: String::new(), notify: false })
    }

    /// Discard pending failure counts (e.g. after resume from suspend or a local network flap).
    pub fn forgive(&mut self) {
        if self.overall != Overall::Down && self.overall != Overall::Attention {
            self.fails = 0;
            if self.overall == Overall::LocalIssue {
                self.overall = if self.metrics.is_some() { Overall::Online } else { Overall::Unknown };
            }
        }
    }

    /// No CPU counters yet: the next probe should ask for a one-second sample.
    pub fn needs_cpu_baseline(&self) -> bool {
        self.prev_cpu.is_none()
    }

    pub fn active_alerts(&self) -> Vec<String> {
        self.breach_active.keys().map(|k| (*k).to_string()).collect()
    }

    pub fn view(&self, g: &General, probing: bool) -> ServerView {
        ServerView {
            name: self.name.clone(),
            target: self.target.clone(),
            enabled: self.enabled,
            overall: self.overall,
            ssh: self.ssh,
            tailscale: self.tailscale,
            metrics: self.metrics.clone(),
            last_check: self.last_check,
            last_ok: self.last_ok,
            down_since: self.down_since,
            detail: self.detail.clone(),
            pending_failures: if matches!(self.overall, Overall::Down | Overall::Attention) { 0 } else { self.fails },
            failure_threshold: g.failure_threshold,
            active_alerts: self.active_alerts(),
            probing,
        }
    }

    /// Seconds until this server should be probed again.
    pub fn next_delay(&self, g: &General, interval: u64) -> u64 {
        match self.overall {
            Overall::Disabled => u64::MAX,
            Overall::Unknown if self.fails == 0 => 0,
            Overall::Down | Overall::Attention => {
                let over = self.fails.saturating_sub(g.failure_threshold).min(10);
                (g.retry_interval_secs.saturating_mul(2u64.saturating_pow(over + 1))).min(g.down_interval_max_secs).max(g.retry_interval_secs)
            }
            _ if self.fails > 0 => g.retry_interval_secs,
            Overall::LocalIssue => g.retry_interval_secs,
            _ => interval,
        }
    }

    pub fn apply(&mut self, outcome: Outcome, ctx: &Context, th: &Thresholds, g: &General) -> Vec<Event> {
        if !self.enabled {
            return vec![];
        }
        self.last_check = Some(ctx.now);
        self.tailscale = ctx.ts;
        match outcome {
            Outcome::Ok(sample) => self.on_success(sample, ctx, th),
            Outcome::Fail { kind, detail } => self.on_failure(kind, detail, ctx, g),
        }
    }

    fn ev(&self, ctx: &Context, kind: EventKind, key: &str, notify: bool, message: String) -> Event {
        Event { ts: ctx.now, server: self.name.clone(), kind, message, key: key.into(), notify }
    }

    fn on_failure(&mut self, kind: FailKind, detail: String, ctx: &Context, g: &General) -> Vec<Event> {
        let mut events = Vec::new();
        self.ssh = SshState::Unavailable(kind);
        // Local problems must never be blamed on the server.
        let local = ctx.ts.is_local_problem() || (!ctx.local_net_ok && (kind.is_connectivity() || kind == FailKind::Dns));
        if local {
            self.overall = Overall::LocalIssue;
            self.detail = if ctx.ts.is_local_problem() {
                format!("local Tailscale is {}", ctx.ts.label())
            } else {
                "no default network route on this machine".into()
            };
            return events;
        }
        self.fails = self.fails.saturating_add(1);
        let mut d = if detail.is_empty() { kind.label().to_string() } else { detail };
        if ctx.ts == TsState::PeerOffline {
            d = format!("Tailscale peer offline; {d}");
        }
        self.detail = d;
        if self.fails < g.failure_threshold {
            if self.overall == Overall::LocalIssue {
                self.overall = if self.metrics.is_some() { Overall::Online } else { Overall::Unknown };
            }
            return events; // debounce: keep previous overall state
        }
        let target = if kind.is_connectivity() { Overall::Down } else { Overall::Attention };
        if self.overall != target {
            // Leaving the opposite fault state: close it out first.
            if self.overall == Overall::Down && target == Overall::Attention {
                events.push(self.ev(ctx, EventKind::Recovered, "down", false, "reachable again, but a different problem remains".into()));
            }
            self.overall = target;
            self.down_since = Some(ctx.now);
            // Alert metric breaches no longer describe a reachable server.
            self.breach_active.clear();
            self.breach_count.clear();
            if target == Overall::Down {
                let ts_note = if ctx.ts == TsState::PeerOffline { " (Tailscale peer is offline)" } else { "" };
                events.push(self.ev(ctx, EventKind::Down, "down", true, format!("{}{ts_note}", self.detail)));
            } else {
                let hint = kind.hint().map(|h| format!(". {h}")).unwrap_or_default();
                events.push(self.ev(ctx, EventKind::Attention, "attention", true, format!("{}{hint}", self.detail)));
            }
        }
        events
    }

    fn on_success(&mut self, sample: RawSample, ctx: &Context, th: &Thresholds) -> Vec<Event> {
        let mut events = Vec::new();
        let prev_overall = self.overall;
        let down_since = self.down_since;
        self.fails = 0;
        self.ssh = SshState::Available;
        self.detail.clear();
        self.last_ok = Some(ctx.now);

        let mut m = sample.metrics;
        if m.cpu_pct.is_none() {
            if let (Some(prev), Some(cur)) = (self.prev_cpu, sample.cpu) {
                m.cpu_pct = metrics::cpu_pct(&prev, &cur);
            }
        }
        self.prev_cpu = sample.cpu;

        match prev_overall {
            Overall::Down => {
                let dur = down_since.map(|d| crate::util::fmt_duration((ctx.now - d).max(0) as u64)).unwrap_or_default();
                events.push(self.ev(ctx, EventKind::Recovered, "down", true, format!("back online after {dur}")));
            }
            Overall::Attention => {
                events.push(self.ev(ctx, EventKind::AttentionCleared, "attention", true, "connection works again".into()));
            }
            _ => {}
        }
        self.down_since = None;

        if let Some(prev_up) = self.last_uptime {
            if m.uptime_secs + 30 < prev_up {
                events.push(self.ev(ctx, EventKind::Rebooted, "reboot", true, "server rebooted".into()));
            }
        }
        self.last_uptime = Some(m.uptime_secs);

        // Thresholds with persistence and hysteresis.
        let mut obs: Vec<(&'static str, f32, f32, String)> = Vec::new();
        if let Some(c) = m.cpu_pct {
            obs.push(("cpu", c, th.cpu_pct, format!("CPU {c:.0}%")));
        }
        obs.push(("mem", m.mem_pct(), th.mem_pct, format!("RAM {:.0}%", m.mem_pct())));
        if let Some(s) = m.swap_pct() {
            obs.push(("swap", s, th.swap_pct, format!("swap {s:.0}%")));
        }
        if let Some(d) = m.worst_disk() {
            obs.push(("disk", d.pct(), th.disk_pct, format!("disk {} {:.0}%", d.mount, d.pct())));
        }
        let lpc = m.load_per_core();
        obs.push(("load", lpc, th.load_per_core, format!("load {:.2} ({:.1}/core)", m.load[0], lpc)));
        for (key, value, limit, text) in obs {
            let clear_level = if key == "load" { limit * 0.8 } else { (limit - 5.0).max(limit * 0.9) };
            if value >= limit {
                let c = self.breach_count.entry(key).or_insert(0);
                *c += 1;
                if *c >= th.samples && !self.breach_active.contains_key(key) {
                    self.breach_active.insert(key, value);
                    events.push(self.ev(ctx, EventKind::ThresholdBreach, &format!("breach:{key}"), true, format!("{text} (limit {limit:.0})")));
                }
            } else {
                if value < clear_level {
                    self.breach_count.remove(key);
                    if self.breach_active.remove(key).is_some() {
                        events.push(self.ev(ctx, EventKind::ThresholdCleared, &format!("breach:{key}"), false, format!("{text} back to normal")));
                    }
                } else if !self.breach_active.contains_key(key) {
                    // In the hysteresis band but not alerting: do not accumulate toward an alert.
                    self.breach_count.remove(key);
                }
            }
        }
        self.metrics = Some(m);
        self.overall = if self.breach_active.is_empty() { Overall::Online } else { Overall::Warning };
        if self.overall == Overall::Warning {
            self.detail = self.breach_active.keys().cloned().collect::<Vec<_>>().join(", ");
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g() -> General {
        General { failure_threshold: 3, retry_interval_secs: 10, down_interval_max_secs: 300, interval_secs: 60, ..General::default() }
    }
    fn ctx(now: i64) -> Context {
        Context { now, ts: TsState::NotApplicable, local_net_ok: true }
    }
    fn sample(cpu: [u64; 8], mem_used_pct: u64, uptime: u64) -> RawSample {
        RawSample {
            ts: 0,
            cpu: Some(cpu),
            metrics: Metrics {
                ts: 0,
                cpu_pct: None,
                cores: Some(4),
                load: [0.5, 0.5, 0.5],
                mem_total_kib: 1000,
                mem_used_kib: mem_used_pct * 10,
                swap_total_kib: 0,
                swap_used_kib: 0,
                disks: vec![DiskUsage { mount: "/".into(), total_kib: 100, used_kib: 10 }],
                uptime_secs: uptime,
                procs: None,
                os: None,
                kernel: None,
                hostname: None,
            },
        }
    }
    fn ok(mem: u64, up: u64) -> Outcome {
        Outcome::Ok(sample([100, 0, 0, 900, 0, 0, 0, 0], mem, up))
    }
    fn fail(k: FailKind) -> Outcome {
        Outcome::Fail { kind: k, detail: String::new() }
    }
    fn tr() -> Tracker {
        Tracker::new("s", "s", true)
    }
    fn kinds(e: &[Event]) -> Vec<EventKind> {
        e.iter().map(|e| e.kind).collect()
    }

    #[test]
    fn first_success_is_online() {
        let mut t = tr();
        assert!(t.apply(ok(10, 100), &ctx(1), &Thresholds::default(), &g()).is_empty());
        assert_eq!(t.overall, Overall::Online);
        assert_eq!(t.ssh, SshState::Available);
    }

    #[test]
    fn debounce_then_down_then_recovered() {
        let (th, g) = (Thresholds::default(), g());
        let mut t = tr();
        t.apply(ok(10, 100), &ctx(1), &th, &g);
        assert!(t.apply(fail(FailKind::Timeout), &ctx(2), &th, &g).is_empty());
        assert!(t.apply(fail(FailKind::Timeout), &ctx(3), &th, &g).is_empty());
        assert_eq!(t.overall, Overall::Online, "not down before threshold");
        let e = t.apply(fail(FailKind::Timeout), &ctx(4), &th, &g);
        assert_eq!(kinds(&e), vec![EventKind::Down]);
        assert!(e[0].notify);
        assert_eq!(t.overall, Overall::Down);
        // staying down emits nothing more
        assert!(t.apply(fail(FailKind::Timeout), &ctx(5), &th, &g).is_empty());
        let e = t.apply(ok(10, 200), &ctx(100), &th, &g);
        assert_eq!(kinds(&e), vec![EventKind::Recovered]);
        assert!(e[0].message.contains("1m"), "{}", e[0].message);
        assert_eq!(t.overall, Overall::Online);
    }

    #[test]
    fn single_blip_recovers_silently() {
        let (th, g) = (Thresholds::default(), g());
        let mut t = tr();
        t.apply(ok(10, 100), &ctx(1), &th, &g);
        t.apply(fail(FailKind::Timeout), &ctx(2), &th, &g);
        assert!(t.apply(ok(10, 130), &ctx(3), &th, &g).is_empty());
        assert_eq!(t.fails, 0);
    }

    #[test]
    fn auth_failure_is_attention_not_down() {
        let (th, g) = (Thresholds::default(), g());
        let mut t = tr();
        let mut ev = vec![];
        for i in 0..3 {
            ev.extend(t.apply(fail(FailKind::Auth), &ctx(i), &th, &g));
        }
        assert_eq!(kinds(&ev), vec![EventKind::Attention]);
        assert_eq!(t.overall, Overall::Attention);
        let e = t.apply(ok(10, 5), &ctx(10), &th, &g);
        assert_eq!(kinds(&e), vec![EventKind::AttentionCleared]);
    }

    #[test]
    fn local_network_down_never_blames_server() {
        let (th, g) = (Thresholds::default(), g());
        let mut t = tr();
        t.apply(ok(10, 100), &ctx(1), &th, &g);
        let c = Context { now: 2, ts: TsState::NotApplicable, local_net_ok: false };
        for _ in 0..10 {
            assert!(t.apply(fail(FailKind::NoRoute), &c, &th, &g).is_empty());
        }
        assert_eq!(t.overall, Overall::LocalIssue);
        assert_eq!(t.fails, 0);
        // network returns, server fine: back to online, no events
        assert!(t.apply(ok(10, 160), &ctx(3), &th, &g).is_empty());
        assert_eq!(t.overall, Overall::Online);
    }

    #[test]
    fn local_tailscale_down_is_local_issue() {
        let (th, g) = (Thresholds::default(), g());
        let mut t = tr();
        let c = Context { now: 2, ts: TsState::DaemonDown, local_net_ok: true };
        for _ in 0..5 {
            assert!(t.apply(fail(FailKind::Timeout), &c, &th, &g).is_empty());
        }
        assert_eq!(t.overall, Overall::LocalIssue);
        assert!(t.detail.contains("Tailscale"));
    }

    #[test]
    fn peer_offline_is_still_down_but_explained() {
        let (th, g) = (Thresholds::default(), g());
        let mut t = tr();
        let c = |n| Context { now: n, ts: TsState::PeerOffline, local_net_ok: true };
        let mut ev = vec![];
        for i in 0..3 {
            ev.extend(t.apply(fail(FailKind::Timeout), &c(i), &th, &g));
        }
        assert_eq!(kinds(&ev), vec![EventKind::Down]);
        assert!(ev[0].message.contains("Tailscale peer is offline"));
        assert_eq!(t.tailscale, TsState::PeerOffline);
        assert_eq!(t.ssh, SshState::Unavailable(FailKind::Timeout));
    }

    #[test]
    fn dns_failure_with_working_network_is_attention() {
        let (th, g) = (Thresholds::default(), g());
        let mut t = tr();
        for i in 0..3 {
            t.apply(fail(FailKind::Dns), &ctx(i), &th, &g);
        }
        assert_eq!(t.overall, Overall::Attention);
    }

    #[test]
    fn cpu_threshold_needs_persistence_and_hysteresis() {
        let th = Thresholds { cpu_pct: 90.0, samples: 2, ..Thresholds::default() };
        let g = g();
        let mut t = tr();
        let busy = |n: u64| Outcome::Ok(sample([n * 95, 0, 0, n * 5, 0, 0, 0, 0], 10, 100 + n));
        let idle = |base: u64, n: u64| Outcome::Ok(sample([base + n * 10, 0, 0, base + n * 90, 0, 0, 0, 0], 10, 100));
        // first sample has no cpu delta
        assert!(t.apply(busy(1), &ctx(1), &th, &g).is_empty());
        assert!(t.apply(busy(2), &ctx(2), &th, &g).is_empty(), "one sample is not enough");
        let e = t.apply(busy(3), &ctx(3), &th, &g);
        assert_eq!(kinds(&e), vec![EventKind::ThresholdBreach]);
        assert_eq!(t.overall, Overall::Warning);
        assert!(t.apply(busy(4), &ctx(4), &th, &g).is_empty(), "no repeat while active");
        // 88% sits in the hysteresis band (limit-5 = 85): stays active
        let band = Outcome::Ok(sample([4 * 95 + 88, 0, 0, 4 * 5 + 12, 0, 0, 0, 0], 10, 110));
        assert!(t.apply(band, &ctx(5), &th, &g).is_empty());
        assert_eq!(t.overall, Overall::Warning);
        let base = 4 * 100 + 100;
        let e = t.apply(idle(base, 1), &ctx(6), &th, &g);
        assert_eq!(kinds(&e), vec![EventKind::ThresholdCleared]);
        assert!(!e[0].notify);
        assert_eq!(t.overall, Overall::Online);
    }

    #[test]
    fn memory_breach_reported_once() {
        let th = Thresholds { mem_pct: 90.0, samples: 1, ..Thresholds::default() };
        let g = g();
        let mut t = tr();
        let e = t.apply(ok(95, 1), &ctx(1), &th, &g);
        assert_eq!(kinds(&e), vec![EventKind::ThresholdBreach]);
        assert_eq!(e[0].key, "breach:mem");
        assert!(t.apply(ok(96, 2), &ctx(2), &th, &g).is_empty());
        assert_eq!(t.active_alerts(), vec!["mem"]);
    }

    #[test]
    fn reboot_detected_by_uptime_drop() {
        let (th, g) = (Thresholds::default(), g());
        let mut t = tr();
        t.apply(ok(10, 5000), &ctx(1), &th, &g);
        let e = t.apply(ok(10, 20), &ctx(2), &th, &g);
        assert_eq!(kinds(&e), vec![EventKind::Rebooted]);
        // small jitter is not a reboot
        assert!(t.apply(ok(10, 15), &ctx(3), &th, &g).is_empty());
    }

    #[test]
    fn disabling_resets_and_blocks_updates() {
        let (th, g) = (Thresholds::default(), g());
        let mut t = tr();
        for i in 0..3 {
            t.apply(fail(FailKind::Timeout), &ctx(i), &th, &g);
        }
        assert_eq!(t.overall, Overall::Down);
        let e = t.set_enabled(false, 10).unwrap();
        assert_eq!(e.kind, EventKind::Disabled);
        assert_eq!(t.overall, Overall::Disabled);
        assert!(t.apply(ok(10, 1), &ctx(11), &th, &g).is_empty());
        assert_eq!(t.overall, Overall::Disabled);
        assert_eq!(t.next_delay(&g, 60), u64::MAX);
        assert!(t.set_enabled(false, 12).is_none());
        t.set_enabled(true, 13);
        assert_eq!(t.overall, Overall::Unknown);
        assert_eq!(t.next_delay(&g, 60), 0);
    }

    #[test]
    fn scheduling_backoff() {
        let (th, g) = (Thresholds::default(), g());
        let mut t = tr();
        assert_eq!(t.next_delay(&g, 60), 0);
        t.apply(ok(10, 1), &ctx(1), &th, &g);
        assert_eq!(t.next_delay(&g, 60), 60);
        t.apply(fail(FailKind::Timeout), &ctx(2), &th, &g);
        assert_eq!(t.next_delay(&g, 60), 10, "fast retry while pending");
        t.apply(fail(FailKind::Timeout), &ctx(3), &th, &g);
        t.apply(fail(FailKind::Timeout), &ctx(4), &th, &g);
        assert_eq!(t.overall, Overall::Down);
        let d1 = t.next_delay(&g, 60);
        t.apply(fail(FailKind::Timeout), &ctx(5), &th, &g);
        let d2 = t.next_delay(&g, 60);
        assert!(d2 > d1);
        for i in 0..30 {
            t.apply(fail(FailKind::Timeout), &ctx(10 + i), &th, &g);
        }
        assert_eq!(t.next_delay(&g, 60), 300, "capped");
    }

    #[test]
    fn forgive_clears_pending_only() {
        let (th, g) = (Thresholds::default(), g());
        let mut t = tr();
        t.apply(ok(10, 1), &ctx(1), &th, &g);
        t.apply(fail(FailKind::Timeout), &ctx(2), &th, &g);
        t.forgive();
        assert_eq!(t.fails, 0);
        for i in 0..3 {
            t.apply(fail(FailKind::Timeout), &ctx(3 + i), &th, &g);
        }
        t.forgive();
        assert_eq!(t.overall, Overall::Down, "a declared outage is not forgiven");
    }
}
