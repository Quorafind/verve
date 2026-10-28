//! Styles: a `<style>` element's rules and inline styles cascading into computed style, with
//! `var()`, inheritance, `!important`, `:hover` from the input interface, `@media` following
//! the viewport — as the drawer reads them in a snapshot, and as `getComputedStyle` answers.

mod common;

use common::{Page, on_page};
use verve_dom::{Options, Pointer, Rect};

#[test]
fn review_relative_has_and_selector_backtracking() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        assert_eq!(page.eval(r#"
            (() => {
                const root = document.createElement('section');
                root.innerHTML = '<div id=a><p><span></span></p></div><a></a><b><i></i></b>';
                const outside = document.createElement('p');
                outside.innerHTML = '<div id=b><span></span></div>';
                root.append(outside);
                const a = root.querySelector('#a'), b = root.querySelector('#b');
                const out = [a.matches('div:has(> p span)'), b.matches('div:has(> p span)'),
                    root.querySelector('a').matches('a:has(+ b i)'), b.matches(':has(p span)')];
                try { root.querySelector(':has(:is(:has(i)))'); out.push(false); }
                catch (e) { out.push(e.name === 'SyntaxError'); }
                root.innerHTML = '<b>'.repeat(80);
                out.push(root.querySelectorAll('b')[79].matches('x ' + Array(40).fill('b').join(' ')));
                return out.join('|');
            })()
        "#), "true|false|true|false|true|false");
    });
}

#[test]
fn review_variables_cycles_fallback_and_expansion_limit() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        assert_eq!(page.eval(r#"
            (() => {
                const p = document.createElement('div');
                p.style.cssText = '--a: var(--b); --b: var(--a); color: var(--a, red); --ok: ; width: var(--missing, 3px)';
                document.body.appendChild(p);
                const out = [getComputedStyle(p).color, getComputedStyle(p).width];
                p.style.cssText = '--a: var(--a) var(--a); color: var(--a, blue)';
                out.push(getComputedStyle(p).color);
                p.style.cssText = '--a: var(--missing, var(--a)); color: var(--a, green)';
                out.push(getComputedStyle(p).color);
                p.style.cssText = '--a: var(--b, var(--c)); --b: var(--a); --c: var(--b, red); --safe: var(--c, orange); color: var(--c, purple); background-color: var(--safe)';
                out.push(getComputedStyle(p).color, getComputedStyle(p).backgroundColor);
                let at = p;
                p.style.cssText = '--v0: x';
                for (let n = 1; n <= 24; n++) {
                    const next = document.createElement('div');
                    next.style.cssText = '--v' + n + ': var(--v' + (n-1) + ') var(--v' + (n-1) + ')';
                    at.appendChild(next); at = next;
                }
                at.style.color = 'var(--v24, purple)';
                out.push(getComputedStyle(at).color);
                return out.join('|');
            })()
        "#), "red|3px|blue|green|purple|orange|purple");
    });
}

#[test]
fn the_font_shorthand_opens_into_its_longhands_after_var() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval(
            r#"
            document.head.innerHTML = `<style>
              :root { --type: 500 13px/20px "Segoe UI", sans-serif; }
              .scale { font: var(--type); }
              .later { font: italic 12px/18px mono; font-size: 20px; }
              .earlier { font-weight: 300; font: 14px serif; }
            </style>`;
            document.body.innerHTML = `
              <div id="scale" class="scale"><span id="child">child</span></div>
              <div id="later" class="later">later</div>
              <div id="earlier" class="earlier">earlier</div>`;
        "#,
        );
        let style = |page: &mut Page, id: &str, property: &str| {
            page.eval(&format!(
                "getComputedStyle(document.getElementById('{id}')).getPropertyValue('{property}')"
            ))
        };
        assert_eq!(style(&mut page, "scale", "font-size"), "13px");
        assert_eq!(style(&mut page, "scale", "line-height"), "20px");
        assert_eq!(style(&mut page, "scale", "font-weight"), "500");
        assert_eq!(
            style(&mut page, "scale", "font-family"),
            "\"Segoe UI\", sans-serif"
        );
        // 简写设置的长属性正常继承，简写之后的长属性优先，之前的则重置。
        assert_eq!(style(&mut page, "child", "font-size"), "13px");
        assert_eq!(style(&mut page, "child", "font-weight"), "500");
        assert_eq!(style(&mut page, "later", "font-size"), "20px");
        assert_eq!(style(&mut page, "later", "font-style"), "italic");
        assert_eq!(style(&mut page, "earlier", "font-weight"), "normal");
        assert_eq!(style(&mut page, "earlier", "font-size"), "14px");
        let snapshot = page.document.update().snapshot;
        assert_eq!(
            snapshot.style(page.find("#scale"), "font-size"),
            Some("13px")
        );
        assert_eq!(page.console(), "");
    });
}

