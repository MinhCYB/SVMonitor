//! Cached dashboard presentation. No network I/O and no GUI dependency.
use crate::model::{Overall, Snapshot, SshState};
use crate::util::{fmt_ago, fmt_duration, fmt_kib, fmt_rate};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Idle,
    Ok,
    Warn,
    Crit,
}
impl Level {
    pub fn class(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Crit => "crit",
        }
    }
}
pub fn level_for(pct: f32) -> Level {
    if !pct.is_finite() {
        Level::Idle
    } else if pct >= 90.0 {
        Level::Crit
    } else if pct >= 70.0 {
        Level::Warn
    } else {
        Level::Ok
    }
}
pub struct Bar {
    pub label: String,
    pub text: String,
    pub frac: f64,
    pub level: Level,
}
pub struct Card {
    pub name: String,
    pub target: String,
    pub enabled: bool,
    pub state: String,
    pub level: Level,
    pub subtitle: String,
    pub stale: bool,
    pub bars: Vec<Bar>,
    pub net: Option<String>,
    pub facts: Vec<(String, String)>,
    pub ssh: String,
    pub tailscale: String,
}
pub struct Header {
    pub title: String,
    pub level: Level,
    pub notes: Vec<String>,
}
fn status_level(o: Overall) -> Level {
    match o {
        Overall::Online => Level::Ok,
        Overall::Warning | Overall::LocalIssue => Level::Warn,
        Overall::Down | Overall::Attention => Level::Crit,
        Overall::Unknown | Overall::Disabled => Level::Idle,
    }
}
pub fn header(s: &Snapshot, running: bool) -> Header {
    let mut notes = Vec::new();
    if !running {
        notes.push("Monitor is not running — showing the last saved state".into());
    }
    if s.muted {
        notes.push("Notifications muted".into());
    }
    if !s.local_net_ok {
        notes.push("This machine has no network route; server alerts are paused".into());
    }
    Header {
        title: if s.servers.is_empty() {
            "No servers yet".into()
        } else {
            format!("{} of {} online", s.count_online(), s.count_enabled())
        },
        level: if running {
            status_level(s.worst())
        } else {
            Level::Idle
        },
        notes,
    }
}
pub fn cards(s: &Snapshot, now: i64) -> Vec<Card> {
    s.servers
        .iter()
        .map(|v| {
            let stale = v.ssh != SshState::Available;
            let mut c = Card {
                name: v.name.clone(),
                target: v.target.clone(),
                enabled: v.enabled,
                state: v.overall.label().into(),
                level: status_level(v.overall),
                stale,
                subtitle: if !v.enabled {
                    "monitoring off".into()
                } else if let Some(down) = v.down_since {
                    format!(
                        "down {} · {}",
                        fmt_duration((now - down).max(0) as u64),
                        v.detail
                    )
                } else if !v.detail.is_empty() {
                    v.detail.clone()
                } else if v.probing {
                    "checking…".into()
                } else if let Some(t) = v.last_check {
                    format!("checked {}", fmt_ago(t, now))
                } else {
                    "waiting for the first check".into()
                },
                bars: Vec::new(),
                net: None,
                facts: Vec::new(),
                ssh: match v.ssh {
                    SshState::Available => "available".into(),
                    SshState::Unknown => "unknown".into(),
                    SshState::Unavailable(k) => k.label().into(),
                },
                tailscale: v.tailscale.label().into(),
            };
            if let Some(m) = &v.metrics {
                let bar = |label: String, pct: Option<f32>, text: String| Bar {
                    label,
                    text,
                    frac: pct
                        .filter(|p| p.is_finite())
                        .unwrap_or(0.0)
                        .clamp(0.0, 100.0) as f64
                        / 100.0,
                    level: pct.map(level_for).unwrap_or(Level::Idle),
                };
                c.bars.push(bar(
                    "CPU".into(),
                    m.cpu_pct,
                    m.cpu_pct
                        .map(|p| format!("{p:.0}%"))
                        .unwrap_or_else(|| "collecting…".into()),
                ));
                c.bars.push(bar(
                    "RAM".into(),
                    Some(m.mem_pct()),
                    format!(
                        "{:.0}% · {} / {}",
                        m.mem_pct(),
                        fmt_kib(m.mem_used_kib),
                        fmt_kib(m.mem_total_kib)
                    ),
                ));
                if let Some(d) = m.worst_disk() {
                    c.bars.push(bar(
                        format!("Disk {}", d.mount),
                        Some(d.pct()),
                        format!(
                            "{:.0}% · {} / {}",
                            d.pct(),
                            fmt_kib(d.used_kib),
                            fmt_kib(d.total_kib)
                        ),
                    ));
                }
                if !stale {
                    c.net = Some(
                        m.net
                            .as_ref()
                            .map(|n| {
                                format!(
                                    "{}  ↓ {}  ↑ {} · {}",
                                    n.iface,
                                    fmt_rate(n.rx_bps),
                                    fmt_rate(n.tx_bps),
                                    if n.sampled {
                                        "1s sample".into()
                                    } else {
                                        format!(
                                            "avg {}",
                                            fmt_duration(n.window_secs.round() as u64)
                                        )
                                    }
                                )
                            })
                            .unwrap_or_else(|| "network: collecting…".into()),
                    );
                }
                let mut fact = |k: &str, val: String| c.facts.push((k.into(), val));
                fact(
                    "Load",
                    format!(
                        "{:.2} {:.2} {:.2}{}",
                        m.load[0],
                        m.load[1],
                        m.load[2],
                        m.cores.map(|n| format!(" ({n} cores)")).unwrap_or_default()
                    ),
                );
                fact(
                    "Swap",
                    m.swap_pct()
                        .map(|p| {
                            format!(
                                "{p:.0}% · {} / {}",
                                fmt_kib(m.swap_used_kib),
                                fmt_kib(m.swap_total_kib)
                            )
                        })
                        .unwrap_or_else(|| "none".into()),
                );
                for d in &m.disks {
                    if m.worst_disk().is_some_and(|w| w.mount == d.mount) {
                        continue;
                    }
                    fact(
                        &format!("Disk {}", d.mount),
                        format!(
                            "{:.0}% · {} / {}",
                            d.pct(),
                            fmt_kib(d.used_kib),
                            fmt_kib(d.total_kib)
                        ),
                    );
                }
                fact("Uptime", fmt_duration(m.uptime_secs));
                if let Some(n) = m.procs {
                    fact("Processes", n.to_string());
                }
                for (k, val) in [
                    ("Hostname", &m.hostname),
                    ("OS", &m.os),
                    ("Kernel", &m.kernel),
                ] {
                    if let Some(val) = val {
                        fact(k, val.clone());
                    }
                }
            }
            if let Some(t) = v.last_check {
                c.facts.push(("Last check".into(), fmt_ago(t, now)));
            }
            if !v.active_alerts.is_empty() {
                c.facts.push(("Alerts".into(), v.active_alerts.join(", ")));
            }
            c
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

    fn metrics() -> Metrics {
        Metrics {
            ts: 100,
            cpu_pct: Some(42.0),
            cores: Some(4),
            load: [0.5, 0.4, 0.3],
            mem_total_kib: 8 * 1024 * 1024,
            mem_used_kib: 2 * 1024 * 1024,
            swap_total_kib: 0,
            swap_used_kib: 0,
            disks: vec![
                DiskUsage {
                    mount: "/".into(),
                    total_kib: 1000,
                    used_kib: 950,
                },
                DiskUsage {
                    mount: "/data".into(),
                    total_kib: 1000,
                    used_kib: 100,
                },
            ],
            uptime_secs: 90_000,
            procs: Some(120),
            os: Some("Debian 13".into()),
            kernel: Some("6.12".into()),
            hostname: Some("vps".into()),
            net: Some(NetRate {
                iface: "eth0".into(),
                rx_bps: 2048,
                tx_bps: 512,
                window_secs: 60.0,
                sampled: false,
            }),
        }
    }

    fn view(name: &str, overall: Overall, ssh: SshState) -> ServerView {
        ServerView {
            name: name.into(),
            target: format!("{name}.example"),
            enabled: overall != Overall::Disabled,
            overall,
            ssh,
            tailscale: TsState::NotApplicable,
            metrics: Some(metrics()),
            last_check: Some(95),
            completed_checks: 3,
            last_ok: Some(95),
            down_since: None,
            detail: String::new(),
            pending_failures: 0,
            failure_threshold: 3,
            active_alerts: vec![],
            probing: false,
        }
    }

    fn snap(servers: Vec<ServerView>) -> Snapshot {
        Snapshot {
            ts: 100,
            pid: 1,
            local_net_ok: true,
            muted: false,
            servers,
            command_capable: true,
        }
    }

    #[test]
    fn bar_levels_follow_fixed_visual_thresholds() {
        assert_eq!(level_for(0.0), Level::Ok);
        assert_eq!(level_for(69.9), Level::Ok);
        assert_eq!(level_for(70.0), Level::Warn);
        assert_eq!(level_for(89.9), Level::Warn);
        assert_eq!(level_for(90.0), Level::Crit);
        assert_eq!(level_for(f32::NAN), Level::Idle);
    }

    #[test]
    fn online_card_shows_cpu_ram_and_the_worst_disk() {
        let c = &cards(
            &snap(vec![view("a", Overall::Online, SshState::Available)]),
            100,
        )[0];
        let b: Vec<_> = c.bars.iter().map(|b| (b.label.as_str(), b.level)).collect();
        assert_eq!(
            b,
            [
                ("CPU", Level::Ok),
                ("RAM", Level::Ok),
                ("Disk /", Level::Crit)
            ]
        );
        assert!((c.bars[0].frac - 0.42).abs() < 1e-3);
        assert!(!c.stale);
        assert_eq!(c.level, Level::Ok);
    }

    #[test]
    fn unreachable_card_keeps_last_values_but_is_marked_stale() {
        let mut v = view("a", Overall::Down, SshState::Unavailable(FailKind::Timeout));
        v.down_since = Some(40);
        v.detail = "connection timed out".into();
        let c = &cards(&snap(vec![v]), 100)[0];
        assert!(c.stale, "numbers shown for an unreachable server are old");
        assert_eq!(c.level, Level::Crit);
        assert!(
            c.net.is_none(),
            "an old throughput is never shown as current"
        );
    }

    #[test]
    fn local_network_problem_is_not_painted_as_a_server_failure() {
        let c = &cards(
            &snap(vec![view("a", Overall::LocalIssue, SshState::Unknown)]),
            100,
        )[0];
        assert_eq!(c.level, Level::Warn);
        assert!(c.stale);
    }

    #[test]
    fn disabled_and_first_poll_cards_have_no_bars() {
        let mut off = view("off", Overall::Disabled, SshState::Unknown);
        off.metrics = None;
        let mut first = view("new", Overall::Unknown, SshState::Unknown);
        first.metrics = None;
        let cs = cards(&snap(vec![off, first]), 100);
        assert!(cs[0].bars.is_empty() && cs[1].bars.is_empty());
        assert_eq!(cs[0].level, Level::Idle);
        assert!(!cs[0].enabled && cs[1].enabled);
    }

    #[test]
    fn header_summarises_counts_and_why_alerts_may_be_quiet() {
        let s = snap(vec![
            view("a", Overall::Online, SshState::Available),
            view("b", Overall::Down, SshState::Unavailable(FailKind::Timeout)),
            view("c", Overall::Disabled, SshState::Unknown),
        ]);
        let h = header(&s, true);
        assert_eq!(h.level, Level::Crit);
        assert!(h.notes.is_empty());

        let mut s2 = s.clone();
        s2.muted = true;
        s2.local_net_ok = false;
        let h2 = header(&s2, true);
        assert_eq!(h2.notes.len(), 2);
    }

    #[test]
    fn header_says_so_when_the_monitor_is_not_running() {
        let s = snap(vec![view("a", Overall::Online, SshState::Available)]);
        let h = header(&s, false);
        assert_eq!(h.level, Level::Idle);
        assert!(!h.notes.is_empty());
    }
}
