# Compatibility and known limits — v0.1.3

## Distributions and builds

| Target | Evidence |
|---|---|
| CachyOS / Arch x86_64 | Packaging configuration provides split packages `svmonitor` (headless daemon) and `svmonitor-popup` (optional GTK4 dashboard popup). Daemon requires no GTK; popup requires `gtk4 >= 4.8`. |
| Debian 12 / 13 amd64 | Debian 12 (bookworm, glibc 2.36, GTK 4.8.3) and Debian 13 (trixie, GTK 4.18) packaging provided. Base `.deb` contains headless daemon with zero GTK dependencies; optional `svmonitor-popup` `.deb` requires `libgtk-4-1 (>= 4.8.0)`. |
| Ubuntu / other glibc distributions | Base package requires glibc >= 2.36 (Ubuntu 22.04+). GUI package requires GTK >= 4.8 runtime. |
| aarch64 / musl | Not verified. |

Build tools: Rust/Cargo **1.89 or newer** applies across the workspace (both daemon and popup) for the committed lockfile and dependencies. The popup GUI additionally requires GTK development headers >= 4.8 (configured with `gtk4` feature `v4_8`).
Runtime daemon links libc and libgcc_s; needs no GTK/Qt/OpenSSL/libdbus; zbus handles D-Bus in Rust.
## Desktop sessions

| Desktop | Tray | Verification |
|---|---|---|
| GNOME 51 / Wayland | Requires AppIndicator and KStatusNotifierItem Support extension | Real tray/submenus, SSH metrics, notifications and terminal launch exercised on CachyOS. |
| KDE Plasma | Native StatusNotifierItem host | Single-click `Activate` opens the dashboard popup directly. |
| XFCE/Cinnamon/MATE/Budgie, waybar/noctalia | Requires an appropriate StatusNotifier host/plugin | Not tested directly. |
| No tray host | Monitoring, notifications and CLI continue; icon attaches if host appears | Headless paths tested; use `run --headless` explicitly without desktop. |

## GTK4 Dashboard Popup

### Plain GTK4 without libadwaita
SVMonitor deliberately targets plain **GTK4 (>= 4.8) without libadwaita**:
- **Distro breadth:** Debian 12 (bookworm) ships libadwaita 1.2, Debian 13 ships 1.7, and Arch/CachyOS ships 1.10+. Debian 12's libadwaita 1.2 lacks modern split-view widgets and theme CSS variables. Plain GTK4 with feature cap `v4_8` ensures identical binary behavior across Debian 12/13, Ubuntu 22.04+, and Arch Linux.
- **Library footprint:** Omitting libadwaita avoids pulling in extra dependencies and unnecessary runtime overhead.
- **Theming & HiDPI:** Follows desktop theme preference via GTK settings / portal color-scheme preference, using standard theme-named CSS tokens (`@theme_fg_color`, `@theme_bg_color`). Light/dark styling and HiDPI rendering were verified on an actual Wayland GTK surface.
### Desktop activation quirks (GNOME vs KDE)
- **DBusMenu protocol limit:** The StatusNotifierItem / DBusMenu specifications support only text labels, icons, checkboxes, and submenus. Rich widgets (cards, progress bars, metric meters) cannot be drawn inside the native menu popup.
- **GNOME AppIndicator double-click:** Under GNOME Shell, the AppIndicator extension handles single left-click by toggling the native DBusMenu; it does **not** send `Activate` on single click. GNOME Shell's `indicatorStatusIcon.js` (`_maybeHandleDoubleClick`) sends `Activate` only on **double-click**, and middle-click sends `SecondaryActivate` (`XAyatanaSecondaryActivate`).
- **Three activation methods:**
  1. The first row in the native tray menu is always **"Open dashboard"** (single-click on any desktop, including GNOME).
  2. Double-click or middle-click on the GNOME tray icon.
  3. Single-click `Activate` on KDE Plasma and other compliant StatusNotifier hosts.
- **Fallback:** If `svmonitor-popup` is not installed or GUI build is disabled, "Open dashboard" is safely omitted or no-op, and the native menu submenus remain fully functional.

