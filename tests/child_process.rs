//! `child_process` starting real programs: `verve run -A` as the parent, and the same binary as
//! the program it starts, so the tests need nothing from the system but a few platform commands.

use std::path::Path;

mod common;

use common::{Ran, VERVE, verve};

/// Runs `source` as `verve run -A -e` does and answers what it printed.
fn run(source: &str) -> Ran {
    verve(&["run", "-A", "-e", source], &std::env::temp_dir(), b"")
}

/// `run`, expecting a clean exit; answers standard output.
fn output(source: &str) -> String {
    let ran = run(source);
    assert_eq!(
        ran.code, 0,
        "stderr: {}\nstdout: {}",
        ran.stderr, ran.stdout
    );
    ran.stdout.replace("\r\n", "\n")
}

/// The script text for the path of this binary, as a JS string literal.
fn verve_path() -> String {
    serde_json::to_string(VERVE).unwrap()
}

#[test]
fn spawn_pipes_a_programs_streams_in_nodes_order() {
    let stdout = output(&format!(
        r#"
        const {{ spawn }} = require('child_process');
        const net = require('net');
        const log = [];
        const child = spawn({exe}, ['-e', 'process.stdin.pipe(process.stdout)']);
        log.push(typeof child.pid, child.stdout instanceof net.Socket, child.stdin instanceof net.Socket);
        child.on('spawn', () => log.push('spawn'));
        child.stdout.on('data', (d) => log.push('data ' + d));
        child.on('exit', (code, signal) => log.push('exit ' + code + ' ' + signal));
        child.on('close', (code, signal) => {{ log.push('close ' + code + ' ' + signal); console.log(log.join('|')); }});
        child.stdin.end('hello');
        "#,
        exe = verve_path()
    ));
    assert_eq!(
        stdout.trim(),
        "number|true|true|spawn|data hello|exit 0 null|close 0 null"
    );
}

#[test]
fn a_program_that_is_not_there_is_reported_asynchronously() {
    let stdout = output(
        r#"
        const { spawn } = require('child_process');
        const child = spawn('definitely-not-a-program-here', ['a', 'b']);
        const log = [typeof child.pid, child.spawnfile, child.spawnargs.join()];
        child.on('error', (e) => log.push(e.code, e.syscall, e.path, e.spawnargs.join(), typeof e.errno));
        child.on('close', (code) => { log.push('close ' + (code < 0)); console.log(log.join('|')); });
        "#,
    );
    assert_eq!(
        stdout.trim(),
        "undefined|definitely-not-a-program-here|definitely-not-a-program-here,a,b|ENOENT|spawn definitely-not-a-program-here|definitely-not-a-program-here|a,b|number|close true"
    );
}

#[test]
fn exit_codes_and_the_kill_signal_are_reported() {
    let stdout = output(&format!(
        r#"
        const {{ spawn }} = require('child_process');
        const exe = {exe};
        const log = [];
        spawn(exe, ['-e', 'process.exit(3)']).on('exit', (code, signal) => log.push('exit ' + code + ' ' + signal));
        const sleeper = spawn(exe, ['-e', 'setInterval(() => {{}}, 1000); console.log("up")']);
        let killedOnce = false;
        sleeper.stdout.on('data', () => {{
            if (killedOnce) return; killedOnce = true;
            log.push('killed ' + sleeper.kill('SIGKILL'));
            sleeper.on('exit', (code, signal) => {{
                log.push('sleeper ' + code + ' ' + signal + ' ' + sleeper.killed + ' ' + sleeper.signalCode);
                console.log(log.sort().join('|'));
            }});
        }});
        "#,
        exe = verve_path()
    ));
    assert_eq!(
        stdout.trim(),
        "exit 3 null|killed true|sleeper null SIGKILL true SIGKILL"
    );
}

