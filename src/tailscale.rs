//! Tailscale state via the local `tailscaled` LocalAPI unix socket (no process spawn).
//! Only the `status` endpoint is read. Absent Tailscale is a normal condition.
use crate::model::TsState;
use std::io::{Read, Write};
use std::net::IpAddr;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

pub const SOCKET: &str = "/var/run/tailscale/tailscaled.sock";
const MAX_BODY: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Peer {
    pub name: String,
    pub dns_name: String,
    pub ips: Vec<IpAddr>,
    pub online: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    /// Socket missing or connection refused.
    DaemonDown,
    Stopped,
    Running {
        peers: Vec<Peer>,
    },
}

pub fn fetch(socket: &Path, timeout: Duration) -> Status {
    match raw_status(socket, timeout) {
        Ok(body) => parse_status(&body).unwrap_or(Status::DaemonDown),
        Err(_) => Status::DaemonDown,
    }
}

fn raw_status(socket: &Path, timeout: Duration) -> std::io::Result<String> {
    let mut s = UnixStream::connect(socket)?;
    s.set_read_timeout(Some(timeout))?;
    s.set_write_timeout(Some(timeout))?;
    s.write_all(b"GET /localapi/v0/status?peers=true HTTP/1.0\r\nHost: local-tailscaled.sock\r\nConnection: close\r\n\r\n")?;
    let mut buf = Vec::new();
    s.take(MAX_BODY as u64).read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| std::io::Error::other("bad http"))?;
    if !head.starts_with("HTTP/1.") || !head.lines().next().unwrap_or("").contains(" 200") {
        return Err(std::io::Error::other("localapi status not 200"));
    }
    Ok(body.to_string())
}

pub fn parse_status(body: &str) -> Option<Status> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let state = v.get("BackendState")?.as_str()?;
    if state != "Running" {
        return Some(Status::Stopped);
    }
    let mut peers = Vec::new();
    if let Some(map) = v.get("Peer").and_then(|p| p.as_object()) {
        for p in map.values() {
            let ips = p
                .get("TailscaleIPs")
                .and_then(|x| x.as_array())
                .map(|a| a.iter().filter_map(|i| i.as_str()?.parse().ok()).collect())
                .unwrap_or_default();
            peers.push(Peer {
                name: p
                    .get("HostName")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                dns_name: p
                    .get("DNSName")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .trim_end_matches('.')
                    .to_string(),
                ips,
                online: p.get("Online").and_then(|x| x.as_bool()).unwrap_or(false),
            });
        }
    }
    Some(Status::Running { peers })
}

/// Does `host` look like it should live in a tailnet (MagicDNS name or CGNAT/ULA address)?
pub fn looks_like_tailnet(host: &str) -> bool {
    let h = host.trim_end_matches('.').to_ascii_lowercase();
    if h.ends_with(".ts.net") {
        return true;
    }
    match h.parse::<IpAddr>() {
        Ok(IpAddr::V4(a)) => {
            let o = a.octets();
            o[0] == 100 && (64..=127).contains(&o[1])
        }
        Ok(IpAddr::V6(a)) => a.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
        Err(_) => false,
    }
}

/// Classify `host` against a Tailscale status.
pub fn classify(host: &str, status: &Status) -> TsState {
    if !looks_like_tailnet(host) {
        return TsState::NotApplicable;
    }
    match status {
        Status::DaemonDown => TsState::DaemonDown,
        Status::Stopped => TsState::Stopped,
        Status::Running { peers } => {
            let h = host.trim_end_matches('.').to_ascii_lowercase();
            let ip: Option<IpAddr> = h.parse().ok();
            let found = peers.iter().find(|p| {
                p.dns_name.eq_ignore_ascii_case(&h) || ip.is_some_and(|ip| p.ips.contains(&ip))
            });
            match found {
                Some(p) if p.online => TsState::PeerOnline,
                Some(_) => TsState::PeerOffline,
                None => TsState::PeerNotFound,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JSON: &str = r#"{"BackendState":"Running","Peer":{
      "k1":{"HostName":"test-server","DNSName":"test-server.tail-example.ts.net.","TailscaleIPs":["100.64.0.10","fd7a:115c:a1e0::1"],"Online":true},
      "k2":{"HostName":"gone","DNSName":"gone.tail-example.ts.net.","TailscaleIPs":["100.64.0.11"],"Online":false}}}"#;

    #[test]
    fn detects_tailnet_hosts() {
        assert!(looks_like_tailnet("x.tail1.ts.net"));
        assert!(looks_like_tailnet("100.64.0.10"));
        assert!(looks_like_tailnet("fd7a:115c:a1e0::1"));
        assert!(!looks_like_tailnet("100.200.1.1"));
        assert!(!looks_like_tailnet("192.168.0.1"));
        assert!(!looks_like_tailnet("example.com"));
    }

    #[test]
    fn classifies_peers() {
        let st = parse_status(JSON).unwrap();
        assert_eq!(
            classify("test-server.tail-example.ts.net", &st),
            TsState::PeerOnline
        );
        assert_eq!(classify("100.64.0.10", &st), TsState::PeerOnline);
        assert_eq!(
            classify("gone.tail-example.ts.net", &st),
            TsState::PeerOffline
        );
        assert_eq!(
            classify("nobody.tail-example.ts.net", &st),
            TsState::PeerNotFound
        );
        assert_eq!(classify("example.com", &st), TsState::NotApplicable);
    }

    #[test]
    fn local_problems_are_not_peer_problems() {
        assert_eq!(
            classify("a.b.ts.net", &Status::DaemonDown),
            TsState::DaemonDown
        );
        assert_eq!(classify("a.b.ts.net", &Status::Stopped), TsState::Stopped);
        assert_eq!(
            classify("example.com", &Status::DaemonDown),
            TsState::NotApplicable
        );
        assert!(TsState::DaemonDown.is_local_problem());
        assert!(!TsState::PeerOffline.is_local_problem());
    }

    #[test]
    fn stopped_and_garbage() {
        assert_eq!(
            parse_status(r#"{"BackendState":"Stopped"}"#),
            Some(Status::Stopped)
        );
        assert_eq!(parse_status("nope"), None);
    }

    #[test]
    fn missing_socket_is_daemon_down() {
        assert_eq!(
            fetch(Path::new("/nonexistent/sock"), Duration::from_millis(100)),
            Status::DaemonDown
        );
    }

    #[test]
    fn talks_http_over_unix_socket() {
        use std::os::unix::net::UnixListener;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("ts.sock");
        let l = UnixListener::bind(&p).unwrap();
        let t = std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut b = [0u8; 512];
            let n = c.read(&mut b).unwrap();
            assert!(String::from_utf8_lossy(&b[..n]).starts_with("GET /localapi/v0/status"));
            let resp = format!("HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n{JSON}");
            c.write_all(resp.as_bytes()).unwrap();
        });
        match fetch(&p, Duration::from_secs(2)) {
            Status::Running { peers } => assert_eq!(peers.len(), 2),
            other => panic!("{other:?}"),
        }
        t.join().unwrap();
    }
}
