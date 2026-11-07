//! StatusNotifierItem tray (works on KDE, and on GNOME with the AppIndicator extension, Cinnamon,
//! XFCE, Budgie, waybar, noctalia, ...). Drawn icons avoid any dependency on the icon theme.
use crate::config::{self, Config, Paths};
use crate::engine::{Cmd, Handle};
use crate::model::*;
use crate::util::{self, fmt_ago, fmt_duration, fmt_kib};
use crate::{autostart, history, launcher, sshconfig};
use ksni::menu::{CheckmarkItem, Disposition, MenuItem, StandardItem, SubMenu};
use ksni::{Category, Icon, Status, ToolTip};
use std::collections::HashMap;

const PAGE_LIMIT: usize = 8;
const TOOLTIP_SERVER_LIMIT: usize = 5;

/// Escape underscores in user-controlled labels for DBusMenu mnemonics.
pub fn escape_label(s: &str) -> String {
    s.replace('_', "__")
}

/// Escape XML/Pango markup and sanitize control characters for tooltips.
pub fn escape_markup(s: &str) -> String {
    util::escape_markup(&util::sanitize_text(s, 240))
}
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
    queued_all: HashMap<String, u64>,
    queued_servers: HashMap<String, u64>,
    pending_actions: Vec<std::sync::mpsc::Receiver<Result<(), String>>>,
    feedback: Option<String>,
}
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
            queued_all: HashMap::new(),
            pending_actions: Vec::new(),
            queued_servers: HashMap::new(),
            feedback: None,
        }
    }

    fn reconcile_queued(&mut self) {
        let snap = &self.snap;
        let pending = |name: &String, count: &mut u64| {
            snap.servers
                .iter()
                .any(|s| s.name == *name && s.enabled && s.completed_checks == *count)
        };
        self.queued_all.retain(pending);
        self.queued_servers.retain(pending);
        self.pending_actions.retain(|rx| {
            use std::sync::mpsc::TryRecvError;
            match rx.try_recv() {
                Ok(Ok(())) => {
                    self.feedback = None;
                    false
                }
                Ok(Err(e)) => {
                    self.feedback = Some(format!(
                        "Action failed: {}",
                        escape_label(&util::sanitize_text(&e, 200))
                    ));
                    false
                }
                Err(TryRecvError::Disconnected) => {
                    self.feedback = Some("Action failed: engine stopped".into());
                    false
                }
                Err(TryRecvError::Empty) => true,
            }
        });
    }

    fn queue_action(&mut self, make: impl FnOnce(crate::engine::Reply) -> Cmd) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.pending_actions.push(rx);
        self.engine.send(make(Some(tx)));
    }

    fn refresh_menu_data(&mut self) {
        self.reconcile_queued();
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

fn server_icon_name(o: Overall) -> &'static str {
    match o {
        Overall::Online => "network-server",
        Overall::Warning => "dialog-warning",
        Overall::Down => "network-error",
        Overall::Attention => "dialog-warning",
        Overall::LocalIssue => "dialog-warning",
        Overall::Unknown => "network-transmit-receive",
        Overall::Disabled => "media-playback-pause",
    }
}

fn server_disposition(o: Overall) -> Disposition {
    match o {
        Overall::Down => Disposition::Alert,
        Overall::Attention | Overall::Warning | Overall::LocalIssue => Disposition::Warning,
        Overall::Online | Overall::Unknown | Overall::Disabled => Disposition::Normal,
    }
}

