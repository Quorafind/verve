//! Verve's own web and Node globals with a document installed: the document extends the
//! realm's `EventTarget` and `Event` rather than replacing them, so the runtime's own targets —
//! `AbortSignal`, `MessagePort` — and everything else behave as they do without one. And
//! `verve-dom run`, Verve's runner with a document.

mod common;

use common::{Page, on_page};
use verve_dom::Options;

#[test]
fn the_runtimes_own_globals_behave_as_without_a_document() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval(
            r#"
            globalThis.out = [];
            class Bus extends EventTarget { constructor() { super(); this.mine = 1; } }
            const bus = new Bus();
            bus.addEventListener('x', (e) => out.push('bus ' + (e.target === bus) + ' ' + e.eventPhase));
            bus.dispatchEvent(new Event('x'));
            const controller = new AbortController();
            controller.signal.addEventListener('abort', () => out.push('aborted ' + controller.signal.reason.name));
            document.body.addEventListener('click', () => out.push('not after abort'), { signal: controller.signal });
            controller.abort();
            document.body.click();
            out.push([controller.signal instanceof EventTarget, document.body instanceof EventTarget,
                      new MessageEvent('message') instanceof Event, new MouseEvent('click') instanceof Event].join(' '));
            const { port1, port2 } = new MessageChannel();
            port2.onmessage = (e) => out.push('message ' + e.data.n);
            port1.postMessage({ n: 7 });
            setTimeout(() => out.push('timeout'), 5);
            queueMicrotask(() => out.push('microtask'));
            out.push(structuredClone({ a: [1, 2] }).a.length);
            out.push(new URL('/x?y=1', 'https://a.test').searchParams.get('y'));
            out.push(typeof process.versions.node + ' ' + typeof crypto.randomUUID() + ' ' + typeof Buffer.from);
            out.push(new TextDecoder().decode(new TextEncoder().encode('ok')));
            new Response('body').text().then((t) => out.push('response ' + t));
            "#,
        );
        page.settle();
        let out = page.eval("out.join(' | ')");
        assert_eq!(
            out,
            "bus true 2 | aborted AbortError | true true true true | 2 | 1 | string string function | ok | microtask | response body | message 7 | timeout"
        );
    });
}

#[test]
fn verve_dom_run_runs_a_script_with_a_document() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_verve-dom"))
        .args([
            "run",
            "-e",
            "document.body.innerHTML = '<p class=a>hi</p>'; requestAnimationFrame(() => console.log(document.querySelector('.a').textContent, typeof window, location.href))",
        ])
        .output()
        .expect("verve-dom runs");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "hi object about:blank");
}
