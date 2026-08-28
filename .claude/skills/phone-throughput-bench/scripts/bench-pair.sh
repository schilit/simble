#!/usr/bin/env bash
# Benchmark phone-to-phone BLE bulk throughput: one phone SOURCE, one SINK,
# both running SimBLE Android, with NO dongle in the path. The source drives the
# transfer over Android's own BluetoothGatt central role (BulkSource); the sink
# counts what lands and REPORTs it back on the control point. Both figures are
# phone-clock and come back over the BLE link's REPORT — no HTTP, so the wifi
# doze that makes /stats flaky here is out of the loop. See ../SKILL.md.
#
# Usage: bench-pair.sh <source-serial> <sink-serial> [bytes]
#   <source-serial>  the phone that sends (needs BLUETOOTH_SCAN)
#   <sink-serial>    the phone that receives (advertises f0bb0001)
#   [bytes]          default 65536
#
# Assumes: APK built at android/app/build/simble-android.apk, run from repo root.

set -uo pipefail

SRC="${1:?usage: bench-pair.sh <source-serial> <sink-serial> [bytes]}"
SINK="${2:?usage: bench-pair.sh <source-serial> <sink-serial> [bytes]}"
BYTES="${3:-65536}"
PKG="com.simble"
APK="android/app/build/simble-android.apk"

ADB="${ADB:-$HOME/Library/Android/sdk/platform-tools/adb}"
[ -x "$ADB" ] || ADB="$(command -v adb)" || { echo "adb not found"; exit 2; }

# Hard timeout so a wedged wireless-adb call can't hang the session.
T() { perl -e 'alarm shift; exec @ARGV' "$@"; }

# Ensure both phones have the current APK and the right permissions. Cheap and
# idempotent; a stale sink (no source role) or a source missing BLUETOOTH_SCAN
# is the usual first-run failure.
echo "== ensure APK + permissions on both =="
for d in "$SRC" "$SINK"; do
  T 40 "$ADB" -s "$d" install -r "$APK" >/dev/null 2>&1 \
    && echo "  installed on $d" || echo "  (install skipped/failed on $d)"
  for p in BLUETOOTH_ADVERTISE BLUETOOTH_CONNECT BLUETOOTH_SCAN; do
    T 8 "$ADB" -s "$d" shell pm grant "$PKG" android.permission.$p >/dev/null 2>&1
  done
done

# One advertiser only: any other phone still advertising f0bb0001 could be the
# one the source grabs. Force-stop the app everywhere except the sink.
echo "== one advertiser: force-stop $PKG except the sink =="
for d in $(T 8 "$ADB" devices | awk 'NR>1 && $2=="device"{print $1}' | grep -v '_adb-tls-'); do
  [ "$d" = "$SINK" ] && continue
  T 8 "$ADB" -s "$d" shell am force-stop "$PKG" >/dev/null 2>&1
done

# The sink's advertised name is what the source scans for. Only the sink
# advertises the service (others are stopped), so even a shared name resolves.
TARGET="$(T 6 "$ADB" -s "$SINK" shell settings get global device_name 2>/dev/null | tr -d '\r')"
[ -n "$TARGET" ] || TARGET="$(T 6 "$ADB" -s "$SINK" shell getprop ro.product.model 2>/dev/null | tr -d '\r')"

echo "== sink = $SINK  (advertises \"$TARGET\") =="
T 8 "$ADB" -s "$SINK" shell am force-stop "$PKG" >/dev/null 2>&1
T 8 "$ADB" -s "$SINK" shell input keyevent KEYCODE_WAKEUP >/dev/null 2>&1
T 10 "$ADB" -s "$SINK" shell am start -n "$PKG/.SimbleActivity" >/dev/null 2>&1
sleep 5   # let the GATT server register and the advertiser actually start
T 6 "$ADB" -s "$SINK" logcat -c >/dev/null 2>&1

echo "== source = $SRC  →  \"$TARGET\", $BYTES bytes =="
T 6 "$ADB" -s "$SRC" logcat -c >/dev/null 2>&1
T 8 "$ADB" -s "$SRC" shell am force-stop "$PKG" >/dev/null 2>&1
T 8 "$ADB" -s "$SRC" shell input keyevent KEYCODE_WAKEUP >/dev/null 2>&1
# The remote shell re-splits on spaces; single-quote the (spaced) sink name.
T 10 "$ADB" -s "$SRC" shell \
  "am start -n $PKG/.SimbleActivity --es role source --es target '$TARGET' --ei bytes $BYTES" \
  >/dev/null 2>&1

# Discovery can take 30 s+ before the transfer even starts (Android scan
# latency finding one named advertiser), then the transfer itself is ~1 s.
echo "== waiting for the transfer (discovery can take 30 s+) =="
RESULT=""
for i in $(seq 1 50); do
  sleep 1
  line="$(T 6 "$ADB" -s "$SRC" logcat -d -s SimbleAndroid 2>/dev/null \
          | grep -iE 'done —|stopped —|dropped' | tail -1)"
  if [ -n "$line" ]; then RESULT="${line#*SimbleAndroid: }"; break; fi
done

[ -n "$RESULT" ] || { echo "  !! no result — the source never finished."; \
  echo "     check both phones are awake, BT on, and the APK current (rebuild + rerun)."; exit 1; }

SINKLINE="$(T 6 "$ADB" -s "$SINK" logcat -d -s SimbleAndroid 2>/dev/null \
            | grep -iE 'run complete|FINISH' | tail -1 | sed 's/.*SimbleAndroid: //')"

echo ""
echo "  source ($SRC): $RESULT"
echo "  sink   ($SINK): ${SINKLINE:-<no FINISH line — source clock only>}"
