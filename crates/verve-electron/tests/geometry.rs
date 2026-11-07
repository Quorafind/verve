//! Geometry a page's script reads right after it changed the document: the page lays itself out
//! on its own thread, so the numbers are those of the document as it is, not as it was when a
//! shell last measured it. These are Verve's own rules (the headless text measurer, no text
//! layout before P31); the pages have no text.
#![allow(clippy::unwrap_used)]

mod support;

use support::run;

/// A page `wc` runs `script` in, after `html` loaded in a 400 x 300 window; logs what it returns.
fn page_says(html: &str, script: &str) -> Vec<String> {
    run(&format!(
        r#"
    const win = new BrowserWindow({{ show: false, width: 400, height: 300 }});
    const wc = win.webContents;
    await load(win, '<style>html,body{{margin:0;padding:0}}</style>{html}');
    log(await wc.executeJavaScript(`{script}`));
    "#
    ))
}

#[test]
fn a_style_set_on_the_line_before_is_in_the_box_read_on_the_next() {
    let lines = page_says(
        r#"<div id="e"></div><div id="n"></div>"#,
        r#"(() => {
          const el = document.getElementById('e');
          el.style.width = '123px';
          const width = el.getBoundingClientRect().width;
          el.style.height = '45px';
          const after = document.getElementById('n').getBoundingClientRect().y;
          return [width, el.offsetWidth, el.clientHeight, after].join(' ');
        })()"#,
    );
    assert_eq!(lines, ["123 123 45 45"]);
}

#[test]
fn the_element_from_a_point_is_the_one_laid_out_there() {
    let lines = page_says(
        r#"<div id="a" style="height:20px"></div><div id="b" style="height:20px"></div>"#,
        r#"(() => {
          const at = (y) => document.elementFromPoint(5, y).id;
          const before = [at(10), at(30)];
          document.getElementById('a').style.height = '40px';
          return [...before, at(30)].join(' ');
        })()"#,
    );
    assert_eq!(lines, ["a b a"]);
}

#[test]
fn a_container_scrolls_at_once_and_stays_inside_what_it_can_reach() {
    let lines = page_says(
        r#"<div id="c" style="width:100px;height:100px;overflow:auto"><div id="k" style="height:400px"></div></div>"#,
        r#"(async () => {
          const c = document.getElementById('c');
          const k = document.getElementById('k');
          const tick = () => new Promise((resolve) => setTimeout(resolve, 20));
          const said = [c.scrollHeight];
          c.scrollTop = 150;
          said.push(c.scrollTop, k.getBoundingClientRect().y, c.scrollHeight);
          await tick();
          c.scrollTop = 9999;
          said.push(c.scrollTop);
          await tick();
          said.push(c.scrollTop, k.getBoundingClientRect().y, c.scrollHeight);
          return said.join(' ');
        })()"#,
    );
    // `scrollHeight` is the content's, not what is left of it after the scroll; a scroll past the end
    // reads back clamped at once.
    assert_eq!(lines, ["400 150 -150 400 300 300 -300 400"]);
}

#[test]
fn the_page_scrolls_with_the_window() {
    let lines = page_says(
        r#"<div id="t" style="height:2000px"></div>"#,
        r#"(async () => {
          window.scrollTo(0, 500);
          await new Promise((resolve) => setTimeout(resolve, 20));
          const top = document.getElementById('t').getBoundingClientRect().y;
          window.scrollTo(0, 99999);
          await new Promise((resolve) => setTimeout(resolve, 20));
          return [top, scrollY === 2000 - innerHeight].join(' ');
        })()"#,
    );
    // The page scrolls as far as its height less the viewport's (`innerHeight`), and no further.
    assert_eq!(lines, ["-500 true"]);
}

#[test]
fn the_wheel_scrolls_the_container_under_the_pointer_and_then_the_page() {
    // The wheel moves the nearest container under the pointer that can move that way; when that
    // one is at its end the next one out takes it. A listener that prevents `wheel` stops it.
    // Electron's `deltaY` is Blink's: negative scrolls down, positive up. Real Electron 39.8.10
    // (Windows, DPR 1): `deltaY: 70` gave a `wheel` event with `deltaY` -70 and no scroll at the
    // top, `deltaY: -20` gave `deltaY` 20 and `scrollY` 20.
    let lines = run(r#"
    const win = new BrowserWindow({ show: false, width: 400, height: 300 });
    const wc = win.webContents;
    await load(win, '<style>html,body{margin:0;padding:0}</style><div id="c" style="width:100px;height:100px;overflow:auto"><div style="height:150px"></div></div><div style="height:1000px"></div>');
    const read = () => wc.executeJavaScript('[document.getElementById("c").scrollTop, scrollY].join(" ")');
    const turn = async (deltaY) => {
      wc.sendInputEvent({ type: 'mouseWheel', x: 10, y: 10, deltaX: 0, deltaY });
      await sleep(100);
      return read();
    };
    log(await turn(-30));
    log(await turn(-500));
    log(await turn(-40));
    log(await turn(25));
    await wc.executeJavaScript('document.addEventListener("wheel", (e) => e.preventDefault(), { passive: false })');
    log(await turn(-40));
    "#);
    // The container has 50 px to scroll; what it cannot take goes to the page, from then on, and
    // a wheel turned the other way moves the container back first.
    assert_eq!(lines, ["30 0", "50 0", "50 40", "25 40", "25 40"]);
}

#[test]
fn scroll_by_adds_once_and_the_last_of_two_scrolls_to_wins() {
    let lines = page_says(
        r#"<div id="c" style="width:100px;height:100px;overflow:auto"><div style="height:400px"></div></div>"#,
        r#"(async () => {
          const c = document.getElementById('c');
          c.scrollBy(0, 50);
          c.scrollBy(0, 50);
          const by = c.scrollTop;
          c.scrollTo(0, 100);
          c.scrollTo(0, 200);
          const to = c.scrollTop;
          await new Promise((resolve) => setTimeout(resolve, 20));
          return [by, to, c.scrollTop].join(' ');
        })()"#,
    );
    assert_eq!(lines, ["100 200 200"]);
}

#[test]
fn a_resize_observer_hears_of_a_box_when_it_is_observed_and_when_it_changes() {
    // The page lays itself out, and nobody reports the boxes to it: the observer hears at the end
    // of the turn that observed or changed the box.
    let lines = run(r#"
    const win = new BrowserWindow({ show: false, width: 400, height: 300 });
    const wc = win.webContents;
    await load(win, '<style>html,body{margin:0;padding:0}</style><div id="e" style="width:100px;height:20px"></div>');
    await wc.executeJavaScript(`window.seen = []; new ResizeObserver((entries) => entries.forEach((e) => seen.push(e.contentRect.width))).observe(document.getElementById('e')); 0`);
    await sleep(100);
    log(await wc.executeJavaScript('seen.join()'));
    await wc.executeJavaScript("document.getElementById('e').style.width = '150px'; 0");
    await sleep(100);
    log(await wc.executeJavaScript('seen.join()'));
    "#);
    assert_eq!(lines, ["100", "100,150"]);
}
