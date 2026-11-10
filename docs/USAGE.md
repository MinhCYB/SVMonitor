# Using SVMonitor

## Concepts
A *server* is an SSH alias from `~/.ssh/config` (or a manual host). SVMonitor runs `ssh <alias> sh -s`, feeds it a read-only
POSIX script (reads `/proc`, one `df`), and parses the output. Every OpenSSH setting you already have applies.
SVMonitor adds only safety options (`BatchMode=yes`, `ConnectTimeout`, no forwarding, no TTY) and never overrides host-key checking.

States: **online**, **warning** (resource threshold), **unreachable** (connectivity failed `failure_threshold` times in a row),
**needs attention** (auth/host-key/DNS/ssh missing: reachable but you must act), **local network issue** (this machine has no route
or local tailscaled is down; nothing is alerted), **monitoring off**.

## First run
1. `svmonitor doctor`: checks ssh, tray host, tailscale, route, terminal, and sends a test notification.
2. `svmonitor discover` then `svmonitor import <alias>` (or tray: *Add from SSH config*).
3. If the status says **needs attention / host key not verified**, run `ssh <alias>` once in a terminal and accept the key. Background probes never prompt.
4. `svmonitor autostart enable` (or tray: *Start with desktop session*). Alternatively `systemctl --user enable --now svmonitor.service`.

## Commands
| Command | Purpose |
|---|---|
| `run [--headless] [--no-notify]` | daemon; tray if a tray host exists |
| `status [--json] [-w]` | state of the running daemon (probes once if not running); exit code 0 ok, 1 warning/local, 2 down/attention |
| `check [names] [--json]` | immediate probe, no daemon needed |
| `list`, `discover`, `import [--all] [names]` | server management |
| `add NAME [--host H --user U --port P --identity PATH --interval S]` | manual server |
| `remove`, `enable`, `disable NAME` | per-server control (daemon reloads via SIGHUP) |
| `history [server] [-n N] [--json]` | event history |
| `ssh NAME` | open interactive SSH in your terminal |
| `autostart enable|disable|status` | XDG autostart entry |
| `reload`, `paths`, `doctor` | maintenance |
| `svmonitor-popup [--config PATH]` | on-demand GTK4 dashboard popup window (optional binary) |

`svmonitor reload` and `systemctl --user reload svmonitor.service` apply configuration in the existing
process; neither restarts monitoring nor creates another instance. Invalid TOML is rejected and the
previous valid configuration remains active (the error is logged in the journal).

### Default terminal

