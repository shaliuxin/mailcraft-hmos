// SPDX-FileCopyrightText: MailCraft contributors (ohos port)
// SPDX-License-Identifier: MIT OR Apache-2.0
//! C ABI bridge for the HarmonyOS NAPI shim: it drives `mailcraft_engine`'s command
//! registry (the same path the CLI / control channel / MCP use) through a couple of
//! `#[no_mangle] extern "C"` entry points. The NAPI `libentry.so` calls these; the
//! ArkUI layer then renders the JSON the bridge hands back.
//!
//! The core never crashes: a command that fails returns an error string, never a panic.
//! Everything the native side passes in (a command id, a JSON parameter blob) is treated
//! as hostile input.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use mailcraft_engine::{sample, Session};
use mailcraft_host::{Host, init as host_init};
use mailcraft_model::{Account, AccountKind, Security, Server};
use serde_json::{json, Value};

mod ohos_platform;
pub use ohos_platform::OhosPlatform;

// ---- P1c: egui rendering harness (the OHOS twin of eframe) ----
mod input;
mod render;

use std::sync::mpsc;
use std::sync::atomic::Ordering;

/// Messages the NAPI thread posts to the render thread over `SEND`.
#[allow(dead_code)]
enum Message {
    /// The XComponent surface appeared: build the wgpu surface + egui renderer on it.
    Attach(usize, u32, u32),
    /// The surface changed size.
    Resize(u32, u32),
    /// The surface disappeared: drop the wgpu surface + renderer.
    Detach,
    /// Render one frame now.
    Frame,
    /// Foreground / background toggle.
    Active(bool),
    /// An input event from the XComponent surface.
    Input(input::Input),
    /// A programmatic command run on the UI's `MailApp` session — e.g. `mail.select`,
    /// `mail.first`, `view.mail`. `params` is a JSON string (may be empty). This is the
    /// "a tap / menu press became an engine command" path: the session mutates and the next
    /// frame's reading pane reflects it. `id` is the command id; `params` the JSON params.
    Command { id: String, params: String },
    /// The egui Context asked to wake (a timer); re-render after the delay.
    Wake(std::time::Duration),
}

/// The one channel from the NAPI thread to the render thread, set on first `egui_init`.
static SEND: std::sync::OnceLock<mpsc::Sender<Message>> = std::sync::OnceLock::new();
fn send(message: Message) {
    if let Some(tx) = SEND.get() {
        let _ = tx.send(message);
    }
}

/// Turn `log` records into hilog so wgpu / egui diagnostics are visible on device.
#[cfg(target_env = "ohos")]
mod glog {
    use std::ffi::CString;
    use std::os::raw::c_char;

    unsafe extern "C" {
        fn ac_hilog(level: i32, message: *const c_char);
    }

    struct Hilog;

    impl log::Log for Hilog {
        fn enabled(&self, _: &log::Metadata<'_>) -> bool {
            true
        }
        fn log(&self, record: &log::Record<'_>) {
            let level = match record.level() {
                log::Level::Error => 2,
                log::Level::Warn => 1,
                _ => 0,
            };
            let line = format!("[{}] {}: {}", record.level(), record.target(), record.args());
            if let Ok(text) = CString::new(line) {
                unsafe { ac_hilog(level, text.as_ptr()) };
            }
        }
        fn flush(&self) {}
    }

    static LOGGER: Hilog = Hilog;

    pub fn install() {
        if log::set_logger(&LOGGER).is_ok() {
            log::set_max_level(log::LevelFilter::Info);
        }
    }
}

#[allow(dead_code)]
fn ensure_glog() {
    #[cfg(target_env = "ohos")]
    glog::install();
}

/// The notify callback type the C++ bridge implements (Rust -> ArkTS).
pub type Notify = extern "C" fn(id: u64, kind: *const u8, kind_len: usize, data: *const u8, data_len: usize);

