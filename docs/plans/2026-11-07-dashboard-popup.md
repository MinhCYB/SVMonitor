# SVMonitor v0.1.3 dashboard popup

## Findings that decide the architecture
| Finding (measured on this machine) | Consequence |
|---|---|
| DBusMenu cannot draw progress bars / grid layout (spec items: label, icon, toggle, submenu only) | A custom window is required for the dashboard; native menu stays as fallback |
| GNOME AppIndicator: single left-click toggles the DBusMenu; `Activate` is sent only on **double-click** (`indicatorStatusIcon.js` `_maybeHandleDoubleClick`); middle-click sends `SecondaryActivate`/`XAyatanaSecondaryActivate` | Cannot replace the menu with one-click popup on GNOME. Provide: Activate (double-click, KDE/others single click) + SecondaryActivate (middle-click) + first native menu row **"Open dashboard"** as the always-available entry |
| GNOME Shell on Wayland forbids client-set window position; `gtk4-layer-shell` absent on this host; a client toplevel cannot be anchored to the panel icon | Popup = undecorated, non-resizable toplevel window; compositor places it. Top-right placement not promised on Wayland; documented limit. `Activate(x,y)` is stored and used only for X11/KDE hints |
| GTK4 Python window RSS: 166 MiB (adw, default GL renderer), 262 MiB (ngl), **74 MiB (cairo renderer)** | Popup is a separate on-demand process, never part of the daemon. Force `GSK_RENDERER=cairo` (CPU-rasterised, ample for a 400px static list) to cut RSS >2x. Closed popup = process exits = 0 MiB |
| Debian 12: GTK 4.8 / libadwaita 1.2; Debian 13: GTK 4.18 / libadwaita 1.7; Arch GTK 4.24 / libadwaita 1.10 | Use plain **GTK4 (>=4.8) only, no libadwaita** -> same code on all targets; follow GNOME/KDE light-dark through `gtk-application-prefer-dark-theme` / portal color-scheme; own CSS using theme-named colours (`@theme_fg_color`...) |
| gtk4-rs 0.11 needs Rust 1.92 + glib 0.22 and GTK >= system version; Debian 12 GTK 4.8 forces `v4_8` feature cap | Separate Cargo feature + separate bin `svmonitor-popup` so the daemon crate keeps its 1.89 MSRV and zero GUI deps; packagers ship GUI as optional |

## Architecture
```
svmonitor (daemon, unchanged footprint)        svmonitor-popup (GTK4, on demand)
  engine -> state.json (atomic, 0600) ───read──> inotify/timer (2 s, only while visible)
  SNI Activate / menu "Open dashboard" ──spawn──>  single instance via flock + `present` on re-activation
  SIGHUP reload                                  actions -> spool command files -> SIGUSR1 -> daemon
```
* **Read path**: popup reads `$XDG_RUNTIME_DIR/svmonitor/state.json` (already published by the engine on change). No extra SSH sessions; opening the popup only shows cached data. Zero daemon-side cost.
* **Write path (no new IPC server)**: popup writes small JSON command files into `$XDG_RUNTIME_DIR/svmonitor/cmd/` (0700 dir, atomic write) and signals the daemon with `SIGUSR1` (PID verified via `/proc/<pid>/comm`, same as reload). Daemon consumes + validates + deletes. Commands: `check{server|all}`, `mute{secs|null}`, `enable/disable{server}`, `remove{server}`. Config edits reuse existing `Engine` command path so persistence/validation stay in one place.
* **Fallback**: no GTK/popup binary -> `Activate` is a no-op and the native menu stays complete (v0.1.2 behaviour). CLI unchanged.
* **Network**: remote script appends `net_iface=<name> rx tx` for non-virtual interfaces from `/proc/net/dev`, plus a second reading in the 1 s sampling window when the first poll needs a CPU baseline; between polls the engine computes average RX/TX from counter deltas / elapsed wall time. Counter decrease / uptime drop / reconnect gap > 3 intervals -> N/A. Interface choice: per-server `net_interface` in config; default = interface of the default route (`/proc/net/route`), else the busiest physical-looking one (not `lo`, `docker*`, `veth*`, `br-*`, `virbr*`, `tailscale*`, `tun*`, `wg*`). Tailscale traffic is excluded by default, never summed.

## Rulings
* Ruling: no libadwaita — Debian 12 ships 1.2 (no `AdwOverlaySplitView`, weak CSS vars) and it adds ~8 MiB; plain GTK4 + own CSS keeps one codebase. Cost: manual dark/light colours.
* Ruling: popup is a normal toplevel, not a layer-shell surface — layer-shell is absent on GNOME (mutter) and on this machine. Cost: window placement chosen by compositor on Wayland.
* Ruling: command spool + SIGUSR1 rather than a D-Bus service in the daemon — avoids a second zbus object and keeps the daemon's idle memory/CPU unchanged. Cost: ~100 ms latency for actions.

## Delivered decisions and verification

- gtk4-rs 0.10 with GTK 4.8 API cap; optional Cargo `popup` feature and separate binary. No GTK linkage in the daemon.
- Single instance uses GtkApplication activation, not flock. Default desktop application ID: `io.github.MinhCYB.SVMonitor.Popup`; isolated configurations use a suffix.
- Tray coordinates are deliberately ignored on all desktops. Compositor controls placement.
- Network timing uses remote `/proc/uptime`, not local wall time. Every failed probe discards the previous network baseline; successful pairs over ten minutes are not averaged.
- Command capability is a backward-compatible snapshot field. Old daemons receive no SIGUSR1 from the popup; restart after upgrade. Startup purges old spool requests.
- Actual SNI activation and GTK actions passed against an isolated daemon; light/dark/HiDPI/stale/20-server surfaces captured. 142 tests passed with the popup feature; clippy clean.
- Final review found a Debian control heredoc defect. Corrected the duplicate opener and restored Maintainer; final package build is the acceptance check.
- Keep local branch `feat/dashboard-popup-v013`; no push, publish or merge requested.
