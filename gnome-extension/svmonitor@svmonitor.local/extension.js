// extension.js - Native GNOME 51 Panel Extension for SVMonitor
import { Extension } from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';
import St from 'gi://St';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Clutter from 'gi://Clutter';
import GObject from 'gi://GObject';

import {
    DEFAULT_MAX_SNAPSHOT_BYTES,
    parseSnapshot,
    formatNetLine,
    formatCpu,
    formatMem,
    getWorstDisk,
    formatUptime,
    formatAge,
    getOverallBadge,
    getSummary,
    getCommandArgv,
    isDaemonAlive,
    canExecuteCommands,
    canExecuteSsh,
} from './model.js';

const SvmonitorIndicator = GObject.registerClass({GTypeName: 'SvmonitorNativeIndicator'},
class SvmonitorIndicator extends PanelMenu.Button {
    _init(extension) {
        super._init(0.5, 'SVMonitor', false);
        this._extension = extension;

        this._snapshot = null;
        this._daemonAlive = false;
        this._expandedServer = null;
        this._managedServer = null;
        this._confirmingRemoveServer = null;
        this._confirmTimeoutId = 0;
        this._relativeTimerId = 0;

        // Panel Icon
        this._icon = new St.Icon({
            icon_name: 'network-server-symbolic',
            style_class: 'system-status-icon',
        });
        this.add_child(this._icon);

        // Menu container styling - width fixed to 440px on menu.box
        this.menu.box.add_style_class_name('svmonitor-menu-box');
        this.menu.box.set_style('width: 440px; min-width: 440px; max-width: 440px;');

        this._buildUi();

        this.menu.connect('open-state-changed', (_menu, isOpen) => {
            this._onDashboardOpenChanged(isOpen);
        });
    }

    _buildUi() {
        // 1. Header (fixed)
        const headerItem = new PopupMenu.PopupBaseMenuItem({
            reactive: false,
            can_focus: false,
            activate: false,
            style_class: 'svmonitor-menu-item',
        });

        this._headerBox = new St.BoxLayout({
            style_class: 'svmonitor-header',
            x_expand: true,
        });

        this._titleLabel = new St.Label({
            text: 'SVMonitor',
            style_class: 'svmonitor-header-title',
            y_align: Clutter.ActorAlign.CENTER,
        });

        this._summaryBadge = new St.Label({
            text: 'Connecting...',
            style_class: 'svmonitor-badge svmonitor-badge-unknown',
            y_align: Clutter.ActorAlign.CENTER,
        });

        this._headerBox.add_child(this._titleLabel);
        this._headerBox.add_child(new St.Widget({ x_expand: true }));
        this._headerBox.add_child(this._summaryBadge);
        headerItem.add_child(this._headerBox);
        this.menu.addMenuItem(headerItem);

        // 2. Scrollable Server List
        const scrollItem = new PopupMenu.PopupBaseMenuItem({
            reactive: false,
            can_focus: false,
            activate: false,
            style_class: 'svmonitor-menu-item',
        });

        this._scrollView = new St.ScrollView({
            style_class: 'svmonitor-scrollview',
            hscrollbar_policy: St.PolicyType.NEVER,
            vscrollbar_policy: St.PolicyType.AUTOMATIC,
        });

        this._cardsContainer = new St.BoxLayout({
            orientation: Clutter.Orientation.VERTICAL,
            style_class: 'svmonitor-cards-container',
            x_expand: true,
        });

        if (typeof this._scrollView.set_child === 'function') {
            this._scrollView.set_child(this._cardsContainer);
        } else if (typeof this._scrollView.add_child === 'function') {
            this._scrollView.add_child(this._cardsContainer);
        } else {
            this._scrollView.add_actor(this._cardsContainer);
        }
        scrollItem.add_child(this._scrollView);
        this.menu.addMenuItem(scrollItem);

        // 3. Footer (fixed)
        const footerItem = new PopupMenu.PopupBaseMenuItem({
            reactive: false,
            can_focus: false,
            activate: false,
            style_class: 'svmonitor-menu-item',
        });

        this._footerBox = new St.BoxLayout({
            style_class: 'svmonitor-footer',
            x_expand: true,
        });

        this._feedbackLabel = new St.Label({
            text: '',
            style_class: 'svmonitor-footer-feedback',
            y_align: Clutter.ActorAlign.CENTER,
            x_expand: true,
        });

        this._settingsBtn = new St.Button({
            label: 'Settings',
            style_class: 'svmonitor-btn',
            can_focus: true,
        });
        this._settingsBtn.connect('clicked', () => {
            this._extension.runUiAction('settings');
        });

        this._refreshAllBtn = new St.Button({
            label: 'Refresh All',
            style_class: 'svmonitor-btn svmonitor-btn-primary',
            can_focus: true,
        });
        this._refreshAllBtn.connect('clicked', () => {
            this._extension.runUiAction('check');
        });

        this._footerBox.add_child(this._feedbackLabel);
        this._footerBox.add_child(this._settingsBtn);
        this._footerBox.add_child(this._refreshAllBtn);
        footerItem.add_child(this._footerBox);
        this.menu.addMenuItem(footerItem);

        this._renderEmpty();
    }

    _onDashboardOpenChanged(isOpen) {
        if (isOpen) {
            this._extension.scheduleCacheReload();
            this._extension.checkDaemonLivenessNow();
            this._startRelativeTimer();
        } else {
            this._stopRelativeTimer();
            this._resetConfirmStates();
        }
    }

    _startRelativeTimer() {
        if (this._relativeTimerId) return;
        this._relativeTimerId = this._extension.addTimeout(GLib.PRIORITY_DEFAULT, 2000, () => {
            this._updateRelativeTimes();
            this._extension.checkDaemonLivenessNow();
            return GLib.SOURCE_CONTINUE;
        });
    }

    _stopRelativeTimer() {
        if (this._relativeTimerId) {
            this._extension.removeTimeout(this._relativeTimerId);
            this._relativeTimerId = 0;
        }
    }

    _resetConfirmStates() {
        if (this._confirmTimeoutId) {
            this._extension.removeTimeout(this._confirmTimeoutId);
            this._confirmTimeoutId = 0;
        }
        this._confirmingRemoveServer = null;
        this._renderCards();
    }

    updateState(snapshot, daemonAlive) {
        this._snapshot = snapshot;
        this._daemonAlive = daemonAlive;

        // Update header summary
        const summary = getSummary(this._snapshot, this._daemonAlive);
        this._summaryBadge.set_text(summary.statusText);
        this._summaryBadge.set_style_class_name(`svmonitor-badge ${summary.badgeClass}`);

        // Update footer buttons sensitivity
        const canCmd = canExecuteCommands(this._snapshot, this._daemonAlive);
        this._refreshAllBtn.reactive = canCmd;
        if (canCmd) {
            this._refreshAllBtn.remove_style_class_name('svmonitor-btn-disabled');
        } else {
            this._refreshAllBtn.add_style_class_name('svmonitor-btn-disabled');
        }

        this._renderCards();
    }

    updateFeedback(text, isError) {
        if (!this._feedbackLabel) return;
        this._feedbackLabel.set_text(text);
        if (isError) {
            this._feedbackLabel.add_style_class_name('svmonitor-footer-feedback-error');
        } else {
            this._feedbackLabel.remove_style_class_name('svmonitor-footer-feedback-error');
        }
    }

    _renderEmpty() {
        this._cardsContainer.destroy_all_children();
        const emptyLabel = new St.Label({
            text: 'Awaiting SVMonitor daemon data...',
            style_class: 'svmonitor-age-row',
            x_align: Clutter.ActorAlign.CENTER,
            y_align: Clutter.ActorAlign.CENTER,
        });
        this._cardsContainer.add_child(emptyLabel);
    }

    _renderCards() {
        this._cardsContainer.destroy_all_children();

        const servers = this._snapshot?.servers || [];
        if (servers.length === 0) {
            const msg = this._daemonAlive ? 'No servers configured' : 'Daemon offline';
            const emptyLabel = new St.Label({
                text: msg,
                style_class: 'svmonitor-age-row',
                x_align: Clutter.ActorAlign.CENTER,
                y_align: Clutter.ActorAlign.CENTER,
            });
            this._cardsContainer.add_child(emptyLabel);
            return;
        }

        const canCmd = canExecuteCommands(this._snapshot, this._daemonAlive);

        for (const server of servers) {
            const card = this._createCard(server, canCmd);
            this._cardsContainer.add_child(card);
        }
    }

    _createProgress(pct, fullWidth = false) {
        const fraction = Number.isFinite(pct) ? Math.min(100, Math.max(0, pct)) / 100 : 0;
        // St.Bin centers its child; a horizontal box allocates from the LTR edge.
        const track = new St.BoxLayout({
            style_class: 'svmonitor-progress-bg',
            x_align: fullWidth ? Clutter.ActorAlign.FILL : Clutter.ActorAlign.START,
            x_expand: true,
        });
        track.set_text_direction(Clutter.TextDirection.LTR);
        if (!fullWidth) track.set_style('width: 180px; min-width: 180px;');
        const severity = pct >= 90 ? 'svmonitor-progress-fill-critical'
            : (pct >= 75 ? 'svmonitor-progress-fill-warning' : '');
        const fill = new St.Widget({
            style_class: `svmonitor-progress-fill ${severity}`,
            x_expand: false,
            x_align: Clutter.ActorAlign.START,
            visible: fraction > 0,
        });
        track.add_child(fill);
        track.connect('notify::allocation', () => {
            fill.set_width(track.width * fraction);
        });
        return track;
    }

    _createCard(server, canCmd) {
        const isExpanded = this._expandedServer === server.name;
        const badge = getOverallBadge(server.overall, server.enabled);

        const cardBox = new St.BoxLayout({
            orientation: Clutter.Orientation.VERTICAL,
            style_class: `svmonitor-card ${isExpanded ? 'svmonitor-card-expanded' : ''} ${!server.enabled ? 'svmonitor-card-disabled' : ''}`,
            x_expand: true,
        });

        // Top toggle button containing the summary card surface
        const toggleBtn = new St.Button({
            style_class: 'svmonitor-card-toggle',
            can_focus: true,
            x_expand: true,
        });
        const toggleInner = new St.BoxLayout({
            orientation: Clutter.Orientation.VERTICAL,
            x_expand: true,
        });
        const titleRow = new St.BoxLayout({ x_expand: true });
        const dot = new St.Widget({
            style_class: `svmonitor-status-dot ${badge.dotClass}`,
            y_align: Clutter.ActorAlign.CENTER,
        });
        const nameLabel = new St.Label({
            text: server.name,
            style_class: 'svmonitor-card-name',
            y_align: Clutter.ActorAlign.CENTER,
        });
        const targetLabel = new St.Label({
            text: server.target !== server.name ? `(${server.target})` : '',
            style_class: 'svmonitor-card-target',
            y_align: Clutter.ActorAlign.CENTER,
        });
        const statusBadge = new St.Label({
            text: badge.label,
            style_class: `svmonitor-badge ${badge.cssClass}`,
            y_align: Clutter.ActorAlign.CENTER,
        });

        titleRow.add_child(dot);
        titleRow.add_child(nameLabel);
        if (server.target !== server.name) {
            titleRow.add_child(targetLabel);
        }
        titleRow.add_child(new St.Widget({ x_expand: true }));
        titleRow.add_child(statusBadge);
        toggleInner.add_child(titleRow);

        // Row 2: CPU / RAM Equal Two Columns
        const colsRow = new St.BoxLayout({
            style_class: 'svmonitor-columns',
            x_expand: true,
        });

        // CPU Column
        const cpuInfo = formatCpu(server.metrics);
        const cpuCol = new St.BoxLayout({
            orientation: Clutter.Orientation.VERTICAL,
            style_class: 'svmonitor-col',
            x_expand: true,
        });
        const cpuHeader = new St.BoxLayout({ x_expand: true });
        cpuHeader.add_child(new St.Label({ text: 'CPU', style_class: 'svmonitor-col-header' }));
        cpuHeader.add_child(new St.Widget({ x_expand: true }));
        cpuHeader.add_child(new St.Label({ text: cpuInfo.text, style_class: 'svmonitor-col-value' }));

        const cpuProgressBg = this._createProgress(cpuInfo.pct);

        cpuCol.add_child(cpuHeader);
        cpuCol.add_child(cpuProgressBg);

        // RAM Column
        const memInfo = formatMem(server.metrics);
        const memCol = new St.BoxLayout({
            orientation: Clutter.Orientation.VERTICAL,
            style_class: 'svmonitor-col',
            x_expand: true,
        });
        const memHeader = new St.BoxLayout({ x_expand: true });
        memHeader.add_child(new St.Label({ text: 'RAM', style_class: 'svmonitor-col-header' }));
        memHeader.add_child(new St.Widget({ x_expand: true }));
        memHeader.add_child(new St.Label({ text: memInfo.text, style_class: 'svmonitor-col-value' }));
        const memProgressBg = this._createProgress(memInfo.pct);

        memCol.add_child(memHeader);
        memCol.add_child(memProgressBg);

        colsRow.add_child(cpuCol);
        colsRow.add_child(memCol);
        toggleInner.add_child(colsRow);

        // Row 3: Network RX/TX on same line with explicit avg/sample
        const fresh = this._daemonAlive && server.enabled && server.ssh.state === 'available';
        const netInfo = fresh ? formatNetLine(server.metrics?.net) : {text: 'Last known values — stale'};
        const netRow = new St.BoxLayout({ style_class: 'svmonitor-net-row', x_expand: true });
        netRow.add_child(new St.Label({ text: netInfo.text, style_class: 'svmonitor-net-row' }));
        toggleInner.add_child(netRow);

        // Row 4: Last check / Age row
        const ageText = formatAge(server.last_check);
        const ageLabel = new St.Label({
            text: `Checked: ${ageText}`,
            style_class: 'svmonitor-age-row',
        });
        ageLabel._lastCheckTs = server.last_check;
        toggleInner.add_child(ageLabel);

        // Alerts if active
        if (server.active_alerts && server.active_alerts.length > 0) {
            const alertText = new St.Label({
                text: `Alert: ${server.active_alerts.join(', ')}`,
                style_class: 'svmonitor-alert-text',
            });
            toggleInner.add_child(alertText);
        }

        if (typeof toggleBtn.set_child === 'function') {
            toggleBtn.set_child(toggleInner);
        } else {
            toggleBtn.add_child(toggleInner);
        }
        toggleBtn.connect('clicked', () => {
            this._toggleExpand(server.name);
        });
        cardBox.add_child(toggleBtn);

        // Row 5: Expanded Detail (accordion)
        if (isExpanded) {
            const detailBox = this._createDetailBox(server, canCmd);
            cardBox.add_child(detailBox);
        }

        return cardBox;
    }

    _createDetailBox(server, canCmd) {
        const detailBox = new St.BoxLayout({
            orientation: Clutter.Orientation.VERTICAL,
            style_class: 'svmonitor-detail-box',
            x_expand: true,
        });

        // Detail info lines
        const sshLine = new St.BoxLayout({ style_class: 'svmonitor-detail-line' });
        sshLine.add_child(new St.Label({ text: 'SSH:', style_class: 'svmonitor-detail-label' }));
        sshLine.add_child(new St.Label({ text: `${server.ssh.state}${server.ssh.reason ? ` (${server.ssh.reason})` : ''}` }));
        detailBox.add_child(sshLine);

        if (server.tailscale && server.tailscale !== 'not_applicable') {
            const tsLine = new St.BoxLayout({ style_class: 'svmonitor-detail-line' });
            tsLine.add_child(new St.Label({ text: 'Tailscale:', style_class: 'svmonitor-detail-label' }));
            tsLine.add_child(new St.Label({ text: server.tailscale }));
            detailBox.add_child(tsLine);
        }

        if (server.metrics) {
            const m = server.metrics;
            if (m.os || m.kernel) {
                const sysLine = new St.BoxLayout({ style_class: 'svmonitor-detail-line' });
                sysLine.add_child(new St.Label({ text: 'System:', style_class: 'svmonitor-detail-label' }));
                sysLine.add_child(new St.Label({ text: `${m.os || ''} ${m.kernel ? `(${m.kernel})` : ''}`.trim() }));
                detailBox.add_child(sysLine);
            }

            if (m.uptime_secs > 0) {
                const upLine = new St.BoxLayout({ style_class: 'svmonitor-detail-line' });
                upLine.add_child(new St.Label({ text: 'Uptime:', style_class: 'svmonitor-detail-label' }));
                upLine.add_child(new St.Label({ text: formatUptime(m.uptime_secs) }));
                detailBox.add_child(upLine);
            }

            const worstDisk = getWorstDisk(m.disks);
            if (worstDisk) {
                const diskBox = new St.BoxLayout({
                    orientation: Clutter.Orientation.VERTICAL,
                    style_class: 'svmonitor-col',
                    x_expand: true,
                });
                const diskHeader = new St.BoxLayout({ x_expand: true });
                diskHeader.add_child(new St.Label({ text: `Disk ${worstDisk.mount}`, style_class: 'svmonitor-col-header' }));
                diskHeader.add_child(new St.Widget({ x_expand: true }));
                diskHeader.add_child(new St.Label({ text: `${worstDisk.pct}%`, style_class: 'svmonitor-col-value' }));
                diskBox.add_child(diskHeader);
                diskBox.add_child(new St.Label({
                    text: `${(worstDisk.used_kib / 1048576).toFixed(1)} GiB / ${(worstDisk.total_kib / 1048576).toFixed(1)} GiB`,
                    style_class: 'svmonitor-col-sub',
                }));
                diskBox.add_child(this._createProgress(worstDisk.pct, true));
                detailBox.add_child(diskBox);
            }
        }

        // Action Buttons Row: SSH, Check, Pause/Resume, Manage
        const actionsRow = new St.BoxLayout({
            style_class: 'svmonitor-action-row',
            x_expand: true,
        });

        // 1. SSH Button
        const sshBtn = new St.Button({
            label: 'SSH',
            style_class: 'svmonitor-btn svmonitor-btn-primary',
            can_focus: true,
        });
        const canSsh = canExecuteSsh(server);
        sshBtn.reactive = canSsh;
        if (!canSsh) sshBtn.add_style_class_name('svmonitor-btn-disabled');
        sshBtn.connect('clicked', () => {
            this._extension.runSsh(server.name);
            this.menu.close();
        });
        actionsRow.add_child(sshBtn);

        // 2. Check Button
        const checkBtn = new St.Button({
            label: 'Check',
            style_class: 'svmonitor-btn',
            can_focus: true,
        });
        checkBtn.reactive = canCmd;
        if (!canCmd) checkBtn.add_style_class_name('svmonitor-btn-disabled');
        checkBtn.connect('clicked', () => {
            this._extension.runUiAction('check', server.name);
        });
        actionsRow.add_child(checkBtn);

        // 3. Pause / Resume Button
        const isPaused = !server.enabled;
        const pauseBtn = new St.Button({
            label: isPaused ? 'Resume' : 'Pause',
            style_class: 'svmonitor-btn',
            can_focus: true,
        });
        pauseBtn.reactive = canCmd;
        if (!canCmd) pauseBtn.add_style_class_name('svmonitor-btn-disabled');
        pauseBtn.connect('clicked', () => {
            this._extension.runUiAction(isPaused ? 'resume' : 'pause', server.name);
        });
        actionsRow.add_child(pauseBtn);

        // 4. Manage Button
        const isManaged = this._managedServer === server.name;
        const manageBtn = new St.Button({
            label: isManaged ? 'Hide Manage' : 'Manage',
            style_class: 'svmonitor-btn',
            can_focus: true,
        });
        manageBtn.connect('clicked', () => {
            this._toggleManage(server.name);
        });
        actionsRow.add_child(manageBtn);

        detailBox.add_child(actionsRow);

        // Manage Drawer (revealed when Manage clicked)
        if (isManaged) {
            const manageDrawer = this._createManageDrawer(server, canCmd);
            detailBox.add_child(manageDrawer);
        }

        return detailBox;
    }

    _createManageDrawer(server, canCmd) {
        const drawer = new St.BoxLayout({
            orientation: Clutter.Orientation.VERTICAL,
            style_class: 'svmonitor-manage-drawer',
            x_expand: true,
        });

        const isConfirming = this._confirmingRemoveServer === server.name;
        const removeBtn = new St.Button({
            label: isConfirming ? 'Click again to confirm remove' : 'Remove Server',
            style_class: `svmonitor-btn svmonitor-btn-danger ${isConfirming ? 'svmonitor-btn-danger-confirm' : ''}`,
            can_focus: true,
        });
        removeBtn.reactive = canCmd;
        if (!canCmd) removeBtn.add_style_class_name('svmonitor-btn-disabled');

        removeBtn.connect('clicked', () => {
            this._handleRemove(server.name);
        });

        drawer.add_child(removeBtn);
        return drawer;
    }

    _toggleExpand(serverName) {
        if (this._expandedServer === serverName) {
            this._expandedServer = null;
        } else {
            this._expandedServer = serverName;
        }
        this._managedServer = null;
        this._confirmingRemoveServer = null;
        this._renderCards();
    }

    _toggleManage(serverName) {
        if (this._managedServer === serverName) {
            this._managedServer = null;
        } else {
            this._managedServer = serverName;
        }
        this._confirmingRemoveServer = null;
        this._renderCards();
    }

    _handleRemove(serverName) {
        if (this._confirmingRemoveServer === serverName) {
            this._confirmingRemoveServer = null;
            if (this._confirmTimeoutId) {
                this._extension.removeTimeout(this._confirmTimeoutId);
                this._confirmTimeoutId = 0;
            }
            this._extension.runUiAction('remove', serverName);
        } else {
            this._confirmingRemoveServer = serverName;
            if (this._confirmTimeoutId) {
                this._extension.removeTimeout(this._confirmTimeoutId);
                this._confirmTimeoutId = 0;
            }
            this._confirmTimeoutId = this._extension.addTimeout(GLib.PRIORITY_DEFAULT, 5000, () => {
                this._confirmTimeoutId = 0;
                this._confirmingRemoveServer = null;
                this._renderCards();
                return GLib.SOURCE_REMOVE;
            });
            this._renderCards();
        }
    }

    _updateRelativeTimes() {
        const now = Date.now() / 1000;
        this._cardsContainer.get_children().forEach(card => {
            const toggle = card.get_children?.()[0];
            const inner = toggle?.get_child?.();
            if (!inner?.get_children) return;

            for (const child of inner.get_children()) {
                if (child._lastCheckTs !== undefined) {
                    child.set_text(`Checked: ${formatAge(child._lastCheckTs, now)}`);
                }
            }
        });
    }

    destroy() {
        this._stopRelativeTimer();
        if (this._confirmTimeoutId) {
            this._extension.removeTimeout(this._confirmTimeoutId);
            this._confirmTimeoutId = 0;
        }
        super.destroy();
    }
});

