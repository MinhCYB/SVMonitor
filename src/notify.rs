//! Desktop notifications over org.freedesktop.Notifications with rate limiting.
//! The policy (`Limiter`) is pure; delivery runs on its own thread so D-Bus can never stall probing.
use crate::model::{Event, EventKind};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

/// Decides whether an event deserves a notification.
#[derive(Debug, Default)]
pub struct Limiter {
    last: HashMap<(String, String), i64>,
    /// (server, key) pairs for which a "bad" notice was shown and not yet closed out by a recovery.
    announced: HashSet<(String, String)>,
}

impl Limiter {
    pub fn allow(&mut self, e: &Event, cooldown_secs: u64) -> bool {
        if !e.notify {
            return false;
        }
        let k = (e.server.clone(), e.key.clone());
        match e.kind {
            // Closing events: only meaningful if the user was told about the problem.
            EventKind::Recovered | EventKind::AttentionCleared => self.announced.remove(&k),
            EventKind::Rebooted
            | EventKind::Down
            | EventKind::Attention
            | EventKind::ThresholdBreach => {
                if let Some(prev) = self.last.get(&k)
                    && e.ts - prev < cooldown_secs as i64
                {
                    return false;
                }
                self.last.insert(k.clone(), e.ts);
                if e.kind != EventKind::Rebooted {
                    self.announced.insert(k);
                }
                true
            }
            _ => false,
        }
    }
    /// A server was removed or disabled; forget its state.
    pub fn forget(&mut self, server: &str) {
        self.last.retain(|(s, _), _| s != server);
        self.announced.retain(|(s, _)| s != server);
    }
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub server: String,
    pub urgency: u8, // 0 low, 1 normal, 2 critical
    pub summary: String,
    pub body: String,
    pub icon: &'static str,
}

pub fn toast_for(e: &Event) -> Toast {
    let (urgency, icon, title) = match e.kind {
        EventKind::Down => (2, "network-error", format!("{} is unreachable", e.server)),
        EventKind::Recovered => (
            1,
            "network-transmit-receive",
            format!("{} is back online", e.server),
        ),
        EventKind::Attention => (1, "dialog-warning", format!("{} needs attention", e.server)),
        EventKind::AttentionCleared => (
            0,
            "emblem-ok-symbolic",
            format!("{} connection restored", e.server),
        ),
        EventKind::ThresholdBreach => {
            (1, "dialog-warning", format!("{}: resource alert", e.server))
        }
        EventKind::Rebooted => (0, "view-refresh", format!("{} rebooted", e.server)),
        _ => (0, "dialog-information", e.server.clone()),
    };
    Toast {
        server: e.server.clone(),
        urgency,
        summary: title,
        body: e.message.clone(),
        icon,
    }
}

pub struct Notifier {
    tx: Sender<Msg>,
}

enum Msg {
    Show(Toast),
    Close(String),
}

impl Notifier {
    pub fn spawn() -> Notifier {
        let (tx, rx) = mpsc::channel();
        let _ = std::thread::Builder::new()
            .name("svm-notify".into())
            .stack_size(256 * 1024)
            .spawn(move || worker(rx));
        Notifier { tx }
    }
    pub fn show(&self, t: Toast) {
        let _ = self.tx.send(Msg::Show(t));
    }
    pub fn close_for(&self, server: &str) {
        let _ = self.tx.send(Msg::Close(server.to_string()));
    }
}

