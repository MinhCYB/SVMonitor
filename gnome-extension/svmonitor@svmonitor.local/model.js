// model.js - Pure presentation model and business logic for SVMonitor GNOME extension
// Independent of GNOME Shell UI; directly testable with `gjs -m test_model.js`.

export const Overall = Object.freeze({
    UNKNOWN: 'unknown',
    ONLINE: 'online',
    WARNING: 'warning',
    DOWN: 'down',
    ATTENTION: 'attention',
    LOCAL_ISSUE: 'local_issue',
    DISABLED: 'disabled',
});

export const SEVERITY = Object.freeze({
    [Overall.DISABLED]: 0,
    [Overall.UNKNOWN]: 1,
    [Overall.ONLINE]: 1,
    [Overall.LOCAL_ISSUE]: 2,
    [Overall.WARNING]: 3,
    [Overall.ATTENTION]: 4,
    [Overall.DOWN]: 5,
});

export const DEFAULT_MAX_SNAPSHOT_BYTES = 2 * 1024 * 1024; // 2 MiB

/**
 * Safely parse snapshot JSON data with size cap and field normalization.
 * Returns normalized Snapshot object, or null on error.
 */
export function parseSnapshot(raw, maxBytes = DEFAULT_MAX_SNAPSHOT_BYTES) {
    if (!raw) return null;

    let text;
    if (typeof raw === 'string') {
        text = raw;
    } else {
        try {
            text = new TextDecoder('utf-8').decode(raw);
        } catch {
            return null;
        }
    }

    if (text.length > maxBytes) {
        return null;
    }

    let data;
    try {
        data = JSON.parse(text);
    } catch {
        return null;
    }

    if (!data || typeof data !== 'object') {
        return null;
    }

    const ts = typeof data.ts === 'number' ? data.ts : 0;
    const pid = typeof data.pid === 'number' ? data.pid : 0;
    const local_net_ok = typeof data.local_net_ok === 'boolean' ? data.local_net_ok : true;
    const muted = Boolean(data.muted);
    const command_capable = Boolean(data.command_capable);

    const rawServers = Array.isArray(data.servers) ? data.servers : [];
    const servers = rawServers.map(normalizeServer).filter(Boolean);

    return {
        ts,
        pid,
        local_net_ok,
        muted,
        command_capable,
        servers,
    };
}

function normalizeServer(s) {
    if (!s || typeof s !== 'object' || typeof s.name !== 'string' || !s.name.trim()) {
        return null;
    }

    return {
        name: s.name.trim(),
        target: typeof s.target === 'string' && s.target ? s.target : s.name.trim(),
        enabled: s.enabled !== false,
        overall: normalizeOverall(s.overall),
        ssh: normalizeSsh(s.ssh),
        tailscale: typeof s.tailscale === 'string' ? s.tailscale : 'not_applicable',
        metrics: normalizeMetrics(s.metrics),
        last_check: typeof s.last_check === 'number' ? s.last_check : null,
        completed_checks: typeof s.completed_checks === 'number' ? s.completed_checks : 0,
        last_ok: typeof s.last_ok === 'number' ? s.last_ok : null,
        down_since: typeof s.down_since === 'number' ? s.down_since : null,
        detail: typeof s.detail === 'string' ? s.detail : '',
        active_alerts: Array.isArray(s.active_alerts) ? s.active_alerts.map(String) : [],
        probing: Boolean(s.probing),
    };
}

function normalizeOverall(o) {
    if (typeof o !== 'string') return Overall.UNKNOWN;
    const lower = o.toLowerCase();
    switch (lower) {
        case 'online': return Overall.ONLINE;
        case 'warning': return Overall.WARNING;
        case 'down': return Overall.DOWN;
        case 'attention': return Overall.ATTENTION;
        case 'local_issue':
        case 'localissue': return Overall.LOCAL_ISSUE;
        case 'disabled': return Overall.DISABLED;
        case 'unknown':
        default: return Overall.UNKNOWN;
    }
}

function normalizeSsh(ssh) {
    if (!ssh) return { state: 'unknown', reason: null };
    if (typeof ssh === 'string') return { state: ssh.toLowerCase(), reason: null };
    if (typeof ssh === 'object') {
        const keys = Object.keys(ssh);
        if (keys.length > 0) {
            const state = keys[0];
            const reason = ssh[state];
            return {
                state: state.toLowerCase(),
                reason: typeof reason === 'string' ? reason : null,
            };
        }
    }
    return { state: 'unknown', reason: null };
}

