# Benchmarks

Machine: CachyOS (kernel 6.18.55-lts), 16 threads, GNOME/Wayland session, release build (`opt-level=s`, LTO, stripped; 3.0 MB).
Tool: `scripts/bench.sh` samples `/proc/<pid>` once per second; CPU = utime+stime of the daemon (and of its reaped ssh children,
from `cutime/cstime`) divided by wall time. One tick = 10 ms, so the resolution is about 0.01-0.03% over these windows; treat
values as upper-bound estimates, not precise.

Idle scenario = a real test VPS over SSH (Tailscale, 6-core VPS), poll interval 60 s, tray enabled (no tray host during the initial measurement).
Outage scenario = two unreachable targets (blackhole 192.0.2.1, refused 127.0.0.1:1), default retry/backoff.

| Scenario (final binary) | Duration | RSS avg / max | Threads max | FDs | Max ssh children | CPU daemon | CPU ssh children | CPU total |
|---|---|---|---|---|---|---|---|---|
| Idle, tray, 1 server @60 s | 240 s | 5.2 / 5.3 MiB | 9 | 10 | 1 | 0.008% | 0.029% | **0.037%** |
| Outage, tray, 2 servers down | 120 s | 5.5 / 5.5 MiB | 9 | 10 | 1 | 0.017% | 0.050% | **0.067%** |

Earlier build (before the security fixes) for reference: idle headless 4.4 MiB / 0.025%, idle tray 4.9 MiB / 0.028%.

Targets: RSS <= 20 MiB (**met, ~5.5 MiB**), CPU < 0.1% (**met in both scenarios**, with the caveat on tick resolution above).
The 9 threads are transient: ksni/zbus executor, notifier, signal thread, and probe worker + 3 pipe readers during the ~2 s
a probe lasts. Between probes only 2-5 remain.

## Leak soak
`scripts/soak.sh`: 3 servers with a fake ssh, interval 5 s (~12x the normal rate), 330 s ≈ 200 probe cycles:

| | start (t=30 s) | end (t=330 s) |
|---|---|---|
| RSS | 4684 KiB | 4696 KiB |
| open fds | 6 | 6 |
| threads | 2 | 2 |
| zombie children | | 0 |

History written: 24 KiB for the run (rotation at 2 MiB per file is covered by a unit test).

## Not measured
- Multi-day (24/7) runtime; only the ~6 minute soak above and accelerated polling.
- Memory with a tray host attached (none exists on this machine); the SNI/dbusmenu code runs either way, but a host adds a little D-Bus traffic.
- Remote-side cost: one `sh` + one `df` per poll (plus `sleep 1` on the first poll for a CPU sample). Not measured on the server.

## v0.1.1 re-measurement (after signal/lifecycle and terminal changes)
Same machine, scenarios and tooling as above; release binary `target/release/svmonitor` v0.1.1.

| Scenario | Duration | RSS avg / max | Threads max | FDs | CPU daemon | CPU ssh children | CPU total |
|---|---|---|---|---|---|---|---|
| Idle, tray, 1 server @60 s (real test VPS) | 240 s | 6.4 / 6.4 MiB | 9 | 10 | 0.013% | 0.025% | **0.037%** |
| Outage, tray, 2 servers down | 120 s | 6.3 / 6.5 MiB | 9 | 10 | 0.033% | 0.042% | **0.075%** |

Soak (3 fake servers, 5 s interval, 330 s): RSS 4600 -> 4608 KiB, fds 6 -> 6, threads 2 -> 2, zombies 0.

Versus v0.1.0: idle CPU unchanged (0.037%); RSS +~1 MiB (5.2 -> 6.4 MiB) and outage CPU +0.008 points (0.067 -> 0.075%), both within
the resolution of this method (10 ms ticks) and well inside the targets (RSS <= 20 MiB, CPU < 0.1%). The RSS difference was not
bisected; plausible contributors are the larger launcher module and a notifier thread now started with signals blocked, but that is
unverified.

## v0.1.2 tray UX re-measurement

Same machine and benchmark runner, installed final release binary 3,129,224 bytes, including review fixes. GNOME AppIndicator host attached; static rack pixmaps, grouped menu and bounded tooltips enabled. Both scenarios ran concurrently with desktop/build activity, not on a dedicated idle system.

