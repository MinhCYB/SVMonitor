#!/bin/bash
# Leak check: run with a fast poll interval and compare RSS / fds / threads at start vs end.
# Usage: scripts/soak.sh <bin> <config> <seconds>
set -u
BIN=$1; CFG=$2; DUR=$3
W=$(mktemp -d /tmp/svm-soak.XXXXXX); mkdir -p "$W/run" "$W/state"; chmod 700 "$W/run"
XDG_RUNTIME_DIR=$W/run XDG_STATE_HOME=$W/state "$BIN" --config "$CFG" run --no-notify --headless >/dev/null 2>&1 &
PID=$!
snap() { echo "$(awk '/VmRSS/{print $2}' /proc/$PID/status) $(ls /proc/$PID/fd | wc -l) $(awk '/Threads/{print $2}' /proc/$PID/status) $(pgrep -P $PID | wc -l)"; }
sleep 30; read -r R0 F0 T0 C0 < <(snap)
sleep $((DUR-30)); read -r R1 F1 T1 C1 < <(snap)
ZOMB=$(ps -eo stat,ppid | awk -v p=$PID '$1 ~ /^Z/ && $2==p' | wc -l)
HIST=$(du -sk "$W/state" | cut -f1)
kill -TERM $PID; wait $PID 2>/dev/null
echo "soak ${DUR}s: RSS ${R0}->${R1} KiB | fds ${F0}->${F1} | threads ${T0}->${T1} | zombies=$ZOMB | history=${HIST}KiB"
rm -rf "$W"
