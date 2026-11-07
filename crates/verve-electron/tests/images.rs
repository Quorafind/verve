//! Pictures in a page, headless: an `<img>` loads and reports its size, `load` and `error` are
//! heard, `decode()` settles, the display list names the decoded picture with the rectangle
//! `object-fit` gives it, a `background-image` is tiled, a `<video>` fails with code 4 and its
//! poster is drawn like an `<img>`.
//!
//! The pictures are `data:` PNGs. Sizes that Chromium decides were observed in Electron 39.8.10
//! (Chromium 142, `show: false`, 800 x 600, `--force-device-scale-factor=1`).
#![allow(clippy::unwrap_used)]

mod support;

use support::{App, TIMEOUT, run};
use verve_layout::{DisplayList, Fill, ImageTable, PaintItem, key_for_url};

const P4X2: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAQAAAACCAYAAAB/qH1jAAAAEklEQVR4nGP4z8DwHxkzoAsAAA8hD/EEN8afAAAAAElFTkSuQmCC";
const P40X20: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAACgAAAAUCAYAAAD/Rn+7AAAAL0lEQVR4nO3OMQ0AAAjAsPk3DSrIOHb0LwPzGXagYEE7ULCgHShY0A4ULGgHCl5biXs6G0TdvUkAAAAASUVORK5CYII=";
const P20X10: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAABQAAAAKCAYAAAC0VX7mAAAAFklEQVR4nGNgYPj/n7p41MBRA4ehgQDwto6AqIZRcgAAAABJRU5ErkJggg==";

/// A page `win` shows, from the main process, in a 400 x 300 window titled `p`.
fn app_with(html: &str) -> App {
    let html = serde_json::to_string(&format!(
        "<style>html,body{{margin:0}} img{{display:block}}</style>{html}"
    ))
    .unwrap();
    App::launch_script(
        &format!(
            r#"const {{ app, BrowserWindow }} = require('electron');
app.on('window-all-closed', () => {{}});
app.whenReady().then(() => {{
  const win = new BrowserWindow({{ title: 'p', show: false, width: 400, height: 300 }});
  win.loadURL('data:text/html,' + encodeURIComponent({html}));
}});"#
        ),
        &[],
    )
}

/// The display list and the pictures it names, once the page has published a list with `want`.
fn list_with(
    app: &App,
    want: impl Fn(&DisplayList, &ImageTable) -> bool,
) -> (std::sync::Arc<DisplayList>, std::sync::Arc<ImageTable>) {
    let window = app.window("p").expect("the window");
    let ready = |app: &App| {
        let seen = app.shell.display_list(window.id)?;
        want(&seen.0, &seen.1).then_some(seen)
    };
    assert!(
        app.run_until(|app| ready(app).is_some(), TIMEOUT),
        "the page never published what was wanted"
    );
    ready(app).expect("it was published")
}

fn images_of(
    list: &DisplayList,
) -> Vec<(verve_dom::Rect, verve_dom::Rect, verve_layout::ImageKey)> {
    list.items
        .iter()
        .filter_map(|item| match item {
            PaintItem::Image {
                bounds,
                clip,
                image,
                ..
            } => Some((*bounds, *clip, *image)),
            _ => None,
        })
        .collect()
}

fn rounded(rect: verve_dom::Rect) -> [i32; 4] {
    [rect.x, rect.y, rect.width, rect.height].map(|v| v.round() as i32)
}

#[test]
fn an_img_loads_reports_its_size_and_decodes() {
    let lines = run(&format!(
        r#"
    const win = new BrowserWindow({{ show: false, width: 400, height: 300 }});
    const wc = win.webContents;
    await load(win, '<img id="a" src="{P4X2}"><img id="b" src="data:image/png;base64,AAAA">');
    log(await wc.executeJavaScript(`(async () => {{
      const a = document.getElementById('a');
      await a.decode();
      const b = document.getElementById('b');
      let rejected = 'resolved';
      try {{ await b.decode(); }} catch (error) {{ rejected = error.name; }}
      return [a.complete, a.naturalWidth, a.naturalHeight, a.width, a.height, b.complete, b.naturalWidth, rejected].join(' ');
    }})()`));
    "#
    ));
    assert_eq!(lines, ["true 4 2 4 2 true 0 EncodingError"]);
}

