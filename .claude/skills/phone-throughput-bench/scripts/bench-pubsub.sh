#!/usr/bin/env bash
# Drive a latest-only publish/collect cycle between two phones running SimBLE
# Android, and check generation dedup. No dongle. See ../SKILL.md.
#
# A publisher raises its hand (advertises [generation][size][PSM]) and serves
# the current payload over L2CAP. A collector scans, and pulls only a generation
# newer than the one it already holds (`since`), skipping without connecting
# otherwise. This exercises: first pull, dedupe, an in-place generation bump
# over HTTP (no relaunch, PSM kept), the pull of the new generation, and dedupe
# again.
#
# Usage: bench-pubsub.sh <publisher-serial> <collector-serial> [bytes]

set -uo pipefail

PUB="${1:?usage: bench-pubsub.sh <publisher-serial> <collector-serial> [bytes]}"
COL="${2:?usage: bench-pubsub.sh <publisher-serial> <collector-serial> [bytes]}"
BYTES="${3:-65536}"
PKG="com.simble"
APK="android/app/build/simble-android.apk"
PUBIP="${PUB%%:*}"

ADB="${ADB:-$HOME/Library/Android/sdk/platform-tools/adb}"
[ -x "$ADB" ] || ADB="$(command -v adb)" || { echo "adb not found"; exit 2; }
T() { perl -e 'alarm shift; exec @ARGV' "$@"; }

echo "== ensure APK + permissions on both =="
for d in "$PUB" "$COL"; do
  T 45 "$ADB" -s "$d" install -r "$APK" >/dev/null 2>&1 && echo "  installed on $d"
  for p in BLUETOOTH_ADVERTISE BLUETOOTH_CONNECT BLUETOOTH_SCAN; do
    T 8 "$ADB" -s "$d" shell pm grant "$PKG" android.permission.$p >/dev/null 2>&1
  done
done

# One publisher: quiet the app on every other phone so the collector's
# service-UUID scan can't match a stray advertiser.
echo "== one publisher: force-stop $PKG except the publisher =="
for d in $(T 8 "$ADB" devices | awk 'NR>1 && $2=="device"{print $1}' | grep -v '_adb-tls-'); do
  [ "$d" = "$PUB" ] && continue
  T 8 "$ADB" -s "$d" shell am force-stop "$PKG" >/dev/null 2>&1
done

# The collector scans for the publisher by its advertised name.
TARGET="$(T 6 "$ADB" -s "$PUB" shell settings get global device_name 2>/dev/null | tr -d '\r')"
[ -n "$TARGET" ] || TARGET="$(T 6 "$ADB" -s "$PUB" shell getprop ro.product.model 2>/dev/null | tr -d '\r')"

echo "== publisher = $PUB (\"$TARGET\"), generation 1, $BYTES bytes =="
T 8 "$ADB" -s "$PUB" shell am force-stop "$PKG" >/dev/null 2>&1
T 8 "$ADB" -s "$PUB" shell input keyevent KEYCODE_WAKEUP >/dev/null 2>&1
T 10 "$ADB" -s "$PUB" shell \
  "am start -n $PKG/.SimbleActivity --es role publish --ei gen 1 --ei bytes $BYTES" >/dev/null 2>&1
sleep 5

# Run a collector with `since=$1`; print its one-line outcome.
collect() {
  local since="$1"
  T 6 "$ADB" -s "$COL" logcat -c >/dev/null 2>&1
  T 8 "$ADB" -s "$COL" shell am force-stop "$PKG" >/dev/null 2>&1
  T 8 "$ADB" -s "$COL" shell input keyevent KEYCODE_WAKEUP >/dev/null 2>&1
  T 10 "$ADB" -s "$COL" shell \
    "am start -n $PKG/.SimbleActivity --es role collect --ei since $since --es target '$TARGET'" \
    >/dev/null 2>&1
  local line=""
  for _ in $(seq 1 20); do
    sleep 1
    line="$(T 6 "$ADB" -s "$COL" logcat -d -s SimbleAndroid SimbleCollector 2>/dev/null \
            | grep -iE 'collected generation|nothing new|no publisher' | tail -1)"
    [ -n "$line" ] && break
  done
  echo "${line#*: }"
}

# Bump the running publisher in place over HTTP — no relaunch, PSM kept.
bump() {
  local gen="$1" size="$2"
  curl -s -m 4 "http://$PUBIP:8099/publish?gen=$gen&size=$size" 2>/dev/null
  echo
  sleep 2
}

echo "== the cycle =="
echo "  collect since=0  -> $(collect 0)   (expect: pull generation 1)"
echo "  collect since=1  -> $(collect 1)   (expect: nothing new)"
echo "  bump -> generation 2, $((BYTES*2)) bytes: $(bump 2 $((BYTES*2)))"
echo "  collect since=1  -> $(collect 1)   (expect: pull generation 2)"
echo "  collect since=2  -> $(collect 2)   (expect: nothing new)"
