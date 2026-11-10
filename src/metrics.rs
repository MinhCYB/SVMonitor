//! Remote collection script and its parser. The script is POSIX `sh`, read-only,
//! uses shell builtins for /proc and a single `df`, and never needs sudo.
use crate::model::{DiskUsage, Metrics};

/// Body of the remote script. `script(sample)` prefixes `SVM_SAMPLE=0|1`; when 1 the script takes a
/// second CPU reading one second later so a single connection yields a CPU percentage.
pub const REMOTE_SCRIPT_BODY: &str = r#"LC_ALL=C; export LC_ALL
echo svm=1
{ read -r a b c d rest; echo "load=$a $b $c"; echo "procs=${d#*/}"; } </proc/loadavg 2>/dev/null
{ read -r up rest; echo "uptime=$up"; } </proc/uptime 2>/dev/null
{ read -r _h; read -r _h; while IFS=': ' read -r n rx _a _b _c _d _e _f _g tx rest; do echo "net=$n|$rx|$tx"; done; } </proc/net/dev 2>/dev/null
n=0
while read -r k u ni s i w irq sirq st rest; do
  case $k in
    cpu) echo "cpu=$u $ni $s $i $w $irq $sirq $st";;
    cpu[0-9]*) n=$((n+1));;
  esac
done </proc/stat 2>/dev/null
echo "cores=$n"
if [ "$SVM_SAMPLE" = 1 ]; then
  sleep 1
  while read -r k u ni s i w irq sirq st rest; do
    case $k in cpu) echo "cpu2=$u $ni $s $i $w $irq $sirq $st"; break;; esac
  done </proc/stat 2>/dev/null
  { read -r up rest; echo "uptime2=$up"; } </proc/uptime 2>/dev/null
  { read -r _h; read -r _h; while IFS=': ' read -r n rx _a _b _c _d _e _f _g tx rest; do echo "net2=$n|$rx|$tx"; done; } </proc/net/dev 2>/dev/null
fi
while read -r k v rest; do
  case $k in
    MemTotal:) echo "mem_total=$v";;
    MemAvailable:) echo "mem_avail=$v";;
    MemFree:) echo "mem_free=$v";;
    SwapTotal:) echo "swap_total=$v";;
    SwapFree:) echo "swap_free=$v";;
  esac
done </proc/meminfo 2>/dev/null
{ read -r _h; while read -r i d rest; do case $d in 00000000) echo "net_route=$i"; break;; esac; done; } </proc/net/route 2>/dev/null
{ df -P -k -x tmpfs -x devtmpfs -x squashfs -x overlay -x efivarfs -x iso9660 2>/dev/null || df -P -k / 2>/dev/null; } | { read -r hdr; while read -r fs t u a p m; do echo "disk=$t|$u|$m"; done; }
while IFS= read -r l; do case $l in PRETTY_NAME=*) echo "os=${l#PRETTY_NAME=}";; esac; done </etc/os-release 2>/dev/null
{ read -r k; echo "kernel=$k"; } </proc/sys/kernel/osrelease 2>/dev/null
{ read -r h; echo "hostname=$h"; } </proc/sys/kernel/hostname 2>/dev/null
echo svm_end=1
"#;

pub fn script(sample: bool) -> String {
    format!("SVM_SAMPLE={}\n{}", u8::from(sample), REMOTE_SCRIPT_BODY)
}

/// Cumulative byte counters of one interface (`/proc/net/dev`).
#[derive(Debug, Clone, PartialEq)]
pub struct NetCounters {
    pub iface: String,
    pub rx: u64,
    pub tx: u64,
}

/// Raw counters from one poll; CPU% and network rate need two of these.
#[derive(Debug, Clone, PartialEq)]
pub struct RawSample {
    pub ts: i64,
    pub cpu: Option<[u64; 8]>,
    /// Newest interface counters and the remote uptime (seconds, `/proc/uptime`) they were read at.
    /// Remote uptime is the clock: it is immune to SSH latency and to local clock changes.
    pub net: Option<NetCounters>,
    pub net_at: Option<f64>,
    pub metrics: Metrics,
}

