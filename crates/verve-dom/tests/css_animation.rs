//! Transitions and animations: the value a property shows between the two the cascade gives it,
//! the frames it moves on, the events it fires, and the `Animation` objects a script holds.

mod common;

use common::{Page, on_page};
use verve_dom::Options;

/// A page whose clock the test moves, with `#box` ready and its inline style set to `inline`.
fn page_with(sheet: &str, inline: &str) -> Page {
    let mut page = Page::new(Options::default());
    page.document.set_stylesheet("test", sheet);
    page.eval(&format!(
        "document.body.innerHTML = '<div id=box></div>'; window.box = document.getElementById('box');
         window.seen = []; box.style.cssText = '{inline}';"
    ));
    page.document.update();
    page
}

/// A page with the sheet `sheet`, `html` (which holds `#box`) in its body, `window.box` for the
/// element and `window.seen` for a test to log what it hears in.
fn box_page(sheet: &str, html: &str) -> Page {
    let mut page = Page::new(Options::default());
    page.document.set_stylesheet("test", sheet);
    page.eval(&format!(
        "document.body.innerHTML = '{html}'; window.box = document.getElementById('box'); window.seen = [];"
    ));
    page.document.update();
    page
}

/// Runs a frame at each of `times`.
fn frames(page: &mut Page, times: &[f64]) {
    for now in times {
        page.document.frame(&mut page.runtime, *now);
    }
}

/// A number from what a script printed.
fn number(page: &mut Page, source: &str) -> f32 {
    page.eval(source).parse().expect("a number")
}

fn near(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() < 0.06,
        "{actual} is not near {expected}"
    );
}

#[test]
fn a_transition_shows_the_value_between_its_ends() {
    on_page(|| {
        let mut page = page_with("#box { transition: opacity 100ms linear }", "opacity: 1");
        page.eval("document.getElementById('box').style.opacity = '0'");
        page.document.update();
        let node = page.find("#box");
        // Made but not yet moved: it stands at its start, not at the new value.
        assert_eq!(page.document.snapshot().style(node, "opacity"), Some("1"));
        frames(&mut page, &[0., 50.]);
        let half: f32 = page
            .document
            .snapshot()
            .style(node, "opacity")
            .unwrap()
            .parse()
            .unwrap();
        near(half, 0.5);
        frames(&mut page, &[100.]);
        assert_eq!(page.document.snapshot().style(node, "opacity"), Some("0"));
        // Once it is over the cascade's own value is shown, and no frame is wanted.
        assert!(!page.document.wants_frame());
    });
}

#[test]
fn transition_all_moves_whatever_changed() {
    on_page(|| {
        let mut page = page_with(
            "#box { transition: all 100ms linear }",
            "opacity: 1; margin-left: 0px; color: rgb(0, 0, 0); display: block",
        );
        page.eval(
            "const b = document.getElementById('box');
             b.style.cssText = 'opacity: 0; margin-left: 100px; color: rgb(200, 200, 200); display: none';",
        );
        page.document.update();
        frames(&mut page, &[0., 50.]);
        // Every property that changed and can move is half way; `display` cannot, and is not.
        near(number(&mut page, "getComputedStyle(box).opacity"), 0.5);
        near(
            number(&mut page, "parseFloat(getComputedStyle(box).marginLeft)") / 100.,
            0.5,
        );
        assert_eq!(
            page.eval("getComputedStyle(box).color"),
            "rgb(100, 100, 100)"
        );
        assert_eq!(page.eval("getComputedStyle(box).display"), "none");
        frames(&mut page, &[100.]);
        assert_eq!(page.eval("getComputedStyle(box).marginLeft"), "100px");
    });
}

#[test]
fn a_transition_fires_its_events_once_each() {
    on_page(|| {
        let mut page = page_with("#box { transition: opacity 100ms linear }", "opacity: 1");
        page.eval(
            "window.seen = [];
             for (const kind of ['transitionrun', 'transitionstart', 'transitionend']) {
                 box.addEventListener(kind, e => seen.push(kind + ':' + e.propertyName + ':' + e.elapsedTime));
             }
             box.style.opacity = '0';",
        );
        page.document.update();
        frames(&mut page, &[0., 40., 100., 140.]);
        assert_eq!(
            page.eval("seen.join('|')"),
            "transitionrun:opacity:0|transitionstart:opacity:0|transitionend:opacity:0.1"
        );
    });
}

#[test]
fn an_interrupted_transition_starts_from_where_it_is() {
    on_page(|| {
        let mut page = page_with("#box { transition: opacity 100ms linear }", "opacity: 1");
        page.eval(
            "window.seen = [];
             box.addEventListener('transitioncancel', e => seen.push('cancel:' + e.elapsedTime));
             box.style.opacity = '0';",
        );
        page.document.update();
        frames(&mut page, &[0., 50.]);
        // Back up to 1 mid-flight: the new transition runs from the value the old one shows.
        page.eval("box.style.opacity = '1'");
        page.document.update();
        frames(&mut page, &[50.]);
        near(number(&mut page, "getComputedStyle(box).opacity"), 0.5);
        // And the old one says how far it got: half of its 100 ms.
        assert_eq!(page.eval("seen.join('|')"), "cancel:0.05");
    });
}

