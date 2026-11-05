//! Looking into a running guest from outside: what a test or `--headless` asks of a script's
//! realm, answered as the JSON the value would be.

use verve::Runtime;

/// Runs `code` in the global scope of `runtime` and answers the result as `JSON.stringify` writes
/// it (`undefined` for a value JSON has no word for), or the message of what it threw.
pub(crate) fn evaluate(runtime: &mut Runtime, code: &str) -> Result<String, String> {
    let value = runtime.eval(code)?;
    let json = runtime.eval("JSON")?;
    let stringify = runtime.eval("JSON.stringify")?;
    let text = runtime.call(&stringify, &json, &[value])?;
    Ok(verve::print(&text))
}