/// One `Session` for the process. `Session` is `!Send`, so it lives behind a `Mutex`
/// on the one thread the NAPI shim marshals calls onto.
static SESSION: Mutex<Option<Session>> = Mutex::new(None);
/// The last result (a JSON blob, or an `{"error": …}` blob) as a null-terminated C string.
static RESULT: Mutex<Option<CString>> = Mutex::new(None);
/// The app's sandbox directory, when the NAPI layer has given one: the credential store is a
/// file inside it.
static SANDBOX: OnceLock<PathBuf> = OnceLock::new();
/// The system's locale (`zh_CN`, `en_US`, …), when the NAPI layer has given one: the interface
/// language follows it while its preference is "auto" (which it is by default).
static LOCALE: OnceLock<String> = OnceLock::new();

/// The sandbox the NAPI layer set (via `ohos_mailcraft_set_sandbox`), if any. The render thread
/// reads this to build the credential store the UI's `Services` uses (`secrets`), so passwords a
/// real account keeps survive between runs (in `secrets.json` inside the sandbox).
#[allow(dead_code)]
pub fn sandbox() -> Option<PathBuf> {
    SANDBOX.get().cloned()
}
/// The system's locale the NAPI layer set (via `ohos_mailcraft_set_locale`), if any. The render
/// thread reads this to feed the UI's `Services.system_locale`, so a "auto" language preference
/// (the default) resolves to the system's language — Chinese on a Chinese system, English otherwise.
#[allow(dead_code)]
pub fn locale() -> Option<String> {
    LOCALE.get().cloned()
}
/// The one `Host` (platform + memory budget), built lazily, for the async sync jobs to run on.
static HOST: Mutex<Option<Host>> = Mutex::new(None);
/// What the last "test account" (a real IMAP/SMTP connection) brought back: "pending" while it
/// runs on its own thread, "ok: …" / "error: …" when it is done. Never a password.
static TEST_RESULT: Mutex<Option<CString>> = Mutex::new(None);

fn store_result(json: &Value) {
    let text = json.to_string();
    let c = match CString::new(text) {
        Ok(c) => Some(c),
        Err(_) => Some(CString::new("{\"error\":\"nul byte in result\"}").unwrap_or_default()),
    };
    if let Ok(mut slot) = RESULT.lock() {
        *slot = c;
    }
}

fn store_err(msg: &str) -> i32 {
    store_result(&json!({ "error": msg }));
    1
}

/// Build the sample mailbox and hand it to a fresh `Session`. Idempotent. Returns `0`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ohos_mailcraft_init() -> i32 {
    let now = mailcraft_time::now();
    let mailbox = sample::mailbox(now, 0);
    let session = Session::new(mailbox);
    if let Ok(mut slot) = SESSION.lock() {
        *slot = Some(session);
    }
    0
}

/// Run a command by id with a JSON parameter blob (an empty/NULL string means `{}`).
/// The result JSON is stored; `ohos_mailcraft_result` returns it. Returns `0` on a run
/// (success or a typed error, both surfaced as JSON), `1` when something can't be parsed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ohos_mailcraft_run(cmd: *const c_char, params: *const c_char) -> i32 {
    let cmd_id = if cmd.is_null() {
        ""
    } else {
        match unsafe { CStr::from_ptr(cmd).to_str() } {
            Ok(s) => s,
            Err(_) => return store_err("bad command id (not UTF-8)"),
        }
    };

    let params_json = if params.is_null() || unsafe { *params } == 0 {
        json!({})
    } else {
        match unsafe { CStr::from_ptr(params).to_str() } {
            Ok(s) if s.trim().is_empty() => json!({}),
            Ok(s) => match serde_json::from_str::<Value>(s) {
                Ok(v) => v,
                Err(_) => return store_err("bad parameters (not JSON)"),
            },
            Err(_) => return store_err("bad parameters (not UTF-8)"),
        }
    };

    let mut slot = match SESSION.lock() {
        Ok(s) => s,
        Err(_) => return store_err("session mutex poisoned"),
    };
    if slot.is_none() {
        // Lazy init so the NAPI side can skip an explicit init call.
        let now = mailcraft_time::now();
        *slot = Some(Session::new(sample::mailbox(now, 0)));
    }
    let session = slot.as_mut().expect("session just set above");
    match session.run(cmd_id, &params_json) {
        Ok(value) => {
            store_result(&value);
            0
        }
        Err(e) => store_err(&e.to_string()),
    }
}

