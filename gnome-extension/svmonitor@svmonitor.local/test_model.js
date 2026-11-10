// test_model.js - Pure model regression tests executable via `gjs -m test_model.js`
import {
    Overall,
    SEVERITY,
    parseSnapshot,
    formatBytes,
    formatBps,
    formatNetLine,
    formatCpu,
    formatMem,
    getWorstDisk,
    formatUptime,
    formatAge,
    getOverallBadge,
    getSummary,
    isValidServerName,
    getCommandArgv,
    isDaemonAlive,
    canExecuteCommands,
    canExecuteSsh,
} from './model.js';

let passed = 0;
let failed = 0;

function assert(condition, message) {
    if (!condition) {
        console.error(`FAIL: ${message}`);
        failed++;
        throw new Error(message);
    } else {
        passed++;
    }
}

function assertEquals(actual, expected, message) {
    if (actual !== expected) {
        console.error(`FAIL: ${message} (Expected: ${JSON.stringify(expected)}, Actual: ${JSON.stringify(actual)})`);
        failed++;
        throw new Error(`${message}: ${actual} !== ${expected}`);
    } else {
        passed++;
    }
}

function assertDeepEquals(actual, expected, message) {
    const actStr = JSON.stringify(actual);
    const expStr = JSON.stringify(expected);
    if (actStr !== expStr) {
        console.error(`FAIL: ${message} (Expected: ${expStr}, Actual: ${actStr})`);
        failed++;
        throw new Error(`${message}: ${actStr} !== ${expStr}`);
    } else {
        passed++;
    }
}

console.log('--- SVMonitor Model Regression Tests ---');

// 1. Snapshot parsing & size cap
{
    // Valid snapshot
    const json = JSON.stringify({
        ts: 1700000000,
        pid: 12345,
        local_net_ok: true,
        muted: false,
        command_capable: true,
        servers: [
            {
                name: 'web-prod',
                target: '10.0.0.1',
                enabled: true,
                overall: 'online',
                ssh: 'available',
                tailscale: 'peer_online',
                metrics: {
                    ts: 1700000000,
                    cpu_pct: 15.5,
                    cores: 4,
                    load: [0.62, 0.55, 0.48],
                    mem_total_kib: 16777216,
                    mem_used_kib: 4194304,
                    swap_total_kib: 4194304,
                    swap_used_kib: 0,
                    disks: [
                        { mount: '/', total_kib: 104857600, used_kib: 52428800 }
                    ],
                    uptime_secs: 864000,
                    net: {
                        iface: 'eth0',
                        rx_bps: 1048576,
                        tx_bps: 262144,
                        window_secs: 1.0,
                        sampled: true
                    }
                },
                last_check: 1700000000,
                completed_checks: 42,
                last_ok: 1700000000,
                down_since: null,
                detail: 'All systems normal',
                active_alerts: [],
                probing: false
            }
        ]
    });

    const snap = parseSnapshot(json);
    assert(snap !== null, 'parseSnapshot parses valid JSON');
    assertEquals(snap.pid, 12345, 'PID matches');
    assertEquals(snap.servers.length, 1, '1 server parsed');
    assertEquals(snap.servers[0].name, 'web-prod', 'Server name matches');
    assertEquals(snap.servers[0].overall, Overall.ONLINE, 'Overall online');
    assertEquals(snap.servers[0].metrics.net.sampled, true, 'Net sampled true');

    // Oversized input cap
    const hugeJson = json + ' '.repeat(300);
    const capped = parseSnapshot(hugeJson, 100); // 100 byte limit
    assertEquals(capped, null, 'Oversized input rejected by size cap');

    // Malformed JSON
    assertEquals(parseSnapshot('{ broken json '), null, 'Malformed JSON returns null');
    assertEquals(parseSnapshot(''), null, 'Empty input returns null');
    assertEquals(parseSnapshot(null), null, 'Null input returns null');
}

