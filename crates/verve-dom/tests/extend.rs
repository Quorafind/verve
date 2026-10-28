//! An embedder's own methods on the document's classes, through `verve_dom::host`: what
//! su-editor's Obsidian host adds (`createEl`, `setText`, `empty`), written outside the crate
//! against the same arena.

mod common;

use common::{Page, on_page};
use verve::boa_engine::{Context, JsResult, JsString, JsValue};
use verve_dom::Options;
use verve_dom::host::{self, Accessor, Method};

fn arg(args: &[JsValue], at: usize) -> JsValue {
    args.get(at).cloned().unwrap_or_default()
}

fn text(value: &JsValue, context: &mut Context) -> JsResult<String> {
    Ok(value.to_string(context)?.to_std_string_escaped())
}

/// `el.createEl(tag, { cls, text })`: an element made, dressed and put last in `el`.
fn create_el(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let parent = host::node_of(this);
    let tag = text(&arg(args, 0), context)?;
    let info = arg(args, 1);
    let cls = match info.as_object() {
        Some(info) => info.get(JsString::from("cls"), context)?,
        None => JsValue::undefined(),
    };
    let content = match info.as_object() {
        Some(info) => info.get(JsString::from("text"), context)?,
        None => JsValue::undefined(),
    };
    let cls = if cls.is_undefined() {
        String::new()
    } else {
        text(&cls, context)?
    };
    let content = if content.is_undefined() {
        None
    } else {
        Some(text(&content, context)?)
    };
    let id = host::with_dom(context, |dom| {
        let id = dom.create_element(&tag);
        for class in cls.split_whitespace() {
            dom.add_class(id, class);
        }
        if let Some(content) = &content {
            dom.set_text(id, content);
        }
        if let Some(parent) = parent {
            dom.append(parent, id);
        }
        id
    });
    Ok(host::wrap(context, id))
}

const METHODS: &[Method] = &[
    ("createEl", 2, create_el),
    ("setText", 1, |this, args, cx| {
        let id = host::node_of(this).unwrap();
        let value = text(&arg(args, 0), cx)?;
        host::with_dom(cx, |dom| dom.set_text(id, &value));
        Ok(JsValue::undefined())
    }),
    ("empty", 0, |this, _, cx| {
        let id = host::node_of(this).unwrap();
        host::with_dom(cx, |dom| dom.empty(id));
        Ok(JsValue::undefined())
    }),
];

const ACCESSORS: &[Accessor] = &[(
    "childCount",
    Some(|this, _, cx| {
        let id = host::node_of(this).unwrap();
        Ok(host::read_dom(cx, |dom| dom.children(id).len()).into())
    }),
    None,
)];

#[test]
fn an_embedder_adds_its_own_methods_to_the_dom() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        host::extend(page.runtime.context(), "Element", METHODS, ACCESSORS).unwrap();
        assert!(host::extend(page.runtime.context(), "NoSuchClass", METHODS, &[]).is_err());
        let out = page.eval(
            r#"
            const list = document.body.createEl('ul', { cls: 'list' });
            for (const word of ['a', 'b']) list.createEl('li', { text: word });
            const heard = [];
            const observer = new MutationObserver((records) => heard.push(records.length));
            observer.observe(list, { childList: true });
            list.firstChild.setText('A');
            const answer = [list.childCount, list.textContent, document.querySelector('.list > li') === list.firstChild];
            list.empty();
            answer.push(list.childCount);
            answer.join(' ')
            "#,
        );
        assert_eq!(out, "2 Ab true 0");
        page.runtime.run_microtasks();
        // One record for each child `empty` took out, none for the text set inside a child.
        assert_eq!(page.eval("heard.join(',')"), "2");
    });
}

#[test]
fn an_embedder_empty_keeps_per_child_mutations_and_siblings() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        host::extend(page.runtime.context(), "Element", METHODS, ACCESSORS).unwrap();
        let out = page.eval(
            r#"
            const list = document.body.createEl('ul');
            const children = ['a', 'b', 'c'].map(text => list.createEl('li', { text }));
            const observer = new MutationObserver(() => {});
            observer.observe(list, { childList: true });
            list.empty();
            const records = observer.takeRecords();
            const answer = [
                list.childCount,
                records.length,
                records.every((r, i) => r.type === 'childList' && r.target === list &&
                    r.addedNodes.length === 0 && r.removedNodes.length === 1 &&
                    r.removedNodes[0] === children[i] && r.previousSibling === null &&
                    r.nextSibling === (children[i + 1] || null)),
                children.every(child => child.parentNode === null && !child.isConnected),
            ];
            list.empty();
            answer.push(observer.takeRecords().length);
            observer.disconnect();
            answer.join(' ')
            "#,
        );
        assert_eq!(out, "0 3 true true 0");
    });
}