fn worker(rx: Receiver<Msg>) {
    let mut conn: Option<zbus::blocking::Connection> = None;
    let mut ids: HashMap<String, u32> = HashMap::new();
    // The bus connection is only held while notifications are flowing.
    loop {
        let msg = match rx.recv_timeout(Duration::from_secs(120)) {
            Ok(m) => m,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                conn = None;
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        if conn.is_none() {
            conn = zbus::blocking::Connection::session().ok();
        }
        let Some(c) = conn.as_ref() else {
            if let Msg::Show(t) = &msg {
                eprintln!("svmonitor: [notify unavailable] {}: {}", t.summary, t.body);
            }
            continue;
        };
        match msg {
            Msg::Show(t) => match notify(c, &t, ids.get(&t.server).copied().unwrap_or(0)) {
                Ok(id) => {
                    ids.insert(t.server.clone(), id);
                }
                Err(e) => {
                    eprintln!("svmonitor: notification failed: {e}");
                    conn = None;
                }
            },
            Msg::Close(server) => {
                if let Some(id) = ids.remove(&server) {
                    let _ = c.call_method(
                        Some("org.freedesktop.Notifications"),
                        "/org/freedesktop/Notifications",
                        Some("org.freedesktop.Notifications"),
                        "CloseNotification",
                        &(id,),
                    );
                }
            }
        }
    }
}

fn notify(c: &zbus::blocking::Connection, t: &Toast, replaces: u32) -> zbus::Result<u32> {
    use std::collections::HashMap as Map;
    use zbus::zvariant::Value;
    let mut hints: Map<&str, Value> = Map::new();
    hints.insert("urgency", Value::U8(t.urgency));
    hints.insert("desktop-entry", Value::from("svmonitor"));
    hints.insert("category", Value::from("network"));
    let timeout: i32 = if t.urgency == 2 { 0 } else { 8000 };
    let reply = c.call_method(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        Some("org.freedesktop.Notifications"),
        "Notify",
        &(
            "SVMonitor",
            replaces,
            t.icon,
            t.summary.as_str(),
            crate::util::escape_markup(&t.body).as_str(),
            Vec::<&str>::new(),
            hints,
            timeout,
        ),
    )?;
    reply.body().deserialize::<u32>()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(ts: i64, kind: EventKind, key: &str) -> Event {
        Event {
            ts,
            server: "s".into(),
            kind,
            message: "m".into(),
            key: key.into(),
            notify: true,
        }
    }

    #[test]
    fn recovery_only_after_announced_down() {
        let mut l = Limiter::default();
        assert!(
            !l.allow(&ev(1, EventKind::Recovered, "down"), 900),
            "no prior down notice"
        );
        assert!(l.allow(&ev(2, EventKind::Down, "down"), 900));
        assert!(l.allow(&ev(3, EventKind::Recovered, "down"), 900));
        assert!(
            !l.allow(&ev(4, EventKind::Recovered, "down"), 900),
            "only once"
        );
    }

    #[test]
    fn flapping_is_rate_limited_and_suppressed_down_gets_no_recovery() {
        let mut l = Limiter::default();
        assert!(l.allow(&ev(0, EventKind::Down, "down"), 900));
        assert!(l.allow(&ev(10, EventKind::Recovered, "down"), 900));
        assert!(
            !l.allow(&ev(20, EventKind::Down, "down"), 900),
            "within cooldown"
        );
        assert!(
            !l.allow(&ev(30, EventKind::Recovered, "down"), 900),
            "its down was never shown"
        );
        assert!(
            l.allow(&ev(1000, EventKind::Down, "down"), 900),
            "cooldown over"
        );
    }

    #[test]
    fn keys_and_servers_are_independent() {
        let mut l = Limiter::default();
        assert!(l.allow(&ev(0, EventKind::ThresholdBreach, "breach:cpu"), 900));
        assert!(l.allow(&ev(1, EventKind::ThresholdBreach, "breach:mem"), 900));
        let mut other = ev(2, EventKind::ThresholdBreach, "breach:cpu");
        other.server = "t".into();
        assert!(l.allow(&other, 900));
        assert!(!l.allow(&ev(3, EventKind::ThresholdBreach, "breach:cpu"), 900));
    }

    #[test]
    fn non_notify_and_info_events_are_silent() {
        let mut l = Limiter::default();
        let mut e = ev(0, EventKind::Down, "down");
        e.notify = false;
        assert!(!l.allow(&e, 0));
        assert!(!l.allow(&ev(0, EventKind::Enabled, ""), 0));
    }

    #[test]
    fn forget_resets() {
        let mut l = Limiter::default();
        l.allow(&ev(0, EventKind::Down, "down"), 900);
        l.forget("s");
        assert!(l.allow(&ev(1, EventKind::Down, "down"), 900));
    }

    #[test]
    fn toast_urgency() {
        assert_eq!(toast_for(&ev(0, EventKind::Down, "down")).urgency, 2);
        assert_eq!(toast_for(&ev(0, EventKind::Recovered, "down")).urgency, 1);
    }
}
