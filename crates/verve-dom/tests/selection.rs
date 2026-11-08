//! Live `Range`s and the `Selection`: boundaries that follow the tree as it changes, the range
//! operations, and the selection's own methods.
//!
//! The numbers for `Selection.modify` were observed in Electron 39.8.10 (Windows 10, a hidden
//! window, probe `p24/probe3`): on `hello world, foo-bar baz` from 0, `extend forward word`
//! lands on 6, 11, 13 (a word, then the run of letters, then the punctuation and its space);
//! `move forward word` lands on 6 then 11 and `move backward word` goes back to 6; `lineboundary`
//! goes to the ends of the line; `direction` is `backward` after `setBaseAndExtent` with the
//! focus before the anchor and `none` when collapsed.

mod common;

use common::{Page, on_page};
use verve_dom::Options;

fn page() -> Page {
    Page::new(Options::default())
}

#[test]
fn a_range_follows_nodes_inserted_and_removed_before_it() {
    on_page(|| {
        let mut page = page();
        page.eval(
            "document.body.innerHTML = '<div id=d><b>a</b><i>b</i><u>c</u></div>';
             var d = document.getElementById('d');
             var r = new Range(); r.setStart(d, 1); r.setEnd(d, 3);",
        );
        // Inserted before the range: both ends move along.
        assert_eq!(
            page.eval("d.insertBefore(document.createElement('s'), d.firstChild); [r.startOffset, r.endOffset].join()"),
            "2,4"
        );
        // Removed before it: they move back.
        assert_eq!(
            page.eval("d.removeChild(d.firstChild); [r.startOffset, r.endOffset].join()"),
            "1,3"
        );
        // A boundary inside a removed node goes to where the node was.
        let inside = page.eval(
            "r.setStart(d.children[1].firstChild, 0); d.removeChild(d.children[1]);
             [r.startContainer === d, r.startOffset, r.endOffset].join()",
        );
        assert_eq!(inside, "true,1,2");
    });
}

#[test]
fn text_edits_and_splits_move_the_boundaries_inside_the_text() {
    on_page(|| {
        let mut page = page();
        page.eval(
            "document.body.innerHTML = 'hello';
             var t = document.body.firstChild;
             var r = new Range(); r.setStart(t, 1); r.setEnd(t, 4);",
        );
        // replaceData(0, 2, 'XYZ'): a boundary inside the replaced part goes to its start, one
        // after it moves by the change in length.
        assert_eq!(
            page.eval("t.replaceData(0, 2, 'XYZ'); [r.startOffset, r.endOffset].join()"),
            "0,5"
        );
        page.eval(
            "t.data = 'abcdef'; r.setStart(t, 1); r.setEnd(t, 5); var tail = t.splitText(3);",
        );
        assert_eq!(
            page.eval("[r.startContainer === t, r.startOffset, r.endContainer === tail, r.endOffset].join()"),
            "true,1,true,2"
        );
        // normalize() joins them again; the end follows into the first node.
        assert_eq!(
            page.eval("document.body.normalize(); [r.endContainer === t, r.endOffset, document.body.childNodes.length].join()"),
            "true,5,1"
        );
    });
}

#[test]
fn contents_are_cloned_extracted_deleted_and_surrounded() {
    on_page(|| {
        let mut page = page();
        page.eval(
            "document.body.innerHTML = '<p id=p>one <b>two</b> three</p>';
             var p = document.getElementById('p');
             var r = new Range(); r.setStart(p.firstChild, 2); r.setEnd(p.lastChild, 3);",
        );
        assert_eq!(page.eval("r.toString()"), "e two th");
        // cloneContents leaves the document alone.
        assert_eq!(
            page.eval("var f = r.cloneContents(); var d = document.createElement('div'); d.appendChild(f); d.innerHTML"),
            "e <b>two</b> th"
        );
        assert_eq!(page.eval("p.innerHTML"), "one <b>two</b> three");
        // extractContents takes it out and collapses the range where it was.
        assert_eq!(
            page.eval("var x = r.extractContents(); var e = document.createElement('div'); e.appendChild(x); [e.innerHTML, p.innerHTML, r.collapsed].join('|')"),
            "e <b>two</b> th|onree|true"
        );
    });
}