fn sort_urgency<'a>(servers: &[&'a ServerView]) -> Vec<&'a ServerView> {
    let mut out = servers.to_vec();
    out.sort_by(|a, b| {
        b.overall
            .severity()
            .cmp(&a.overall.severity())
            .then_with(|| b.pending_failures.cmp(&a.pending_failures))
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

fn ssh_label(ssh: SshState) -> String {
    match ssh {
        SshState::Unknown => "unknown".into(),
        SshState::Available => "available".into(),
        SshState::Unavailable(k) => format!("unavailable ({})", k.label()),
    }
}

pub fn tooltip_summary(s: &Snapshot) -> String {
    if s.servers.is_empty() {
        return "SVMonitor - no servers configured".into();
    }
    let online = s.count_online();
    let enabled = s.count_enabled();
    let down = s
        .servers
        .iter()
        .filter(|v| v.overall == Overall::Down)
        .count();
    let attn = s
        .servers
        .iter()
        .filter(|v| v.overall == Overall::Attention)
        .count();
    let warn = s
        .servers
        .iter()
        .filter(|v| v.overall == Overall::Warning)
        .count();
    let retrying = s
        .servers
        .iter()
        .filter(|v| v.enabled && v.pending_failures > 0 && v.overall != Overall::Down)
        .count();
    let checking = s
        .servers
        .iter()
        .filter(|v| v.enabled && (v.probing || v.overall == Overall::Unknown))
        .count();
    let disabled = s.servers.iter().filter(|v| !v.enabled).count();

    let mut parts = vec![format!("{online}/{enabled} online")];
    if down > 0 {
        parts.push(format!("{down} down"));
    }
    if attn > 0 {
        parts.push(format!("{attn} attention"));
    }
    if warn > 0 {
        parts.push(format!("{warn} warning"));
    }
    if retrying > 0 {
        parts.push(format!("{retrying} retrying"));
    }
    if checking > 0 {
        parts.push(format!("{checking} checking"));
    }
    if disabled > 0 {
        parts.push(format!("{disabled} disabled"));
    }
    format!("SVMonitor - {}", parts.join(", "))
}

pub fn tooltip_text(s: &Snapshot) -> String {
    if s.servers.is_empty() {
        return "No servers configured".into();
    }
    let mut lines = Vec::new();
    lines.push(escape_markup(&tooltip_summary(s)));
    if !s.local_net_ok {
        lines.push("Local network issue: server alerts paused".into());
    }

    let refs: Vec<&ServerView> = s.servers.iter().collect();
    let ordered = sort_urgency(&refs);

    for v in ordered.iter().take(TOOLTIP_SERVER_LIMIT) {
        let mut state = v.overall.label().to_string();
        if v.pending_failures > 0 && v.overall != Overall::Down {
            state.push_str(&format!(
                " (retry {}/{})",
                v.pending_failures, v.failure_threshold
            ));
        }
        let ssh = ssh_label(v.ssh);
        let ts = v.tailscale.label();
        let extra = match (&v.metrics, v.ssh, v.overall) {
            (Some(m), SshState::Available, Overall::Online | Overall::Warning) => {
                let cpu = m
                    .cpu_pct
                    .map(|c| format!("{c:.0}%"))
                    .unwrap_or_else(|| "-".into());
                format!(" | CPU {cpu}, RAM {:.0}%", m.mem_pct())
            }
            _ if !v.detail.is_empty() => format!(" | {}", v.detail),
            _ => String::new(),
        };
        let raw_line = format!(
            "{}: {} [SSH: {}, Tailscale: {}]{}",
            v.name, state, ssh, ts, extra
        );
        lines.push(escape_markup(&raw_line));
    }

    if ordered.len() > TOOLTIP_SERVER_LIMIT {
        let rem = ordered.len() - TOOLTIP_SERVER_LIMIT;
        lines.push(format!("...and {rem} more servers"));
    }

    lines.join("\n")
}

fn info(label: impl Into<String>) -> MenuItem<SvTray> {
    StandardItem {
        label: label.into(),
        enabled: false,
        disposition: Disposition::Informative,
        ..Default::default()
    }
    .into()
}

fn info_icon(label: impl Into<String>, icon: &str, disp: Disposition) -> MenuItem<SvTray> {
    StandardItem {
        label: label.into(),
        icon_name: icon.into(),
        enabled: false,
        disposition: disp,
        ..Default::default()
    }
    .into()
}

fn action(
    label: impl Into<String>,
    icon: &str,
    f: impl Fn(&mut SvTray) + Send + 'static,
) -> MenuItem<SvTray> {
    StandardItem {
        label: label.into(),
        icon_name: icon.into(),
        activate: Box::new(f),
        ..Default::default()
    }
    .into()
}

fn quick_metrics_lines(v: &ServerView) -> Vec<String> {
    let Some(m) = &v.metrics else {
        return Vec::new();
    };
    let stale = if v.ssh == SshState::Available {
        ""
    } else {
        " (stale)"
    };
    let mut out = Vec::new();
    if let Some(c) = m.cpu_pct {
        out.push(format!(
            "CPU: {c:.0}%{stale} ({} cores)",
            m.cores.unwrap_or(0)
        ));
    } else {
        out.push(format!("CPU: -{stale} ({} cores)", m.cores.unwrap_or(0)));
    }
    out.push(format!(
        "RAM: {:.0}%{stale} ({} / {})",
        m.mem_pct(),
        fmt_kib(m.mem_used_kib),
        fmt_kib(m.mem_total_kib)
    ));
    if let Some(d) = m.worst_disk() {
        out.push(format!(
            "Disk {}: {:.0}%{stale} ({} / {})",
            escape_label(&util::sanitize_text(&d.mount, 48)),
            d.pct(),
            fmt_kib(d.used_kib),
            fmt_kib(d.total_kib)
        ));
    }
    out
}

fn full_detail_lines(v: &ServerView, now: i64) -> Vec<String> {
    let mut out = Vec::new();
    out.push(format!(
        "Target: {}",
        escape_label(&util::sanitize_text(&v.target, 80))
    ));
    if let Some(m) = &v.metrics {
        let stale = if v.ssh == SshState::Available {
            ""
        } else {
            " (stale)"
        };
        if let Some(sp) = m.swap_pct() {
            out.push(format!(
                "Swap: {sp:.0}%{stale} ({} / {})",
                fmt_kib(m.swap_used_kib),
                fmt_kib(m.swap_total_kib)
            ));
        } else {
            out.push(format!("Swap: none{stale}"));
        }
        for d in &m.disks {
            out.push(format!(
                "Disk {}: {:.0}%{stale} ({} / {})",
                escape_label(&util::sanitize_text(&d.mount, 48)),
                d.pct(),
                fmt_kib(d.used_kib),
                fmt_kib(d.total_kib)
            ));
        }
        out.push(format!(
            "Load: {:.2} {:.2} {:.2}{stale}",
            m.load[0], m.load[1], m.load[2]
        ));
        out.push(format!("Uptime: {}{stale}", fmt_duration(m.uptime_secs)));
        if let Some(procs) = m.procs {
            out.push(format!("Processes: {procs}{stale}"));
        }
        if let Some(host) = &m.hostname {
            out.push(format!(
                "Hostname: {}{stale}",
                escape_label(&util::sanitize_text(host, 64))
            ));
        }
        if let Some(os) = &m.os {
            out.push(format!(
                "OS: {}{stale}",
                escape_label(&util::sanitize_text(os, 64))
            ));
        }
        if let Some(k) = &m.kernel {
            out.push(format!(
                "Kernel: {}{stale}",
                escape_label(&util::sanitize_text(k, 64))
            ));
        }
    }
    if let Some(t) = v.last_check {
        out.push(format!("Last check: {}", fmt_ago(t, now)));
    }
    if let Some(ok) = v.last_ok {
        out.push(format!("Last OK: {}", fmt_ago(ok, now)));
    }
    out
}

pub fn detail_lines(v: &ServerView, now: i64) -> Vec<String> {
    let mut out = Vec::new();
    let mut status = format!("Status: {}", v.overall.label());
    if v.pending_failures > 0 {
        status.push_str(&format!(
            " ({}/{} failed checks)",
            v.pending_failures, v.failure_threshold
        ));
    }
    if let (Overall::Down, Some(d)) = (v.overall, v.down_since) {
        status.push_str(&format!(" for {}", fmt_duration((now - d).max(0) as u64)));
    }
    out.push(status);
    if !v.detail.is_empty() && v.overall != Overall::Online {
        out.push(format!(
            "Cause: {}",
            escape_label(&util::sanitize_text(&v.detail, 120))
        ));
    }
    if let SshState::Unavailable(k) = v.ssh
        && let Some(h) = k.hint()
    {
        out.push(format!("Hint: {}", escape_label(h)));
    }
    out.push(format!("SSH: {}", ssh_label(v.ssh)));
    out.push(format!("Tailscale: {}", v.tailscale.label()));
    for alert in &v.active_alerts {
        out.push(format!(
            "Alert: {}",
            escape_label(&util::sanitize_text(alert, 100))
        ));
    }
    out.extend(quick_metrics_lines(v));
    out.extend(full_detail_lines(v, now));
    out
}

impl SvTray {
    fn is_server_queued(&self, v: &ServerView) -> bool {
        v.enabled
            && !v.probing
            && self
                .queued_all
                .get(&v.name)
                .or_else(|| self.queued_servers.get(&v.name))
                .is_some_and(|count| *count == v.completed_checks)
    }

    fn is_all_queued(&self) -> bool {
        self.snap.servers.iter().any(|v| {
            v.enabled
                && self
                    .queued_all
                    .get(&v.name)
                    .is_some_and(|count| *count == v.completed_checks)
        })
    }

    fn build_server_submenu(&self, v: &ServerView, now: i64) -> MenuItem<Self> {
        let name = v.name.clone();
        let escaped_name = escape_label(&util::sanitize_text(&v.name, 64));
        let mut sub: Vec<MenuItem<Self>> = Vec::new();

        // Primary actions near top
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

        let srv_queued = self.is_server_queued(v);
        let (check_label, check_enabled) = if !v.enabled {
            ("Check now", false)
        } else if v.probing {
            ("Checking...", false)
        } else if srv_queued {
            ("Check queued", false)
        } else {
            ("Check now", true)
        };
        let n = name.clone();
        let last_chk = v.completed_checks;
        sub.push(
            StandardItem {
                label: check_label.into(),
                icon_name: "view-refresh".into(),
                enabled: check_enabled,
                activate: Box::new(move |t: &mut SvTray| {
                    t.queued_servers.insert(n.clone(), last_chk);
                    t.feedback = None;
                    t.engine.check_now(Some(&n));
                }),
                ..Default::default()
            }
            .into(),
        );

        sub.push(MenuItem::Separator);

        // Short state heading and cause
        let mut status = format!("Status: {}", v.overall.label());
        if v.pending_failures > 0 {
            status.push_str(&format!(
                " ({}/{} failed checks)",
                v.pending_failures, v.failure_threshold
            ));
        }
        if let (Overall::Down, Some(d)) = (v.overall, v.down_since) {
            status.push_str(&format!(" for {}", fmt_duration((now - d).max(0) as u64)));
        }
        sub.push(info_icon(
            status,
            server_icon_name(v.overall),
            server_disposition(v.overall),
        ));

        if !v.detail.is_empty() && v.overall != Overall::Online {
            sub.push(info(format!(
                "Cause: {}",
                escape_label(&util::sanitize_text(&v.detail, 120))
            )));
        }
        if let SshState::Unavailable(k) = v.ssh
            && let Some(h) = k.hint()
        {
            sub.push(info(format!("Hint: {}", escape_label(h))));
        }

        // SSH and Tailscale separate
        sub.push(info(format!("SSH: {}", ssh_label(v.ssh))));
        sub.push(info(format!("Tailscale: {}", v.tailscale.label())));

        // Resource alerts textual and decorated with standard warning icon
        for alert in &v.active_alerts {
            sub.push(info_icon(
                format!("Alert: {}", escape_label(&util::sanitize_text(alert, 100))),
                "dialog-warning",
                Disposition::Warning,
            ));
        }

        // Quick metrics view (with explicit stale flag if SSH != Available)
        let quick = quick_metrics_lines(v);
        if !quick.is_empty() {
            sub.push(MenuItem::Separator);
            for line in quick {
                sub.push(info(line));
            }
        }

        sub.push(MenuItem::Separator);

        // Deeper Details submenu
        let details: Vec<MenuItem<Self>> = full_detail_lines(v, now)
            .into_iter()
            .map(|line| {
                if line.starts_with("Disk ") {
                    info_icon(line, "drive-harddisk", Disposition::Informative)
                } else {
                    info(line)
                }
            })
            .collect();
        sub.push(
            SubMenu {
                label: "Details".into(),
                icon_name: "computer".into(),
                submenu: details,
                ..Default::default()
            }
            .into(),
        );

        // Collapsed Recent events submenu
        let recent: Vec<&Event> = self
            .recent
            .iter()
            .rev()
            .filter(|e| e.server == v.name && !matches!(e.kind, EventKind::Started))
            .take(8)
            .collect();
        if !recent.is_empty() {
            let event_items: Vec<MenuItem<Self>> = recent
                .iter()
                .map(|e| {
                    info(format!(
                        "{} - {}",
                        fmt_ago(e.ts, now),
                        escape_label(&util::sanitize_text(&e.message, 100))
                    ))
                })
                .collect();
            sub.push(
                SubMenu {
                    label: format!("Recent events ({})", recent.len()),
                    submenu: event_items,
                    ..Default::default()
                }
                .into(),
            );
        }

        // Manage server submenu (monitoring toggle + removal)
        let mut manage_sub: Vec<MenuItem<Self>> = Vec::new();
        let n = name.clone();
        let on = !v.enabled;
        manage_sub.push(
            CheckmarkItem {
                label: "Monitoring enabled".into(),
                checked: v.enabled,
                activate: Box::new(move |t: &mut SvTray| {
                    t.feedback = Some(format!(
                        "Requested monitoring {} for {}",
                        if on { "on" } else { "off" },
                        escape_label(&n)
                    ));
                    t.queue_action(|reply| Cmd::SetEnabled(n.clone(), on, reply));
                }),
                ..Default::default()
            }
            .into(),
        );
        manage_sub.push(MenuItem::Separator);
        let n = name.clone();
        manage_sub.push(
            StandardItem {
                label: "Remove from SVMonitor".into(),
                icon_name: "list-remove".into(),
                disposition: Disposition::Warning,
                activate: Box::new(move |t: &mut SvTray| {
                    t.feedback = Some(format!("Requested removal of {}", escape_label(&n)));
                    t.queue_action(|reply| Cmd::Remove(n.clone(), reply));
                }),
                ..Default::default()
            }
            .into(),
        );
        sub.push(
            SubMenu {
                label: "Manage".into(),
                icon_name: "preferences-system".into(),
                submenu: manage_sub,
                ..Default::default()
            }
            .into(),
        );

        // Top-level server item label (plain ASCII + escaped underscores)
        let mut state_str = v.overall.label().to_string();
        if v.probing {
            state_str = "checking".into();
        } else if v.pending_failures > 0 && v.overall != Overall::Down {
            state_str = format!(
                "{} (retry {}/{})",
                state_str, v.pending_failures, v.failure_threshold
            );
        }
        let mut label = format!("{escaped_name} - {state_str}");
        if let (Some(mm), SshState::Available, Overall::Online | Overall::Warning) =
            (&v.metrics, v.ssh, v.overall)
        {
            label.push_str(&format!(" | RAM {:.0}%", mm.mem_pct()));
            if let Some(c) = mm.cpu_pct {
                label.push_str(&format!(" CPU {c:.0}%"));
            }
        }

        SubMenu {
            label,
            icon_name: server_icon_name(v.overall).into(),
            disposition: server_disposition(v.overall),
            submenu: sub,
            ..Default::default()
        }
        .into()
    }

    fn build_paged_servers(
        &self,
        servers: &[&ServerView],
        now: i64,
        group_prefix: &str,
    ) -> Vec<MenuItem<Self>> {
        if servers.len() <= PAGE_LIMIT {
            return servers
                .iter()
                .map(|v| self.build_server_submenu(v, now))
                .collect();
        }

        let mut groups: Vec<(String, Disposition, Vec<MenuItem<Self>>)> = Vec::new();
        for (idx, chunk) in servers.chunks(PAGE_LIMIT).enumerate() {
            let start = idx * PAGE_LIMIT + 1;
            let end = start + chunk.len() - 1;
            let worst = chunk
                .iter()
                .map(|s| s.overall)
                .max_by_key(|o| o.severity())
                .unwrap_or(Overall::Disabled);
            let first_name = escape_label(&util::sanitize_text(&chunk[0].name, 20));
            let last_name = escape_label(&util::sanitize_text(&chunk[chunk.len() - 1].name, 20));
            let label = if chunk.len() == 1 {
                format!("{group_prefix} {start} ({first_name})")
            } else {
                format!("{group_prefix} {start}-{end} ({first_name} .. {last_name})")
            };
            let items = chunk
                .iter()
                .map(|v| self.build_server_submenu(v, now))
                .collect();
            groups.push((label, server_disposition(worst), items));
        }

        while groups.len() > PAGE_LIMIT {
            let mut super_groups = Vec::new();
            let mut source = groups.into_iter();
            let mut idx = 0;
            loop {
                let gchunk: Vec<_> = source.by_ref().take(PAGE_LIMIT).collect();
                if gchunk.is_empty() {
                    break;
                }
                let start = idx * PAGE_LIMIT + 1;
                let end = start + gchunk.len() - 1;
                let disp = if gchunk.iter().any(|(_, d, _)| *d == Disposition::Alert) {
                    Disposition::Alert
                } else if gchunk.iter().any(|(_, d, _)| *d == Disposition::Warning) {
                    Disposition::Warning
                } else {
                    Disposition::Normal
                };
                let sub_items: Vec<MenuItem<Self>> = gchunk
                    .into_iter()
                    .map(|(lbl, d, sub)| {
                        SubMenu {
                            label: lbl,
                            icon_name: "network-server".into(),
                            disposition: d,
                            submenu: sub,
                            ..Default::default()
                        }
                        .into()
                    })
                    .collect();
                super_groups.push((
                    format!("{group_prefix} Groups {start}-{end}"),
                    disp,
                    sub_items,
                ));
                idx += 1;
            }
            groups = super_groups;
        }

        groups
            .into_iter()
            .map(|(lbl, disp, sub)| {
                SubMenu {
                    label: lbl,
                    icon_name: "network-server".into(),
                    disposition: disp,
                    submenu: sub,
                    ..Default::default()
                }
                .into()
            })
            .collect()
    }

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
            let msg = format!(
                "SSH launch failed for {}: {}",
                escape_label(name),
                escape_label(&util::sanitize_text(&format!("{e:#}"), 100))
            );
            eprintln!("svmonitor: cannot open ssh for {name}: {e:#}");
            self.feedback = Some(msg);
        } else {
            self.feedback = None;
        }
    }

    fn open_file(&mut self, label_what: &str, path: &std::path::Path) {
        if let Err(e) = launcher::open_path(path) {
            let msg = format!(
                "Cannot open {label_what}: {}",
                escape_label(&util::sanitize_text(&format!("{e:#}"), 100))
            );
            eprintln!("svmonitor: open {label_what}: {e:#}");
            self.feedback = Some(msg);
        } else {
            self.feedback = None;
        }
    }

    fn toggle_autostart(&mut self) {
        let p = autostart::autostart_path();
        let r = if autostart::is_enabled(&p) {
            autostart::disable(&p).map(|_| ())
        } else {
            autostart::enable(&p, &self.exe)
        };
        self.autostart_on = autostart::is_enabled(&p);
        if let Err(e) = r {
            let msg = format!(
                "Autostart failed: {}",
                escape_label(&util::sanitize_text(&format!("{e:#}"), 100))
            );
            eprintln!("svmonitor: autostart: {e:#}");
            self.feedback = Some(msg);
        } else {
            self.feedback = None;
        }
    }
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
        crate::tray_icon::icons(self.snap.worst())
    }
    fn attention_icon_pixmap(&self) -> Vec<Icon> {
        crate::tray_icon::icons(self.snap.worst())
    }
    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: escape_markup(&tooltip_summary(&self.snap)),
            description: tooltip_text(&self.snap),
            icon_name: String::new(),
            icon_pixmap: vec![],
        }
    }
    fn menu_about_to_show(&mut self) {
        self.refresh_menu_data();
    }
    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        true // keep running; re-attach when tray host appears
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let now = util::now_unix();
        let mut m: Vec<MenuItem<Self>> = Vec::new();

        let header = tooltip_summary(&self.snap);
        m.push(info(header));

        if !self.snap.local_net_ok {
            m.push(info_icon(
                "Local network route down - server alerts paused",
                "dialog-warning",
                Disposition::Warning,
            ));
        }

        if let Some(fb) = &self.feedback {
            m.push(info_icon(
                fb.clone(),
                "dialog-warning",
                Disposition::Warning,
            ));
        }

        m.push(MenuItem::Separator);

        // Check all action near top
        let any_enabled = self.snap.servers.iter().any(|s| s.enabled);
        let any_probing = self.snap.servers.iter().any(|s| s.probing);
        let all_queued = self.is_all_queued();
        let (chk_label, chk_enabled) = if !any_enabled {
            ("Check all now", false)
        } else if any_probing {
            ("Checking...", false)
        } else if all_queued {
            ("Check queued", false)
        } else {
            ("Check all now", true)
        };
        m.push(
            StandardItem {
                label: chk_label.into(),
                icon_name: "view-refresh".into(),
                enabled: chk_enabled,
                activate: Box::new(|t: &mut SvTray| {
                    t.queued_all = t
                        .snap
                        .servers
                        .iter()
                        .filter(|v| v.enabled)
                        .map(|v| (v.name.clone(), v.completed_checks))
                        .collect();
                    t.feedback = None;
                    t.engine.check_now(None);
                }),
                ..Default::default()
            }
            .into(),
        );

        m.push(MenuItem::Separator);

        if self.snap.servers.is_empty() {
            m.push(info("No servers configured - use Settings & Manage below"));
        } else {
            let active_refs: Vec<&ServerView> = self
                .snap
                .servers
                .iter()
                .filter(|s| s.enabled || s.overall != Overall::Disabled)
                .collect();
            let disabled_refs: Vec<&ServerView> = self
                .snap
                .servers
                .iter()
                .filter(|s| !s.enabled && s.overall == Overall::Disabled)
                .collect();

            let active_sorted = sort_urgency(&active_refs);
            if active_sorted.len() <= PAGE_LIMIT {
                for v in &active_sorted {
                    m.push(self.build_server_submenu(v, now));
                }
            } else {
                // Urgent / top priority direct items + paged remainder so top-level stays <= 8
                let direct_count = PAGE_LIMIT - 1;
                for v in &active_sorted[..direct_count] {
                    m.push(self.build_server_submenu(v, now));
                }
                let rest = &active_sorted[direct_count..];
                let rest_items = self.build_paged_servers(rest, now, "Page");
                let rest_worst = rest
                    .iter()
                    .map(|s| s.overall)
                    .max_by_key(|o| o.severity())
                    .unwrap_or(Overall::Online);
                m.push(
                    SubMenu {
                        label: format!("More servers ({})", rest.len()),
                        icon_name: "network-server".into(),
                        disposition: server_disposition(rest_worst),
                        submenu: rest_items,
                        ..Default::default()
                    }
                    .into(),
                );
            }

            if !disabled_refs.is_empty() {
                let mut disabled_sorted = disabled_refs;
                disabled_sorted.sort_by(|a, b| a.name.cmp(&b.name));
                let dis_items = self.build_paged_servers(&disabled_sorted, now, "Disabled");
                m.push(
                    SubMenu {
                        label: format!("Disabled servers ({})", disabled_sorted.len()),
                        icon_name: "media-playback-pause".into(),
                        submenu: dis_items,
                        ..Default::default()
                    }
                    .into(),
                );
            }
        }

        m.push(MenuItem::Separator);

        // Secondary settings / import / history / autostart under compact Settings & Manage submenu
        let mut settings_sub: Vec<MenuItem<Self>> = Vec::new();

        let mut add_sub: Vec<MenuItem<Self>> = Vec::new();
        if self.discovered.is_empty() {
            add_sub.push(info("No unmonitored hosts in ~/.ssh/config"));
        } else {
            let hosts: Vec<String> = self.discovered.clone();
            if hosts.len() <= PAGE_LIMIT {
                for h in hosts {
                    let label = escape_label(&util::sanitize_text(&h, 64));
                    add_sub.push(action(label, "list-add", move |t: &mut SvTray| {
                        t.feedback = Some(format!("Requested adding {}", escape_label(&h)));
                        t.queue_action(|reply| Cmd::Add(config::Server::alias(&h), reply));
                    }));
                }
            } else {
                for (idx, chunk) in hosts.chunks(PAGE_LIMIT).enumerate() {
                    let start = idx * PAGE_LIMIT + 1;
                    let end = start + chunk.len() - 1;
                    let sub_hosts: Vec<MenuItem<Self>> = chunk
                        .iter()
                        .cloned()
                        .map(|h| {
                            let label = escape_label(&util::sanitize_text(&h, 64));
                            action(label, "list-add", move |t: &mut SvTray| {
                                t.feedback = Some(format!("Requested adding {}", escape_label(&h)));
                                t.queue_action(|reply| Cmd::Add(config::Server::alias(&h), reply));
                            })
                        })
                        .collect();
                    add_sub.push(
                        SubMenu {
                            label: format!("Hosts {start}-{end}"),
                            icon_name: "list-add".into(),
                            submenu: sub_hosts,
                            ..Default::default()
                        }
                        .into(),
                    );
                }
            }
        }
        settings_sub.push(
            SubMenu {
                label: "Add from SSH config".into(),
                icon_name: "list-add".into(),
                submenu: add_sub,
                ..Default::default()
            }
            .into(),
        );

        settings_sub.push(MenuItem::Separator);

        settings_sub.push(
            CheckmarkItem {
                label: "Mute notifications 1 hour".into(),
                checked: self.snap.muted,
                activate: Box::new(|t: &mut SvTray| {
                    let on = !t.snap.muted;
                    t.engine.send(Cmd::Mute(on.then_some(3600)));
                }),
                ..Default::default()
            }
            .into(),
        );
        settings_sub.push(
            CheckmarkItem {
                label: "Start with desktop session".into(),
                checked: self.autostart_on,
                activate: Box::new(|t: &mut SvTray| t.toggle_autostart()),
                ..Default::default()
            }
            .into(),
        );

        settings_sub.push(MenuItem::Separator);

        settings_sub.push(action(
            "Open config file",
            "preferences-system",
            |t: &mut SvTray| {
                let p = t.paths.config.clone();
                t.open_file("config file", &p);
            },
        ));
        settings_sub.push(action(
            "Open event history",
            "utilities-terminal",
            |t: &mut SvTray| {
                let p = t.paths.events.clone();
                t.open_file("event history", &p);
            },
        ));
        settings_sub.push(action("Reload config", "view-refresh", |t: &mut SvTray| {
            t.feedback = None;
            t.engine.send(Cmd::Reload);
        }));

        m.push(
            SubMenu {
                label: "Settings & Manage".into(),
                icon_name: "preferences-system".into(),
                submenu: settings_sub,
                ..Default::default()
            }
            .into(),
        );

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use ksni::Tray;
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
            completed_checks: 0,
            last_ok: None,
            down_since: Some(40),
            detail: "connection timed out".into(),
            pending_failures: 0,
            failure_threshold: 3,
            active_alerts: vec![],
            probing: false,
        }
    }

    fn test_tray(snap: Snapshot) -> (tempfile::TempDir, SvTray) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            config: dir.path().join("config.toml"),
            state_dir: dir.path().join("state"),
            runtime_dir: dir.path().join("run"),
        };
        let mut cfg = Config::default();
        for s in &snap.servers {
            let _ = cfg.add(config::Server {
                host: Some(s.target.clone()),
                enabled: s.enabled,
                ..config::Server::alias(&s.name)
            });
        }
        let eng_paths = Paths {
            config: paths.config.clone(),
            state_dir: paths.state_dir.clone(),
            runtime_dir: paths.runtime_dir.clone(),
        };
        let (_eng, handle) = Engine::new(cfg.clone(), eng_paths, false);
        let mut tray = SvTray::new(handle, &paths, cfg);
        tray.snap = snap;
        (dir, tray)
    }

    fn collect_labels(items: &[MenuItem<SvTray>]) -> Vec<String> {
        let mut out = Vec::new();
        for it in items {
            match it {
                MenuItem::Standard(s) => out.push(s.label.clone()),
                MenuItem::Checkmark(c) => out.push(c.label.clone()),
                MenuItem::SubMenu(sub) => {
                    out.push(sub.label.clone());
                    out.extend(collect_labels(&sub.submenu));
                }
                _ => {}
            }
        }
        out
    }

    fn max_servers_in_any_group(items: &[MenuItem<SvTray>]) -> usize {
        let mut max_count = 0;
        let mut server_items_here = 0;
        for it in items {
            if let MenuItem::SubMenu(sub) = it {
                if sub.submenu.iter().any(|child| match child {
                    MenuItem::Standard(s) => s.label.contains("SSH") || s.label.contains("Check"),
                    MenuItem::SubMenu(s2) => {
                        s2.label.contains("Details") || s2.label.contains("Manage")
                    }
                    _ => false,
                }) {
                    server_items_here += 1;
                }
                let child_max = max_servers_in_any_group(&sub.submenu);
                if child_max > max_count {
                    max_count = child_max;
                }
            }
        }
        max_count.max(server_items_here)
    }

    #[test]
    fn test_underscores_retained_by_escaping() {
        assert_eq!(escape_label("db_prod_01"), "db__prod__01");
        assert_eq!(escape_label("app_api_v2_core"), "app__api__v2__core");
        assert_eq!(escape_label("simple"), "simple");
    }

    #[test]
    fn test_markup_escape_and_control_sanitization() {
        let raw = "<script>alert('hello' & \"world\");</script>\x07\r\x1b[31m";
        let escaped = escape_markup(raw);
        assert!(!escaped.contains('<'));
        assert!(!escaped.contains('>'));
        assert!(escaped.contains("&lt;script&gt;"));
        assert!(escaped.contains("&amp;"));
        assert!(!escaped.chars().any(|c| c.is_control()));
    }

    #[test]
    fn test_no_omissions_at_25_plus_servers_and_bounded_groups() {
        let mut servers = Vec::new();
        for i in 0..24 {
            let overall = match i % 3 {
                0 => Overall::Down,
                1 => Overall::Warning,
                _ => Overall::Online,
            };
            let mut s = view(overall);
            s.name = format!("srv_prod_{:02}", i);
            s.target = format!("10.0.0.{}", i + 1);
            s.enabled = true;
            servers.push(s);
        }
        for i in 24..32 {
            let mut s = view(Overall::Disabled);
            s.name = format!("srv_dis_{:02}", i);
            s.target = format!("10.0.1.{}", i + 1);
            s.enabled = false;
            servers.push(s);
        }

        let snap = Snapshot {
            ts: 1000,
            pid: 1,
            local_net_ok: true,
            muted: false,
            servers,
        };

        let (_dir, tray) = test_tray(snap.clone());
        let menu = tray.menu();
        let labels = collect_labels(&menu);

        for s in &snap.servers {
            let escaped = escape_label(&s.name);
            assert!(
                labels
                    .iter()
                    .any(|l| l.contains(&escaped) || l.contains(&s.name)),
                "Server {} must not disappear from navigation",
                s.name
            );
        }

        let max_group = max_servers_in_any_group(&menu);
        assert!(
            max_group <= 8,
            "Server groups must be bounded to <= 8, found {max_group}"
        );
    }

    #[test]
    fn test_urgent_first_stable_server_ordering() {
        let servers = vec![
            ServerView {
                name: "srv_z_ok".into(),
                overall: Overall::Online,
                enabled: true,
                ..view(Overall::Online)
            },
            ServerView {
                name: "srv_a_ok".into(),
                overall: Overall::Online,
                enabled: true,
                ..view(Overall::Online)
            },
            ServerView {
                name: "srv_warn".into(),
                overall: Overall::Warning,
                enabled: true,
                ..view(Overall::Warning)
            },
            ServerView {
                name: "srv_down".into(),
                overall: Overall::Down,
                enabled: true,
                ..view(Overall::Down)
            },
            ServerView {
                name: "srv_attn".into(),
                overall: Overall::Attention,
                enabled: true,
                ..view(Overall::Attention)
            },
        ];
        let snap = Snapshot {
            ts: 1000,
            pid: 1,
            local_net_ok: true,
            muted: false,
            servers,
        };
        let refs: Vec<_> = snap.servers.iter().collect();
        let ordered = sort_urgency(&refs);
        let position = |name: &str| ordered.iter().position(|v| v.name == name).unwrap();
        let pos_down = position("srv_down");
        let pos_attn = position("srv_attn");
        let pos_warn = position("srv_warn");
        let pos_a_ok = position("srv_a_ok");
        let pos_z_ok = position("srv_z_ok");

        assert!(
            pos_down < pos_a_ok,
            "Down server must appear before Online server"
        );
        assert!(
            pos_attn < pos_a_ok,
            "Attention server must appear before Online server"
        );
        assert!(
            pos_warn < pos_a_ok,
            "Warning server must appear before Online server"
        );
        assert!(
            pos_a_ok < pos_z_ok,
            "Same urgency servers must be sorted stably"
        );
    }

    #[test]
    fn test_retry_state_shown() {
        let mut s = view(Overall::Warning);
        s.name = "db_node".into();
        s.pending_failures = 2;
        s.failure_threshold = 3;

        let lines = detail_lines(&s, 100);
        assert!(
            lines.iter().any(|l| l.contains("2/3 failed checks")),
            "Retry state (failed check count) must be shown in details: {lines:?}"
        );

        let snap = Snapshot {
            ts: 100,
            pid: 1,
            local_net_ok: true,
            muted: false,
            servers: vec![s],
        };
        let tt = tooltip_text(&snap);
        assert!(
            tt.contains("retry") || tt.contains("retrying"),
            "Tooltip concise counts must include retry state: {tt}"
        );
    }

    #[test]
    fn test_stale_metrics_on_unavailable_ssh() {
        let metrics = Metrics {
            ts: 80,
            cpu_pct: Some(45.0),
            cores: Some(4),
            load: [1.2, 0.8, 0.5],
            mem_total_kib: 16000000,
            mem_used_kib: 8000000,
            swap_total_kib: 0,
            swap_used_kib: 0,
            disks: vec![],
            uptime_secs: 3600,
            procs: None,
            os: None,
            kernel: None,
            hostname: None,
        };

        let mut s = view(Overall::Attention);
        s.ssh = SshState::Unavailable(FailKind::Auth);
        s.metrics = Some(metrics.clone());
        let lines = detail_lines(&s, 100);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("CPU:") && (l.contains("stale") || l.contains("last known"))),
            "Metrics must be marked stale when SSH unavailable: {lines:?}"
        );

        let mut s_unk = view(Overall::Unknown);
        s_unk.ssh = SshState::Unknown;
        s_unk.metrics = Some(metrics.clone());
        let lines_unk = detail_lines(&s_unk, 100);
        assert!(
            lines_unk
                .iter()
                .any(|l| l.contains("CPU:") && (l.contains("stale") || l.contains("last known"))),
            "Metrics must be marked stale when SSH is unknown: {lines_unk:?}"
        );

        let mut s_ok = view(Overall::Online);
        s_ok.ssh = SshState::Available;
        s_ok.metrics = Some(metrics);
        let lines_ok = detail_lines(&s_ok, 100);
        assert!(
            lines_ok
                .iter()
                .any(|l| l.contains("CPU:") && !l.contains("stale") && !l.contains("last known")),
            "Metrics must not be marked stale when SSH is available: {lines_ok:?}"
        );
    }

    #[test]
    fn test_refresh_disabled_while_probing() {
        let mut s = view(Overall::Online);
        s.probing = true;
        let snap = Snapshot {
            ts: 100,
            pid: 1,
            local_net_ok: true,
            muted: false,
            servers: vec![s],
        };
        let (_dir, tray) = test_tray(snap);
        let menu = tray.menu();

        let check_all = menu.iter().find(|item| match item {
            MenuItem::Standard(st) => st.label.contains("Check") || st.label.contains("Checking"),
            _ => false,
        });
        assert!(check_all.is_some(), "Check all item must exist");
        if let Some(MenuItem::Standard(st)) = check_all {
            assert!(
                !st.enabled,
                "Check all must be disabled while probing is active"
            );
            assert!(
                st.label.contains("Checking") || st.label.contains("queued"),
                "Label should reflect probing: {}",
                st.label
            );
        }
    }

    #[test]
    fn test_bounded_tooltip_five_plus_overflow_and_separate_ssh_tailscale() {
        let mut servers = Vec::new();
        for i in 0..7 {
            let mut s = view(if i == 0 {
                Overall::Down
            } else {
                Overall::Online
            });
            s.name = format!("server_{:02}", i);
            s.ssh = SshState::Available;
            s.tailscale = TsState::PeerOnline;
            s.detail = format!("<alert & check {}>", i);
            servers.push(s);
        }
        let snap = Snapshot {
            ts: 100,
            pid: 1,
            local_net_ok: true,
            muted: false,
            servers,
        };
        let tt = tooltip_text(&snap);

        assert!(
            !tt.contains('<'),
            "Tooltip must escape HTML/markup tags: {tt}"
        );
        assert!(
            !tt.contains('>'),
            "Tooltip must escape HTML/markup tags: {tt}"
        );
        assert!(
            tt.contains("online"),
            "Tooltip must have counts header: {tt}"
        );
        assert!(
            tt.contains("and 2 more") || tt.contains("+2 more") || tt.contains("2 more"),
            "Tooltip must bound to 5 servers and show overflow count: {tt}"
        );
        assert!(
            tt.contains("SSH") && tt.contains("Tailscale"),
            "Tooltip must show SSH and Tailscale separately: {tt}"
        );
    }

    #[test]
    fn test_no_unicode_ornaments_in_menu_labels() {
        let snap = Snapshot {
            ts: 100,
            pid: 1,
            local_net_ok: false,
            muted: false,
            servers: vec![view(Overall::Down), view(Overall::Online)],
        };
        let (_dir, tray) = test_tray(snap);
        let menu = tray.menu();
        let labels = collect_labels(&menu);

        for l in &labels {
            for ornament in &['●', '▲', '✖', '◐', '…', '○', '⚠'] {
                assert!(
                    !l.contains(*ornament),
                    "Menu label '{l}' contains unicode ornament '{ornament}'"
                );
            }
        }
    }

    #[test]
    fn down_details_show_duration_and_reason() {
        let l = detail_lines(&view(Overall::Down), 100);
        assert!(l[0].contains("unreachable for 1m"), "{l:?}");
        assert!(l.iter().any(|x| x.contains("connection timed out")));
    }

    #[test]
    fn all_refresh_waits_for_every_server() {
        let mut first = view(Overall::Online);
        first.name = "first".into();
        let mut second = first.clone();
        second.name = "second".into();
        let (_, mut tray) = test_tray(Snapshot {
            ts: 100,
            pid: 1,
            local_net_ok: true,
            muted: false,
            servers: vec![first, second],
        });
        let action = tray
            .menu()
            .into_iter()
            .find_map(|it| match it {
                MenuItem::Standard(st) if st.label == "Check all now" => Some(st),
                _ => None,
            })
            .unwrap();
        (action.activate)(&mut tray);
        tray.snap.ts = 101;
        tray.snap.servers[0].last_check = Some(101);
        tray.snap.servers[0].completed_checks += 1;
        assert!(
            tray.is_all_queued(),
            "second server has not completed its requested check"
        );
        tray.reconcile_queued();
        assert!(tray.is_server_queued(&tray.snap.servers[1]));
        tray.snap.servers[1].completed_checks += 1;
        assert!(!tray.is_all_queued());
        assert!(!tray.is_server_queued(&tray.snap.servers[1]));
    }

    #[test]
    fn mutation_error_is_visible_without_blocking() {
        let (_, mut tray) = test_tray(Snapshot {
            ts: 100,
            pid: 1,
            local_net_ok: true,
            muted: false,
            servers: vec![view(Overall::Online)],
        });
        let (tx, rx) = std::sync::mpsc::channel();
        tray.pending_actions.push(rx);
        tx.send(Err("config is read-only".into())).unwrap();
        tray.reconcile_queued();
        assert!(
            collect_labels(&tray.menu())
                .iter()
                .any(|s| s.contains("config is read-only"))
        );
        assert!(tray.pending_actions.is_empty());
    }

    #[test]
    fn large_fleet_preserves_every_server_in_bounded_pages() {
        let servers: Vec<_> = (0..150)
            .map(|i| {
                let mut v = view(Overall::Online);
                v.name = format!("node{i:03}");
                v
            })
            .collect();
        let (_, tray) = test_tray(Snapshot {
            ts: 100,
            pid: 1,
            local_net_ok: true,
            muted: false,
            servers,
        });
        let refs: Vec<_> = tray.snap.servers.iter().collect();
        let pages = tray.build_paged_servers(&refs, 100, "Page");
        fn check(items: &[MenuItem<SvTray>]) {
            assert!(items.len() <= PAGE_LIMIT);
            for item in items {
                if let MenuItem::SubMenu(sub) = item
                    && !sub.submenu.iter().any(
                        |it| matches!(it, MenuItem::Standard(s) if s.label == "Open SSH terminal"),
                    )
                {
                    check(&sub.submenu);
                }
            }
        }
        check(&pages);
        let labels = collect_labels(&pages);
        for v in &tray.snap.servers {
            assert!(
                labels
                    .iter()
                    .any(|l| l.starts_with(&format!("{} -", v.name)))
            );
        }
    }

    #[test]
    fn test_callback_actions_queue_without_blocking_and_show_feedback() {
        let mut s = view(Overall::Online);
        s.name = "srv_cb".into();
        let snap = Snapshot {
            ts: 100,
            pid: 1,
            local_net_ok: true,
            muted: false,
            servers: vec![s],
        };
        // Notice _eng is NOT running a loop; synchronous Handle::set_enabled / remove would block 5s and fail,
        // whereas queueing Cmd with reply None returns immediately!
        let (dir, mut tray) = test_tray(snap);

        // 1. Trigger Check all now -> becomes Check queued and disabled
        let menu = tray.menu();
        let check_all = menu
            .into_iter()
            .find_map(|it| match it {
                MenuItem::Standard(st) if st.label == "Check all now" => Some(st),
                _ => None,
            })
            .unwrap();
        assert!(check_all.enabled);
        (check_all.activate)(&mut tray);

        let menu2 = tray.menu();
        let queued_item = menu2
            .iter()
            .find_map(|it| match it {
                MenuItem::Standard(st) if st.label == "Check queued" => Some(st),
                _ => None,
            })
            .expect("Should show Check queued immediately after clicking Check all now");
        assert!(
            !queued_item.enabled,
            "Check queued must be disabled until probe completes"
        );

        // Advance snapshot -> clears queued state
        tray.snap.ts = 105;
        tray.snap.servers[0].last_check = Some(105);
        tray.snap.servers[0].completed_checks += 1;
        let menu3 = tray.menu();
        assert!(menu3.iter().any(
            |it| matches!(it, MenuItem::Standard(st) if st.label == "Check all now" && st.enabled)
        ));

        // 2. Trigger Manage -> Monitoring enabled & Remove without engine running (must not block)
        let start = std::time::Instant::now();
        let srv_sub = menu3
            .into_iter()
            .find_map(|it| match it {
                MenuItem::SubMenu(sub) if sub.label.contains("srv__cb") => Some(sub),
                _ => None,
            })
            .unwrap();
        let manage_sub = srv_sub
            .submenu
            .into_iter()
            .find_map(|it| match it {
                MenuItem::SubMenu(sub) if sub.label == "Manage" => Some(sub),
                _ => None,
            })
            .unwrap();
        for item in manage_sub.submenu {
            match item {
                MenuItem::Checkmark(chk) if chk.label == "Monitoring enabled" => {
                    (chk.activate)(&mut tray);
                }
                MenuItem::Standard(st) if st.label == "Remove from SVMonitor" => {
                    (st.activate)(&mut tray);
                }
                _ => {}
            }
        }
        assert!(
            start.elapsed() < std::time::Duration::from_millis(500),
            "Callbacks must queue immediately without waiting on engine reply"
        );
        // Snapshot must NOT pretend server was removed before engine publishes
        assert_eq!(tray.snap.servers.len(), 1);

        // 3. Failed launcher surfaces feedback in menu instead of silent ignore
        tray.cfg.general.terminal = "/nonexistent/svmonitor_fake_terminal_bin".into();
        tray.open_ssh("srv_cb");
        assert!(
            tray.feedback.is_some(),
            "Launcher failure must populate tray feedback"
        );
        let menu4 = tray.menu();
        let labels4 = collect_labels(&menu4);
        assert!(
            labels4.iter().any(|l| l.contains("SSH launch failed")),
            "Menu must surface launcher error feedback: {labels4:?}"
        );
        drop(dir);
    }
}
