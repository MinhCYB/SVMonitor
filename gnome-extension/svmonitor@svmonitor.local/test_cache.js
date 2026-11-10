// Real Gio directory events; run inside Shell with the active extension.
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';

export async function run(extension) {
    const dir = GLib.dir_make_tmp('svmonitor-cache-test-XXXXXX');
    let reloads = 0;
    const harness = {
        _runtimeDir: dir,
        _cancellable: new Gio.Cancellable(),
        scheduleCacheReload() { reloads++; },
    };
    const settle = () => new Promise(resolve => GLib.timeout_add(GLib.PRIORITY_DEFAULT, 200, () => { resolve(); return GLib.SOURCE_REMOVE; }));
    const counts = [];
    try {
        Object.getPrototypeOf(extension)._watchDirectory.call(harness);
        for (let i = 0; i < 3; i++) {
            const tmp = `${dir}/.state.json.tmp`;
            GLib.file_set_contents(tmp, JSON.stringify({generation: i}));
            Gio.File.new_for_path(tmp).move(Gio.File.new_for_path(`${dir}/state.json`), Gio.FileCopyFlags.OVERWRITE, null, null);
            await settle();
            counts.push(reloads);
        }
        return {counts, passed: counts.every((n, i) => n > (i ? counts[i - 1] : 0))};
    } finally {
        harness._fileMonitor?.cancel();
        harness._cancellable.cancel();
        Gio.File.new_for_path(`${dir}/state.json`).delete(null);
        Gio.File.new_for_path(dir).delete(null);
    }
}