function normalizeMetrics(m) {
    if (!m || typeof m !== 'object') return null;

    return {
        ts: typeof m.ts === 'number' ? m.ts : 0,
        cpu_pct: typeof m.cpu_pct === 'number' && !isNaN(m.cpu_pct) ? m.cpu_pct : null,
        cores: typeof m.cores === 'number' && m.cores > 0 ? m.cores : null,
        load: Array.isArray(m.load) ? m.load.map(n => (typeof n === 'number' && !isNaN(n) ? n : 0)) : [0, 0, 0],
        mem_total_kib: typeof m.mem_total_kib === 'number' ? m.mem_total_kib : 0,
        mem_used_kib: typeof m.mem_used_kib === 'number' ? m.mem_used_kib : 0,
        swap_total_kib: typeof m.swap_total_kib === 'number' ? m.swap_total_kib : 0,
        swap_used_kib: typeof m.swap_used_kib === 'number' ? m.swap_used_kib : 0,
        disks: Array.isArray(m.disks) ? m.disks.map(normalizeDisk).filter(Boolean) : [],
        uptime_secs: typeof m.uptime_secs === 'number' ? m.uptime_secs : 0,
        procs: typeof m.procs === 'number' ? m.procs : null,
        os: typeof m.os === 'string' ? m.os : null,
        kernel: typeof m.kernel === 'string' ? m.kernel : null,
        hostname: typeof m.hostname === 'string' ? m.hostname : null,
        net: normalizeNet(m.net),
    };
}

function normalizeDisk(d) {
    if (!d || typeof d !== 'object' || typeof d.mount !== 'string') return null;
    return {
        mount: d.mount,
        total_kib: typeof d.total_kib === 'number' ? d.total_kib : 0,
        used_kib: typeof d.used_kib === 'number' ? d.used_kib : 0,
    };
}

function normalizeNet(n) {
    if (!n || typeof n !== 'object') return null;
    return {
        iface: typeof n.iface === 'string' ? n.iface : '',
        rx_bps: typeof n.rx_bps === 'number' ? n.rx_bps : 0,
        tx_bps: typeof n.tx_bps === 'number' ? n.tx_bps : 0,
        window_secs: typeof n.window_secs === 'number' ? n.window_secs : 1.0,
        sampled: Boolean(n.sampled),
    };
}

/**
 * Format raw byte count into human readable string (KB, MB, GB).
 */
export function formatBytes(bytes) {
    if (!Number.isFinite(bytes) || bytes <= 0) return '0 B';
    if (bytes < 1024) return `${Math.round(bytes)} B`;
    const kib = bytes / 1024;
    if (kib < 1024) return `${kib >= 100 ? Math.round(kib) : kib.toFixed(1)} KB`;
    const mib = kib / 1024;
    if (mib < 1024) return `${mib >= 100 ? Math.round(mib) : mib.toFixed(1)} MB`;
    const gib = mib / 1024;
    return `${gib.toFixed(1)} GB`;
}

/**
 * Format bytes per second into rate string (B/s, KB/s, MB/s).
 */
export function formatBps(bps) {
    if (!Number.isFinite(bps) || bps <= 0) return '0 B/s';
    return `${formatBytes(bps)}/s`;
}

/**
 * Format network throughput onto one line with explicit avg/sample indicator.
 * e.g. "RX 1.2 MB/s · TX 340 KB/s (avg)" or "RX 450 KB/s · TX 45 KB/s (sample)"
 */
export function formatNetLine(net) {
    if (!net || (net.rx_bps === 0 && net.tx_bps === 0 && !net.iface)) {
        return {
            text: 'Net: —',
            isSampled: false,
            available: false,
        };
    }

    const rx = formatBps(net.rx_bps);
    const tx = formatBps(net.tx_bps);
    const tag = net.sampled ? 'sample' : 'avg';
    const ifacePrefix = net.iface ? `${net.iface}: ` : '';

    return {
        text: `${ifacePrefix}↓ ${rx}  ↑ ${tx} (${tag})`,
        rxText: rx,
        txText: tx,
        isSampled: net.sampled,
        available: true,
    };
}

/**
 * Format CPU usage percentage and load average.
 */
export function formatCpu(metrics) {
    if (!metrics || !Number.isFinite(metrics.cpu_pct)) {
        return { pct: 0, text: '—', loadText: '' };
    }
    const pct = Math.min(100, Math.max(0, Math.round(metrics.cpu_pct)));
    const load1 = metrics.load && metrics.load[0] !== undefined ? metrics.load[0].toFixed(2) : null;
    const perCore = metrics.cores && load1 !== null ? (metrics.load[0] / metrics.cores).toFixed(2) : null;
    const loadText = perCore !== null ? `load ${perCore}/c` : (load1 !== null ? `load ${load1}` : '');

    return {
        pct,
        text: `${pct}%`,
        loadText,
    };
}

/**
 * Format RAM usage percentage and gigabytes used / total.
 */
