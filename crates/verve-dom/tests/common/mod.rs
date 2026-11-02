//! What the tests share: a page on a thread with room for Boa's stack, on a clock that moves
//! only when the test says, and a way to run what is due until the page is idle.

#![allow(dead_code)]

use std::cell::Cell;
use std::rc::Rc;

use verve::Runtime;
use verve_dom::{Document, Options};

pub struct Page {
    pub runtime: Runtime,
    pub document: Document,
    pub clock: Rc<Cell<f64>>,
}

impl Page {
    pub fn new(options: Options) -> Self {
        let clock = Rc::new(Cell::new(0.));
        let time = clock.clone();
        let mut runtime = Runtime::with_clock(move || time.get());
        let document = verve_dom::install(&mut runtime, options).expect("the document installs");
        Page {
            runtime,
            document,
            clock,
        }
    }

    /// Evaluates `source` as a script and answers its value as `console.log` would print it;
    /// panics with what was thrown.
    pub fn eval(&mut self, source: &str) -> String {
        match self.runtime.eval(source) {
            Ok(value) => verve::print(&value),
            Err(error) => panic!("script failed: {error}\n--- console:\n{}", self.console()),
        }
    }

    pub fn try_eval(&mut self, source: &str) -> Result<String, String> {
        self.runtime.eval(source).map(|value| verve::print(&value))
    }

    /// Runs every timer due, moving the clock to each in turn, until none is due within
    /// `limit` milliseconds of virtual time.
    pub fn settle_for(&mut self, limit: f64) {
        let start = self.clock.get();
        self.runtime.run_microtasks();
        for _ in 0..100_000 {
            let Some(due) = self.runtime.next_due() else {
                break;
            };
            if due > start + limit {
                break;
            }
            if due > self.clock.get() {
                self.clock.set(due);
            }
            self.runtime.run_due();
        }
        self.runtime.run_microtasks();
    }

    /// [`Page::settle_for`] a second.
    pub fn settle(&mut self) {
        self.settle_for(1000.);
    }

    pub fn console(&mut self) -> String {
        self.runtime
            .take_console()
            .into_iter()
            .map(|line| format!("{:?}: {}", line.level, line.text))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The first element `selector` finds.
    pub fn find(&self, selector: &str) -> verve_dom::NodeId {
        let dom = self.document.dom();
        dom.query(dom.document, selector)
            .unwrap_or_else(|| panic!("nothing matches {selector}"))
    }
}

/// A page with `css` in a `<style>` element of its head, and `body` in its body.
pub fn styled(css: &str, body: &str) -> Page {
    let mut page = Page::new(Options::default());
    let css = serde_json::to_string(css).expect("a string");
    let body = serde_json::to_string(body).expect("a string");
    page.eval(&format!(
        "document.head.innerHTML = '<style>' + {css} + '</style>';
         document.body.innerHTML = {body};"
    ));
    page
}

/// `getComputedStyle(document.getElementById(id)).getPropertyValue(property)`.
pub fn style(page: &mut Page, id: &str, property: &str) -> String {
    page.eval(&format!(
        "getComputedStyle(document.getElementById('{id}')).getPropertyValue('{property}')"
    ))
}

/// Runs `f` on a thread with a stack Boa can recurse in, as Verve's own isolates do.
pub fn on_page<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> R {
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

/// A 1x1 PNG, as an `<img src>` takes it.
pub const PNG_1X1: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";

impl Page {
    /// The addresses the page has asked the loader for as images since this was last asked.
    pub fn image_requests(&mut self) -> Vec<String> {
        self.document.update();
        self.document
            .take_requests()
            .into_iter()
            .filter_map(|request| match request {
                verve_dom::Request::Image(url) => Some(url),
                _ => None,
            })
            .collect()
    }
}

/// A page with a `width` by `height` canvas `c` in the body, its 2D context `g`, and `px(x, y)`
/// answering that pixel as `r,g,b,a`.
pub fn canvas_page(width: u32, height: u32) -> Page {
    let mut page = Page::new(Options {
        url: "https://app.test/index.html".into(),
        ..Options::default()
    });
    page.eval(&format!(
        "globalThis.c = document.createElement('canvas'); c.width = {width}; c.height = {height};
         document.body.append(c); globalThis.g = c.getContext('2d');
         globalThis.px = (x, y) => Array.from(g.getImageData(x, y, 1, 1).data).join(',');"
    ));
    page
}