/// Interfaces that are not the server's own traffic: loopback, container/VM bridges and tunnels.
/// Summing them would double count (a container packet crosses veth, bridge and the NIC) and a
/// Tailscale tunnel carries traffic that already also appears on the physical NIC.
fn is_virtual_iface(n: &str) -> bool {
    n == "lo"
        || [
            "docker",
            "veth",
            "br-",
            "virbr",
            "tailscale",
            "tun",
            "tap",
            "wg",
            "vnet",
            "cni",
            "flannel",
            "cali",
            "podman",
            "lxc",
            "zt",
            "dummy",
            "ifb",
            "bond_slave",
        ]
        .iter()
        .any(|p| n.starts_with(p))
}

/// Choose the interface to report: explicit override, else the default-route interface, else the
/// busiest non-virtual one. None when nothing qualifies (rather than inventing a number).
fn pick_iface(
    all: &[NetCounters],
    route: Option<&str>,
    wanted: Option<&str>,
) -> Option<NetCounters> {
    if let Some(w) = wanted {
        return all.iter().find(|c| c.iface == w).cloned();
    }
    if let Some(r) = route
        && let Some(c) = all.iter().find(|c| c.iface == r)
        && !is_virtual_iface(&c.iface)
    {
        return Some(c.clone());
    }
    all.iter()
        .filter(|c| !is_virtual_iface(&c.iface))
        .max_by_key(|c| c.rx.saturating_add(c.tx))
        .cloned()
}

/// Average rate between two readings of the same interface `secs` apart. None when time did not
/// advance, the interface differs, or a counter went backwards (reboot, driver reset, wrap).
pub fn net_rate(
    prev: &NetCounters,
    cur: &NetCounters,
    secs: f64,
    sampled: bool,
) -> Option<crate::model::NetRate> {
    if prev.iface != cur.iface || !secs.is_finite() || secs <= 0.0 {
        return None;
    }
    let rx = cur.rx.checked_sub(prev.rx)?;
    let tx = cur.tx.checked_sub(prev.tx)?;
    Some(crate::model::NetRate {
        iface: cur.iface.clone(),
        rx_bps: (rx as f64 / secs).round() as u64,
        tx_bps: (tx as f64 / secs).round() as u64,
        window_secs: secs as f32,
        sampled,
    })
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    NoMarker,
    Truncated,
    Missing(&'static str),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::NoMarker => write!(
                f,
                "remote output not recognised (is the remote shell POSIX?)"
            ),
            ParseError::Truncated => write!(f, "remote output truncated"),
            ParseError::Missing(k) => write!(f, "remote did not report {k}"),
        }
    }
}

const MAX_DISKS: usize = 8;

/// Remote supplied strings are untrusted display text.
fn clean(v: &str) -> String {
    crate::util::sanitize_text(v, 80)
}

/// Parse the script output. Unknown lines are ignored so MOTD noise cannot break parsing.
pub fn parse(output: &str, ts: i64) -> Result<RawSample, ParseError> {
    parse_with(output, ts, None)
}

