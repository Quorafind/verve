//! The web platform's pieces libraries test for, beside the DOM proper — ported from
//! su-editor's Obsidian host's own tests, less what is Obsidian's.

mod common;

use common::{Page, on_page};
use verve_dom::Options;

#[test]
fn review_navigation_and_forms_only_queue_http_urls() {
    on_page(|| {
        let mut page = Page::new(Options {
            url: "https://example.test/app/".into(),
            ..Options::default()
        });
        page.eval(r#"
            for (const url of ['javascript:alert(1)', 'file:///C:/Windows/win.ini', 'data:text/html,x', 'custom:x', 'http://[']) {
                try { location.href = url; } catch (_) {}
                try { window.open(url); } catch (_) {}
                const a = document.createElement('a'); a.href = url;
                try { a.click(); } catch (_) {}
                const f = document.createElement('form'); f.action = url;
                try { f.submit(); } catch (_) {}
                try { f.requestSubmit(); } catch (_) {}
            }
        "#);
        assert!(page.document.take_requests().is_empty());
        page.eval("location.href = '/next'; window.open('https://example.test/ok'); const f = document.createElement('form'); f.action = '/send'; f.submit()");
        let requests = page.document.take_requests();
        assert_eq!(requests.len(), 3);
        assert!(
            matches!(&requests[0], verve_dom::Request::Navigate { url, .. } if url == "https://example.test/next")
        );
        assert!(
            matches!(&requests[1], verve_dom::Request::Open(url) if url == "https://example.test/ok")
        );
        assert!(matches!(&requests[2], verve_dom::Request::Submit(_)));
        page.eval("f.submit(); f.action = 'file:///C:/Windows/win.ini'");
        assert!(page.document.take_requests().is_empty());
    });
}

#[test]
fn the_webs_pieces_libraries_test_for_are_there() {
    on_page(|| {
        let mut page = Page::new(Options {
            url: "https://example.test/app/index.html".into(),
            ..Options::default()
        });
        let cases = [
            (
                "(() => { const t = new EventTarget(); let n = 0; t.addEventListener('x', () => n++); t.dispatchEvent(new Event('x'));
                   class Bus extends EventTarget {} const b = new Bus(); b.addEventListener('y', e => n += 10, { once: true });
                   b.dispatchEvent(new CustomEvent('y')); b.dispatchEvent(new CustomEvent('y')); return n })()",
                "11",
            ),
            ("'oninput' in document.createElement('div')", "true"),
            (
                "(() => { const el = document.createElement('button'); let n = 0; el.onclick = () => n++; el.click(); el.onclick = null; el.click(); return n })()",
                "1",
            ),
            ("(() => { const f = document.createElement('a'); f.onclick = () => false; return f.dispatchEvent(new MouseEvent('click', { cancelable: true })) })()", "false"),
            ("new Image(3, 4).outerHTML", "<img width=\"3\" height=\"4\">"),
            ("new Image() instanceof HTMLImageElement", "true"),
            ("new Option('A', 'a').value", "a"),
            ("new XMLSerializer().serializeToString(document.implementation.createHTMLDocument('t'))", "<!DOCTYPE html><html><head><title>t</title></head><body></body></html>"),
            (
                "(() => { const ns = 'http://www.w3.org/2000/svg'; const s = document.createElementNS(ns, 'svg'), p = document.createElementNS(ns, 'clipPath');
                   s.appendChild(p); return [s instanceof SVGSVGElement, p instanceof SVGElement, p.tagName, p.namespaceURI === ns, s.outerHTML].join(' ') })()",
                "true true clipPath true <svg><clipPath/></svg>",
            ),
            (
                "(() => { const s = document.createElement('style'); s.textContent = '.a { color: red }'; document.head.appendChild(s);
                   s.sheet.insertRule('.b { color: blue }', 1); const n = s.sheet.cssRules.length; const sel = s.sheet.cssRules[1].selectorText;
                   s.remove(); return n + ' ' + sel })()",
                "2 .b",
            ),
            ("document.createElement('canvas').getContext('webgl')", "null"),
            ("document.createElement('canvas').getContext('2d').measureText('hello').width > 0", "true"),
            ("(() => { const a = document.createElement('div'), b = document.createElement('p'); a.append(b); return a.compareDocumentPosition(b) + ' ' + b.compareDocumentPosition(a) })()", "20 10"),
            (
                "(() => { const host = document.createElement('div'); const root = host.attachShadow({ mode: 'open' });
                   root.innerHTML = '<p id=x>in</p>'; return [host.shadowRoot === root, root.host === host, host.children.length, root.getElementById('x').textContent, root instanceof ShadowRoot].join(' ') })()",
                "true true 0 in true",
            ),
            ("matchMedia('(prefers-color-scheme: dark)') instanceof EventTarget", "true"),
            ("matchMedia('(min-width: 600px)').matches + ' ' + matchMedia('(max-width: 600px)').matches", "true false"),
            ("top === window && parent === window && window instanceof Window && window instanceof EventTarget", "true"),
            ("new URL('main.js', document.baseURI).href", "https://example.test/app/main.js"),
            ("location.pathname + ' ' + location.origin", "/app/index.html https://example.test"),
            (
                "(() => { const d = document.createElement('a'); d.setAttribute('href', 'x'); d.className = 'c';
                   return [...d.attributes].map(a => a.name + '=' + a.value).join(',') + ' ' + d.attributes.href.value + ' ' + d.href })()",
                "href=x,class=c x https://example.test/app/x",
            ),
            (
                "(() => { class Badge extends HTMLElement { connectedCallback() { this.textContent = 'hi' } }
                   customElements.define('su-badge', Badge); const e = document.createElement('su-badge'); document.body.appendChild(e);
                   return [e instanceof Badge, e.tagName, customElements.get('su-badge') === Badge, e.textContent].join(' ') })()",
                "true SU-BADGE true hi",
            ),
            (
                "(() => { const f = document.createRange().createContextualFragment('<b>a</b>c'); return f.childNodes.length + ' ' + getSelection().rangeCount })()",
                "2 0",
            ),
            ("(() => { const t = document.createElement('table'); t.createTBody().insertRow().insertCell().textContent = 'x'; return t.outerHTML })()", "<table><tbody><tr><td>x</td></tr></tbody></table>"),
            ("(() => { const d = document.createElement('div'), s = document.createElement('span'); d.append(s); s.outerHTML = '<i>q</i>'; return d.innerHTML })()", "<i>q</i>"),
            ("CSS.escape('a b#1')", "a\\ b\\#1"),
            (
                "(() => { localStorage.setItem('k', 'v'); localStorage.other = 1; sessionStorage.setItem('s', 't');
                   return [localStorage.getItem('k'), localStorage.length, Object.keys(localStorage).join(','), localStorage.other, sessionStorage.s, 'k' in localStorage].join(' ') })()",
                "v 2 k,other 1 t true",
            ),
            (
                "(() => { const t = document.createElement('template'); t.innerHTML = '<li>a</li><li>b</li>';
                   const copy = t.content.cloneNode(true); return [t.childNodes.length, t.content.childNodes.length, copy.childNodes.length, t.content instanceof DocumentFragment].join(' ') })()",
                "0 2 2 true",
            ),
            ("(() => { const e = document.createElement('div'); e.dataset.fooBar = 'x'; return e.getAttribute('data-foo-bar') + ' ' + JSON.stringify({ ...e.dataset }) })()", "x {\"fooBar\":\"x\"}"),
            ("(() => { const e = document.createElement('p'); e.style.backgroundColor = 'red'; e.style.setProperty('--gap', '2px'); e.style.cssFloat = 'left'; return e.getAttribute('style') + ' | ' + e.style.length + ' ' + e.style[0] })()", "background-color: red; --gap: 2px; float: left; | 3 background-color"),
            ("(() => { try { document.querySelector('a[') } catch (e) { return e.name + ' ' + (e instanceof DOMException) } })()", "SyntaxError true"),
            ("(() => { try { document.body.appendChild(document.documentElement) } catch (e) { return e.name } })()", "HierarchyRequestError"),
            ("(() => { const w = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT); document.body.innerHTML = '<p>a<b>b</b></p>c'; const out = []; while (w.nextNode()) out.push(w.currentNode.data); return out.join('') })()", "abc"),
            ("(() => { let n = 0; requestAnimationFrame(() => n++); return typeof requestAnimationFrame + ' ' + typeof cancelAnimationFrame + ' ' + n })()", "function function 0"),
            ("new DOMMatrix('translate(10px, 5px) scale(2)').transformPoint({ x: 1, y: 1 }).x", "12"),
            ("document.createElement('div') instanceof HTMLDivElement && document instanceof HTMLDocument && document instanceof Document", "true"),
            ("document.documentElement.namespaceURI + ' ' + document.createElement('x-y').localName", "http://www.w3.org/1999/xhtml x-y"),
            ("history.pushState({ a: 1 }, '', '/next?q=1'); location.href + ' ' + history.state.a + ' ' + history.length", "https://example.test/next?q=1 1 2"),
        ];
        for (source, expected) in cases {
            let answer = page.eval(source);
            assert_eq!(answer.trim_matches('"'), expected, "{source}");
        }
        page.settle();
        assert_eq!(page.console(), "");
    });
}