// 2. Offline / stale / dead daemon handling
{
    const summaryOffline = getSummary(null, false);
    assertEquals(summaryOffline.isDaemonAlive, false, 'Daemon marked dead');
    assertEquals(summaryOffline.onlineCount, 0, '0 online when dead');
    assertEquals(summaryOffline.statusText, 'Offline', 'Status text says Offline');

    const snapEmpty = parseSnapshot(JSON.stringify({
        ts: 1000,
        pid: 0,
        local_net_ok: false,
        servers: []
    }));
    const summaryEmpty = getSummary(snapEmpty, true);
    assertEquals(summaryEmpty.totalCount, 0, '0 total servers');
    assertEquals(summaryEmpty.statusText, 'All paused', 'Empty list status');
}

// 3. Disabled servers handling
{
    const snapDisabled = parseSnapshot(JSON.stringify({
        ts: 1700000000,
        pid: 123,
        command_capable: true,
        servers: [
            { name: 'srv1', enabled: false, overall: 'online' },
            { name: 'srv2', enabled: true, overall: 'online' }
        ]
    }));
    const summary = getSummary(snapDisabled, true);
    assertEquals(summary.totalCount, 2, '2 total servers');
    assertEquals(summary.enabledCount, 1, '1 enabled server');
    assertEquals(summary.onlineCount, 1, '1 online server');
    assertEquals(summary.statusText, '1/1 online', 'Count reflects enabled servers');

    const badgeDisabled = getOverallBadge(Overall.ONLINE, false);
    assertEquals(badgeDisabled.label, 'Paused', 'Disabled server badge says Paused');
    assertEquals(badgeDisabled.color, '#77767b', 'Disabled color matches');
}

// 4. Invalid and partial metrics
{
    // Null metrics
    const cpuNull = formatCpu(null);
    assertEquals(cpuNull.pct, 0, 'Null metrics CPU pct 0');
    assertEquals(cpuNull.text, '—', 'Null metrics CPU text em-dash');

    const memNull = formatMem(null);
    assertEquals(memNull.pct, 0, 'Null metrics RAM pct 0');
    assertEquals(memNull.text, '—', 'Null metrics RAM text em-dash');

    const netNull = formatNetLine(null);
    assertEquals(netNull.available, false, 'Null net unavailable');
    assertEquals(netNull.text, 'Net: —', 'Null net fallback string');

    // Partial metrics (NaN, negative, missing fields)
    const cpuPartial = formatCpu({ cpu_pct: NaN, load: [] });
    assertEquals(cpuPartial.pct, 0, 'NaN CPU pct returns 0');

    const memPartial = formatMem({ mem_total_kib: 0, mem_used_kib: 500 });
    assertEquals(memPartial.pct, 0, 'Zero total RAM returns 0%');

    // Disks empty or invalid
    assertEquals(getWorstDisk([]), null, 'Empty disks returns null');
    assertEquals(getWorstDisk(null), null, 'Null disks returns null');

    const worstDisk = getWorstDisk([
        { mount: '/boot', total_kib: 1000, used_kib: 200 },
        { mount: '/', total_kib: 10000, used_kib: 8500 }
    ]);
    assertEquals(worstDisk.mount, '/', 'Identifies / as worst disk');
    assertEquals(worstDisk.pct, 85, 'Worst disk pct 85%');

    // Uptime
    assertEquals(formatUptime(0), '—', '0 uptime returns em-dash');
    assertEquals(formatUptime(90000), '1d 1h', '90000s returns 1d 1h');
    assertEquals(formatUptime(3660), '1h 1m', '3660s returns 1h 1m');
    assertEquals(formatUptime(300), '5m', '300s returns 5m');
}

// 5. Network RX/TX formatting & explicit avg/sample
{
    const netSampled = {
        iface: 'eth0',
        rx_bps: 1500000,
        tx_bps: 350000,
        window_secs: 1.0,
        sampled: true
    };
    const resSampled = formatNetLine(netSampled);
    assert(resSampled.text.includes('(sample)'), 'Sampled net includes (sample)');
    assert(resSampled.text.includes('eth0:'), 'Includes interface name');
    assert(resSampled.text.includes('1.4 MB/s') || resSampled.text.includes('1.5 MB/s'), 'Formatted RX rate');

    const netAvg = {
        iface: '',
        rx_bps: 1024,
        tx_bps: 512,
        window_secs: 5.0,
        sampled: false
    };
    const resAvg = formatNetLine(netAvg);
    assert(resAvg.text.includes('(avg)'), 'Average net includes (avg)');
    assertEquals(resAvg.isSampled, false, 'isSampled is false');
}

