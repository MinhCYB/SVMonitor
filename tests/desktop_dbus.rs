//! Desktop integration on a private D-Bus session: a fake StatusNotifierWatcher + Notifications
//! server stand in for the desktop. Exercises the real tray (SNI + dbusmenu) and notifier code.
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use svmonitor::config::{Config, Paths, Server};
use svmonitor::engine::Engine;
use svmonitor::model::*;
use svmonitor::notify::{Notifier, toast_for};
use svmonitor::tray::SvTray;
use zbus::blocking::{Connection, connection::Builder};
use zbus::zvariant::{OwnedValue, Value};

struct Bus(Child);
impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_bus() -> Option<Bus> {
    let mut c = Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address=1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut line = String::new();
    BufReader::new(c.stdout.take()?).read_line(&mut line).ok()?;
    // SAFETY: this test file contains a single #[test], so no concurrent env access.
    unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", line.trim()) };
    Some(Bus(c))
}

struct Watcher {
    items: Arc<Mutex<Vec<String>>>,
}

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    fn register_status_notifier_item(
        &self,
        service: &str,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
    ) {
        let sender = hdr.sender().map(|s| s.to_string()).unwrap_or_default();
        let full = if service.starts_with('/') {
            format!("{sender}{service}")
        } else {
            service.to_string()
        };
        self.items.lock().unwrap().push(full);
    }
    fn register_status_notifier_host(&self, _s: &str) {}
    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.items.lock().unwrap().clone()
    }
    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }
    #[zbus(signal)]
    async fn status_notifier_item_registered(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        s: &str,
    ) -> zbus::Result<()>;
}

type Seen = Arc<Mutex<Vec<(u32, String, String, u8)>>>;

struct Notifications {
    seen: Seen,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Notifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        _app: &str,
        replaces: u32,
        _icon: &str,
        summary: &str,
        body: &str,
        _actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        _t: i32,
    ) -> u32 {
        let urgency = hints
            .get("urgency")
            .and_then(|v| u8::try_from(v.clone()).ok())
            .unwrap_or(255);
        self.seen
            .lock()
            .unwrap()
            .push((replaces, summary.into(), body.into(), urgency));
        if replaces != 0 { replaces } else { 42 }
    }
    fn close_notification(&self, _id: u32) {}
    fn get_capabilities(&self) -> Vec<String> {
        vec!["body".into()]
    }
}

fn wait_for<T>(secs: u64, mut f: impl FnMut() -> Option<T>) -> T {
    let end = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < end, "timeout");
        std::thread::sleep(Duration::from_millis(50));
    }
}

type Layout = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

fn label_of(props: &HashMap<String, OwnedValue>) -> String {
    props
        .get("label")
        .and_then(|v| String::try_from(v.clone()).ok())
        .unwrap_or_default()
}

fn flatten(l: &Layout, out: &mut Vec<(i32, String, bool)>) {
    let enabled =
        l.1.get("enabled")
            .and_then(|v| bool::try_from(v.clone()).ok())
            .unwrap_or(true);
    out.push((l.0, label_of(&l.1), enabled));
    for c in &l.2 {
        let v = Value::from(c.clone());
        if let Ok(child) = <Layout>::try_from(v) {
            flatten(&child, out);
        }
    }
}