#[test]
fn xml_http_request_is_answered_through_fetch_on_the_hosts_beat() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval(
            r#"
            globalThis.out = [];
            const xhr = new XMLHttpRequest();
            xhr.onreadystatechange = () => out.push('state ' + xhr.readyState);
            xhr.addEventListener('load', () => out.push('load ' + xhr.status + ' ' + xhr.response.method + ' ' + xhr.getResponseHeader('Content-Type')));
            xhr.onloadend = () => out.push('end');
            xhr.open('GET', 'data:application/json,{"method":"GET"}');
            xhr.responseType = 'json';
            xhr.setRequestHeader('X-Test', '1');
            xhr.send();
            // The network is denied to a realm by default: an error, as an unreachable host's.
            const bad = new XMLHttpRequest();
            bad.onerror = () => out.push('error ' + bad.status);
            bad.open('GET', 'http://127.0.0.1:9/nope');
            bad.send();
            "#,
        );
        for _ in 0..50 {
            page.settle();
            page.runtime.run_network_until(None);
            if page.eval("out.includes('end') && out.some(l => l.startsWith('error'))") == "true" {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let out: Vec<String> = serde_json::from_str(&page.eval("JSON.stringify(out)")).unwrap();
        assert!(
            out.contains(&"load 200 GET application/json".to_owned()),
            "{out:?}"
        );
        assert!(out.contains(&"error 0".to_owned()), "{out:?}");
        let first = out.iter().position(|l| l == "state 1").unwrap();
        let done = out.iter().position(|l| l == "state 4").unwrap();
        let end = out.iter().position(|l| l == "end").unwrap();
        assert!(first < done && done < end, "{out:?}");
    });
}
