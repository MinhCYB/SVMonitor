//! Shared data types. Three independent dimensions are tracked per server:
//! Tailscale state, SSH availability and (derived) health.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Overall {
    Unknown,
    Online,
    Warning,
    Down,
    /// Reachable but needs the user: auth failure, host key problem, DNS, missing ssh binary.
    Attention,
    /// Our own machine has a network/Tailscale problem; the server is not blamed.
    LocalIssue,
    Disabled,
}

impl Overall {
    pub fn label(self) -> &'static str {
        match self {
            Overall::Unknown => "checking",
            Overall::Online => "online",
            Overall::Warning => "warning",
            Overall::Down => "unreachable",
            Overall::Attention => "needs attention",
            Overall::LocalIssue => "local network issue",
            Overall::Disabled => "monitoring off",
        }
    }
    /// Severity used to pick the tray icon colour (higher = worse).
    pub fn severity(self) -> u8 {
        match self {
            Overall::Disabled => 0,
            Overall::Unknown => 1,
            Overall::Online => 1,
            Overall::LocalIssue => 2,
            Overall::Warning => 3,
            Overall::Attention => 4,
            Overall::Down => 5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailKind {
    Timeout,
    Refused,
    NoRoute,
    Reset,
    Dns,
    Auth,
    HostKey,
    Spawn,
    /// ssh connected but the remote command failed or printed unexpected output.
    Remote,
    Other,
}

impl FailKind {
    /// Failures that mean "cannot reach the server" and count toward Down.
    pub fn is_connectivity(self) -> bool {
        matches!(
            self,
            FailKind::Timeout
                | FailKind::Refused
                | FailKind::NoRoute
                | FailKind::Reset
                | FailKind::Other
        )
    }
    pub fn label(self) -> &'static str {
        match self {
            FailKind::Timeout => "connection timed out",
            FailKind::Refused => "connection refused",
            FailKind::NoRoute => "no route to host",
            FailKind::Reset => "connection reset",
            FailKind::Dns => "hostname does not resolve",
            FailKind::Auth => "authentication failed",
            FailKind::HostKey => "host key not verified",
            FailKind::Spawn => "cannot run ssh",
            FailKind::Remote => "remote command failed",
            FailKind::Other => "ssh error",
        }
    }
    pub fn hint(self) -> Option<&'static str> {
        match self {
            FailKind::Auth => Some("Check the key / ssh-agent; test with `ssh <alias>`"),
            FailKind::HostKey => {
                Some("Run `ssh <alias>` once in a terminal to verify the host key")
            }
            FailKind::Dns => Some("Check DNS / Tailscale MagicDNS on this machine"),
            FailKind::Spawn => Some("Install OpenSSH client or set general.ssh_binary"),
            FailKind::Remote => Some(
                "Remote login shell must run POSIX `sh -s`; check ~/.ssh/config RemoteCommand/ForceCommand",
            ),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SshState {
    Unknown,
    Available,
    Unavailable(FailKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TsState {
    /// Target is not a Tailscale address, or Tailscale checks are off.
    NotApplicable,
    /// Could not determine (e.g. LocalAPI unreadable).
    Unknown,
    /// tailscaled is not running on this machine.
    DaemonDown,
    /// tailscaled runs but is stopped / needs login.
    Stopped,
    PeerOnline,
    PeerOffline,
    /// Looks like a tailnet address but no such peer in the tailnet.
    PeerNotFound,
}

impl TsState {
    pub fn label(self) -> &'static str {
        match self {
            TsState::NotApplicable => "n/a",
            TsState::Unknown => "unknown",
            TsState::DaemonDown => "tailscaled not running (local)",
            TsState::Stopped => "stopped (local)",
            TsState::PeerOnline => "peer online",
            TsState::PeerOffline => "peer offline",
            TsState::PeerNotFound => "peer not in tailnet",
        }
    }
    /// The local Tailscale is the problem, not the remote server.
    pub fn is_local_problem(self) -> bool {
        matches!(self, TsState::DaemonDown | TsState::Stopped)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiskUsage {
    pub mount: String,
    pub total_kib: u64,
    pub used_kib: u64,
}

impl DiskUsage {
    pub fn pct(&self) -> f32 {
        pct(self.used_kib, self.total_kib)
    }
}

/// Network throughput of one interface. `sampled` = measured over a ~1 s window inside one probe
/// (near real time); otherwise an average over the interval between two probes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NetRate {
    pub iface: String,
    pub rx_bps: u64,
    pub tx_bps: u64,
    pub window_secs: f32,
    pub sampled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metrics {
    pub ts: i64,
    pub cpu_pct: Option<f32>,
    pub cores: Option<u32>,
    pub load: [f32; 3],
    pub mem_total_kib: u64,
    pub mem_used_kib: u64,
    pub swap_total_kib: u64,
    pub swap_used_kib: u64,
    pub disks: Vec<DiskUsage>,
    pub uptime_secs: u64,
    pub procs: Option<u32>,
    pub os: Option<String>,
    pub kernel: Option<String>,
    pub hostname: Option<String>,
    /// None until two counter readings exist (first poll after start, reset, or reconnect).
    #[serde(default)]
    pub net: Option<NetRate>,
}

pub fn pct(used: u64, total: u64) -> f32 {
    if total == 0 {
        0.0
    } else {
        (used as f64 * 100.0 / total as f64) as f32
    }
}

impl Metrics {
    pub fn mem_pct(&self) -> f32 {
        pct(self.mem_used_kib, self.mem_total_kib)
    }
    pub fn swap_pct(&self) -> Option<f32> {
        (self.swap_total_kib > 0).then(|| pct(self.swap_used_kib, self.swap_total_kib))
    }
    pub fn worst_disk(&self) -> Option<&DiskUsage> {
        self.disks.iter().max_by(|a, b| {
            a.pct()
                .partial_cmp(&b.pct())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    }
    /// 1-minute load divided by core count (falls back to raw load if cores unknown).
    pub fn load_per_core(&self) -> f32 {
        self.load[0] / self.cores.filter(|c| *c > 0).unwrap_or(1) as f32
    }
}

/// Everything a UI needs to render one server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerView {
    pub name: String,
    pub target: String,
    pub enabled: bool,
    pub overall: Overall,
    pub ssh: SshState,
    pub tailscale: TsState,
    pub metrics: Option<Metrics>,
    pub last_check: Option<i64>,
    #[serde(default)]
    pub completed_checks: u64,
    pub last_ok: Option<i64>,
    pub down_since: Option<i64>,
    pub detail: String,
    pub pending_failures: u32,
    pub failure_threshold: u32,
    pub active_alerts: Vec<String>,
    pub probing: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub ts: i64,
    pub pid: u32,
    pub local_net_ok: bool,
    pub muted: bool,
    pub servers: Vec<ServerView>,
    #[serde(default)]
    pub command_capable: bool,
}

impl Snapshot {
    pub fn worst(&self) -> Overall {
        self.servers
            .iter()
            .map(|s| s.overall)
            .max_by_key(|o| o.severity())
            .unwrap_or(Overall::Disabled)
    }
    pub fn count_online(&self) -> usize {
        self.servers
            .iter()
            .filter(|s| matches!(s.overall, Overall::Online | Overall::Warning))
            .count()
    }
    pub fn count_enabled(&self) -> usize {
        self.servers.iter().filter(|s| s.enabled).count()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Down,
    Recovered,
    Attention,
    AttentionCleared,
    ThresholdBreach,
    ThresholdCleared,
    Rebooted,
    Enabled,
    Disabled,
    Started,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub ts: i64,
    pub server: String,
    pub kind: EventKind,
    pub message: String,
    /// Key used for notification rate limiting, e.g. "down" or "breach:cpu".
    #[serde(default)]
    pub key: String,
    /// Whether this event should raise a desktop notification.
    #[serde(default)]
    pub notify: bool,
}
