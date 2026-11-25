// SPDX-FileCopyrightText: MailCraft contributors (ohos port)
// SPDX-License-Identifier: MIT OR Apache-2.0
// NAPI shim for the MailCraft OHOS app. Two roles:
//   1. The smoke C ABI (runCommand/setSandbox/hostReady/testAccount) that drives the core
//      through libmailcraft_ohos_bridge.a (kept for the non-UI diagnostics).
//   2. The egui rendering bridge: an XComponent SURFACE that the Rust render worker paints
//      onto, plus input forwarding and a Rust->ArkTS notify channel. This is the OHOS twin
//      of eframe, driving the original `mailcraft-ui-egui` UI (see native/ohos-bridge/src/render.rs).

#include <napi/native_api.h>
#include <ace/xcomponent/native_interface_xcomponent.h>
#include <native_window/external_window.h>
#include <native_buffer/native_buffer.h>
#include <hilog/log.h>
#include <arkui/ui_input_event.h>

#include <cstdlib>
#include <cstring>
#include <string>
#include <vector>
#include <mutex>
#include <atomic>
#include <memory>
#include <map>
#include <sys/mman.h>
#include <unistd.h>

// ---- C ABI exported by the Rust bridge (mailcraft-ohos-bridge), statically linked in ----
extern "C" int ohos_mailcraft_init();
extern "C" int ohos_mailcraft_run(const char *cmd, const char *params);
extern "C" const char *ohos_mailcraft_result();
extern "C" int ohos_mailcraft_set_sandbox(const char *path);
extern "C" int ohos_mailcraft_set_locale(const char *locale);
extern "C" int ohos_mailcraft_host_ready();
extern "C" int ohos_mailcraft_test_account(const char *host, int port, const char *security,
                                           const char *username, const char *password);
extern "C" const char *ohos_mailcraft_test_account_result();
// The egui render bridge (P1c): the render worker paints onto an XComponent surface.
extern "C" int ohos_mailcraft_egui_init(float scale, void (*notify)(uint64_t, const uint8_t *, size_t,
                                                                    const uint8_t *, size_t));
extern "C" void ohos_mailcraft_egui_attach(uintptr_t window, uint32_t width, uint32_t height);
extern "C" void ohos_mailcraft_egui_resize(uint32_t width, uint32_t height);
extern "C" void ohos_mailcraft_egui_detach();
extern "C" void ohos_mailcraft_egui_frame();
extern "C" void ohos_mailcraft_egui_input(const uint8_t *ptr, size_t len);
extern "C" void ohos_mailcraft_egui_active(bool active);
extern "C" void ohos_mailcraft_egui_tap(float x, float y);
extern "C" void ohos_mailcraft_egui_command(const char *id, const char *params);
extern "C" void ohos_mailcraft_egui_set_phone(bool is_phone);

// The Rust `log` crate's sink: wgpu / egui diagnostics only reach the device through hilog.
extern "C" void ac_hilog(int32_t level, const char *message) {
  const LogLevel priority = level >= 2 ? LOG_ERROR : (level == 1 ? LOG_WARN : LOG_INFO);
  OH_LOG_Print(LOG_APP, priority, 0xD001, "MailCraftGL", "%{public}s", message ? message : "");
}