#[test]
fn keyframes_run_and_announce_each_iteration() {
    on_page(|| {
        let mut page = box_page(
            "@keyframes fade { from { opacity: 0 } to { opacity: 1 } }",
            "<div id=box></div>",
        );
        page.eval(
            "box.style.opacity = '0';
             box.style.animation = 'fade 100ms linear 2';
             box.addEventListener('animationiteration', () => seen.push('iteration'));
             box.addEventListener('animationend', e => seen.push('end:' + e.animationName));",
        );
        frames(&mut page, &[0., 50.]);
        near(number(&mut page, "getComputedStyle(box).opacity"), 0.5);
        frames(&mut page, &[100., 150., 200., 240.]);
        assert_eq!(page.eval("seen.join('|')"), "iteration|end:fade");
    });
}

#[test]
fn a_name_that_ends_in_s_is_not_read_as_a_time() {
    on_page(|| {
        let mut page = box_page(
            "@keyframes spin-forms { from { opacity: 0 } to { opacity: 1 } }
             #box { animation: spin-forms 100ms linear }",
            "<div id=box></div>",
        );
        page.eval("box.addEventListener('animationstart', e => seen.push(e.animationName));");
        assert_eq!(
            page.eval("getComputedStyle(box).animationName"),
            "spin-forms"
        );
        assert_eq!(
            page.eval("getComputedStyle(box).animationDuration"),
            "100ms"
        );
        frames(&mut page, &[0., 50.]);
        assert_eq!(page.eval("seen.join()"), "spin-forms");
        near(number(&mut page, "getComputedStyle(box).opacity"), 0.5);
    });
}

#[test]
fn removing_the_animation_name_cancels_the_animation() {
    on_page(|| {
        let mut page = box_page(
            "@keyframes pulse { from { opacity: 0.2 } to { opacity: 1 } }
             .on { animation: pulse 100ms linear infinite }",
            "<div id=box class=on></div>",
        );
        page.eval(
            "box.addEventListener('animationcancel', e => seen.push('cancel:' + e.elapsedTime));",
        );
        frames(&mut page, &[0., 50.]);
        assert!(page.document.wants_frame());
        page.eval("box.className = ''");
        page.document.update();
        frames(&mut page, &[60.]);
        // The event is told on the frame after, and carries how far the animation had got.
        assert_eq!(page.eval("seen.join()"), "cancel:0.05");
        assert!(!page.document.wants_frame());
        assert_eq!(page.eval("getComputedStyle(box).opacity"), "1");
    });
}

#[test]
fn an_infinite_animation_on_a_node_that_left_stops_asking_for_frames() {
    on_page(|| {
        let mut page = box_page(
            "@keyframes pulse { from { opacity: 0.2 } to { opacity: 1 } }
             #box { animation: pulse 100ms linear infinite }",
            "<div id=box></div>",
        );
        page.eval("box.addEventListener('animationcancel', () => seen.push('cancel'));");
        frames(&mut page, &[0., 50.]);
        assert!(page.document.wants_frame());
        page.eval("window.gone = box; box.remove()");
        page.document.update();
        assert!(
            !page.document.wants_frame(),
            "nothing is drawn, so nothing is asked for"
        );
        frames(&mut page, &[100.]);
        // No one is in the document to hear it.
        assert_eq!(page.eval("seen.join()"), "");
        // And once the node is freed the store lets go of it.
        page.eval("window.gone = null");
        page.document.sweep(&mut page.runtime);
        page.document.sweep(&mut page.runtime);
        assert!(!page.document.wants_frame());
    });
}

#[test]
fn nothing_runs_when_nothing_animates() {
    on_page(|| {
        let mut page = page_with("#box { color: red }", "opacity: 1");
        page.eval("box.style.opacity = '0.5'; box.className = 'x'");
        page.document.update();
        frames(&mut page, &[0., 16.]);
        assert!(!page.document.wants_frame());
    });
}

#[test]
fn a_transition_runs_on_the_realms_own_clock_with_no_animation_frame_loop() {
    on_page(|| {
        // The default clock is the realm's timers: nothing here calls `frame()`, and no script
        // asks for an animation frame.
        let mut page = Page::new(Options::default());
        page.document
            .set_stylesheet("test", "#box { transition: opacity 100ms linear }");
        page.eval(
            "document.body.innerHTML = '<div id=box></div>'; window.box = document.getElementById('box');
             window.seen = [];
             box.style.opacity = '1';
             box.addEventListener('transitionend', e => seen.push(e.propertyName));",
        );
        page.settle();
        page.eval("box.style.opacity = '0'");
        page.settle();
        assert_eq!(page.eval("seen.join()"), "opacity");
        assert_eq!(page.eval("getComputedStyle(box).opacity"), "0");
    });
}

