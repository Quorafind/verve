//! An Electron executable run as Node: `ELECTRON_RUN_AS_NODE=1 electron script.js`, which is how
//! `fork`, VS Code and many tools use it. The test is its own executable (`harness = false`): with
//! the variable set it is the `electron` binary of an embedder, answering to `cli::main_node`;
//! without it, it runs that binary and checks what it says.

use std::ffi::OsString;
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    if std::env::var_os("ELECTRON_RUN_AS_NODE").is_some_and(|v| !v.is_empty()) {
        let args: Vec<OsString> = std::env::args_os().collect();
        return verve::cli::main_node(args, |_| Ok(()));
    }
    version_and_print();
    fork_inherits_node_mode();
    permission_and_unknown_flags();
    println!("run_as_node: ok");
    ExitCode::SUCCESS
}

/// Runs this executable as Node with `args`; answers its standard output.
fn node(args: &[&str]) -> String {
    let output = Command::new(std::env::current_exe().unwrap())
        .args(args)
        .env("ELECTRON_RUN_AS_NODE", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n")
}

fn version_and_print() {
    assert_eq!(node(&["--version"]), "v22.20.0\n");
    assert_eq!(node(&["-p", "process.versions.node"]), "22.20.0\n");
    // Node's `process`, with Electron's version still on it, and no Electron `type`.
    assert_eq!(
        node(&[
            "-p",
            "[typeof process.versions.electron, typeof process.type].join()"
        ]),
        "string,undefined\n"
    );
    // Everything is granted, programs included.
    assert_eq!(
        node(&[
            "-p",
            "process.permission.has('child') && process.permission.has('fs.read')"
        ]),
        "true\n"
    );
    assert_eq!(
        node(&["-e", "console.log(process.argv.slice(1).join())", "a", "b"]),
        "a,b\n"
    );
}

/// `fork` of the executable from itself: in Electron's Node mode it sets `ELECTRON_RUN_AS_NODE`
/// for the child, which then is Node too, and answers over the channel.
fn fork_inherits_node_mode() {
    let dir = std::env::temp_dir().join(format!("verve-run-as-node-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let child = dir.join("child.js");
    std::fs::write(
        &child,
        "process.on('message', (m) => { process.send([m, process.env.ELECTRON_RUN_AS_NODE, typeof process.versions.electron]); process.disconnect(); });",
    )
    .unwrap();
    let shown = serde_json::to_string(&child.to_string_lossy()).unwrap();
    let script = format!(
        "const c = require('child_process').fork({shown}, [], {{ execArgv: [] }}); \
         c.on('message', (m) => console.log(JSON.stringify(m))); c.send('hello');"
    );
    assert_eq!(node(&["-e", &script]), "[\"hello\",\"1\",\"string\"]\n");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `--permission` turns Node's permission model on; Node's other flags are accepted and ignored;
/// and a first word that is a Verve subcommand elsewhere is a script here.
fn permission_and_unknown_flags() {
    assert_eq!(
        node(&["--permission", "-p", "process.permission.has('child')"]),
        "false\n"
    );
    assert_eq!(
        node(&[
            "--trace-warnings",
            "--inspect=0",
            "--enable-source-maps",
            "-p",
            "1+1"
        ]),
        "2\n"
    );
    let dir = std::env::temp_dir().join(format!("verve-run-as-node-sub-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("run");
    std::fs::write(&script, "console.log('ran')").unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .arg("run")
        .env("ELECTRON_RUN_AS_NODE", "1")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&output.stdout), "ran\n");
    let _ = std::fs::remove_dir_all(&dir);
}
