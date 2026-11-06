//! Configuration (TOML). Servers normally reference an SSH alias only; credentials are
//! never stored. Writes are atomic and never touch `~/.ssh/config`.
use crate::util::atomic_write;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct General {
    pub ssh_binary: String,
    /// Seconds between probes of a healthy server.
    pub interval_secs: u64,
    /// Hard deadline for one probe (ssh process is killed after this).
    pub probe_timeout_secs: u64,
    pub connect_timeout_secs: u64,
    /// Consecutive connectivity failures before a server is declared Down.
    pub failure_threshold: u32,
    /// Seconds between retries while failures are pending (debounce).
    pub retry_interval_secs: u64,
    /// Max seconds between probes of a Down server (exponential backoff cap).
    pub down_interval_max_secs: u64,
    pub max_parallel_probes: usize,
    pub check_tailscale: bool,
    pub notifications: bool,
    /// Minimum seconds between notifications of the same kind for the same server.
    pub notify_cooldown_secs: u64,
    /// Terminal used by "Open SSH". Empty = auto detect.
    pub terminal: String,
    pub history_max_events: usize,
    pub keep_samples: bool,
}

impl Default for General {
    fn default() -> Self {
        General {
            ssh_binary: "ssh".into(),
            interval_secs: 60,
            probe_timeout_secs: 20,
            connect_timeout_secs: 10,
            failure_threshold: 3,
            retry_interval_secs: 10,
            down_interval_max_secs: 300,
            max_parallel_probes: 4,
            check_tailscale: true,
            notifications: true,
            notify_cooldown_secs: 900,
            terminal: String::new(),
            history_max_events: 2000,
            keep_samples: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Thresholds {
    pub cpu_pct: f32,
    pub mem_pct: f32,
    pub swap_pct: f32,
    pub disk_pct: f32,
    /// 1-minute load per core.
    pub load_per_core: f32,
    /// Consecutive samples over the limit before alerting.
    pub samples: u32,
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds {
            cpu_pct: 95.0,
            mem_pct: 92.0,
            swap_pct: 80.0,
            disk_pct: 90.0,
            load_per_core: 4.0,
            samples: 3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Server {
    /// Display name and ssh alias (unless `host` is set).
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Manual server: connect to this host instead of treating `name` as an alias.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Path to an identity file (a path only, never key material).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_file: Option<String>,
    /// Per-server override of general.interval_secs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_secs: Option<u64>,
}

fn yes() -> bool {
    true
}

impl Server {
    pub fn alias(name: &str) -> Server {
        Server {
            name: name.into(),
            enabled: true,
            host: None,
            user: None,
            port: None,
            identity_file: None,
            interval_secs: None,
        }
    }
    pub fn is_manual(&self) -> bool {
        self.host.is_some()
    }
    /// What is passed to ssh as destination.
    pub fn destination(&self) -> String {
        match &self.host {
            Some(h) => match &self.user {
                Some(u) => format!("{u}@{h}"),
                None => h.clone(),
            },
            None => self.name.clone(),
        }
    }
    /// Extra `-o`/`-p`/`-i` style arguments for manual servers.
    pub fn extra_args(&self) -> Vec<String> {
        let mut a = Vec::new();
        if self.is_manual() {
            if let Some(p) = self.port {
                a.push("-p".into());
                a.push(p.to_string());
            }
            if let Some(i) = &self.identity_file {
                a.push("-i".into());
                a.push(i.clone());
            }
        }
        a
    }
    pub fn validate(&self) -> Result<()> {
        validate_name(&self.name)?;
        for (label, v) in [
            ("host", self.host.as_deref()),
            ("user", self.user.as_deref()),
        ] {
            if let Some(v) = v {
                if v.is_empty()
                    || v.starts_with('-')
                    || v.chars()
                        .any(|c| c.is_whitespace() || c.is_control() || c == '@' && label == "host")
                {
                    bail!("invalid {label}: {v:?}");
                }
            }
        }
        if let Some(i) = &self.identity_file {
            if i.starts_with('-') || i.chars().any(|c| c.is_control()) {
                bail!("invalid identity_file");
            }
        }
        Ok(())
    }
}

/// Names double as ssh aliases and are passed on a command line: forbid option-like or odd values.
pub fn validate_name(n: &str) -> Result<()> {
    if n.is_empty() || n.len() > 253 {
        bail!("server name must be 1-253 characters");
    }
    if n.starts_with('-') {
        bail!("server name must not start with '-'");
    }
    if n.chars().any(|c| {
        c.is_whitespace()
            || c.is_control()
            || matches!(
                c,
                '"' | '\'' | '\\' | '`' | '$' | ';' | '&' | '|' | '<' | '>' | '*' | '?' | '!'
            )
    }) {
        bail!("server name contains unsupported characters: {n:?}");
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct Config {
    pub general: General,
    pub thresholds: Thresholds,
    #[serde(rename = "server")]
    pub servers: Vec<Server>,
}

impl Config {
    pub fn parse(text: &str) -> Result<Config> {
        let c: Config = toml::from_str(text).context("parsing config")?;
        c.validate()?;
        Ok(c)
    }
    pub fn validate(&self) -> Result<()> {
        let g = &self.general;
        if g.interval_secs < 5 {
            bail!("general.interval_secs must be >= 5");
        }
        if g.probe_timeout_secs < 3 || g.connect_timeout_secs < 1 {
            bail!("timeouts too small");
        }
        if g.failure_threshold == 0 {
            bail!("general.failure_threshold must be >= 1");
        }
        if g.max_parallel_probes == 0 || g.max_parallel_probes > 32 {
            bail!("general.max_parallel_probes must be 1..=32");
        }
        let mut seen = std::collections::HashSet::new();
        for s in &self.servers {
            s.validate()?;
            if !seen.insert(&s.name) {
                bail!("duplicate server {:?}", s.name);
            }
            if s.interval_secs.is_some_and(|i| i < 5) {
                bail!("server {:?}: interval_secs must be >= 5", s.name);
            }
        }
        Ok(())
    }
    pub fn server(&self, name: &str) -> Option<&Server> {
        self.servers.iter().find(|s| s.name == name)
    }
    pub fn server_mut(&mut self, name: &str) -> Option<&mut Server> {
        self.servers.iter_mut().find(|s| s.name == name)
    }
    pub fn add(&mut self, s: Server) -> Result<()> {
        s.validate()?;
        if self.server(&s.name).is_some() {
            bail!("server {:?} already exists", s.name);
        }
        self.servers.push(s);
        Ok(())
    }
    pub fn remove(&mut self, name: &str) -> bool {
        let n = self.servers.len();
        self.servers.retain(|s| s.name != name);
        self.servers.len() != n
    }
    pub fn interval_for(&self, s: &Server) -> u64 {
        s.interval_secs.unwrap_or(self.general.interval_secs).max(5)
    }
}

pub struct Paths {
    pub config: PathBuf,
    pub state_dir: PathBuf,
    pub runtime_dir: PathBuf,
}

impl Paths {
    pub fn from_env() -> Paths {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        let xdg = |var: &str, fallback: PathBuf| match std::env::var_os(var) {
            Some(v) if !v.is_empty() && PathBuf::from(&v).is_absolute() => PathBuf::from(v),
            _ => fallback,
        };
        let cfg = xdg("XDG_CONFIG_HOME", home.join(".config")).join("svmonitor");
        let state = xdg("XDG_STATE_HOME", home.join(".local/state")).join("svmonitor");
        let run = match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(v) if !v.is_empty() => PathBuf::from(v).join("svmonitor"),
            // SAFETY: getuid has no preconditions.
            _ => PathBuf::from(format!("/tmp/svmonitor-{}", unsafe { libc::getuid() })),
        };
        Paths {
            config: cfg.join("config.toml"),
            state_dir: state,
            runtime_dir: run,
        }
    }
    pub fn events(&self) -> PathBuf {
        self.state_dir.join("events.jsonl")
    }
    pub fn samples(&self) -> PathBuf {
        self.state_dir.join("samples.jsonl")
    }
    pub fn snapshot(&self) -> PathBuf {
        self.runtime_dir.join("state.json")
    }
    pub fn lock(&self) -> PathBuf {
        self.runtime_dir.join("lock")
    }
    pub fn control(&self) -> PathBuf {
        self.runtime_dir.join("control.sock")
    }
}

pub fn load(path: &Path) -> Result<Config> {
    match std::fs::read_to_string(path) {
        Ok(t) => Config::parse(&t).with_context(|| format!("in {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn save(path: &Path, cfg: &Config) -> Result<()> {
    cfg.validate()?;
    let text = toml::to_string_pretty(cfg).context("serialising config")?;
    let header = "# SVMonitor configuration. Servers reference SSH aliases from ~/.ssh/config.\n# No passwords or keys are stored here.\n\n";
    atomic_write(path, format!("{header}{text}").as_bytes())
        .with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_valid_and_roundtrip() {
        let mut c = Config::default();
        c.add(Server::alias("catplus")).unwrap();
        c.add(Server {
            host: Some("10.0.0.1".into()),
            user: Some("root".into()),
            port: Some(2200),
            ..Server::alias("manual")
        })
        .unwrap();
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("c/config.toml");
        save(&p, &c).unwrap();
        assert_eq!(load(&p).unwrap(), c);
    }

    #[test]
    fn missing_file_gives_defaults() {
        assert_eq!(
            load(Path::new("/nonexistent/x.toml")).unwrap(),
            Config::default()
        );
    }

    #[test]
    fn partial_file_fills_defaults() {
        let c = Config::parse("[[server]]\nname=\"a\"\n[general]\ninterval_secs=30\n").unwrap();
        assert_eq!(c.general.interval_secs, 30);
        assert_eq!(c.general.failure_threshold, 3);
        assert!(c.servers[0].enabled);
    }

    #[test]
    fn rejects_dangerous_or_invalid() {
        assert!(Config::parse("[[server]]\nname=\"-oProxyCommand=x\"\n").is_err());
        assert!(Config::parse("[[server]]\nname=\"a b\"\n").is_err());
        assert!(Config::parse("[[server]]\nname=\"a\"\nhost=\"-oProxyCommand=x\"\n").is_err());
        assert!(Config::parse("[[server]]\nname=\"a\"\n[[server]]\nname=\"a\"\n").is_err());
        assert!(Config::parse("[general]\ninterval_secs=1\n").is_err());
        assert!(Config::parse("[general]\nfailure_threshold=0\n").is_err());
        assert!(Config::parse("this is not toml").is_err());
    }

    #[test]
    fn destination_and_args() {
        let a = Server::alias("web");
        assert_eq!(a.destination(), "web");
        assert!(a.extra_args().is_empty());
        let m = Server {
            host: Some("h".into()),
            user: Some("u".into()),
            port: Some(22),
            identity_file: Some("/k".into()),
            ..Server::alias("m")
        };
        assert_eq!(m.destination(), "u@h");
        assert_eq!(m.extra_args(), vec!["-p", "22", "-i", "/k"]);
    }

    #[test]
    fn per_server_interval() {
        let mut c = Config::default();
        c.add(Server {
            interval_secs: Some(120),
            ..Server::alias("a")
        })
        .unwrap();
        assert_eq!(c.interval_for(&c.servers[0]), 120);
        c.add(Server::alias("b")).unwrap();
        assert_eq!(c.interval_for(&c.servers[1]), 60);
    }
}