#[test]
fn tray_and_notifications_over_dbus() {
    let Some(_bus) = start_bus() else {
        eprintln!("dbus-daemon not available; skipping");
        return;
    };
    // Fake desktop services.
    let items = Arc::new(Mutex::new(Vec::new()));
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let desktop = Builder::session()
        .unwrap()
        .name("org.kde.StatusNotifierWatcher")
        .unwrap()
        .name("org.freedesktop.Notifications")
        .unwrap()
        .serve_at(
            "/StatusNotifierWatcher",
            Watcher {
                items: items.clone(),
            },
        )
        .unwrap()
        .serve_at(
            "/org/freedesktop/Notifications",
            Notifications { seen: seen.clone() },
        )
        .unwrap()
        .build()
        .unwrap();

    // Real engine (no servers needed) + real tray.
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths {
        config: dir.path().join("config.toml"),
        state_dir: dir.path().join("state"),
        runtime_dir: dir.path().join("run"),
    };
    let mut cfg = Config::default();
    cfg.add(Server {
        host: Some("x.invalid".into()),
        enabled: false,
        ..Server::alias("demo")
    })
    .unwrap();
    svmonitor::config::save(&paths.config, &cfg).unwrap();
    let (mut engine, handle) = Engine::new(
        cfg.clone(),
        Paths {
            config: paths.config.clone(),
            state_dir: paths.state_dir.clone(),
            runtime_dir: paths.runtime_dir.clone(),
        },
        false,
    );
    use ksni::blocking::TrayMethods;
    let tray = SvTray::new(handle.clone(), &paths, cfg)
        .assume_sni_available(true)
        .spawn()
        .expect("tray spawn on bus with watcher");
    let th = tray.clone();
    let (published, initial) = std::sync::mpsc::channel();
    engine.add_sink(Box::new(move |s| {
        let s = s.clone();
        th.update(move |t| t.snap = s);
        let _ = published.send(());
    }));
    let eng_thread = std::thread::spawn(move || engine.run());
    initial.recv_timeout(Duration::from_secs(5)).unwrap();
    let registered = wait_for(5, || items.lock().unwrap().first().cloned());
    assert!(
        registered.starts_with("org.kde.StatusNotifierItem-"),
        "{registered}"
    );

    // Push a snapshot with a Down server; check item properties via D-Bus like a tray host would.
    let snap = Snapshot {
        ts: 1000,
        pid: 1,
        local_net_ok: true,
        command_capable: true,
        muted: false,
        servers: vec![ServerView {
            name: "demo".into(),
            target: "demo".into(),
            enabled: false,
            overall: Overall::Down,
            ssh: SshState::Unavailable(FailKind::Timeout),
            tailscale: TsState::PeerOffline,
            metrics: None,
            last_check: Some(990),
            completed_checks: 0,
            last_ok: None,
            down_since: Some(900),
            detail: "connection timed out".into(),
            pending_failures: 0,
            failure_threshold: 3,
            active_alerts: vec![],
            probing: false,
        }],
    };
    tray.update(|t| t.snap = snap);

    let client = Connection::session().unwrap();
    let (svc, path) = (registered.clone(), "/StatusNotifierItem".to_string());
    let prop = |name: &str| -> OwnedValue {
        let r = client
            .call_method(
                Some(svc.as_str()),
                path.as_str(),
                Some("org.freedesktop.DBus.Properties"),
                "Get",
                &("org.kde.StatusNotifierItem", name),
            )
            .unwrap();
        let v: OwnedValue = r.body().deserialize().unwrap();
        v
    };
    assert_eq!(String::try_from(prop("Status")).unwrap(), "NeedsAttention");
    assert_eq!(String::try_from(prop("Id")).unwrap(), "svmonitor");
    let menu_path = zbus::zvariant::OwnedObjectPath::try_from(prop("Menu")).unwrap();

    // Fetch the dbusmenu layout.
    let layout = |c: &Connection| -> Layout {
        let r = c
            .call_method(
                Some(svc.as_str()),
                menu_path.as_str(),
                Some("com.canonical.dbusmenu"),
                "GetLayout",
                &(0i32, -1i32, Vec::<&str>::new()),
            )
            .unwrap();
        let (_rev, l): (u32, Layout) = r.body().deserialize().unwrap();
        l
    };
    let _ = client.call_method(
        Some(svc.as_str()),
        menu_path.as_str(),
        Some("com.canonical.dbusmenu"),
        "AboutToShow",
        &(0i32,),
    );
    let mut flat = Vec::new();
    flatten(&layout(&client), &mut flat);
    let labels: Vec<&str> = flat.iter().map(|(_, l, _)| l.as_str()).collect();
    assert!(
        labels
            .iter()
            .any(|l| l.contains("demo") && l.contains("unreachable")),
        "{labels:?}"
    );
    assert!(labels.contains(&"Quit"), "{labels:?}");
    assert!(labels.contains(&"Add from SSH config"), "{labels:?}");
    assert!(
        labels.iter().any(|l| l.contains("Tailscale: peer offline")),
        "{labels:?}"
    );
    assert!(
        labels.iter().any(|l| l.contains("Open SSH terminal")),
        "{labels:?}"
    );
    assert!(
        labels.iter().any(|l| l.contains("Manage")),
        "Manage submenu must exist: {labels:?}"
    );
    assert!(
        !labels.iter().any(|l| l.contains('●') || l.contains('✖')),
        "Labels must not contain unicode status ornaments: {labels:?}"
    );
    // Informational rows disabled, actions enabled.
    let status_row = flat
        .iter()
        .find(|(_, l, _)| l.starts_with("Status:"))
        .unwrap();
    assert!(!status_row.2);
    let _ = &desktop;

    // Clicking "Monitoring enabled" really toggles the engine (demo starts disabled in engine).
    let toggle = flat
        .iter()
        .find(|(_, l, _)| l == "Monitoring enabled")
        .unwrap()
        .0;
    let before = handle.snapshot().unwrap().servers[0].enabled;
    assert!(!before);
    client
        .call_method(
            Some(svc.as_str()),
            menu_path.as_str(),
            Some("com.canonical.dbusmenu"),
            "Event",
            &(toggle, "clicked", Value::from(0i32), 0u32),
        )
        .unwrap();
    wait_for(5, || handle.snapshot().filter(|s| s.servers[0].enabled));

    // Notifications: replace-id per server, urgency mapping.
    let notifier = Notifier::spawn();
    let down = Event {
        ts: 1,
        server: "demo".into(),
        kind: EventKind::Down,
        message: "connection timed out".into(),
        key: "down".into(),
        notify: true,
    };
    let rec = Event {
        ts: 2,
        server: "demo".into(),
        kind: EventKind::Recovered,
        message: "back online after 1m".into(),
        key: "down".into(),
        notify: true,
    };
    notifier.show(toast_for(&down));
    wait_for(5, || (seen.lock().unwrap().len() == 1).then_some(()));
    notifier.show(toast_for(&rec));
    wait_for(5, || (seen.lock().unwrap().len() == 2).then_some(()));
    let s = seen.lock().unwrap().clone();
    assert_eq!(s[0].0, 0);
    assert_eq!(s[0].3, 2, "critical urgency for down");
    assert!(s[0].1.contains("demo is unreachable"));
    assert_eq!(s[1].0, 42, "second toast replaces the first");
    assert!(s[1].1.contains("back online"));

    // Quit item stops the engine.
    flat.clear();
    flatten(&layout(&client), &mut flat);
    let quit = flat.iter().find(|(_, l, _)| l == "Quit").unwrap().0;
    client
        .call_method(
            Some(svc.as_str()),
            menu_path.as_str(),
            Some("com.canonical.dbusmenu"),
            "Event",
            &(quit, "clicked", Value::from(0i32), 0u32),
        )
        .unwrap();
    eng_thread.join().unwrap();
    tray.shutdown().wait();
}
