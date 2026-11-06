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


## Tray menu
Per server submenu: status, SSH/Tailscale state, CPU/RAM/swap/disk/load/uptime, last events, **Open SSH terminal**, **Check now**,
**Monitoring enabled**, **Remove**. Global: Check all, Add from SSH config, Mute notifications 1 h, autostart, open config/history, Quit.

## Files
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
