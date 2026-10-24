//! IndexedDB as the libraries plugins carry use it: an upgrade that makes stores and
//! indexes, transactions awaited across, cursors walked, a key range, an abort put back, and
//! databases that are there again in a runtime started later.

use verve::Runtime;

fn js(rt: &mut Runtime, source: &str) -> String {
    let value = rt.eval(source).unwrap_or_else(|e| panic!("{source}: {e}"));
    verve::print(&value)
}

/// Runs tasks until nothing is left to do.
fn settle(rt: &mut Runtime) {
    for _ in 0..500 {
        rt.run_microtasks();
        if rt.run_due() == 0 && rt.next_due().is_none() {
            break;
        }
    }
}

/// `promisify` for a request, and an `await`ed scenario whose outcome lands in `out`.
const PRELUDE: &str = r#"
    globalThis.req = (r) => new Promise((ok, no) => { r.onsuccess = () => ok(r.result); r.onerror = () => no(r.error); });
    globalThis.done = (tx) => new Promise((ok, no) => { tx.oncomplete = () => ok('complete'); tx.onabort = () => no('abort'); });
    globalThis.out = [];
    globalThis.openDb = (name, version, upgrade) => new Promise((ok, no) => {
        const r = indexedDB.open(name, version);
        r.onupgradeneeded = (e) => upgrade && upgrade(r.result, e, r.transaction);
        r.onsuccess = () => ok(r.result);
        r.onerror = () => no(r.error);
    });
"#;

fn outcome(rt: &mut Runtime) -> String {
    settle(rt);
    js(rt, "JSON.stringify(out)")
}

#[test]
fn stores_indexes_cursors_and_ranges() {
    let mut rt = Runtime::with_clock(|| 0.);
    js(&mut rt, PRELUDE);
    js(
        &mut rt,
        r#"
        (async () => {
            const db = await openDb('notes', 1, (db, e) => {
                out.push('upgrade ' + e.oldVersion + '->' + e.newVersion);
                const notes = db.createObjectStore('notes', { keyPath: 'id', autoIncrement: true });
                notes.createIndex('tags', 'tags', { multiEntry: true });
                notes.createIndex('path', 'path', { unique: true });
                db.createObjectStore('plain');
            });
            out.push([...db.objectStoreNames].join(','), db.version);

            // Written in one transaction, awaited between requests, as `idb` does.
            const tx = db.transaction(['notes', 'plain'], 'readwrite');
            const notes = tx.objectStore('notes');
            const a = await req(notes.add({ path: 'a.md', tags: ['x', 'y'], when: new Date(5) }));
            const b = await req(notes.put({ path: 'b.md', tags: ['y'], bytes: new Uint8Array([1, 2]) }));
            await req(notes.put({ path: 'c.md', tags: [], meta: new Map([['k', 1]]) }));
            await req(tx.objectStore('plain').put('hello', ['compound', 1]));
            out.push(a, b, await done(tx));

            // Read back, as values cloned.
            const read = db.transaction('notes').objectStore('notes');
            const first = await req(read.get(1));
            out.push(first.path, first.when instanceof Date && first.when.getTime(), first.id);
            const second = await req(read.get(2));
            out.push(second.bytes instanceof Uint8Array && [...second.bytes].join(''));
            out.push((await req(read.get(3))).meta.get('k'));
            out.push(await req(read.count()));
            out.push((await req(read.index('tags').getAll('y'))).map(n => n.path).join(','));
            out.push(await req(read.index('path').getKey('c.md')));
            out.push((await req(read.getAll(IDBKeyRange.bound(2, 3)))).length);
            out.push((await req(read.getAllKeys(IDBKeyRange.lowerBound(1, true)))).join(','));
            out.push(await req(db.transaction('plain').objectStore('plain').get(['compound', 1])));

            // A unique index refuses a second note at the same path.
            const clash = db.transaction('notes', 'readwrite');
            try { await req(clash.objectStore('notes').add({ path: 'a.md', tags: [] })); }
            catch (e) { out.push(e.name); }
            out.push(await done(clash).catch(e => e));

            // A cursor walked backwards, changing and deleting as it goes.
            const walk = db.transaction('notes', 'readwrite');
            const seen = [];
            await new Promise((ok) => {
                const r = walk.objectStore('notes').openCursor(null, 'prev');
                r.onsuccess = () => {
                    const c = r.result;
                    if (!c) return ok();
                    seen.push(c.key + ':' + c.value.path);
                    if (c.key === 2) c.update({ ...c.value, path: 'B.md' });
                    if (c.key === 3) c.delete();
                    c.continue();
                };
            });
            await done(walk);
            out.push(seen.join(' '));
            const after = db.transaction('notes').objectStore('notes');
            out.push((await req(after.getAll())).map(n => n.path).join(','));

            // Aborted: put back as it was.
            const undo = db.transaction('notes', 'readwrite');
            undo.objectStore('notes').clear();
            undo.abort();
            out.push(await req(db.transaction('notes').objectStore('notes').count()));

            out.push(indexedDB.cmp(1, 'a'), indexedDB.cmp([1, 2], [1, 2]), indexedDB.cmp('b', 'a'));
        })().catch(e => out.push('failed ' + e));
    "#,
    );
    assert_eq!(
        outcome(&mut rt),
        r#"["upgrade 0->1","notes,plain",1,1,2,"complete","a.md",5,1,"12",1,3,"a.md,b.md",3,2,"2,3","hello","ConstraintError","abort","3:c.md 2:b.md 1:a.md","a.md,B.md",2,-1,0,1]"#
    );
}

#[test]
fn databases_are_kept_on_disk() {
    let dir = std::env::temp_dir().join(format!("verve-idb-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    {
        let mut rt = Runtime::with_clock(|| 0.);
        rt.set_storage_dir(Some(dir.clone()));
        js(&mut rt, PRELUDE);
        js(
            &mut rt,
            r#"
            (async () => {
                const db = await openDb('kept', 2, (db) => db.createObjectStore('kv'));
                const tx = db.transaction('kv', 'readwrite');
                tx.objectStore('kv').put({ n: 42, list: [1, 'two'] }, 'answer');
                out.push(await done(tx));
            })().catch(e => out.push('failed ' + e));
        "#,
        );
        assert_eq!(outcome(&mut rt), r#"["complete"]"#);
    }
    let mut rt = Runtime::with_clock(|| 0.);
    rt.set_storage_dir(Some(dir.clone()));
    js(&mut rt, PRELUDE);
    js(
        &mut rt,
        r#"
        (async () => {
            // Opened again with no version: no upgrade, the version it had.
            const db = await openDb('kept', undefined, () => out.push('upgraded?'));
            out.push(db.version);
            const value = await req(db.transaction('kv').objectStore('kv').get('answer'));
            out.push(value.n, value.list[1]);
            out.push((await indexedDB.databases()).map(d => d.name + '@' + d.version).join());
            // A lower version is refused.
            await openDb('kept', 1).catch(e => out.push(e.name));
            await req(indexedDB.deleteDatabase('kept'));
            out.push((await indexedDB.databases()).length);
        })().catch(e => out.push('failed ' + e));
    "#,
    );
    assert_eq!(
        outcome(&mut rt),
        r#"[2,42,"two","kept@2","VersionError",0]"#
    );
    let _ = std::fs::remove_dir_all(&dir);
}
