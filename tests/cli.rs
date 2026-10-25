//! `verve run` as a process: the built binary started as a child, as an embedder starts it —
//! its argv and environment, how and when it exits, its standard streams, and the permission
//! flags, Node's spellings beside Deno's.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const VERVE: &str = env!("CARGO_BIN_EXE_verve");

/// A folder of the test's own, removed when it is done.
struct Dir(PathBuf);

impl Dir {
    fn new(test: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("verve-cli-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // The real path: what the runtime resolves to, so grants and answers compare.
        Self(std::fs::canonicalize(&dir).map(unverbatim).unwrap_or(dir))
    }

    fn file(&self, name: &str, text: &str) -> PathBuf {
        let path = self.0.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, text).unwrap();
        path
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn unverbatim(path: PathBuf) -> PathBuf {
    match path.to_string_lossy().strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => path,
    }
}

struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
    took: Duration,
}

fn command(args: &[&str], cwd: &Path) -> Command {
    let mut command = Command::new(VERVE);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// Runs `verve` with `args` in `cwd`, `input` on its standard input (closed after it), and
/// waits for it — at most 60 seconds, after which it is killed and the test fails.
fn verve(args: &[&str], cwd: &Path, input: &[u8]) -> Ran {
    let started = Instant::now();
    let mut child = command(args, cwd).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let input = input.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let ran = wait(child, started);
    let _ = writer.join();
    ran
}

fn wait(mut child: Child, started: Instant) -> Ran {
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text
    });
    let err = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    let deadline = started + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("verve did not exit within 60 seconds");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let took = started.elapsed();
    Ran {
        code: status.code().unwrap_or(-1),
        stdout: out.join().unwrap(),
        stderr: err.join().unwrap(),
        took,
    }
}

fn script(dir: &Dir, name: &str, source: &str, flags: &[&str], args: &[&str]) -> Ran {
    let path = dir.file(name, source);
    let path = path.to_string_lossy().into_owned();
    let mut all: Vec<&str> = vec!["run"];
    all.extend(flags);
    all.push(&path);
    all.extend(args);
    verve(&all, &dir.0, b"")
}

fn eval(source: &str, flags: &[&str]) -> Ran {
    let mut all: Vec<&str> = flags.to_vec();
    all.push("-e");
    all.push(source);
    verve(&all, &std::env::temp_dir(), b"")
}

// ─── The process ────────────────────────────────────────────────────────────────────────────

#[test]
fn argv_env_and_versions_are_the_processs_own() {
    let dir = Dir::new("argv");
    let path = dir.file(
        "show.js",
        "console.log(JSON.stringify({
            argv: process.argv.slice(1), exec: process.argv[0] === process.execPath,
            argv0: process.argv0, env: process.env.VERVE_CLI_TEST, execArgv: process.execArgv,
            node: process.versions.node, verve: typeof process.versions.verve,
            electron: process.versions.electron, type: process.type, main: require.main === module,
            filename: __filename,
        }))",
    );
    let shown = path.to_string_lossy().into_owned();
    let mut child = command(&["run", "--allow-read", &shown, "a", "--b"], &dir.0);
    child.env("VERVE_CLI_TEST", "from the parent");
    let ran = wait(child.spawn().unwrap(), Instant::now());
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    let seen: serde_json::Value = serde_json::from_str(ran.stdout.trim()).unwrap();
    assert_eq!(seen["argv"], serde_json::json!([shown, "a", "--b"]));
    assert_eq!(seen["exec"], true);
    assert_eq!(seen["argv0"], VERVE);
    assert_eq!(seen["env"], "from the parent");
    assert_eq!(seen["execArgv"], serde_json::json!(["--allow-read"]));
    let major: u32 = seen["node"]
        .as_str()
        .unwrap()
        .split('.')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(major >= 22, "{seen}");
    assert_eq!(seen["verve"], "string");
    assert!(
        seen.get("electron").is_none() && seen.get("type").is_none(),
        "{seen}"
    );
    assert_eq!(seen["main"], true);
    assert_eq!(seen["filename"], shown.as_str());
}