export default class SvmonitorExtension extends Extension {
    enable() {
        try {
            this._enableInternal();
        } catch (e) {
            console.error(`[SVMonitor] Enable failure, cleaning up: ${e}`);
            this.disable();
            throw e;
        }
    }

    _enableInternal() {
        this._cancellable = new Gio.Cancellable();
        this._timers = new Set();
        this._signals = [];
        this._snapshot = null;
        this._daemonAlive = false;

        this._hiddenIndicatorActor = null;
        this._hiddenIndicatorDestroyId = 0;
        this._hiddenIndicatorNotifyId = 0;
        this._indicatorOrigVisible = null;

        this._fileMonitor = null;
        this._reloadTimeoutId = 0;
        this._dirRetryTimeoutId = 0;
        this._feedbackTimeoutId = 0;

        // Create Panel Button
        this._indicator = new SvmonitorIndicator(this);
        Main.panel.addToStatusArea(this.uuid, this._indicator, 1, 'right');

        // Setup async cache monitoring
        this._initCacheMonitor();

        // Setup AppIndicator actor takeover
        this._setupAppIndicatorTakeover();
    }

    disable() {
        // 1. Cancel all async operations
        if (this._cancellable) {
            this._cancellable.cancel();
            this._cancellable = null;
        }

        // 2. Disconnect and clear all timers
        if (this._timers) {
            for (const id of this._timers) {
                GLib.source_remove(id);
            }
            this._timers.clear();
            this._timers = null;
        }
        this._reloadTimeoutId = 0;
        this._dirRetryTimeoutId = 0;
        this._feedbackTimeoutId = 0;

        // 3. Stop file monitor
        if (this._fileMonitor) {
            this._fileMonitor.cancel();
            this._fileMonitor = null;
        }

        // 4. Disconnect external signals (panel box child-added, etc.)
        if (this._signals) {
            for (const { obj, id } of this._signals) {
                try {
                    if (obj && typeof obj.disconnect === 'function') {
                        obj.disconnect(id);
                    }
                } catch {
                    // Object may be destroyed
                }
            }
            this._signals = [];
        }

        // 5. Restore AppIndicator visibility
        this._restoreAppIndicator();

        // 6. Destroy indicator
        if (this._indicator) {
            this._indicator.destroy();
            this._indicator = null;
        }

        this._snapshot = null;
        this._daemonAlive = false;
    }

