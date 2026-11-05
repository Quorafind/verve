//! The GPUI shell on a real display, held to the contract every shell keeps
//! (`crates/verve-electron/tests/support/contract.rs`, which the headless shell runs too), and
//! the options a window is made with, read back from its `HWND`.
//!
//! These open real windows, on the screen of whoever runs them: a few seconds of an 800 x 600
//! window that is shown, hidden, maximized, minimized, put on full screen and closed. They are
//! `#[ignore]`d for that reason; run them with
//! `cargo test -p verve-gpui --test shell_windows -- --ignored --test-threads=1`.
#![cfg(windows)]

#[path = "../../verve-electron/tests/support/contract.rs"]
mod contract;

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::time::Duration;

use contract::{Rig, watch};
use verve_electron::msg::{Rect, ToShell, WindowEvent, WindowOp, WindowOptions};
use verve_electron::{ElectronOptions, Hub};
use verve_gpui::{AppRegion, Control};
use windows_sys::Win32::Foundation::{HWND, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GW_OWNER, GWL_EXSTYLE, GWL_STYLE, GetLayeredWindowAttributes, GetWindow, GetWindowLongPtrW,
    GetWindowRect, GetWindowTextW, HTCAPTION, HTCLIENT, MA_NOACTIVATE, MINMAXINFO, SPI_GETWORKAREA,
    SendMessageW, SystemParametersInfoW, WM_CLOSE, WM_GETMINMAXINFO, WM_MOUSEACTIVATE,
    WM_MOUSEMOVE, WM_NCHITTEST, WS_CAPTION, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_MAXIMIZEBOX,
};

/// How long the whole run may take before the test ends the process, windows and all.
const LIMIT: Duration = Duration::from_secs(120);

/// Runs `scenario` against the GPUI shell, on a hub with no main process: the scenario is it.
/// GPUI's application runs once in a process, so a test file runs one scenario at a time.
fn on_the_gpui_shell(scenario: impl FnOnce(&Rig, &Control) + Send + 'static) {
    std::thread::spawn(|| {
        std::thread::sleep(LIMIT);
        eprintln!("the GPUI shell did not finish in {LIMIT:?}; ending the process");
        std::process::exit(3);
    });
    let options = ElectronOptions {
        app_path: std::env::temp_dir(),
        dark: false,
    };
    let hub: Arc<Hub> = Hub::new(options);
    let said = watch(&hub);
    let code = verve_gpui::run_with(hub, move |hub, control| {
        let hub = Arc::clone(hub);
        std::thread::spawn(move || {
            let rig = Rig::new(&hub, &said, true);
            let outcome = catch_unwind(AssertUnwindSafe(|| scenario(&rig, &control)));
            if let Err(panic) = &outcome {
                eprintln!("{panic:?}");
            }
            hub.shutdown(if outcome.is_ok() { 0 } else { 7 });
        });
    });
    assert_eq!(code, 0, "the scenario failed (see standard error)");
}

/// Makes window `id` with `options` and answers its `HWND`.
fn open(rig: &Rig, id: u32, options: WindowOptions) -> HWND {
    rig.hub.to_shell(ToShell::CreateWindow {
        window: id,
        contents: id,
        options,
    });
    loop {
        let quiet = Duration::from_millis(100);
        let events = rig.said.settle(quiet, Duration::from_secs(10));
        let handle = events.iter().find_map(|(window, event)| match event {
            WindowEvent::NativeHandle(handle) if *window == id => Some(*handle),
            _ => None,
        });
        if let Some(handle) = handle {
            return handle as HWND;
        }
    }
}

fn long(hwnd: HWND, index: i32) -> u32 {
    // SAFETY: `hwnd` is a window of the shell under test, alive until the scenario destroys it.
    unsafe { GetWindowLongPtrW(hwnd, index) as u32 }
}

/// Sends `op` to window `window` and waits for the shell to be done with it.
fn op(rig: &Rig, window: u32, op: WindowOp) {
    rig.hub.to_shell(ToShell::Window { window, op });
    rig.said
        .settle(Duration::from_millis(150), Duration::from_secs(5));
}

fn hidden() -> WindowOptions {
    WindowOptions {
        show: false,
        ..WindowOptions::default()
    }
}

