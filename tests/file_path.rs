//! A `File` the host made from a path on disk remembers the path, where script cannot read it;
//! one a script makes has none.

use verve::Runtime;
use verve::blob::{file_with_path, path_of_file};

#[test]
fn a_file_the_host_found_has_its_path_and_a_page_made_one_has_none() {
    let mut rt = Runtime::new();
    let path = std::path::Path::new("C:/data/report.txt");
    let file = file_with_path(
        rt.context(),
        b"abc".to_vec(),
        "report.txt",
        "text/plain",
        path,
    )
    .expect("a file");
    assert_eq!(path_of_file(&file, rt.context()).as_deref(), Some(path));
    rt.context()
        .global_object()
        .set(boa_engine::js_string!("host"), file, false, rt.context())
        .unwrap();
    let seen = rt
        .eval(
            "[host.name, host.size, host.type, host instanceof File, host instanceof Blob, \
             Object.getOwnPropertyNames(host).join(), Object.getOwnPropertySymbols(host).length].join('|')",
        )
        .expect("it runs");
    let seen = verve::print(&seen);
    assert!(
        seen.starts_with("report.txt|3|text/plain|true|true|"),
        "{seen}"
    );
    // Script cannot find the path among the names, symbols or values of the file.
    assert!(!seen.contains("data") && !seen.contains("C:"), "{seen}");
    let made = rt.eval("new File(['x'], 'mine.txt')").expect("a file");
    assert_eq!(path_of_file(&made, rt.context()), None);
    assert_eq!(
        path_of_file(&boa_engine::JsValue::null(), rt.context()),
        None
    );
}

#[test]
fn a_world_has_files_with_paths_too() {
    let mut rt = Runtime::new();
    let world = rt.create_world().expect("a world");
    let previous = rt.enter_world(world);
    let file = file_with_path(
        rt.context(),
        Vec::new(),
        "x.bin",
        "",
        std::path::Path::new("/tmp/x.bin"),
    )
    .expect("a file");
    rt.enter_world(previous);
    assert!(path_of_file(&file, rt.context()).is_some());
}
