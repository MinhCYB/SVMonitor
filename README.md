# SVMonitor

Lightweight server monitoring for Linux, from your system tray. It reuses your existing OpenSSH setup
(`~/.ssh/config`, keys, agent, ProxyJump, Include, host-key verification) and installs **nothing** on the servers.

- Picks up SSH host aliases automatically; choose a server, turn monitoring on, done.
- Read-only metrics over SSH: CPU, RAM, swap, disk, load, uptime. No `sudo`, no agent, no stored credentials.
- Keeps **Tailscale state, SSH reachability and server health** separate, so a local network or tailscaled problem is never reported as "server offline".
- Debounce, retry/backoff, suspend/resume grace, per-event notification cooldown.
- Tray menu (StatusNotifierItem), desktop notifications, one-click SSH terminal, event history, autostart.
- Headless CLI for terminals and servers without a tray.
- Native Rust, no async runtime, no GUI toolkit: ~5 MiB RSS and <0.1% CPU idle (see [docs/BENCHMARKS.md](docs/BENCHMARKS.md)).

## Install

Arch / CachyOS:

```sh
cd packaging/arch && makepkg -si
```

Debian / Ubuntu (build in a Debian 12 container for broad glibc compatibility, needs docker):

```sh
packaging/debian/docker-build.sh && sudo apt install ./dist/svmonitor_0.1.0_amd64.deb
```

From source (Rust >= 1.85):

```sh
cargo build --release && install -Dm755 target/release/svmonitor ~/.local/bin/svmonitor
```

Uninstall: `sudo pacman -R svmonitor` / `sudo apt remove svmonitor`; user data removal is in [docs/USAGE.md](docs/USAGE.md#uninstall).

## Quick start

```sh
svmonitor discover            # list aliases from ~/.ssh/config
svmonitor import catplus      # or: svmonitor import --all
svmonitor check               # one-off read-only probe
svmonitor autostart enable    # start with the desktop session
svmonitor run                 # tray (falls back to headless)
```

GNOME needs the *AppIndicator and KStatusNotifierItem Support* extension for any tray icon; run `svmonitor doctor` to check.

Docs: [usage & troubleshooting](docs/USAGE.md) · [design](docs/design.md) · [benchmarks](docs/BENCHMARKS.md) ·
[security review](docs/SECURITY.md) · [compatibility & limits](docs/COMPATIBILITY.md)

License: MIT
