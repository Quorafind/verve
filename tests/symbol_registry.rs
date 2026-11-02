//! `Symbol.for` across threads: the registry is shared by every realm, so a key several
//! realms ask for at once must come back as one symbol — React caches `Symbol.for('react.element')`
//! in each module that uses it, and a realm that holds a symbol the others do not rejects its own elements.

#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Barrier};

use verve::Runtime;

const THREADS: usize = 8;
const KEYS: usize = 3000;

#[test]
fn symbol_for_gives_every_thread_the_same_symbol_for_a_key_asked_at_once() {
    let asked = Arc::new(Barrier::new(THREADS));
    let done = Arc::new(Barrier::new(THREADS));
    let realms: Vec<_> = (0..THREADS)
        .map(|_| {
            let (asked, done) = (asked.clone(), done.clone());
            std::thread::Builder::new()
                .stack_size(16 << 20)
                .spawn(move || {
                    verve::set_thread_stack(16 << 20);
                    let mut runtime = Runtime::new();
                    runtime
                        .eval(&format!(
                            "globalThis.keys = Array.from({{ length: {KEYS} }}, (_, i) => 'race.' + i)"
                        ))
                        .unwrap();
                    asked.wait();
                    runtime
                        .eval("globalThis.held = keys.map((key) => Symbol.for(key))")
                        .unwrap();
                    done.wait();
                    let strays = runtime
                        .eval("keys.filter((key, i) => Symbol.for(key) !== held[i]).length")
                        .unwrap();
                    verve::print(&strays)
                })
                .unwrap()
        })
        .collect();
    for realm in realms {
        assert_eq!(realm.join().unwrap(), "0");
    }
}
