//! Acceptance: the two Rebon programs `verve run` exists to host, run exactly as Rebon starts
//! them under Node — its contained plugin host (`plugin-host/src/cli.mjs`), and Code Mode's
//! one-shot runner (`code_mode_runner.js`, given to `-e`).
//!
//! They need a Rebon checkout: `VERVE_REBON_CHECKOUT`, or `../reboncode-verve` beside this
//! repository. Without one these tests say so and pass without running anything — they are
//! skipped, not failed.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const VERVE: &str = env!("CARGO_BIN_EXE_verve");

fn checkout() -> Option<PathBuf> {
    let path = std::env::var_os("VERVE_REBON_CHECKOUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../reboncode-verve"));
    let path = std::fs::canonicalize(path).ok()?;
    let path = match path.to_string_lossy().strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => path,
    };
    path.join("runtimes/node/plugin-host/src/cli.mjs")
        .is_file()
        .then_some(path)
}

macro_rules! rebon_or_skip {
    () => {
        match checkout() {
            Some(path) => path,
            None => {
                eprintln!(
                    "skipped: no Rebon checkout (set VERVE_REBON_CHECKOUT, or put one at ../reboncode-verve)"
                );
                return;
            }
        }
    };
}

/// Waits for `child` to exit, at most `limit`.
fn exit_within(child: &mut std::process::Child, limit: Duration) -> i32 {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status.code().unwrap_or(-1);
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("did not exit within {limit:?}");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn envelope(call: &str, method: &str) -> String {
    format!(
        r#"{{"protocol_version":1,"host_epoch":7,"plugin_id":"$rebon/platform","scope_id":"$rebon/control","scope_generation":0,"call_id":"{call}","message":{{"type":"request","method":"{method}","payload":null}}}}"#
    )
}

/// (a) The contained plugin host, with the flags `container_launch` builds for Node: the
/// runtime's scripts readable, a data folder writable, nothing else; initialized, then shut
/// down, answering both and exiting 0.
#[test]
fn rebons_plugin_host_initializes_and_shuts_down() {
    let rebon = rebon_or_skip!();
    let runtimes = rebon.join("runtimes").join("node");
    let host = runtimes.join("plugin-host").join("src").join("cli.mjs");
    let loader = runtimes
        .join("compose-runtime")
        .join("src")
        .join("index.mjs");
    let data = std::env::temp_dir().join(format!("verve-rebon-host-{}", std::process::id()));
    std::fs::create_dir_all(&data).unwrap();
    let data = std::fs::canonicalize(&data).unwrap();
    let data = PathBuf::from(data.to_string_lossy().trim_start_matches(r"\\?\"));
    let mut child = Command::new(VERVE)
        .arg("run")
        .arg("--permission")
        .arg(format!("--allow-fs-read={}", runtimes.display()))
        .arg(format!("--allow-fs-read={}", data.display()))
        .arg(format!("--allow-fs-write={}", data.display()))
        .arg(&host)
        .arg("--loader")
        .arg(&loader)
        .current_dir(&data)
        .env_clear()
        .envs(
            ["SystemRoot", "SYSTEMROOT", "windir"]
                .iter()
                .filter_map(|k| std::env::var_os(k).map(|v| (k.to_string(), v))),
        )
        .env("HOME", &data)
        .env("USERPROFILE", &data)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let read_frame = |stdout: &mut BufReader<_>| -> serde_json::Value {
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or_else(|_| panic!("not a frame: {line:?}"))
    };
    writeln!(stdin, "{}", envelope("init-1", "platform/initialize")).unwrap();
    stdin.flush().unwrap();
    let initialized = read_frame(&mut stdout);
    assert_eq!(initialized["call_id"], "init-1");
    assert_eq!(initialized["host_epoch"], 7);
    assert_eq!(initialized["message"]["type"], "terminal");
    assert_eq!(initialized["message"]["status"], "success", "{initialized}");
    assert_eq!(
        initialized["message"]["payload"]["capabilities"]["scope_lifecycle"],
        true
    );
    writeln!(stdin, "{}", envelope("down-2", "platform/shutdown")).unwrap();
    stdin.flush().unwrap();
    let down = read_frame(&mut stdout);
    assert_eq!(down["call_id"], "down-2");
    assert_eq!(down["message"]["status"], "success", "{down}");
    assert_eq!(down["message"]["payload"]["shutdown"], true);
    // Exits on its own, its input still open: the host ends itself after shutdown.
    let code = exit_within(&mut child, Duration::from_secs(30));
    drop(stdin);
    let mut stderr = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut stderr).unwrap();
    assert_eq!(code, 0, "stderr: {stderr}");
    let _ = std::fs::remove_dir_all(&data);
}

/// The plugin host refuses to start without its scripts granted: the permission model is
/// really on.
#[test]
fn rebons_plugin_host_without_its_grant_cannot_load() {
    let rebon = rebon_or_skip!();
    let runtimes = rebon.join("runtimes").join("node");
    let host = runtimes.join("plugin-host").join("src").join("cli.mjs");
    let output = Command::new(VERVE)
        .arg("run")
        .arg("--permission")
        .arg(&host)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("ERR_ACCESS_DENIED"), "{stderr}");
}

/// Code Mode's command line, flag for flag (`harden` in `rebon-code-runner`): no environment
/// but what Windows needs, the permission model on with nothing granted, `-e` the runner.
fn code_mode(source: &str) -> Command {
    let mut command = Command::new(VERVE);
    command
        .current_dir(std::env::temp_dir())
        .env_clear()
        .envs(
            ["SystemRoot", "SystemDrive", "WINDIR"]
                .iter()
                .filter_map(|k| std::env::var_os(k).map(|v| (k.to_string(), v))),
        )
        .arg("--no-warnings")
        .arg("--permission")
        .arg("--experimental-vm-modules")
        .arg("--no-addons")
        .arg("--disable-proto=throw")
        .arg("--max-old-space-size=192")
        .arg(format!("--stack_size={}", (4 * 1024 * 1024 / 1024) * 3 / 4))
        .arg("-e")
        .arg(source);
    command
}

fn execute_frame(code: &str) -> String {
    serde_json::json!({
        "v": 1,
        "type": "execute",
        "code": code,
        "limits": {
            "maxOutputBytes": 512 * 1024,
            "maxLogBytes": 256 * 1024,
            "maxLogLines": 1024,
            "maxFrameBytes": 1024 * 1024,
            "maxIncomingBytes": 8 * 1024 * 1024,
        },
    })
    .to_string()
}

/// (b) Code Mode: the probe it starts every runtime with, then an execute frame whose program
/// logs, calls a tool, and answers — and a program that does not parse.
#[test]
fn rebons_code_mode_runner_executes_a_program() {
    let rebon = rebon_or_skip!();
    let runner =
        std::fs::read_to_string(rebon.join("crates/rebon-code-runner/js/code_mode_runner.js"))
            .unwrap();
    // `probe_permission_flag`: the hardened flags with `-e 0` must start and exit 0.
    let probe = code_mode("0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(probe.success());

    let mut child = code_mode(&runner)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let program = "console.log('adding', 40); const got = await tools.add({ a: 40, b: 1 }); return { sum: got.sum + 1 };";
    writeln!(stdin, "{}", execute_frame(program)).unwrap();
    stdin.flush().unwrap();
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let call: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(call["type"], "tool_call", "{line}");
    assert_eq!(call["name"], "add");
    assert_eq!(call["input"], serde_json::json!({ "a": 40, "b": 1 }));
    let result = serde_json::json!({ "v": 1, "type": "tool_result", "id": call["id"], "ok": true, "value": { "sum": 41 } });
    writeln!(stdin, "{result}").unwrap();
    stdin.flush().unwrap();
    line.clear();
    stdout.read_line(&mut line).unwrap();
    let terminal: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(terminal["type"], "terminal", "{line}");
    assert_eq!(terminal["ok"], true, "{line}");
    assert_eq!(terminal["result"], serde_json::json!({ "sum": 42 }));
    assert_eq!(terminal["logs"], serde_json::json!(["adding 40"]));
    // Its input still open, it exits once it has answered.
    assert_eq!(exit_within(&mut child, Duration::from_secs(30)), 0);
    drop(stdin);

    // A program that does not parse is a `syntax` terminal and exit 1.
    let mut child = code_mode(&runner)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, "{}", execute_frame("return (;")).unwrap();
    drop(stdin);
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    line.clear();
    stdout.read_line(&mut line).unwrap();
    let terminal: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(terminal["ok"], false, "{line}");
    assert_eq!(terminal["kind"], "syntax", "{line}");
    assert_eq!(exit_within(&mut child, Duration::from_secs(30)), 1);
}

/// What Code Mode's guest is denied: the disk, by the permission model; `__proto__`, by the
/// flag.
#[test]
fn code_modes_guest_is_denied_the_disk() {
    let _rebon = rebon_or_skip!();
    let output = code_mode(
        "const fs = process.getBuiltinModule('fs');
         let read; try { fs.readFileSync(process.execPath); read = 'read'; } catch (e) { read = e.code; }
         let proto; try { ({}).__proto__; proto = 'reached'; } catch (e) { proto = e.code; }
         console.log(read, proto, process.permission.has('fs.read'), process.versions.node.split('.')[0] >= 20);",
    )
    .stdin(Stdio::null())
    .output()
    .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "ERR_ACCESS_DENIED ERR_PROTO_ACCESS false true\n"
    );
}