/// One scenario of the real-display test.
fn the_options_of_a_window_reach_its_hwnd(rig: &Rig, _: &Control) {
    let framed = open(rig, 11, hidden());
    assert_ne!(
        long(framed, GWL_STYLE) & WS_CAPTION,
        0,
        "frame: true has a caption"
    );
    let bare = WindowOptions {
        frame: false,
        maximizable: false,
        ..hidden()
    };
    let bare = open(rig, 12, bare);
    assert_eq!(
        long(bare, GWL_STYLE) & WS_CAPTION,
        0,
        "frame: false has none"
    );
    assert_eq!(
        long(bare, GWL_STYLE) & WS_MAXIMIZEBOX,
        0,
        "maximizable: false"
    );
    let tool = WindowOptions {
        skip_taskbar: true,
        focusable: false,
        always_on_top: true,
        ..hidden()
    };
    let tool = open(rig, 13, tool);
    let ex = long(tool, GWL_EXSTYLE);
    assert_ne!(ex & WS_EX_TOOLWINDOW, 0, "skipTaskbar");
    assert_ne!(ex & WS_EX_NOACTIVATE, 0, "focusable: false");
    assert_ne!(ex & WS_EX_TOPMOST, 0, "alwaysOnTop");
    op(rig, 13, WindowOp::SetAlwaysOnTop(false));
    assert_eq!(
        long(tool, GWL_EXSTYLE) & WS_EX_TOPMOST,
        0,
        "setAlwaysOnTop(false)"
    );
    op(rig, 11, WindowOp::SetOpacity(0.5));
    let (mut alpha, mut flags) = (0u8, 0u32);
    // SAFETY: `framed` is alive and the out pointers are locals.
    unsafe { GetLayeredWindowAttributes(framed, std::ptr::null_mut(), &mut alpha, &mut flags) };
    assert_eq!(alpha, 128, "setOpacity(0.5)");
    op(rig, 11, WindowOp::SetMinSize(400., 300.));
    // SAFETY: an all-zero `MINMAXINFO` is a valid one.
    let mut info: MINMAXINFO = unsafe { std::mem::zeroed() };
    let at = &mut info as *mut MINMAXINFO as isize;
    // SAFETY: `lparam` of this message is a `MINMAXINFO`, which `info` is.
    unsafe { SendMessageW(framed, WM_GETMINMAXINFO, 0, at) };
    let min = (info.ptMinTrackSize.x, info.ptMinTrackSize.y);
    assert_eq!(min, (400, 300), "setMinimumSize");
    op(rig, 11, WindowOp::SetTitle("renamed".into()));
    let mut title = [0u16; 16];
    // SAFETY: the buffer is 16 `u16`s, as the length says.
    let length = unsafe { GetWindowTextW(framed, title.as_mut_ptr(), 16) } as usize;
    assert_eq!(String::from_utf16_lossy(&title[..length]), "renamed");
    let set = WindowOp::SetBounds {
        x: Some(0.),
        y: Some(0.),
        width: Some(300.),
        height: Some(200.),
        animate: false,
    };
    op(rig, 11, set);
    op(rig, 11, WindowOp::Center);
    let (mut window, mut work) = (RECT::default(), RECT::default());
    // SAFETY: `framed` is alive and both out pointers are locals of the right type.
    unsafe {
        GetWindowRect(framed, &mut window);
        SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut work as *mut RECT as *mut c_void, 0);
    }
    let middle = |low: i32, high: i32| (low + high) / 2;
    let off_x = middle(window.left, window.right) - middle(work.left, work.right);
    let off_y = middle(window.top, window.bottom) - middle(work.top, work.bottom);
    assert!(
        off_x.abs() <= 1 && off_y.abs() <= 1,
        "center(): off by {off_x}, {off_y}"
    );
    for id in 11..=13 {
        op(rig, id, WindowOp::Destroy);
    }
}

/// One scenario of the real-display test.
fn a_modal_child_blocks_its_parent_until_it_goes(rig: &Rig, _: &Control) {
    let parent = open(rig, 21, hidden());
    op(rig, 21, WindowOp::ShowInactive);
    let modal = WindowOptions {
        parent: Some(21),
        modal: true,
        ..hidden()
    };
    let child = open(rig, 22, modal);
    // SAFETY: the windows are alive until the scenario destroys them.
    let enabled = |window: HWND| unsafe { IsWindowEnabled(window) } != 0;
    // SAFETY: as above.
    let owner = unsafe { GetWindow(child, GW_OWNER) };
    assert_eq!(owner, parent, "the parent owns the child");
    assert!(
        enabled(parent),
        "a modal child that is not shown blocks nothing"
    );
    op(rig, 22, WindowOp::ShowInactive);
    assert!(!enabled(parent), "a shown modal child blocks its parent");
    let second = WindowOptions {
        parent: Some(21),
        modal: true,
        ..hidden()
    };
    open(rig, 24, second);
    op(rig, 24, WindowOp::ShowInactive);
    op(rig, 22, WindowOp::Destroy);
    assert!(!enabled(parent), "the second modal child still blocks it");
    op(rig, 24, WindowOp::Hide);
    assert!(enabled(parent), "and a hidden one gives it back");
    op(rig, 24, WindowOp::ShowInactive);
    assert!(!enabled(parent), "shown again, it blocks again");
    op(rig, 24, WindowOp::Destroy);
    assert!(enabled(parent), "and destroyed, gives it back");
    let plain = WindowOptions {
        parent: Some(21),
        ..hidden()
    };
    open(rig, 23, plain);
    let destroy = ToShell::Window {
        window: 21,
        op: WindowOp::Destroy,
    };
    rig.hub.to_shell(destroy);
    let said = rig
        .said
        .settle(Duration::from_millis(300), Duration::from_secs(5));
    let closed: Vec<_> = (said.iter())
        .filter(|(_, event)| matches!(event, WindowEvent::Closed))
        .map(|(window, _)| *window)
        .collect();
    assert_eq!(
        closed,
        [23, 21],
        "a parent takes its children with it, and says so"
    );
}

