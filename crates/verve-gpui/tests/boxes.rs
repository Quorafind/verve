//! The `boxes` fixture (`tests/electron/apps/boxes`) laid out and drawn by Verve, against the
//! picture Chromium made of it: `expected-chromium.png`, written by Electron 39.8.10 on Windows
//! with `capturePage()` at a device scale factor of 1 (`VERVE_BOXES_PNG=<path> electron
//! tests/electron/apps/boxes`).
//!
//! The first test needs no window: it lays the page out with `verve-layout` and draws the display
//! list with the software painter that `--screenshot` uses. The second runs the `electron` binary
//! with `--screenshot` on a real window and is `#[ignore]`d for that. The picture `--screenshot`
//! writes is the display list drawn by that software painter, not what the window shows: the
//! GPUI path itself has no pixel test.
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tiny_skia::Pixmap;
use verve_dom::{Options, html};
use verve_gpui::screenshot;
use verve_layout::{DisplayList, HeadlessMeasurer, LayoutEngine};

const PAGE: &str = include_str!("../../../tests/electron/apps/boxes/index.html");
const EXPECTED: &[u8] = include_bytes!("../../../tests/electron/apps/boxes/expected-chromium.png");

/// The size of the window the picture was taken of, in CSS pixels.
const SIZE: (u32, u32) = (400, 300);

/// What one channel of a pixel may differ by for the color to be the same: Chromium and the
/// software painter round differently.
const CHANNEL: i16 = 8;

/// The share of the picture that may differ by more than `EDGE` in a channel: its edges
/// (anti-aliasing, the blur of the shadow, the gradients). The acceptance is under 3%.
const EDGE_SHARE: f64 = 0.03;
const EDGE: i16 = 48;

/// How long a frame may take to build at the 95th percentile, in milliseconds (release build).
const FRAME_P95_MS: f32 = 4.;