namespace {

// ---- Rust -> ArkTS notify channel (a thread-safe function on the ArkUI thread) ----
std::mutex notifyMutex;
napi_threadsafe_function notifyFn = nullptr;
std::atomic<bool> attached{false};
struct WinState {
  std::atomic<int32_t> touchId{-1};
};
std::mutex winMutex;
OH_NativeXComponent *curComponent = nullptr;
std::shared_ptr<WinState> curState;
std::string tag = "MailCraft";

void CallJS(napi_env env, napi_value callback, void *, void *ptr) {
  auto *r = static_cast<std::string *>(ptr);
  if (env && callback) {
    napi_value args[1];
    napi_create_string_utf8(env, r->data(), r->size(), &args[0]);
    napi_value result, receiver;
    napi_get_undefined(env, &receiver);
    napi_call_function(env, receiver, callback, 1, args, &result);
  }
  delete r;
}

void Json(const std::string &s) {
  ohos_mailcraft_egui_input(reinterpret_cast<const uint8_t *>(s.data()), s.size());
}

// ---- XComponent surface callbacks ----
void SurfaceCreated(OH_NativeXComponent *c, void *window) {
  auto *state = curState.get();
  if (!state) {
    OH_LOG_Print(LOG_APP, LOG_ERROR, 0xD001, tag.c_str(), "SurfaceCreated with no state");
    return;
  }
  uint64_t w = 0, h = 0;
  if (!window ||
      OH_NativeXComponent_GetXComponentSize(c, window, &w, &h) != OH_NATIVEXCOMPONENT_RESULT_SUCCESS) {
    OH_LOG_Print(LOG_APP, LOG_ERROR, 0xD001, tag.c_str(), "SurfaceCreated size failed");
    return;
  }
  auto ref = OH_NativeWindow_NativeObjectReference(window);
  if (ref != 0) {
    OH_LOG_Print(LOG_APP, LOG_ERROR, 0xD001, tag.c_str(), "NativeObjectReference failed %{public}d", ref);
    return;
  }
  // Pin the buffer geometry so EGL swaps into a composite-able buffer (else: black screen).
  auto *nativeWindow = static_cast<OHNativeWindow *>(window);
  OH_NativeWindow_NativeWindowHandleOpt(nativeWindow, SET_BUFFER_GEOMETRY,
                                        static_cast<int32_t>(w), static_cast<int32_t>(h));
  // GPU rendering needs HW_RENDER in the buffer usage, or eglSwapBuffers is invisible.
  uint64_t usageBefore = 0;
  OH_NativeWindow_NativeWindowHandleOpt(nativeWindow, GET_USAGE, &usageBefore);
  if ((usageBefore & NATIVEBUFFER_USAGE_HW_RENDER) == 0) {
    const uint64_t want =
        usageBefore | NATIVEBUFFER_USAGE_HW_RENDER | NATIVEBUFFER_USAGE_HW_TEXTURE;
    OH_NativeWindow_NativeWindowHandleOpt(nativeWindow, SET_USAGE, want);
  }
  attached = true;
  OH_LOG_Print(LOG_APP, LOG_INFO, 0xD001, tag.c_str(),
               "Surface created %{public}llu x %{public}llu",
               static_cast<unsigned long long>(w), static_cast<unsigned long long>(h));
  ohos_mailcraft_egui_attach(reinterpret_cast<uintptr_t>(window), static_cast<uint32_t>(w),
                             static_cast<uint32_t>(h));
}
void SurfaceChanged(OH_NativeXComponent *c, void *window) {
  if (!curState) return;
  uint64_t w = 0, h = 0;
  if (OH_NativeXComponent_GetXComponentSize(c, window, &w, &h) != OH_NATIVEXCOMPONENT_RESULT_SUCCESS)
    return;
  ohos_mailcraft_egui_resize(static_cast<uint32_t>(w), static_cast<uint32_t>(h));
}
void SurfaceDestroyed(OH_NativeXComponent *, void *) {
  if (!curState) return;
  attached = false;
  ohos_mailcraft_egui_detach();
}
void Touch(OH_NativeXComponent *c, void *window) {
  if (!curState) return;
  OH_NativeXComponent_TouchEvent e{};
  if (OH_NativeXComponent_GetTouchEvent(c, window, &e) != OH_NATIVEXCOMPONENT_RESULT_SUCCESS) return;
  OH_LOG_Print(LOG_APP, LOG_INFO, 0xD001, tag.c_str(),
               "Touch type %{public}d numPoints %{public}u",
               static_cast<int>(e.type), static_cast<unsigned>(e.numPoints));
  if (e.type == OH_NATIVEXCOMPONENT_CANCEL) {
    curState->touchId = -1;
    Json("{\"kind\":\"blur\"}");
    return;
  }
  // egui distinguishes a click by a press event followed by a release, so the per-point
  // `pressed` flag must follow the XComponent event type: a DOWN/MOVE keeps the pointer down,
  // an UP releases it. Skipping the UP (the old behaviour) left egui with only a "press" that
  // never released, so no click ever registered.
  const bool pressed = (e.type == OH_NATIVEXCOMPONENT_DOWN || e.type == OH_NATIVEXCOMPONENT_MOVE);
  std::string points;
  for (uint32_t i = 0; i < e.numPoints && i < OH_NATIVE_XCOMPONENT_MAX_TOUCH_POINTS_NUMBER; ++i) {
    const auto &point = e.touchPoints[i];
    if (!points.empty()) points += ",";
    points += "{\"id\":" + std::to_string(static_cast<uint32_t>(point.id)) +
              ",\"x\":" + std::to_string(point.x) + ",\"y\":" + std::to_string(point.y) +
              ",\"pressed\":" + std::string(pressed ? "true" : "false") + "}";
  }
  Json(std::string("{\"kind\":\"touch\",\"points\":[") + points + "]}");
}
OH_NativeXComponent_Callback callbacks{SurfaceCreated, SurfaceChanged, SurfaceDestroyed, Touch};

// Register the callback set on the XComponent's native handle, given the component object.
void RegisterComponent(OH_NativeXComponent *c) {
  if (!c) return;
  auto state = std::make_shared<WinState>();
  {
    std::lock_guard<std::mutex> lock(winMutex);
    curComponent = c;
    curState = state;
  }
  OH_LOG_Print(LOG_APP, LOG_INFO, 0xD001, tag.c_str(), "Registering XComponent callbacks");
  OH_NativeXComponent_RegisterCallback(c, &callbacks);
}

std::string String(napi_env env, napi_value v) {
  size_t len = 0;
  if (!v || napi_get_value_string_utf8(env, v, nullptr, 0, &len) != napi_ok || len > 1024 * 1024) return {};
  std::vector<char> data(len + 1);
  if (napi_get_value_string_utf8(env, v, data.data(), data.size(), &len) != napi_ok) return {};
  return std::string(data.data(), len);
}
napi_value Undefined(napi_env env) {
  napi_value v;
  napi_get_undefined(env, &v);
  return v;
}

}  // namespace

