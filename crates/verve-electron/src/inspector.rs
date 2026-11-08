//! The text dump that stands in for DevTools' Elements panel (`webContents.openDevTools()`, P43).
//!
//! One line for each element of the page: two spaces of indent for each level, the tag, `#id`,
//! `.class` for each class, the box the page's layout gives it as `x,y,width,height` in CSS pixels
//! (`-` for an element with no box), and the computed `display`. The page's own thread
//! writes it, from the document as it is when the message arrives.
//!
//! It is also the page's side of the inspector window's REPL: [`eval`] runs what the window typed
//! in the page's own world and answers with text.

use boa_engine::{Context, Source};
use verve::{MAIN_WORLD, Runtime};
use verve_dom::{Document, NodeId, Rect, Snapshot};

use crate::api::ipc_renderer::outbox;
use crate::ids::ContentsId;
use crate::log::Log;
use crate::msg::FromRenderer;

/// Writes the element tree of `document` to the terminal, or with `at` the line of the element
/// under that point of the viewport, if there is one.
pub(crate) fn dump(document: &Document, log: &Log, contents: ContentsId, at: Option<(f32, f32)>) {
    let snapshot = document.snapshot();
    match at {
        None => {
            log.terminal(&format!(
                "[devtools:{contents}] element tree of {}",
                snapshot.url
            ));
            for line in tree(&snapshot, |node| document.rect(node)) {
                log.terminal(&line);
            }
        }
        Some((x, y)) => {
            if let Some(node) = document.hit_test(x, y) {
                log.terminal(&line_of(&snapshot, node, 0, document.rect(node)));
            }
        }
    }
}

/// Runs `code` as a script in the page's main world and answers [`FromRenderer::InspectorEvalReply`]
/// `call` with what it gave as `util.inspect` shows it, or with the error it threw shown the same
/// way. The page's console hears nothing of it, and the answer is text, never an object. The
/// microtasks the script queued run after the answer is made, so a promise it returned shows as
/// the state it had when it returned.
pub(crate) fn eval(runtime: &mut Runtime, call: u64, code: &str) {
    let (text, is_error) = shown(runtime.context(), code);
    outbox(runtime.context())
        .borrow_mut()
        .push(FromRenderer::InspectorEvalReply {
            call,
            text,
            is_error,
        });
    runtime.run_microtasks();
}

/// What running `code` in the main world comes to as text, and whether it threw.
fn shown(context: &mut Context, code: &str) -> (String, bool) {
    verve::in_world(context, MAIN_WORLD, |cx| {
        match cx.eval(Source::from_bytes(code)) {
            Ok(value) => (verve::inspect_value(cx, &value), false),
            Err(error) => {
                // An error that cannot be turned into a value is shown as the engine words it.
                let text = match error.clone().into_opaque(cx) {
                    Ok(value) => verve::inspect_value(cx, &value),
                    Err(_) => error.to_string(),
                };
                (text, true)
            }
        }
    })
}

/// The lines of the elements under the document, in document order.
fn tree(snapshot: &Snapshot, rect: impl Fn(NodeId) -> Option<Rect>) -> Vec<String> {
    let dom = &snapshot.dom;
    let mut lines = Vec::new();
    let mut pending: Vec<(NodeId, usize)> = dom
        .children(dom.document)
        .iter()
        .rev()
        .map(|node| (*node, 0))
        .collect();
    while let Some((node, depth)) = pending.pop() {
        if !dom.get(node).is_some_and(|n| n.is_element()) {
            continue;
        }
        lines.push(line_of(snapshot, node, depth, rect(node)));
        pending.extend(dom.children(node).iter().rev().map(|c| (*c, depth + 1)));
    }
    lines
}

/// The line of one element: `  div#main.wide.dark 8,8,784,16 block`.
fn line_of(snapshot: &Snapshot, node: NodeId, depth: usize, rect: Option<Rect>) -> String {
    let mut line = "  ".repeat(depth);
    if let Some(element) = snapshot.dom.get(node) {
        line.push_str(element.tag().unwrap_or("?"));
        if let Some(id) = element.attr("id").filter(|id| !id.is_empty()) {
            line.push_str(&format!("#{id}"));
        }
        for class in &element.classes {
            line.push_str(&format!(".{class}"));
        }
    }
    let display = snapshot.style(node, "display").unwrap_or("inline");
    format!("{line} {} {display}", box_text(rect))
}

/// `x,y,width,height` rounded to a tenth of a pixel, or `-`.
fn box_text(rect: Option<Rect>) -> String {
    let Some(rect) = rect else {
        return "-".to_owned();
    };
    let numbers = [rect.x, rect.y, rect.width, rect.height];
    let text: Vec<String> = numbers.iter().map(|n| number(*n)).collect();
    text.join(",")
}

fn number(value: f32) -> String {
    let tenths = (value * 10.).round() / 10.;
    if tenths.fract() == 0. {
        format!("{tenths:.0}")
    } else {
        format!("{tenths}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_box_is_four_numbers_to_a_tenth_and_a_missing_one_is_a_dash() {
        assert_eq!(
            box_text(Some(Rect::new(8., 8., 784.04, 16.25))),
            "8,8,784,16.3"
        );
        assert_eq!(box_text(Some(Rect::new(0., 0., 0., 0.))), "0,0,0,0");
        assert_eq!(box_text(None), "-");
    }
}