/// Null-terminated result of the last `ohos_mailcraft_run` (JSON: a value, or
/// `{"error":…}`); a null pointer when nothing has run yet.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ohos_mailcraft_result() -> *const c_char {
    let slot = match RESULT.lock() {
        Ok(s) => s,
        Err(e) => e.into_inner(),
    };
    match slot.as_ref() {
        Some(c) => c.as_ptr(),
        None => std::ptr::null(),
    }
}

/// Give the app's sandbox directory (where the credential store lives). Idempotent: the first
/// non-empty path wins. Returns `0` on a set, `1` when already fixed, `2` for a path that is not
/// UTF-8. When no sandbox is ever given, the platform has no credential store (secrets are not
/// kept between runs), as a system without one.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ohos_mailcraft_set_sandbox(path: *const c_char) -> i32 {
    // The pointer is the caller's (the NAPI layer); reading it and its first byte is the only
    // unsafe, and it is confined to the two reads below.
    let first = unsafe {
        if path.is_null() {
            return 2;
        }
        *path
    };
    if first == 0 {
        return 2;
    }
    let text = match unsafe { CStr::from_ptr(path).to_str() } {
        Ok(s) => s,
        Err(_) => return 2,
    };
    let dir = PathBuf::from(text);
    if SANDBOX.set(dir).is_err() {
        return 1;
    }
    0
}

/// Give the system's locale (`zh_CN`, `en_US`, …). Idempotent: the first non-empty value wins.
/// Returns `0` on a set, `1` when already fixed, `2` for a value that is not UTF-8 or is empty. The
/// render thread reads this via [`locale`] to feed the UI's `Services.system_locale`, so a "auto"
/// language preference (the default) resolves to the system's language.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ohos_mailcraft_set_locale(locale: *const c_char) -> i32 {
    // The pointer is the caller's (the NAPI layer); reading it and its first byte is the only
    // unsafe, confined to the two reads below.
    let first = unsafe {
        if locale.is_null() {
            return 2;
        }
        *locale
    };
    if first == 0 {
        return 2;
    }
    let text = match unsafe { CStr::from_ptr(locale).to_str() } {
        Ok(s) => s,
        Err(_) => return 2,
    };
    if text.trim().is_empty() {
        return 2;
    }
    if LOCALE.set(text.trim().to_string()).is_err() {
        return 1;
    }
    log::info!("mailcraft system locale set: {text}");
    0
}

/// Build (once) the one [`Host`] from the OpenHarmony platform, ready for the async sync jobs to
/// run on. The credential store is the sandbox's `secrets.json` when a sandbox was given, else
/// none. Returns `0` on a build (or when one is already up), `1` when the platform reports no
/// memory to spare. The result JSON, stored as usual, says what the platform offers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ohos_mailcraft_host_ready() -> i32 {
    let mut slot = match HOST.lock() {
        Ok(s) => s,
        Err(e) => e.into_inner(),
    };
    if slot.is_some() {
        store_result(&json!({ "host": "ready", "network": "online", "redirect": "custom-scheme:mailcraft" }));
        return 0;
    }
    let platform = match SANDBOX.get() {
        Some(sandbox) => OhosPlatform::with_sandbox(sandbox.clone()),
        None => OhosPlatform::without_secrets(),
    };
    let platform = std::sync::Arc::new(platform);
    match host_init(platform) {
        Ok(host) => {
            // Read the budget before the Host is moved into the one slot (Host is not `Clone`).
            let budget = host.memory_budget();
            *slot = Some(host);
            drop(slot);
            store_result(&json!({
                "host": "ready",
                "memory_budget": budget,
                "network": "online",
                "redirect": "custom-scheme:mailcraft",
            }));
            0
        }
        Err(e) => {
            store_err(&format!("the platform could not be started: {e}"));
            1
        }
    }
}

// ---- Real IMAP/SMTP "test connection" (the CA task's payoff) --------------------------------
// A "test account" opens the account's incoming IMAP socket over TLS and signs in, then logs out.
// That is the one path that exercises the webpki-roots verifier the CA task wired for OHOS
// (`crates/net/src/tls.rs`): the server's certificate must chain to a root we carry. It blocks on
// the network (the engine's own timeouts apply), so it runs on a background thread; the outcome is
// polled with `ohos_mailcraft_test_account_result`. A password never leaves the bridge: the net
// crate blanks it in the error it hands back, and this scrubs it again as a second line of defence.