#[test]
fn the_version_and_bad_flags() {
    let ran = verve(&["--version"], &std::env::temp_dir(), b"");
    assert_eq!(ran.code, 0);
    assert!(ran.stdout.starts_with("verve "), "{}", ran.stdout);
    let ran = verve(&["run", "--frobnicate", "x.js"], &std::env::temp_dir(), b"");
    assert_eq!(ran.code, 9);
    assert!(
        ran.stderr.contains("unknown flag --frobnicate"),
        "{}",
        ran.stderr
    );
    assert!(ran.stderr.contains("usage: verve run"), "{}", ran.stderr);
    let ran = verve(&["run"], &std::env::temp_dir(), b"");
    assert_eq!(ran.code, 9);
}

#[test]
fn exit_codes_are_nodes() {
    // Explicit exit: `exit` listeners told, the rest never runs.
    let ran = eval(
        "process.on('exit', (c) => console.log('exit', c)); setTimeout(() => console.log('never'), 10000); process.exit(5); console.log('after');",
        &[],
    );
    assert_eq!((ran.code, ran.stdout.as_str()), (5, "exit 5\n"));
    // `exitCode`, at the natural end.
    let ran = eval(
        "process.exitCode = 3; setTimeout(() => console.log('ran'), 20);",
        &[],
    );
    assert_eq!((ran.code, ran.stdout.as_str()), (3, "ran\n"));
    // An `exit` listener may change the code.
    let ran = eval("process.on('exit', () => { process.exitCode = 4 });", &[]);
    assert_eq!(ran.code, 4);
    // Nothing to do at all.
    let ran = eval("", &[]);
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (0, "", "")
    );
    // An uncaught exception, at once or later.
    let ran = eval("throw new TypeError('at once')", &[]);
    assert_eq!(ran.code, 1);
    assert!(ran.stderr.contains("TypeError: at once"), "{}", ran.stderr);
    let ran = eval("setTimeout(() => { throw new Error('later') }, 5)", &[]);
    assert_eq!(ran.code, 1);
    assert!(ran.stderr.contains("Error: later"), "{}", ran.stderr);
    // ... unless an `uncaughtException` listener hears it.
    let ran = eval(
        "process.on('uncaughtException', (e, origin) => console.log('heard', e.message, origin)); setTimeout(() => { throw new Error('x') }, 1); setTimeout(() => console.log('still running'), 20);",
        &[],
    );
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(ran.stdout, "heard x uncaughtException\nstill running\n");
    // An unhandled rejection is an uncaught exception, as Node's default mode has it.
    let ran = eval("Promise.reject(new Error('rejected'))", &[]);
    assert_eq!(ran.code, 1);
    assert!(ran.stderr.contains("Error: rejected"), "{}", ran.stderr);
    let ran = eval("(async () => { throw 'plain' })()", &[]);
    assert_eq!(ran.code, 1);
    assert!(
        ran.stderr.contains("ERR_UNHANDLED_REJECTION") && ran.stderr.contains("plain"),
        "{}",
        ran.stderr
    );
    // ... unless an `unhandledRejection` listener hears it.
    let ran = eval(
        "process.on('unhandledRejection', (r) => console.log('heard', r)); Promise.reject('r');",
        &[],
    );
    assert_eq!((ran.code, ran.stdout.as_str()), (0, "heard r\n"));
}

#[test]
fn before_exit_can_keep_the_process_going() {
    let ran = eval(
        "let n = 0; process.on('beforeExit', (code) => { console.log('beforeExit', code, n); if (n++ < 2) setTimeout(() => {}, 5); }); process.on('exit', (c) => console.log('exit', c));",
        &[],
    );
    assert_eq!(ran.code, 0);
    assert_eq!(
        ran.stdout,
        "beforeExit 0 0\nbeforeExit 0 1\nbeforeExit 0 2\nexit 0\n"
    );
}

