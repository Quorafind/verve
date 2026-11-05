//! Element reads and writes of typed arrays compiled inline (`compiler/typed.rs`): every kind,
//! index, value and change to a buffer that decides whether a guard holds. The three modes must
//! agree, and agree with Node's output for the same program (`tests_typed_elements/`).

use super::{
    tests::Mode,
    tests_typed_length::{context_with_natives, run_with_natives},
};
use crate::Source;

/// The program: `detach` and `gc` come from the test, as in Node's script (`structuredClone`
/// with a transfer, and a function that does nothing).
const SCENARIOS: &str = include_str!("tests_typed_elements/scenarios.js");

/// What Node 22 prints for [`SCENARIOS`].
const EXPECTED: &str = include_str!("tests_typed_elements/expected.txt");

/// Huge, negative, fractional and non-finite numbers written to every element kind, to
/// `DataView` and through `String.fromCharCode` and `Atomics`: the small integer conversions
/// take the low bits of the truncated number however large it is.
const HUGE: &str = include_str!("tests_typed_elements/huge.js");

/// What Node 22 prints for [`HUGE`].
const HUGE_EXPECTED: &str = include_str!("tests_typed_elements/huge.txt");

/// Random operations of every numeric kind, one hot loop each: the three modes only.
const RANDOM: &str = include_str!("tests_typed_elements/random.js");

fn assert_modes_agree(source: &str) -> String {
    let interpreted = run_with_natives(source, Mode::Interpreted);
    for mode in [Mode::Eager, Mode::Hot] {
        assert_eq!(
            run_with_natives(source, mode),
            interpreted,
            "{mode:?} differs from the interpreter"
        );
    }
    interpreted
}

#[test]
fn typed_array_elements_match_the_interpreter_and_node() {
    let interpreted = assert_modes_agree(SCENARIOS);
    let expected: Vec<&str> = EXPECTED.trim_end().lines().collect();
    assert_eq!(interpreted.lines().count(), expected.len());
    let differing: Vec<String> = interpreted
        .lines()
        .zip(expected)
        .filter(|(line, expected)| line != expected)
        .map(|(line, expected)| {
            format!(
                "got      {line}
expected {expected}"
            )
        })
        .collect();
    assert!(
        differing.is_empty(),
        "{}",
        differing.join(
            "
"
        )
    );
}

#[test]
fn random_typed_array_accesses_match_the_interpreter() {
    let output = assert_modes_agree(RANDOM);
    assert_eq!(output.lines().count(), 36, "{output}");
}

#[test]
fn the_layout_of_typed_arrays_checks_out() {
    let layout = super::layout::object_layout().expect("the object layout checks out");
    assert!(layout.typed.is_some(), "typed arrays are read inline");
}

#[test]
fn huge_floats_written_to_typed_arrays_match_node() {
    let output = assert_modes_agree(HUGE);
    assert_eq!(
        output.lines().collect::<Vec<_>>(),
        HUGE_EXPECTED.lines().collect::<Vec<_>>()
    );
}

/// A function with `accesses` element reads and writes of `a`, called `rounds` times in a row on
/// each receiver in turn; the program returns what they sum to.
const FEEDBACK: &str = r"
const f = new Function('a', 'let s = 0; for (let i = 0; i < 4; i++) { s += a[i]; a[i] = i + 1; } return s;');
const run = (receivers) => { const out = []; for (const r of receivers) for (let k = 0; k < 20; k++) out.push(f(r)); return out.join(); };
const plain = () => [1, 2, 3, 4];
const typed = () => new Float64Array([1, 2, 3, 4]);
const objectLike = () => ({ 0: 1, 1: 2, 2: 3, 3: 4 });
const shapes = [
  [plain(), typed()],
  [typed(), plain(), objectLike()],
  [typed(), typed(), new Uint8Array(2), plain(), typed()],
  [plain(), plain(), plain(), typed(), typed(), objectLike(), typed()],
];
shapes.map((s, n) => 'g' + n + ':' + run(s)).join('|');
";

/// A site that sees a plain array first and a typed array after it is compiled, and the reverse,
/// agree with the interpreter in every mode (the site without feedback leaves a typed array to
/// the helper).
#[test]
fn a_site_that_meets_a_typed_array_before_or_after_compiling_agrees() {
    let output = assert_modes_agree(FEEDBACK);
    assert_eq!(output.matches('|').count(), 3, "{output}");
}

/// The inline typed-array path is compiled only in the functions that have seen a typed array
/// (unless the options ask for it everywhere).
#[test]
fn only_functions_that_saw_a_typed_array_get_the_inline_path() {
    use super::compiler::TYPED_PATHS;
    let source = r"
const f = new Function('a', 'let s = 0; for (let i = 0; i < 4; i++) { s += a[i]; a[i] = i + 1; } return s;');
const g = new Function('a', 'let s = 0; for (let i = 0; i < 4; i++) { s += a[i]; a[i] = i + 1; } return s;');
let r = 0;
for (let k = 0; k < 20; k++) { r += f(new Float64Array(4)); r += g([1, 2, 3, 4]); }
r;";
    let paths = |everywhere: bool| {
        let mut context = context_with_natives(Mode::Hot);
        context.jit_options_mut().typed_elements_everywhere = everywhere;
        TYPED_PATHS.with(|count| count.set(0));
        context.eval(Source::from_bytes(source)).expect("runs");
        TYPED_PATHS.with(std::cell::Cell::get)
    };
    // `f` has a read and a write; `g` gets none from the feedback.
    assert_eq!(paths(false), 2);
    assert_eq!(paths(true), 4);
}
