//! 媒体矩阵只注入真实宿主回执，不靠计时器模拟解码或播放。

mod common;

use common::{Page, on_page};
use verve_dom::{MediaCommand, MediaCommandKind as Command, MediaUpdate as Update, Options};

fn video() -> Page {
    let mut page = Page::new(Options {
        url: "https://pet.test/app/index.html".into(),
        ..Options::default()
    });
    page.eval(
        r#"
        globalThis.v = document.createElement('video');
        v.id = 'pet'; document.body.append(v);
        globalThis.events = []; globalThis.result = 'pending';
        for (const name of ['loadedmetadata', 'loadeddata', 'canplay', 'play', 'playing',
                            'pause', 'timeupdate', 'ended', 'error', 'seeking', 'seeked'])
            v.addEventListener(name, () => events.push(name));
        "#,
    );
    page
}

fn load(page: &mut Page) -> MediaCommand {
    page.eval("v.src = 'blob:pet'; v.load()");
    let commands = page.document.take_media_commands();
    assert_eq!(commands.len(), 1, "superseded loads must not escape");
    assert!(matches!(&commands[0].kind, Command::Load { src, .. } if src == "blob:pet"));
    commands.into_iter().next().unwrap()
}

fn update(page: &mut Page, command: &MediaCommand, feedback: Update) -> bool {
    page.document
        .media_update(
            &mut page.runtime,
            command.node,
            command.generation,
            feedback,
        )
        .expect("valid host feedback")
}

fn ready(page: &mut Page, command: &MediaCommand) {
    assert!(update(
        page,
        command,
        Update::Metadata {
            duration: 4.,
            width: 320,
            height: 240,
            seekable: true,
        }
    ));
    assert!(update(page, command, Update::FrameReady));
}

#[test]
fn initial_and_unacknowledged_media_never_become_ready() {
    on_page(|| {
        let mut page = video();
        assert_eq!(page.eval("[v.paused, v.ended, v.currentTime, Number.isNaN(v.duration), v.readyState, v.videoWidth, v.videoHeight, v.muted, v.loop, v.playbackRate].join('|')"), "true|false|0|true|0|0|0|false|false|1");
        let command = load(&mut page);
        page.eval("v.play().then(() => result = 'ok', e => result = e.name)");
        page.settle();
        assert_eq!(
            page.eval("[result, v.readyState, events.join(',')].join('|')"),
            "pending|0|"
        );
        assert_eq!(
            page.document.snapshot().media[&command.node].generation,
            command.generation
        );
    });
}

#[test]
fn only_metadata_then_actual_frame_dispatch_readiness_once() {
    on_page(|| {
        let mut page = video();
        let command = load(&mut page);
        assert!(
            page.document
                .media_update(
                    &mut page.runtime,
                    command.node,
                    command.generation,
                    Update::FrameReady
                )
                .is_err()
        );
        assert_eq!(page.eval("events.length"), "0");
        assert!(update(
            &mut page,
            &command,
            Update::Metadata {
                duration: 4.,
                width: 320,
                height: 240,
                seekable: true
            }
        ));
        assert_eq!(page.eval("[v.readyState, v.duration, v.videoWidth, v.videoHeight, events.join(',')].join('|')"), "1|4|320|240|loadedmetadata");
        assert!(update(&mut page, &command, Update::FrameReady));
        assert!(!update(&mut page, &command, Update::FrameReady));
        assert_eq!(
            page.eval("[v.readyState, events.join(',')].join('|')"),
            "3|loadedmetadata,loadeddata,canplay"
        );
        let snapshot = page.document.snapshot();
        assert_eq!(snapshot.media[&command.node].width, 320);
        assert_eq!(snapshot.media[&command.node].height, 240);
    });
}