void NotifyRust(uint64_t id, const uint8_t *json, size_t n, const uint8_t *data, size_t size) {
  std::lock_guard<std::mutex> lock(notifyMutex);
  if (!notifyFn) return;
  auto *r = new std::string(reinterpret_cast<const char *>(json), n);
  if (napi_call_threadsafe_function(notifyFn, r, napi_tsfn_nonblocking) != napi_ok) delete r;
}

// ---- egui NAPI entry points ----

// Start the render worker: builds the egui Context + spawns the render thread. Reads a config
// string (unused by the placeholder) and a notify callback (used for Rust->ArkTS).
static napi_value EguiInit(napi_env env, napi_callback_info info) {
  size_t n = 2;
  napi_value args[2];
  napi_get_cb_info(env, info, &n, args, nullptr, nullptr);
  double scale = 1.0;
  if (n > 0) napi_get_value_double(env, args[0], &scale);
  {
    std::lock_guard<std::mutex> lock(notifyMutex);
    if (notifyFn) napi_release_threadsafe_function(notifyFn, napi_tsfn_abort);
    napi_value name;
    napi_create_string_utf8(env, "MailCraft platform", NAPI_AUTO_LENGTH, &name);
    napi_value cb = (n > 1 && args[1]) ? args[1] : nullptr;
    auto status = napi_create_threadsafe_function(env, cb, nullptr, name, 0, 1, nullptr, nullptr,
                                                  nullptr, CallJS, &notifyFn);
    OH_LOG_Print(LOG_APP, LOG_INFO, 0xD001, tag.c_str(), "TSFN status %{public}d",
                 static_cast<int>(status));
    if (status == napi_ok) napi_unref_threadsafe_function(env, notifyFn);
  }
  int rc = ohos_mailcraft_egui_init(static_cast<float>(scale), NotifyRust);
  OH_LOG_Print(LOG_APP, LOG_INFO, 0xD001, tag.c_str(),
               "egui init rc %{public}d scale %{public}f", rc, static_cast<double>(scale));
  return Undefined(env);
}