/// Runs `f` on a thread with a stack Boa can recurse in, as Verve's own isolates do.
fn on_page<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> R {
    const STACK: usize = 64 << 20;
    std::thread::Builder::new()
        .stack_size(STACK)
        .spawn(move || {
            verve::set_thread_stack(STACK);
            f()
        })
        .expect("a thread")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

/// The display list of `markup` in a window of `SIZE`.
fn list_of(markup: &'static str) -> Arc<DisplayList> {
    on_page(move || {
        let mut runtime = verve::Runtime::new();
        let options = Options {
            viewport: (SIZE.0 as f32, SIZE.1 as f32),
            ..Options::default()
        };
        let document = verve_dom::install(&mut runtime, options).expect("a document");
        let (root, body) = (document.document_node(), document.body());
        verve_dom::host::with_dom(runtime.context(), |dom| {
            dom.empty(body);
            html::parse_document(dom, root, markup);
        });
        let update = document.update();
        let mut engine = LayoutEngine::new(Arc::new(HeadlessMeasurer));
        engine.update(&update.snapshot, &update.changes);
        engine.display_list()
    })
}

fn pixel(picture: &Pixmap, x: u32, y: u32) -> [i16; 4] {
    let pixel = picture.pixel(x, y).expect("a pixel in the picture");
    [pixel.red(), pixel.green(), pixel.blue(), pixel.alpha()].map(i16::from)
}

/// How many pixels of `got` are not those of `want`, and the worst difference, over the picture.
fn differences(got: &Pixmap, want: &Pixmap, tolerance: i16) -> (usize, i16) {
    assert_eq!(
        (got.width(), got.height()),
        (want.width(), want.height()),
        "the pictures are the same size"
    );
    let (mut count, mut worst) = (0, 0);
    for y in 0..got.height() {
        for x in 0..got.width() {
            let (a, b) = (pixel(got, x, y), pixel(want, x, y));
            let most = (0..3).map(|i| (a[i] - b[i]).abs()).max().unwrap_or(0);
            worst = worst.max(most);
            count += usize::from(most > tolerance);
        }
    }
    (count, worst)
}

/// Where the fixture's boxes are plain colors, away from every edge: the layout puts them where
/// Chromium did if the picture has Chromium's color here.
const PROBES: &[(&str, u32, u32)] = &[
    ("plain", 50, 40),
    ("border", 103, 40),
    ("inside the border", 140, 40),
    ("round", 230, 40),
    ("under", 30, 120),
    ("under and over", 80, 120),
    ("over", 150, 155),
    ("z-index 1", 185, 85),
    ("z-index 0", 200, 175),
    ("z-index 2", 260, 160),
    // Where two of the three overlap, and the markup order is not the z order.
    ("z-index 2 over 1", 220, 120),
    ("z-index 1 over 0", 200, 120),
    ("shadow's box", 335, 115),
    ("flex, first", 30, 205),
    ("flex, the one that grows", 120, 205),
    ("flex, aligned to the end", 190, 215),
    ("flex, its padding", 12, 183),
    ("grid, first column", 240, 205),
    ("grid, the gap", 262, 205),
    ("grid, second column", 330, 205),
    ("clip, inside", 30, 265),
    ("clip, the clipped box", 90, 265),
    ("clip, outside it", 120, 265),
    ("linear gradient, left", 285, 40),
    ("linear gradient, middle", 335, 40),
    ("linear gradient, right", 385, 40),
    ("radial gradient, centre", 170, 265),
    ("radial gradient, off centre", 150, 265),
    ("radial gradient, near the corner", 135, 245),
    ("pill", 310, 265),
    ("pill's border", 310, 242),
    ("the page", 5, 295),
];

fn expected() -> Pixmap {
    Pixmap::decode_png(EXPECTED).expect("expected-chromium.png is a PNG")
}

#[test]
fn the_boxes_page_is_where_and_what_color_chromium_drew_it() {
    let list = list_of(PAGE);
    let got = screenshot::render(&list, SIZE, Some([1.; 4]), &|_| None).expect("a picture");
    let want = expected();
    let mut wrong = Vec::new();
    for (what, x, y) in PROBES {
        let (a, b) = (pixel(&got, *x, *y), pixel(&want, *x, *y));
        if (0..3).any(|i| (a[i] - b[i]).abs() > CHANNEL) {
            wrong.push(format!("{what} at ({x}, {y}): drew {a:?}, Chromium {b:?}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    // The edges (anti-aliasing, the blur of the shadow, the gradients) differ a little: a few
    // percent of the picture at most.
    let (count, worst) = differences(&got, &want, EDGE);
    let share = count as f64 / f64::from(SIZE.0 * SIZE.1);
    assert!(
        share < EDGE_SHARE,
        "{count} pixels ({:.1}%) differ by more than {EDGE} (the worst by {worst})",
        share * 100.
    );
}

#[test]
#[ignore = "opens real windows"]
fn the_picture_the_electron_binary_writes_of_the_boxes_page_is_close_to_chromiums() {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("verve-gpui-boxes-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a directory for the picture");
    let picture = dir.join("boxes.png");
    let app = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/electron/apps/boxes");
    let mut child = Command::new(env!("CARGO_BIN_EXE_electron"))
        .arg("--screenshot")
        .arg(&picture)
        .arg(&app)
        .arg(format!("--user-data-dir={}", dir.join("profile").display()))
        .env("VERVE_NO_SYSTEM_DIALOGS", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("the electron binary starts");
    let deadline = Instant::now() + Duration::from_secs(90);
    let code = loop {
        match child.try_wait().expect("the child can be asked") {
            Some(status) => break status.code(),
            None if Instant::now() >= deadline => {
                // Only the process this test started.
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    };
    assert_eq!(
        code,
        Some(0),
        "the binary ends once it has taken the picture"
    );
    let bytes = std::fs::read(&picture).expect("the picture was written");
    let got = Pixmap::decode_png(&bytes).expect("a PNG");
    let (count, worst) = differences(&got, &expected(), EDGE);
    let share = count as f64 / f64::from(SIZE.0 * SIZE.1);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        share < EDGE_SHARE,
        "{count} pixels ({:.1}%) differ by more than {EDGE} (the worst by {worst})",
        share * 100.
    );
}

/// The 2000-box page (`tests/electron/apps/boxes-2000`), drawn frame after frame in a real window:
/// the binary logs how long a frame took to build (`VERVE_FRAME_STATS`), from the start of the
/// page's prepaint to the end of its paint. The 95th percentile has to be under `FRAME_P95_MS`.
/// Meant for a release build.
#[test]
#[ignore = "opens real windows"]
fn two_thousand_boxes_are_replayed_within_a_frame() {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("verve-gpui-boxes-2000-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a directory for the profile");
    let app =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/electron/apps/boxes-2000");
    let child = Command::new(env!("CARGO_BIN_EXE_electron"))
        .arg(&app)
        .arg(format!("--user-data-dir={}", dir.join("profile").display()))
        .env("VERVE_NO_SYSTEM_DIALOGS", "1")
        .env("VERVE_FRAME_STATS", "1")
        .env("VERVE_BOXES_MS", "10000")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the electron binary starts");
    let id = child.id();
    // The binary ends itself; a watchdog kills this one process if it does not.
    let (done, wait) = std::sync::mpsc::channel();
    let watchdog = std::thread::spawn(move || {
        if wait.recv_timeout(Duration::from_secs(90)).is_err() {
            kill_by_pid(id);
        }
    });
    let output = child.wait_with_output().expect("the binary ends");
    let _ = done.send(());
    let _ = watchdog.join();
    let _ = std::fs::remove_dir_all(&dir);
    let log = String::from_utf8_lossy(&output.stderr);
    let lines: Vec<&str> = log
        .lines()
        .filter(|line| line.starts_with("frames:"))
        .collect();
    eprintln!("{}", lines.join("\n"));
    assert!(!lines.is_empty(), "no frame stats in:\n{log}");
    for line in lines {
        // `frames: 240 built p50 0.31 ms p95 0.52 ms, apart ...`
        let built_p95: f32 = line
            .split("built p50")
            .nth(1)
            .and_then(|rest| rest.split("p95 ").nth(1))
            .and_then(|rest| rest.split(' ').next())
            .and_then(|number| number.parse().ok())
            .expect("a p95 in the line");
        assert!(
            built_p95 < FRAME_P95_MS,
            "a frame took {built_p95} ms at the 95th percentile: {line}"
        );
    }
}

/// Stops the process `id`, which this test started, and no other.
fn kill_by_pid(id: u32) {
    #[cfg(windows)]
    let (program, args) = (
        "taskkill",
        vec!["/PID".to_owned(), id.to_string(), "/F".to_owned()],
    );
    #[cfg(not(windows))]
    let (program, args) = ("kill", vec!["-9".to_owned(), id.to_string()]);
    let _ = Command::new(program).args(args).output();
}
