//! What the tests share: running an async body to its end on a test's clock, a folder of a
//! test's own, and reading the `result` a script left on its exports.

use std::cell::Cell;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

use boa_engine::JsValue;
use verve::Runtime;

/// Runs `body` — an async function's body, which sets `result` to an array — on a clock the
/// test moves, until it has set it; answers the array's items as strings.
#[allow(dead_code)] // Not every test file uses every helper.
pub fn run_async(body: &str) -> Vec<String> {
    let time = Rc::new(Cell::new(0.));
    let read = Rc::clone(&time);
    let mut rt = Runtime::with_clock(move || read.get());
    let source = format!(
        "globalThis.result = undefined; (async () => {{ {body}\n}})().catch(e => {{ globalThis.result = ['threw ' + (e && e.stack || e)]; }});"
    );
    let dir = std::env::temp_dir();
    rt.load_module(&dir.join("verve-streams-test.js"), &source)
        .unwrap_or_else(|e| panic!("{e}"));
    for _ in 0..200 {
        if rt
            .eval("globalThis.result !== undefined")
            .map(|v| v.to_boolean())
            .unwrap_or(false)
        {
            break;
        }
        time.set(time.get() + 10.);
        rt.run_due();
    }
    let text = rt
        .eval("JSON.stringify(globalThis.result)")
        .unwrap_or_else(|e| panic!("{e}"));
    let parsed: serde_json::Value = serde_json::from_str(&verve::print(&text))
        .unwrap_or_else(|_| panic!("no result: {}", verve::print(&text)));
    parsed
        .as_array()
        .unwrap_or_else(|| panic!("result is not an array: {parsed}"))
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| v.to_string())
        })
        .collect()
}

/// A folder of the test's own, removed when it is done; `.0` is its path.
#[allow(dead_code)]
pub struct Dir(pub PathBuf);

impl Dir {
    /// An empty folder for `test`, at its real path: what the runtime resolves to, so grants and
    /// answers compare, and without Windows' `\\?\` prefix, which a `file:` URL would keep.
    #[allow(dead_code)]
    pub fn new(test: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("verve-test-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let real = std::fs::canonicalize(&dir).unwrap_or(dir);
        Self(match real.to_string_lossy().strip_prefix(r"\\?\") {
            Some(rest) => PathBuf::from(rest),
            None => real,
        })
    }

    /// Writes `text` to `name` in the folder, making the folders on the way; answers its path.
    #[allow(dead_code)]
    pub fn file(&self, name: &str, text: &str) -> PathBuf {
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

/// The items of `exports.result`, an array a script set, as strings.
#[allow(dead_code)]
pub fn result_of(rt: &mut Runtime, exports: JsValue) -> Vec<String> {
    let json = rt.eval("JSON.stringify").unwrap();
    let text = rt
        .call(&json, &JsValue::undefined(), &[exports])
        .unwrap_or_else(|e| panic!("{e}"));
    let parsed: serde_json::Value = serde_json::from_str(&verve::print(&text)).unwrap();
    parsed["result"]
        .as_array()
        .unwrap_or_else(|| panic!("no result in {parsed}"))
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| v.to_string())
        })
        .collect()
}

// ─── The binary, run as a process ───────────────────────────────────────────────────────────

#[allow(dead_code)] // Not every test file uses every helper.
pub const VERVE: &str = env!("CARGO_BIN_EXE_verve");

#[allow(dead_code)] // Not every test file uses every helper.
pub struct Ran {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    #[allow(dead_code)]
    pub took: Duration,
}

#[allow(dead_code)] // Not every test file uses every helper.
pub fn command(args: &[&str], cwd: &Path) -> Command {
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
#[allow(dead_code)] // Not every test file uses every helper.
pub fn verve(args: &[&str], cwd: &Path, input: &[u8]) -> Ran {
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

#[allow(dead_code)] // Not every test file uses every helper.
pub fn wait(mut child: Child, started: Instant) -> Ran {
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