Leave `general.terminal = ""` to use the desktop/user terminal through
[`xdg-terminal-exec`](https://github.com/Vladimir-csp/xdg-terminal-exec).
This dispatcher follows XDG terminal preferences (`~/.config/xdg-terminals.list` and desktop-specific
variants) and terminal desktop entries; SVMonitor does not create or modify those preferences.
On Arch/CachyOS install `xdg-terminal-exec`; on Debian use the distro package when available.
Without it, SVMonitor uses an explicitly selected `$TERMINAL` or Debian's `x-terminal-emulator`
alternative, not a priority list of installed emulators. If no resolver is available, configure an
override instead of silently choosing an unrelated terminal.

For an application-only override use, for example, `terminal = "kitty"`,
`terminal = "konsole"`, or an argument prefix such as `terminal = '"/opt/My Terminal/bin/term" --run'`.
Quoted paths and arguments are supported; shell substitutions, pipes and redirections are not
executed. SSH is appended as separate arguments, preserving aliases, ssh-agent and host-key policy.


## GTK4 Dashboard Popup
The optional `svmonitor-popup` binary provides an on-demand GTK4 graphical window displaying server cards with resource meters (CPU, RAM, swap, disk), network throughput rates (RX/TX), SSH and Tailscale connectivity states, and quick action buttons (Check now, Mute notifications, Enable/Disable, Terminal, Remove).

### Opening the dashboard
- **Native tray menu:** The first menu item is always **"Open dashboard"**, offering single-click access on all desktop environments including GNOME.
- **Tray icon click:**
  - **GNOME AppIndicator:** Double-click left mouse button or middle-click (`SecondaryActivate`). GNOME's `indicatorStatusIcon.js` only sends `Activate` on double-click; a single left-click toggles the native DBusMenu.
  - **KDE Plasma & others:** Single left-click sends `Activate` directly.
- **Command line:** Run `svmonitor-popup` (or `svmonitor-popup --config PATH` for custom config paths).

### Architecture and memory footprint
- **Independent process:** `svmonitor-popup` runs only when summoned and **exits completely when closed** (`exit(0)`). Idle memory cost when closed is 0 MiB.
- **Separate daemon footprint:** The background daemon remains strictly headless and GUI-free, maintaining its low memory footprint (~4-7 MiB RSS). Daemon RSS targets are completely separate from the GTK window while open.
- **Cairo renderer:** The popup defaults to `GSK_RENDERER=cairo` (CPU rasterization), unless the environment already selects a renderer.
- **No libadwaita:** Plain GTK4 (>=4.8) only. Follows system light/dark preference via GTK settings / desktop portal preference and CSS with standard theme colors (`@theme_fg_color`, `@theme_bg_color`), supporting HiDPI displays.
- **Window behavior & Wayland placement:** Undecorated, non-resizable 440×650 logical-pixel window. Close via **Close** or **Escape**; no auto-dismiss. The compositor chooses placement; tray click coordinates are not used.
- **Upgrade and daemon capability handshake:** Existing running v0.1.2 daemons lack `SIGUSR1` command file handling. When upgrading to v0.1.3, restart the daemon (`systemctl --user restart svmonitor.service` or restart `svmonitor run`); a configuration reload (`svmonitor reload`) does not add signal handling to an older process. The popup verifies daemon capabilities in the snapshot; if run against an older daemon, interactive actions are refused safely rather than sending unhandled signals.
- **Fallback:** If `svmonitor-popup` is not installed, the native D-Bus tray menu remains complete and fully functional (v0.1.2 behavior).
## Network monitoring
SVMonitor tracks remote network throughput without installing agents:
- **Configuration:** Configure `net_interface = "eth0"` under `[[server]]` in `config.toml` to monitor a specific interface.
- **Auto-detection:** When omitted, SVMonitor reads `/proc/net/route` on the remote server to detect the default route interface. If no default route exists, it selects the physical-looking interface with the highest traffic. Virtual interfaces (`lo`, `docker*`, `veth*`, `br-*`, `virbr*`, `tailscale*`, `tun*`, `wg*`) are ignored by default.
- **Tailscale:** Tailscale traffic is excluded by default and never summed into default traffic. Set `net_interface = "tailscale0"` to monitor tailnet traffic specifically.
- **Sampling mechanics & qualifiers:**
 - *First poll / CPU baseline:* Two `/proc/net/dev` readings across a one-second sleep (`sampled = true`). The dashboard labels this `1s sample`; native tray details label it `live`.
  - *Routine polls:* Rate is computed as an average over the polling interval from counter deltas divided by elapsed remote `/proc/uptime` (`sampled = false`). Monotonic remote uptime eliminates SSH connection latency and local clock drift. Displayed with the `avg` qualifier.
## Tray menu
The native tray keeps urgent servers first. Up to eight monitored servers appear directly; larger fleets use bounded groups, with monitoring-off servers separate. Names containing underscores remain literal.

Each server offers **Open SSH terminal** and **Check now**, separate SSH/Tailscale status, a compact CPU/RAM/disk view, and a **Details** submenu for full metrics/system information. Unavailable SSH marks retained metrics stale. Recent events and removal are nested instead of expanding the main menu.

Global refresh shows queued/checking feedback and is unavailable while probes are active. Secondary controls (SSH-config import, notification mute, autostart, configuration and history) live under **Manage**. Launcher/autostart errors appear in the menu. Status badges use shape as well as color; essential information remains in text because tooltip/icon support varies by tray host.

Refresh completion is tracked per server with a monotonic completed-check counter, including checks that finish within the same second. Configuration actions are queued without waiting under the tray lock; reopening the menu consumes engine replies and displays save errors. A failed save can leave a runtime enable/remove change active until configuration reload; check the error and writable config location before restarting.

## Installation and packages
- **Arch Linux / CachyOS:**
  `cd packaging/arch && makepkg -si`
  Produces `svmonitor` (headless daemon) and `svmonitor-popup` (optional GTK4 dashboard).
- **Debian 12 / 13:**
  `./packaging/debian/docker-build.sh` produces `svmonitor_0.1.3_amd64.deb` and `svmonitor-popup_0.1.3_amd64.deb`.
- **Build from source:**
  - Headless daemon (no GTK dependencies, Rust >= 1.89):
    `cargo build --locked --release`
  - Dashboard popup GUI (requires GTK >= 4.8 development headers; Rust >= 1.89 across the workspace):
    `cargo build --release --features popup --bin svmonitor-popup`

## Files
- Config `~/.config/svmonitor/config.toml` (see `config.example.toml`)
- History `~/.local/state/svmonitor/events.jsonl` (+ `.1`), `samples.jsonl` (+ `.1`); each rotates at 2 MiB
- Runtime `$XDG_RUNTIME_DIR/svmonitor/` (lock, state.json, cmd/ spool)
- Config `~/.config/svmonitor/config.toml` (see `config.example.toml`)
- History `~/.local/state/svmonitor/events.jsonl` (+ `.1`), `samples.jsonl` (+ `.1`); each rotates at 2 MiB
- Runtime `$XDG_RUNTIME_DIR/svmonitor/` (lock, state.json)

## Troubleshooting
| Symptom | Cause / fix |
|---|---|
| No tray icon on GNOME | Install `gnome-shell-extension-appindicator`, enable it, re-login. The monitor keeps running and the icon appears once a host exists. |
| "host key not verified" | Run `ssh <alias>` once interactively. |
| "authentication failed" | Key not loaded: `ssh-add`; background probes cannot ask for passphrases. Test `ssh -o BatchMode=yes <alias> true`. |
| "remote command failed / output not recognised" | Remote login shell prints extra output or `RemoteCommand`/`ForceCommand` is set. |
| Open SSH does nothing | Install `xdg-terminal-exec`, or configure `general.terminal`. The installed dispatcher follows the desktop's terminal choice; no system preference is changed. |
| Marked "local network issue" | No default route or local tailscaled stopped; resolves itself. |
| Another instance error | `svmonitor` allows one instance per user (flock). |

## Uninstall
```sh
svmonitor autostart disable
systemctl --user disable --now svmonitor.service 2>/dev/null
sudo pacman -R svmonitor        # or: sudo apt remove svmonitor
rm -rf ~/.config/svmonitor ~/.local/state/svmonitor   # optional: config + history
```
`~/.ssh/config` is never modified by SVMonitor.
