//! Runs a hot numeric loop and prints whether the JIT compiled it.
//!
//! Prints `compiled=<n> unavailable=<reason|none> result=<value>`. CI signs this binary the ways
//! an embedder might ship it (hardened with `allow-jit`, hardened without it) and checks the JIT
//! is used where it may be and falls back to the interpreter, with the same result, where not.

fn main() {
    let mut runtime = verve::Runtime::new();
    let result = runtime
        .eval(
            "function f(n) { let s = 0; for (let i = 0; i < n; i++) { s = (s + i * 3) | 0; } return s; }
             let t = 0;
             for (let k = 0; k < 2000; k++) { t = (t + f(1000)) | 0; }
             t",
        )
        .expect("the probe runs");
    let stats = runtime.jit_stats();
    let unavailable = verve::boa_engine::vm::jit_unavailable()
        .map_or_else(|| "none".to_owned(), |reason| format!("{reason:?}"));
    println!(
        "compiled={} unavailable={unavailable} result={}",
        stats.compiled,
        result.display()
    );
}
