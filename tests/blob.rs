//! `verve::blob`: an embedder makes a `Blob` that scripts read as their own, and reads back the
//! bytes of one a script made.

use verve::Runtime;

#[test]
fn an_embedders_blob_reads_as_a_scripts_and_back() {
    let mut runtime = Runtime::new();
    let blob = verve::blob::new_blob(b"hi".to_vec(), "Text/Plain", runtime.context()).unwrap();
    runtime
        .context()
        .global_object()
        .set(
            boa_engine::js_string!("made"),
            blob,
            false,
            runtime.context(),
        )
        .unwrap();
    let seen = runtime
        .eval("[made instanceof Blob, made.size, made.type].join()")
        .unwrap();
    assert_eq!(verve::print(&seen), "true,2,text/plain");

    let theirs = runtime
        .eval("new Blob(['a', new Uint8Array([98])])")
        .unwrap();
    assert!(verve::blob::is_blob(&theirs, runtime.context()));
    assert_eq!(
        verve::blob::blob_bytes(&theirs, runtime.context()).unwrap(),
        b"ab"
    );
}
