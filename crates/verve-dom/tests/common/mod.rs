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
