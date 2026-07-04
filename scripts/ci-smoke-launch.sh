#!/usr/bin/env bash
# v0.11.0 — Linux GUI launch-smoke (WebKitGTK under xvfb).
#
# Companion to ci-smoke-launch.ps1. Runs headlessly in CI (ubuntu-latest)
# under a virtual X server. Same posture:
#   HARD gates: process survives launch (crash-on-launch) + frontend-ready
#               marker appears (white-screen).
#   SOFT:       xdotool window search + `import` screenshot (warn only).
#
# Invoke under xvfb, e.g.:  xvfb-run -a bash scripts/ci-smoke-launch.sh
set -uo pipefail

EXE="${1:-src-tauri/target/release/cli-pulse-desktop}"
TIMEOUT_SEC="${TIMEOUT_SEC:-45}"
OUTDIR="${OUTDIR:-smoke-artifacts}"
mkdir -p "$OUTDIR"

marker="$(mktemp -u)/clipulse-frontend-ready.marker"
mkdir -p "$(dirname "$marker")"
rm -f "$marker"

screenshot() { # $1 = filename ; best-effort, needs imagemagick
  import -window root "$OUTDIR/$1" 2>/dev/null && echo "screenshot: $OUTDIR/$1" \
    || echo "::warning::screenshot ($1) unavailable"
}
collect_logs() {
  local d="$HOME/.local/share/dev.clipulse.desktop/logs"
  [ -d "$d" ] && cp "$d"/* "$OUTDIR"/ 2>/dev/null && echo "collected app logs from $d" || true
}

if [ ! -f "$EXE" ]; then echo "::error::built exe not found at $EXE"; exit 3; fi
size=$(stat -c%s "$EXE" 2>/dev/null || stat -f%z "$EXE")
echo "== launch-smoke (linux) =="
echo "exe:    $EXE ($size bytes)"
echo "marker: $marker"
# GUI-binary sanity (sidecars are sub-MB).
if [ "$size" -lt 1000000 ]; then
  echo "::error::exe < 1MB ($size B) — looks like a sidecar, not the GUI binary"; exit 3
fi

export CLI_PULSE_SMOKE_MARKER="$marker"
export CLI_PULSE_DISABLE_REMOTE_AGENT="1"

"$EXE" >"$OUTDIR/app-stdout.log" 2>&1 &
pid=$!
echo "launched pid $pid"

mounted=0
deadline=$(( SECONDS + TIMEOUT_SEC ))
while [ "$SECONDS" -lt "$deadline" ]; do
  if ! kill -0 "$pid" 2>/dev/null; then
    wait "$pid"; code=$?
    echo "::error::process exited early (exit $code) before frontend-ready — crash-on-launch class (cf. v0.8.0)"
    screenshot "crash-screenshot.png"; collect_logs
    exit 1
  fi
  if [ -f "$marker" ]; then mounted=1; break; fi
  sleep 0.5
done

if [ "$mounted" -ne 1 ]; then
  alive=$(kill -0 "$pid" 2>/dev/null && echo true || echo false)
  echo "::error::frontend-ready marker never appeared within ${TIMEOUT_SEC}s — white-screen class (cf. v0.2.11). process-alive=$alive"
  screenshot "whitescreen-screenshot.png"; collect_logs
  kill -9 "$pid" 2>/dev/null || true
  exit 2
fi
echo "PASS(2): frontend-ready marker present — React mounted"
sed 's/^/  marker> /' "$marker" || true

sleep 3
if ! kill -0 "$pid" 2>/dev/null; then
  wait "$pid"; code=$?
  echo "::error::process exited right after mounting (exit $code)"
  screenshot "postmount-crash.png"; collect_logs
  exit 1
fi
echo "PASS(1): process alive after mount (pid $pid)"

# SOFT — window search + screenshot.
if command -v xdotool >/dev/null 2>&1; then
  wins=$(xdotool search --name "CLI Pulse" 2>/dev/null | wc -l)
  if [ "$wins" -gt 0 ]; then echo "PASS(4): 'CLI Pulse' window present ($wins)"
  else echo "::warning::no 'CLI Pulse' window found via xdotool — marker gate already proved mount"; fi
fi
screenshot "launch-screenshot.png"
cp "$marker" "$OUTDIR/frontend-ready.marker" 2>/dev/null || true
collect_logs

kill -9 "$pid" 2>/dev/null || true
echo "== launch-smoke PASS =="
exit 0