/// `net_iface`: report exactly this interface (even a virtual one) instead of auto-selecting.
pub fn parse_with(output: &str, ts: i64, net_iface: Option<&str>) -> Result<RawSample, ParseError> {
    let mut saw_start = false;
    let mut saw_end = false;
    let mut load = None;
    let mut uptime = None;
    let mut cpu: Option<[u64; 8]> = None;
    let mut cpu2 = None;
    let (mut net1, mut net2): (Vec<NetCounters>, Vec<NetCounters>) = (vec![], vec![]);
    let (mut net_route, mut uptime2) = (None::<String>, None::<f64>);
    let mut uptime_f = None::<f64>;
    let mut cores = None;
    let mut procs = None;
    let (mut mem_total, mut mem_avail, mut mem_free) = (None, None, None);
    let (mut swap_total, mut swap_free) = (None, None);
    let mut disks = Vec::new();
    let (mut os, mut kernel, mut hostname) = (None, None, None);

    for line in output.lines() {
        let line = line.trim_end();
        if line == "svm=1" {
            saw_start = true;
            continue;
        }
        if !saw_start {
            continue; // banner / MOTD before our marker
        }
        if line == "svm_end=1" {
            saw_end = true;
            break;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        match k {
            "load" => {
                let p: Vec<f32> = v
                    .split_whitespace()
                    .filter_map(|x| x.parse().ok())
                    .collect();
                if p.len() == 3 {
                    load = Some([p[0], p[1], p[2]]);
                }
            }
            "procs" => procs = v.trim().parse::<u32>().ok(),
            "uptime" => {
                uptime_f = v
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|u| u.is_finite() && *u >= 0.0);
                uptime = uptime_f.map(|u| u as u64);
            }
            "cpu" => {
                let p: Vec<u64> = v
                    .split_whitespace()
                    .filter_map(|x| x.parse().ok())
                    .collect();
                if p.len() == 8 {
                    let mut a = [0u64; 8];
                    a.copy_from_slice(&p);
                    cpu = Some(a);
                }
            }
            "cpu2" => {
                let p: Vec<u64> = v
                    .split_whitespace()
                    .filter_map(|x| x.parse().ok())
                    .collect();
                if p.len() == 8 {
                    let mut a = [0u64; 8];
                    a.copy_from_slice(&p);
                    cpu2 = Some(a);
                }
            }
            "net" | "net2" => {
                let mut it = v.splitn(3, '|');
                if let (Some(n), Some(rx), Some(tx)) = (it.next(), it.next(), it.next())
                    && let (Ok(rx), Ok(tx)) = (rx.parse::<u64>(), tx.parse::<u64>())
                    && !n.is_empty()
                    && n.len() <= 32
                {
                    let c = NetCounters {
                        iface: clean(n),
                        rx,
                        tx,
                    };
                    let list = if k == "net" { &mut net1 } else { &mut net2 };
                    if list.len() < 64 {
                        list.push(c);
                    }
                }
            }
            "net_route" => net_route = Some(clean(v.trim())).filter(|s| !s.is_empty()),
            "uptime2" => {
                uptime2 = v
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|u| u.is_finite() && *u >= 0.0)
            }
            "cores" => cores = v.trim().parse::<u32>().ok().filter(|c| *c > 0),
            "mem_total" => mem_total = v.trim().parse::<u64>().ok(),
            "mem_avail" => mem_avail = v.trim().parse::<u64>().ok(),
            "mem_free" => mem_free = v.trim().parse::<u64>().ok(),
            "swap_total" => swap_total = v.trim().parse::<u64>().ok(),
            "swap_free" => swap_free = v.trim().parse::<u64>().ok(),
            "disk" => {
                if disks.len() < MAX_DISKS {
                    let mut it = v.splitn(3, '|');
                    if let (Some(t), Some(u), Some(m)) = (it.next(), it.next(), it.next())
                        && let (Ok(t), Ok(u)) = (t.parse::<u64>(), u.parse::<u64>())
                        && t > 0
                        && !m.is_empty()
                        && !disks.iter().any(|d: &DiskUsage| d.mount == m)
                    {
                        disks.push(DiskUsage {
                            mount: clean(m),
                            total_kib: t,
                            used_kib: u,
                        });
                    }
                }
            }
            "os" => os = Some(clean(v.trim().trim_matches('"'))).filter(|s| !s.is_empty()),
            "kernel" => kernel = Some(clean(v)).filter(|s| !s.is_empty()),
            "hostname" => hostname = Some(clean(v)).filter(|s| !s.is_empty()),
            _ => {}
        }
    }
    if !saw_start {
        return Err(ParseError::NoMarker);
    }
    if !saw_end {
        return Err(ParseError::Truncated);
    }
    let mem_total = mem_total.ok_or(ParseError::Missing("memory"))?;
    let mem_avail = mem_avail
        .or(mem_free)
        .ok_or(ParseError::Missing("memory"))?;
    let swap_total = swap_total.unwrap_or(0);
    let swap_free = swap_free.unwrap_or(swap_total);
    let mut m = Metrics {
        ts,
        cpu_pct: None,
        cores,
        load: load.ok_or(ParseError::Missing("load average"))?,
        mem_total_kib: mem_total,
        mem_used_kib: mem_total.saturating_sub(mem_avail),
        swap_total_kib: swap_total,
        swap_used_kib: swap_total.saturating_sub(swap_free),
        disks,
        uptime_secs: uptime.ok_or(ParseError::Missing("uptime"))?,
        procs,
        os,
        kernel,
        hostname,
        net: None,
    };
    if let (Some(a), Some(b)) = (cpu, cpu2) {
        m.cpu_pct = cpu_pct(&a, &b);
        cpu = Some(b);
    }
    // Network: a second in-probe reading (the 1 s CPU window) gives a near-real-time rate; the
    // newest reading is kept as the baseline for the between-probe average computed in the tracker.
    let first = pick_iface(&net1, net_route.as_deref(), net_iface);
    let (net, net_at) = match (&first, uptime_f, uptime2) {
        (Some(a), Some(t0), Some(t1)) if t1 > t0 => {
            let b = pick_iface(&net2, Some(a.iface.as_str()), Some(a.iface.as_str()));
            if let Some(b) = b {
                m.net = net_rate(a, &b, t1 - t0, true);
                (Some(b), Some(t1))
            } else {
                (first.clone(), Some(t0))
            }
        }
        (Some(a), Some(_), Some(t1)) => {
            let b = pick_iface(&net2, Some(a.iface.as_str()), Some(a.iface.as_str()));
            (b.or_else(|| first.clone()), Some(t1))
        }
        (Some(_), Some(t0), None) => (first.clone(), Some(t0)),
        _ => (None, None),
    };
    Ok(RawSample {
        ts,
        cpu,
        net,
        net_at,
        metrics: m,
    })
}

