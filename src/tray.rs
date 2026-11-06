//! StatusNotifierItem tray (works on KDE, and on GNOME with the AppIndicator extension, Cinnamon,
//! XFCE, Budgie, waybar, noctalia, ...). Drawn icons avoid any dependency on the icon theme.
use crate::config::{self, Config, Paths};
use crate::engine::Handle;
use crate::model::*;
use crate::util::{fmt_ago, fmt_duration, fmt_kib};
use crate::{autostart, history, launcher, sshconfig};
use ksni::menu::{CheckmarkItem, MenuItem, StandardItem, SubMenu};
use ksni::{Category, Icon, Status, ToolTip};

/// Is a StatusNotifierWatcher (tray host) currently on the session bus?
pub fn watcher_present() -> bool {
    zbus::blocking::Connection::session()
        .ok()
        .and_then(|c| {
            let r = c
                .call_method(
                    Some("org.freedesktop.DBus"),
                    "/org/freedesktop/DBus",
                    Some("org.freedesktop.DBus"),
                    "NameHasOwner",
                    &("org.kde.StatusNotifierWatcher",),
                )
                .ok()?;
            r.body().deserialize::<bool>().ok()
        })
        .unwrap_or(false)
}

pub struct SvTray {
    pub engine: Handle,
    pub paths: PathsOwned,
    pub snap: Snapshot,
    cfg: Config,
    discovered: Vec<String>,
    recent: Vec<Event>,
    autostart_on: bool,
    exe: String,
}

#[derive(Clone)]
pub struct PathsOwned {
    pub config: std::path::PathBuf,
    pub events: std::path::PathBuf,
}

impl PathsOwned {
    pub fn from(p: &Paths) -> PathsOwned {
        PathsOwned {
            config: p.config.clone(),
            events: p.events(),
        }
    }
}

impl SvTray {
    pub fn new(engine: Handle, paths: &Paths, cfg: Config) -> SvTray {
        SvTray {
            engine,
            paths: PathsOwned::from(paths),
            snap: Snapshot {
                ts: 0,
                pid: std::process::id(),
                local_net_ok: true,
                muted: false,
                servers: vec![],
            },
            cfg,
            discovered: vec![],
            recent: vec![],
            autostart_on: false,
            exe: std::env::current_exe()
                .ok()
                .and_then(|p| p.to_str().map(String::from))
                .unwrap_or_else(|| "svmonitor".into()),
        }
    }

    fn refresh_menu_data(&mut self) {
        if let Ok(c) = config::load(&self.paths.config) {
            self.cfg = c;
        }
        self.discovered = sshconfig::default_config_path()
            .map(|p| sshconfig::discover(&p))
            .unwrap_or_default()
            .into_iter()
            .filter(|h| self.cfg.server(h).is_none())
            .collect();
        self.recent = history::read_events(&self.paths.events, None, 400);
        self.autostart_on = autostart::is_enabled(&autostart::autostart_path());
    }
}

pub fn marker(o: Overall) -> &'static str {
    match o {
        Overall::Online => "●",
        Overall::Warning => "▲",
        Overall::Down => "✖",
        Overall::Attention => "!",
        Overall::LocalIssue => "◐",
        Overall::Unknown => "…",
        Overall::Disabled => "○",
    }
}

pub fn color(o: Overall) -> (u8, u8, u8) {
    match o {
        Overall::Online => (0x2e, 0xcc, 0x71),
        Overall::Warning => (0xf1, 0xc4, 0x0f),
        Overall::Attention | Overall::LocalIssue => (0xe6, 0x7e, 0x22),
        Overall::Down => (0xe7, 0x4c, 0x3c),
        Overall::Unknown | Overall::Disabled => (0x95, 0xa5, 0xa6),
    }
}

/// ARGB32 (network byte order) anti-aliased disc with a darker ring.
pub fn dot_icon(size: i32, rgb: (u8, u8, u8)) -> Icon {
    let s = size as f32;
    let c = (s - 1.0) / 2.0;
    let r_out = s / 2.0 - 0.5;
    let r_in = r_out - (s / 8.0).max(1.5);
    let mut data = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
            let cover = (r_out + 0.5 - d).clamp(0.0, 1.0);
            let ring = (d - (r_in - 0.5)).clamp(0.0, 1.0);
            let k = 1.0 - 0.35 * ring;
            let a = (cover * 255.0) as u8;
            data.extend_from_slice(&[
                a,
                (rgb.0 as f32 * k) as u8,
                (rgb.1 as f32 * k) as u8,
                (rgb.2 as f32 * k) as u8,
            ]);
        }
    }
    Icon {
        width: size,
        height: size,
        data,
    }
}