#[test]
fn an_unsettled_top_level_await_exits_13() {
    let dir = Dir::new("tla");
    let ran = script(
        &dir,
        "wait.mjs",
        "console.log('waiting'); await new Promise(() => {});",
        &[],
        &[],
    );
    assert_eq!(ran.code, 13);
    assert_eq!(ran.stdout, "waiting\n");
    assert!(
        ran.stderr
            .contains("Warning: Detected unsettled top-level await at file:"),
        "{}",
        ran.stderr
    );
    // Settled by a timer: no warning, and the code is its.
    let ran = script(
        &dir,
        "settles.mjs",
        "await new Promise((r) => setTimeout(r, 20)); process.exitCode = 2;",
        &[],
        &[],
    );
    assert_eq!((ran.code, ran.stderr.as_str()), (2, ""));
    // A module that throws as it is evaluated.
    let ran = script(&dir, "throws.mjs", "throw new RangeError('top');", &[], &[]);
    assert_eq!(ran.code, 1);
    assert!(ran.stderr.contains("RangeError: top"), "{}", ran.stderr);
}

#[test]
fn unrefd_timers_keep_nothing_alive() {
    let ran = eval(
        "setTimeout(() => console.log('never'), 30000).unref(); setInterval(() => {}, 1000).unref(); console.log('done');",
        &[],
    );
    assert_eq!((ran.code, ran.stdout.as_str()), (0, "done\n"));
    assert!(ran.took < Duration::from_secs(10), "{:?}", ran.took);
    // `ref()` again keeps it waiting; an unref'd one still fires while something else runs.
    let ran = eval(
        "const t = setTimeout(() => console.log('reffed'), 50); t.unref(); t.ref(); setTimeout(() => console.log('unreffed'), 10).unref(); console.log(t.hasRef());",
        &[],
    );
    assert_eq!(ran.stdout, "true\nunreffed\nreffed\n");
}

#[test]
fn eval_is_node_minus_e() {
    let dir = Dir::new("eval");
    dir.file("helper.js", "module.exports = 'helped';");
    let ran = verve(
        &[
            "--allow-read",
            "-e",
            "console.log(__filename, __dirname, process.argv.length, process.argv.slice(1).join(','), require('./helper.js'), typeof module, typeof exports)",
            "x",
            "y",
        ],
        &dir.0,
        b"",
    );
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(ran.stdout, "[eval] . 3 x,y helped object object\n");
    // `verve eval` says the same, and needs no grant for itself.
    let ran = verve(&["eval", "console.log(1 + 1)"], &dir.0, b"");
    assert_eq!((ran.code, ran.stdout.as_str()), (0, "2\n"));
}

// ─── Standard streams ───────────────────────────────────────────────────────────────────────

#[test]
fn stdin_is_read_as_an_async_iterator_and_by_readline() {
    let input = "first line\nsecond line\nthird é line\n".repeat(200);
    let iterated = verve(
        &[
            "-e",
            "(async () => { const chunks = []; for await (const c of process.stdin) chunks.push(c); const all = Buffer.concat(chunks).toString(); console.log(all.length, all.split('\\n').length); })()",
        ],
        &std::env::temp_dir(),
        input.as_bytes(),
    );
    assert_eq!(iterated.code, 0, "{}", iterated.stderr);
    assert_eq!(
        iterated.stdout,
        format!("{} {}\n", input.chars().count(), 601)
    );
    let lines = verve(
        &[
            "-e",
            "const rl = require('readline').createInterface({ input: process.stdin }); let n = 0, last; rl.on('line', (l) => { n++; last = l; }); rl.on('close', () => console.log(n, last));",
        ],
        &std::env::temp_dir(),
        input.as_bytes(),
    );
    assert_eq!(lines.code, 0, "{}", lines.stderr);
    assert_eq!(lines.stdout, "600 third é line\n");
    // `data` events and `end`.
    let data = verve(
        &[
            "-e",
            "let n = 0; process.stdin.on('data', (c) => n += c.length); process.stdin.on('end', () => console.log('end', n));",
        ],
        &std::env::temp_dir(),
        b"12345",
    );
    assert_eq!(data.stdout, "end 5\n");
}

