#!/usr/bin/env bash
# Benchmark BLE bulk throughput to ONE phone running SimBLE Android.
# Dongle (CSR8510) is the central; the phone is the sink. Stats come back
# off-link over HTTP. See ../SKILL.md for the topology and the traps.
#
# Usage: bench-one.sh <adb-serial> [bytes]
#   <adb-serial>  e.g. 192.168.86.90:41103 (wireless) or a USB serial
#   [bytes]       default 65536
#
# Assumes: dongle plugged in (0a12:0001), APK built at
# android/app/build/simble-android.apk, run from the repo root.

set -uo pipefail

SERIAL="${1:?usage: bench-one.sh <adb-serial> [bytes]}"
BYTES="${2:-65536}"
SELECTOR="${SIMBLE_DONGLE:-0a12:0001}"   # CSR8510 A10
PKG="com.simble"

ADB="${ADB:-$HOME/Library/Android/sdk/platform-tools/adb}"
[ -x "$ADB" ] || ADB="$(command -v adb)" || { echo "adb not found"; exit 2; }

# Hard timeout so a wedged wireless-adb call can't hang the session.
T() { perl -e 'alarm shift; exec @ARGV' "$@"; }

echo "== dongle present? =="
ioreg -p IOUSB -l -w 0 2>/dev/null | grep -i 'USB Product Name' | grep -i 'CSR8510' \
  || echo "  (no CSR8510 seen in ioreg — check the plug; system_profiler lies here)"

echo "== one advertiser: force-stop $PKG on every other device =="
for d in $(T 8 "$ADB" devices | awk 'NR>1 && $2=="device"{print $1}' | grep -v '_adb-tls-'); do
  [ "$d" = "$SERIAL" ] && continue
  T 8 "$ADB" -s "$d" shell am force-stop "$PKG" >/dev/null 2>&1 && echo "  stopped on $d"
done

echo "== launch sink on $SERIAL =="
T 10 "$ADB" -s "$SERIAL" shell monkey -p "$PKG" -c android.intent.category.LAUNCHER 1 >/dev/null 2>&1
sleep 3

# Stats path: prefer DIRECT WiFi (the app binds 0.0.0.0) so the run needs no adb;
# fall back to adb forward (the only path for a USB serial with no routable IP).
echo "== establish off-link stats channel =="
STATS=""
if [[ "$SERIAL" == *:* ]]; then
  IP="${SERIAL%%:*}"
  for i in 1 2 3 4 5 6; do
    if [ -n "$(curl -s -m 2 http://$IP:8099/stats 2>/dev/null)" ]; then
      STATS="$IP:8099"; echo "  via direct WiFi (no adb): $STATS"; break
    fi
    sleep 1
  done
fi
if [ -z "$STATS" ]; then
  T 6 "$ADB" -s "$SERIAL" forward --remove-all >/dev/null 2>&1
  if T 6 "$ADB" -s "$SERIAL" forward tcp:8099 tcp:8099 >/dev/null 2>&1; then
    for i in 1 2 3 4 5 6; do
      if [ -n "$(curl -s -m 2 http://127.0.0.1:8099/stats 2>/dev/null)" ]; then
        STATS="127.0.0.1:8099"; echo "  via adb forward: $STATS"; break
      fi
      sleep 1
    done
  fi
fi
[ -n "$STATS" ] || { echo "  !! sink stats unreachable (forward and WiFi both dead)."; \
  echo "     wireless adb likely wedged under prior BLE load — put this phone on USB."; exit 1; }

echo "== run: $BYTES bytes, dongle $SELECTOR, stats $STATS =="
SIMBLE_SINK_HTTP="$STATS" cargo run --release --example phone_bulk -- "$SELECTOR" "$BYTES"