export function formatMem(metrics) {
    if (!metrics || !Number.isFinite(metrics.mem_total_kib) || metrics.mem_total_kib <= 0 || !Number.isFinite(metrics.mem_used_kib)) {
        return { pct: 0, text: '—', detailText: '' };
    }
    const used = Math.max(0, metrics.mem_used_kib);
    const total = metrics.mem_total_kib;
    const pct = Math.min(100, Math.max(0, Math.round((used / total) * 100)));
    const usedGib = (used / (1024 * 1024)).toFixed(1);
    const totalGib = (total / (1024 * 1024)).toFixed(1);

    return {
        pct,
        text: `${pct}%`,
        detailText: `${usedGib} / ${totalGib} GB`,
    };
}

/**
 * Find the disk with highest usage percentage.
 */
export function getWorstDisk(disks) {
    if (!Array.isArray(disks) || disks.length === 0) return null;
    let worst = null;
    let highestPct = -1;

    for (const d of disks) {
        if (!d || !Number.isFinite(d.total_kib) || d.total_kib <= 0 || !Number.isFinite(d.used_kib)) continue;
        const p = (d.used_kib / d.total_kib) * 100;
        if (Number.isFinite(p) && p > highestPct) {
            highestPct = p;
            worst = {
                mount: d.mount,
                pct: Math.min(100, Math.max(0, Math.round(p))),
                total_kib: d.total_kib,
                used_kib: d.used_kib,
            };
        }
    }
    return worst;
}

/**
 * Format uptime seconds into days, hours, mins.
 */
export function formatUptime(uptimeSecs) {
    if (typeof uptimeSecs !== 'number' || uptimeSecs <= 0) return '—';
    const days = Math.floor(uptimeSecs / 86400);
    const hours = Math.floor((uptimeSecs % 86400) / 3600);
    const mins = Math.floor((uptimeSecs % 3600) / 60);

    if (days > 0) return `${days}d ${hours}h`;
    if (hours > 0) return `${hours}h ${mins}m`;
    return `${mins}m`;
}

/**
 * Format relative timestamp ago string.
 */
export function formatAge(epochSecs, nowSecs = Date.now() / 1000) {
    if (typeof epochSecs !== 'number' || epochSecs <= 0) return 'never';
    const diff = Math.max(0, Math.floor(nowSecs - epochSecs));
    if (diff < 5) return '< 5s ago';
    if (diff < 60) return `${diff}s ago`;
    const mins = Math.floor(diff / 60);
    if (mins < 60) return `${mins}m ago`;
    const hours = Math.floor(mins / 60);
    if (hours < 24) return `${hours}h ago`;
    const days = Math.floor(hours / 24);
    return `${days}d ago`;
}

/**
 * Overall badge styling and display text.
 */
export function getOverallBadge(overall, enabled = true) {
    if (!enabled) {
        return {
            label: 'Paused',
            cssClass: 'svmonitor-badge-disabled',
            dotClass: 'svmonitor-dot-disabled',
            color: '#77767b',
            severity: SEVERITY[Overall.DISABLED],
        };
    }

    switch (overall) {
        case Overall.ONLINE:
            return {
                label: 'Online',
                cssClass: 'svmonitor-badge-online',
                dotClass: 'svmonitor-dot-online',
                color: '#2ec27e',
                severity: SEVERITY[Overall.ONLINE],
            };
        case Overall.WARNING:
            return {
                label: 'Warning',
                cssClass: 'svmonitor-badge-warning',
                dotClass: 'svmonitor-dot-warning',
                color: '#f5c211',
                severity: SEVERITY[Overall.WARNING],
            };
        case Overall.DOWN:
            return {
                label: 'Down',
                cssClass: 'svmonitor-badge-down',
                dotClass: 'svmonitor-dot-down',
                color: '#e01b24',
                severity: SEVERITY[Overall.DOWN],
            };
        case Overall.ATTENTION:
            return {
                label: 'Attention',
                cssClass: 'svmonitor-badge-attention',
                dotClass: 'svmonitor-dot-attention',
                color: '#ff7800',
                severity: SEVERITY[Overall.ATTENTION],
            };
        case Overall.LOCAL_ISSUE:
            return {
                label: 'Local Issue',
                cssClass: 'svmonitor-badge-local',
                dotClass: 'svmonitor-dot-local',
                color: '#3584e4',
                severity: SEVERITY[Overall.LOCAL_ISSUE],
            };
        case Overall.UNKNOWN:
        default:
            return {
                label: 'Checking',
                cssClass: 'svmonitor-badge-unknown',
                dotClass: 'svmonitor-dot-unknown',
                color: '#9a9996',
                severity: SEVERITY[Overall.UNKNOWN],
            };
    }
}

/**
 * Returns overall summary statistics for header badge and panel indicator.
 */
