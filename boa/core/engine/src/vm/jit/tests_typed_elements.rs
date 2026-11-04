//! Element reads and writes of typed arrays compiled inline (`compiler/typed.rs`): every kind,
//! index, value and change to a buffer that decides whether a guard holds. The three modes must
//! agree, and agree with Node's output for the same program (`tests_typed_elements/`).

use super::{tests::Mode, tests_typed_length::run_with_natives};

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
