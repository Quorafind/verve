//! The `images` fixture (`tests/electron/apps/images`) drawn by the `electron` binary, against the
//! picture Chromium made of it (`expected-chromium.png`, written by Electron 39.8.10 on Windows
//! with `capturePage()` at a device scale factor of 1: `VERVE_IMAGES_PNG=<path> electron
//! tests/electron/apps/images`), and the memory of a page of 200 one-megapixel pictures that
//! scrolls. Both open real windows, so both are `#[ignore]`d; run them with `--ignored`.
//!
//! The picture `--screenshot` writes is the display list drawn by the software painter, not what
//! the window shows. The animated GIF is left out of the comparison: the capture (2.5 s after load,
//! in a window never shown) caught Chromium on the GIF's first frame, and Verve's ends on its last.
//! (Hidden windows that are painted do animate; see `p35-review3/gif/out.json`.)
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tiny_skia::Pixmap;

mod support;
use support::kill_by_pid;

const EXPECTED: &[u8] = include_bytes!("../../../tests/electron/apps/images/expected-chromium.png");

/// The size of the window the picture was taken of, in CSS pixels.
const SIZE: (u32, u32) = (480, 330);

/// Where the animated GIF is: not compared.
const ANIMATION: (u32, u32, u32, u32) = (232, 10, 64, 64);

/// The share of the picture that may differ by more than `EDGE` in a channel. The acceptance is
/// under 3%.
const SHARE: f64 = 0.03;
const EDGE: i16 = 48;

/// What the working set of the page of 200 pictures may reach, and what it must be under once
/// the pictures have scrolled away, in bytes.
const PEAK_LIMIT: u64 = 400 << 20;

fn pixel(picture: &Pixmap, x: u32, y: u32) -> [i16; 4] {
    let pixel = picture.pixel(x, y).expect("a pixel in the picture");
    [pixel.red(), pixel.green(), pixel.blue(), pixel.alpha()].map(i16::from)
}

/// How many pixels of `got` differ from those of `want` by more than `tolerance` in a channel,
/// outside the animation.
fn differences(got: &Pixmap, want: &Pixmap, tolerance: i16) -> usize {
    assert_eq!(
        (got.width(), got.height()),
        (want.width(), want.height()),
        "the pictures are the same size"
    );
    let mut count = 0;
    for y in 0..got.height() {
        for x in 0..got.width() {
            let (left, top, width, height) = ANIMATION;
            if (left..left + width).contains(&x) && (top..top + height).contains(&y) {
                continue;
            }
            let (a, b) = (pixel(got, x, y), pixel(want, x, y));
            count += usize::from((0..3).any(|i| (a[i] - b[i]).abs() > tolerance));
        }
    }
    count
}

/// The parts of the page (`tests/electron/apps/images/index.html`): name, left, top, width, height
/// in CSS pixels. The animated GIF is not one.
const PARTS: &[(&str, u32, u32, u32, u32)] = &[
    ("PNG", 10, 10, 64, 64),
    ("JPEG", 84, 10, 64, 64),
    ("WebP", 158, 10, 64, 64),
    ("icon 1", 10, 90, 32, 32),
    ("icon 2", 50, 90, 32, 32),
    ("icon 3", 90, 90, 32, 32),
    ("icon 4", 130, 90, 32, 32),
    ("icon 5", 170, 90, 32, 32),
    ("icon 6", 210, 90, 32, 32),
    ("SVG in an img", 250, 90, 32, 32),
    ("SVG as a background", 290, 90, 32, 32),
    ("tiles", 10, 140, 200, 80),
    ("cover", 220, 140, 124, 84),
    ("SVG tiles", 350, 140, 100, 80),
    ("object-fit: cover", 10, 236, 100, 40),
    ("object-fit: contain", 120, 236, 100, 60),
    ("canvas clock", 240, 230, 90, 90),
];

/// The share of a part's own pixels that may differ.
const PART_SHARE: f64 = 0.05;

/// In `area`: how many pixels differ by more than `EDGE`, how many are not white in `got`, and
/// how many are not white in `want`.
fn compare_part(
    got: &Pixmap,
    want: &Pixmap,
    (left, top, width, height): (u32, u32, u32, u32),
) -> (usize, usize, usize) {
    let (mut count, mut inked_got, mut inked_want) = (0, 0, 0);
    let inked = |p: [i16; 4]| (0..3).any(|i| 255 - p[i] > EDGE);
    for y in top..top + height {
        for x in left..left + width {
            let (a, b) = (pixel(got, x, y), pixel(want, x, y));
            count += usize::from((0..3).any(|i| (a[i] - b[i]).abs() > EDGE));
            inked_got += usize::from(inked(a));
            inked_want += usize::from(inked(b));
        }
    }
    (count, inked_got, inked_want)
}