    addTimeout(priority, intervalMs, callback) {
        if (!this._timers) return 0;
        let id = 0;
        id = GLib.timeout_add(priority, intervalMs, () => {
            if (!this._timers || !this._timers.has(id)) {
                return GLib.SOURCE_REMOVE;
            }
            const res = callback();
            if (res === GLib.SOURCE_REMOVE || res === false) {
                this._timers.delete(id);
                return GLib.SOURCE_REMOVE;
            }
            return GLib.SOURCE_CONTINUE;
        });
        this._timers.add(id);
        return id;
    }

    removeTimeout(id) {
        if (!id || !this._timers) return;
        if (this._timers.has(id)) {
            GLib.source_remove(id);
            this._timers.delete(id);
        }
    }

    _initCacheMonitor() {
        const runtimeBase = GLib.get_user_runtime_dir();
        this._runtimeDir = GLib.build_filenamev([runtimeBase, 'svmonitor']);
        this._stateFilePath = GLib.build_filenamev([this._runtimeDir, 'state.json']);

        this._watchDirectory();
        this._readCacheAsync();
    }

    _watchDirectory() {
        const dirFile = Gio.File.new_for_path(this._runtimeDir);
        if (!dirFile.query_exists(null)) {
            // Directory doesn't exist yet, retry every 4s until it appears
            this._dirRetryTimeoutId = this.addTimeout(GLib.PRIORITY_DEFAULT, 4000, () => {
                if (dirFile.query_exists(null)) {
                    this._dirRetryTimeoutId = 0;
                    this._watchDirectory();
                    this._readCacheAsync();
                    return GLib.SOURCE_REMOVE;
                }
                return GLib.SOURCE_CONTINUE;
            });
            return;
        }

        try {
            this._fileMonitor = dirFile.monitor_directory(
                Gio.FileMonitorFlags.WATCH_MOVES,
                this._cancellable
            );
            this._fileMonitor.connect('changed', (_mon, file, other, _eventType) => {
                const basename = file?.get_basename?.();
                if (!basename || basename === 'state.json' || other?.get_basename?.() === 'state.json') {
                    this.scheduleCacheReload();
                }
            });
        } catch (e) {
            console.warn(`[SVMonitor] Failed to monitor directory: ${e.message}`);
        }
    }

