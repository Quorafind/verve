//! Deep recursion — through JavaScript alone, and through natives that call back into it —
//! ends in a `RangeError` a script can catch, never in the thread's stack running out.

use verve::Runtime;

#[test]
fn deep_recursion_is_a_range_error_a_script_catches() {
    let stack = 8 * 1024 * 1024;
    std::thread::Builder::new()
        .stack_size(stack)
        .spawn(move || {
            verve::set_thread_stack(stack);
            let mut rt = Runtime::new();
            let answer = rt
                .eval(
                    "const deep = (n) => [n].map((m) => deep(m + 1))[0];
                     const plain = (n) => plain(n + 1);
                     const caught = (f) => { try { f(0); return 'returned'; } catch (e) { return e instanceof RangeError ? 'RangeError' : String(e); } };
                     caught(deep) + ' ' + caught(plain)",
                )
                .unwrap();
            assert_eq!(verve::print(&answer), "RangeError RangeError");
        })
        .unwrap()
        .join()
        .unwrap();
}