/// Waits for `child` for `seconds`, then kills it (by its process id).
fn wait_or_kill(child: &mut std::process::Child, seconds: u64) -> Option<i32> {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    loop {
        match child.try_wait().expect("the child can be asked") {
            Some(status) => return status.code(),
            None if Instant::now() >= deadline => {
                kill_by_pid(child.id());
                let _ = child.wait();
                return None;
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

#[test]
#[ignore = "opens real windows"]
fn the_picture_the_electron_binary_writes_of_the_images_page_is_close_to_chromiums() {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("verve-gpui-images-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a directory for the picture");
    let picture = dir.join("images.png");
    let app = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/electron/apps/images");
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
    let code = wait_or_kill(&mut child, 90);
    assert_eq!(
        code,
        Some(0),
        "the binary ends once it has taken the picture"
    );
    let bytes = std::fs::read(&picture).expect("the picture was written");
    let got = Pixmap::decode_png(&bytes).expect("a PNG");
    let want = Pixmap::decode_png(EXPECTED).expect("expected-chromium.png is a PNG");
    let count = differences(&got, &want, EDGE);
    let share = count as f64 / f64::from(SIZE.0 * SIZE.1);
    // Each part of the page on its own: a part that is missing is a few percent of the window
    // but all of its own rectangle.
    let missing: Vec<String> = PARTS
        .iter()
        .filter_map(|(name, left, top, width, height)| {
            let area = (*left, *top, *width, *height);
            let (count, inked_got, inked_want) = compare_part(&got, &want, area);
            let share = count as f64 / f64::from(width * height);
            let present = inked_got as f64 >= inked_want as f64 * 0.8;
            (share >= PART_SHARE || !present).then(|| {
                format!(
                    "{name}: {:.1}% of its pixels differ, {inked_got} drawn of {inked_want}",
                    share * 100.
                )
            })
        })
        .collect();
    assert!(
        missing.is_empty(),
        "{}",
        missing.join(
            "
"
        )
    );
    // Kept where a person can look at it when it fails.
    if share >= SHARE {
        let _ = std::fs::copy(&picture, std::env::temp_dir().join("verve-images-got.png"));
    } else {
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert!(
        share < SHARE,
        "{count} pixels ({:.2}%) differ by more than {EDGE}: see verve-images-got.png in the temp folder",
        share * 100.
    );
}

/// The working set of the process `id`, in bytes, from PowerShell (the test has no Windows API
/// of its own).
fn working_set(id: u32) -> Option<u64> {
    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!("(Get-Process -Id {id} -ErrorAction Stop).WorkingSet64"),
        ])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

/// Writes `count` pictures of one megapixel each (different from one another, and flat enough to
/// compress) and a page that shows them one under the other, each at 500 x 500, and scrolls it.
fn write_page(dir: &Path, count: u32) {
    for number in 0..count {
        let image = image::RgbImage::from_fn(1000, 1000, |x, y| {
            let band = ((x / 50 + y / 50 + number) % 7) as u8 * 30;
            image::Rgb([band, (number % 256) as u8, (x / 8) as u8])
        });
        image
            .save(dir.join(format!("p{number}.png")))
            .expect("a picture is written");
    }
    let pictures: String = (0..count)
        .map(|number| format!("<img src=\"p{number}.png\">"))
        .collect();
    let page = format!(
        "<!doctype html><style>body{{margin:0}} img{{display:block;width:500px;height:500px}}</style>{pictures}\
         <script>let at = 0; const step = setInterval(() => {{ at += 1500; window.scrollTo(0, at); if (at >= 100000) clearInterval(step); }}, 100);</script>"
    );
    std::fs::write(dir.join("index.html"), page).expect("the page is written");
    std::fs::write(
        dir.join("package.json"),
        r#"{"name":"images-200","main":"main.js"}"#,
    )
    .expect("package.json is written");
    std::fs::write(
        dir.join("main.js"),
        "const { app, BrowserWindow } = require('electron'); const fs = require('fs'); const path = require('path'); app.whenReady().then(() => { const win = new BrowserWindow({ width: 800, height: 600, title: 'images-200' }); win.loadFile('index.html'); setTimeout(async () => { const seen = await win.webContents.executeJavaScript('JSON.stringify({ loaded: [...document.images].filter((i) => i.complete && i.naturalWidth > 0).length, scrolled: window.scrollY })'); fs.writeFileSync(path.join(__dirname, 'result.json'), seen); }, 15000); });",
    )
    .expect("main.js is written");
}

#[test]
#[ignore = "opens real windows"]
fn two_hundred_megapixel_pictures_that_scroll_stay_under_400_mb_and_fall_back() {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("verve-gpui-images-200-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a directory for the page");
    write_page(&dir, 200);
    let mut child = Command::new(env!("CARGO_BIN_EXE_electron"))
        .arg(&dir)
        .arg(format!("--user-data-dir={}", dir.join("profile").display()))
        .env("VERVE_NO_SYSTEM_DIALOGS", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("the electron binary starts");
    // The page scrolls 1500 px every 100 ms through 100000 px of pictures and stops at the end, after
    // about seven seconds. (A page that keeps calling `scrollTo` is never still, so the pictures
    // far from the window are kept within the budgets instead of let go.) The working set is read
    // every second for twenty.
    let mut samples = Vec::new();
    for _ in 0..20 {
        std::thread::sleep(Duration::from_secs(1));
        samples.extend(working_set(child.id()));
    }
    kill_by_pid(child.id());
    let _ = child.wait();
    let seen = std::fs::read_to_string(dir.join("result.json")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    // The page loaded its pictures and scrolled to the end of them.
    assert!(seen.contains("\"loaded\":200"), "{seen}");
    let scrolled: f64 = seen
        .split("\"scrolled\":")
        .nth(1)
        .and_then(|rest| rest.trim_end_matches('}').parse().ok())
        .unwrap_or(0.);
    assert!(scrolled > 90_000., "{seen}");
    let megabytes: Vec<u64> = samples.iter().map(|bytes| bytes >> 20).collect();
    eprintln!("working set (MB), once a second: {megabytes:?}");
    let peak = samples.iter().copied().max().expect("samples were taken");
    assert!(
        peak < PEAK_LIMIT,
        "the working set reached {} MB",
        peak >> 20
    );
    // It rose with the pictures and fell once they were far above the window: the scroll ends at
    // about seven seconds and the lists then stand still for a second. Measured by hand (on
    // the same page): 229 MB at the first second (the first sample depends on how far the page has
    // loaded), 317 at the peak, 114 from the ninth second on; the same page with grey boxes in
    // place of the pictures stays at 67.
    let after = samples[10..]
        .iter()
        .copied()
        .min()
        .expect("samples after the scroll");
    assert!(
        peak >= after + (80 << 20),
        "the pictures were not decoded and let go: {megabytes:?}"
    );
}

/// A page with a GIF that never stops (loop count 0, two frames of 100 ms) is still, for a
/// screenshot, when only its frames move.
#[test]
#[ignore = "opens real windows"]
fn a_screenshot_is_taken_of_a_page_with_an_animation_that_never_ends() {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("verve-gpui-images-loop-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a directory for the page");
    let gif = "data:image/gif;base64,R0lGODlhAQABAIAAAP8AAAAA/yH/C05FVFNDQVBFMi4wAwEAAAAh+QQACgAAACwAAAAAAQABAAACAkQBACH5BAAKAAAALAAAAAABAAEAAAICTAEAOw==";
    std::fs::write(
        dir.join("index.html"),
        format!("<body style=\"margin:0\"><img src=\"{gif}\" style=\"width:20px;height:20px\">"),
    )
    .unwrap();
    std::fs::write(
        dir.join("package.json"),
        r#"{"name":"loop","main":"main.js"}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.js"),
        "const { app, BrowserWindow } = require('electron'); app.whenReady().then(() => { new BrowserWindow({ width: 100, height: 100 }).loadFile('index.html'); });",
    )
    .unwrap();
    let picture = dir.join("loop.png");
    let mut child = Command::new(env!("CARGO_BIN_EXE_electron"))
        .arg("--screenshot")
        .arg(&picture)
        .arg(&dir)
        .arg(format!("--user-data-dir={}", dir.join("profile").display()))
        .env("VERVE_NO_SYSTEM_DIALOGS", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("the electron binary starts");
    // Well under the 30 seconds the capture gives a page that keeps changing.
    let code = wait_or_kill(&mut child, 20);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(code, Some(0), "the picture was taken");
}
