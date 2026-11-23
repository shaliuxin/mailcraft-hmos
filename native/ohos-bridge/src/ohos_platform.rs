// SPDX-FileCopyrightText: MailCraft contributors (ohos port)
// SPDX-License-Identifier: MIT OR Apache-2.0
//! The OpenHarmony platform, for the engine's [`Platform`](mailcraft_host::Platform) seam.
//!
//! The trait's defaults already do the right thing for two of the hardest entries on this
//! target, so this implementation overrides only what a phone must:
//!
//! - **The clock** (`now`, `utc_offset_minutes`) and **the socket** (`connect`) are left to the
//!   trait's defaults: `mailcraft_time` reads the system clock, and `connect` drives `std::net`
//!   (plain TCP, which the OHOS target has). Neither needs an override.
//! - **The credential store** is a file in the app's sandbox (`FileSecretStore`), set with
//!   [`OhosPlatform::with_sandbox`]. A phone has no Keyring/Secret Service; the sandbox file is
//!   the pragmatic stand-in. It is a first pass — a production build should hand the store to the
//!   system's own credential manager (`@ohos.useriam`); the trait is the seam that makes that a
//!   drop-in replacement.
//! - **The network** is reported `Online` (a phone that is connected, on a metered or saving
//!   link unknown here): the caller then tries, as always.
//! - **OAuth** comes back on a custom scheme (`Redirect::CustomScheme`) the system routes to the
//!   app — the phone's way of a sign-in round trip, where the desktop uses a loopback listener.
//! - Everything else (notifications, opening a file or a link, the sign-in page, scheduled
//!   reminders) is a no-op for now: the trait's `Unsupported` default, to be wired to the
//!   HarmonyOS notification / intent / webview APIs as the UI grows.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use mailcraft_host::{
    NetworkState, Platform, Redirect, SecretError, SecretStore,
};

use serde_json::Map;

/// The OpenHarmony platform: the sandbox-backed credential store, an online network, and a
/// custom-scheme OAuth redirect; everything else on the default (a no-op).
pub struct OhosPlatform {
    /// The app's sandbox directory, when the caller gave one (then the credential store is a file
    /// inside it); `None` means no store, as a system without one.
    sandbox: Option<PathBuf>,
    /// The one store, built lazily the first time it is asked for and held for the process.
    store: OnceLock<Arc<FileSecretStore>>,
}

impl OhosPlatform {
    /// A platform with no credential store (a system without one): secrets are not kept between
    /// runs. The network and OAuth still work.
    pub fn without_secrets() -> Self {
        Self { sandbox: None, store: OnceLock::new() }
    }

    /// A platform whose credential store is `secrets.json` inside `sandbox`.
    pub fn with_sandbox(sandbox: impl Into<PathBuf>) -> Self {
        Self { sandbox: Some(sandbox.into()), store: OnceLock::new() }
    }
}

impl Platform for OhosPlatform {
    fn secrets(&self) -> Option<Arc<dyn SecretStore>> {
        let sandbox = self.sandbox.as_ref()?;
        Some(self.store.get_or_init(|| {
            Arc::new(FileSecretStore {
                path: sandbox.join("secrets.json"),
                guard: Mutex::new(()),
            })
        }).clone())
    }

    fn network(&self) -> NetworkState {
        // A phone that is up, on a link we don't know the metering of: try, as always.
        NetworkState::Online { metered: false, data_saving: false }
    }

    fn redirect(&self) -> Redirect {
        // The sign-in page hands the owner back through the app's own scheme, which the system
        // routes to it. The desktop instead listens on loopback.
        Redirect::CustomScheme("mailcraft".to_string())
    }

    // `now`, `utc_offset_minutes`, `connect` and `memory_budget` use the trait's defaults: the
    // system clock and `std::net` both work on this target, and no device budget is reported.
    // `notify`, `schedule_reminder`, `open_file`, `open_link` and `open_sign_in_page` use the
    // default `Unsupported` no-op, pending the HarmonyOS notification / intent / webview wiring.
}

/// A credential store that keeps a flat `{"<key>": "<secret>"}` JSON map in one file.
///
/// It is not a secure store — a production build should use the system's credential manager — but
/// it is the seam: `set` writes through a temp file that is renamed into place (an atomic swap, so
/// a crash mid-write never leaves a half-written file), and a `Mutex` serializes the read-modify-
/// writes across the host's worker threads.
pub struct FileSecretStore {
    path: PathBuf,
    /// The one lock over the read-modify-write; held only for the duration of a store call.
    guard: Mutex<()>,
}

