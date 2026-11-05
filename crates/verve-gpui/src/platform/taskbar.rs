//! The progress bar on a window's taskbar button: `ITaskbarList3`, called through its vtable
//! because `windows-sys` has no COM interfaces.

use std::cell::Cell;
use std::ffi::c_void;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
};
use windows_sys::core::GUID;

const CLSID_TASKBAR_LIST: GUID = GUID::from_u128(0x56fdf344_fd6d_11d0_958a_006097c9a090);
const IID_TASKBAR_LIST3: GUID = GUID::from_u128(0xea1afb91_9e28_4b86_90e9_9e9f8a5eefaf);

const NO_PROGRESS: i32 = 0;
const INDETERMINATE: i32 = 1;
const NORMAL: i32 = 2;

/// `ITaskbarList3`'s vtable as far as the progress calls: `IUnknown`, then `ITaskbarList`'s five
/// methods, `ITaskbarList2`'s one, and `ITaskbarList3`'s first two.
#[repr(C)]
struct Vtable {
    _unknown: [usize; 2],
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    hr_init: unsafe extern "system" fn(*mut c_void) -> i32,
    _list: [usize; 4],
    _list2: usize,
    set_progress_value: unsafe extern "system" fn(*mut c_void, HWND, u64, u64) -> i32,
    set_progress_state: unsafe extern "system" fn(*mut c_void, HWND, i32) -> i32,
}

thread_local! {
    /// The taskbar object of this thread, made once and kept as long as the process runs. Null
    /// until it is made, and after a try that failed: that is not tried again.
    static LIST: Cell<*mut c_void> = const { Cell::new(std::ptr::null_mut()) };
    static TRIED: Cell<bool> = const { Cell::new(false) };
}

/// A call of the taskbar object's, which `list` made and keeps alive for the life of the thread
/// (it is never released once it works), or a pointer `list` has just made from the system's
/// answer, with the window of this thread it is asked about.
macro_rules! com {
    ($call:expr) => {
        // SAFETY: as the macro's documentation says.
        unsafe { $call }
    };
}

fn list() -> Option<*mut c_void> {
    if TRIED.replace(true) {
        return Some(LIST.get()).filter(|list| !list.is_null());
    }
    let mut made: *mut c_void = std::ptr::null_mut();
    // GPUI has made this thread's apartment (a single-threaded one, which asking again answers
    // with `S_FALSE`); a thread that is in another mode would answer an error, and the object is
    // made all the same.
    com!(CoInitializeEx(
        std::ptr::null(),
        COINIT_APARTMENTTHREADED as u32
    ));
    let hr = com!(CoCreateInstance(
        &CLSID_TASKBAR_LIST,
        std::ptr::null_mut(),
        CLSCTX_INPROC_SERVER,
        &IID_TASKBAR_LIST3,
        &mut made,
    ));
    if hr < 0 || made.is_null() {
        return None;
    }
    let vtable = com!(&**(made as *const *const Vtable));
    if com!((vtable.hr_init)(made)) < 0 {
        com!((vtable.release)(made));
        return None;
    }
    LIST.set(made);
    Some(made)
}

/// Shows `progress` on the window's taskbar button: `-1` takes the bar away, 0 to 1 fills it,
/// above 1 is Electron's "indeterminate".
pub(super) fn progress(hwnd: HWND, progress: f64) {
    let Some(list) = list() else {
        return;
    };
    let vtable = com!(&**(list as *const *const Vtable));
    let state = match progress {
        p if p < 0. => NO_PROGRESS,
        p if p > 1. => INDETERMINATE,
        _ => NORMAL,
    };
    com!((vtable.set_progress_state)(list, hwnd, state));
    if state == NORMAL {
        com!((vtable.set_progress_value)(
            list,
            hwnd,
            (progress * 1000.).round() as u64,
            1000
        ));
    }
}
