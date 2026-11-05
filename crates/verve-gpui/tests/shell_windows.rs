//! The GPUI shell on a real display, held to the contract every shell keeps
//! (`crates/verve-electron/tests/support/contract.rs`, which the headless shell runs too).
//!
//! These open real windows, on the screen of whoever runs them: a few seconds of an 800 x 600
//! window that is shown, hidden, maximized, minimized, put on full screen and closed. They are
//! `#[ignore]`d for that reason; run them with `cargo test -p verve-gpui --test shell_windows -- --ignored`.
#![cfg(windows)]

#[path = "../../verve-electron/tests/support/contract.rs"]
mod contract;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::time::Duration;

use contract::{Rig, watch};
use verve_electron::{ElectronOptions, Hub};

/// How long the whole run may take before the test ends the process, windows and all.
const LIMIT: Duration = Duration::from_secs(120);

/// Runs `scenario` against the GPUI shell, on a hub with no main process: the scenario is it.
fn on_the_gpui_shell(scenario: impl FnOnce(&Rig) + Send + 'static) {
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
    let code = verve_gpui::run_with(hub, move |hub, _| {
        let hub = Arc::clone(hub);
        std::thread::spawn(move || {
            let rig = Rig::new(&hub, &said, true);
            let outcome = catch_unwind(AssertUnwindSafe(|| scenario(&rig)));
            if let Err(panic) = &outcome {
                eprintln!("{panic:?}");
            }
            hub.shutdown(if outcome.is_ok() { 0 } else { 7 });
        });
    });
    assert_eq!(code, 0, "the scenario failed (see standard error)");
}

#[test]
#[ignore = "opens real windows"]
fn the_gpui_shell_keeps_the_shell_contract() {
    on_the_gpui_shell(|rig| {
        contract::window_state(rig, 1);
        contract::quiet_and_closing(rig, 2);
        contract::limits(rig, 3);
    });
}