#[test]
fn a_script_animation_resolves_finished_and_fires_finish() {
    on_page(|| {
        let mut page = page_with("", "");
        page.eval(
            "window.seen = [];
             window.animation = box.animate([{ opacity: '0' }, { opacity: '1' }], { duration: 100, easing: 'linear' });
             animation.finished.then(a => seen.push('finished:' + (a === animation)));
             animation.onfinish = () => seen.push('onfinish');
             animation.addEventListener('finish', () => seen.push('listener'));",
        );
        assert_eq!(page.eval("animation.playState"), "running");
        frames(&mut page, &[0., 50.]);
        assert_eq!(page.eval("seen.join('|')"), "");
        near(number(&mut page, "getComputedStyle(box).opacity"), 0.5);
        frames(&mut page, &[100., 140.]);
        assert_eq!(
            page.eval("seen.join('|')"),
            "onfinish|listener|finished:true"
        );
        assert_eq!(page.eval("animation.playState"), "finished");
        assert_eq!(page.eval("animation.currentTime"), "100");
    });
}

#[test]
fn finish_acts_on_that_animation_only() {
    on_page(|| {
        let mut page = page_with("", "");
        page.eval(
            "window.seen = [];
             window.quick = box.animate([{ opacity: '0' }, { opacity: '1' }], 100);
             window.slow = box.animate([{ marginLeft: '0px' }, { marginLeft: '100px' }], 1000);
             quick.finished.then(() => seen.push('quick'));
             slow.finished.then(() => seen.push('slow'));",
        );
        frames(&mut page, &[0., 10.]);
        page.eval("quick.finish()");
        page.document.frame(&mut page.runtime, 20.);
        assert_eq!(page.eval("seen.join()"), "quick");
        assert_eq!(
            page.eval("[quick.playState, slow.playState].join()"),
            "finished,running"
        );
        // The other one has not been moved to its end, nor had its start stamped in the future.
        near(
            number(&mut page, "parseFloat(getComputedStyle(box).marginLeft)") / 100.,
            0.02,
        );
        // An animation that never ends cannot be finished.
        assert_eq!(
            page.eval(
                "(() => { try { box.animate([{opacity: 0}, {opacity: 1}], {duration: 10, iterations: Infinity}).finish(); return 'no'; }
                          catch (e) { return e.name; } })()"
            ),
            "InvalidStateError"
        );
    });
}

#[test]
fn cancel_rejects_finished_with_an_abort_error_and_fires_cancel() {
    on_page(|| {
        let mut page = page_with("", "");
        page.eval(
            "window.seen = [];
             window.animation = box.animate([{ opacity: '0' }, { opacity: '1' }], 100);
             animation.finished.catch(e => seen.push('rejected:' + e.name));
             animation.oncancel = () => seen.push('oncancel');
             animation.addEventListener('cancel', () => seen.push('listener'));",
        );
        frames(&mut page, &[0., 30.]);
        page.eval("animation.cancel()");
        page.runtime.run_microtasks();
        assert_eq!(
            page.eval("seen.join('|')"),
            "oncancel|listener|rejected:AbortError"
        );
        assert_eq!(page.eval("animation.playState"), "idle");
        assert_eq!(page.eval("animation.currentTime"), "null");
        // Cancelled, it no longer drives the element.
        assert_eq!(page.eval("getComputedStyle(box).opacity"), "1");
        assert_eq!(page.console(), "");
    });
}

#[test]
fn play_pause_reverse_and_the_playback_rate_move_one_animation() {
    on_page(|| {
        let mut page = page_with("", "");
        page.eval(
            "window.animation = box.animate([{ marginLeft: '0px' }, { marginLeft: '100px' }], { duration: 100, easing: 'linear', fill: 'both' });",
        );
        frames(&mut page, &[0., 40.]);
        page.eval("animation.pause()");
        assert_eq!(page.eval("animation.playState"), "paused");
        frames(&mut page, &[90.]);
        // Held where it was while paused.
        assert_eq!(page.eval("animation.currentTime"), "40");
        assert_eq!(page.eval("getComputedStyle(box).marginLeft"), "40px");
        page.eval("animation.play()");
        frames(&mut page, &[100., 120.]);
        // It takes its place on the timeline at the next frame, 100, from the 40 it stood at.
        assert_eq!(page.eval("animation.currentTime"), "60");
        page.eval("animation.updatePlaybackRate(2)");
        frames(&mut page, &[130., 140.]);
        assert_eq!(page.eval("animation.playbackRate"), "2");
        assert_eq!(page.eval("animation.currentTime"), "80");
        page.eval("animation.reverse()");
        assert_eq!(page.eval("animation.playbackRate"), "-2");
        frames(&mut page, &[150., 160.]);
        assert_eq!(page.eval("animation.currentTime"), "60");
        // Played to the end, it can be played again from the start.
        page.eval("animation.finish()");
        assert_eq!(page.eval("animation.playState"), "finished");
        page.eval("animation.currentTime = 10; animation.play()");
        assert_eq!(page.eval("animation.playState"), "running");
    });
}