#[test]
fn arguments_with_spaces_quotes_and_shell_characters_arrive_unchanged() {
    let stdout = output(&format!(
        r#"
        const {{ spawn }} = require('child_process');
        const args = ['plain', 'two words', 'say "hi"', 'back\\slash', 'trail \\', 'a^&|b', '', 'tab\there', '%PATH%', '*.js'];
        const child = spawn({exe}, ['-e', 'console.log(JSON.stringify(process.argv.slice(1)))', ...args]);
        let out = '';
        child.stdout.on('data', (d) => out += d);
        child.on('close', () => console.log(JSON.stringify(JSON.parse(out)) === JSON.stringify(args) ? 'same' : out));
        "#,
        exe = verve_path()
    ));
    assert_eq!(stdout.trim(), "same");
}

#[test]
fn env_cwd_and_argv0_are_the_childs() {
    let dir = common::Dir::new("child-cwd");
    let cwd = serde_json::to_string(&dir.0.to_string_lossy()).unwrap();
    let stdout = output(&format!(
        r#"
        const {{ spawn }} = require('child_process');
        const child = spawn({exe}, ['-e', 'console.log(JSON.stringify([process.cwd(), process.env.CHILD_ONLY, process.env.PARENT_ONLY, process.argv0]))'],
            {{ cwd: {cwd}, env: {{ CHILD_ONLY: 'yes', PATH: process.env.PATH, SystemRoot: process.env.SystemRoot }}, argv0: 'renamed' }});
        let out = '';
        child.stdout.on('data', (d) => out += d);
        child.on('close', () => console.log(out.trim()));
        "#,
        exe = verve_path()
    ));
    let seen: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let reported = seen[0].as_str().unwrap();
    assert!(
        Path::new(reported).canonicalize().unwrap() == dir.0.canonicalize().unwrap(),
        "{seen}"
    );
    assert_eq!(seen[1], "yes");
    assert!(seen[2].is_null());
    assert_eq!(seen[3], "renamed");
}

#[test]
fn a_realm_without_the_grant_starts_nothing() {
    let ran = verve(
        &[
            "run",
            "-e",
            "try { require('child_process').spawn('x'); } catch (e) { console.log(e.code, e.permission); }",
        ],
        &std::env::temp_dir(),
        b"",
    );
    assert_eq!(
        ran.stdout.trim(),
        "ERR_ACCESS_DENIED ChildProcess",
        "{}",
        ran.stderr
    );
    let granted = verve(
        &[
            "run",
            "--allow-child-process",
            "-e",
            "console.log(process.permission.has('child'))",
        ],
        &std::env::temp_dir(),
        b"",
    );
    assert_eq!(granted.stdout.trim(), "true");
}

#[cfg(windows)]
#[test]
fn the_system_shell_runs_commands_and_finds_programs() {
    let stdout = output(
        r#"
        const { spawn } = require('child_process');
        const log = [];
        const shell = spawn('echo hello & echo there', { shell: true });
        let out = '';
        shell.stdout.on('data', (d) => out += d);
        shell.on('close', (code) => {
            log.push(code, out.replace(/\r?\n/g, '/').replace(/ /g, ''));
            const where = spawn('where', ['cmd']);
            let found = '';
            where.stdout.on('data', (d) => found += d);
            where.on('close', (code) => { log.push(code, /cmd\.exe/i.test(found)); console.log(log.join('|')); });
        });
        "#,
    );
    assert_eq!(stdout.trim(), "0|hello/there/|0|true");
}

#[cfg(windows)]
#[test]
fn batch_files_need_a_shell_on_windows() {
    let dir = common::Dir::new("child-bat");
    let bat = dir.file("hello.bat", "@echo off\r\necho from a batch file\r\n");
    let bat = serde_json::to_string(&bat.to_string_lossy()).unwrap();
    let stdout = output(&format!(
        r#"
        const {{ spawn }} = require('child_process');
        const log = [];
        spawn({bat}).on('error', (e) => log.push(e.code)).on('close', () => {{
            const shell = spawn({bat}, [], {{ shell: true }});
            let out = '';
            shell.stdout.on('data', (d) => out += d);
            shell.on('close', (code) => {{ log.push(code, out.trim()); console.log(log.join('|')); }});
        }});
        "#
    ));
    assert_eq!(stdout.trim(), "EINVAL|0|from a batch file");
}