    scheduleCacheReload() {
        if (this._reloadTimeoutId) {
            this.removeTimeout(this._reloadTimeoutId);
            this._reloadTimeoutId = 0;
        }
        this._reloadTimeoutId = this.addTimeout(GLib.PRIORITY_DEFAULT, 80, () => {
            this._reloadTimeoutId = 0;
            this._readCacheAsync();
            return GLib.SOURCE_REMOVE;
        });
    }

    _readCacheAsync() {
        if (!this._cancellable || this._cancellable.is_cancelled()) return;
        const generation = this._cancellable;
        const request = this._readSerial = (this._readSerial ?? 0) + 1;
        const current = () => this._cancellable === generation && !generation.is_cancelled() && this._readSerial === request;
        const stale = () => {
            if (current()) {
                this._daemonAlive = false;
                this._indicator?.updateState(this._snapshot, false);
            }
        };

        const file = Gio.File.new_for_path(this._stateFilePath);
        file.query_info_async(
            Gio.FILE_ATTRIBUTE_STANDARD_SIZE,
            Gio.FileQueryInfoFlags.NONE,
            GLib.PRIORITY_DEFAULT,
            this._cancellable,
            (f, res) => {
                try {
                    const info = f.query_info_finish(res);
                    if (!current()) return;
                    const size = info.get_size();
                    if (size > DEFAULT_MAX_SNAPSHOT_BYTES) {
                        console.warn(`[SVMonitor] state.json size ${size} exceeds cap`);
                        stale();
                        return;
                    }

                    f.load_contents_async(generation, (f2, res2) => {
                        try {
                            const [ok, contents] = f2.load_contents_finish(res2);
                            if (!current()) return;
                            if (!ok || !contents) { stale(); return; }

                            const snapshot = parseSnapshot(contents);
                            if (snapshot) {
                                this._checkDaemonLiveness(snapshot.pid, (alive) => {
                                    if (!current()) return;
                                    this._snapshot = snapshot;
                                    this._daemonAlive = alive;
                                    if (this._indicator) {
                                        this._indicator.updateState(this._snapshot, this._daemonAlive);
                                    }
                                });
                            } else {
                                stale();
                            }
                        } catch (e) {
                            if (!e.matches?.(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED)) stale();
                        }
                    });
                } catch {
                    stale();
                }
            }
        );
    }