#[test]
fn play_pause_end_and_promises_follow_matching_host_results() {
    on_page(|| {
        let mut page = video();
        let source = load(&mut page);
        ready(&mut page, &source);
        page.eval("events.length = 0; v.play().then(() => result = 'ok', e => result = e.name)");
        let play = page.document.take_media_commands().pop().unwrap();
        assert!(matches!(play.kind, Command::Play));
        assert_eq!(page.eval("result"), "pending");
        assert!(update(
            &mut page,
            &play,
            Update::Playing {
                request_id: play.request_id
            }
        ));
        assert_eq!(
            page.eval("[result, v.paused, events.join(',')].join('|')"),
            "ok|false|play,playing"
        );
        assert!(update(&mut page, &play, Update::Time { current_time: 1.5 }));
        page.eval("v.pause()");
        let pause = page.document.take_media_commands().pop().unwrap();
        assert!(matches!(pause.kind, Command::Pause));
        assert!(!update(
            &mut page,
            &play,
            Update::Playing {
                request_id: play.request_id
            }
        ));
        assert!(update(
            &mut page,
            &pause,
            Update::Paused {
                request_id: pause.request_id
            }
        ));
        page.eval("v.play().then(() => result = 'again')");
        let play = page.document.take_media_commands().pop().unwrap();
        update(
            &mut page,
            &play,
            Update::Playing {
                request_id: play.request_id,
            },
        );
        update(&mut page, &play, Update::Ended);
        assert_eq!(page.eval("[result, v.paused, v.ended, v.currentTime, events.filter(x => x === 'ended').length].join('|')"), "again|true|true|4|1");
        assert!(!update(&mut page, &play, Update::Ended));
    });
}

#[test]
fn source_replacement_load_and_reentrant_listeners_invalidate_old_results() {
    on_page(|| {
        let mut page = video();
        let old = load(&mut page);
        page.eval("v.play().catch(e => result = e.name); v.setAttribute('src', 'next.webm')");
        page.runtime.run_microtasks();
        assert_eq!(page.eval("result"), "AbortError");
        let new = page.document.take_media_commands().pop().unwrap();
        assert!(new.generation > old.generation);
        assert!(!update(
            &mut page,
            &old,
            Update::Metadata {
                duration: 9.,
                width: 9,
                height: 9,
                seekable: true
            }
        ));
        page.eval("v.onloadeddata = () => { v.src = 'last.webm'; }");
        ready(&mut page, &new);
        assert_eq!(
            page.eval("[v.readyState, events.includes('canplay')].join('|')"),
            "0|false"
        );
        let last = page.document.take_media_commands().pop().unwrap();
        assert!(last.generation > new.generation);
        assert!(!update(&mut page, &new, Update::Ended));
        page.eval("v.load()");
        let reloaded = page.document.take_media_commands().pop().unwrap();
        assert!(reloaded.generation > last.generation);
    });
}

#[test]
fn host_failures_and_pause_reject_instead_of_faking_play_success() {
    on_page(|| {
        let mut page = video();
        let source = load(&mut page);
        ready(&mut page, &source);
        page.eval("events.length = 0; v.play().catch(e => result = e.name)");
        let play = page.document.take_media_commands().pop().unwrap();
        update(
            &mut page,
            &play,
            Update::CommandFailed {
                request_id: play.request_id,
                message: "decoder refused playback".into(),
            },
        );
        assert_eq!(
            page.eval("[result, v.paused, v.error.message, events.join(',')].join('|')"),
            "NotSupportedError|true|decoder refused playback|error"
        );
        page.eval("v.play().catch(e => result = e.name); v.pause()");
        page.runtime.run_microtasks();
        assert_eq!(page.eval("result"), "AbortError");
        update(
            &mut page,
            &source,
            Update::Error {
                code: 3,
                message: "invalid VP9".into(),
            },
        );
        assert_eq!(
            page.eval("[v.error.code, v.error.message, v.readyState].join('|')"),
            "3|invalid VP9|0"
        );
        assert!(!update(&mut page, &source, Update::FrameReady));
    });
}