/// One scenario of the real-display test.
fn the_system_asks_the_drag_area_where_the_window_is_dragged_by(rig: &Rig, control: &Control) {
    let bare = WindowOptions {
        frame: false,
        width: 400.,
        height: 300.,
        ..hidden()
    };
    let hwnd = open(rig, 31, bare);
    op(rig, 31, WindowOp::ShowInactive);
    let region = |x, width, drag| AppRegion {
        rect: Rect {
            x,
            y: 0.,
            width,
            height: 40.,
        },
        drag,
    };
    control.set_app_regions(31, vec![region(0., 400., true), region(340., 60., false)]);
    std::thread::sleep(Duration::from_millis(500));
    let hit = |x: i32, y: i32| hit_test(hwnd, x, y);
    assert_eq!(hit(100, 20), HTCAPTION as isize, "the title bar drags");
    assert_ne!(hit(370, 20), HTCAPTION as isize, "the button does not");
    assert_ne!(hit(100, 200), HTCAPTION as isize, "the page does not");
    op(rig, 31, WindowOp::Destroy);
}

/// What the system is answered when it asks where the point (`x`, `y`) of the window's content is.
fn hit_test(hwnd: HWND, x: i32, y: i32) -> isize {
    let mut at = POINT { x, y };
    // SAFETY: `hwnd` is alive; the messages carry coordinates only.
    unsafe {
        ClientToScreen(hwnd, &mut at);
        // The window learns where the mouse is from a move: GPUI decides what is under it.
        let client = ((y as isize) << 16) | (x as isize & 0xffff);
        SendMessageW(hwnd, WM_MOUSEMOVE, 0, client);
        let screen = ((at.y as isize) << 16) | (at.x as isize & 0xffff);
        SendMessageW(hwnd, WM_NCHITTEST, 0, screen)
    }
}