    checkDaemonLivenessNow() {
        if (!this._cancellable || !this._snapshot) return;
        const generation = this._cancellable;
        this._checkDaemonLiveness(this._snapshot.pid, alive => {
            if (this._cancellable !== generation || generation.is_cancelled()) return;
            if (alive !== this._daemonAlive) {
                this._daemonAlive = alive;
                this._indicator?.updateState(this._snapshot, alive);
            }
        });
    }

    _checkDaemonLiveness(pid, callback) {
        if (!pid || pid <= 0) {
            callback(false);
            return;
        }

        const generation = this._cancellable;
        if (!generation || generation.is_cancelled()) return;
        const procCommFile = Gio.File.new_for_path(`/proc/${pid}/comm`);
        procCommFile.load_contents_async(this._cancellable, (file, res) => {
            try {
                const [ok, contents] = file.load_contents_finish(res);
                if (this._cancellable !== generation || generation.is_cancelled()) return;
                if (ok && contents) {
                    const comm = new TextDecoder('utf-8').decode(contents);
                    const alive = isDaemonAlive(pid, comm);
                    callback(alive);
                    return;
                }
            } catch {
                // File does not exist or process died
            }
            if (this._cancellable === generation && !generation.is_cancelled()) callback(false);
        });
    }

    _setupAppIndicatorTakeover() {
        this._scanAndHideAppIndicator();

        // Watch panel boxes for recreated or newly added actors without touching AppIndicator code
        const boxes = [Main.panel._rightBox, Main.panel._leftBox, Main.panel._centerBox];
        for (const box of boxes) {
            if (!box || typeof box.connect !== 'function') continue;
        const id = box.connect('child-added', (_container, child) => {
            if (this._isSvmonitorIndicator(child)) {
                this._hideAppIndicatorActor(child);
            } else {
                this.addTimeout(GLib.PRIORITY_DEFAULT, 150, () => {
                    if (this._isSvmonitorIndicator(child)) {
                        this._hideAppIndicatorActor(child);
                    }
                    return GLib.SOURCE_REMOVE;
                });
            }
        });
            this._signals.push({ obj: box, id });
        }
    }

