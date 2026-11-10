//! Command line interface and the daemon entry point (tray or headless).
use crate::config::{self, Config, Paths, Server};
use crate::engine::{Cmd, Engine, Handle};
use crate::health::{Context, Outcome, Tracker};
use crate::model::*;
use crate::probe::{self, ProbeResult, ProbeSpec};
use crate::tray::{self, SvTray};
use crate::util::{fmt_ago, fmt_duration, fmt_kib, fmt_rate, now_unix};
use crate::{autostart, history, ipc, launcher, localnet, sshconfig, tailscale};
use anyhow::{Context as _, Result, bail};
use clap::{Parser, Subcommand};
use std::fs::OpenOptions;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "svmonitor", version, about = "Lightweight SSH server monitor with system tray", long_about = None)]
pub struct Cli {
    /// Use this config file instead of ~/.config/svmonitor/config.toml
    #[arg(long, global = true, value_name = "FILE")]
    pub config: Option<std::path::PathBuf>,
    #[command(subcommand)]
    pub cmd: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run the monitor (system tray when available, otherwise headless). Default command.
    Run {
        /// Never create a tray icon; print events to stdout
        #[arg(long)]
        headless: bool,
        /// Do not send desktop notifications
        #[arg(long)]
        no_notify: bool,
    },
    /// Show state of the running monitor (or probe once if it is not running)
    Status {
        #[arg(long)]
        json: bool,
        /// Keep refreshing
        #[arg(long, short)]
        watch: bool,
    },
    /// Probe servers once right now, without the daemon (read-only)
    Check {
        /// Server names (default: all enabled)
        names: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// List configured servers
    List,
    /// List SSH host aliases found in ~/.ssh/config
    Discover,
    /// Import SSH aliases for monitoring
    Import {
        /// Import every discovered alias without asking
        #[arg(long)]
        all: bool,
        /// Alias names to import
        names: Vec<String>,
    },
    /// Add a server: an SSH alias, or --host for a manual one
    Add {
        name: String,
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        user: Option<String>,
        #[arg(long)]
        port: Option<u16>,
        #[arg(long, value_name = "PATH")]
        identity: Option<String>,
        #[arg(long, value_name = "SECS")]
        interval: Option<u64>,
    },
    /// Remove a server from monitoring (SSH config is untouched)
    Remove { name: String },
    /// Turn monitoring on for a server
    Enable { name: String },
    /// Turn monitoring off for a server
    Disable { name: String },
    /// Show event history
    History {
        server: Option<String>,
        #[arg(long, short, default_value_t = 20)]
        n: usize,
        #[arg(long)]
        json: bool,
    },
    /// Open an interactive SSH session in a terminal
    Ssh { name: String },
    /// Manage start-with-session
    Autostart {
        #[command(subcommand)]
        action: AutostartCmd,
    },
    /// Tell the running monitor to re-read the config
    Reload,
    /// Show where files live
    Paths,
    /// Send a test desktop notification and report whether a tray host is present
    Doctor,
}

#[derive(Subcommand)]
pub enum AutostartCmd {
    Enable,
    Disable,
    Status,
}

fn paths_for(cli: &Cli) -> Paths {
    let mut p = Paths::from_env();
    if let Some(c) = &cli.config {
        p.config = c.clone();
    }
    p
}

pub fn main(cli: Cli) -> Result<i32> {
    let paths = paths_for(&cli);
    match cli.cmd.unwrap_or(Command::Run {
        headless: false,
        no_notify: false,
    }) {
        Command::Run {
            headless,
            no_notify,
        } => {
            run_daemon(paths, headless, no_notify)?;
            Ok(0)
        }
        Command::Status { json, watch } => status(&paths, json, watch),
        Command::Check { names, json } => check(&paths, &names, json),
        Command::List => {
            let cfg = config::load(&paths.config)?;
            if cfg.servers.is_empty() {
                println!(
                    "No servers configured. Try `svmonitor discover` then `svmonitor import <alias>`."
                );
            }
            for s in &cfg.servers {
                println!(
                    "{:<24} {:<4} {}{}",
                    s.name,
                    if s.enabled { "on" } else { "off" },
                    s.destination(),
                    if s.is_manual() {
                        "  (manual)"
                    } else {
                        "  (ssh alias)"
                    }
                );
            }
            Ok(0)
        }
        Command::Discover => {
            let cfg = config::load(&paths.config)?;
            let Some(p) = sshconfig::default_config_path() else {
                bail!("HOME is not set")
            };
            let hosts = sshconfig::discover(&p);
            if hosts.is_empty() {
                println!("No host aliases found in {}", p.display());
            }
            for h in hosts {
                let r = sshconfig::resolve(&cfg.general.ssh_binary, &h);
                let tag = if cfg.server(&h).is_some() {
                    "[monitored]"
                } else {
                    ""
                };
                match r {
                    Some(r) => println!("{:<20} {}@{}:{} {}", h, r.user, r.hostname, r.port, tag),
                    None => println!("{h:<20} (cannot resolve) {tag}"),
                }
            }
            Ok(0)
        }
        Command::Import { all, names } => {
            let mut cfg = config::load(&paths.config)?;
            let Some(p) = sshconfig::default_config_path() else {
                bail!("HOME is not set")
            };
            let found = sshconfig::discover(&p);
            let picks: Vec<String> = if all { found.clone() } else { names };
            if picks.is_empty() {
                bail!("nothing selected: pass alias names or --all (see `svmonitor discover`)");
            }
            let mut n = 0;
            for name in picks {
                if !found.contains(&name) {
                    eprintln!("skip {name}: not an alias in {}", p.display());
                } else if cfg.server(&name).is_some() {
                    eprintln!("skip {name}: already monitored");
                } else {
                    cfg.add(Server::alias(&name))?;
                    println!("added {name}");
                    n += 1;
                }
            }
            if n > 0 {
                config::save(&paths.config, &cfg)?;
                signal_reload(&paths);
            }
            Ok(0)
        }
        Command::Add {
            name,
            host,
            user,
            port,
            identity,
            interval,
        } => {
            let mut cfg = config::load(&paths.config)?;
            let s = Server {
                host,
                user,
                port,
                identity_file: identity,
                interval_secs: interval,
                ..Server::alias(&name)
            };
            cfg.add(s)?;
            config::save(&paths.config, &cfg)?;
            signal_reload(&paths);
            println!("added {name}");
            Ok(0)
        }
        Command::Remove { name } => mutate(
            &paths,
            |c| {
                if c.remove(&name) {
                    Ok(())
                } else {
                    bail!("unknown server {name:?}")
                }
            },
            &format!("removed {name}"),
        ),
        Command::Enable { name } => mutate(
            &paths,
            |c| set_enabled(c, &name, true),
            &format!("{name}: monitoring on"),
        ),
        Command::Disable { name } => mutate(
            &paths,
            |c| set_enabled(c, &name, false),
            &format!("{name}: monitoring off"),
        ),
        Command::History { server, n, json } => {
            let evs = history::read_events(&paths.events(), server.as_deref(), n);
            let now = now_unix();
            for e in evs {
                if json {
                    println!("{}", serde_json::to_string(&e)?);
                } else {
                    println!(
                        "{} {:<16} {:<18} {}",
                        fmt_time(e.ts),
                        e.server,
                        format!("{:?}", e.kind).to_lowercase(),
                        e.message
                    );
                    let _ = now;
                }
            }
            Ok(0)
        }
        Command::Ssh { name } => {
            let cfg = config::load(&paths.config)?;
            let s = cfg
                .server(&name)
                .cloned()
                .unwrap_or_else(|| Server::alias(&name));
            s.validate()?;
            launcher::open_ssh(&cfg.general.terminal, &cfg.general.ssh_binary, &s)?;
            Ok(0)
        }
        Command::Autostart { action } => {
            let p = autostart::autostart_path();
            match action {
                AutostartCmd::Enable => {
                    let exe = std::env::current_exe()?;
                    autostart::enable(&p, exe.to_str().context("non-UTF-8 executable path")?)?;
                    println!("enabled: {}", p.display());
                }
                AutostartCmd::Disable => println!(
                    "{}",
                    if autostart::disable(&p)? {
                        "disabled"
                    } else {
                        "was not enabled"
                    }
                ),
                AutostartCmd::Status => println!(
                    "{}",
                    if autostart::is_enabled(&p) {
                        "enabled"
                    } else {
                        "disabled"
                    }
                ),
            }
            Ok(0)
        }
        Command::Reload => {
            if signal_reload(&paths) {
                println!("reload requested")
            } else {
                println!("monitor is not running")
            }
            Ok(0)
        }
        Command::Doctor => doctor(&paths),
        Command::Paths => {
            println!("config:   {}", paths.config.display());
            println!("history:  {}", paths.events().display());
            println!("samples:  {}", paths.samples().display());
            println!("runtime:  {}", paths.runtime_dir.display());
            println!("autostart:{}", autostart::autostart_path().display());
            Ok(0)
        }
    }
}

fn doctor(paths: &Paths) -> Result<i32> {
    let cfg = config::load(&paths.config)?;
    let mut bad = 0;
    let mut line = |ok: bool, what: String| {
        println!("[{}] {what}", if ok { " ok " } else { "WARN" });
        if !ok {
            bad += 1;
        }
    };
    line(
        launcher::find_in_path(&cfg.general.ssh_binary).is_some(),
        format!("ssh client: {}", cfg.general.ssh_binary),
    );
    line(
        sshconfig::default_config_path().is_some_and(|p| p.exists()),
        "~/.ssh/config present".into(),
    );
    line(
        tray::watcher_present(),
        "tray host (StatusNotifierWatcher) on session bus; GNOME needs the AppIndicator extension"
            .into(),
    );
    let ts = tailscale::fetch(
        std::path::Path::new(tailscale::SOCKET),
        Duration::from_secs(2),
    );
    line(
        matches!(ts, tailscale::Status::Running { .. }),
        "tailscaled reachable and running (optional)".into(),
    );
    line(
        localnet::has_default_route(),
        "default network route".into(),
    );
    line(
        launcher::terminal_prefix(&cfg.general.terminal, &|b| {
            launcher::find_in_path(b).is_some()
        })
        .is_some(),
        "terminal emulator for 'Open SSH'".into(),
    );
    let n = crate::notify::Notifier::spawn();
    n.show(crate::notify::Toast {
        server: "doctor".into(),
        urgency: 0,
        summary: "SVMonitor test notification".into(),
        body: "If you can read this, notifications work.".into(),
        icon: "dialog-information",
    });
    std::thread::sleep(Duration::from_millis(800));
    println!("(a test notification was sent; check your desktop)");
    Ok(i32::from(bad > 0))
}

fn set_enabled(c: &mut Config, name: &str, on: bool) -> Result<()> {
    match c.server_mut(name) {
        Some(s) => {
            s.enabled = on;
            Ok(())
        }
        None => bail!("unknown server {name:?}"),
    }
}

fn mutate(paths: &Paths, f: impl FnOnce(&mut Config) -> Result<()>, msg: &str) -> Result<i32> {
    let mut cfg = config::load(&paths.config)?;
    f(&mut cfg)?;
    config::save(&paths.config, &cfg)?;
    signal_reload(paths);
    println!("{msg}");
    Ok(0)
}

fn fmt_time(ts: i64) -> String {
    // Local time without a tz crate: use libc localtime_r.
    let t: libc::time_t = ts as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: valid pointers to initialised structs.
    unsafe { libc::localtime_r(&t, &mut tm) };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// PID of the running monitor, verified against /proc so a stale state file (crash + PID reuse)
/// can never make us signal an unrelated process.
fn running_pid(paths: &Paths) -> Option<u32> {
    let snap: Snapshot = serde_json::from_slice(&std::fs::read(paths.snapshot()).ok()?).ok()?;
    is_svmonitor(snap.pid).then_some(snap.pid)
}

fn is_svmonitor(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .map(|c| c.trim() == "svmonitor")
        .unwrap_or(false)
}

/// SIGHUP the running monitor, if any.
fn signal_reload(paths: &Paths) -> bool {
    match running_pid(paths) {
        Some(pid) => {
            // SAFETY: plain syscall.
            unsafe { libc::kill(pid as i32, libc::SIGHUP) == 0 }
        }
        None => false,
    }
}

pub fn exit_code_for(servers: &[ServerView]) -> i32 {
    let worst = servers
        .iter()
        .filter(|s| s.enabled)
        .map(|s| s.overall.severity())
        .max()
        .unwrap_or(0);
    match worst {
        0..=1 => 0,
        2..=3 => 1,
        _ => 2,
    }
}

fn status(paths: &Paths, json: bool, watch: bool) -> Result<i32> {
    loop {
        let snap = match (running_pid(paths), std::fs::read(paths.snapshot())) {
            (Some(_), Ok(b)) => serde_json::from_slice::<Snapshot>(&b).context("state file")?,
            _ => {
                if watch {
                    bail!("monitor is not running (start it with `svmonitor run`)");
                }
                eprintln!("(monitor not running; probing once)");
                let cfg = config::load(&paths.config)?;
                Snapshot {
                    ts: now_unix(),
                    pid: 0,
                    local_net_ok: localnet::has_default_route(),
                    muted: false,
                    servers: one_shot_all(&cfg, &[]),
                    command_capable: false,
                }
            }
        };
        if json {
            println!("{}", serde_json::to_string_pretty(&snap)?);
        } else {
            if watch {
                print!("\x1b[2J\x1b[H");
            }
            print_table(&snap);
        }
        if !watch {
            return Ok(exit_code_for(&snap.servers));
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

pub fn print_table(s: &Snapshot) {
    if s.servers.is_empty() {
        println!("No servers configured.");
        return;
    }
    if !s.local_net_ok {
        println!("! this machine has no network route\n");
    }
    println!(
        "{:<18} {:<16} {:<14} {:>5} {:>5} {:>5} {:>10} {:>10} {:>13} DETAIL",
        "SERVER", "STATE", "TAILSCALE", "CPU", "RAM", "DISK", "RX", "TX", "UPTIME"
    );
    for v in &s.servers {
        let (cpu, ram, disk, up) = match (&v.metrics, v.ssh) {
            (Some(m), SshState::Available) => (
                m.cpu_pct
                    .map(|c| format!("{c:.0}%"))
                    .unwrap_or_else(|| "-".into()),
                format!("{:.0}%", m.mem_pct()),
                m.worst_disk()
                    .map(|d| format!("{:.0}%", d.pct()))
                    .unwrap_or_else(|| "-".into()),
                fmt_duration(m.uptime_secs),
            ),
            _ => ("-".into(), "-".into(), "-".into(), "-".into()),
        };
        let (rx, tx) = match (&v.metrics, v.ssh) {
            (Some(Metrics { net: Some(n), .. }), SshState::Available) => {
                (fmt_rate(n.rx_bps), fmt_rate(n.tx_bps))
            }
            _ => ("-".into(), "-".into()),
        };
        let mut state = v.overall.label().to_string();
        if v.pending_failures > 0 {
            state += &format!(" ({}/{})", v.pending_failures, v.failure_threshold);
        }
        let mut detail = v.detail.clone();
        if let (Overall::Down, Some(d)) = (v.overall, v.down_since) {
            detail = format!("down {} — {detail}", fmt_duration((s.ts - d).max(0) as u64));
        }
        if detail.is_empty()
            && let Some(t) = v.last_check
        {
            detail = format!("checked {}", fmt_ago(t, s.ts));
        }
        println!(
            "{:<18} {:<16} {:<14} {:>5} {:>5} {:>5} {:>10} {:>10} {:>13} {}",
            trunc(&v.name, 18),
            trunc(&state, 16),
            trunc(v.tailscale.label(), 14),
            cpu,
            ram,
            disk,
            rx,
            tx,
            up,
            detail
        );
    }
    if let Some(v) = s
        .servers
        .iter()
        .find(|v| v.metrics.is_some() && v.ssh == SshState::Available)
    {
        let m = v.metrics.as_ref().unwrap();
        println!(
            "\n{}: load {:.2} {:.2} {:.2}, RAM {} / {}",
            v.name,
            m.load[0],
            m.load[1],
            m.load[2],
            fmt_kib(m.mem_used_kib),
            fmt_kib(m.mem_total_kib)
        );
    }
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n - 1).collect::<String>())
    }
}

/// One immediate, debounce-free probe of a server (for `check` / `status` without a daemon).
pub fn one_shot(cfg: &Config, s: &Server) -> ServerView {
    let g = &cfg.general;
    let spec = ProbeSpec {
        ssh_bin: g.ssh_binary.clone(),
        server: s.clone(),
        connect_timeout: Duration::from_secs(g.connect_timeout_secs),
        deadline: Duration::from_secs(g.probe_timeout_secs),
        sample_cpu: true,
    };
    let host = s
        .host
        .clone()
        .or_else(|| sshconfig::resolve(&g.ssh_binary, &s.name).map(|r| r.hostname));
    let ts = match (&host, g.check_tailscale) {
        (Some(h), true) => tailscale::classify(
            h,
            &tailscale::fetch(
                std::path::Path::new(tailscale::SOCKET),
                Duration::from_secs(2),
            ),
        ),
        _ => TsState::NotApplicable,
    };
    let mut gg = g.clone();
    gg.failure_threshold = 1;
    let mut t = Tracker::new(&s.name, &s.destination(), s.enabled);
    if !s.enabled {
        return t.view(&gg, false);
    }
    let outcome = if ts.is_local_problem() {
        Outcome::Fail {
            kind: FailKind::Timeout,
            detail: String::new(),
        }
    } else {
        match probe::run(&spec, now_unix()) {
            ProbeResult::Ok(r) => Outcome::Ok(r),
            ProbeResult::Fail { kind, detail } => Outcome::Fail { kind, detail },
        }
    };
    t.apply(
        outcome,
        &Context {
            now: now_unix(),
            ts,
            local_net_ok: localnet::has_default_route(),
        },
        &cfg.thresholds,
        &gg,
    );
    t.view(&gg, false)
}

fn one_shot_all(cfg: &Config, names: &[String]) -> Vec<ServerView> {
    let targets: Vec<&Server> = cfg
        .servers
        .iter()
        .filter(|s| names.is_empty() || names.contains(&s.name))
        .collect();
    std::thread::scope(|sc| {
        let hs: Vec<_> = targets
            .iter()
            .map(|s| sc.spawn(|| one_shot(cfg, s)))
            .collect();
        hs.into_iter().filter_map(|h| h.join().ok()).collect()
    })
}

fn check(paths: &Paths, names: &[String], json: bool) -> Result<i32> {
    let cfg = config::load(&paths.config)?;
    for n in names {
        if cfg.server(n).is_none() {
            bail!("unknown server {n:?}; see `svmonitor list`");
        }
    }
    if cfg.servers.is_empty() {
        bail!("no servers configured");
    }
    let views = one_shot_all(&cfg, names);
    let snap = Snapshot {
        ts: now_unix(),
        pid: 0,
        local_net_ok: localnet::has_default_route(),
        muted: false,
        servers: views,
        command_capable: false,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&snap)?);
    } else {
        print_table(&snap);
    }
    Ok(exit_code_for(&snap.servers))
}

