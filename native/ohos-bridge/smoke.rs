// SPDX-FileCopyrightText: MailCraft contributors (ohos port)
// SPDX-License-Identifier: MIT OR Apache-2.0
// Host smoke test: drives the bridge's C ABI entry points and runs a few real
// commands against the sample mailbox, proving `Session::run` produces output.
// Built for the host (not OHOS); the OHOS build only ships the cdylib/staticlib.

use mailcraft_ohos_bridge::{ohos_mailcraft_init, ohos_mailcraft_result, ohos_mailcraft_run};

fn main() {
    unsafe {
        let rc = ohos_mailcraft_init();
        println!("[init] rc={rc}");

        for cmd in ["folder.list", "view.search", "account.list"] {
            let cmd_c = std::ffi::CString::new(cmd).expect("cmd");
            let params_c = std::ffi::CString::new("{}").expect("params");
            let rc = ohos_mailcraft_run(cmd_c.as_ptr(), params_c.as_ptr());
            let result = ohos_mailcraft_result();
            let text = if result.is_null() {
                "<null>".to_string()
            } else {
                std::ffi::CStr::from_ptr(result).to_string_lossy().into_owned()
            };
            let snippet: String = text.chars().take(220).collect();
            println!("[run] {cmd:<16} rc={rc}\n      {snippet}");
        }

        // A deliberately bad command id, to prove errors are surfaced as JSON, not a panic.
        let bad = std::ffi::CString::new("does.not.exist").expect("bad");
        let empty = std::ffi::CString::new("").expect("empty");
        let rc = ohos_mailcraft_run(bad.as_ptr(), empty.as_ptr());
        let result = ohos_mailcraft_result();
        let text = std::ffi::CStr::from_ptr(result).to_string_lossy().into_owned();
        println!("[bad-id] rc={rc} -> {text}");
    }
}