#[test]
fn seek_settings_and_loop_have_explicit_commands_and_no_optimistic_seek() {
    on_page(|| {
        let mut page = video();
        let source = load(&mut page);
        assert!(page.try_eval("v.currentTime = 2").is_err());
        ready(&mut page, &source);
        page.eval("v.muted = true; v.loop = true; v.playbackRate = 2; v.currentTime = 2");
        let commands = page.document.take_media_commands();
        assert!(
            commands
                .iter()
                .any(|c| matches!(c.kind, Command::SetMuted { muted: true }))
        );
        assert!(
            commands
                .iter()
                .any(|c| matches!(c.kind, Command::SetLoop { looping: true }))
        );
        assert!(
            commands
                .iter()
                .any(|c| matches!(c.kind, Command::SetPlaybackRate { rate: 2. }))
        );
        let seek = commands
            .into_iter()
            .find(|c| matches!(c.kind, Command::Seek { .. }))
            .unwrap();
        assert_eq!(page.eval("[v.currentTime, v.seeking].join('|')"), "0|true");
        update(
            &mut page,
            &seek,
            Update::CommandFailed {
                request_id: seek.request_id,
                message: "seek unsupported".into(),
            },
        );
        assert_eq!(page.eval("[v.currentTime, v.seeking].join('|')"), "0|false");
        page.eval("v.currentTime = 1");
        let seek = page.document.take_media_commands().pop().unwrap();
        update(
            &mut page,
            &seek,
            Update::Seeked {
                request_id: seek.request_id,
                current_time: 1.,
            },
        );
        assert_eq!(page.eval("[v.currentTime, v.seeking].join('|')"), "1|false");
        assert!(page.try_eval("v.playbackRate = -1").is_err());
        page.eval("v.play().catch(() => {})");
        let play = page.document.take_media_commands().pop().unwrap();
        update(
            &mut page,
            &play,
            Update::Playing {
                request_id: play.request_id,
            },
        );
        update(&mut page, &source, Update::Ended);
        assert_eq!(
            page.eval("[v.ended, events.includes('ended')].join('|')"),
            "false|false"
        );
        assert!(
            page.document
                .take_media_commands()
                .iter()
                .any(|c| matches!(c.kind, Command::Seek { current_time: 0. }))
        );
    });
}

#[test]
fn removal_and_collection_dispose_media_without_rooting_detached_nodes() {
    on_page(|| {
        let mut page = video();
        let baseline = page.document.dom().len() - 1;
        let source = load(&mut page);
        page.eval("globalThis.promise = v.play().catch(e => result = e.name); v.remove()");
        page.runtime.run_microtasks();
        assert_eq!(page.eval("result"), "AbortError");
        let commands = page.document.take_media_commands();
        assert_eq!(commands.len(), 1);
        assert!(matches!(commands[0].kind, Command::Dispose));
        assert!(!update(&mut page, &source, Update::FrameReady));
        page.eval("v = null; promise = null");
        page.document.sweep(&mut page.runtime);
        assert_eq!(page.document.dom().len(), baseline);
        assert!(!page.document.snapshot().media.contains_key(&source.node));
        assert!(!update(&mut page, &source, Update::Ended));
        page.eval("(() => { const n = document.createElement('video'); n.src = 'blob:discard'; n.play().then(() => n.id); })()");
        page.document.sweep(&mut page.runtime);
        assert_eq!(page.document.dom().len(), baseline);
    });
}

#[test]
fn reused_slots_and_retained_promises_do_not_resurrect_media_nodes() {
    on_page(|| {
        let mut page = video();
        let old = load(&mut page);
        page.eval("v.remove(); v = null");
        page.document.sweep(&mut page.runtime);
        // Dispose 在 sweep 后仍要交给宿主释放旧代，不能随状态一起丢弃。
        let disposal = page.document.take_media_commands();
        assert_eq!(disposal.len(), 1);
        assert!(matches!(disposal[0].kind, Command::Dispose));
        page.eval("globalThis.next = document.createElement('video'); next.id = 'new'; document.body.append(next); next.src = 'new.webm'");
        let new = page.document.take_media_commands().pop().unwrap();
        assert_ne!(new.node, old.node);
        assert!(new.generation > old.generation);
        assert!(!update(
            &mut page,
            &old,
            Update::Metadata {
                duration: 99.,
                width: 99,
                height: 99,
                seekable: true
            }
        ));
        assert_eq!(page.eval("next.readyState"), "0");
        page.eval(
            r#"
            globalThis.retained = (() => {
                const detached = document.createElement('video');
                detached.src = 'detached.webm';
                const promise = detached.play();
                promise.catch(e => result = e.name);
                return promise;
            })();
        "#,
        );
        let count = page.document.dom().len();
        page.document.sweep(&mut page.runtime);
        page.runtime.run_microtasks();
        assert_eq!(page.document.dom().len(), count - 1);
        assert_eq!(page.eval("result"), "AbortError");
    });
}

