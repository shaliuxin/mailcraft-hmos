# Minimal HarmonyOS NAPI shim: builds libentry.so, which loads the MailCraft core
# through the C ABI of libmailcraft_ohos_bridge.a and exposes `runCommand` to ArkUI.
cmake_minimum_required(VERSION 3.5.0)
project(mailcraft_entry)

set(NATIVERENDER_ROOT_PATH ${CMAKE_CURRENT_SOURCE_DIR})

# The Rust core, as a static library, per ABI.
set(BRIDGE_LIB "${NATIVERENDER_ROOT_PATH}/../../../libs/${CMAKE_OHOS_ARCH_ABI}/libmailcraft_ohos_bridge.a")

add_library(entry SHARED napi_init.cpp)

target_link_libraries(entry PUBLIC
    libace_napi.z.so
    # XComponent native surface API (OH_NativeXComponent_*) lives in libace_ndk.z.so in this SDK.
    libace_ndk.z.so
    # NativeWindow / NativeBuffer APIs (OH_NativeWindow_*, buffer usage flags) use the plain
    # non-".z" names in this SDK's sysroot.
    libnative_window.so
    libnative_buffer.so
    libhilog_ndk.z.so
    -ldl)

# Link the Rust core static library (must exist; built by native/ohos-bridge for this ABI).
target_link_libraries(entry PUBLIC "${BRIDGE_LIB}")
