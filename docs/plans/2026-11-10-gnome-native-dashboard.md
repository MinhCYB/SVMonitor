# Native GNOME 51 dashboard

Goal: replace SVMonitor's AppIndicator/GTK entry with one PanelMenu.Button and native popup on this GNOME 51.1 Wayland session. Rust monitoring remains unchanged.

## Decisions
- GJS ES module extension at `gnome-extension/svmonitor@svmonitor.local/`; St widgets and GNOME popup menu handle anchoring, focus, outside-click, keyboard and scale.
- Read state.json asynchronously, watch its parent directory for atomic replacement; only update relative time while menu open. No SSH polling for presentation.
- Existing Rust spool commands remain authoritative. Add CLI `ui` bridge accepting typed actions; extension uses asynchronous subprocess without shell interpolation. SSH and config open reuse Rust launcher.
- Only SVMonitor's AppIndicator actor is temporarily hidden after native button exists. Restore its prior visibility on disable/error; no AppIndicator code/config modification.
- One expanded server at a time, equal CPU/RAM columns, RX/TX and update age. Two normal cards fit; scroll only list, fixed header/footer.
- Native Shell theme colors plus light/dark variants; logical pixel width 440. No GTK helper in native path. Existing GTK fallback retained for explicit rollback.
- No logout, Shell restart, global unsafe-mode switch, or unrelated extension changes. If GNOME refuses live registration, stop before session-affecting steps and explain prerequisite.

## Execution
1. Implement pure presentation model tests for offline/stale, disabled, invalid metrics, age and command argv; add Rust CLI bridge using existing IPC.
2. Implement extension UI and async cache lifecycle, action feedback, exact AppIndicator actor takeover/restore; installer and rollback instructions.
3. Validate current GNOME live loading, popup open/outside-close/anchoring, data and actions, recovery, multiple screens/scales. Measure process CPU/RSS while closed/open without claiming extension-private RSS.
4. Narrow review, fix important issues, local commit; no push.

Verification: GJS model checks, Rust targeted CLI/IPC tests; actual GNOME surface/interaction evidence for desktop claims. No Debian/KDE/XFCE scope.