#[test]
fn delete_and_surround_contents() {
    on_page(|| {
        let mut page = page();
        page.eval(
            "document.body.innerHTML = '<p id=p>hello world</p>';
             var p = document.getElementById('p');
             var r = new Range(); r.setStart(p.firstChild, 0); r.setEnd(p.firstChild, 6);",
        );
        assert_eq!(
            page.eval("r.deleteContents(); p.innerHTML + '|' + r.collapsed"),
            "world|true"
        );
        page.eval("r.setStart(p.firstChild, 1); r.setEnd(p.firstChild, 3); r.surroundContents(document.createElement('em'));");
        assert_eq!(page.eval("p.innerHTML"), "w<em>or</em>ld");
        // A range that cuts an element in half cannot be surrounded.
        assert_eq!(
            page.eval("r.setStart(p.firstChild, 0); r.setEnd(p.querySelector('em').firstChild, 1); try { r.surroundContents(document.createElement('u')); 'no' } catch (e) { e.name }"),
            "InvalidStateError"
        );
        // insertNode splits the text at the start.
        page.eval("r.setStart(p.lastChild, 1); r.collapse(true); r.insertNode(document.createElement('hr'));");
        assert_eq!(page.eval("p.innerHTML"), "w<em>or</em>l<hr>d");
    });
}

#[test]
fn points_are_compared_in_tree_order() {
    on_page(|| {
        let mut page = page();
        page.eval(
            "document.body.innerHTML = '<p id=p>abcdef</p><p id=q>x</p>';
             var p = document.getElementById('p'), q = document.getElementById('q');
             var t = p.firstChild; var r = new Range(); r.setStart(t, 2); r.setEnd(t, 4);
             var r2 = new Range(); r2.setStart(t, 3); r2.setEnd(q.firstChild, 1);",
        );
        assert_eq!(
            page.eval(
                "r.comparePoint(t, 1) + ',' + r.comparePoint(t, 3) + ',' + r.comparePoint(t, 5)"
            ),
            "-1,0,1"
        );
        assert_eq!(
            page.eval("r.isPointInRange(t, 4) + ',' + r.isPointInRange(q.firstChild, 0)"),
            "true,false"
        );
        // START_TO_START, END_TO_END, START_TO_END and END_TO_START.
        assert_eq!(
            page.eval("[0, 2, 1, 3].map(h => r.compareBoundaryPoints(h, r2)).join()"),
            "-1,-1,1,-1"
        );
        assert_eq!(
            page.eval("r.intersectsNode(p) + ',' + r.intersectsNode(q)"),
            "true,false"
        );
        assert_eq!(page.eval("r.commonAncestorContainer === t"), "true");
        assert_eq!(
            page.eval("r2.commonAncestorContainer === document.body"),
            "true"
        );
        // Setting a start after the end moves the end along.
        assert_eq!(
            page.eval("r.setStart(t, 5); [r.startOffset, r.endOffset, r.collapsed].join()"),
            "5,5,true"
        );
    });
}

