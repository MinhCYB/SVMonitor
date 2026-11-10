#!/bin/sh
# Install only SVMonitor's GNOME 51 extension; never restart the desktop session.
set -eu
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
UUID=svmonitor@svmonitor.local
DEST=${XDG_DATA_HOME:-$HOME/.local/share}/gnome-shell/extensions/$UUID
case ${1:-install} in
  install)
    mkdir -p "$DEST"
    cp "$ROOT/gnome-extension/$UUID/metadata.json" "$DEST/metadata.json"
    cp "$ROOT/gnome-extension/$UUID/extension.js" "$DEST/extension.js"
    for file in model.js stylesheet.css stylesheet-dark.css stylesheet-light.css; do
      if [ -f "$ROOT/gnome-extension/$UUID/$file" ]; then
        cp "$ROOT/gnome-extension/$UUID/$file" "$DEST/$file"
      fi
    done
    printf 'Installed %s\n' "$DEST"
    printf 'Enable: gnome-extensions enable %s\n' "$UUID"
    printf 'If the running Shell does not know this new UUID, see docs/GNOME.md. No session restart is performed.\n'
    ;;
  disable)
    gnome-extensions disable "$UUID"
    ;;
  *) printf 'Usage: %s [install|disable]\n' "$0" >&2; exit 2 ;;
esac