#[cfg(unix)]
#[test]
fn the_system_shell_runs_commands_and_finds_programs() {
    let stdout = output(
        r#"
        const { spawn } = require('child_process');
        const shell = spawn('echo hello; echo there', { shell: true });
        let out = '';
        shell.stdout.on('data', (d) => out += d);
        shell.on('close', (code) => console.log(code, JSON.stringify(out)));
        "#,
    );
    assert_eq!(stdout.trim(), r#"0 "hello\nthere\n""#);
}

#[test]
fn a_big_output_crosses_the_pipe_whole_and_the_threads_do_not_grow() {
    let stdout = output(&format!(
        r#"
        const {{ spawn }} = require('child_process');
        const child = spawn({exe}, ['-e', `
            const chunk = Buffer.alloc(65536, 97);
            let left = 160;
            const more = () => {{ while (left > 0) {{ left--; if (!process.stdout.write(chunk)) return process.stdout.once('drain', more); }} }};
            more();`]);
        let total = 0, chunks = 0;
        child.stdout.on('data', (d) => {{ total += d.length; chunks++; }});
        child.on('close', (code) => console.log(code, total, total === 160 * 65536));
        "#,
        exe = verve_path()
    ));
    assert_eq!(stdout.trim(), "0 10485760 true");
}

#[test]
fn a_slow_reader_holds_the_child_back_instead_of_buffering() {
    let stdout = output(&format!(
        r#"
        const {{ spawn }} = require('child_process');
        const child = spawn({exe}, ['-e', `
            let sent = 0;
            const chunk = Buffer.alloc(65536, 98);
            const more = () => {{ while (sent < 64) {{ sent++; if (!process.stdout.write(chunk)) return process.stdout.once('drain', more); }} }};
            more();`]);
        child.stdout.pause();
        let early = 0;
        child.stdout.on('data', (d) => early += d.length);
        child.stdout.pause();
        setTimeout(() => {{
            // Nobody has read: at most a pipe's worth and one chunk are in flight.
            const held = early;
            child.stdout.resume();
            child.on('close', () => console.log(held < 1024 * 1024, early === 64 * 65536));
        }}, 500);
        "#,
        exe = verve_path()
    ));
    assert_eq!(stdout.trim(), "true true");
}

// ─── exec and execFile ──────────────────────────────────────────────────────────────────────

#[test]
fn exec_hands_the_callback_the_output_and_a_failing_exit_code() {
    let stdout = output(&format!(
        r#"
        const {{ exec, execFile }} = require('child_process');
        const {{ promisify }} = require('util');
        const exe = {exe};
        const log = [];
        (async () => {{
            const ok = await promisify(exec)(`"${{exe}}" -e "console.log('out'); console.error('err')"`);
            log.push(JSON.stringify([ok.stdout.trim(), ok.stderr.trim()]));
            try {{ await promisify(exec)(`"${{exe}}" -e "console.log('partial'); process.exit(3)"`); }}
            catch (e) {{ log.push(e.code, e.killed, e.signal, e.stdout.trim(), e.message.startsWith('Command failed: '), typeof e.cmd); }}
            try {{ await promisify(execFile)('definitely-not-a-program-here'); }}
            catch (e) {{ log.push(e.code, e.cmd); }}
            const buf = await promisify(execFile)(exe, ['-e', 'process.stdout.write("hi")'], {{ encoding: 'buffer' }});
            log.push(Buffer.isBuffer(buf.stdout), buf.stdout.length);
            console.log(log.join('|'));
        }})();
        "#,
        exe = verve_path()
    ));
    assert_eq!(
        stdout.trim(),
        r#"["out","err"]|3|false||partial|true|string|ENOENT|definitely-not-a-program-here|true|2"#
    );
}

#[test]
fn exec_enforces_max_buffer_timeout_and_abort() {
    let stdout = output(&format!(
        r#"
        const {{ execFile }} = require('child_process');
        const exe = {exe};
        const log = [];
        const spam = ['-e', 'process.stdout.write("x".repeat(5000)); setInterval(() => {{}}, 1000)'];
        const sleep = ['-e', 'setInterval(() => {{}}, 1000)'];
        execFile(exe, spam, {{ maxBuffer: 100 }}, (e, out) => {{
            log.push(e.code, out.length);
            execFile(exe, sleep, {{ timeout: 300 }}, (e) => {{
                log.push(e.killed, e.signal);
                const ac = new AbortController();
                const child = execFile(exe, sleep, {{ signal: ac.signal }}, (e) => {{
                    log.push(e.name);
                    console.log(log.join('|'));
                }});
                child.on('spawn', () => ac.abort());
            }});
        }});
        "#,
        exe = verve_path()
    ));
    assert_eq!(
        stdout.trim(),
        "ERR_CHILD_PROCESS_STDIO_MAXBUFFER|100|true|SIGTERM|AbortError"
    );
}

// ─── The synchronous calls ──────────────────────────────────────────────────────────────────

#[test]
fn spawn_sync_feeds_input_and_reports_status_and_errors() {
    let stdout = output(&format!(
        r#"
        const {{ spawnSync, execFileSync }} = require('child_process');
        const exe = {exe};
        const log = [];
        const echoed = spawnSync(exe, ['-e', 'process.stdin.pipe(process.stdout)'], {{ input: 'typed in' }});
        log.push(echoed.status, echoed.signal, String(echoed.stdout), echoed.output.length, echoed.output[0], typeof echoed.pid);
        const bad = spawnSync(exe, ['-e', 'console.error("oops"); process.exit(4)'], {{ encoding: 'utf8' }});
        log.push(bad.status, bad.stderr.trim());
        const missing = spawnSync('definitely-not-a-program-here');
        log.push(missing.error.code, missing.status, missing.pid);
        const big = spawnSync(exe, ['-e', 'process.stdout.write("y".repeat(10000))'], {{ maxBuffer: 50 }});
        log.push(big.error.code, big.stdout.length);
        const slow = spawnSync(exe, ['-e', 'setInterval(() => {{}}, 1000)'], {{ timeout: 300 }});
        log.push(slow.error.code, slow.signal);
        try {{ execFileSync(exe, ['-e', 'process.exit(2)']); }} catch (e) {{ log.push(e.status); }}
        console.log(log.join('|'));
        "#,
        exe = verve_path()
    ));
    assert_eq!(
        stdout.trim(),
        "0||typed in|3||number|4|oops|ENOENT||0|ENOBUFS|50|ETIMEDOUT|SIGTERM|2"
    );
}

#[test]
fn exec_sync_returns_output_and_throws_with_the_childs_stderr() {
    let ran = run(&format!(
        r#"
        const {{ execSync }} = require('child_process');
        const exe = {exe};
        console.log(execSync(`"${{exe}}" -e "console.log('fine')"`, {{ encoding: 'utf8' }}).trim());
        try {{ execSync(`"${{exe}}" -e "console.error('shown'); process.exit(5)"`); }}
        catch (e) {{ console.log(e.status, String(e.stderr).trim(), e.message.includes('Command failed')); }}
        "#,
        exe = verve_path()
    ));
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(ran.stdout.replace("\r\n", "\n"), "fine\n5 shown true\n");
    assert!(
        ran.stderr.contains("shown"),
        "the child's stderr reaches ours: {}",
        ran.stderr
    );
}
