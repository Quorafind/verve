//! What the main process asks Windows for without a window: the cursor, the user's idle time and
//! power, the theme's flags, the system colours and animation settings.

use std::mem::size_of;

use windows_sys::Win32::Foundation::{LocalFree, POINT};
use windows_sys::Win32::Graphics::Gdi::GetSysColor;
use windows_sys::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CryptProtectData, CryptUnprotectData,
};
use windows_sys::Win32::System::Diagnostics::Debug::MessageBeep;
use windows_sys::Win32::System::Power::{
    ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, GetSystemPowerStatus,
    SYSTEM_POWER_STATUS, SetThreadExecutionState,
};
use windows_sys::Win32::System::SystemInformation::GetTickCount64;
use windows_sys::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows_sys::Win32::UI::Shell::{
    FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, SHFILEOPSTRUCTW,
    SHFileOperationW, ShellExecuteW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, SPI_GETCLIENTAREAANIMATION, SPI_GETHIGHCONTRAST, SW_SHOWNORMAL,
    SystemParametersInfoW,
};

use super::{Block, SystemColor};
use crate::api::app::platform::{Root, read_dword, wide};

const PERSONALIZE: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
const DWM: &str = r"Software\Microsoft\Windows\DWM";

/// The cursor in screen pixels.
pub(crate) fn cursor_position() -> (i32, i32) {
    let mut point = POINT { x: 0, y: 0 };
    // SAFETY: `point` is the POINT the call fills.
    unsafe { GetCursorPos(&mut point) };
    (point.x, point.y)
}

/// Whether the user turned on a high contrast theme.
pub(crate) fn high_contrast() -> bool {
    let mut info = HIGHCONTRASTW {
        cbSize: size_of::<HIGHCONTRASTW>() as u32,
        dwFlags: 0,
        lpszDefaultScheme: std::ptr::null_mut(),
    };
    // SAFETY: `info` is the HIGHCONTRASTW the query fills, `cbSize` says how big it is.
    let asked = unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            info.cbSize,
            (&mut info as *mut HIGHCONTRASTW).cast(),
            0,
        )
    };
    asked != 0 && info.dwFlags & HCF_HIGHCONTRASTON != 0
}

/// Whether "transparency effects" is off in the personalisation settings.
pub(crate) fn reduced_transparency() -> bool {
    read_dword(Root::CurrentUser, PERSONALIZE, "EnableTransparency") == Some(0)
}

/// Whether the system draws its windows' animations (and so scrolls smoothly).
pub(crate) fn animations_enabled() -> bool {
    let mut on = 1i32;
    // SAFETY: the query fills a BOOL.
    let asked = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            (&mut on as *mut i32).cast(),
            0,
        )
    };
    asked == 0 || on != 0
}

/// Seconds since the user last touched the keyboard or the mouse.
pub(crate) fn idle_seconds() -> u64 {
    let mut info = LASTINPUTINFO {
        cbSize: size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    // SAFETY: `info` is the LASTINPUTINFO the call fills.
    if unsafe { GetLastInputInfo(&mut info) } == 0 {
        return 0;
    }
    // SAFETY: no arguments.
    let now = unsafe { GetTickCount64() } as u32;
    u64::from(now.wrapping_sub(info.dwTime)) / 1000
}

/// Whether the machine runs on its battery.
pub(crate) fn on_battery() -> bool {
    // SAFETY: an all-zero SYSTEM_POWER_STATUS is a valid value for the call to fill.
    let mut status: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    // SAFETY: `status` is the struct the call fills.
    unsafe { GetSystemPowerStatus(&mut status) != 0 && status.ACLineStatus == 0 }
}

/// Keeps the system, or the display too, awake for as long as any of `blocks` is in force: the
/// execution state belongs to the calling thread, which is always the main process's.
pub(crate) fn keep_awake(blocks: &[Block]) {
    let mut state = ES_CONTINUOUS;
    for block in blocks {
        state |= match block {
            Block::AppSuspension => ES_SYSTEM_REQUIRED,
            Block::DisplaySleep => ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED,
        };
    }
    // SAFETY: the call takes flags.
    unsafe { SetThreadExecutionState(state) };
}

/// The accent colour as `RRGGBBAA`, the blue Windows ships with when the user chose none.
pub(crate) fn accent_color() -> String {
    let abgr = read_dword(Root::CurrentUser, DWM, "AccentColor").unwrap_or(0xFFD7_7800);
    let [r, g, b] = [abgr & 0xFF, (abgr >> 8) & 0xFF, (abgr >> 16) & 0xFF];
    format!("{r:02X}{g:02X}{b:02X}FF")
}

/// A system colour as `#RRGGBBAA`.
pub(crate) fn system_color(color: SystemColor) -> String {
    // SAFETY: the index is one of the COLOR_ constants.
    let bgr = unsafe { GetSysColor(color_index(color)) };
    format!(
        "#{:02X}{:02X}{:02X}FF",
        bgr & 0xFF,
        (bgr >> 8) & 0xFF,
        (bgr >> 16) & 0xFF
    )
}

/// `shell.beep()`.
pub(crate) fn beep() {
    // SAFETY: `MB_OK` is the default sound.
    unsafe { MessageBeep(0) };
}

/// Wraps `data` for the current Windows user (DPAPI), as Chromium's `OSCrypt` does its key.
pub(crate) fn dpapi_protect(data: &[u8]) -> Option<Vec<u8>> {
    dpapi(data, true)
}

/// The key `dpapi_protect` wrapped, for the same user; `None` for anyone else's.
pub(crate) fn dpapi_unprotect(data: &[u8]) -> Option<Vec<u8>> {
    dpapi(data, false)
}

fn dpapi(data: &[u8], protect: bool) -> Option<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(data.len()).ok()?,
        pbData: data.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let none = std::ptr::null();
    // SAFETY: `input` borrows `data` for the call; the output blob is allocated by the system and
    // freed below once copied.
    let done = unsafe {
        if protect {
            CryptProtectData(
                &input,
                std::ptr::null(),
                none,
                none.cast(),
                none.cast(),
                0,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                none,
                none.cast(),
                none.cast(),
                0,
                &mut output,
            )
        }
    };
    if done == 0 || output.pbData.is_null() {
        return None;
    }
    // SAFETY: the system filled `output` with `cbData` bytes at `pbData`, which `LocalFree` owns.
    unsafe {
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData.cast());
        Some(bytes)
    }
}