| Scenario | Duration | RSS avg / max / final | Threads max | FDs | Max SSH children | CPU daemon | CPU SSH children | CPU total |
|---|---|---|---|---|---|---|---|---|
| Tray, real catplus, 60 s interval | 240 s | 4.3 / 6.6 / 4.8 MiB | 9 | 10 | 1 | 0.017% | 0.029% | 0.046% |
| Tray, two unreachable targets | 120 s | 5.5 / 6.8 / 4.8 MiB | 9 | 10 | 1 | 0.033% | 0.058% | 0.092% |

Both measured scenarios remain within the RSS/CPU targets. RSS residency varies under memory pressure; lower RSS is not evidence of an allocation reduction. CPU totals increased by 0.009/0.017 percentage points versus v0.1.1 during concurrent build activity; these tick-based short runs do not establish causation or multi-day leak freedom.

Additional native-menu smoke: 32 simulated servers (24 online, 2 refused, 2 authentication failures, 4 disabled), all reachable in the exported D-Bus hierarchy. A real D-Bus refresh click showed queued feedback and completed. GTK/libdbusmenu rendered the actual application's grouped navigation, disabled list and metrics/details in Adwaita light/dark on XWayland. The fake SSH fixture never contacted or interrupted a server. Installed binary v0.1.2 separately collected real catplus metrics and launched its SSH terminal.

## v0.1.3 optional dashboard

Release comparison on this CachyOS/GNOME Wayland machine. Private runtime/config directories,
fake SSH only; no server disruption. Each window: 30 seconds after a 3-second warm-up,
`/proc/<pid>/stat` CPU ticks, RSS sampled once per second. CPU percentages are one-core usage;
0.000% means no tick observed, not zero CPU. Concurrent desktop/build activity; short-run estimates.

| Version / scenario | Process | RSS average / maximum MiB | CPU self | CPU reaped children |
|---|---|---|---|---|
| v0.1.2, no servers, headless | daemon | 4.14 / 4.25 | 0.000% | 0.000% |
| v0.1.3, no servers, headless | daemon | 4.15 / 4.18 | 0.000% | 0.000% |
| v0.1.2, 20 simulated failures | daemon | 4.80 / 4.86 | 0.167% | 0.567% |
| v0.1.3, 20 simulated failures | daemon | 4.91 / 4.98 | 0.233% | 0.567% |
| v0.1.3, 20 healthy simulated servers, popup open | daemon | 4.49 / 4.72 | 0.000% | 0.000% |
| Same run, cairo GTK4 dashboard | popup | 50.09 / 57.41 | 0.167% | 0.000% |

The daemon remains below 20 MiB in these runs. The optional open GTK window is **not** within
the daemon's 20 MiB target. The 20-server failure scenario exceeds the 0.1% CPU goal; it is not
an idle measurement. Closing the last dashboard window exits its process. `ldd svmonitor`
shows libc/libgcc only, no GTK; GTK is linked only by `svmonitor-popup`.

Actual GNOME Wayland GTK surfaces were captured in light/dark themes, a 2x scale environment,
20-server scrolling and stale/offline state. Captures used temporary GTK snapshot instrumentation,
removed afterward; not panel-anchoring proof. Actual SNI Activate/SecondaryActivate opened one
dashboard; GTK buttons exercised mute, pause/resume and check-all against an isolated real daemon.
Read-only `catplus` check returned live CPU/RAM/disk and RX/TX. No multi-day leak claim.

## Native GNOME 51 panel dashboard

Current CachyOS GNOME 51.1 Wayland session, two real monitored servers. Samples taken inside
Shell from `/proc/self/stat` and `/proc/self/status`; CPU uses 100 Hz ticks and one-core percentage.
Popup state verified at both ends of each 10-second interval. Desktop and other extensions active;
these are **whole-Shell measurements**, not isolated SVMonitor overhead or an idle benchmark.

| Popup state | Duration | Whole-Shell RSS at end | Whole-Shell CPU |
|---|---|---|---|
| Open throughout | 9.999 s | 365.58 MiB | 12.201% |
| Closed throughout | 10.001 s | 363.98 MiB | 12.299% |

Daemon RSS separately observed at 4.0 MiB. Native UI adds no GTK helper process; GJS runs inside
Shell, so its private RSS cannot be inferred from the rows above. These short, non-isolated windows
do not prove the extension meets the daemon's CPU/RAM targets. Earlier samples where a screenshot
portal or input helper closed the popup were discarded. No multi-day runtime claim.

Live verification: two-server popup and SSH launch, Check/Refresh All, Pause/Resume, default config
editor, stale/offline fixtures, exact tray takeover/restore. Sixteen-server fixture measured a 520 px
scroll viewport within a 614 px menu with fixed 52 px footer. See [GNOME instructions](GNOME.md).