/// CPU busy percentage between two /proc/stat snapshots (user nice system idle iowait irq softirq steal).
pub fn cpu_pct(prev: &[u64; 8], cur: &[u64; 8]) -> Option<f32> {
    let total = |a: &[u64; 8]| a.iter().sum::<u64>();
    let idle = |a: &[u64; 8]| a[3] + a[4];
    let dt = total(cur).checked_sub(total(prev))?;
    let di = idle(cur).checked_sub(idle(prev))?;
    if dt == 0 || di > dt {
        return None;
    }
    Some(((dt - di) as f64 * 100.0 / dt as f64) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const FIXTURE: &str = "Welcome to a server\nsvm=1\nload=0.54 0.40 0.37\nprocs=525\nuptime=12345.67\ncpu=1000 0 500 8000 100 0 50 0\ncores=6\nmem_total=16000000\nmem_avail=4000000\nmem_free=1000000\nswap_total=2000000\nswap_free=1500000\ndisk=82446204|26197280|/\ndisk=1000|10|/boot with space\nos=\"Debian GNU/Linux 13 (trixie)\"\nkernel=6.12.41\nhostname=vps\nsvm_end=1\n";

    #[test]
    fn parses_fixture() {
        let r = parse(FIXTURE, 100).unwrap();
        let m = &r.metrics;
        assert_eq!(m.load, [0.54, 0.40, 0.37]);
        assert_eq!(m.cores, Some(6));
        assert_eq!(m.uptime_secs, 12345);
        assert_eq!(m.mem_used_kib, 12_000_000);
        assert!((m.mem_pct() - 75.0).abs() < 0.01);
        assert!((m.swap_pct().unwrap() - 25.0).abs() < 0.01);
        assert_eq!(m.disks.len(), 2);
        assert_eq!(m.disks[1].mount, "/boot with space");
        assert_eq!(m.os.as_deref(), Some("Debian GNU/Linux 13 (trixie)"));
        assert_eq!(m.procs, Some(525));
        assert_eq!(r.cpu, Some([1000, 0, 500, 8000, 100, 0, 50, 0]));
    }

    #[test]
    fn rejects_garbage_and_truncation() {
        assert_eq!(parse("hello\n", 0).unwrap_err(), ParseError::NoMarker);
        let cut = FIXTURE.replace("svm_end=1\n", "");
        assert_eq!(parse(&cut, 0).unwrap_err(), ParseError::Truncated);
        let nomem = "svm=1\nload=1 1 1\nuptime=5\nsvm_end=1\n";
        assert_eq!(parse(nomem, 0).unwrap_err(), ParseError::Missing("memory"));
    }

    #[test]
    fn remote_strings_cannot_inject_terminal_escapes() {
        let t = "svm=1\nload=1 1 1\nuptime=5\nmem_total=100\nmem_avail=50\nos=\"Evil\x1b]0;pwn\x07\"\nhostname=h\x1b[2J\ndisk=100|1|/mnt/\x1b[31m\nsvm_end=1\n";
        let m = parse(t, 0).unwrap().metrics;
        for s in [m.os.unwrap(), m.hostname.unwrap(), m.disks[0].mount.clone()] {
            assert!(!s.chars().any(|c| c.is_control()), "{s:?}");
        }
    }

    #[test]
    fn no_swap_and_mem_free_fallback() {
        let t = "svm=1\nload=1 1 1\nuptime=5\nmem_total=100\nmem_free=40\nsvm_end=1\n";
        let m = parse(t, 0).unwrap().metrics;
        assert_eq!(m.swap_total_kib, 0);
        assert_eq!(m.swap_pct(), None);
        assert_eq!(m.mem_used_kib, 60);
    }

    #[test]
    fn cpu_delta() {
        let a = [1000, 0, 500, 8000, 100, 0, 50, 0];
        let b = [1100, 0, 600, 8800, 100, 0, 50, 0];
        // dt = 1000, idle delta = 800 -> 20%
        assert!((cpu_pct(&a, &b).unwrap() - 20.0).abs() < 0.01);
        assert_eq!(cpu_pct(&b, &a), None); // counters went backwards (reboot)
        assert_eq!(cpu_pct(&a, &a), None);
    }

    #[test]
    fn script_runs_locally_and_parses() {
        // The remote script is plain POSIX sh; run it against this machine's /proc.
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(script(false))
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let r = parse(&text, 1).unwrap_or_else(|e| panic!("{e}: {text}"));
        assert!(r.metrics.mem_total_kib > 0);
        assert!(r.metrics.cores.unwrap() >= 1);
        assert!(!r.metrics.disks.is_empty());
        assert!(r.cpu.is_some());
    }

    #[test]
    fn sampling_mode_yields_cpu_percentage() {
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(script(true))
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let r = parse(&text, 1).unwrap();
        let c = r.metrics.cpu_pct.expect("cpu% from two readings");
        assert!((0.0..=100.0).contains(&c), "{c}");
    }

    #[test]
    fn script_is_posix_under_dash_if_present() {
        for sh in ["/bin/dash", "/usr/bin/dash", "/bin/busybox"] {
            if std::path::Path::new(sh).exists() {
                let mut c = std::process::Command::new(sh);
                if sh.ends_with("busybox") {
                    c.arg("sh");
                }
                let out = c.arg("-c").arg(script(false)).output().unwrap();
                parse(&String::from_utf8_lossy(&out.stdout), 1).unwrap();
            }
        }
    }

    const NET_SAMPLE: &str = "svm=1\nload=1 1 1\nuptime=500.00\nmem_total=100\nmem_avail=50\n\
        net_route=eth0\nnet=lo|900|900\nnet=eth0|1000|2000\nnet=docker0|5|5\nnet=veth12ab|7|7\n\
        net=tailscale0|300|400\nnet=br-1a2b3c|9|9\nnet=ens3|50|60\nsvm_end=1\n";

    #[test]
    fn net_counters_pick_default_route_and_ignore_virtual_interfaces() {
        let r = parse(NET_SAMPLE, 100).unwrap();
        let n = r.net.as_ref().unwrap();
        assert_eq!(n.iface, "eth0");
        assert_eq!((n.rx, n.tx), (1000, 2000));
    }

    #[test]
    fn net_without_default_route_prefers_physical_and_never_sums_tailscale() {
        let t = NET_SAMPLE.replace("net_route=eth0\n", "");
        let n = parse(&t, 0).unwrap().net.unwrap();
        assert_eq!(n.iface, "eth0", "largest physical-looking interface wins");
        let only_virtual = "svm=1\nload=1 1 1\nuptime=5\nmem_total=100\nmem_avail=50\n\
            net=lo|9|9\nnet=tailscale0|5|5\nnet=wg0|1|1\nnet=docker0|2|2\nsvm_end=1\n";
        assert!(parse(only_virtual, 0).unwrap().net.is_none());
    }

    #[test]
    fn net_interface_override_selects_exact_interface_even_if_virtual() {
        let r = parse_with(NET_SAMPLE, 0, Some("tailscale0")).unwrap();
        let n = r.net.unwrap();
        assert_eq!((n.iface.as_str(), n.rx, n.tx), ("tailscale0", 300, 400));
        assert!(
            parse_with(NET_SAMPLE, 0, Some("nope"))
                .unwrap()
                .net
                .is_none()
        );
    }

    #[test]
    fn net_rate_uses_counter_delta_over_elapsed_time() {
        let a = NetCounters {
            iface: "eth0".into(),
            rx: 1_000,
            tx: 5_000,
        };
        let b = NetCounters {
            iface: "eth0".into(),
            rx: 1_000 + 120_000,
            tx: 5_000 + 35_000,
        };
        let r = net_rate(&a, &b, 10.0, false).unwrap();
        assert_eq!((r.rx_bps, r.tx_bps), (12_000, 3_500));
        assert!(!r.sampled);
        assert!(
            net_rate(&a, &b, 0.0, false).is_none(),
            "no elapsed time -> N/A"
        );
        assert!(
            net_rate(&a, &b, -1.0, false).is_none(),
            "negative elapsed time -> N/A"
        );
        assert!(
            net_rate(&a, &b, f64::NAN, false).is_none(),
            "NaN elapsed time -> N/A"
        );
        assert!(
            net_rate(&a, &b, f64::INFINITY, false).is_none(),
            "Inf elapsed time -> N/A"
        );
    }

    #[test]
    fn net_rate_is_unavailable_after_counter_reset_or_interface_change() {
        let a = NetCounters {
            iface: "eth0".into(),
            rx: 9_000,
            tx: 9_000,
        };
        let reset = NetCounters {
            iface: "eth0".into(),
            rx: 100,
            tx: 9_500,
        };
        assert!(
            net_rate(&a, &reset, 10.0, false).is_none(),
            "rx went backwards: reboot/reset"
        );
        let other = NetCounters {
            iface: "ens3".into(),
            rx: 99_000,
            tx: 99_000,
        };
        assert!(
            net_rate(&a, &other, 10.0, false).is_none(),
            "different interface"
        );
    }

    #[test]
    fn net_sampling_window_is_marked_realtime() {
        let t = NET_SAMPLE.replace(
            "svm_end=1\n",
            "uptime2=501.00\nnet2=lo|900|900\nnet2=eth0|3048|2000\nsvm_end=1\n",
        );
        let r = parse(&t, 0).unwrap();
        let rate = r
            .metrics
            .net
            .clone()
            .expect("rate from the in-probe window");
        assert!(rate.sampled);
        assert_eq!(rate.iface, "eth0");
        assert_eq!((rate.rx_bps, rate.tx_bps), (2_048, 0));
        assert!((rate.window_secs - 1.0).abs() < 0.01);
        // The newest counters become the baseline for the next between-probe average.
        assert_eq!(r.net.unwrap().rx, 3048);
        assert!((r.net_at.unwrap() - 501.0).abs() < 0.001);
    }
}