/// One scenario of the real-display test.
fn the_displays_are_told_with_a_primary_one_and_a_scale(rig: &Rig, _: &Control) {
    let start = std::time::Instant::now();
    let displays = loop {
        if let Some(displays) = rig.said.displays() {
            break displays;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "no displays were reported"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(displays.iter().filter(|display| display.primary).count(), 1);
    let primary = displays
        .iter()
        .find(|display| display.primary)
        .expect("a primary");
    assert!(primary.scale_factor >= 1. && primary.bounds.width > 0.);
    assert!(primary.work_area.height > 0.);
}

/// A person closing the window (`WM_CLOSE`, which the close button and Alt+F4 send) asks the main
/// process, a window that is not closable does not ask, and one the main process does not answer
/// is given up on at the next try after five seconds.
fn a_users_close_asks_and_a_silent_main_process_is_given_up_on(rig: &Rig, _: &Control) {
    let hwnd = open(rig, 50, hidden());
    op(rig, 50, WindowOp::ShowInactive);
    let user_close = |hwnd: HWND| {
        // SAFETY: `hwnd` is a window of the shell under test.
        unsafe { SendMessageW(hwnd, WM_CLOSE, 0, 0) };
        let said = rig
            .said
            .settle(Duration::from_millis(300), Duration::from_secs(5));
        // The focus a real window has is the machine's to decide.
        contract::without_focus(&said)
    };
    assert_eq!(user_close(hwnd), ["CloseRequested"]);
    op(rig, 50, WindowOp::CloseAnswer { prevented: true });
    assert_eq!(
        user_close(hwnd),
        ["CloseRequested"],
        "a kept window can be asked again"
    );
    assert!(
        user_close(hwnd).is_empty(),
        "more clicks wait for the answer, they do not ask again"
    );
    std::thread::sleep(Duration::from_millis(5200));
    assert_eq!(
        user_close(hwnd),
        ["Closed"],
        "a main process silent for five seconds is given up on"
    );

    let fixed = open(
        rig,
        51,
        WindowOptions {
            closable: false,
            ..hidden()
        },
    );
    op(rig, 51, WindowOp::ShowInactive);
    assert!(
        user_close(fixed).is_empty(),
        "a window that cannot be closed does not ask"
    );
    op(rig, 51, WindowOp::Destroy);
}

/// A window that covers its display is not dragged, resized or maximized by what GPUI answers
/// the system, and gets all that back when it leaves full screen.
fn a_full_screen_window_is_not_dragged_or_maximized(rig: &Rig, control: &Control) {
    let bare = WindowOptions {
        frame: false,
        width: 400.,
        height: 300.,
        ..hidden()
    };
    let hwnd = open(rig, 61, bare);
    op(rig, 61, WindowOp::ShowInactive);
    let bar = AppRegion {
        rect: Rect {
            x: 0.,
            y: 0.,
            width: 400.,
            height: 40.,
        },
        drag: true,
    };
    control.set_app_regions(61, vec![bar]);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(hit_test(hwnd, 100, 20), HTCAPTION as isize);
    op(rig, 61, WindowOp::SetFullScreen(true));
    assert_eq!(
        long(hwnd, GWL_STYLE) & WS_MAXIMIZEBOX,
        0,
        "no maximize box while full screen"
    );
    assert_eq!(
        hit_test(hwnd, 100, 20),
        HTCLIENT as isize,
        "a full-screen window is not dragged"
    );
    op(rig, 61, WindowOp::SetFullScreen(false));
    assert_ne!(
        long(hwnd, GWL_STYLE) & WS_MAXIMIZEBOX,
        0,
        "and the box comes back"
    );
    assert_eq!(hit_test(hwnd, 100, 20), HTCAPTION as isize);
    op(rig, 61, WindowOp::SetResizable(false));
    assert_eq!(
        long(hwnd, GWL_STYLE) & WS_MAXIMIZEBOX,
        0,
        "not resizable, not maximizable"
    );
    assert_eq!(
        hit_test(hwnd, 100, 1),
        HTCAPTION as isize,
        "the top of a drag bar drags, though the window cannot be resized"
    );
    op(rig, 61, WindowOp::SetResizable(true));
    op(rig, 61, WindowOp::Destroy);
}

/// Minimizing a maximized window says `Minimized` and not `Unmaximized`; restoring says
/// `Restored` and `Maximized`. A window that is not focusable is not activated by a click.
fn a_minimized_maximized_window_and_a_click_on_one_that_cannot_focus(rig: &Rig, _: &Control) {
    open(rig, 62, hidden());
    let step = |op: WindowOp| {
        rig.hub.to_shell(ToShell::Window { window: 62, op });
        let said = rig
            .said
            .settle(Duration::from_millis(300), Duration::from_secs(5));
        contract::without_focus(&said)
    };
    step(WindowOp::ShowInactive);
    step(WindowOp::Maximize);
    assert_eq!(step(WindowOp::Minimize), ["Minimized"]);
    assert_eq!(step(WindowOp::Restore), ["Restored", "Maximized"]);
    step(WindowOp::Destroy);

    let still = WindowOptions {
        focusable: false,
        ..hidden()
    };
    let hwnd = open(rig, 63, still);
    // SAFETY: `hwnd` is alive; `WM_MOUSEACTIVATE` carries nothing the answer reads.
    let answer = unsafe { SendMessageW(hwnd, WM_MOUSEACTIVATE, 0, 0) };
    assert_eq!(
        answer, MA_NOACTIVATE as isize,
        "a click does not activate focusable: false"
    );
    op(rig, 63, WindowOp::Destroy);
}

#[test]
#[ignore = "opens real windows"]
fn the_gpui_shell_keeps_its_contract_on_a_real_display() {
    on_the_gpui_shell(|rig, control| {
        contract::window_state(rig, 1);
        contract::quiet_and_closing(rig, 2);
        contract::limits(rig, 3);
        the_options_of_a_window_reach_its_hwnd(rig, control);
        a_modal_child_blocks_its_parent_until_it_goes(rig, control);
        the_system_asks_the_drag_area_where_the_window_is_dragged_by(rig, control);
        the_displays_are_told_with_a_primary_one_and_a_scale(rig, control);
        a_users_close_asks_and_a_silent_main_process_is_given_up_on(rig, control);
        a_full_screen_window_is_not_dragged_or_maximized(rig, control);
        a_minimized_maximized_window_and_a_click_on_one_that_cannot_focus(rig, control);
    });
}