#[test]
fn commit_styles_writes_the_value_into_the_element_and_persist_says_it_is_unsupported() {
    on_page(|| {
        let mut page = page_with("", "");
        page.eval(
            "window.animation = box.animate([{ marginLeft: '0px' }, { marginLeft: '100px' }], { duration: 100, easing: 'linear', fill: 'forwards' });",
        );
        frames(&mut page, &[0., 50.]);
        page.eval("animation.commitStyles(); animation.cancel()");
        assert_eq!(page.eval("box.style.marginLeft"), "50px");
        assert_eq!(page.eval("getComputedStyle(box).marginLeft"), "50px");
        assert_eq!(
            page.eval(
                "(() => { try { animation.persist(); return 'no'; } catch (e) { return e.code; } })()"
            ),
            "ERR_NOT_SUPPORTED"
        );
    });
}

#[test]
fn get_animations_hands_back_the_same_objects_with_a_working_finished() {
    on_page(|| {
        let mut page = box_page(
            "@keyframes fade { from { opacity: 0 } to { opacity: 1 } }
             #box { animation: fade 100ms linear }",
            "<div id=box></div>",
        );
        page.eval("window.seen = [];");
        frames(&mut page, &[0., 30.]);
        page.eval(
            "window.script = box.animate([{ marginLeft: '0px' }, { marginLeft: '10px' }], 100);
             window.all = box.getAnimations();
             Promise.all(box.getAnimations().map(a => a.finished)).then(() => seen.push('all'));",
        );
        assert_eq!(page.eval("all.length"), "2");
        assert_eq!(
            page.eval("all.includes(script) && box.getAnimations().every((a, i) => a === all[i])"),
            "true"
        );
        assert_eq!(page.eval("document.getAnimations().length"), "2");
        assert_eq!(page.eval("all.map(a => a.animationName).join()"), "fade,");
        // `finished` waits for the end of both, instead of being resolved at once.
        page.runtime.run_microtasks();
        assert_eq!(page.eval("seen.join()"), "");
        frames(&mut page, &[60., 100., 140., 200.]);
        assert_eq!(page.eval("seen.join()"), "all");
        assert_eq!(page.eval("box.getAnimations().length"), "0");
    });
}

#[test]
fn keyframes_may_be_given_by_property() {
    on_page(|| {
        let mut page = page_with("", "");
        page.eval(
            "window.animation = box.animate({ opacity: [0, 1], marginLeft: ['0px', '50px', '0px'] }, { duration: 100, easing: 'linear' });",
        );
        frames(&mut page, &[0., 25.]);
        near(number(&mut page, "getComputedStyle(box).opacity"), 0.25);
        // Three values for a property fall at 0, a half and 1.
        near(
            number(&mut page, "parseFloat(getComputedStyle(box).marginLeft)") / 50.,
            0.5,
        );
        frames(&mut page, &[50.]);
        assert_eq!(page.eval("getComputedStyle(box).marginLeft"), "50px");
    });
}

#[test]
fn a_shorthand_with_a_variable_in_it_is_opened_once_the_variable_is_read() {
    on_page(|| {
        // Tailwind v4's `animate-spin` is `animation: var(--animate-spin)`.
        let mut page = box_page(
            ":root { --animate-spin: spin 100ms linear infinite }
             @keyframes spin { to { transform: rotate(360deg) } }
             #box { animation: var(--animate-spin) }",
            "<div id=box></div>",
        );
        assert_eq!(page.eval("getComputedStyle(box).animationName"), "spin");
        assert_eq!(
            page.eval("getComputedStyle(box).animationIterationCount"),
            "infinite"
        );
        frames(&mut page, &[0., 25.]);
        // And it spins: a transform the element did not have moves from `none`.
        assert_eq!(
            page.eval("getComputedStyle(box).transform"),
            "rotate(90deg)"
        );
    });
}

#[test]
fn keyframes_read_their_variables_where_the_animation_runs() {
    on_page(|| {
        // tw-animate-css's `enter`: the start is a variable the element sets, with a fallback.
        let mut page = box_page(
            "@keyframes enter { from { opacity: var(--tw-enter-opacity, 1) } }
             #box { animation: enter var(--tw-duration, 100ms) var(--tw-ease, linear); --tw-enter-opacity: 0 }
             #other { animation: enter 100ms linear }",
            "<div id=box></div>",
        );
        assert_eq!(
            page.eval("getComputedStyle(box).animationDuration"),
            "100ms"
        );
        frames(&mut page, &[0., 50.]);
        near(number(&mut page, "getComputedStyle(box).opacity"), 0.5);
        // Without the variable, the fallback.
        page.eval("box.id = 'other'");
        page.document.update();
        frames(&mut page, &[60., 110.]);
        assert_eq!(page.eval("getComputedStyle(box).opacity"), "1");
    });
}

