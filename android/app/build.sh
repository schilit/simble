#!/usr/bin/env bash
# Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
#
# Builds SimBLE Sink into an installable APK using only the Android SDK build
# tools and a JDK -- no Gradle, no Kotlin compiler, no network.
#
# That is a deliberate choice for this app and not a general position. The
# app is one Java file with no dependencies; Gradle would add a wrapper
# download, a daemon and a dependency graph to a build that is four commands.
# The eventual headless runner in `docs/phone-as-backend.md` needs the NDK and
# JNI and should use Gradle. This does not.
#
#   ./build.sh            build, sign
#   ./build.sh install    also install to the connected device
#
set -euo pipefail

SDK="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
API=34
BUILD_TOOLS="$SDK/build-tools/34.0.0"
ANDROID_JAR="$SDK/platforms/android-$API/android.jar"
MIN_SDK=31   # BLUETOOTH_ADVERTISE and BLUETOOTH_CONNECT are API 31+

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
out="$here/build"
pkg=com.simble.sink

for tool in aapt2 d8 zipalign apksigner; do
    [ -x "$BUILD_TOOLS/$tool" ] || { echo "missing $tool in $BUILD_TOOLS" >&2; exit 1; }
done
[ -f "$ANDROID_JAR" ] || { echo "missing $ANDROID_JAR" >&2; exit 1; }

rm -rf "$out"
mkdir -p "$out/classes"

# A debug keystore, made once. apksigner will not sign without one, and an
# unsigned APK will not install.
keystore="$here/debug.keystore"
if [ ! -f "$keystore" ]; then
    echo "==> generating a debug keystore"
    keytool -genkeypair -keystore "$keystore" -storepass android -keypass android \
        -alias androiddebugkey -dname "CN=SimBLE Debug, O=SimBLE, C=US" \
        -keyalg RSA -keysize 2048 -validity 10000 >/dev/null 2>&1
fi

echo "==> packaging resources"
"$BUILD_TOOLS/aapt2" link \
    -I "$ANDROID_JAR" \
    --manifest "$here/AndroidManifest.xml" \
    --min-sdk-version "$MIN_SDK" \
    --target-sdk-version "$API" \
    -o "$out/base.apk"

echo "==> compiling java"
# -source/-target 11 keeps the class files inside what d8 accepts. android.jar
# on the classpath rather than the bootclasspath is what the warning is about;
# it is expected and harmless here.
javac -nowarn -source 11 -target 11 \
    -classpath "$ANDROID_JAR" \
    -d "$out/classes" \
    $(find "$here/src" -name '*.java') 2>&1 | grep -v 'bootstrap class path' || true

echo "==> dexing"
"$BUILD_TOOLS/d8" \
    --lib "$ANDROID_JAR" \
    --min-api "$MIN_SDK" \
    --output "$out" \
    $(find "$out/classes" -name '*.class')

echo "==> assembling"
(cd "$out" && zip -q base.apk classes.dex)
"$BUILD_TOOLS/zipalign" -f 4 "$out/base.apk" "$out/aligned.apk"
"$BUILD_TOOLS/apksigner" sign \
    --ks "$keystore" --ks-pass pass:android --key-pass pass:android \
    --out "$out/simble-sink.apk" "$out/aligned.apk"

echo "built $out/simble-sink.apk"

if [ "${1:-}" = "install" ]; then
    echo "==> installing"
    adb install -r "$out/simble-sink.apk"
    # Runtime permissions, granted without touching the screen. Without these
    # the app launches and immediately reports that it cannot advertise.
    adb shell pm grant $pkg android.permission.BLUETOOTH_ADVERTISE
    adb shell pm grant $pkg android.permission.BLUETOOTH_CONNECT
    echo "==> launching"
    adb shell am start -n "$pkg/.SinkActivity"
fi