pub fn tooltip_text(s: &Snapshot) -> String {
    if s.servers.is_empty() {
        return "No servers configured".into();
    }
    s.servers
        .iter()
        .map(|v| {
            let extra = match (&v.metrics, v.overall) {
                (Some(m), Overall::Online | Overall::Warning) => format!(
                    " — CPU {} · RAM {:.0}%",
                    m.cpu_pct
                        .map(|c| format!("{c:.0}%"))
                        .unwrap_or_else(|| "–".into()),
                    m.mem_pct()
                ),
                _ if !v.detail.is_empty() => format!(" — {}", v.detail),
                _ => String::new(),
            };
            format!(
                "{} {}: {}{}",
                marker(v.overall),
                v.name,
                v.overall.label(),
                extra
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn info(label: String) -> MenuItem<SvTray> {
    StandardItem {
        label,
        enabled: false,
        ..Default::default()
    }
    .into()
}

fn action(label: &str, f: impl Fn(&mut SvTray) + Send + 'static) -> MenuItem<SvTray> {
    StandardItem {
        label: label.into(),
        activate: Box::new(f),
        ..Default::default()
    }
    .into()
}

pub fn detail_lines(v: &ServerView, now: i64) -> Vec<String> {
    let mut out = Vec::new();
    let mut status = format!("Status: {}", v.overall.label());
    if v.pending_failures > 0 {
        status += &format!(
            " ({}/{} failed checks)",
            v.pending_failures, v.failure_threshold
        );
    }
    if let (Overall::Down, Some(d)) = (v.overall, v.down_since) {
        status += &format!(" for {}", fmt_duration((now - d).max(0) as u64));
    }
    out.push(status);
    if !v.detail.is_empty() && v.overall != Overall::Online {
        out.push(format!("Detail: {}", v.detail));
    }
    out.push(format!(
        "SSH: {}",
        match v.ssh {
            SshState::Unknown => "unknown".to_string(),
            SshState::Available => "available".to_string(),
            SshState::Unavailable(k) => format!("unavailable ({})", k.label()),
        }
    ));
    out.push(format!("Tailscale: {}", v.tailscale.label()));
    if let Some(m) = &v.metrics
        && (v.ssh == SshState::Available || v.overall == Overall::Down)
    {
        let stale = if v.ssh == SshState::Available {
            ""
        } else {
            " (last known)"
        };
        if let Some(c) = m.cpu_pct {
            out.push(format!(
                "CPU: {c:.0}%{stale}  ({} cores)",
                m.cores.unwrap_or(0)
            ));
        } else {
            out.push(format!("CPU: –  ({} cores){stale}", m.cores.unwrap_or(0)));
        }
        out.push(format!(
            "RAM: {:.0}%  {} / {}",
            m.mem_pct(),
            fmt_kib(m.mem_used_kib),
            fmt_kib(m.mem_total_kib)
        ));
        if let Some(sp) = m.swap_pct() {
            out.push(format!(
                "Swap: {sp:.0}%  {} / {}",
                fmt_kib(m.swap_used_kib),
                fmt_kib(m.swap_total_kib)
            ));
        }
        if let Some(d) = m.worst_disk() {
            out.push(format!(
                "Disk {}: {:.0}%  {} / {}",
                d.mount,
                d.pct(),
                fmt_kib(d.used_kib),
                fmt_kib(d.total_kib)
            ));
        }
        out.push(format!(
            "Load: {:.2} {:.2} {:.2}",
            m.load[0], m.load[1], m.load[2]
        ));
        out.push(format!("Uptime: {}", fmt_duration(m.uptime_secs)));
        if let Some(os) = &m.os {
            out.push(format!("OS: {os}"));
        }
    }
    if let Some(t) = v.last_check {
        out.push(format!("Last check: {}", fmt_ago(t, now)));
    }
    out
}

impl ksni::Tray for SvTray {
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "svmonitor".into()
    }
    fn title(&self) -> String {
        "SVMonitor".into()
    }
    fn category(&self) -> Category {
        Category::ApplicationStatus
    }
    fn status(&self) -> Status {
        match self.snap.worst() {
            Overall::Down | Overall::Attention => Status::NeedsAttention,
            _ => Status::Active,
        }
    }
    fn icon_pixmap(&self) -> Vec<Icon> {
        let c = color(self.snap.worst());
        vec![dot_icon(22, c), dot_icon(32, c), dot_icon(48, c)]
    }
    fn attention_icon_pixmap(&self) -> Vec<Icon> {
        vec![
            dot_icon(22, color(Overall::Down)),
            dot_icon(32, color(Overall::Down)),
        ]
    }
    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: format!(
                "SVMonitor — {}/{} online",
                self.snap.count_online(),
                self.snap.count_enabled()
            ),
            description: tooltip_text(&self.snap),
            icon_name: String::new(),
            icon_pixmap: vec![],
        }
    }
    fn menu_about_to_show(&mut self) {
        self.refresh_menu_data();
    }
    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        true // keep running; re-attach when a tray host appears
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let now = crate::util::now_unix();
        let mut m: Vec<MenuItem<Self>> = Vec::new();
        let header = if self.snap.servers.is_empty() {
            "SVMonitor — no servers yet".to_string()
        } else {
            format!(
                "SVMonitor — {}/{} online",
                self.snap.count_online(),
                self.snap.count_enabled()
            )
        };
        m.push(info(header));
        if !self.snap.local_net_ok {
            m.push(info(
                "⚠ This machine has no network route; server alerts paused".into(),
            ));
        }
        m.push(MenuItem::Separator);

        for v in &self.snap.servers {
            let name = v.name.clone();
            let mut sub: Vec<MenuItem<Self>> = detail_lines(v, now).into_iter().map(info).collect();
            let recent: Vec<&Event> = self
                .recent
                .iter()
                .rev()
                .filter(|e| e.server == v.name && !matches!(e.kind, EventKind::Started))
                .take(5)
                .collect();
            if !recent.is_empty() {
                sub.push(MenuItem::Separator);
                sub.push(info("Recent events".into()));
                for e in recent {
                    sub.push(info(format!("  {} — {}", fmt_ago(e.ts, now), e.message)));
                }
            }
            sub.push(MenuItem::Separator);
            let n = name.clone();
            sub.push(
                StandardItem {
                    label: "Open SSH terminal".into(),
                    icon_name: "utilities-terminal".into(),
                    activate: Box::new(move |t: &mut SvTray| t.open_ssh(&n)),
                    ..Default::default()
                }
                .into(),
            );
            let n = name.clone();
            sub.push(
                StandardItem {
                    label: "Check now".into(),
                    icon_name: "view-refresh".into(),
                    enabled: v.enabled,
                    activate: Box::new(move |t: &mut SvTray| t.engine.check_now(Some(&n))),
                    ..Default::default()
                }
                .into(),
            );
            let n = name.clone();
            sub.push(
                CheckmarkItem {
                    label: "Monitoring enabled".into(),
                    checked: v.enabled,
                    activate: Box::new(move |t: &mut SvTray| {
                        let on = !t
                            .snap
                            .servers
                            .iter()
                            .find(|s| s.name == n)
                            .map(|s| s.enabled)
                            .unwrap_or(true);
                        let _ = t.engine.set_enabled(&n, on);
                    }),
                    ..Default::default()
                }
                .into(),
            );
            let n = name.clone();
            sub.push(
                StandardItem {
                    label: "Remove from SVMonitor".into(),
                    icon_name: "list-remove".into(),
                    activate: Box::new(move |t: &mut SvTray| {
                        let _ = t.engine.remove(&n);
                    }),
                    ..Default::default()
                }
                .into(),
            );
            let mut label = format!("{} {} — {}", marker(v.overall), v.name, v.overall.label());
            if let (Some(mm), Overall::Online | Overall::Warning) = (&v.metrics, v.overall) {
                label += &format!("  ·  RAM {:.0}%", mm.mem_pct());
                if let Some(c) = mm.cpu_pct {
                    label += &format!("  CPU {c:.0}%");
                }
            }
            m.push(
                SubMenu {
                    label,
                    submenu: sub,
                    ..Default::default()
                }
                .into(),
            );
        }
        if self.snap.servers.is_empty() {
            m.push(info("Use “Add from SSH config” below".into()));
        }
        m.push(MenuItem::Separator);
        m.push(
            StandardItem {
                label: "Check all now".into(),
                icon_name: "view-refresh".into(),
                activate: Box::new(|t: &mut SvTray| t.engine.check_now(None)),
                ..Default::default()
            }
            .into(),
        );

        let mut add: Vec<MenuItem<Self>> = Vec::new();
        if self.discovered.is_empty() {
            add.push(info("No unmonitored hosts in ~/.ssh/config".into()));
        }
        for h in self.discovered.iter().take(40) {
            let h = h.clone();
            add.push(action(&h.clone(), move |t: &mut SvTray| {
                let _ = t.engine.add(config::Server::alias(&h));
            }));
        }
        m.push(
            SubMenu {
                label: "Add from SSH config".into(),
                icon_name: "list-add".into(),
                submenu: add,
                ..Default::default()
            }
            .into(),
        );

        m.push(MenuItem::Separator);
        m.push(
            CheckmarkItem {
                label: "Mute notifications for 1 hour".into(),
                checked: self.snap.muted,
                activate: Box::new(|t: &mut SvTray| {
                    let on = !t.snap.muted;
                    t.engine.send(crate::engine::Cmd::Mute(on.then_some(3600)));
                }),
                ..Default::default()
            }
            .into(),
        );
        m.push(
            CheckmarkItem {
                label: "Start with desktop session".into(),
                checked: self.autostart_on,
                activate: Box::new(|t: &mut SvTray| t.toggle_autostart()),
                ..Default::default()
            }
            .into(),
        );
        m.push(action("Open config file", |t: &mut SvTray| {
            let _ = launcher::open_path(&t.paths.config);
        }));
        m.push(action("Open event history", |t: &mut SvTray| {
            let _ = launcher::open_path(&t.paths.events);
        }));
        m.push(MenuItem::Separator);
        m.push(
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|t: &mut SvTray| t.engine.shutdown()),
                ..Default::default()
            }
            .into(),
        );
        m
    }
}