/// What Code Mode's guest cannot reach: the runner puts it in a `vm` context holding only its
/// `console` and `tools` facades, with code generation from strings and WebAssembly refused and
/// dynamic import disabled. Every way to `process` it tries fails, nothing it writes reaches
/// stdout, and its tool call still goes through.
#[test]
fn code_modes_guest_cannot_reach_process() {
    let rebon = rebon_or_skip!();
    let runner =
        std::fs::read_to_string(rebon.join("crates/rebon-code-runner/js/code_mode_runner.js"))
            .unwrap();
    let program = r#"
        const probes = {};
        const attempt = (name, f) => {
            try { probes[name] = String(f()); } catch (e) { probes[name] = e.name + ': ' + e.message; }
        };
        attempt('typeof', () => typeof process);
        attempt('global', () => globalThis.process);
        attempt('this', () => this.process);
        attempt('constructor', () => this.constructor.constructor("return process")());
        attempt('facade', () => console.log.constructor("return process")());
        attempt('tools', () => tools.constructor.constructor("return process")());
        attempt('require', () => require('fs'));
        attempt('write', () => process.stdout.write('{"v":1,"type":"terminal","ok":true}\n'));
        attempt('exit', () => process.exit(3));
        attempt('eval', () => eval('1 + 1'));
        attempt('wasm', () => new WebAssembly.Module(new Uint8Array([0, 97, 115, 109, 1, 0, 0, 0])));
        attempt('names', () => Object.getOwnPropertyNames(globalThis)
            .filter(n => ['process', 'require', 'module', 'Buffer', 'setTimeout', 'global'].includes(n))
            .join());
        try { await import('node:fs'); probes.import = 'imported'; } catch (e) { probes.import = e.message; }
        const got = await tools.add({ a: 1, b: 2 });
        return { probes, sum: got.sum };
    "#;
    let mut child = code_mode(&runner)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    writeln!(stdin, "{}", execute_frame(program)).unwrap();
    stdin.flush().unwrap();
    // The first line out is the tool call: no frame the guest forged came before it.
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let call: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(call["type"], "tool_call", "{line}");
    assert_eq!(call["name"], "add");
    let result = serde_json::json!({ "v": 1, "type": "tool_result", "id": call["id"], "ok": true, "value": { "sum": 3 } });
    writeln!(stdin, "{result}").unwrap();
    stdin.flush().unwrap();
    line.clear();
    stdout.read_line(&mut line).unwrap();
    let terminal: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(terminal["type"], "terminal", "{line}");
    assert_eq!(terminal["ok"], true, "{line}");
    assert_eq!(terminal["result"]["sum"], 3, "{line}");
    let probes = &terminal["result"]["probes"];
    let denied = "EvalError: Code generation from strings disallowed for this context";
    let expected = serde_json::json!({
        "typeof": "undefined",
        "global": "undefined",
        "this": "undefined",
        "constructor": denied,
        "facade": denied,
        "tools": denied,
        "require": "ReferenceError: require is not defined",
        "write": "ReferenceError: process is not defined",
        "exit": "ReferenceError: process is not defined",
        "eval": denied,
        "wasm": "CompileError: Wasm code generation disallowed by embedder",
        "names": "",
        "import": "dynamic import is disabled in Code Mode",
    });
    assert_eq!(probes, &expected, "{line}");
    assert_eq!(exit_within(&mut child, Duration::from_secs(30)), 0);
    // Nothing more was written: the guest's `process.stdout.write` never ran.
    line.clear();
    stdout.read_line(&mut line).unwrap();
    assert_eq!(line, "");
    drop(stdin);
}