#[test]
fn a_property_the_element_never_set_moves_from_its_initial_value() {
    on_page(|| {
        // `.hide` sets the opacity, and taking it off puts back what was never written down.
        let mut page = box_page(
            "#box { transition: opacity 100ms linear } .hide { opacity: 0 }",
            "<div id=box></div>",
        );
        page.eval("box.className = 'hide'");
        page.document.update();
        frames(&mut page, &[0., 50.]);
        near(number(&mut page, "getComputedStyle(box).opacity"), 0.5);
        frames(&mut page, &[100.]);
        page.eval("box.className = ''");
        page.document.update();
        frames(&mut page, &[200., 250.]);
        near(number(&mut page, "getComputedStyle(box).opacity"), 0.5);
        // A transform from `none` moves too.
        page.eval("box.style.transition = 'transform 100ms linear'; box.style.transform = 'translateX(100px)'");
        page.document.update();
        frames(&mut page, &[300., 350.]);
        assert_eq!(
            page.eval("getComputedStyle(box).transform"),
            "translateX(50px)"
        );
    });
}

#[test]
fn a_transition_waits_out_its_delay_showing_where_it_began() {
    on_page(|| {
        let mut page = page_with(
            "#box { transition: opacity 100ms linear 100ms }",
            "opacity: 1",
        );
        page.eval(
            "box.addEventListener('transitionrun', e => seen.push('run:' + e.elapsedTime));
             box.addEventListener('transitionstart', e => seen.push('start:' + e.elapsedTime));
             box.addEventListener('transitionend', e => seen.push('end:' + e.elapsedTime));
             box.style.opacity = '0';",
        );
        page.document.update();
        frames(&mut page, &[0., 50.]);
        // Not the end value, and not started yet.
        assert_eq!(page.eval("getComputedStyle(box).opacity"), "1");
        assert_eq!(page.eval("seen.join()"), "run:0");
        frames(&mut page, &[150.]);
        near(number(&mut page, "getComputedStyle(box).opacity"), 0.5);
        frames(&mut page, &[250.]);
        // `elapsedTime` leaves the delay out.
        assert_eq!(page.eval("seen.join()"), "run:0,start:0,end:0.1");
    });
}

#[test]
fn a_negative_delay_has_already_run_when_it_begins() {
    on_page(|| {
        let mut page = page_with(
            "#box { transition: opacity 100ms linear -50ms }",
            "opacity: 1",
        );
        page.eval(
            "box.addEventListener('transitionrun', e => seen.push('run:' + e.elapsedTime));
             box.addEventListener('transitionstart', e => seen.push('start:' + e.elapsedTime));
             box.style.opacity = '0';",
        );
        page.document.update();
        frames(&mut page, &[0.]);
        assert_eq!(page.eval("seen.join()"), "run:0.05,start:0.05");
        near(number(&mut page, "getComputedStyle(box).opacity"), 0.5);
    });
}

#[test]
fn an_animation_starts_when_its_delay_ends_and_counts_its_iterations_without_it() {
    on_page(|| {
        let mut page = box_page(
            "@keyframes f { from { opacity: 0 } to { opacity: 1 } }
             #box { animation: f 100ms linear 50ms 2 }",
            "<div id=box></div>",
        );
        page.eval(
            "for (const kind of ['animationstart', 'animationiteration', 'animationend']) {
                 box.addEventListener(kind, e => seen.push(kind.slice(9) + ':' + e.elapsedTime));
             }",
        );
        frames(&mut page, &[0., 40.]);
        assert_eq!(
            page.eval("seen.join()"),
            "",
            "nothing before the delay is over"
        );
        frames(&mut page, &[60., 160., 260.]);
        assert_eq!(page.eval("seen.join()"), "start:0,iteration:0.1,end:0.2");
    });
}

#[test]
fn finished_settles_for_an_animation_nobody_holds_across_a_sweep() {
    on_page(|| {
        let mut page = page_with("", "");
        page.eval("box.animate([{ opacity: '0' }, { opacity: '1' }], 100).finished.then(() => seen.push('done'));");
        frames(&mut page, &[0.]);
        // The object is garbage as far as the script can tell, and a sweep collects what it can.
        page.document.sweep(&mut page.runtime);
        page.document.sweep(&mut page.runtime);
        frames(&mut page, &[50., 150.]);
        assert_eq!(page.eval("seen.join()"), "done");
    });
}

