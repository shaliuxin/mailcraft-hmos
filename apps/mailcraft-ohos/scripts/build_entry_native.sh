#!/usr/bin/env bash
# Build the HarmonyOS native side of the minimal link:
#   1. cross-compile the Rust C-ABI bridge (native/ohos-bridge) -> libmailcraft_ohos_bridge.a
#      (a static library; the OHOS NAPI shim is then linked against it by hvigor's CMake build)
#   2. place only that static library into entry/libs/<abi>/ for hvigor to package into the HAP.
#
# NOTE: hvigor's CMake (entry/src/main/cpp/CMakeLists.txt) compiles napi_init.cpp and links the
# bridge .a to produce libentry.so itself. We must NOT drop a pre-built libentry.so (or the bridge
# .so) into entry/libs/ -- hvigor would then find two libentry.so and fail the merge step.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO="$(cd "$ROOT/../.." && pwd)"
NDK="${OHOS_SDK:-C:/ohos-sdk/default/openharmony/native}"
ABI="arm64-v8a"; TARGET="aarch64-unknown-linux-ohos"

mkdir -p "$ROOT/entry/libs/$ABI"
echo "== [1/2] bridge staticlib ($TARGET) =="
cargo build --release --target "$TARGET" --manifest-path "$REPO/native/ohos-bridge/Cargo.toml"
cp "$REPO/native/ohos-bridge/target/$TARGET/release/libmailcraft_ohos_bridge.a" \
   "$ROOT/entry/libs/$ABI/"

echo "== [2/2] placed static library for hvigor =="
ls -la "$ROOT/entry/libs/$ABI/"
echo "done. Next: build the HAP with DevEco/hvigor (it links the .a + napi_init.cpp -> libentry.so)."