/// Read a NUL-terminated C string as an owned `String` (empty for a null / non-UTF-8 pointer).
fn read_cstr(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    match unsafe { CStr::from_ptr(ptr) }.to_str() {
        Ok(s) => s.to_string(),
        Err(_) => String::new(),
    }
}

/// Start a "test connection" for an IMAP/SMTP account on a background thread. `security` is one of
/// "none"/"starttls"/"tls" (parsed by the model; "tls" when blank). Returns 0 = started, 1 = bad
/// input (a null host) or the thread could not start. The account's incoming IMAP server is what is
/// checked; the password is held in the thread and blanked out of every message it produces.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ohos_mailcraft_test_account(
    host: *const c_char,
    port: i32,
    security: *const c_char,
    username: *const c_char,
    password: *const c_char,
) -> i32 {
    let host = read_cstr(host);
    if host.is_empty() {
        return 1;
    }
    let security = Security::parse(&read_cstr(security)).unwrap_or(Security::Tls);
    let username = read_cstr(username);
    let password = read_cstr(password);
    let port = port.max(0) as u16;

    // A password-based IMAP account (no OAuth): the engine's `test_account` opens its incoming
    // server and signs in. `id` is not read, so the default (0) is fine.
    let account = Account {
        id: 0,
        kind: AccountKind::Imap,
        email: username.clone(),
        username,
        incoming: Some(Server { host, port, security }),
        ..Default::default()
    };

    // Mark the result "pending" before the thread runs, so a poll can't read a stale "ok".
    *TEST_RESULT.lock().unwrap() = Some(CString::new("pending").unwrap_or_default());

    let started = std::thread::Builder::new().name("mc-test-account".into()).spawn(move || {
        let outcome = match mailcraft_net::test_account(&account, &password) {
            Ok(()) => "ok: connected and signed in to the IMAP server".to_string(),
            Err(e) => format!("error: {}", scrub_secret(&e.to_string(), &password)),
        };
        *TEST_RESULT.lock().unwrap() = Some(CString::new(outcome).unwrap_or_default());
    });

    if started.is_err() {
        *TEST_RESULT.lock().unwrap() = Some(CString::new("error: could not start the test thread").unwrap_or_default());
        return 1;
    }
    0
}

/// The last "test account"'s outcome: "none" (never run), "pending" (running), "ok: …", or
/// "error: …". The pointer is into the bridge's own slot (stable for as long as nothing re-runs a
/// test), as with `ohos_mailcraft_result`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ohos_mailcraft_test_account_result() -> *const c_char {
    let slot = match TEST_RESULT.lock() {
        Ok(s) => s,
        Err(e) => e.into_inner(),
    };
    match slot.as_ref() {
        Some(c) => c.as_ptr(),
        None => b"none\0".as_ptr() as *const c_char,
    }
}

/// Blank `secret` out of `text` (a password a server echoed back, should that ever happen).
fn scrub_secret(text: &str, secret: &str) -> String {
    if secret.is_empty() || !text.contains(secret) {
        text.to_string()
    } else {
        text.replace(secret, "***")
    }
}

// ============================================================================
// P1c: egui rendering harness entry points (the C ABI the NAPI shim calls)
// ============================================================================

/// Build the egui Context + start the render thread. Idempotent: the first call wins.
/// `scale` is the device pixel ratio. `notify` is the Rust -> ArkTS callback (may be null).
/// Returns `0` on success, `1` if already initialised.
///
/// # Safety
/// `notify` is a caller-supplied, thread-safe function pointer; the caller guarantees its
/// lifetime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ohos_mailcraft_egui_init(scale: f32, _notify: Option<Notify>) -> i32 {
    ensure_glog();
    static EGUI_READY: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    let already = EGUI_READY.get();
    if already.is_some() {
        return 1;
    }
    EGUI_READY.set(());

    if scale > 0.0 {
        // Stash the scale for the render thread's input translation.
        SCALE.store((scale * 1000.0) as i32, Ordering::Relaxed);
    }

    // egui 0.37 builds a Context via `Default` (not `new()`).
    let ctx = egui::Context::default();
    let (tx, rx) = mpsc::channel::<Message>();
    if SEND.set(tx).is_err() {
        return 1;
    }
    std::thread::Builder::new()
        .name("mc-render".into())
        .spawn(move || render::run(rx, ctx))
        .ok();
    log::info!("mailcraft egui initialised (scale={scale})");
    0
}