// Forward a JSON input event to the render worker (from ETS, e.g. a soft-keyboard text commit).
static napi_value EguiInput(napi_env env, napi_callback_info info) {
  size_t n = 1;
  napi_value arg;
  napi_get_cb_info(env, info, &n, &arg, nullptr, nullptr);
  if (n) Json(String(env, arg));
  return Undefined(env);
}
static napi_value EguiFrame(napi_env env, napi_callback_info info) {
  (void)env;
  (void)info;
  ohos_mailcraft_egui_frame();
  return Undefined(env);
}
static napi_value EguiActive(napi_env env, napi_callback_info info) {
  size_t n = 1;
  napi_value arg;
  napi_get_cb_info(env, info, &n, &arg, nullptr, nullptr);
  bool active = false;
  if (n) napi_get_value_bool(env, arg, &active);
  ohos_mailcraft_egui_active(active);
  return Undefined(env);
}

// Report whether this device is a phone (from `@ohos.deviceInfo.deviceType`). Call before
// `eguiInit` so the render thread, spawned by `eguiInit`, reads it and sets the UI's
// `phone_layout`: a phone shows the phone interface, a tablet / 2-in-1 PC keeps the desktop UI.
static napi_value EguiSetPhone(napi_env env, napi_callback_info info) {
  size_t n = 1;
  napi_value arg;
  napi_get_cb_info(env, info, &n, &arg, nullptr, nullptr);
  bool is_phone = false;
  if (n) napi_get_value_bool(env, arg, &is_phone);
  ohos_mailcraft_egui_set_phone(is_phone);
  return Undefined(env);
}

// Self-test hook: a synthetic click at egui *point* coordinates (x, y are points, not device
// pixels). Bypasses the XComponent → device-pixel routing so a tap proves click → select →
// repaint regardless of window position on a multi-window HarmonyOS PC.
static napi_value EguiTap(napi_env env, napi_callback_info info) {
  size_t n = 2;
  napi_value args[2];
  napi_get_cb_info(env, info, &n, args, nullptr, nullptr);
  // napi_get_value_double writes a 8-byte double; read into a double, then narrow to the
  // bridge's float. (Writing a double into a float's 4 bytes corrupts the value.)
  double dx = 0;
  double dy = 0;
  if (n > 0) napi_get_value_double(env, args[0], &dx);
  if (n > 1) napi_get_value_double(env, args[1], &dy);
  ohos_mailcraft_egui_tap(static_cast<float>(dx), static_cast<float>(dy));
  return Undefined(env);
}

// Programmatic-command hook: run an engine command (id + JSON params) on the UI's MailApp
// session. Turns a tap / menu press into the same action the engine runs, so the session
// mutates and the next frame's reading pane reflects it — without depending on pixel coords.
static napi_value EguiCommand(napi_env env, napi_callback_info info) {
  size_t argc = 2;
  napi_value argv[2];
  napi_get_cb_info(env, info, &argc, argv, nullptr, nullptr);
  char id_buf[256] = {0};
  char params_buf[1024] = {0};
  size_t len = 0;
  if (argc > 0) napi_get_value_string_utf8(env, argv[0], id_buf, sizeof(id_buf) - 1, &len);
  if (argc > 1) napi_get_value_string_utf8(env, argv[1], params_buf, sizeof(params_buf) - 1, &len);
  ohos_mailcraft_egui_command(id_buf, params_buf);
  return Undefined(env);
}