/// Exclusive per-user lock; held for process lifetime.
fn acquire_lock(paths: &Paths) -> Result<std::fs::File> {
    crate::util::ensure_private_dir(&paths.runtime_dir)?;
    let f = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(paths.lock())?;
    // SAFETY: fd is valid for the lifetime of `f`.
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        bail!(
            "svmonitor is already running (lock {})",
            paths.lock().display()
        );
    }
    Ok(f)
}

struct SignalMaskGuard {
    old_set: libc::sigset_t,
}

impl SignalMaskGuard {
    fn block() -> Result<(Self, libc::sigset_t)> {
        // Block SIGHUP, SIGINT, SIGTERM in the calling thread before any other threads
        // are spawned. Future threads inherit this mask so process-wide signals cannot kill them.
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            for s in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGUSR1] {
                libc::sigaddset(&mut set, s);
            }
            let mut old_set: libc::sigset_t = std::mem::zeroed();
            if libc::pthread_sigmask(libc::SIG_BLOCK, &set, &mut old_set) != 0 {
                bail!("failed to block signals");
            }
            Ok((Self { old_set }, set))
        }
    }
}

impl Drop for SignalMaskGuard {
    fn drop(&mut self) {
        // Restore the original signal mask of the calling thread on exit.
        unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, &self.old_set, std::ptr::null_mut());
        }
    }
}