    _scanAndHideAppIndicator() {
        if (Main.panel?.statusArea) {
            for (const item of Object.values(Main.panel.statusArea)) {
                if (item && this._isSvmonitorIndicator(item)) {
                    this._hideAppIndicatorActor(item);
                    return;
                }
            }
        }

        const boxes = [Main.panel._rightBox, Main.panel._leftBox, Main.panel._centerBox];
        for (const box of boxes) {
            if (!box?.get_children) continue;
            for (const child of box.get_children()) {
                if (this._isSvmonitorIndicator(child)) {
                    this._hideAppIndicatorActor(child);
                    return;
                }
            }
        }
    }

    _isSvmonitorIndicator(actor) {
        return Boolean(actor && actor !== this._indicator && actor._indicator?.id === 'svmonitor');
    }

    _hideAppIndicatorActor(actor) {
        if (!actor || this._hiddenIndicatorActor === actor) return;

        this._restoreAppIndicator();

        this._hiddenIndicatorActor = actor;
        this._indicatorOrigVisible = actor.visible;

        this._hiddenIndicatorDestroyId = actor.connect('destroy', () => {
            this._hiddenIndicatorActor = null;
            this._hiddenIndicatorDestroyId = 0;
            this._hiddenIndicatorNotifyId = 0;
        });

        this._hiddenIndicatorNotifyId = actor.connect('notify::visible', () => {
            if (this._hiddenIndicatorActor && this._hiddenIndicatorActor.visible) {
                this._hiddenIndicatorActor.visible = false;
            }
        });

        actor.visible = false;
    }

