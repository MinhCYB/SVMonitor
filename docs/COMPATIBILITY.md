# Compatibility and known limits

## Distributions
| Target | Status |
|---|---|
| CachyOS / Arch | **Built and tested on CachyOS** (this machine): `makepkg` → `svmonitor-0.1.0-1-x86_64.pkg.tar.zst`; runtime deps `openssh gcc-libs glibc`. Package contents verified; *not installed with pacman* (needs root). |
| Debian 12 / 13 | `.deb` built **inside a Debian 12 container** (glibc 2.36, binary needs ≥ GLIBC_2.34), unit tests run there. Install/remove test: see below. Ubuntu 22.04+ shares glibc ≥ 2.35 → expected to work, **not tested**. |
| Other | Any glibc ≥ 2.34 x86_64 with OpenSSH. musl/aarch64: `cargo build` should work, **not tested**. |

Binary: dynamic against libc and libgcc_s only. No GTK/Qt/OpenSSL/libdbus: D-Bus via pure-Rust zbus.

## Desktops
| DE | Tray | Notifications |
|---|---|---|
| KDE Plasma | native StatusNotifierItem | native (**not tested here**) |
| GNOME | needs *AppIndicator and KStatusNotifierItem Support* extension (not installed on the test machine → icon cannot be shown; verified with a fake watcher on a private bus) | **verified on real GNOME 3x/Wayland session** |
| XFCE/Cinnamon/MATE/Budgie, waybar/noctalia | StatusNotifier hosts exist; **not tested** | standard |
| No tray host | automatic headless mode; `status`, `check`, notifications keep working | — |

## Limits
- Remote hosts must be Linux with a POSIX `sh`, `/proc`, `df` (BusyBox ok in principle, untested). macOS/BSD targets unsupported.
- Passphrase-protected keys must be in ssh-agent; probes never prompt. Unknown host keys must be accepted once manually.
- One `ssh` process per server per poll; with `ControlMaster` configured by the user it is reused, SVMonitor never starts masters itself.
- CPU% on the very first poll uses a 1 s on-server sample; later polls use deltas between polls.
- Disk list capped at 8 filesystems; tmpfs/overlay/squashfs excluded.
- No graphs, no remote actions, no multi-user daemon. History is local JSONL.
- Tray icon is a single coloured dot (worst state) drawn in-process; per-server state is in the menu/tooltip.
- `.deb`/PKGBUILD are unsigned and local; nothing is published.

## Default terminal ("Open SSH")
Resolution order when `general.terminal` is empty: `xdg-terminal-exec` (follows `xdg-terminals.list` / desktop entries, so GNOME, KDE and
others get *their* terminal) -> GNOME/Unity `gsettings` terminal (only when `XDG_CURRENT_DESKTOP` names GNOME/Unity) -> `$TERMINAL` ->
Debian `x-terminal-emulator` -> error. There is no built-in priority list of emulators. An explicit `general.terminal` always wins and is
parsed as an argv (quotes/escapes, no shell).
- **Verified here (CachyOS, GNOME 51 Wayland):** with `xdg-terminal-exec` resolves to `org.gnome.Console` (kgx); tray click and `svmonitor ssh` both open it with `ssh -- catplus` as a child; explicit override with a spaced path and quoted args delivers the exact argv.
- **Not verified:** KDE/Konsole, XFCE, Debian `x-terminal-emulator`, `$TERMINAL`, non-GNOME `gsettings` behaviour (covered by unit tests with injected environments only).
- `xdg-terminal-exec` is an optional dependency (Arch `extra/xdg-terminal-exec`; Debian package availability not checked). GNOME 51 here points its terminal setting at `xdg-terminal-exec` but did not have it installed; without it (or an override) Open SSH reports an actionable error instead of guessing a terminal.
- KDE's `kdeglobals` `TerminalApplication` is deliberately not parsed; KDE users get their terminal through `xdg-terminal-exec` or an override.