/// Input that stays open does not keep a process that stopped reading it: what Code Mode's
/// runner does, closing its readline once it has answered.
#[test]
fn input_left_open_keeps_nothing_alive_once_it_is_let_go() {
    let mut child = command(
        &[
            "-e",
            "const rl = require('readline').createInterface({ input: process.stdin }); rl.on('line', (l) => { console.log('got', l); rl.close(); });",
        ],
        &std::env::temp_dir(),
    )
    .spawn()
    .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"hello\n").unwrap();
    stdin.flush().unwrap();
    let started = Instant::now();
    let ran = wait(child, started);
    drop(stdin);
    assert_eq!((ran.code, ran.stdout.as_str()), (0, "got hello\n"));
    assert!(ran.took < Duration::from_secs(20), "{:?}", ran.took);
    // Input that is never read keeps nothing alive either.
    let mut child = command(&["-e", "console.log('not reading')"], &std::env::temp_dir())
        .spawn()
        .unwrap();
    let stdin = child.stdin.take().unwrap();
    let ran = wait(child, Instant::now());
    drop(stdin);
    assert_eq!(ran.code, 0);
}

#[test]
fn stdin_answers_as_it_arrives() {
    let mut child = command(
        &[
            "-e",
            "process.stdin.on('data', (c) => process.stdout.write('echo ' + c)); process.stdin.on('end', () => console.log('bye'));",
        ],
        &std::env::temp_dir(),
    )
    .spawn()
    .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    for n in 0..3 {
        stdin.write_all(format!("{n}\n").as_bytes()).unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        assert_eq!(line, format!("echo {n}\n"));
    }
    drop(stdin);
    let mut rest = String::new();
    stdout.read_to_string(&mut rest).unwrap();
    assert_eq!(rest, "bye\n");
    assert!(child.wait().unwrap().success());
}

#[test]
fn stdout_keeps_order_under_load_and_calls_back() {
    let ran = eval(
        "let called = 0; for (let i = 0; i < 10000; i++) { if (i % 1000 === 0) console.log('log', i); process.stdout.write('line ' + i + '\\n', () => called++); }
         process.stderr.write('to stderr\\n');
         const big = 'x'.repeat(100000) + '\\n';
         const accepted = process.stdout.write(big);
         process.stdout.once('drain', () => console.log('drained', accepted));
         process.on('exit', () => console.log('called', called));",
        &[],
    );
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(ran.stderr, "to stderr\n");
    let lines: Vec<&str> = ran.stdout.lines().collect();
    let mut expected = Vec::new();
    for i in 0..10000 {
        if i % 1000 == 0 {
            expected.push(format!("log {i}"));
        }
        expected.push(format!("line {i}"));
    }
    expected.push("x".repeat(100000));
    expected.push("drained false".into());
    expected.push("called 10000".into());
    assert_eq!(lines, expected);
}

// ─── Permissions ────────────────────────────────────────────────────────────────────────────

