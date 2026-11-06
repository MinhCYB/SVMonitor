//! SVMonitor: lightweight SSH based server monitor.
pub mod autostart;
pub mod cli;
pub mod config;
pub mod engine;
pub mod health;
pub mod history;
pub mod launcher;
pub mod localnet;
pub mod metrics;
pub mod model;
pub mod notify;
pub mod probe;
pub mod sshconfig;
pub mod tailscale;
pub mod tray;
pub mod util;