#[test]
fn load_and_error_are_heard_by_a_handler_set_with_the_src() {
    let lines = run(&format!(
        r#"
    const win = new BrowserWindow({{ show: false, width: 400, height: 300 }});
    const wc = win.webContents;
    await load(win, '<body>');
    log(await wc.executeJavaScript(`new Promise((resolve) => {{
      const heard = [];
      const make = (name, src) => {{
        const img = new Image();
        img.onload = () => heard.push(name + ':load');
        img.onerror = () => heard.push(name + ':error');
        img.src = src;
      }};
      make('good', '{P4X2}');
      make('bad', 'data:image/png;base64,AAAA');
      setTimeout(() => resolve(heard.sort().join(' ')), 300);
    }})`));
    "#
    ));
    assert_eq!(lines, ["bad:error good:load"]);
}

#[test]
fn the_picture_is_decoded_and_the_list_places_it_as_object_fit_says() {
    let app = app_with(&format!(
        r#"<img src="{P40X20}" style="width:100px;height:100px;object-fit:contain">
           <img src="{P20X10}" style="width:100px;height:100px;object-fit:cover;object-position:0 0">"#
    ));
    let key = key_for_url(P40X20);
    let (list, table) = list_with(&app, |list, table| {
        images_of(list).len() == 2 && table.get(key).is_some()
    });
    let pictures = images_of(&list);
    // Chromium painted a 40x20 picture, `contain` in 100x100, at (0, 25) 100 x 50.
    assert_eq!(rounded(pictures[0].0), [0, 25, 100, 50]);
    assert_eq!(rounded(pictures[0].1), [0, 0, 100, 100]);
    assert_eq!(pictures[0].2, key);
    // `cover` at the top left: the 2:1 picture is 200x100, cut to the box.
    assert_eq!(rounded(pictures[1].0), [0, 100, 200, 100]);
    assert_eq!(rounded(pictures[1].1), [0, 100, 100, 100]);
    let decoded = table.get(key).unwrap();
    assert_eq!((decoded.width, decoded.height), (40, 20));
    // Red and opaque, as BGRA.
    assert_eq!(&decoded.frames[0].0[..4], &[0, 0, 255, 255]);
}

#[test]
fn a_background_image_is_a_tiled_picture_layer() {
    let app = app_with(&format!(
        r#"<div style="width:100px;height:50px;background:url({P40X20})"></div>"#
    ));
    let key = key_for_url(P40X20);
    let (list, _) = list_with(&app, |list, table| {
        table.get(key).is_some()
            && list.items.iter().any(|item| {
                matches!(item, PaintItem::Box { background, .. }
                    if background.iter().any(|fill| matches!(fill, Fill::Picture { .. })))
            })
    });
    let fills: Vec<&Fill> = list
        .items
        .iter()
        .filter_map(|item| match item {
            PaintItem::Box { background, .. } => Some(background),
            _ => None,
        })
        .flatten()
        .collect();
    let Some(Fill::Picture {
        image, count, step, ..
    }) = fills
        .into_iter()
        .find(|f| matches!(f, Fill::Picture { .. }))
    else {
        panic!("a picture layer");
    };
    assert_eq!(*image, key);
    assert_eq!((*step, *count), ([40., 20.], [3, 3]));
}

#[test]
fn a_video_fails_with_code_four_and_its_poster_is_drawn_like_an_img() {
    let app = app_with(&format!(
        r#"<video id="v" src="http://127.0.0.1:9/nothing.mp4" poster="{P40X20}" style="width:100px;height:100px"></video>
           <audio id="a" src="http://127.0.0.1:9/nothing.mp3"></audio>"#
    ));
    let window = app.window("p").expect("the window");
    let said = |js: &str| app.eval_renderer(window.contents, js);
    assert!(app.run_until(
        |_| said("document.getElementById('v')?.error?.code") == "4",
        TIMEOUT
    ));
    assert!(app.run_until(
        |_| said("document.getElementById('a')?.error?.code") == "4",
        TIMEOUT
    ));
    let key = key_for_url(P40X20);
    let (list, _) = list_with(&app, |list, table| {
        table.get(key).is_some() && !images_of(list).is_empty()
    });
    // `video { object-fit: contain }` in the user agent sheet: the poster is letterboxed.
    assert_eq!(rounded(images_of(&list)[0].0), [0, 25, 100, 50]);
}