#[test]
fn files_need_a_grant_and_the_entry_is_granted() {
    let dir = Dir::new("grants");
    dir.file("data/in.txt", "inside");
    dir.file("lib/dep.js", "module.exports = 'dep';");
    let source = "const fs = require('fs'); const path = require('path');
        const at = (p) => path.join(__dirname, p);
        const tryIt = (f) => { try { return String(f()); } catch (e) { return e.code + ':' + (e.permission || ''); } };
        console.log(JSON.stringify([
            tryIt(() => fs.readFileSync(at('data/in.txt'), 'utf8')),
            tryIt(() => fs.writeFileSync(at('data/out.txt'), 'x')),
            tryIt(() => require('./lib/dep.js')),
            tryIt(() => process.permission.has('fs.read', at('data'))),
            tryIt(() => process.permission.has('fs.write', at('data'))),
            tryIt(() => process.permission.has('worker')),
        ]))";
    let data = dir.0.join("data").to_string_lossy().into_owned();
    let lib = dir.0.join("lib").to_string_lossy().into_owned();
    let none = script(&dir, "main.js", source, &["--permission"], &[]);
    assert_eq!(none.code, 0, "{}", none.stderr);
    assert_eq!(
        none.stdout.trim(),
        r#"["ERR_ACCESS_DENIED:FileSystemRead","ERR_ACCESS_DENIED:FileSystemWrite","ERR_ACCESS_DENIED:FileSystemRead","false","false","false"]"#
    );
    // Node's spellings, one path each.
    let node = script(
        &dir,
        "main.js",
        source,
        &[
            "--permission",
            &format!("--allow-fs-read={data}"),
            &format!("--allow-fs-read={lib}"),
            &format!("--allow-fs-write={data}"),
        ],
        &[],
    );
    assert_eq!(
        node.stdout.trim(),
        r#"["inside","undefined","dep","true","true","false"]"#,
        "{}",
        node.stderr
    );
    // Deno's: a list, and bare for everything.
    let deno = script(
        &dir,
        "main.js",
        source,
        &[&format!("--allow-read={data},{lib}"), "--allow-write"],
        &[],
    );
    assert_eq!(
        deno.stdout.trim(),
        r#"["inside","undefined","dep","true","true","false"]"#
    );
    let all = script(&dir, "main.js", source, &["-A"], &[]);
    assert_eq!(
        all.stdout.trim(),
        r#"["inside","undefined","dep","true","true","true"]"#
    );
    // A missing entry.
    let ran = verve(&["run", "nowhere.js"], &dir.0, b"");
    assert_eq!(ran.code, 1);
    assert!(ran.stderr.contains("Cannot find module"), "{}", ran.stderr);
}

#[test]
fn an_es_module_entrys_imports_need_a_grant() {
    let dir = Dir::new("esm-grants");
    dir.file("dep.mjs", "export const answer = 42;");
    let main = dir.file(
        "sub/main.mjs",
        "import { answer } from '../dep.mjs'; console.log(answer);",
    );
    let main = main.to_string_lossy().into_owned();
    let denied = verve(&["run", &main], &dir.0, b"");
    assert_eq!(denied.code, 1);
    assert!(
        denied.stderr.contains("ERR_ACCESS_DENIED"),
        "{}",
        denied.stderr
    );
    let root = dir.0.to_string_lossy().into_owned();
    let granted = verve(
        &["run", &format!("--allow-fs-read={root}"), &main],
        &dir.0,
        b"",
    );
    assert_eq!(
        (granted.code, granted.stdout.as_str()),
        (0, "42\n"),
        "{}",
        granted.stderr
    );
}

#[test]
fn workers_need_a_grant_and_keep_the_process_until_they_end() {
    let source = "try {
            const w = new Worker('data:text/javascript,setTimeout(() => postMessage(\"from worker\"), 50)');
            w.onmessage = (e) => { console.log(e.data); w.terminate(); };
        } catch (e) { console.log(e.code, e.permission); }";
    let denied = eval(source, &[]);
    assert_eq!(denied.stdout, "ERR_ACCESS_DENIED WorkerThreads\n");
    let granted = eval(source, &["--allow-worker"]);
    assert_eq!(
        (granted.code, granted.stdout.as_str()),
        (0, "from worker\n"),
        "{}",
        granted.stderr
    );
}

#[test]
fn net_flags_map_to_outbound_rules() {
    let has = "console.log(['net', 'net|127.0.0.1:8080', 'net|example.com:443', 'net|example.com'].map((s) => { const [scope, ref] = s.split('|'); return process.permission.has(scope, ref); }).join(' '))";
    assert_eq!(eval(has, &[]).stdout, "false false false false\n");
    assert_eq!(eval(has, &["--allow-net"]).stdout, "true true true true\n");
    assert_eq!(
        eval(has, &["--allow-net=127.0.0.1,example.com:443"]).stdout,
        "false true true false\n"
    );
}

