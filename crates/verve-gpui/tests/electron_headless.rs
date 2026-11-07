//! `electron --headless`: the same binary on a shell with no display, which opens no window and
//! prints what the app's windows would do. It runs anywhere, a display or not.

mod support;

/// Runs `main_js` as an app with `--headless` and answers its exit code and standard error.
fn run_headless(name: &str, main_js: &str) -> (Option<i32>, String) {
    let output = support::run_headless(name, main_js, &[], &[], &[]);
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn the_windows_of_an_app_are_printed_and_its_exit_code_is_the_processs() {
    let (code, stderr) = run_headless(
        "windows",
        r#"
        const { BrowserWindow } = require('electron');
        const win = new BrowserWindow({ width: 640, height: 480, title: 'traced' });
        win.on('closed', () => process.exit(6));
        setTimeout(() => win.close(), 50);
        "#,
    );
    assert_eq!(code, Some(6));
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(
        lines[0], r#"[window 1] create 640x480 "traced""#,
        "{stderr}"
    );
    assert!(lines.contains(&"[window 1] Close"), "{stderr}");
    assert_eq!(lines.last(), Some(&"[app] quit 6"), "{stderr}");
}
