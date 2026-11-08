//! `innerText` as the rendered text collection steps give it. The expectations come from the
//! HTML specification's algorithm (blocks break lines, paragraphs leave a blank one, whitespace
//! collapses), not from a run in Chrome.

mod common;

use common::{Page, on_page};
use verve_dom::Options;

fn inner_text_of(html: &str) -> String {
    let html = html.to_owned();
    on_page(move || {
        let mut page = Page::new(Options::default());
        page.eval(&format!(
            "document.body.innerHTML = `{html}`; window.box = document.body.firstElementChild"
        ));
        page.eval("JSON.stringify(box.innerText)")
    })
}

#[test]
fn rendered_text_follows_the_boxes() {
    let samples: [(&str, &str); 10] = [
        ("<div>a<div>b</div>c</div>", r#""a\nb\nc""#),
        ("<div><p>one</p><p>two</p></div>", r#""one\n\ntwo""#),
        ("<div>a<br>b<br><br>c</div>", r#""a\nb\n\nc""#),
        ("<div>  lots   of\n  space  </div>", r#""lots of space""#),
        ("<div>a <b> b</b> <i>c </i>d</div>", r#""a b c d""#),
        (
            "<div>shown<span style='display:none'>hidden</span><script>x</script></div>",
            r#""shown""#,
        ),
        (
            "<div style='text-transform:uppercase'>up <em>and</em> away</div>",
            r#""UP AND AWAY""#,
        ),
        ("<pre>  keep\n  this</pre>", r#""  keep\n  this""#),
        (
            "<table><tr><td>a</td><td>b</td></tr><tr><td>c</td><td>d</td></tr></table>",
            r#""a\tb\nc\td""#,
        ),
        ("<ul><li>one</li><li>two</li></ul>", r#""one\ntwo""#),
    ];
    for (html, expected) in samples {
        assert_eq!(inner_text_of(html), expected, "{html}");
    }
}

#[test]
fn text_that_is_not_rendered_reads_as_its_text_content() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        assert_eq!(
            page.eval(
                r#"
                const el = document.createElement('div');
                el.innerHTML = 'a  <b>b</b>\n';
                const detached = JSON.stringify(el.innerText);
                el.style.display = 'none';
                document.body.append(el);
                detached + JSON.stringify(el.innerText)
                "#
            ),
            r#""a  b\n""a  b\n""#
        );
    });
}