#[test]
fn the_selection_is_one_live_range_with_a_direction() {
    on_page(|| {
        let mut page = page();
        page.eval("document.body.innerHTML = '<p id=p>hello world</p>'; var p = document.getElementById('p');
             var t = p.firstChild; var s = getSelection();");
        assert_eq!(
            page.eval("s.rangeCount + ',' + s.type + ',' + s.anchorNode"),
            "0,None,null"
        );
        page.eval("s.setBaseAndExtent(t, 5, t, 2);");
        assert_eq!(
            page.eval("[s.rangeCount, s.type, s.direction, s.anchorOffset, s.focusOffset, s.toString()].join()"),
            "1,Range,backward,5,2,llo"
        );
        // extend keeps the anchor.
        page.eval("s.collapse(t, 2); s.extend(t, 6);");
        assert_eq!(
            page.eval("s.direction + ',' + s.toString()"),
            "forward,llo "
        );
        // getRangeAt hands out the live range: it follows an edit and the selection follows it.
        page.eval("var r = s.getRangeAt(0); t.replaceData(0, 0, '12');");
        assert_eq!(
            page.eval("[r.startOffset, s.anchorOffset, s.focusOffset].join()"),
            "4,4,8"
        );
        assert_eq!(page.eval("s.getRangeAt(0) === r"), "true");
        // addRange takes a range only when the selection has none, and keeps it live.
        page.eval("s.removeAllRanges(); var mine = new Range(); mine.selectNodeContents(p); s.addRange(mine); mine.setEnd(t, 3);");
        assert_eq!(page.eval("s.getRangeAt(0) === mine && s.focusOffset"), "3");
        assert_eq!(
            page.eval("s.collapseToStart(); s.type + ',' + s.direction + ',' + s.anchorOffset"),
            "Caret,none,0"
        );
        assert_eq!(
            page.eval("s.selectAllChildren(p); s.toString()"),
            "12hello world"
        );
        assert_eq!(page.eval("s.containsNode(t, false) + ',' + s.containsNode(document.body, true) + ',' + s.containsNode(document.body, false)"), "true,true,false");
        // deleteFromDocument removes the selected text and leaves a caret.
        assert_eq!(
            page.eval("s.deleteFromDocument(); p.innerHTML + '|' + s.type"),
            "|Caret"
        );
        assert_eq!(
            page.eval("try { s.getRangeAt(1) } catch (e) { e.name }"),
            "IndexSizeError"
        );
    });
}

#[test]
fn modify_steps_by_character_word_and_line_end() {
    on_page(|| {
        let mut page = page();
        page.eval(
            "document.body.innerHTML = '<p id=p>hello world, foo-bar baz</p><div id=a>one</div>';
             var p = document.getElementById('p'), a = document.getElementById('a');
             var t = p.firstChild; var s = getSelection(); s.collapse(t, 0);",
        );
        // Observed in Electron 39.8.10: 6, 11, 13, then one character further.
        let steps = page.eval(
            "var out = []; for (var i = 0; i < 3; i++) { s.modify('extend', 'forward', 'word'); out.push(s.focusOffset); }
             s.modify('extend', 'forward', 'character'); out.push(s.focusOffset); out.join()",
        );
        assert_eq!(steps, "6,11,13,14");
        assert_eq!(page.eval("s.toString()"), "hello world, f");
        let moves = page.eval(
            "s.collapse(t, 0); s.modify('move', 'forward', 'word'); var w1 = s.focusOffset;
             s.modify('move', 'forward', 'word'); var w2 = s.focusOffset;
             s.modify('move', 'backward', 'word'); [w1, w2, s.focusOffset].join()",
        );
        assert_eq!(moves, "6,11,6");
        assert_eq!(
            page.eval(
                "s.collapse(t, 3); s.modify('move', 'forward', 'lineboundary'); s.focusOffset"
            ),
            "24"
        );
        assert_eq!(
            page.eval(
                "s.collapse(t, 3); s.modify('move', 'backward', 'lineboundary'); s.focusOffset"
            ),
            "0"
        );
        // At the start nothing is before it; a step forward from the end of one block lands at
        // the start of the next.
        assert_eq!(
            page.eval(
                "s.collapse(t, 0); s.modify('extend', 'backward', 'character'); s.focusOffset"
            ),
            "0"
        );
        assert_eq!(page.eval("s.collapse(t, 24); s.modify('move', 'forward', 'character'); (s.focusNode === a.firstChild) + ',' + s.focusOffset"), "true,0");
    });
}

#[test]
fn selectionchange_is_one_event_for_the_changes_before_the_page_next_runs() {
    on_page(|| {
        let mut page = page();
        page.eval(
            "document.body.innerHTML = '<p id=p>hello</p>';
             var p = document.getElementById('p'); var t = p.firstChild; var n = 0;
             var trusted = [];
             document.addEventListener('selectionchange', (e) => { n++; trusted.push(e.isTrusted); });",
        );
        // `eval` runs the microtasks after the script, so the count is read inside it.
        assert_eq!(
            page.eval(
                "getSelection().collapse(t, 1); getSelection().collapse(t, 2);
                 getSelection().extend(t, 4); n"
            ),
            "0"
        );
        assert_eq!(page.eval("n + ',' + trusted.join()"), "1,true");
        page.eval("getSelection().collapse(t, 3);");
        page.runtime.run_microtasks();
        assert_eq!(page.eval("n"), "2");
        // Removing the selected text moves the selection too.
        page.eval("p.removeChild(t);");
        page.runtime.run_microtasks();
        assert_eq!(page.eval("n"), "3");
    });
}

#[test]
fn static_ranges_and_collected_ranges() {
    on_page(|| {
        let mut page = page();
        page.eval(
            "document.body.innerHTML = '<p id=p>hello</p>'; var t = document.getElementById('p').firstChild;",
        );
        assert_eq!(
            page.eval("var sr = new StaticRange({startContainer: t, startOffset: 1, endContainer: t, endOffset: 3}); t.data = 'x'; [sr.startOffset, sr.endOffset, sr.collapsed].join()"),
            "1,3,false"
        );
        // Ranges nobody holds any more give their slots back once the collector has run.
        page.eval(
            "for (var i = 0; i < 5000; i++) { var q = document.createRange(); q.setStart(t, 0); }",
        );
        boa_gc::force_collect();
        page.eval("document.createRange().collapsed");
        let kept = page.document.live_ranges();
        assert!(kept < 20, "{kept} ranges remain after a collection");
    });
}

#[test]
fn inserting_a_fragment_empties_a_range_in_it_as_removing_its_children_does() {
    // Electron 39.8.10: a range over a fragment's two children is collapsed at (fragment, 0)
    // once the fragment is appended and has given up its children.
    on_page(|| {
        let mut page = page();
        page.eval(
            "document.body.innerHTML = '<div id=ed></div>';
             var f = document.createDocumentFragment();
             f.appendChild(document.createElement('i')); f.appendChild(document.createElement('b'));
             var h = new Range(); h.setStart(f, 0); h.setEnd(f, 2);
             document.getElementById('ed').appendChild(f);",
        );
        assert_eq!(
            page.eval(
                "[h.startContainer === f, h.startOffset, h.endContainer === f, h.endOffset].join()"
            ),
            "true,0,true,0"
        );
    });
}

#[test]
fn character_data_methods_move_boundaries_as_the_standard_says() {
    // The numbers are the standard's "replace data" steps for a range (2,4) in "abcdef"; the
    // review's probe in Electron 39.8.10 gave 2,4 for appendData and 4,6 for insertData.
    on_page(|| {
        let mut page = page();
        page.eval(
            "document.body.innerHTML = 'abcdef'; var t = document.body.firstChild;
             var r = new Range(); r.setStart(t, 2); r.setEnd(t, 4);",
        );
        assert_eq!(
            page.eval("t.appendData('gh'); [r.startOffset, r.endOffset].join()"),
            "2,4"
        );
        assert_eq!(
            page.eval("t.insertData(0, 'ZZ'); [r.startOffset, r.endOffset].join()"),
            "4,6"
        );
        assert_eq!(
            page.eval("t.deleteData(0, 5); [r.startOffset, r.endOffset].join()"),
            "0,1"
        );
    });
}

#[test]
fn offsets_are_unsigned_longs_and_a_same_value_data_setter_still_replaces() {
    // Electron 39.8.10: insertData(-1) and splitText(-1) throw IndexSizeError, deleteData(0, -1)
    // deletes to the end, replaceData(1, -1, 'Z') on "abcd" gives "aZ"; `t.data = t.data` moves
    // boundaries inside the node to 0.
    on_page(|| {
        let mut page = page();
        page.eval("document.body.innerHTML = 'abcd'; var t = document.body.firstChild;");
        assert_eq!(
            page.eval("try { t.insertData(-1, 'x'); 'no' } catch (e) { e.name }"),
            "IndexSizeError"
        );
        assert_eq!(
            page.eval("try { t.splitText(-1); 'no' } catch (e) { e.name }"),
            "IndexSizeError"
        );
        assert_eq!(page.eval("t.replaceData(1, -1, 'Z'); t.data"), "aZ");
        assert_eq!(
            page.eval("t.data = 'abcd'; t.deleteData(0, -1); t.data"),
            ""
        );
        assert_eq!(
            page.eval(
                "t.data = 'abcd'; var r = new Range(); r.setStart(t, 2); r.setEnd(t, 3); t.data = t.data; [r.startOffset, r.endOffset].join()"
            ),
            "0,0"
        );
    });
}

#[test]
fn delete_contents_removes_a_doctype_where_clone_and_extract_refuse() {
    // Electron 39.8.10: cloneContents and extractContents throw HierarchyRequestError over a
    // document type; deleteContents removes it.
    on_page(|| {
        let mut page = page();
        page.eval(
            "var d = new DOMParser().parseFromString('<!DOCTYPE html><p>x</p>', 'text/html');
             var r = d.createRange(); r.setStart(d, 0); r.setEnd(d, d.childNodes.length);",
        );
        assert_eq!(page.eval("d.doctype !== null"), "true");
        assert_eq!(
            page.eval("try { r.cloneContents(); 'no' } catch (e) { e.name }"),
            "HierarchyRequestError"
        );
        assert_eq!(
            page.eval("try { r.extractContents(); 'no' } catch (e) { e.name }"),
            "HierarchyRequestError"
        );
        assert_eq!(page.eval("r.deleteContents(); d.doctype === null"), "true");
    });
}