// 6. Age calculations
{
    const now = 10000;
    assertEquals(formatAge(10000, now), '< 5s ago', 'Current time returns < 5s ago');
    assertEquals(formatAge(9970, now), '30s ago', '30s difference returns 30s ago');
    assertEquals(formatAge(9700, now), '5m ago', '300s difference returns 5m ago');
    assertEquals(formatAge(2800, now), '2h ago', '7200s difference returns 2h ago');
    assertEquals(formatAge(null, now), 'never', 'null ts returns never');
}

// 7. Command argv generation and injection safety
{
    const bin = '/home/user/.local/bin/svmonitor';

    // Refresh all
    assertDeepEquals(getCommandArgv(bin, 'check'), [bin, 'ui', 'check'], 'Check all argv');

    // Single server check
    assertDeepEquals(getCommandArgv(bin, 'check', 'alpha'), [bin, 'ui', 'check', 'alpha'], 'Check alpha argv');

    // Pause / Resume
    assertDeepEquals(getCommandArgv(bin, 'pause', 'beta'), [bin, 'ui', 'pause', 'beta'], 'Pause beta argv');
    assertDeepEquals(getCommandArgv(bin, 'resume', 'beta'), [bin, 'ui', 'resume', 'beta'], 'Resume beta argv');

    // Remove
    assertDeepEquals(getCommandArgv(bin, 'remove', 'gamma'), [bin, 'ui', 'remove', 'gamma'], 'Remove gamma argv');

    // Add
    assertDeepEquals(getCommandArgv(bin, 'add', 'delta'), [bin, 'ui', 'add', 'delta'], 'Add delta argv');

    // Settings
    assertDeepEquals(getCommandArgv(bin, 'settings'), [bin, 'ui', 'settings'], 'Settings argv');

    // SSH
    assertDeepEquals(getCommandArgv(bin, 'ssh', 'web-prod'), [bin, 'ssh', 'web-prod'], 'SSH argv');

    // Validation: invalid server names rejected
    assert(!isValidServerName(''), 'Empty name rejected');
    assert(!isValidServerName('srv; rm -rf /'), 'Semicolon rejected');
    assert(!isValidServerName('srv name'), 'Spaces rejected');
    assert(!isValidServerName('srv$foo'), 'Dollar sign rejected');
    assert(!isValidServerName('srv`whoami`'), 'Backticks rejected');
    assert(isValidServerName('web-prod.tailnet-123_test'), 'Valid complex name allowed');
    assert(isValidServerName('prod+eu'), 'Rust-valid alias punctuation preserved');
    assert(isValidServerName('a'.repeat(253)), 'Rust-valid 253-byte alias supported');
    assert(!isValidServerName('-option'), 'Leading option prefix rejected');

    let threw = false;
    try {
        getCommandArgv(bin, 'check', 'bad name!');
    } catch {
        threw = true;
    }
    assert(threw, 'Invalid server name throws in getCommandArgv');
}

// 8. Daemon liveness & gating
{
    assert(isDaemonAlive(1234, 'svmonitor\n'), 'Daemon alive with svmonitor comm');
    assert(isDaemonAlive(1234, 'svmonitor'), 'Daemon alive with bare svmonitor');
    assert(!isDaemonAlive(1234, 'bash\n'), 'Daemon dead if comm is bash');
    assert(!isDaemonAlive(0, 'svmonitor\n'), 'Daemon dead if pid <= 0');
    assert(!isDaemonAlive(1234, null), 'Daemon dead if comm is null');

    const snap = { command_capable: true };
    assert(canExecuteCommands(snap, true), 'Commands allowed when alive and capable');
    assert(!canExecuteCommands(snap, false), 'Commands denied when daemon dead');
    assert(!canExecuteCommands({ command_capable: false }, true), 'Commands denied when capable=false');

    assert(canExecuteSsh({ name: 'server1' }), 'SSH allowed for valid server');
    assert(!canExecuteSsh(null), 'SSH denied for null server');
    assert(!canExecuteSsh({ name: 'bad name;' }), 'SSH denied for bad name');
}

console.log(`\nAll tests completed: ${passed} passed, ${failed} failed.`);
if (failed > 0) {
    throw new Error(`${failed} tests failed`);
}
