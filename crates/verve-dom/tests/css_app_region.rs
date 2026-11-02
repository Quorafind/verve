//! `-webkit-app-region`: Electron's way to say which boxes of a frameless window drag it. It
//! must come through the cascade as an ordinary property, and not be inherited — a drawer
//! works the drag region out as a box less the no-drag boxes inside it, so a child that
//! inherited `drag` would be one.

mod common;

use common::{on_page, style, styled};

#[test]
fn the_app_region_survives_the_cascade() {
    on_page(|| {
        let page = styled(
            ".bar { -webkit-app-region: drag }
             .bar button { -webkit-app-region: no-drag }
             .plain { -webkit-app-region: none }",
            r#"<div id="bar" class="bar"><button id="button">b</button><span id="span">s</span></div>
               <div id="plain" class="plain"></div>
               <div id="child"></div>"#,
        );
        let snapshot = page.document.update().snapshot;
        let region = |selector: &str| snapshot.style(page.find(selector), "-webkit-app-region");
        assert_eq!(region("#bar"), Some("drag"));
        assert_eq!(region("#button"), Some("no-drag"));
        assert_eq!(region("#plain"), Some("none"));
        // Not inherited: what is in a drag box and says nothing about it has no region.
        assert_eq!(region("#span"), None);
        assert_eq!(region("#child"), None);
    });
}

#[test]
fn the_app_region_comes_through_inline_styles_variables_and_layers() {
    on_page(|| {
        let mut page = styled(
            "@layer base { .a { -webkit-app-region: drag } }
             :root { --region: no-drag }
             .b { -webkit-app-region: var(--region) }
             .c { -webkit-app-region: drag !important }",
            r#"<div id="a" class="a"></div><div id="b" class="b"></div>
               <div id="c" class="c" style="-webkit-app-region: no-drag"></div>
               <div id="d" style="-webkit-app-region: drag"></div>"#,
        );
        assert_eq!(style(&mut page, "a", "-webkit-app-region"), "drag");
        assert_eq!(style(&mut page, "b", "-webkit-app-region"), "no-drag");
        // `!important` in a sheet beats the element's own style.
        assert_eq!(style(&mut page, "c", "-webkit-app-region"), "drag");
        assert_eq!(style(&mut page, "d", "-webkit-app-region"), "drag");
        // Chrome answers `none` for an element that says nothing.
        assert_eq!(
            page.eval("getComputedStyle(document.body).getPropertyValue('-webkit-app-region')"),
            "none"
        );
        // Script sets it through the style object, and the cascade sees it.
        page.eval("document.getElementById('b').style.setProperty('-webkit-app-region', 'drag')");
        assert_eq!(style(&mut page, "b", "-webkit-app-region"), "drag");
    });
}
