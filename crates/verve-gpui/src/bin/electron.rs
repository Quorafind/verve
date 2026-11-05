//! `electron [--headless] [options] [app-directory | file.js | url]`: the command line of Electron
//! 39. `--headless` runs the app on a shell with no display that prints what the windows do.
//!
//! What the arguments mean is `verve_electron::launcher`'s; this decides what to do with it: print
//! a version, run as Node (`ELECTRON_RUN_AS_NODE`) before any window system is touched, or start
//! the app's processes in this one.

use std::ffi::OsString;
use std::io::Write as _;
use std::process::ExitCode;

use verve_electron::launcher::{self, Env, Launch, USAGE};

fn main() -> ExitCode {
    let mut args = std::env::args_os();
    let argv0 = args.next().unwrap_or_else(|| OsString::from("electron"));
    let mut rest: Vec<OsString> = args.collect();
    let headless = rest.first().is_some_and(|arg| arg == "--headless");
    if headless {
        rest.remove(0);
    }
    match launcher::parse(rest, &launcher::exe_path(), &Env::from_process()) {
        Ok(Launch::Version) => print_line(&format!("v{}", verve_electron::ELECTRON_VERSION)),
        Ok(Launch::Abi) => print_line(verve_electron::MODULES_VERSION),
        Ok(Launch::Help) => print_line(USAGE),
        Ok(Launch::Interactive) => fail("REPL is not supported by Verve"),
        Ok(Launch::RunAsNode { args }) => launcher::run_as_node(argv0, args),
        Ok(Launch::Start(start)) => match launcher::boot(*start) {
            Ok((hub, program)) if headless => finish(verve_electron::headless::run(&hub, program)),
            Ok((hub, program)) => finish(verve_gpui::run(hub, program)),
            Err(message) => fail(&message),
        },
        Err(message) => fail(&message),
    }
}

fn print_line(text: &str) -> ExitCode {
    println!("{text}");
    ExitCode::SUCCESS
}

fn fail(message: &str) -> ExitCode {
    eprintln!("{message}");
    ExitCode::FAILURE
}

/// Ends the process with the app's code, whole: Windows takes 32 bits, and GPUI's own quit would
/// have ended a macOS process with 0.
fn finish(code: i32) -> ! {
    // A flush that fails has nowhere left to say so.
    let _ = (std::io::stdout().flush(), std::io::stderr().flush());
    std::process::exit(code)
}
