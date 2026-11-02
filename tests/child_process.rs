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
        // Windows ignores trailing dots and spaces, so these are batch files too, and the
        // argument must never reach cmd.exe as a command.
        for (const name of [{bat}, {bat} + ' ', {bat} + '. .']) {{
            try {{ spawn(name, ['a&echo INJECTED']); log.push('started'); }}
            catch (e) {{ log.push(e.code, e.syscall === 'spawn ' + name); }}
        }}
        const shell = spawn({bat}, [], {{ shell: true }});
        let out = '';
        shell.stdout.on('data', (d) => out += d);
        shell.on('close', (code) => {{ log.push(code, out.trim()); console.log(log.join('|')); }});
        "#
    ));
    assert_eq!(
        stdout.trim(),
        "EINVAL|true|EINVAL|true|EINVAL|true|0|from a batch file"
    );
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
fn a_big_output_crosses_the_pipe_whole() {
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
            log.push(e.code, e instanceof RangeError, out.length);
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
        "ERR_CHILD_PROCESS_STDIO_MAXBUFFER|true|100|true|SIGTERM|AbortError"
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

// ─── fork and the IPC channel ───────────────────────────────────────────────────────────────

/// A module that answers every message it is sent, and leaves when told to.
const ECHO_CHILD: &str = r#"
process.on('message', (m) => {
  if (m === 'bye') { process.disconnect(); return; }
  process.send({ echo: m, connected: process.connected, type: typeof process.send }, (err) => {
    if (err) process.exit(9);
  });
});
process.on('disconnect', () => process.stdout.write('child saw disconnect\n'));
"#;

#[test]
fn fork_round_trips_json_messages_and_disconnects_on_both_sides() {
    let dir = common::Dir::new("fork-json");
    let child = serde_json::to_string(&dir.file("child.js", ECHO_CHILD).to_string_lossy()).unwrap();
    let stdout = output(&format!(
        r#"
        const {{ fork }} = require('child_process');
        const child = fork({child}, [], {{ execPath: {exe}, execArgv: [], silent: true }});
        const log = [child.connected, typeof child.channel.unref];
        let out = '';
        child.stdout.on('data', (d) => out += d);
        child.on('message', (m) => {{
            log.push(JSON.stringify(m));
            if (m.echo === 'two') child.send('bye');
        }});
        child.on('disconnect', () => log.push('parent saw disconnect ' + child.connected));
        child.on('close', (code) => {{ log.push('close ' + code + ' ' + out.trim()); console.log(log.join('|')); }});
        child.send({{ n: 1, list: [1, 'a', null] }}, (err) => log.push('sent ' + err));
        child.send('two');
        "#,
        exe = verve_path()
    ));
    assert_eq!(
        stdout.trim(),
        concat!(
            "true|function|sent null|",
            r#"{"echo":{"n":1,"list":[1,"a",null]},"connected":true,"type":"function"}|"#,
            r#"{"echo":"two","connected":true,"type":"function"}|"#,
            "parent saw disconnect false|close 0 child saw disconnect"
        )
    );
}

#[test]
fn fork_with_advanced_serialization_carries_maps_dates_and_bytes() {
    let dir = common::Dir::new("fork-advanced");
    let child = serde_json::to_string(&dir.file("child.js", ECHO_CHILD).to_string_lossy()).unwrap();
    let stdout = output(&format!(
        r#"
        const {{ fork }} = require('child_process');
        const child = fork({child}, [], {{ execPath: {exe}, execArgv: [], silent: true, serialization: 'advanced' }});
        child.on('message', (m) => {{
            const e = m.echo;
            console.log(e.map instanceof Map, e.map.get('k'), e.date instanceof Date, e.date.getTime(), e.bytes instanceof Uint8Array, e.bytes.join());
            child.disconnect();
        }});
        child.send({{ map: new Map([['k', 'v']]), date: new Date(1700000000000), bytes: new Uint8Array([1, 2, 3]) }});
        "#,
        exe = verve_path()
    ));
    assert_eq!(stdout.trim(), "true v true 1700000000000 true 1,2,3");
}

#[test]
fn sending_a_handle_is_refused_and_a_closed_channel_is_reported() {
    let stdout = output(&format!(
        r#"
        const {{ spawn }} = require('child_process');
        const child = spawn({exe}, ['-e', '0']);
        child.send('x', (err) => {{
            let handle;
            try {{ child.send('x', {{}}); }} catch (e) {{ handle = e.code; }}
            console.log(err.code, child.connected, handle);
        }});
        "#,
        exe = verve_path()
    ));
    assert_eq!(
        stdout.trim(),
        "ERR_IPC_CHANNEL_CLOSED false ERR_FEATURE_UNAVAILABLE_ON_PLATFORM"
    );
}

#[test]
fn process_kill_signals_a_process_by_pid_and_says_when_there_is_none() {
    let stdout = output(&format!(
        r#"
        const {{ spawn }} = require('child_process');
        const child = spawn({exe}, ['-e', 'setInterval(() => {{}}, 1000)']);
        const log = [process.kill(child.pid, 0)];
        child.on('exit', () => {{
            try {{ process.kill(child.pid, 0); log.push('still there'); }} catch (e) {{ log.push(e.code, e.syscall); }}
            try {{ process.kill('nope'); }} catch (e) {{ log.push(e.code); }}
            console.log(log.join('|'));
        }});
        log.push(process.kill(child.pid, 'SIGKILL'));
        "#,
        exe = verve_path()
    ));
    assert_eq!(stdout.trim(), "true|true|ESRCH|kill|ERR_INVALID_ARG_TYPE");
}

/// A hundred programs at once all finish, and the reactor does not take a thread for each.
#[test]
#[ignore = "starts a hundred processes"]
fn a_hundred_concurrent_children_all_exit() {
    let stdout = output(&format!(
        r#"
        const {{ spawn }} = require('child_process');
        let done = 0, bad = 0;
        for (let i = 0; i < 100; i++) {{
            const c = spawn({exe}, ['-e', 'process.stdout.write(String(' + i + '))']);
            let out = '';
            c.stdout.on('data', (d) => out += d);
            c.on('close', (code) => {{ if (code !== 0 || out !== String(i)) bad++; if (++done === 100) console.log(done, bad); }});
        }}
        "#,
        exe = verve_path()
    ));
    assert_eq!(stdout.trim(), "100 0");
}

// ─── Arguments, inheritance and the rest of the review ──────────────────────────────────────

#[test]
fn a_nul_in_what_goes_to_the_system_is_refused() {
    let stdout = output(&format!(
        r#"
        const {{ spawn, spawnSync }} = require('child_process');
        const exe = {exe};
        const log = [];
        const attempts = [
            () => spawn(exe, ['a\0b']),
            () => spawn(exe, [], {{ env: {{ A: 'x\0B=1' }} }}),
            () => spawn(exe, [], {{ env: {{ 'A\0B': 'x' }} }}),
            () => spawn(exe, [], {{ argv0: 'a\0b' }}),
            () => spawn(exe, [], {{ shell: 'sh\0' }}),
            () => spawn('a\0b'),
            () => spawn(exe, [], {{ timeout: -1 }}),
        ];
        for (const attempt of attempts) {{ try {{ attempt(); log.push('started'); }} catch (e) {{ log.push(e.code); }} }}
        // Arguments that are not strings are made strings, as Node makes them.
        const r = spawnSync(exe, ['-e', 'console.log(process.argv.slice(1).join())', 1, true, null]);
        log.push(String(r.stdout).trim());
        console.log(log.join('|'));
        "#,
        exe = verve_path()
    ));
    assert_eq!(
        stdout.trim(),
        "ERR_INVALID_ARG_VALUE|ERR_INVALID_ARG_VALUE|ERR_INVALID_ARG_VALUE|ERR_INVALID_ARG_VALUE|ERR_INVALID_ARG_VALUE|ERR_INVALID_ARG_VALUE|ERR_OUT_OF_RANGE|1,true,null"
    );
}

#[test]
fn fork_does_not_run_the_parents_eval_source_again() {
    let dir = common::Dir::new("fork-bomb");
    let child = dir.file(
        "child.js",
        "process.send(process.execArgv); process.disconnect();",
    );
    let child = serde_json::to_string(&child.to_string_lossy()).unwrap();
    // No `execArgv` is given, so the child gets this process's — less `-e <this script>`, which
    // would run this script again, and fork again.
    let stdout = output(&format!(
        r#"
        const {{ fork }} = require('child_process');
        const child = fork({child}, [], {{ execPath: {exe}, silent: true }});
        child.on('message', (m) => console.log(JSON.stringify(m)));
        "#,
        exe = verve_path()
    ));
    assert_eq!(stdout.trim(), r#"["-A"]"#);
}

#[test]
fn what_is_sent_before_disconnect_still_arrives() {
    let dir = common::Dir::new("fork-flush");
    let child = dir.file(
        "child.js",
        "for (let i = 0; i < 200; i++) process.send({ i, pad: 'x'.repeat(2000) }); process.disconnect();",
    );
    let child = serde_json::to_string(&child.to_string_lossy()).unwrap();
    let stdout = output(&format!(
        r#"
        const {{ fork }} = require('child_process');
        const child = fork({child}, [], {{ execPath: {exe}, execArgv: [], silent: true }});
        let seen = 0, last = -1, ordered = true;
        child.on('message', (m) => {{ seen++; if (m.i !== last + 1) ordered = false; last = m.i; }});
        child.on('close', () => console.log(seen, ordered));
        "#,
        exe = verve_path()
    ));
    assert_eq!(stdout.trim(), "200 true");
}

#[test]
fn spawn_with_an_ipc_stdio_entry_gets_a_channel_without_fork() {
    let dir = common::Dir::new("spawn-ipc");
    let child = dir.file(
        "child.js",
        "process.on('message', (m) => { process.send(m + 1); process.disconnect(); });",
    );
    let child = serde_json::to_string(&child.to_string_lossy()).unwrap();
    let stdout = output(&format!(
        r#"
        const {{ spawn }} = require('child_process');
        const child = spawn({exe}, [{child}], {{ stdio: ['ignore', 'ignore', 'ignore', 'ipc'] }});
        child.on('message', (m) => console.log(m, child.stdio[3]));
        child.send(41);
        "#,
        exe = verve_path()
    ));
    assert_eq!(stdout.trim(), "42 null");
}

#[cfg(windows)]
#[test]
fn only_the_signals_windows_has_are_sent_and_hide_still_runs_the_program() {
    let stdout = output(&format!(
        r#"
        const {{ spawn }} = require('child_process');
        const log = [];
        const child = spawn({exe}, ['-e', 'setInterval(() => {{}}, 1000)']);
        try {{ child.kill('SIGHUP'); log.push('sent'); }} catch (e) {{ log.push(e.code); }}
        log.push(child.killed);
        child.on('exit', (code, signal) => {{
            log.push(signal);
            const hidden = spawn('cmd', ['/c', 'echo shown'], {{ windowsHide: true }});
            let out = '';
            hidden.stdout.on('data', (d) => out += d);
            hidden.on('close', () => {{ log.push(out.trim()); console.log(log.join('|')); }});
        }});
        child.kill('SIGKILL');
        "#,
        exe = verve_path()
    ));
    assert_eq!(stdout.trim(), "ENOSYS|false|SIGKILL|shown");
}

/// A child that is not detached dies with its parent on Windows, as libuv's does (a job object).
#[cfg(windows)]
#[test]
fn a_child_dies_with_the_process_that_started_it() {
    use std::io::{BufRead, BufReader};
    let mut parent = common::command(
        &[
            "run",
            "-A",
            "-e",
            &format!(
                "const c = require('child_process').spawn({}, ['-e', 'setInterval(() => {{}}, 1000)']); console.log(c.pid); setInterval(() => {{}}, 1000);",
                verve_path()
            ),
        ],
        &std::env::temp_dir(),
    )
    .spawn()
    .unwrap();
    let mut line = String::new();
    BufReader::new(parent.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let pid = line.trim().to_owned();
    let alive = |pid: &str| {
        let probe = format!(
            "try {{ process.kill({pid}, 0); console.log('alive') }} catch (e) {{ console.log(e.code) }}"
        );
        output(&probe).trim() == "alive"
    };
    assert!(alive(&pid), "the child runs while its parent does");
    parent.kill().unwrap();
    let _ = parent.wait();
    let mut gone = false;
    for _ in 0..50 {
        if !alive(&pid) {
            gone = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(gone, "the child outlived its parent");
}

#[cfg(unix)]
#[test]
fn a_detached_child_leads_a_session_of_its_own() {
    let stdout = output(
        r#"
        const { spawn, execSync } = require('child_process');
        const sid = (pid) => execSync(`ps -o sid= -p ${pid}`).toString().trim();
        const normal = spawn('sleep', ['5']);
        const detached = spawn('sleep', ['5'], { detached: true });
        console.log(sid(normal.pid) === sid(process.pid), sid(detached.pid) === String(detached.pid));
        normal.kill(); detached.kill();
        "#,
    );
    assert_eq!(stdout.trim(), "true true");
}
