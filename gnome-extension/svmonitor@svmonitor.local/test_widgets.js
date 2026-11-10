// Run inside GNOME Shell: import this module, then await run(indicator).
import St from 'gi://St';
import Clutter from 'gi://Clutter';
import GLib from 'gi://GLib';

export async function run(indicator) {
    const failures = [];
    let checks = 0;
    const check = (ok, message) => { checks++; if (!ok) failures.push(message); };
    const find = (root, predicate) => {
        const found = [];
        function walk(actor) {
            if (predicate(actor)) found.push(actor);
            for (const child of actor.get_children()) walk(child);
        }
        walk(root);
        return found;
    };
    const labels = root => find(root, actor => actor instanceof St.Label).map(actor => actor.text);
    const bars = root => find(root, actor => actor.has_style_class_name?.('svmonitor-progress-bg'));
    const settle = () => new Promise(resolve => GLib.timeout_add(GLib.PRIORITY_DEFAULT, 100, () => { resolve(); return GLib.SOURCE_REMOVE; }));
    const saved = indicator._expandedServer;
    const savedAlive = indicator._daemonAlive;
    const host = new St.BoxLayout({orientation: Clutter.Orientation.VERTICAL, width: 440, x: 20, y: 50});
    global.stage.add_child(host);
    try {
        for (const pct of [0, 1, 5, 10, 50, 90, 99, 100]) {
            const server = {
                name: 'widget-fixture', target: 'widget-fixture', enabled: true, overall: 'online',
                ssh: {state: 'available'}, tailscale: 'peer_online', last_check: Date.now() / 1000,
                metrics: {cpu_pct: pct, mem_used_kib: pct * 1048576, mem_total_kib: 100 * 1048576,
                    os: 'Fixture OS', uptime_secs: 3600,
                    disks: [{mount: '/', used_kib: pct * 1048576, total_kib: 100 * 1048576}]},
            };
            indicator._expandedServer = null;
            const collapsed = indicator._createCard(server, true);
            host.add_child(collapsed);
            await settle();
            check(bars(collapsed).length === 2, `${pct}% collapsed has only CPU/RAM bars`);
            check(!labels(collapsed).some(text => text.includes('Disk')), `${pct}% collapsed excludes Disk`);
            collapsed.destroy();
            indicator._expandedServer = server.name;
            const expanded = indicator._createCard(server, true);
            host.add_child(expanded);
            await settle();
            const tracks = bars(expanded);
            check(tracks.length === 3, `${pct}% expanded has CPU/RAM/Disk bars`);
            const diskTrack = tracks[2];
            check(Boolean(diskTrack) && Math.abs(diskTrack.width - diskTrack.get_parent().width) <= 1,
                `${pct}% Disk spans the full Details resource width`);
            check(tracks[0].width === 180 && tracks[1].width === 180,
                `${pct}% CPU/RAM retain their existing track width`);
            const text = labels(expanded).join(' ');
            check(text.includes(`${pct}.0 GiB / 100.0 GiB`) && text.includes(`${pct}%`), `${pct}% expanded Disk has used/total/percent`);
            for (const [i, track] of tracks.entries()) {
                const fill = track.get_first_child();
                const box = fill.get_allocation_box();
                check(pct === 0 ? !fill.visible : fill.visible && Math.abs(box.x1) < 0.01,
                    `${pct}% bar ${i} starts at left; zero is empty (x=${box.x1})`);
                check(pct === 0 || Math.abs(fill.width - track.width * pct / 100) <= 1,
                    `${pct}% bar ${i} width matches allocated track (${fill.width}/${track.width})`);
                check(track.height === 5 && (pct === 0 || fill.height === 5), `${pct}% bar ${i} shared height`);
            }
            expanded.destroy();
        }
        indicator._daemonAlive = false;
        indicator._expandedServer = 'stale-fixture';
        const stale = indicator._createCard({
            name: 'stale-fixture', target: 'stale-fixture', enabled: true, overall: 'down',
            ssh: {state: 'unavailable'}, tailscale: 'peer_offline',
            metrics: {cpu_pct: 5, mem_used_kib: 50, mem_total_kib: 100,
                disks: [{mount: '/', used_kib: 50 * 1048576, total_kib: 100 * 1048576}]},
        }, false);
        host.add_child(stale);
        await settle();
        check(labels(stale).includes('Last known values — stale'), 'Offline card marks retained values stale');
        check(bars(stale).length === 3 && labels(stale).includes('50.0 GiB / 100.0 GiB'), 'Offline expanded Disk retains capacity and bar');
        stale.destroy();
        indicator._expandedServer = null;
        for (let i = 0; i < 16; i++) {
            host.add_child(indicator._createCard({name: `many-${i}`, target: `many-${i}`,
                enabled: true, overall: 'online', ssh: {state: 'available'},
                metrics: {disks: [{mount: '/', used_kib: 10, total_kib: 100}]}}, false));
        }
        await settle();
        check(bars(host).length === 32, 'Sixteen collapsed servers have exactly two bars each');
        check(!labels(host).some(text => text.includes('Disk')), 'Multiserver collapsed cards exclude Disk');
    } finally {
        host.destroy();
        indicator._expandedServer = saved;
        indicator._daemonAlive = savedAlive;
    }
    return {checks, failures};
}
