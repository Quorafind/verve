//! `Headers`, `Request`, `Response` and `FormData`, made and read without any network: bodies
//! from text, bytes, blobs, forms, search params and streams, read once each way.

mod common;

use common::run_async;

#[test]
fn headers_fold_case_sort_and_join() {
    let result = run_async(
        r#"
        const h = new Headers({ 'Content-Type': 'text/plain', 'X-B': '1' });
        h.append('x-b', '2');
        h.append('Set-Cookie', 'a=1');
        h.append('set-cookie', 'b=2');
        const fromPairs = new Headers([['a', ' spaced ']]);
        const copy = new Headers(h);
        copy.set('X-B', 'only');
        const seen = [];
        h.forEach((value, name) => seen.push(name + '=' + value));
        let bad;
        try { h.append('bad name', 'x'); } catch (e) { bad = e.constructor.name; }
        result = [
            h.get('content-type'), h.get('X-B'), h.has('x-b'), h.get('missing'),
            JSON.stringify([...h.keys()]), h.getSetCookie().join(';'), fromPairs.get('A'),
            copy.get('x-b'), h.get('x-b'), seen.join('&'), bad,
            JSON.stringify(Object.fromEntries(new Headers({ b: '2', a: '1' }))),
        ];
    "#,
    );
    assert_eq!(
        result,
        [
            "text/plain",
            "1, 2",
            "true",
            "null",
            r#"["content-type","set-cookie","set-cookie","x-b"]"#,
            "a=1;b=2",
            "spaced",
            "only",
            "1, 2",
            "content-type=text/plain&set-cookie=a=1&set-cookie=b=2&x-b=1, 2",
            "TypeError",
            r#"{"a":"1","b":"2"}"#,
        ]
    );
}

#[test]
fn responses_read_their_bodies_once_every_way() {
    let result = run_async(
        r#"
        const json = await new Response('{"a":1}').json();
        const text = new Response('hello');
        const t = await text.text();
        let again;
        try { await text.text(); } catch (e) { again = e.constructor.name; }
        const buffer = await new Response(new Uint8Array([1, 2, 3])).arrayBuffer();
        const bytes = await new Response('ab').bytes();
        const blob = await new Response('xyz', { headers: { 'Content-Type': 'Text/Plain' } }).blob();
        const fromBlob = await new Response(new Blob(['from blob'], { type: 'text/x' })).text();
        const stream = new ReadableStream({ start(c) { c.enqueue(new TextEncoder().encode('str')); c.enqueue(new TextEncoder().encode('eam')); c.close(); } });
        const fromStream = await new Response(stream).text();
        const r = new Response('body', { status: 201, statusText: 'Created', headers: { 'X-A': 'b' } });
        const viaBody = [];
        for await (const chunk of new Response('streamed').body) viaBody.push(new TextDecoder().decode(chunk));
        const cloned = new Response('twice');
        const copy = cloned.clone();
        const both = [await cloned.text(), await copy.text()];
        let badStatus;
        try { new Response('', { status: 99 }); } catch (e) { badStatus = e.constructor.name; }
        let nullBody;
        try { new Response('x', { status: 204 }); } catch (e) { nullBody = e.constructor.name; }
        const statics = [Response.error().type, Response.error().status, Response.redirect('https://e.com/x', 301).headers.get('location'), (await Response.json({ ok: true }).json()).ok, Response.json(1).headers.get('content-type')];
        result = [
            json.a, t, again, buffer.byteLength, bytes.constructor.name + bytes.length, blob.type, blob.size, fromBlob,
            fromStream, r.status, r.statusText, r.ok, r.headers.get('x-a'), r.type, r.bodyUsed, new Response(null).body,
            viaBody.join(''), both.join(','), badStatus, nullBody, statics.join(','),
            new Response('x').headers.get('content-type'),
        ];
    "#,
    );
    assert_eq!(
        result,
        [
            "1",
            "hello",
            "TypeError",
            "3",
            "Uint8Array2",
            "text/plain",
            "3",
            "from blob",
            "stream",
            "201",
            "Created",
            "true",
            "b",
            "default",
            "false",
            "null",
            "streamed",
            "twice,twice",
            "RangeError",
            "TypeError",
            "error,0,https://e.com/x,true,application/json",
            "text/plain;charset=UTF-8",
        ]
    );
}

#[test]
fn requests_resolve_urls_normalize_methods_and_carry_bodies() {
    let result = run_async(
        r#"
        const r = new Request('https://example.com/a/../b?q=1', { method: 'post', body: JSON.stringify({ x: 1 }), headers: { 'Content-Type': 'application/json' } });
        const copy = new Request(r, { headers: { 'X-Extra': '1' } });
        let getBody, badUrl, forbidden;
        try { new Request('https://e.com', { method: 'GET', body: 'x' }); } catch (e) { getBody = e.constructor.name; }
        try { new Request('not a url'); } catch (e) { badUrl = e.constructor.name; }
        try { new Request('https://e.com', { method: 'TRACE' }); } catch (e) { forbidden = e.constructor.name; }
        const controller = new AbortController();
        const withSignal = new Request('https://e.com', { signal: controller.signal });
        result = [
            r.url, r.method, new Request('https://e.com', { method: 'patch' }).method, r.headers.get('content-type'),
            copy.method, copy.headers.get('x-extra'), (await copy.json()).x, r.bodyUsed, getBody, badUrl, forbidden,
            withSignal.signal === controller.signal, typeof new Request('https://e.com').signal.aborted,
            new Request('https://e.com').credentials, new Request('https://e.com').body,
        ];
    "#,
    );
    assert_eq!(
        result,
        [
            "https://example.com/b?q=1",
            "POST",
            "patch",
            "application/json",
            "POST",
            "1",
            "1",
            "true",
            "TypeError",
            "TypeError",
            "TypeError",
            "true",
            "boolean",
            "same-origin",
            "null",
        ]
    );
}

#[test]
fn form_data_holds_text_and_files_and_round_trips() {
    let result = run_async(
        r#"
        const form = new FormData();
        form.append('name', 'Ada');
        form.append('tag', 'a');
        form.append('tag', 'b');
        form.append('file', new Blob(['file body'], { type: 'text/plain' }), 'note.txt');
        form.set('name', 'Grace');
        const file = form.get('file');
        const request = new Request('https://e.com/upload', { method: 'POST', body: form });
        const kind = request.headers.get('content-type');
        const back = await request.formData();
        const backFile = back.get('file');
        const encoded = await new Response(new URLSearchParams({ a: '1', b: 'x y' })).formData();
        const names = [];
        for (const [name] of form) names.push(name);
        result = [
            form.get('name'), form.getAll('tag').join(), form.has('tag'), file.name, file instanceof File, file.size,
            kind.startsWith('multipart/form-data; boundary='), back.get('name'), back.getAll('tag').join(),
            backFile.name, await backFile.text(), backFile.type, encoded.get('b'), names.join(),
            new Response(new URLSearchParams('q=1')).headers.get('content-type'),
        ];
    "#,
    );
    assert_eq!(
        result,
        [
            "Grace",
            "a,b",
            "true",
            "note.txt",
            "true",
            "9",
            "true",
            "Grace",
            "a,b",
            "note.txt",
            "file body",
            "text/plain",
            "x y",
            "name,tag,tag,file",
            "application/x-www-form-urlencoded;charset=UTF-8",
        ]
    );
}