impl FileSecretStore {
    /// Load the current map (an empty one when the file is absent or unreadable).
    fn load(&self) -> Map<String, serde_json::Value> {
        let bytes = match std::fs::read(&self.path) {
            Ok(b) => b,
            Err(_) => return Map::new(),
        };
        serde_json::from_slice::<Map<String, serde_json::Value>>(&bytes).unwrap_or_default()
    }

    /// Write `map` atomically: to a sibling temp file, then renamed over the target.
    fn write_map(&self, map: &Map<String, serde_json::Value>) -> Result<(), SecretError> {
        let text = match serde_json::to_string(map) {
            Ok(t) => t,
            Err(_) => return Err(SecretError::Refused),
        };
        // Write through a sibling temp so a crash never leaves a half-written file, then rename.
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, &text).map_err(|_| SecretError::Refused)?;
        std::fs::rename(&tmp, &self.path).map_err(|_| SecretError::Refused)
    }
}

impl SecretStore for FileSecretStore {
    fn get(&self, key: &str) -> Option<String> {
        let _lock = self.guard.lock().ok()?;
        let map = self.load();
        map.get(key).and_then(|v| v.as_str()).map(str::to_string)
    }

    fn set(&self, key: &str, secret: &str) -> Result<(), SecretError> {
        let _lock = self.guard.lock().unwrap_or_else(|e| e.into_inner());
        let mut map = self.load();
        map.insert(key.to_string(), serde_json::Value::String(secret.to_string()));
        self.write_map(&map)
    }

    fn delete(&self, key: &str) {
        let _lock = self.guard.lock().unwrap_or_else(|e| e.into_inner());
        let mut map = self.load();
        if map.remove(key).is_some() {
            let _ = self.write_map(&map);
        }
    }

    /// `get`, but says when the store itself could not be read (a store with a problem, not an
    /// empty one): `Refused` when the file exists but could not be parsed or written.
    fn read(&self, key: &str) -> Result<Option<String>, SecretError> {
        let _lock = self.guard.lock().unwrap_or_else(|e| e.into_inner());
        let bytes = match std::fs::read(&self.path) {
            Ok(b) => b,
            Err(_) => return Ok(None), // no file is no store, not a trouble
        };
        let map = match serde_json::from_slice::<Map<String, serde_json::Value>>(&bytes) {
            Ok(m) => m,
            Err(_) => return Err(SecretError::Refused),
        };
        Ok(map.get(key).and_then(|v| v.as_str()).map(str::to_string))
    }

    fn remove(&self, key: &str) -> Result<(), SecretError> {
        let _lock = self.guard.lock().unwrap_or_else(|e| e.into_inner());
        let mut map = self.load();
        let had = map.remove(key).is_some();
        if had {
            self.write_map(&map)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn tmp_store() -> FileSecretStore {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("mc-secrets-{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        FileSecretStore { path: dir.join("secrets.json"), guard: Mutex::new(()) }
    }

    #[test]
    fn set_then_get_round_trips() {
        let s = tmp_store();
        s.set("password:a@example.com", "s3cret").unwrap();
        assert_eq!(s.get("password:a@example.com").as_deref(), Some("s3cret"));
        assert_eq!(s.get("missing"), None);
    }

    #[test]
    fn delete_then_get_is_none() {
        let s = tmp_store();
        s.set("refresh:a@example.com", "tok").unwrap();
        s.delete("refresh:a@example.com");
        assert_eq!(s.get("refresh:a@example.com"), None);
        assert!(s.remove("refresh:a@example.com").is_ok()); // removing an absent key is still ok
    }

    #[test]
    fn store_persists_across_handles() {
        let dir = std::env::temp_dir().join(format!("mc-persist-{}", COUNTER.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("secrets.json");
        {
            let a = FileSecretStore { path: path.clone(), guard: Mutex::new(()) };
            a.set("password:a@example.com", "kept").unwrap();
        }
        // A fresh handle over the same file reads what the first kept.
        let b = FileSecretStore { path, guard: Mutex::new(()) };
        assert_eq!(b.get("password:a@example.com").as_deref(), Some("kept"));
    }
}