### Wayland compositor window placement limit
- **No client-side positioning:** On Wayland (specifically GNOME Shell / Mutter), client applications are strictly forbidden from specifying their own absolute window coordinates or anchoring to panel icons.
- **No layer-shell on GNOME:** Protocols such as `gtk4-layer-shell` or `wlr-layer-shell` are absent on standard GNOME Mutter.
- **Behavior & Geometry:** Undecorated, non-resizable 440×650 logical-pixel window. Close via **Close** or **Escape**; no auto-dismiss. The compositor chooses placement; tray click coordinates are not used.
- **Upgrade & Daemon capability handshake:** Running v0.1.2 daemons do not handle `SIGUSR1` command file draining. Upgrading requires a full daemon restart (`systemctl --user restart svmonitor.service` or restarting `svmonitor run`); `svmonitor reload` does not add signal handlers to an older process. The popup verifies daemon capabilities in the snapshot before dispatching actions, refusing mutations safely against older daemons rather than sending unhandled signals.

### Memory footprint separation
- **Independent process:** `svmonitor-popup` is an on-demand helper process, never linked into the monitoring daemon.
- **Process exits on close:** Closing the popup window exits the process completely (`exit(0)`), returning idle memory cost to 0 MiB.
- **Daemon RSS separation:** The daemon's idle memory footprint (~4-7 MiB RSS) remains entirely independent of whether the GTK popup window is open. GTK runtime allocations do not impact daemon footprint.
- **Cairo renderer:** The popup forces `GSK_RENDERER=cairo` (CPU rasterization), avoiding GL/ngl texture caches and reducing GTK4 window memory footprint.

- **Interface configuration:** Server blocks accept `net_interface = "name"` in `config.toml`.
- **Auto-detection:** If omitted, SVMonitor reads `/proc/net/route` on the remote host to pick the interface with the default route (`00000000`). If no default route is present, it picks the physical-looking interface with the highest combined RX+TX traffic.
- **Virtual interface exclusion:** Virtual and container interfaces (`lo`, `docker*`, `veth*`, `br-*`, `virbr*`, `tailscale*`, `tun*`, `wg*`) are ignored during auto-detection. Tailscale traffic is never summed into default traffic; monitor it explicitly by setting `net_interface = "tailscale0"`.
- **Sampling mechanics & qualifiers:**
 - The first poll (or when CPU needs a one-second baseline) collects two `/proc/net/dev` readings (`sampled = true`). Dashboard qualifier: `1s sample`; native tray qualifier: `live`.
  - Subsequent routine polls compute the rate from counter deltas divided by elapsed remote `/proc/uptime` (`sampled = false`). Remote uptime is monotonic and immune to SSH network latency or local clock shifts. Displayed with the `avg` qualifier.

## Default terminal

With `general.terminal = ""`, resolution is:

1. `xdg-terminal-exec`, following XDG terminal preferences and desktop entries.
2. Native GNOME/Unity `gsettings` terminal, only in those desktop sessions.
3. Explicit `$TERMINAL` environment preference.
4. Debian's `x-terminal-emulator` alternative.
5. An actionable error; no arbitrary list of installed emulators is scanned.

An explicit SVMonitor terminal override wins. It is parsed as argv (quotes/escapes, no shell).
GNOME Console selected through `xdg-terminal-exec`, the real tray action and CLI launcher were tested
on GNOME 51 Wayland. Quoted paths/arguments were checked with a recording terminal wrapper.
KDE/Konsole, XFCE and distro fallback execution were not tested live; resolution edge cases have unit tests.
KDE's `kdeglobals` is not parsed by SVMonitor: use the XDG dispatcher or an explicit override.
The XDG default-terminal specification is still a proposal; SVMonitor delegates interpretation to
[the dispatcher](https://github.com/Vladimir-csp/xdg-terminal-exec), not a second custom desktop-entry parser.

## Current limits

- Remote systems must be Linux with POSIX `sh`, `/proc` and `df`; BSD/macOS metrics are unsupported.
  BusyBox portability has not been verified on a real remote host.
- Passphrase-protected keys must already be available through ssh-agent. Unknown host keys must be
  verified interactively first. Probes never prompt or bypass host-key verification.
- Each server uses one short-lived SSH probe per poll; an existing OpenSSH multiplexed master may
  be reused, but SVMonitor does not start persistent masters.
- CPU's first poll uses a one-second remote sample; subsequent polls use counter deltas.
- At most eight disk rows are retained; pseudo-filesystems are excluded where `df` supports it.
- Local network attribution checks default routes and local Tailscale state, not a full Internet
  connectivity proof. A default route can exist while upstream connectivity is broken.
- No graphs or remote administration actions. History is local, size-rotated JSONL.
- Measurements cover minutes and accelerated probes, not multi-day 24/7 operation.
- Source and packaging scripts are public; no binary release or package repository is provided here.