#[test]
fn review_stylesheets_and_sibling_indices_remain_fresh() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        assert_eq!(
            page.eval(
                r#"
            (() => {
                for (let n = 0; n < 1000; n++) {
                    const style = document.createElement('style');
                    style.textContent = '#target { color: red }';
                    document.head.appendChild(style);
                }
                document.body.innerHTML = '<div id=target></div>';
                const target = document.getElementById('target');
                for (let n = 0; n < 20; n++) {
                    if (getComputedStyle(target).color !== 'red') throw Error('样式缓存');
                }
                const last = document.head.lastChild;
                last.firstChild.data = '#target { color: blue }';
                const out = [getComputedStyle(target).color];
                document.head.prepend(last);
                out.push(getComputedStyle(target).color);
                target.innerHTML = '<i class=ok></i>'.repeat(4000);
                out.push(target.querySelectorAll('x ~ i').length);
                out.push(target.querySelectorAll(':nth-child(2n of .ok)').length);
                return out.join('|');
            })()
        "#
            ),
            "blue|red|0|2000"
        );
    });
}

const PAGE: &str = r#"
document.head.innerHTML = `<style>
  :root { --accent: rgb(0, 128, 0); --pad: 6px; }
  .card { color: var(--accent); padding: var(--pad) calc(2 * var(--pad)); font-size: 14px; border: 1px solid var(--missing, blue); }
  .card .title { font-weight: bold; }
  .card > p:hover { color: rgb(255, 0, 0); }
  .loud { color: purple !important; }
  @media (max-width: 600px) { .card { font-size: 12px; } }
  #main.card { margin: 0 auto; }
</style>`;
document.body.innerHTML = `
  <section id="main" class="card">
    <h2 class="title">Title <span id="deep">deep</span></h2>
    <p id="para">text</p>
    <p id="inline" style="color: orange">inline</p>
    <p id="loud" class="loud" style="color: orange">loud</p>
  </section>`;
"#;

#[test]
fn rules_inline_styles_and_inheritance_cascade_into_computed_style() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval(PAGE);
        let snapshot = page.document.update().snapshot;
        let main = page.find("#main");
        let deep = page.find("#deep");
        let inline = page.find("#inline");
        let loud = page.find("#loud");
        // `var()`, with its fallback, and the shorthands opened up.
        assert_eq!(snapshot.style(main, "color"), Some("rgb(0, 128, 0)"));
        assert_eq!(snapshot.style(main, "padding-top"), Some("6px"));
        assert_eq!(snapshot.style(main, "padding-left"), Some("calc(2 * 6px)"));
        assert_eq!(snapshot.style(main, "border-top-color"), Some("blue"));
        assert_eq!(snapshot.style(main, "margin-left"), Some("auto"));
        // Inherited down two levels; a property that does not inherit is not.
        assert_eq!(snapshot.style(deep, "color"), Some("rgb(0, 128, 0)"));
        // A relative size computed to pixels before it is inherited: the heading's 1.5em of 14px.
        assert_eq!(snapshot.style(deep, "font-size"), Some("21px"));
        assert_eq!(snapshot.style(deep, "font-weight"), Some("bold"));
        assert_eq!(snapshot.style(deep, "padding-top"), None);
        // The user agent's own sheet under the page's.
        assert_eq!(snapshot.style(main, "display"), Some("block"));
        assert_eq!(snapshot.style(deep, "display"), None);
        // The inline style over every rule; an important rule over the inline style.
        assert_eq!(snapshot.style(inline, "color"), Some("orange"));
        assert_eq!(snapshot.style(loud, "color"), Some("purple"));
        // getComputedStyle answers the same, with initial values for what nothing set.
        assert_eq!(
            page.eval("(() => { const s = getComputedStyle(document.getElementById('deep')); return [s.color, s.fontSize, s.display, s.getPropertyValue('--accent'), s.position].join('|') })()"),
            "rgb(0, 128, 0)|21px|inline|rgb(0, 128, 0)|static"
        );
        assert_eq!(page.console(), "");
    });
}

