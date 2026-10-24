//! A host stopping what a context runs, from another thread.

use crate::{
    Context, Source,
    error::{EngineError, RuntimeLimitError},
};

#[test]
fn an_endless_loop_is_stopped_from_another_thread() {
    let mut context = Context::default();
    let handle = context.interrupt_handle();
    let stopper = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(50));
        handle.interrupt();
    });
    // Not even a `catch` keeps it running.
    let result = context.eval(Source::from_bytes(
        "try { while (true) {} } catch (e) { 'caught' }",
    ));
    stopper.join().unwrap();
    let error = result.expect_err("the loop was stopped");
    assert_eq!(
        error.as_engine(),
        Some(&EngineError::RuntimeLimit(RuntimeLimitError::Interrupted))
    );
    // The request is used up: what runs next runs.
    let value = context
        .eval(Source::from_bytes(
            "let n = 0; for (let i = 0; i < 1000; i++) n += i; n",
        ))
        .unwrap();
    assert_eq!(value.as_number(), Some(499_500.));
}

#[test]
fn an_endless_chain_of_calls_is_stopped() {
    let mut context = Context::default();
    let handle = context.interrupt_handle();
    handle.interrupt();
    let result = context.eval(Source::from_bytes("function f() { return 1 } f()"));
    assert!(result.is_err(), "the next call throws");
    handle.clear();
    assert!(
        context
            .eval(Source::from_bytes("function g() { return 1 } g()"))
            .is_ok()
    );
}
