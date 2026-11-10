#!/bin/sh
# Build a .deb with dpkg-deb. Run it where binary should be built (use packaging/debian/docker-build.sh
# to build against Debian 12 glibc/toolchain for compatibility).
# Note: Binaries built directly on an Arch/CachyOS host are NOT Debian-compatible.
# Usage: packaging/debian/build-deb.sh [path/to/svmonitor] [arch] [path/to/svmonitor-popup]
set -eu
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
BIN=${1:-$ROOT/target/release/svmonitor}
ARCH=${2:-$(dpkg --print-architecture 2>/dev/null || echo amd64)}
POPUP_BIN=${3:-}
VER=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)
OUT=${OUT:-$ROOT/dist}

detect_shlibdeps() {
    bin=$1
    if command -v dpkg-shlibdeps >/dev/null 2>&1; then
        out=$(dpkg-shlibdeps -O "$bin" 2>/dev/null | sed -n 's/^shlibs:Depends=//p' || true)
        if [ -n "$out" ]; then
            echo "$out"
            return 0
        fi
    fi
    echo ""
}
if [ ! -f "$BIN" ]; then
    echo "Error: binary not found at $BIN" >&2
    exit 1
fi
W=$(mktemp -d)
trap 'rm -rf "$W"' EXIT
P=$W/svmonitor_${VER}_${ARCH}
install -Dm755 "$BIN" "$P/usr/bin/svmonitor"
install -Dm644 "$ROOT/data/svmonitor.desktop" "$P/usr/share/applications/svmonitor.desktop"
install -Dm644 "$ROOT/data/svmonitor.service" "$P/usr/lib/systemd/user/svmonitor.service"
install -Dm644 "$ROOT/README.md" "$P/usr/share/doc/svmonitor/README.md"
install -Dm644 "$ROOT/docs/USAGE.md" "$P/usr/share/doc/svmonitor/USAGE.md"
install -Dm644 "$ROOT/docs/COMPATIBILITY.md" "$P/usr/share/doc/svmonitor/COMPATIBILITY.md"
install -Dm644 "$ROOT/docs/config.example.toml" "$P/usr/share/doc/svmonitor/config.example.toml"
install -Dm644 "$ROOT/LICENSE" "$P/usr/share/doc/svmonitor/copyright"
SIZE=$(du -sk "$P/usr" | cut -f1)
mkdir -p "$P/DEBIAN"
DETECTED_DAEMON_DEPS=$(detect_shlibdeps "$BIN")
if [ -n "$DETECTED_DAEMON_DEPS" ]; then
    DAEMON_DEPS="$DETECTED_DAEMON_DEPS, openssh-client"
else
    DAEMON_DEPS="libc6 (>= 2.36), libgcc-s1, openssh-client"
fi
cat > "$P/DEBIAN/control" <<CTL
Package: svmonitor
Version: $VER
Section: net
Priority: optional
Architecture: $ARCH
Depends: $DAEMON_DEPS
Suggests: tailscale, xdg-terminal-exec, gnome-shell-extension-appindicator, svmonitor-popup
Installed-Size: $SIZE
Maintainer: SVMonitor contributors <svmonitor@localhost>
Homepage: https://github.com/MinhCYB/SVMonitor
Description: Lightweight SSH server monitor with system tray
 Monitors Linux servers over your existing OpenSSH configuration. No agent
 or service is installed on the servers; metrics are collected read-only.
 Provides a StatusNotifier tray icon, desktop notifications, quick SSH,
 event history and a headless CLI.
CTL
find "$P" -type d -exec chmod 0755 {} +
chmod 0644 "$P/DEBIAN/control"
mkdir -p "$OUT"
dpkg-deb --root-owner-group --build "$P" "$OUT/svmonitor_${VER}_${ARCH}.deb" >/dev/null
echo "$OUT/svmonitor_${VER}_${ARCH}.deb"

if [ -n "$POPUP_BIN" ]; then
    if [ ! -f "$POPUP_BIN" ]; then
        echo "Error: popup binary not found at $POPUP_BIN" >&2
        exit 1
    fi
    P_POPUP=$W/svmonitor-popup_${VER}_${ARCH}
    install -Dm755 "$POPUP_BIN" "$P_POPUP/usr/bin/svmonitor-popup"
    if [ -f "$ROOT/data/io.github.MinhCYB.SVMonitor.Popup.desktop" ]; then
        install -Dm644 "$ROOT/data/io.github.MinhCYB.SVMonitor.Popup.desktop" "$P_POPUP/usr/share/applications/io.github.MinhCYB.SVMonitor.Popup.desktop"
    fi
    install -Dm644 "$ROOT/README.md" "$P_POPUP/usr/share/doc/svmonitor-popup/README.md"
    install -Dm644 "$ROOT/LICENSE" "$P_POPUP/usr/share/doc/svmonitor-popup/copyright"
    SIZE_POPUP=$(du -sk "$P_POPUP/usr" | cut -f1)
    mkdir -p "$P_POPUP/DEBIAN"

    DETECTED_POPUP_DEPS=$(detect_shlibdeps "$POPUP_BIN")
    if [ -n "$DETECTED_POPUP_DEPS" ]; then
        POPUP_DEPS="svmonitor (= $VER), $DETECTED_POPUP_DEPS"
    else
        POPUP_DEPS="svmonitor (= $VER), libgtk-4-1 (>= 4.8.0), libglib2.0-0 (>= 2.74.0), libpango-1.0-0 (>= 1.50.0), libcairo2 (>= 1.16.0), libgdk-pixbuf-2.0-0 (>= 2.42.0), libgraphene-1.0-0 (>= 1.10.0), libc6 (>= 2.36), libgcc-s1"
    fi

    cat > "$P_POPUP/DEBIAN/control" <<CTL
Package: svmonitor-popup
Version: $VER
Section: net
Priority: optional
Architecture: $ARCH
Depends: $POPUP_DEPS
Installed-Size: $SIZE_POPUP
Maintainer: SVMonitor contributors <svmonitor@localhost>
Homepage: https://github.com/MinhCYB/SVMonitor
Description: Lightweight SSH server monitor - GTK4 dashboard popup
 Optional on-demand graphical dashboard popup for SVMonitor.
 Displays server resource metrics, network counters and quick actions.
 Uses plain GTK4 (>=4.8) and exits on close to preserve minimal footprint.
CTL
    find "$P_POPUP" -type d -exec chmod 0755 {} +
    chmod 0644 "$P_POPUP/DEBIAN/control"
    dpkg-deb --root-owner-group --build "$P_POPUP" "$OUT/svmonitor-popup_${VER}_${ARCH}.deb" >/dev/null
    echo "$OUT/svmonitor-popup_${VER}_${ARCH}.deb"
fi
