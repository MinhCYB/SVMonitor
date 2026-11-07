# Compatibility and known limits — v0.1.2

## Distributions and builds

| Target | Evidence |
|---|---|
| CachyOS / Arch x86_64 | Final v0.1.2 built with `makepkg` as `svmonitor-0.1.2-1-x86_64.pkg.tar.zst`; 98 release unit tests passed. User-local installed v0.1.2 exercised on CachyOS. System-wide `pacman -U` was not run (requires root). |
| Debian 12 / 13 amd64 | Final v0.1.2 `.deb` built inside Debian 12 (glibc 2.36); binary references up to GLIBC_2.34, 98 release unit tests passed. v0.1.2 installation, CLI startup and removal tested in Debian 12/13 containers; repeated SIGHUP/graceful shutdown were tested on v0.1.1. Desktop sessions not tested there. |
| Ubuntu / other glibc distributions | Not tested. The `.deb` declares libc6 >= 2.36; do not assume installation works on releases with older glibc. Build from source for a different baseline. |
| aarch64 / musl | Not verified. |

Build tools: Rust/Cargo **1.89 or newer** for the committed dependency lockfile, plus a C linker.
Runtime links to libc and libgcc_s. The binary needs no GTK/Qt/OpenSSL/libdbus; zbus handles D-Bus in Rust.

## Desktop sessions

| Desktop | Tray | Verification |
|---|---|---|
| GNOME 51 / Wayland | Requires AppIndicator and KStatusNotifierItem Support extension | Real tray/submenus, SSH metrics, notifications and terminal launch exercised on CachyOS. |
| KDE Plasma | Native StatusNotifierItem host | Not tested directly. |
| XFCE/Cinnamon/MATE/Budgie, waybar/noctalia | Requires an appropriate StatusNotifier host/plugin | Not tested directly. |
| No tray host | Monitoring, notifications and CLI continue; icon attaches if a host appears | Headless paths tested; use `run --headless` explicitly without a desktop. |

The README screenshot is a real metrics submenu rendered from the running application's D-Bus menu
by a GTK/libdbusmenu host on CachyOS. Only that widget is captured: no server names, desktop windows
or personal paths are included. Popup styling differs between desktop hosts.

v0.1.2 validation additionally used a disposable 32-server simulated fleet, real D-Bus actions, and actual GTK/libdbusmenu widgets in Adwaita light/dark on XWayland. GNOME's live accessibility tree confirmed the new native menu labels and hierarchy; automated pointer injection did not open its popup reliably, so v0.1.2 visual captures use the GTK host, not a claimed GNOME-shell popup capture.

Known host quirk: GNOME AppIndicator may retain an extra underscore when one label contains multiple mnemonic escapes (notably page name ranges). The full server remains selectable; GTK/libdbusmenu displays the literal names. Configuration save errors are now surfaced on menu reopen, but inherited enable/remove mutations are not transactional with disk persistence.

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
