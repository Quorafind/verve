//! `document.fonts`: the set a page adds faces to, and what it says of their loading.

mod common;

use common::{Page, on_page};
use verve_dom::{FontOutcome, Options, Request};

#[test]
fn faces_given_bytes_are_loaded_and_the_set_answers_for_them() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        assert_eq!(
            page.eval(
                r#"
                const face = new FontFace('Brand', new ArrayBuffer(8), { weight: '700' });
                const fonts = document.fonts;
                const before = [fonts.size, fonts.has(face), fonts.check('16px Brand')];
                fonts.add(face);
                [before.join(), face.status, face.weight, face.style, fonts.size, fonts.has(face),
                 fonts.check('bold 16px "Brand", serif'), fonts.status, [...fonts].length,
                 fonts.add(face) === fonts, fonts.size, fonts.delete(face), fonts.size].join(' ')
                "#
            ),
            "0,false,true loaded 700 normal 1 true true loaded 1 true 1 true 0"
        );
    });
}

#[test]
fn a_face_with_an_address_loads_until_the_host_says_so() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval(
            r#"window.seen = [];
               window.face = new FontFace('Remote', 'url(remote.woff2)');
               for (const kind of ['loading', 'loadingdone', 'loadingerror'])
                   document.fonts.addEventListener(kind, () => seen.push(kind));
               document.fonts.add(face);"#,
        );
        assert_eq!(
            page.eval(
                "[face.status, document.fonts.status, document.fonts.check('12px Remote')].join()"
            ),
            "unloaded,loaded,false"
        );
        page.eval(
            "window.loading = face.load(); document.fonts.ready.then(() => seen.push('ready'))",
        );
        assert_eq!(
            page.eval("[face.status, document.fonts.status].join()"),
            "loading,loading"
        );
        page.runtime.run_microtasks();
        assert_eq!(page.eval("seen.join()"), "loading");
        let asked = page.document.take_requests();
        let [Request::LoadFont { face, url }] = &asked[..] else {
            panic!("the host is asked for the font once: {asked:?}");
        };
        assert!(url.ends_with("remote.woff2"), "{url}");
        page.document
            .settle_font(&mut page.runtime, *face, FontOutcome::Loaded);
        assert_eq!(
            page.eval("[face.status, document.fonts.status, document.fonts.check('12px Remote'), seen.join()].join(' ')"),
            "loaded loaded true loading,loadingdone,ready"
        );
    });
}

#[test]
fn the_host_takes_the_bytes_of_a_face_and_says_whether_it_loaded() {
    on_page(|| {
        use std::cell::RefCell;
        use std::rc::Rc;
        let mut page = Page::new(Options::default());
        let taken = Rc::new(RefCell::new(Vec::new()));
        let record = Rc::clone(&taken);
        page.document.set_font_provider(Some(Box::new(move |font| {
            record.borrow_mut().push((
                font.family.clone(),
                font.weight,
                font.italic,
                font.bytes.len(),
            ));
            font.family != "Broken"
        })));
        page.eval(
            r#"window.good = new FontFace('"Brand"', new ArrayBuffer(8),
                   { weight: 'bold', style: 'italic' });
               window.bad = new FontFace('Broken', new ArrayBuffer(4));
               window.remote = new FontFace('Remote', 'url(remote.ttf)', { weight: '300 500' });
               window.states = [];
               bad.loaded.catch(() => states.push('bad failed'));
               remote.load().then(() => states.push('remote loaded'));"#,
        );
        page.runtime.run_microtasks();
        assert_eq!(
            page.eval("[good.status, bad.status, remote.status].join()"),
            "loaded,error,loading"
        );
        let asked = page.document.take_requests();
        let [Request::LoadFont { face, .. }] = &asked[..] else {
            panic!("one fetch: {asked:?}");
        };
        page.document
            .settle_font_bytes(&mut page.runtime, *face, vec![0; 12]);
        assert_eq!(page.eval("states.join()"), "bad failed,remote loaded");
        assert_eq!(
            *taken.borrow(),
            [
                ("Brand".to_owned(), 700., true, 8),
                ("Broken".to_owned(), 400., false, 4),
                ("Remote".to_owned(), 300., false, 12),
            ]
        );
    });
}
