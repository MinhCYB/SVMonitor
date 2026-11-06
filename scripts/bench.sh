#!/bin/bash
# Usage: scripts/bench.sh <svmonitor-binary> <config.toml> <seconds> <label> [extra run args]
# Samples RSS / threads / CPU of the daemon (and its reaped ssh children) once per second.
# Uses private runtime/state dirs so it can run next to a real instance.
set -u
BIN=$1; CFG=$2; DUR=$3; LABEL=$4; shift 4
W=$(mktemp -d /tmp/svm-bench.XXXXXX)

mkdir -p "$W/run" "$W/state"; chmod 700 "$W/run"
XDG_RUNTIME_DIR=$W/run XDG_STATE_HOME=$W/state "$BIN" --config "$CFG" run --no-notify "$@" >"$W/out.log" 2>&1 &
PID=$!
sleep 0.2; kill -0 $PID 2>/dev/null || { cat "$W/out.log"; exit 1; }
CLK=$(getconf CLK_TCK)
read_cpu() { awk '{print $14+$15, $16+$17}' /proc/$1/stat; }
read -r U0 C0 < <(read_cpu $PID)
MAXRSS=0; SUMRSS=0; N=0; MAXTH=0; MAXCH=0; SUMCH=0
for ((i=0;i<DUR;i++)); do
  sleep 1
  kill -0 $PID 2>/dev/null || break
  RSS=$(awk '/VmRSS/{print $2}' /proc/$PID/status)
  TH=$(awk '/Threads/{print $2}' /proc/$PID/status)
  CH=$(pgrep -P $PID | wc -l)
  (( RSS>MAXRSS )) && MAXRSS=$RSS
  (( TH>MAXTH )) && MAXTH=$TH
  (( CH>MAXCH )) && MAXCH=$CH
  SUMRSS=$((SUMRSS+RSS)); SUMCH=$((SUMCH+CH)); N=$((N+1))
done
read -r U1 C1 < <(read_cpu $PID)
FD=$(ls /proc/$PID/fd | wc -l)
FINALRSS=$(awk '/VmRSS/{print $2}' /proc/$PID/status)
kill -TERM $PID; wait $PID 2>/dev/null
SELF=$((U1-U0)); KIDS=$((C1-C0))
awk -v l="$LABEL" -v d="$N" -v clk="$CLK" -v s="$SELF" -v k="$KIDS" -v mr="$MAXRSS" -v ar="$((SUMRSS/(N>0?N:1)))" -v fr="$FINALRSS" -v th="$MAXTH" -v mc="$MAXCH" -v fd="$FD" 'BEGIN{
 printf "%-22s dur=%ds  RSS avg=%.1f MiB max=%.1f MiB final=%.1f MiB | threads max=%d | fds=%d | max ssh children=%d\n", l, d, ar/1024, mr/1024, fr/1024, th, fd, mc;
 printf "%-22s CPU self=%.3f%%  children(ssh)=%.3f%%  total=%.3f%%  (self %d ticks, children %d ticks over %ds)\n", l, s/clk/d*100, k/clk/d*100, (s+k)/clk/d*100, s, k, d}'
rm -rf "$W"
