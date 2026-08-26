#!/usr/bin/env bash
# Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
#
# Builds SimBLE Android into an installable APK using only the Android SDK build
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
pkg=com.simble

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
    --out "$out/simble-android.apk" "$out/aligned.apk"

echo "built $out/simble-android.apk"

if [ "${1:-}" = "install" ]; then
    # A shell without the SDK on PATH still has to find adb, and one phone can
    # show up twice — a wifi transport and an mdns one are two adb devices and
    # one radio, which is enough for adb to refuse to guess.
    ADB="$(command -v adb || true)"
    for candidate in "${ANDROID_HOME:-}/platform-tools/adb" \
                     "$HOME/Library/Android/sdk/platform-tools/adb" \
                     "$HOME/Android/Sdk/platform-tools/adb"; do
        [ -n "$ADB" ] && break
        [ -x "$candidate" ] && ADB="$candidate"
    done
    if [ -z "$ADB" ]; then
        echo "adb not found — put it on PATH or set ANDROID_HOME" >&2
        exit 1
    fi
    if [ -z "${ANDROID_SERIAL:-}" ] \
       && [ "$("$ADB" devices | grep -c "device$")" -gt 1 ]; then
        echo "more than one device; set ANDROID_SERIAL to one of:" >&2
        "$ADB" devices | grep "device$" >&2
        exit 1
    fi
    echo "==> installing"
    # A renamed package is a *different* app to Android. Leaving the old one
    # installed means two icons and two advertisers on the same service UUID,
    # which a scan filtering by name cannot tell apart.
    "$ADB" uninstall com.simble.sink >/dev/null 2>&1 || true
    "$ADB" install -r "$out/simble-android.apk"
    # Runtime permissions, granted without touching the screen. Without these
    # the app launches and immediately reports that it cannot advertise.
    "$ADB" shell pm grant $pkg android.permission.BLUETOOTH_ADVERTISE
    "$ADB" shell pm grant $pkg android.permission.BLUETOOTH_CONNECT
    echo "==> launching"
    "$ADB" shell am start -n "$pkg/.SimbleActivity"
fi