// ---- Keep the smoke C ABI for the non-UI diagnostics ----
static napi_value RunCommand(napi_env env, napi_callback_info info) {
  size_t argc = 2;
  napi_value argv[2];
  napi_get_cb_info(env, info, &argc, argv, nullptr, nullptr);
  char cmd_buf[512] = {0};
  char params_buf[1024] = {0};
  size_t len = 0;
  if (argc > 0) napi_get_value_string_utf8(env, argv[0], cmd_buf, sizeof(cmd_buf) - 1, &len);
  if (argc > 1) napi_get_value_string_utf8(env, argv[1], params_buf, sizeof(params_buf) - 1, &len);
  ohos_mailcraft_init();
  int rc = ohos_mailcraft_run(cmd_buf, params_buf);
  const char *result = ohos_mailcraft_result();
  std::string out;
  out += "[rc=";
  out += std::to_string(rc);
  out += "] ";
  out += result ? result : "null";
  napi_value reply;
  napi_create_string_utf8(env, out.c_str(), NAPI_AUTO_LENGTH, &reply);
  return reply;
}
static napi_value SetSandbox(napi_env env, napi_callback_info info) {
  size_t argc = 1;
  napi_value argv[1];
  napi_get_cb_info(env, info, &argc, argv, nullptr, nullptr);
  char path_buf[1024] = {0};
  size_t len = 0;
  if (argc > 0) napi_get_value_string_utf8(env, argv[0], path_buf, sizeof(path_buf) - 1, &len);
  int rc = ohos_mailcraft_set_sandbox(path_buf);
  std::string out = "[rc=" + std::to_string(rc) + "] sandbox";
  napi_value reply;
  napi_create_string_utf8(env, out.c_str(), NAPI_AUTO_LENGTH, &reply);
  return reply;
}
// SetLocale hands the system's locale (e.g. "zh_CN") to the bridge, so a "auto" language
// preference (the default) resolves to the system's language. Idempotent; "rc=0" on a set.
static napi_value SetLocale(napi_env env, napi_callback_info info) {
  size_t argc = 1;
  napi_value argv[1];
  napi_get_cb_info(env, info, &argc, argv, nullptr, nullptr);
  char locale_buf[64] = {0};
  size_t len = 0;
  if (argc > 0) napi_get_value_string_utf8(env, argv[0], locale_buf, sizeof(locale_buf) - 1, &len);
  int rc = ohos_mailcraft_set_locale(locale_buf);
  std::string out = "[rc=" + std::to_string(rc) + "] locale " + locale_buf;
  napi_value reply;
  napi_create_string_utf8(env, out.c_str(), NAPI_AUTO_LENGTH, &reply);
  return reply;
}
static napi_value HostReady(napi_env env, napi_callback_info info) {
  int rc = ohos_mailcraft_host_ready();
  const char *result = ohos_mailcraft_result();
  std::string out;
  out += "[rc=";
  out += std::to_string(rc);
  out += "] ";
  out += result ? result : "null";
  napi_value reply;
  napi_create_string_utf8(env, out.c_str(), NAPI_AUTO_LENGTH, &reply);
  return reply;
}
static napi_value TestAccount(napi_env env, napi_callback_info info) {
  size_t argc = 5;
  napi_value argv[5];
  napi_get_cb_info(env, info, &argc, argv, nullptr, nullptr);
  char host_buf[256] = {0};
  char port_buf[16] = {0};
  char sec_buf[32] = {0};
  char user_buf[512] = {0};
  char pass_buf[512] = {0};
  size_t len = 0;
  if (argc > 0) napi_get_value_string_utf8(env, argv[0], host_buf, sizeof(host_buf) - 1, &len);
  if (argc > 1) napi_get_value_string_utf8(env, argv[1], port_buf, sizeof(port_buf) - 1, &len);
  if (argc > 2) napi_get_value_string_utf8(env, argv[2], sec_buf, sizeof(sec_buf) - 1, &len);
  if (argc > 3) napi_get_value_string_utf8(env, argv[3], user_buf, sizeof(user_buf) - 1, &len);
  if (argc > 4) napi_get_value_string_utf8(env, argv[4], pass_buf, sizeof(pass_buf) - 1, &len);
  int rc = ohos_mailcraft_test_account(host_buf, std::atoi(port_buf), sec_buf, user_buf, pass_buf);
  std::string out = "[rc=" + std::to_string(rc) + "] started";
  napi_value reply;
  napi_create_string_utf8(env, out.c_str(), NAPI_AUTO_LENGTH, &reply);
  return reply;
}
static napi_value TestAccountResult(napi_env env, napi_callback_info info) {
  const char *result = ohos_mailcraft_test_account_result();
  std::string out = "[rc=0] ";
  out += result ? result : "none";
  napi_value reply;
  napi_create_string_utf8(env, out.c_str(), NAPI_AUTO_LENGTH, &reply);
  return reply;
}