#[test]
fn a_server_keeps_the_process_until_it_closes_and_nothing_polls_meanwhile() {
    let ran = eval(
        "const net = require('net');
         const server = net.createServer((socket) => { socket.end('hi'); });
         server.listen(0, '127.0.0.1', () => {
             const { port } = server.address();
             const client = net.connect(port, '127.0.0.1');
             let got = '';
             client.on('data', (d) => got += d);
             client.on('end', () => setTimeout(() => { console.log('got', got); server.close(); }, 300));
         });",
        &["--allow-net"],
    );
    assert_eq!(
        (ran.code, ran.stdout.as_str()),
        (0, "got hi\n"),
        "{}",
        ran.stderr
    );
    // An unref'd server keeps nothing alive.
    let ran = eval(
        "require('net').createServer().listen(0, '127.0.0.1').unref(); console.log('listening');",
        &["--allow-net"],
    );
    assert_eq!((ran.code, ran.stdout.as_str()), (0, "listening\n"));
    let denied = eval(
        "require('net').createServer().listen(0, '127.0.0.1').on('error', (e) => console.log(e.code));",
        &[],
    );
    assert_eq!(denied.stdout, "EACCES\n");
}

// ─── Node's other flags ─────────────────────────────────────────────────────────────────────

#[test]
fn disable_proto_throws_or_deletes() {
    let probe = "let r; try { r = typeof ({}).__proto__; } catch (e) { r = e.code; } console.log(r, Object.getPrototypeOf({ __proto__: Array.prototype }) === Array.prototype)";
    assert_eq!(eval(probe, &[]).stdout, "object true\n");
    assert_eq!(
        eval(probe, &["--disable-proto=throw"]).stdout,
        "ERR_PROTO_ACCESS true\n"
    );
    assert_eq!(
        eval(probe, &["--disable-proto=delete"]).stdout,
        "undefined true\n"
    );
    let bad = eval(probe, &["--disable-proto=maybe"]);
    assert_eq!(bad.code, 9);
}

#[test]
fn stack_size_sets_the_script_threads_stack() {
    // Recursion through a native function, so the stack runs out before Boa's frame count.
    let depth = "let n = 0; const f = () => { n++; [0].forEach(() => f()); }; try { f(); } catch (e) { console.log(e.name, n); }";
    let small = eval(depth, &["--stack-size=4096"]);
    let large = eval(depth, &["--stack_size=131072"]);
    assert_eq!(small.code, 0, "{}", small.stderr);
    assert_eq!(large.code, 0, "{}", large.stderr);
    let count = |ran: &Ran| -> u64 {
        let (name, n) = ran.stdout.trim().split_once(' ').unwrap();
        assert_eq!(name, "RangeError");
        n.parse().unwrap()
    };
    assert!(
        count(&large) > count(&small),
        "{} vs {}",
        small.stdout,
        large.stdout
    );
    assert_eq!(eval("0", &["--stack-size=lots"]).code, 9);
}

#[test]
fn no_warnings_and_the_flags_with_nothing_to_change() {
    let warn = "process.on('warning', (w) => console.log('heard', w.message)); process.emitWarning('careful');";
    let loud = eval(warn, &[]);
    assert!(loud.stderr.contains("Warning: careful"), "{}", loud.stderr);
    assert_eq!(loud.stdout, "heard careful\n");
    let quiet = eval(warn, &["--no-warnings"]);
    assert_eq!(
        (quiet.stderr.as_str(), quiet.stdout.as_str()),
        ("", "heard careful\n")
    );
    let accepted = eval(
        "console.log('fine')",
        &[
            "--experimental-vm-modules",
            "--no-addons",
            "--max-old-space-size=192",
            "--permission",
            "--experimental-permission",
        ],
    );
    assert_eq!((accepted.code, accepted.stdout.as_str()), (0, "fine\n"));
    assert_eq!(eval("0", &["--allow-child-process"]).code, 9);
}
