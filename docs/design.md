# SVMonitor design (V1)

Decisions made autonomously per the project brief (no per-phase approval).

## Goals / non-goals
- Goal: tray + CLI monitor for Linux servers reachable through the user's existing OpenSSH setup. Idle RSS <= 20 MiB, ~0 CPU.
- Non-goals: remote agents, storing credentials, editing `~/.ssh/config`, Windows/macOS, graphs, remote control of servers.

## Findings that drove the design
| Finding | Consequence |
|---|---|
| No MeshPerch code on the machine | Nothing to reuse; greenfield |
| GNOME/Wayland here has **no StatusNotifierWatcher** (no AppIndicator extension) | Tray must degrade to headless without crashing, tell the user how to fix it, and attach automatically if a watcher appears later (`assume_sni_available`) |
| `catplus` uses Tailscale MagicDNS name, `StrictHostKeyChecking ask`, `ControlMaster false` | Probe through system `ssh`; never override host-key policy; classify "host key unknown" separately from "offline" |
| tailscaled LocalAPI socket is world rw | Query peer state over the unix socket (no process spawn) |
| Rust absent; Debian 12 ships rustc 1.63 | Install via rustup for dev; build Debian package inside a Debian container with rustup for glibc compatibility |

## Architecture
Single process, plain threads, **no async runtime**. One event-loop thread (`engine`) blocks on an mpsc channel with a timeout equal to the next due probe, so idle wake-ups happen only when a probe is due.

```
signal thread ─┐
inotify thread ─┤            ┌──────────────┐  Snapshot   ┌──────────┐
tray callbacks ─┼─ Cmd ────▶ │  engine loop │ ──────────▶ │ tray(ksni)│
probe workers ──┘            │ (scheduler,  │             └──────────┘
                             │  health SM,  │ ──▶ notifier (D-Bus Notify, rate limited)
                             │  history)    │ ──▶ history JSONL, state.json
                             └──────────────┘
probe worker (1 short-lived thread / due server, max 4 parallel):
  tailscale LocalAPI status (cached 5 s) + local-network check + `ssh -T … sh -s` (script on stdin)
```

### Probing
- One `ssh` process per server per cycle; the remote script is POSIX `sh`, uses shell builtins (`read`) for `/proc`, one `df` fork. Read-only, no sudo, no agent.
- Flags added: `BatchMode=yes`, `ConnectTimeout`, `ServerAlive*`, `ClearAllForwardings=yes`, `ForwardAgent=no`, `ForwardX11=no`, `PermitLocalCommand=no`, `RemoteCommand=none`, `RequestTTY=no`. Host key checking, identities, ProxyJump, Include etc. come from the user's config untouched.
- Hard deadline enforced by killing the ssh process group.
- CPU% is computed from `/proc/stat` deltas between consecutive polls (no sleep on the remote).

### Three independent dimensions (no false "offline")
1. **Tailscale**: `NotApplicable | LocalDown | PeerOnline | PeerOffline | Unmatched`.
2. **SSH**: `Unknown | Available | Unavailable(kind)`; kinds: Timeout, Refused, NoRoute, Reset, Dns, Auth, HostKey, Spawn, Other.
3. **Health**: thresholds (CPU/RAM/swap/disk/load) with 2-sample persistence and hysteresis.

Overall state: `Unknown | Online | Warning | Down | Attention | LocalIssue | Disabled`.
- Auth/HostKey/Dns/Spawn → `Attention` (never "offline").
- Connectivity failures count toward `Down` only when the local network (default route) and, for tailscale targets, local tailscaled are up; otherwise `LocalIssue` and nothing is alerted.
- Debounce: `failure_threshold` (default 3) consecutive failures, fast retry while pending; backoff while down.
- Suspend/resume detected via CLOCK_BOOTTIME − CLOCK_MONOTONIC drift → grace period, failures discarded, reprobe.

### Notifications
`org.freedesktop.Notifications` via zbus; per-server replace-id so a server never stacks toasts; per-(server,kind) cooldown; recovery only after a delivered down notice; nothing during `LocalIssue`/grace.

### Persistence
- Config `~/.config/svmonitor/config.toml` (atomic write). Servers refer to SSH aliases; optional manual host/user/port/identity.
- History `~/.local/state/svmonitor/{events,samples}.jsonl`, size-rotated.
- Runtime `state.json` in `$XDG_RUNTIME_DIR/svmonitor/` + flock single-instance lock.

### Why not…
- tokio/async: not needed, costs RSS.
- SQLite: extra C dependency; JSONL is enough for append + tail.
- libssh2/russh: would bypass ssh-agent/ProxyJump/Include/known_hosts semantics the product promises to reuse.
- GTK/Qt tray: heavy; SNI over D-Bus (ksni) is native to KDE/GNOME(extension)/others.

## Deviations from the first sketch (learned during implementation)
- The probe script is fed on stdin as `SVM_SAMPLE=<0|1>` + body; the first poll adds a 1 s `sleep` on the server to get a CPU % without waiting a full interval.
- systemd unit hardening is minimal: `ProtectSystem`/`ProtectHome`/`PrivateTmp` break ssh in a user manager (see SECURITY.md #11).
- `.deb` is built in a Debian 12 container; Arch package via `makepkg` from the local tree.
- `Outcome::Ok` and `ProbeResult::Ok` are boxed (large variant).
