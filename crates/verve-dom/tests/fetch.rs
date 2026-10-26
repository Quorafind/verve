//! DOM fetch 只解析地址，仍把 Request/init、拒绝和权限交给原有 fetch。

mod common;

use common::{Page, on_page};
use verve_dom::Options;

#[test]
fn relative_and_absolute_inputs_use_the_current_location_and_original_fetch() {
    on_page(|| {
        let mut runtime = verve::Runtime::new();
        runtime
            .eval(
                r#"
            globalThis.calls = []; globalThis.init = {method: 'POST', body: 'body'};
            globalThis.fetch = (input, options) => {
                calls.push([typeof input === 'string' ? input : input.url, options === init]);
                return Promise.resolve(new Response('mock'));
            };
        "#,
            )
            .unwrap();
        let document = verve_dom::install(
            &mut runtime,
            Options {
                url: "http://127.0.0.1:7340/app/index.html".into(),
                ..Options::default()
            },
        )
        .unwrap();
        runtime.eval(r#"
            fetch('/dsh-pet-7340/config', init);
            fetch('https://other.test/a', init);
            history.replaceState(null, '', '/changed/page');
            fetch('asset.webm', init);
            fetch(new URL('../url.webm', location.href), init);
            globalThis.request = new Request('https://other.test/post', {method:'POST', body:'keep'});
            fetch(request, init);
        "#).unwrap();
        assert_eq!(
            verve::print(&runtime.eval("JSON.stringify(calls)").unwrap()),
            r#"[["http://127.0.0.1:7340/dsh-pet-7340/config",true],["https://other.test/a",true],["http://127.0.0.1:7340/changed/asset.webm",true],["http://127.0.0.1:7340/url.webm",true],["https://other.test/post",true]]"#
        );
        runtime.eval("globalThis.rejected = []; for (const u of ['file:///secret', 'javascript:1', 'ftp://bad.test/a', 'http://[']) fetch(u).catch(e => rejected.push(e.name))").unwrap();
        runtime.run_microtasks();
        assert_eq!(
            verve::print(
                &runtime
                    .eval("[calls.length, rejected.join(',')].join('|')")
                    .unwrap()
            ),
            "5|TypeError,TypeError,TypeError,TypeError"
        );
        assert!(document.take_requests().is_empty());
    });
}

#[test]
fn relative_fetch_uses_the_existing_permission_refusal_without_network() {
    on_page(|| {
        let mut page = Page::new(Options {
            url: "http://127.0.0.1:7340/client".into(),
            ..Options::default()
        });
        page.runtime.set_permissions(verve::Permissions::none());
        page.eval(
            r#"
            globalThis.denied = '';
            fetch('/dsh-pet-7340/config').catch(e => denied =
                [e.cause.code, e.cause.permission, e.cause.resource].join('|'));
        "#,
        );
        page.runtime.run_microtasks();
        assert_eq!(page.eval("denied"), "ERR_ACCESS_DENIED|net|127.0.0.1:7340");
        page.eval("globalThis.local = ''; fetch('data:text/plain,local').then(r => r.text()).then(t => local = t)");
        page.runtime.run_microtasks();
        assert_eq!(page.eval("local"), "local");
    });
}

#[test]
fn installing_no_dom_keeps_node_fetch_absolute_only() {
    on_page(|| {
        let mut runtime = verve::Runtime::new();
        runtime
            .eval("globalThis.result = ''; fetch('/relative').catch(e => result = e.message)")
            .unwrap();
        runtime.run_microtasks();
        assert!(verve::print(&runtime.eval("result").unwrap()).contains("Failed to parse URL"));
    });
}

#[test]
fn object_url_bytes_come_from_runtime_storage_and_revoke_removes_them() {
    on_page(|| {
        let mut runtime = verve::Runtime::new();
        let url = verve::print(
            &runtime
                .eval(
                    "globalThis.url = URL.createObjectURL(new Blob([new Uint8Array([0, 1, 255])]))",
                )
                .unwrap(),
        );
        assert_eq!(runtime.object_url_bytes(&url), Some(vec![0, 1, 255]));
        let mut copy = runtime.object_url_bytes(&url).unwrap();
        copy[0] = 9;
        assert_eq!(runtime.object_url_bytes(&url), Some(vec![0, 1, 255]));
        assert_eq!(runtime.object_url_bytes("https://not-a-blob.test"), None);
        runtime.eval("URL.revokeObjectURL(url)").unwrap();
        assert_eq!(runtime.object_url_bytes(&url), None);
        runtime.eval("globalThis.fetchedURL = ''; fetch('data:application/octet-stream;base64,AAH/').then(r => r.blob()).then(b => fetchedURL = URL.createObjectURL(b))").unwrap();
        runtime.run_microtasks();
        let fetched_url = verve::print(&runtime.eval("fetchedURL").unwrap());
        assert_eq!(
            runtime.object_url_bytes(&fetched_url),
            Some(vec![0, 1, 255])
        );
        let other = verve::Runtime::new();
        assert_eq!(other.object_url_bytes(&fetched_url), None);
    });
}
