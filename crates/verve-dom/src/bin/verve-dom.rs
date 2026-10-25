//! `verve-dom run`: `verve run` with a document in the realm — `window`, `document` and the
//! web platform's classes, as [`verve_dom::install`] gives them — for trying a DOM-using script
//! from the command line. Nothing draws it: `requestAnimationFrame` runs on the realm's timers.

use std::process::ExitCode;

fn main() -> ExitCode {
    verve::cli::main_with(std::env::args_os(), |runtime| {
        verve_dom::install(runtime, verve_dom::Options::default()).map(|_| ())
    })
}
