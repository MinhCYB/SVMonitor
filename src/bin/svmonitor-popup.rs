//! On-demand GTK4 dashboard. Closing the window terminates this process.
use gtk4::{self as gtk, gio, glib, prelude::*};
use std::{cell::RefCell, collections::HashSet, rc::Rc, time::Duration};
use svmonitor::{
    config::{self, Paths, Server},
    dashboard::{self, Card},
    ipc::{self, UiCommand},
    launcher,
    model::{Snapshot, SshState},
    util,
};

const CSS: &str = r#"
window { border-radius: 16px; }
.dashboard { padding: 16px; }
.heading { font-size: 19px; font-weight: 700; }
.summary { font-weight: 600; }
.dim { opacity: 0.66; }
.small { font-size: 0.92em; }
.card { border: 1px solid alpha(@theme_fg_color, 0.14); border-radius: 12px; padding: 14px; }
.name { font-size: 1.12em; font-weight: 700; }
.badge { border-radius: 8px; padding: 3px 8px; font-size: 0.9em; }
.ok.badge { color: #16834b; background: alpha(#26a269, 0.14); }
.warn.badge { color: #b07800; background: alpha(#e5a50a, 0.15); }
.crit.badge { color: #d33b3b; background: alpha(#e33b3b, 0.13); }
.idle.badge { background: alpha(@theme_fg_color, 0.08); }
progressbar trough { min-height: 5px; border-radius: 4px; background: alpha(@theme_fg_color, 0.1); }
progressbar progress { min-height: 5px; border-radius: 4px; background: #26a269; }
progressbar.warn progress { background: #e5a50a; }
progressbar.crit progress { background: #e33b3b; }
progressbar.idle progress { background: alpha(@theme_fg_color, 0.3); }
.stale { opacity: 0.55; }
.banner { border-radius: 8px; padding: 8px; background: alpha(#e5a50a, 0.14); }
button { border-radius: 8px; }
"#;

fn label(text: &str, class: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_xalign(0.0);
    l.set_wrap(true);
    l.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    for c in class.split_whitespace() {
        l.add_css_class(c);
    }
    l
}
fn row() -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Horizontal, 8)
}
fn column(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, spacing)
}
fn button(text: &str, tip: &str, f: impl Fn() + 'static) -> gtk::Button {
    let b = gtk::Button::with_label(text);
    b.set_tooltip_text(Some(tip));
    b.connect_clicked(move |_| f());
    b
}
fn feedback(l: &gtk::Label, result: Result<(), String>) {
    match result {
        Ok(()) => l.set_text("Request sent"),
        Err(e) => l.set_text(&e),
    }
    l.set_visible(true);
}
fn send(paths: &Paths, cmd: UiCommand, message: &gtk::Label) {
    feedback(message, ipc::send(paths, &cmd).map_err(|e| e.to_string()));
}
fn ssh(paths: &Paths, name: &str) -> Result<(), String> {
    let cfg = config::load(&paths.config).map_err(|e| e.to_string())?;
    let server = cfg
        .servers
        .iter()
        .find(|s| s.name == name)
        .cloned()
        .ok_or("Server no longer configured")?;
    launcher::open_ssh(&cfg.general.terminal, &cfg.general.ssh_binary, &server)
        .map_err(|e| e.to_string())
}

fn server_card(
    c: Card,
    paths: &Paths,
    message: &gtk::Label,
    expanded: Rc<RefCell<HashSet<String>>>,
    running: bool,
) -> gtk::Box {
    let box_ = column(9);
    box_.add_css_class("card");
    let heading = row();
    let name = label(&c.name, "name");
    name.set_hexpand(true);
    heading.append(&name);
    let state = label(&c.state, &format!("badge {}", c.level.class()));
    heading.append(&state);
    box_.append(&heading);
    box_.append(&label(&c.target, "dim small"));
    box_.append(&label(&c.subtitle, "dim small"));
    let meters = column(8);
    if c.stale {
        meters.add_css_class("stale");
        box_.append(&label("Last known values · stale", "small"));
    }
    for b in c.bars {
        let head = row();
        let title = label(&b.label, "small");
        title.set_hexpand(true);
        head.append(&title);
        let value = label(&b.text, "small");
        value.set_xalign(1.0);
        head.append(&value);
        meters.append(&head);
        let p = gtk::ProgressBar::new();
        p.set_fraction(b.frac);
        p.add_css_class(b.level.class());
        meters.append(&p);
    }
    box_.append(&meters);
    if let Some(net) = c.net {
        box_.append(&label(&net, "small"));
    }
    let actions = row();
    let p = paths.clone();
    let n = c.name.clone();
    let msg = message.clone();
    actions.append(&button("SSH", "Open SSH in your terminal", move || {
        feedback(&msg, ssh(&p, &n))
    }));
    let p = paths.clone();
    let n = c.name.clone();
    let msg = message.clone();
    let check = button("Check", "Check this server now", move || {
        send(
            &p,
            UiCommand::CheckNow {
                server: Some(n.clone()),
            },
            &msg,
        )
    });
    check.set_sensitive(running && c.enabled);
    actions.append(&check);
    let p = paths.clone();
    let n = c.name.clone();
    let msg = message.clone();
    let enabled = c.enabled;
    let toggle = button(
        if enabled { "Pause" } else { "Resume" },
        "Toggle monitoring for this server",
        move || {
            send(
                &p,
                UiCommand::SetEnabled {
                    server: n.clone(),
                    enabled: !enabled,
                },
                &msg,
            )
        },
    );
    toggle.set_sensitive(running);
    actions.append(&toggle);
    let details = gtk::Expander::new(Some("Details"));
    details.set_hexpand(true);
    details.set_expanded(expanded.borrow().contains(&c.name));
    let inner = column(6);
    inner.append(&label(
        &format!("SSH: {}\nTailscale: {}", c.ssh, c.tailscale),
        "small",
    ));
    for (k, v) in c.facts {
        inner.append(&label(&format!("{k}: {v}"), "small dim"));
    }
    let p = paths.clone();
    let n = c.name.clone();
    let msg = message.clone();
    let remove = button(
        "Remove from monitoring",
        "SSH config is never changed",
        move || send(&p, UiCommand::Remove { server: n.clone() }, &msg),
    );
    remove.set_sensitive(running);
    inner.append(&remove);
    details.set_child(Some(&inner));
    let name = c.name;
    details.connect_expanded_notify(move |e| {
        if e.is_expanded() {
            expanded.borrow_mut().insert(name.clone());
        } else {
            expanded.borrow_mut().remove(&name);
        }
    });
    box_.append(&actions);
    box_.append(&details);
    box_
}

struct Ui {
    paths: Paths,
    summary: gtk::Label,
    notes: gtk::Label,
    list: gtk::Box,
    message: gtk::Label,
    scroll: gtk::ScrolledWindow,
    mute: gtk::Button,
    check: gtk::Button,
    add: gtk::Button,
    refreshed_at: i64,
    last: Option<(Snapshot, bool)>,
    expanded: Rc<RefCell<HashSet<String>>>,
}
impl Ui {
    fn refresh(&mut self) {
        let running = ipc::running_pid(&self.paths).is_some();
        self.check.set_sensitive(running);
        self.mute.set_sensitive(running);
        self.add.set_sensitive(false);
        let snap = std::fs::read(self.paths.snapshot())
            .ok()
            .and_then(|s| serde_json::from_slice::<Snapshot>(&s).ok());
        let Some(mut snap) = snap else {
            self.summary.set_text("No monitoring state available");
            self.notes.set_text("Start SVMonitor, then add a server below or import SSH aliases from the tray menu.");
            self.notes.set_visible(true);
            while let Some(w) = self.list.first_child() {
                self.list.remove(&w);
            }
            self.last = None;
            return;
        };
        let capable = running && snap.command_capable;
        self.check.set_sensitive(capable);
        self.mute.set_sensitive(capable);
        self.add.set_sensitive(capable);
        if self.last.as_ref().is_some_and(|(last, r)| {
            last.servers == snap.servers
                && last.muted == snap.muted
                && last.local_net_ok == snap.local_net_ok
                && *r == running
                && util::now_unix() - self.refreshed_at < 30
        }) {
            return;
        }
        self.last = Some((snap.clone(), running));
        self.refreshed_at = util::now_unix();
        let mut h = dashboard::header(&snap, running);
        if running && !snap.command_capable {
            h.notes
                .push("Restart SVMonitor after upgrading to enable dashboard actions".into());
        }
        self.summary.set_text(&h.title);
        self.notes.set_text(&h.notes.join("\n"));
        self.notes.set_visible(!h.notes.is_empty());
        self.mute
            .set_label(if snap.muted { "Unmute" } else { "Mute 1h" });
        if !running {
            for v in &mut snap.servers {
                v.ssh = SshState::Unknown;
            }
        }
        let position = self.scroll.vadjustment().value();
        while let Some(w) = self.list.first_child() {
            self.list.remove(&w);
        }
        if snap.servers.is_empty() {
            self.list.append(&label(
                "Choose an existing SSH alias below. Your SSH keys and config stay untouched.",
                "dim",
            ));
        }
        for c in dashboard::cards(&snap, util::now_unix()) {
            self.list.append(&server_card(
                c,
                &self.paths,
                &self.message,
                self.expanded.clone(),
                capable,
            ));
        }
        let adj = self.scroll.vadjustment();
        glib::idle_add_local_once(move || {
            adj.set_value(position.min((adj.upper() - adj.page_size()).max(0.0)))
        });
    }
}
fn build(app: &gtk::Application, paths: Paths) {
    if let Some(w) = app.active_window() {
        w.present();
        return;
    }
    if let Some(settings) = gtk::Settings::default()
        && let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)
        && let Ok(value) = bus.call_sync(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Settings",
            "Read",
            Some(&("org.freedesktop.appearance", "color-scheme").to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            500,
            gio::Cancellable::NONE,
        )
        && let Some(scheme) = value
            .child_value(0)
            .as_variant()
            .and_then(|v| v.get::<u32>())
    {
        settings.set_gtk_application_prefer_dark_theme(scheme == 1);
    }
    let provider = gtk::CssProvider::new();
    provider.load_from_data(CSS);
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().expect("GTK display"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    let win = gtk::ApplicationWindow::builder()
        .application(app)
        .title("SVMonitor Dashboard")
        .default_width(440)
        .default_height(650)
        .build();
    win.set_resizable(false);
    win.set_decorated(false);
    let root = column(12);
    root.add_css_class("dashboard");
    let head = row();
    let title = label("SVMonitor", "heading");
    title.set_hexpand(true);
    head.append(&title);
    let w = win.downgrade();
    head.append(&button("Close", "Close dashboard (Escape)", move || {
        if let Some(w) = w.upgrade() {
            w.close();
        }
    }));
    root.append(&head);
    let summary = label("Loading…", "summary");
    root.append(&summary);
    let notes = label("", "banner small");
    notes.set_visible(false);
    root.append(&notes);
    let message = label("", "small");
    message.set_visible(false);
    let list = column(12);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&list)
        .build();
    root.append(&scroll);
    let actions = row();
    let p = paths.clone();
    let msg = message.clone();
    let check = button("Check all", "Check all enabled servers", move || {
        send(&p, UiCommand::CheckNow { server: None }, &msg)
    });
    actions.append(&check);
    let p = paths.clone();
    let msg = message.clone();
    let mute = button(
        "Mute 1h",
        "Mute notifications for one hour, or unmute",
        move || {
            let muted = std::fs::read(p.snapshot())
                .ok()
                .and_then(|b| serde_json::from_slice::<Snapshot>(&b).ok())
                .is_some_and(|s| s.muted);
            send(
                &p,
                UiCommand::Mute {
                    secs: if muted { None } else { Some(3600) },
                },
                &msg,
            );
        },
    );
    actions.append(&mute);
    let p = paths.clone();
    let msg = message.clone();
    actions.append(&button("History", "Open event history", move || {
        feedback(
            &msg,
            launcher::open_path(&p.events()).map_err(|e| e.to_string()),
        )
    }));
    let p = paths.clone();
    let msg = message.clone();
    actions.append(&button("Config", "Open configuration file", move || {
        feedback(
            &msg,
            launcher::open_path(&p.config).map_err(|e| e.to_string()),
        )
    }));
    root.append(&actions);
    let add = row();
    let entry = gtk::Entry::builder()
        .placeholder_text("SSH alias to monitor")
        .hexpand(true)
        .build();
    add.append(&entry);
    let p = paths.clone();
    let msg = message.clone();
    let e = entry.clone();
    let add_button = button("Add", "Monitor an existing SSH alias", move || {
        let name = e.text().trim().to_string();
        match Server::alias(&name).validate() {
            Ok(()) => send(&p, UiCommand::Add { server: name }, &msg),
            Err(err) => feedback(&msg, Err(err.to_string())),
        }
    });
    add.append(&add_button);
    root.append(&add);
    root.append(&message);
    let ui = Rc::new(RefCell::new(Ui {
        paths,
        summary,
        notes,
        list,
        message,
        scroll,
        mute,
        check,
        add: add_button,
        refreshed_at: 0,
        last: None,
        expanded: Rc::new(RefCell::new(HashSet::new())),
    }));
    ui.borrow_mut().refresh();
    let weak = win.downgrade();
    glib::timeout_add_local(Duration::from_secs(2), move || {
        if weak.upgrade().is_none() {
            return glib::ControlFlow::Break;
        }
        ui.borrow_mut().refresh();
        glib::ControlFlow::Continue
    });
    let keys = gtk::EventControllerKey::new();
    let w = win.downgrade();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape {
            if let Some(w) = w.upgrade() {
                w.close();
            }
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    win.add_controller(keys);
    win.set_child(Some(&root));
    win.present();
}
fn main() -> glib::ExitCode {
    let mut paths = Paths::from_env();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--config" => {
                let Some(p) = args.next() else {
                    eprintln!("--config requires a path");
                    return glib::ExitCode::FAILURE;
                };
                paths.config = p.into();
            }
            "--help" | "-h" => {
                println!(
                    "Usage: svmonitor-popup [--config PATH]\nShows cached monitoring state; opens no SSH connection until Check is requested."
                );
                return glib::ExitCode::SUCCESS;
            }
            _ => {
                eprintln!("Unknown argument: {a}");
                return glib::ExitCode::FAILURE;
            }
        }
    }
    // Before GTK or any other threads exist. Cairo avoids allocating a GL renderer for static cards.
    if std::env::var_os("GSK_RENDERER").is_none() {
        unsafe {
            std::env::set_var("GSK_RENDERER", "cairo");
        }
    }
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    paths.runtime_dir.hash(&mut hash);
    paths.config.hash(&mut hash);
    let standard = Paths::from_env();
    let id = if paths.config == standard.config && paths.runtime_dir == standard.runtime_dir {
        "io.github.MinhCYB.SVMonitor.Popup".to_string()
    } else {
        format!("io.github.MinhCYB.SVMonitor.Popup.i{:x}", hash.finish())
    };
    let app = gtk::Application::builder()
        .application_id(id)
        .flags(gio::ApplicationFlags::empty())
        .build();
    app.connect_activate(move |app| build(app, paths.clone()));
    app.run_with_args::<&str>(&[])
}
