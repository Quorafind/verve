use crate::{TestAction, run_test_actions};
use boa_macros::js_str;
use indoc::indoc;

#[test]
fn compare_may_read_compare_while_converting_its_arguments() {
    // `ToString` of an argument runs user code that reads `compare`, which borrows the collator.
    run_test_actions([TestAction::assert_eq(
        indoc! {r#"
            const c = new Intl.Collator();
            const log = [];
            const r = c.compare({ toString() { const f = c.compare; log.push(typeof f); return 'a'; } }, 'b');
            [r, log.join()].join(" ");
        "#},
        js_str!("-1 function"),
    )]);
}
