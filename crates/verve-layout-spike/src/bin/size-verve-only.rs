//! Q2 baseline: the Verve runtime alone (no GPUI), the same eval as size-app.
fn main() {
    let mut rt = verve::Runtime::new();
    println!("{:?}", rt.eval("1 + 1").is_ok());
}
