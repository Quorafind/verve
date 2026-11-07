//! The proxy the machine is set to (`setProxy({ mode: 'system' })`), read and never changed:
//! the current user's Internet Options on Windows, the `http_proxy`, `https_proxy` and
//! `no_proxy` variables elsewhere.

/// The machine's `proxyRules` and `proxyBypassRules`; empty when it has none.
pub fn read() -> (String, String) {
    imp::read()
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::Networking::WinHttp::{
        WINHTTP_CURRENT_USER_IE_PROXY_CONFIG, WinHttpGetIEProxyConfigForCurrentUser,
    };

    /// The text of a wide string the API allocated, which is freed.
    ///
    /// # Safety
    ///
    /// `text` is null or a NUL-terminated string from `GlobalAlloc`.
    unsafe fn take(text: *mut u16) -> String {
        if text.is_null() {
            return String::new();
        }
        // SAFETY: the caller promises a NUL-terminated string; it is copied before it is freed.
        unsafe {
            let length = (0..).take_while(|&i| *text.add(i) != 0).count();
            let copy = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
            GlobalFree(text.cast());
            copy
        }
    }

    pub fn read() -> (String, String) {
        let mut config = WINHTTP_CURRENT_USER_IE_PROXY_CONFIG {
            fAutoDetect: 0,
            lpszAutoConfigUrl: std::ptr::null_mut(),
            lpszProxy: std::ptr::null_mut(),
            lpszProxyBypass: std::ptr::null_mut(),
        };
        // SAFETY: `config` is the structure the call fills; each string in it is the caller's
        // to free once, which `take` does.
        unsafe {
            if WinHttpGetIEProxyConfigForCurrentUser(&mut config) == 0 {
                return (String::new(), String::new());
            }
            drop(take(config.lpszAutoConfigUrl));
            (take(config.lpszProxy), take(config.lpszProxyBypass))
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn read() -> (String, String) {
        let var = |name: &str| {
            std::env::var(name)
                .or_else(|_| std::env::var(name.to_uppercase()))
                .unwrap_or_default()
        };
        let (http, https) = (var("http_proxy"), var("https_proxy"));
        let rules = match (http.is_empty(), https.is_empty()) {
            (true, true) => String::new(),
            (false, true) => http,
            (true, false) => format!("https={https}"),
            (false, false) => format!("http={http};https={https}"),
        };
        (rules, var("no_proxy"))
    }
}