    _restoreAppIndicator() {
        if (this._hiddenIndicatorActor) {
            try {
                if (this._hiddenIndicatorDestroyId) {
                    this._hiddenIndicatorActor.disconnect(this._hiddenIndicatorDestroyId);
                    this._hiddenIndicatorDestroyId = 0;
                }
                if (this._hiddenIndicatorNotifyId) {
                    this._hiddenIndicatorActor.disconnect(this._hiddenIndicatorNotifyId);
                    this._hiddenIndicatorNotifyId = 0;
                }
                this._hiddenIndicatorActor.visible = this._indicatorOrigVisible !== null
                    ? this._indicatorOrigVisible
                    : true;
            } catch {
                // Actor might already be destroyed
            }
            this._hiddenIndicatorActor = null;
        }
        this._indicatorOrigVisible = null;
    }

    _getExecutablePath() {
        const home = GLib.get_home_dir();
        const localBin = GLib.build_filenamev([home, '.local', 'bin', 'svmonitor']);
        if (GLib.file_test(localBin, GLib.FileTest.IS_EXECUTABLE)) {
            return localBin;
        }
        const inPath = GLib.find_program_in_path('svmonitor');
        if (inPath) {
            return inPath;
        }
        return 'svmonitor';
    }

    runUiAction(action, serverName = null) {
        if (action !== 'settings' && !canExecuteCommands(this._snapshot, this._daemonAlive)) {
            this.setFeedback('Cannot execute: daemon is offline', true);
            return;
        }

        try {
            const bin = this._getExecutablePath();
            const argv = getCommandArgv(bin, action, serverName);
            this.setFeedback(`Executing ${action}...`, false);

            const proc = Gio.Subprocess.new(
                argv,
                Gio.SubprocessFlags.STDOUT_PIPE | Gio.SubprocessFlags.STDERR_PIPE
            );

            proc.communicate_utf8_async(null, this._cancellable, (p, res) => {
                try {
                    const [_ok, stdout, stderr] = p.communicate_utf8_finish(res);
                    if (p.get_if_exited()) {
                        const code = p.get_exit_status();
                        if (code === 0) {
                            const msg = action === 'settings' ? 'Settings opened' : `${action} request sent`;
                            this.setFeedback(msg, false);
                            this.scheduleCacheReload();
                        } else {
                            const err = (stderr || stdout || '').trim() || `Exit code ${code}`;
                            this.setFeedback(`Failed: ${err}`, true);
                        }
                    }
                } catch (e) {
                    if (!e.matches?.(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED)) {
                        this.setFeedback(`Error: ${e.message}`, true);
                    }
                }
            });
        } catch (e) {
            this.setFeedback(`Failed: ${e.message}`, true);
        }
    }

    runSsh(serverName) {
        if (!canExecuteSsh({ name: serverName })) {
            this.setFeedback(`Invalid server: ${serverName}`, true);
            return;
        }

        try {
            const bin = this._getExecutablePath();
            const argv = getCommandArgv(bin, 'ssh', serverName);
            // Spawn without shell and no timers
            Gio.Subprocess.new(argv, Gio.SubprocessFlags.NONE);
        } catch (e) {
            this.setFeedback(`Failed to launch SSH: ${e.message}`, true);
        }
    }

    setFeedback(text, isError = false) {
        if (this._indicator) {
            this._indicator.updateFeedback(text, isError);
        }
        if (this._feedbackTimeoutId) {
            this.removeTimeout(this._feedbackTimeoutId);
            this._feedbackTimeoutId = 0;
        }
        this._feedbackTimeoutId = this.addTimeout(GLib.PRIORITY_DEFAULT, 4000, () => {
            this._feedbackTimeoutId = 0;
            if (this._indicator) {
                this._indicator.updateFeedback('', false);
            }
            return GLib.SOURCE_REMOVE;
        });
    }
}