fn install_signals(handle: Handle, set: libc::sigset_t, paths: Paths) -> Result<()> {
    // Signals were blocked before any threads were spawned; consume in a dedicated one.
    std::thread::Builder::new()
        .name("svm-signal".into())
        .stack_size(64 * 1024)
        .spawn(move || {
            loop {
                let mut sig = 0;
                // SAFETY: set is a local sigset_t passed by value.
                if unsafe { libc::sigwait(&set, &mut sig) } != 0 {
                    continue;
                }
                match sig {
                    libc::SIGHUP => handle.send(Cmd::Reload),
                    // A UI helper queued commands in the spool and woke us; drain and forward.
                    libc::SIGUSR1 => {
                        for _ in 0..16 {
                            let cmds = ipc::drain(&paths);
                            if cmds.is_empty() {
                                break;
                            }
                            for c in cmds {
                                handle.send(c.into_cmd());
                            }
                        }
                    }
                    _ => {
                        handle.shutdown();
                        return;
                    }
                }
            }
        })?;
    Ok(())
}

fn run_daemon(paths: Paths, headless: bool, no_notify: bool) -> Result<()> {
    // Block signals BEFORE spawning any threads (Engine, Notifier, Tray, etc.)
    let (_sig_guard, sig_set) = SignalMaskGuard::block()?;
    let _lock = acquire_lock(&paths)?;
    ipc::purge(&paths);
    let cfg = config::load(&paths.config)?;
    let (mut engine, handle) = Engine::new(
        cfg.clone(),
        Paths {
            config: paths.config.clone(),
            state_dir: paths.state_dir.clone(),
            runtime_dir: paths.runtime_dir.clone(),
        },
        !no_notify,
    );
    install_signals(handle.clone(), sig_set, paths.clone())?;
    let mut tray_handle = None;
    if !headless {
        use ksni::blocking::TrayMethods;
        match SvTray::new(handle.clone(), &paths, cfg.clone())
            .assume_sni_available(true)
            .spawn()
        {
            Ok(h) => {
                let h = std::sync::Arc::new(h);
                let th = h.clone();
                engine.add_sink(Box::new(move |s: &Snapshot| {
                    let s = s.clone();
                    th.update(move |t| t.snap = s);
                }));
                tray_handle = Some(h);
                if tray::watcher_present() {
                    eprintln!("svmonitor: tray started ({} servers)", cfg.servers.len());
                } else {
                    eprintln!(
                        "svmonitor: tray registered but no StatusNotifierWatcher is running, so no icon is visible yet.\n  GNOME: install/enable the AppIndicator extension (Arch: gnome-shell-extension-appindicator, Debian: gnome-shell-extension-appindicator).\n  The monitor keeps working (notifications, `svmonitor status`) and the icon appears automatically once a tray host starts."
                    );
                }
            }
            Err(e) => eprintln!(
                "svmonitor: no system tray ({e}); running headless. Use `svmonitor status` or run with --headless."
            ),
        }
    }
    if tray_handle.is_none() {
        engine.add_event_sink(Box::new(|e: &Event| {
            println!(
                "{} {} [{:?}] {}",
                fmt_time(e.ts),
                e.server,
                e.kind,
                e.message
            );
        }));
    }
    if cfg.servers.is_empty() {
        eprintln!(
            "svmonitor: no servers configured. Run `svmonitor discover` and `svmonitor import <alias>`."
        );
    }
    engine.run();
    if let Some(h) = tray_handle {
        h.shutdown().wait();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(o: Overall, en: bool) -> ServerView {
        ServerView {
            name: "a".into(),
            target: "a".into(),
            enabled: en,
            overall: o,
            ssh: SshState::Unknown,
            tailscale: TsState::NotApplicable,
            metrics: None,
            last_check: None,
            completed_checks: 0,
            last_ok: None,
            down_since: None,
            detail: String::new(),
            pending_failures: 0,
            failure_threshold: 3,
            active_alerts: vec![],
            probing: false,
        }
    }

    #[test]
    fn exit_codes() {
        assert_eq!(exit_code_for(&[]), 0);
        assert_eq!(exit_code_for(&[v(Overall::Online, true)]), 0);
        assert_eq!(exit_code_for(&[v(Overall::Warning, true)]), 1);
        assert_eq!(exit_code_for(&[v(Overall::LocalIssue, true)]), 1);
        assert_eq!(
            exit_code_for(&[v(Overall::Online, true), v(Overall::Down, true)]),
            2
        );
        assert_eq!(
            exit_code_for(&[v(Overall::Down, false)]),
            0,
            "disabled servers do not count"
        );
    }

    #[test]
    fn stale_pid_is_not_a_monitor() {
        assert!(!is_svmonitor(0));
        assert!(
            !is_svmonitor(std::process::id()),
            "test binary has a different comm"
        );
        assert!(!is_svmonitor(u32::MAX - 1));
    }

    #[test]
    fn truncation() {
        assert_eq!(trunc("abcdef", 4), "abc…");
        assert_eq!(trunc("abc", 4), "abc");
    }
}
