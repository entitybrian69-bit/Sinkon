#!/usr/bin/env bash
# =============================================================================
#  Build libsinkon.so for every Android ABI and drop it into the Kotlin module.
# =============================================================================
#  Requirements: Rust stable + `cargo-ndk`, Android NDK r26+ (ANDROID_NDK_HOME or
#  ANDROID_NDK_ROOT), `rustup target add` for the three targets.
#
#  Usage:
#      scripts/build-android.sh                 # release, all ABIs
#      scripts/build-android.sh --debug         # debug build (keeps symbols)
#      scripts/build-android.sh --abi=arm64-v8a # one ABI only
#      scripts/build-android.sh --features=tracy,alloc-gate
#
#  Output: android/app/src/main/jniLibs/<abi>/libsinkon.so
# =============================================================================
set -eu

LOG="[sinkon-build]"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
JNI_DIR="$ROOT/android/app/src/main/jniLibs"

PROFILE="release"
PROFILE_FLAG="--release"
FEATURES=""
ABIS="arm64-v8a armeabi-v7a x86_64"

usage() {
    sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'
}

while [ $# -gt 0 ]; do
    case "$1" in
        --help|-h) usage; exit 0 ;;
        --debug) PROFILE="debug"; PROFILE_FLAG="" ;;
        --release) PROFILE="release"; PROFILE_FLAG="--release" ;;
        --abi=*) ABIS="${1#--abi=}" ;;
        --features=*) FEATURES="${1#--features=}" ;;
        *) printf '%s unknown option: %s\n' "$LOG" "$1" >&2; exit 2 ;;
    esac
    shift
done

command -v cargo >/dev/null 2>&1 || { printf '%s cargo not found (https://rustup.rs)\n' "$LOG" >&2; exit 2; }
command -v cargo-ndk >/dev/null 2>&1 || {
    printf '%s cargo-binstall/cargo install cargo-ndk is required\n' "$LOG" >&2
    exit 2
}
if [ -z "${ANDROID_NDK_HOME:-}" ] && [ -z "${ANDROID_NDK_ROOT:-}" ]; then
    printf '%s set ANDROID_NDK_HOME (NDK r26+) before building\n' "$LOG" >&2
    exit 2
fi

for abi in $ABIS; do
    case "$abi" in
        arm64-v8a|armeabi-v7a|x86_64) ;;
        *) printf '%s unsupported ABI %s (expected arm64-v8a, armeabi-v7a, x86_64)\n' "$LOG" "$abi" >&2; exit 2 ;;
    esac
done

FEATURE_ARG=""
if [ -n "$FEATURES" ]; then
    FEATURE_ARG="--features $FEATURES"
fi

build_one() {
    abi="$1"
    printf '%s building %s (%s)\n' "$LOG" "$abi" "$PROFILE"
    # shellcheck disable=SC2086
    cargo ndk --platform 26 --target "$abi" build $PROFILE_FLAG $FEATURE_ARG -p sinkon
    src="$ROOT/target/$abi/$PROFILE/libsinkon.so"
    if [ ! -f "$src" ]; then
        printf '%s expected %s to exist after the build\n' "$LOG" "$src" >&2
        exit 1
    fi
    mkdir -p "$JNI_DIR/$abi"
    cp "$src" "$JNI_DIR/$abi/libsinkon.so"
    size=$(wc -c < "$JNI_DIR/$abi/libsinkon.so" | tr -d ' ')
    printf '%s   → %s (%s bytes)\n' "$LOG" "$JNI_DIR/$abi/libsinkon.so" "$size"
}

for abi in $ABIS; do
    build_one "$abi"
done

printf '%s done. Rebuild the APK with ./gradlew assembleDebug\n' "$LOG"