#[test]
fn css_transitions_and_animations_are_css_transition_and_css_animation() {
    on_page(|| {
        let mut page = box_page(
            "@keyframes fade { from { opacity: 0 } to { opacity: 1 } }
             #box { transition: margin-left 100ms linear; animation: fade 100ms linear }",
            "<div id=box></div>",
        );
        page.eval("box.style.marginLeft = '10px'");
        page.document.update();
        frames(&mut page, &[0., 10.]);
        let kinds = "box.getAnimations().map(a => [a instanceof CSSTransition, a instanceof CSSAnimation, a instanceof Animation].join(':')).sort().join('|')";
        assert_eq!(page.eval(kinds), "false:true:true|true:false:true");
        assert_eq!(
            page.eval(
                "box.getAnimations().find(a => a instanceof CSSTransition).transitionProperty"
            ),
            "margin-left"
        );
        assert_eq!(
            page.eval("box.getAnimations().find(a => a instanceof CSSAnimation).animationName"),
            "fade"
        );
        // Headless UI v2 asks exactly this.
        assert_eq!(
            page.eval("box.getAnimations().filter(a => a instanceof CSSTransition).length"),
            "1"
        );
        assert_eq!(
            page.eval("typeof CSSTransition + typeof CSSAnimation"),
            "functionfunction"
        );
    });
}

#[test]
fn a_play_after_a_long_wait_goes_on_from_where_it_stood() {
    on_page(|| {
        let mut page = page_with("", "");
        page.eval("window.animation = box.animate([{ marginLeft: '0px' }, { marginLeft: '1000px' }], { duration: 1000, easing: 'linear' });");
        frames(&mut page, &[0., 100.]);
        page.eval("animation.pause()");
        // No frame between the play and the next, which comes much later.
        page.eval("animation.play()");
        frames(&mut page, &[5000.]);
        assert_eq!(page.eval("animation.playState"), "running");
        assert_eq!(page.eval("animation.currentTime"), "100");
        frames(&mut page, &[5200.]);
        assert_eq!(page.eval("animation.currentTime"), "300");
    });
}

#[test]
fn a_cancelled_animation_can_be_played_finished_and_seeked_again() {
    on_page(|| {
        let mut page = page_with("", "");
        page.eval("window.animation = box.animate([{ opacity: '0' }, { opacity: '1' }], 100);");
        frames(&mut page, &[0., 30.]);
        page.eval("animation.cancel()");
        assert_eq!(page.eval("animation.playState"), "idle");
        page.eval("animation.play()");
        assert_eq!(page.eval("animation.playState"), "running");
        frames(&mut page, &[100., 150.]);
        assert_eq!(page.eval("animation.currentTime"), "50");
        page.eval("animation.cancel(); animation.finish()");
        assert_eq!(page.eval("animation.playState"), "finished");
        page.eval("animation.cancel(); animation.currentTime = 40");
        assert_eq!(page.eval("animation.playState"), "paused");
        assert_eq!(page.eval("animation.currentTime"), "40");
        // A rate of nothing holds an animation where it is.
        page.eval("animation.play(); animation.playbackRate = 0");
        assert_eq!(page.eval("animation.playbackRate"), "0");
        frames(&mut page, &[200., 400.]);
        assert_eq!(page.eval("animation.currentTime"), "40");
    });
}

#[test]
fn what_finish_and_cancel_announce_waits_for_the_handler_set_after_them() {
    on_page(|| {
        let mut page = page_with("", "");
        page.eval(
            "const a = box.animate([{ opacity: '0' }, { opacity: '1' }], 100);
             a.finish();
             seen.push('after finish');
             a.onfinish = () => seen.push('finish');
             const b = box.animate([{ opacity: '0' }, { opacity: '1' }], 100);
             b.cancel();
             seen.push('after cancel');
             b.oncancel = () => seen.push('cancel');",
        );
        // Told once the script is done with what it set up, not in the middle of it.
        assert_eq!(
            page.eval("seen.join()"),
            "after finish,after cancel,finish,cancel"
        );
    });
}

#[test]
fn a_node_that_is_freed_takes_its_script_animation_with_it() {
    on_page(|| {
        let mut page = page_with("", "");
        page.eval(
            "(() => {
                 const lone = document.createElement('div');
                 lone.animate([{ opacity: '0' }, { opacity: '1' }], 10000).finished.catch(e => seen.push(e.name));
             })();",
        );
        frames(&mut page, &[0., 10.]);
        // It has a long way to go, so it asks for frames: until the node is freed.
        assert!(page.document.wants_frame());
        page.document.sweep(&mut page.runtime);
        page.document.sweep(&mut page.runtime);
        assert!(!page.document.wants_frame());
        page.runtime.run_microtasks();
        assert_eq!(page.eval("seen.join()"), "AbortError");
    });
}

