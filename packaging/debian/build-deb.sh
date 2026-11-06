#!/bin/sh
# Build a .deb with dpkg-deb. Run it where the binary should be built (use packaging/debian/docker-build.sh
# to build against Debian 12 glibc for maximum compatibility).
# Usage: packaging/debian/build-deb.sh [path/to/svmonitor] [arch]
set -eu
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
BIN=${1:-$ROOT/target/release/svmonitor}
ARCH=${2:-$(dpkg --print-architecture 2>/dev/null || echo amd64)}
VER=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)
OUT=${OUT:-$ROOT/dist}
W=$(mktemp -d)
trap 'rm -rf "$W"' EXIT
P=$W/svmonitor_${VER}_${ARCH}
install -Dm755 "$BIN" "$P/usr/bin/svmonitor"
install -Dm644 "$ROOT/data/svmonitor.desktop" "$P/usr/share/applications/svmonitor.desktop"
install -Dm644 "$ROOT/data/svmonitor.service" "$P/usr/lib/systemd/user/svmonitor.service"
install -Dm644 "$ROOT/README.md" "$P/usr/share/doc/svmonitor/README.md"
install -Dm644 "$ROOT/docs/USAGE.md" "$P/usr/share/doc/svmonitor/USAGE.md"
install -Dm644 "$ROOT/docs/config.example.toml" "$P/usr/share/doc/svmonitor/config.example.toml"
install -Dm644 "$ROOT/LICENSE" "$P/usr/share/doc/svmonitor/copyright"
SIZE=$(du -sk "$P/usr" | cut -f1)
mkdir -p "$P/DEBIAN"
cat > "$P/DEBIAN/control" <<CTL
Package: svmonitor
Version: $VER
Section: net
Priority: optional
Architecture: $ARCH
Depends: libc6 (>= 2.36), libgcc-s1, openssh-client
Suggests: tailscale, xdg-terminal-exec, gnome-shell-extension-appindicator
Installed-Size: $SIZE
Maintainer: SVMonitor contributors <svmonitor@localhost>
Homepage: https://github.com/svmonitor/svmonitor
Description: Lightweight SSH server monitor with system tray
 Monitors Linux servers over your existing OpenSSH configuration. No agent
 or service is installed on the servers; metrics are collected read-only.
 Provides a StatusNotifier tray icon, desktop notifications, quick SSH,
 event history and a headless CLI.
CTL
mkdir -p "$OUT"
dpkg-deb --root-owner-group --build "$P" "$OUT/svmonitor_${VER}_${ARCH}.deb" >/dev/null
echo "$OUT/svmonitor_${VER}_${ARCH}.deb"