/// Has the shell open `target` (an address, a file, a folder) with what the user chose for it.
pub(crate) fn open_with_shell(target: &str) -> bool {
    shell_execute("open", target, None)
}

/// Opens Explorer with the file `path` selected.
pub(crate) fn show_in_folder(path: &str) -> bool {
    shell_execute("open", "explorer.exe", Some(&format!("/select,\"{path}\"")))
}

fn shell_execute(verb: &str, file: &str, parameters: Option<&str>) -> bool {
    let (verb, file) = (wide(verb), wide(file));
    let parameters = parameters.map(wide);
    let parameters = parameters.as_ref().map_or(std::ptr::null(), |p| p.as_ptr());
    // SAFETY: every string is NUL-terminated and outlives the call.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            parameters,
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    result as usize > 32
}

/// Moves the file or folder at `path` to the recycle bin, without asking.
pub(crate) fn move_to_trash(path: &str) -> bool {
    // The list of paths ends with two NULs.
    let mut from = wide(path);
    from.push(0);
    let mut operation = SHFILEOPSTRUCTW {
        hwnd: std::ptr::null_mut(),
        wFunc: FO_DELETE,
        pFrom: from.as_ptr(),
        pTo: std::ptr::null(),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT) as u16,
        fAnyOperationsAborted: 0,
        hNameMappings: std::ptr::null_mut(),
        lpszProgressTitle: std::ptr::null(),
    };
    // SAFETY: `operation` points at `from`, which is double-NUL-terminated and outlives the call.
    unsafe { SHFileOperationW(&mut operation) == 0 && operation.fAnyOperationsAborted == 0 }
}

/// The `COLOR_*` index of a system colour.
fn color_index(color: SystemColor) -> i32 {
    match color.name() {
        "3d-dark-shadow" => 21,
        "3d-face" => 15,
        "3d-highlight" => 20,
        "3d-light" => 22,
        "3d-shadow" => 16,
        "active-border" => 10,
        "active-caption" => 2,
        "active-caption-gradient" => 27,
        "app-workspace" => 12,
        "button-text" => 18,
        "caption-text" => 9,
        "desktop" => 1,
        "disabled-text" => 17,
        "highlight" => 13,
        "highlight-text" => 14,
        "hotlight" => 26,
        "inactive-border" => 11,
        "inactive-caption" => 3,
        "inactive-caption-gradient" => 28,
        "inactive-caption-text" => 19,
        "info-background" => 24,
        "info-text" => 23,
        "menu" => 4,
        "menu-highlight" => 29,
        "menu-bar" => 30,
        "menu-text" => 7,
        "scrollbar" => 0,
        "window" => 5,
        "window-frame" => 6,
        "window-text" => 8,
        _ => 0,
    }
}

/// A message box of the system's own, over no window; it returns when the person has read it.
pub(crate) fn error_box(title: &str, content: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    let (title, content) = (wide(title), wide(content));
    // SAFETY: `wide` ends both texts with a NUL and they outlive the call; a null owner is allowed.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            content.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}