#[test]
fn a_css_animation_that_is_dropped_rejects_the_finished_a_script_holds() {
    on_page(|| {
        let mut page = box_page(
            "@keyframes pulse { from { opacity: 0.2 } to { opacity: 1 } }
             #box { animation: pulse 100ms linear infinite }",
            "<div id=box></div>",
        );
        frames(&mut page, &[0., 30.]);
        page.eval(
            "window.held = box.getAnimations()[0]; held.finished.catch(e => seen.push(e.name));
             box.remove();",
        );
        page.document.update();
        frames(&mut page, &[60.]);
        assert_eq!(page.eval("seen.join()"), "AbortError");
        assert_eq!(page.eval("held.playState"), "idle");
    });
}

#[test]
fn a_restyles_cancel_is_asked_for_on_the_embedders_clock() {
    on_page(|| {
        let mut page = Page::new(Options {
            frames: verve_dom::FrameClock::Host,
            ..Options::default()
        });
        page.document.set_stylesheet(
            "test",
            "@keyframes pulse { from { opacity: 0.2 } to { opacity: 1 } } .on { animation: pulse 100ms linear infinite }",
        );
        page.eval(
            "document.body.innerHTML = '<div id=box class=on></div>'; window.box = document.getElementById('box'); window.seen = [];
             box.addEventListener('animationcancel', () => seen.push('cancel'));",
        );
        page.document.update();
        frames(&mut page, &[0., 30.]);
        page.document.take_requests();
        page.eval("box.className = ''");
        page.document.update();
        // Nothing moves any more, but the event waits to be told, and the embedder is asked.
        assert!(page.document.wants_frame());
        assert!(
            page.document
                .take_requests()
                .contains(&verve_dom::Request::Frame)
        );
        frames(&mut page, &[40.]);
        assert_eq!(page.eval("seen.join()"), "cancel");
        assert!(!page.document.wants_frame());
    });
}

#[test]
fn animations_are_brought_to_the_frame_before_its_callbacks_run() {
    on_page(|| {
        let mut page = page_with("", "");
        page.eval(
            "box.animate([{ opacity: '0' }, { opacity: '1' }], { duration: 100, easing: 'linear' });
             requestAnimationFrame(() => seen.push(getComputedStyle(box).opacity));
             requestAnimationFrame(() => seen.push(getComputedStyle(box).opacity));",
        );
        frames(&mut page, &[0.]);
        frames(&mut page, &[50.]);
        page.eval("requestAnimationFrame(() => seen.push(getComputedStyle(box).opacity))");
        frames(&mut page, &[75.]);
        // A callback reads the value of the frame it is in.
        assert_eq!(page.eval("seen.join()"), "0,0,0.75");
    });
}

#[test]
fn a_child_inherits_what_its_parent_is_animating() {
    on_page(|| {
        let mut page = box_page(
            "#box { transition: color 100ms linear }",
            "<div id=box><span id=child>t</span></div>",
        );
        page.eval("box.style.color = 'rgb(0, 0, 0)'");
        page.document.update();
        frames(&mut page, &[0.]);
        page.eval("box.style.color = 'rgb(100, 100, 100)'");
        page.document.update();
        frames(&mut page, &[10., 60.]);
        assert_eq!(page.eval("getComputedStyle(box).color"), "rgb(50, 50, 50)");
        assert_eq!(
            page.eval("getComputedStyle(document.getElementById('child')).color"),
            "rgb(50, 50, 50)"
        );
        let child = page.find("#child");
        assert_eq!(
            page.document.snapshot().style(child, "color"),
            Some("rgb(50, 50, 50)")
        );
    });
}

#[test]
fn shorthands_in_transition_property_and_keyframes_move_their_longhands() {
    on_page(|| {
        let mut page = box_page(
            "@keyframes bg { from { background: rgb(0, 0, 0) } to { background: rgb(100, 100, 100) } }
             #box { transition: border-color 100ms linear }
             #bg { animation: bg 100ms linear }",
            "<div id=box style=\"border-color: rgb(0, 0, 0)\"></div>",
        );
        page.eval("box.style.borderColor = 'rgb(100, 100, 100)'");
        page.document.update();
        frames(&mut page, &[0., 50.]);
        assert_eq!(
            page.eval("getComputedStyle(box).borderTopColor"),
            "rgb(50, 50, 50)"
        );
        assert_eq!(
            page.eval("getComputedStyle(box).borderLeftColor"),
            "rgb(50, 50, 50)"
        );
        page.eval("box.id = 'bg'");
        page.document.update();
        frames(&mut page, &[100., 150.]);
        assert_eq!(
            page.eval("getComputedStyle(box).backgroundColor"),
            "rgb(50, 50, 50)"
        );
    });
}

#[test]
fn what_is_not_drawn_does_not_keep_frames_coming() {
    on_page(|| {
        let mut page = box_page(
            "@keyframes pulse { from { opacity: 0.2 } to { opacity: 1 } }
             #box { animation: pulse 100ms linear infinite } .hidden { display: none }",
            "<div id=box></div>",
        );
        frames(&mut page, &[0., 30.]);
        assert!(page.document.wants_frame());
        page.eval("box.className = 'hidden'");
        page.document.update();
        assert!(!page.document.wants_frame());
        // And it goes on when it is shown again.
        page.eval("box.className = ''");
        page.document.update();
        assert!(page.document.wants_frame());
    });
}

