// napi bindings for the MailCraft OHOS bridge. The native module (libentry.so, module name "entry")
// exposes ArkTS-callable functions that drive the Rust core through the C ABI bridge.
//
// runCommand drives a command by id (the same registry the CLI / MCP use) and returns its JSON.
// setSandbox gives the platform the app's credential-store home; hostReady builds the one Host
// the async sync jobs run on.
// testAccount opens a real IMAP/SMTP connection (over TLS, using the webpki-roots verifier
// built for OHOS) on a background thread and starts it; testAccountResult reads its outcome
// ("pending" / "ok: …" / "error: …") later.
export const runCommand: (cmd: string, params: string) => string;
export const setSandbox: (path: string) => string;
// Hand the system's locale (e.g. "zh_CN") to the bridge so a "auto" language preference (the
// default) resolves to the system's language — Chinese on a Chinese system, English otherwise.
export const setLocale: (locale: string) => string;
export const hostReady: () => string;
export const testAccount: (host: string, port: string, security: string, username: string, password: string) => string;
export const testAccountResult: () => string;

// egui render bridge (P1c): the render worker paints the original MailCraft UI onto an
// XComponent SURFACE. eguiInit builds the egui Context + starts the render thread (pass a
// device pixel ratio and a Rust->ArkTS notify callback); eguiFrame requests one painted frame
// (pumped by a timer in the ETS layer); eguiActive toggles foreground/background.
export const eguiInit: (scale: number, notify: (message: string) => void) => void;
export const eguiInput: (json: string) => void;
export const eguiFrame: () => void;
export const eguiActive: (active: boolean) => void;
// Self-test hook: a synthetic click at egui *point* coordinates (not device pixels). Used to
// prove click → select → repaint without the XComponent → device-pixel routing noise on a
// multi-window HarmonyOS PC.
export const eguiTap: (x: number, y: number) => void;
// Programmatic-command hook: run an engine command (id + JSON params) on the UI's MailApp
// session. Turns a tap / menu press into the same action the engine runs (mail.select /
// mail.first / view.mail), so the session mutates and the next frame's reading pane reflects
// it — without depending on pixel coordinates.
export const eguiCommand: (id: string, params: string) => void;
// Report whether this device is a phone (from `@ohos.deviceInfo.deviceType`). Call BEFORE
// eguiInit so the render thread picks the right layout: a phone shows the phone interface, a
// tablet / 2-in-1 PC keeps MailCraft's desktop interface.
export const eguiSetPhone: (isPhone: boolean) => void;