/// The stashed device pixel ratio (milli-fixed: scale * 1000), read by the input layer.
static SCALE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
#[allow(dead_code)]
fn current_scale() -> f32 {
    SCALE.load(Ordering::Relaxed) as f32 / 1000.0
}

/// Whether this device is a *phone*, reported by ArkTS from `@ohos.deviceInfo.deviceType`
/// (== 'phone') before `ohos_mailcraft_egui_init`. Only a phone defaults to the phone interface;
/// a tablet / 2-in-1 PC keeps MailCraft's original desktop layout. Defaults to `false` (desktop)
/// when ArkTS never reports it, so a missing probe degrades to the desktop UI, not the phone one.
static IS_PHONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[allow(dead_code)]
fn is_phone() -> bool {
    IS_PHONE.load(Ordering::Relaxed)
}

/// Tell the bridge whether the device is a phone. ArkTS calls this **before**
/// `ohos_mailcraft_egui_init`; the render thread reads it (via [`is_phone`]) to set the UI's
/// `phone_layout` so a phone shows the Outlook-for-Android style interface and a tablet / 2-in-1
/// PC shows the desktop interface.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ohos_mailcraft_egui_set_phone(is_phone: bool) {
    IS_PHONE.store(is_phone, Ordering::Relaxed);
    log::info!("mailcraft device is_phone={is_phone}");
}

/// Tell the render thread the XComponent surface appeared, on a given `window` handle.
///
/// # Safety
/// `window` is an OHNativeWindow handle owned by the NAPI layer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ohos_mailcraft_egui_attach(window: usize, width: u32, height: u32) {
    send(Message::Attach(window, width, height));
}

/// Tell the render thread the surface changed size.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ohos_mailcraft_egui_resize(width: u32, height: u32) {
    send(Message::Resize(width, height));
}

/// Tell the render thread the surface went away.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ohos_mailcraft_egui_detach() {
    send(Message::Detach);
}

/// Post an input event (a JSON blob the NAPI layer produced from an XComponent event).
///
/// # Safety
/// `ptr`/`len` point at caller-owned, readable UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ohos_mailcraft_egui_input(ptr: *const u8, len: usize) {
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    if let Ok(s) = std::str::from_utf8(bytes) {
        if let Ok(input) = serde_json::from_str::<input::Input>(s) {
            send(Message::Input(input));
        } else {
            log::warn!("mailcraft egui_input: bad JSON");
        }
    }
}

/// Request the render thread to draw one frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ohos_mailcraft_egui_frame() {
    send(Message::Frame);
}

/// Foreground / background toggle for the render thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ohos_mailcraft_egui_active(active: bool) {
    send(Message::Active(active));
}

/// Self-test hook: a synthetic click at egui *point* coordinates (not device pixels). Feeds a
/// press+release so the render thread's next frame registers a click — proving "click → select →
/// repaint" end-to-end without depending on the XComponent → device-pixel routing (which is noisy
/// on a multi-window HarmonyOS PC). `x`/`y` are egui points, the same space the UI lays out in.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ohos_mailcraft_egui_tap(x: f32, y: f32) {
    send(Message::Input(input::Input::Tap { x, y }));
}

/// Programmatic-command hook: run an engine command (`id` + JSON `params`) on the UI's `MailApp`
/// session on the render thread. This is how a tap or a menu press is turned into the same action
/// the CLI / control channel run (e.g. `mail.select` / `mail.first` / `view.mail`), so the session
/// mutates and the next frame's reading pane reflects it — proving "action → session → repaint"
/// without depending on pixel coordinates. Both pointers are null-terminated C strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ohos_mailcraft_egui_command(id: *const c_char, params: *const c_char) {
    let id = read_cstr(id);
    let params = read_cstr(params);
    log::info!("mailcraft egui_command {id} ({params})");
    send(Message::Command { id, params });
}