export function getSummary(snapshot, daemonAlive = true) {
    if (!snapshot || !daemonAlive) {
        return {
            totalCount: snapshot?.servers?.length || 0,
            enabledCount: 0,
            onlineCount: 0,
            worstOverall: Overall.DOWN,
            isMuted: Boolean(snapshot?.muted),
            isDaemonAlive: Boolean(daemonAlive),
            statusText: daemonAlive ? 'No data' : 'Offline',
            badgeClass: 'svmonitor-badge-down',
        };
    }

    const servers = snapshot.servers || [];
    const enabledServers = servers.filter(s => s.enabled);
    const onlineServers = enabledServers.filter(
        s => s.overall === Overall.ONLINE || s.overall === Overall.WARNING
    );

    let worst = Overall.ONLINE;
    let maxSev = 0;
    for (const s of enabledServers) {
        const sev = SEVERITY[s.overall] || 0;
        if (sev > maxSev) {
            maxSev = sev;
            worst = s.overall;
        }
    }

    if (enabledServers.length === 0) {
        worst = Overall.DISABLED;
    }

    const badge = getOverallBadge(worst, true);

    let statusText;
    if (enabledServers.length === 0) {
        statusText = 'All paused';
    } else if (onlineServers.length === enabledServers.length) {
        statusText = `${onlineServers.length}/${enabledServers.length} online`;
    } else {
        const downCount = enabledServers.length - onlineServers.length;
        statusText = `${onlineServers.length}/${enabledServers.length} online (${downCount} issue${downCount > 1 ? 's' : ''})`;
    }

    return {
        totalCount: servers.length,
        enabledCount: enabledServers.length,
        onlineCount: onlineServers.length,
        worstOverall: worst,
        isMuted: snapshot.muted,
        isDaemonAlive: true,
        statusText,
        badgeClass: badge.cssClass,
        severity: badge.severity,
    };
}

/**
 * Server alias validation mirrors Rust config::validate_name (UTF-8 byte limit).
 */
export function isValidServerName(name) {
    if (typeof name !== 'string' || !name || new TextEncoder().encode(name).length > 253 || name.startsWith('-')) return false;
    return !/[\s\x00-\x1f\x7f-\x9f"'\\`$;&|<>*?!]/u.test(name);
}

/**
 * Produce typed Gio.Subprocess argv array without shell interpolation.
 * Actions: 'check' [NAME], 'pause' NAME, 'resume' NAME, 'remove' NAME, 'add' NAME, 'settings', 'ssh' NAME.
 */
export function getCommandArgv(binaryPath, action, serverName = null) {
    if (!binaryPath || typeof binaryPath !== 'string') {
        throw new Error('binaryPath is required');
    }

    switch (action) {
        case 'check':
            if (serverName) {
                if (!isValidServerName(serverName)) throw new Error(`Invalid server name: ${serverName}`);
                return [binaryPath, 'ui', 'check', serverName];
            }
            return [binaryPath, 'ui', 'check'];

        case 'pause':
            if (!isValidServerName(serverName)) throw new Error(`Invalid server name: ${serverName}`);
            return [binaryPath, 'ui', 'pause', serverName];

        case 'resume':
            if (!isValidServerName(serverName)) throw new Error(`Invalid server name: ${serverName}`);
            return [binaryPath, 'ui', 'resume', serverName];

        case 'remove':
            if (!isValidServerName(serverName)) throw new Error(`Invalid server name: ${serverName}`);
            return [binaryPath, 'ui', 'remove', serverName];

        case 'add':
            if (!isValidServerName(serverName)) throw new Error(`Invalid server name: ${serverName}`);
            return [binaryPath, 'ui', 'add', serverName];

        case 'settings':
            return [binaryPath, 'ui', 'settings'];

        case 'ssh':
            if (!isValidServerName(serverName)) throw new Error(`Invalid server name: ${serverName}`);
            return [binaryPath, 'ssh', serverName];

        default:
            throw new Error(`Unknown action: ${action}`);
    }
}

/**
 * Check if the daemon process is alive based on pid and /proc/[pid]/comm content.
 */
export function isDaemonAlive(pid, commContent) {
    if (typeof pid !== 'number' || pid <= 0) return false;
    if (typeof commContent !== 'string') return false;
    const trimmed = commContent.trim();
    return trimmed === 'svmonitor' || trimmed.startsWith('svmonitor');
}

/**
 * Determine if daemon command execution is available.
 */
export function canExecuteCommands(snapshot, daemonAlive) {
    return Boolean(daemonAlive && snapshot && snapshot.command_capable);
}

/**
 * Determine if SSH can be launched for a server (SSH is standalone, doesn't depend on daemon).
 */
export function canExecuteSsh(server) {
    return Boolean(server && isValidServerName(server.name));
}