#[test]
fn clearing_source_keeps_disposal_and_rejects_late_results() {
    on_page(|| {
        let mut page = video();
        let old = load(&mut page);
        page.eval("v.play().catch(e => result = e.name); v.src = ''; v.load(); v.load()");
        page.runtime.run_microtasks();
        assert_eq!(page.eval("result"), "AbortError");
        let commands = page.document.take_media_commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].generation, old.generation);
        assert!(matches!(commands[0].kind, Command::Dispose));
        assert!(!update(&mut page, &old, Update::FrameReady));
        assert_eq!(
            page.eval("[v.currentSrc, v.readyState, Number.isNaN(v.duration)].join('|')"),
            "|0|true"
        );
        page.eval("v.play().catch(e => result = e.name)");
        page.runtime.run_microtasks();
        assert_eq!(page.eval("result"), "NotSupportedError");
    });
}

#[test]
fn host_capabilities_and_preserved_settings_are_explicit() {
    on_page(|| {
        let mut page = video();
        page.eval("v.muted = true; v.defaultMuted = false; v.loop = true; v.playbackRate = 2");
        let source = load(&mut page);
        assert!(
            matches!(source.kind, Command::Load { settings, .. } if settings.muted && settings.looping && settings.playback_rate == 2.)
        );
        assert_eq!(
            page.eval("[v.muted, v.defaultMuted].join('|')"),
            "true|false"
        );
        update(
            &mut page,
            &source,
            Update::Metadata {
                duration: 4.,
                width: 1,
                height: 1,
                seekable: false,
            },
        );
        assert!(page.try_eval("v.currentTime = 1").is_err());
        assert!(page.try_eval("v.fastSeek(1)").is_err());
        assert!(page.document.take_media_commands().is_empty());
        update(&mut page, &source, Update::FrameReady);
        page.eval("v.play().catch(e => result = e.name)");
        page.document.take_media_commands();
        update(
            &mut page,
            &source,
            Update::Error {
                code: 3,
                message: "decoder failed".into(),
            },
        );
        assert_eq!(
            page.eval("[result, v.error.message].join('|')"),
            "NotSupportedError|decoder failed"
        );
        let mut foreign = verve::Runtime::new();
        assert!(
            page.document
                .media_update(
                    &mut foreign,
                    source.node,
                    source.generation,
                    Update::FrameReady
                )
                .is_err()
        );
    });
}

#[test]
fn repeated_play_seek_and_end_handlers_keep_request_identity() {
    on_page(|| {
        let mut page = video();
        let source = load(&mut page);
        ready(&mut page, &source);
        page.eval("globalThis.results = []; v.play().then(() => results.push(1)); v.play().then(() => results.push(2))");
        let commands = page.document.take_media_commands();
        assert_eq!(commands.len(), 1);
        let play = &commands[0];
        update(
            &mut page,
            play,
            Update::Playing {
                request_id: play.request_id,
            },
        );
        assert_eq!(page.eval("results.join(',')"), "1,2");
        page.eval("v.currentTime = 1");
        let first = page.document.take_media_commands().pop().unwrap();
        page.eval("v.currentTime = 2");
        let second = page.document.take_media_commands().pop().unwrap();
        assert!(!update(
            &mut page,
            &first,
            Update::Seeked {
                request_id: first.request_id,
                current_time: 1.
            }
        ));
        assert!(!update(
            &mut page,
            &source,
            Update::Time { current_time: 0.5 }
        ));
        update(
            &mut page,
            &second,
            Update::Seeked {
                request_id: second.request_id,
                current_time: 2.,
            },
        );
        assert_eq!(page.eval("v.currentTime"), "2");
        page.eval(
            "v.onended = () => v.play().then(() => results.push(3), e => results.push(e.name))",
        );
        update(&mut page, &source, Update::Ended);
        let commands = page.document.take_media_commands();
        let seek = commands
            .iter()
            .find(|c| matches!(c.kind, Command::Seek { .. }))
            .unwrap();
        let play = commands
            .iter()
            .find(|c| matches!(c.kind, Command::Play))
            .unwrap();
        update(
            &mut page,
            seek,
            Update::Seeked {
                request_id: seek.request_id,
                current_time: 0.,
            },
        );
        update(
            &mut page,
            play,
            Update::Playing {
                request_id: play.request_id,
            },
        );
        assert_eq!(
            page.eval("[results.join(','), v.ended, v.paused].join('|')"),
            "1,2,3|false|false"
        );
    });
}