#[test]
fn srcset_picks_by_density_and_the_natural_size_is_divided_by_it() {
    let app = app_with(&format!(
        r#"<img id="a" srcset="{P20X10} 2x, {P40X20} 4x"><img id="b" src="{P4X2}" srcset="{P40X20} 2x">"#
    ));
    let window = app.window("p").expect("the window");
    let said = |js: &str| app.eval_renderer(window.contents, js);
    // The window can be there before its page is: an empty `document.images` is waited out.
    let js = "(() => { const [a, b] = [...document.images]; return b ? [a.complete, a.naturalWidth, b.naturalWidth].join(' ') : ''; })()";
    assert!(
        app.run_until(|_| said(js) == "\"true 10 4\"", TIMEOUT),
        "{}",
        said(js)
    );
    // The specification's rule: at a density of 1 the first candidate that reaches it is the 2x
    // one, and `src` is the 1x one. Both candidates of `#a` are 10 wide in CSS pixels, so the
    // address is what tells them apart. (Chromium 142 itself chooses the 4x one for these `data:`
    // addresses, observed in Electron 39.8.10; the rule here is the specification's.)
    assert_eq!(
        said("document.images[0].currentSrc"),
        format!("\"{P20X10}\"")
    );
    assert_eq!(
        said("document.images[1].currentSrc.startsWith('data:image/png')"),
        "true"
    );
}

#[test]
fn an_img_that_arrives_over_http_is_laid_out_and_drawn_when_it_does() {
    let png = P4X2.trim_start_matches("data:image/png;base64,");
    let app = App::launch_script(
        &format!(
            r#"const {{ app, BrowserWindow }} = require('electron');
const http = require('http');
app.on('window-all-closed', () => {{}});
const png = Buffer.from('{png}', 'base64');
const server = http.createServer((req, res) => {{
  res.setHeader('Content-Type', 'image/png');
  res.end(png);
}});
app.whenReady().then(() => server.listen(0, '127.0.0.1', () => {{
  const win = new BrowserWindow({{ title: 'p', show: false, width: 400, height: 300 }});
  const page = '<style>html,body{{margin:0}} img{{display:block}}</style><img src="http://127.0.0.1:' + server.address().port + '/a.png">';
  win.loadURL('data:text/html,' + encodeURIComponent(page));
}}));"#
        ),
        &[],
    );
    let (list, table) = list_with(&app, |list, table| {
        images_of(list).len() == 1 && !table.is_empty()
    });
    // The page was laid out once without the picture; the bytes' arrival laid it out again.
    assert_eq!(rounded(images_of(&list)[0].0), [0, 0, 4, 2]);
    assert_eq!(table.len(), 1);
}

#[test]
fn a_video_with_a_poster_is_as_big_as_the_poster() {
    // Observed in Electron 39.8.10 (Chromium 142), DPR 1: a 40x20 poster, with or without a
    // `src` that fails; one side given makes the other by the ratio.
    let app = app_with(&format!(
        r#"<video id="a" poster="{P40X20}"></video>
           <video id="b" poster="{P40X20}" width="100"></video>
           <video id="c" poster="{P40X20}" height="50"></video>
           <video id="d" poster="{P40X20}" style="width:200px"></video>
           <video id="e" poster=""></video>"#
    ));
    let window = app.window("p").expect("the window");
    let key = key_for_url(P40X20);
    list_with(&app, |list, table| {
        table.get(key).is_some() && !images_of(list).is_empty()
    });
    let sizes = "['a', 'b', 'c', 'd', 'e'].map((id) => { const r = document.getElementById(id).getBoundingClientRect(); return r.width + 'x' + r.height; }).join(' ')";
    assert_eq!(
        app.eval_renderer(window.contents, sizes),
        "\"40x20 100x50 100x50 200x100 300x150\""
    );
}
