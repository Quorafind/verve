//! The headless shell, held to the contract every shell keeps (`support/contract.rs`): the
//! GPUI shell runs the same steps in `crates/verve-gpui/tests/shell_windows.rs`.

#[path = "support/contract.rs"]
mod contract;

use std::sync::Arc;

use contract::{Rig, Said, watch};
use verve_electron::{ElectronOptions, HeadlessOptions, HeadlessShell, Hub};

/// A hub with a headless shell under it and nothing above: the test is the main process.
fn rig(test: impl FnOnce(&Rig)) {
    let options = ElectronOptions {
        app_path: std::env::temp_dir(),
        dark: false,
    };
    let hub: Arc<Hub> = Hub::new(options);
    let shell = HeadlessShell::new(&hub, HeadlessOptions::default());
    let said: Said = watch(&hub);
    let rig = Rig::new(&hub, &said, false);
    test(&rig);
    drop(shell);
}

#[test]
fn a_window_goes_through_the_window_state_fixtures_steps_in_electrons_order() {
    rig(|rig| contract::window_state(rig, 1));
}

#[test]
fn quiet_operations_say_nothing_and_a_kept_close_keeps_the_window() {
    rig(|rig| contract::quiet_and_closing(rig, 1));
}

#[test]
fn a_window_stays_within_its_limits() {
    rig(|rig| contract::limits(rig, 1));
}