EXTERN_C_START
static napi_value Init(napi_env env, napi_value exports) {
  napi_property_descriptor desc[] = {
      {"runCommand", nullptr, RunCommand, nullptr, nullptr, nullptr, napi_default, nullptr},
      {"setSandbox", nullptr, SetSandbox, nullptr, nullptr, nullptr, napi_default, nullptr},
      {"setLocale", nullptr, SetLocale, nullptr, nullptr, nullptr, napi_default, nullptr},
      {"hostReady", nullptr, HostReady, nullptr, nullptr, nullptr, napi_default, nullptr},
      {"testAccount", nullptr, TestAccount, nullptr, nullptr, nullptr, napi_default, nullptr},
      {"testAccountResult", nullptr, TestAccountResult, nullptr, nullptr, nullptr, napi_default, nullptr},
      {"eguiInit", nullptr, EguiInit, nullptr, nullptr, nullptr, napi_default, nullptr},
      {"eguiInput", nullptr, EguiInput, nullptr, nullptr, nullptr, napi_default, nullptr},
      {"eguiFrame", nullptr, EguiFrame, nullptr, nullptr, nullptr, napi_default, nullptr},
      {"eguiActive", nullptr, EguiActive, nullptr, nullptr, nullptr, napi_default, nullptr},
      {"eguiTap", nullptr, EguiTap, nullptr, nullptr, nullptr, napi_default, nullptr},
      {"eguiCommand", nullptr, EguiCommand, nullptr, nullptr, nullptr, napi_default, nullptr},
      {"eguiSetPhone", nullptr, EguiSetPhone, nullptr, nullptr, nullptr, napi_default, nullptr},
  };
  napi_define_properties(env, exports, sizeof(desc) / sizeof(desc[0]), desc);

  // Register the XComponent callbacks when the ArkTS side hands us its native surface handle.
  napi_value value;
  if (napi_get_named_property(env, exports, OH_NATIVE_XCOMPONENT_OBJ, &value) == napi_ok) {
    OH_NativeXComponent *c = nullptr;
    if (napi_unwrap(env, value, reinterpret_cast<void **>(&c)) == napi_ok) {
      RegisterComponent(c);
    }
  }
  return exports;
}
EXTERN_C_END

// The .so is libentry.so; the module is "entry". The XComponent libraryname is "entry".
static napi_module _module = {
    .nm_version = 1,
    .nm_flags = 0,
    .nm_filename = nullptr,
    .nm_register_func = Init,
    .nm_modname = "entry",
    .nm_priv = nullptr,
    .reserved = {0},
};

extern "C" __attribute__((constructor)) void RegisterMailCraftModule() {
  napi_module_register(&_module);
}

// Kept for the runtime-hook loading path as well, so both import styles work.
napi_module *napi_get_runtime_hook_module() {
  return &_module;
}