impl SvTray {
    fn open_ssh(&mut self, name: &str) {
        let server = self
            .cfg
            .server(name)
            .cloned()
            .unwrap_or_else(|| config::Server::alias(name));
        if let Err(e) = launcher::open_ssh(
            &self.cfg.general.terminal,
            &self.cfg.general.ssh_binary,
            &server,
        ) {
            eprintln!("svmonitor: cannot open ssh for {name}: {e:#}");
        }
    }
    fn toggle_autostart(&mut self) {
        let p = autostart::autostart_path();
        let r = if autostart::is_enabled(&p) {
            autostart::disable(&p).map(|_| ())
        } else {
            autostart::enable(&p, &self.exe)
        };
        if let Err(e) = r {
            eprintln!("svmonitor: autostart: {e:#}");
        }
        self.autostart_on = autostart::is_enabled(&p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(o: Overall) -> ServerView {
        ServerView {
            name: "a".into(),
            target: "a".into(),
            enabled: true,
            overall: o,
            ssh: SshState::Unknown,
            tailscale: TsState::NotApplicable,
            metrics: None,
            last_check: Some(90),
            last_ok: None,
            down_since: Some(40),
            detail: "connection timed out".into(),
            pending_failures: 0,
            failure_threshold: 3,
            active_alerts: vec![],
            probing: false,
        }
    }

    #[test]
    fn icon_is_valid_argb() {
        let i = dot_icon(22, (255, 0, 0));
        assert_eq!(i.data.len(), 22 * 22 * 4);
        assert_eq!(i.data[0], 0, "corner transparent");
        let mid = ((11 * 22 + 11) * 4) as usize;
        assert_eq!(i.data[mid], 255, "centre opaque");
        assert!(i.data[mid + 1] > 200, "red channel");
    }

    #[test]
    fn down_details_show_duration_and_reason() {
        let l = detail_lines(&view(Overall::Down), 100);
        assert!(l[0].contains("unreachable for 1m"), "{l:?}");
        assert!(l.iter().any(|x| x.contains("connection timed out")));
    }

    #[test]
    fn tooltip_lists_every_server() {
        let s = Snapshot {
            ts: 0,
            pid: 1,
            local_net_ok: true,
            muted: false,
            servers: vec![view(Overall::Down), view(Overall::Online)],
        };
        assert_eq!(tooltip_text(&s).lines().count(), 2);
    }
}