#[test]
fn a_duration_of_nothing_ends_the_transition_under_way() {
    on_page(|| {
        let mut page = page_with("#box { transition: opacity 100ms linear }", "opacity: 1");
        page.eval(
            "box.addEventListener('transitioncancel', () => seen.push('cancel'));
             box.style.opacity = '0';",
        );
        page.document.update();
        frames(&mut page, &[0., 50.]);
        page.eval("box.style.transitionDuration = '0s'; box.style.opacity = '0.8'");
        page.document.update();
        frames(&mut page, &[60.]);
        assert_eq!(page.eval("getComputedStyle(box).opacity"), "0.8");
        assert_eq!(page.eval("seen.join()"), "cancel");
    });
}

#[test]
fn a_fade_out_that_also_hides_stays_visible_until_it_has_faded() {
    on_page(|| {
        let mut page = box_page(
            "#box { transition: opacity 100ms linear, visibility 100ms linear }
             .out { opacity: 0; visibility: hidden }",
            "<div id=box></div>",
        );
        page.eval("box.className = 'out'");
        page.document.update();
        frames(&mut page, &[0., 50., 99.]);
        assert_eq!(page.eval("getComputedStyle(box).visibility"), "visible");
        frames(&mut page, &[100.]);
        assert_eq!(page.eval("getComputedStyle(box).visibility"), "hidden");
    });
}

#[test]
fn the_longhands_alone_mean_all_properties_and_ease() {
    on_page(|| {
        let mut page = page_with("#box { transition-duration: 100ms }", "opacity: 1");
        page.eval("box.style.opacity = '0'");
        page.document.update();
        frames(&mut page, &[0., 50.]);
        // `ease` is ahead of a straight line at the middle.
        let value = number(&mut page, "getComputedStyle(box).opacity");
        assert!(value < 0.3, "{value}");
    });
}

#[test]
fn a_keyframes_own_timing_function_moves_it_and_does_not_leak() {
    on_page(|| {
        let mut page = box_page(
            "@keyframes k { from { opacity: 0; animation-timing-function: steps(2, end) } to { opacity: 1 } }
             #box { animation: k 100ms linear }",
            "<div id=box></div>",
        );
        frames(&mut page, &[0., 40.]);
        // The first frame's `steps(2)` holds 0 until the half.
        assert_eq!(page.eval("getComputedStyle(box).opacity"), "0");
        frames(&mut page, &[60.]);
        assert_eq!(page.eval("getComputedStyle(box).opacity"), "0.5");
        // And the element's own `animation-timing-function` is what the style says.
        assert_eq!(
            page.eval("getComputedStyle(box).animationTimingFunction"),
            "linear"
        );
    });
}

#[test]
fn a_fill_that_holds_the_value_outlives_the_sweep() {
    on_page(|| {
        let mut page = page_with("", "");
        page.eval("box.animate([{ opacity: '0' }, { opacity: '0.3' }], { duration: 100, fill: 'forwards' });");
        frames(&mut page, &[0., 200.]);
        assert_eq!(page.eval("getComputedStyle(box).opacity"), "0.3");
        page.document.sweep(&mut page.runtime);
        page.document.sweep(&mut page.runtime);
        frames(&mut page, &[300.]);
        assert_eq!(page.eval("getComputedStyle(box).opacity"), "0.3");
    });
}

#[test]
fn a_longhand_that_won_over_a_shorthand_with_a_var_keeps_its_value() {
    on_page(|| {
        let mut page = box_page(
            "@keyframes k { from { opacity: 0 } to { opacity: 1 } }
             :root { --a: k 500ms linear }
             #box { animation: var(--a); animation-delay: 1000ms }",
            "<div id=box></div>",
        );
        assert_eq!(page.eval("getComputedStyle(box).animationDelay"), "1000ms");
        assert_eq!(
            page.eval("getComputedStyle(box).animationDuration"),
            "500ms"
        );
        page.eval("box.style.animationPlayState = 'paused'");
        page.document.update();
        assert_eq!(
            page.eval("getComputedStyle(box).animationPlayState"),
            "paused"
        );
    });
}

#[test]
fn the_rate_of_a_running_animation_counts_from_the_moment_it_is_set() {
    on_page(|| {
        let mut page = page_with("", "");
        page.eval("window.animation = box.animate([{ opacity: '0' }, { opacity: '1' }], 10000);");
        frames(&mut page, &[0., 100.]);
        page.eval("animation.playbackRate = 2");
        frames(&mut page, &[200.]);
        assert_eq!(page.eval("animation.currentTime"), "300");
    });
}