#[test]
fn hover_from_the_pointer_and_media_from_the_viewport_restyle() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval(PAGE);
        let para = page.find("#para");
        page.document
            .set_layout(&mut page.runtime, [(para, Rect::new(0., 100., 300., 20.))]);
        let update = page.document.update();
        assert!(update.changes.restyled_all);
        assert_eq!(update.snapshot.style(para, "color"), Some("rgb(0, 128, 0)"));
        assert!(page.document.hover_sensitive(para));
        // The pointer onto it: `:hover` applies, and only what changed is restyled.
        page.document.pointer_move(
            &mut page.runtime,
            &Pointer {
                x: 10.,
                y: 110.,
                ..Pointer::default()
            },
        );
        let update = page.document.update();
        assert_eq!(update.snapshot.style(para, "color"), Some("rgb(255, 0, 0)"));
        assert!(
            update.changes.restyled.contains(&para),
            "{:?}",
            update.changes
        );
        assert!(!update.changes.restyled_all);
        assert_eq!(
            page.eval("getComputedStyle(document.getElementById('para')).color"),
            "rgb(255, 0, 0)"
        );
        page.document.pointer_leave(&mut page.runtime);
        let update = page.document.update();
        assert_eq!(update.snapshot.style(para, "color"), Some("rgb(0, 128, 0)"));
        // A narrower viewport: the `@media` rule applies, and `matchMedia` hears it.
        page.eval("globalThis.heard = []; matchMedia('(max-width: 600px)').addEventListener('change', (e) => heard.push(e.matches))");
        let main = page.find("#main");
        page.document.set_viewport(&mut page.runtime, 500., 800.);
        let update = page.document.update();
        assert_eq!(update.snapshot.style(main, "font-size"), Some("12px"));
        assert_eq!(page.eval("heard.join(',') + ' ' + innerWidth"), "true 500");
        // A sheet added later restyles what it matches.
        page.eval("const s = document.createElement('style'); s.textContent = '#para { text-decoration: underline }'; document.head.append(s)");
        let update = page.document.update();
        assert_eq!(
            update.snapshot.style(para, "text-decoration"),
            Some("underline")
        );
        // And one the embedder adds, under the page's: what the page says too stays the page's.
        page.document.set_stylesheet(
            "theme",
            ":root { --accent: rgb(1, 2, 3) } #para { text-decoration: none; letter-spacing: 1px }",
        );
        let update = page.document.update();
        assert_eq!(update.snapshot.style(para, "color"), Some("rgb(0, 128, 0)"));
        assert_eq!(
            update.snapshot.style(para, "text-decoration"),
            Some("underline")
        );
        assert_eq!(update.snapshot.style(para, "letter-spacing"), Some("1px"));
        assert_eq!(page.console(), "");
    });
}

#[test]
fn a_change_dirties_only_its_partition_and_restyles_only_its_subtree() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval(PAGE);
        page.eval(
            "document.body.insertAdjacentHTML('beforeend', '<aside id=side><b>x</b></aside>')",
        );
        let main = page.find("#main");
        let side = page.find("#side");
        page.document.set_partition(main, true);
        page.document.set_partition(side, true);
        page.document.update();
        page.eval("document.getElementById('deep').textContent = 'changed'");
        let changes = page.document.update().changes;
        assert_eq!(changes.partitions, vec![main]);
        assert!(page.document.update().changes.is_empty());
        page.eval("document.querySelector('#side b').classList.add('on')");
        let changes = page.document.update().changes;
        assert_eq!(changes.partitions, vec![side]);
        assert!(!changes.restyled.contains(&main));
    });
}

#[test]
fn the_font_shorthand_opens_into_its_longhands_after_var() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval(
            r#"
            document.head.innerHTML = `<style>
              :root { --type: 500 13px/20px "Segoe UI", sans-serif; }
              .scale { font: var(--type); }
              .later { font: italic 12px/18px mono; font-size: 20px; }
              .earlier { font-weight: 300; font: 14px serif; }
            </style>`;
            document.body.innerHTML = `
              <div id="scale" class="scale"><span id="child">child</span></div>
              <div id="later" class="later">later</div>
              <div id="earlier" class="earlier">earlier</div>`;
            "#,
        );
        let style = |page: &mut Page, id: &str, property: &str| {
            page.eval(&format!(
                "getComputedStyle(document.getElementById('{id}')).getPropertyValue('{property}')"
            ))
        };
        assert_eq!(style(&mut page, "scale", "font-size"), "13px");
        assert_eq!(style(&mut page, "scale", "line-height"), "20px");
        assert_eq!(style(&mut page, "scale", "font-weight"), "500");
        assert_eq!(
            style(&mut page, "scale", "font-family"),
            "\"Segoe UI\", sans-serif"
        );
        // What the shorthand set is inherited as any longhand is.
        assert_eq!(style(&mut page, "child", "font-size"), "13px");
        assert_eq!(style(&mut page, "child", "font-weight"), "500");
        // A longhand after the shorthand wins over it; one before is reset by it.
        assert_eq!(style(&mut page, "later", "font-size"), "20px");
        assert_eq!(style(&mut page, "later", "font-style"), "italic");
        assert_eq!(style(&mut page, "earlier", "font-weight"), "normal");
        assert_eq!(style(&mut page, "earlier", "font-size"), "14px");
        assert_eq!(page.console(), "");
    });
}
