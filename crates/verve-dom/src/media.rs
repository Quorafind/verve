//! GUI 无关的媒体协议。这里保存状态与命令，不保存解码器、时钟或像素。

use std::collections::HashMap;
use std::rc::Rc;

use boa_engine::{
    Context, Finalize, JsData, JsObject, JsResult, JsSymbol, JsValue, Trace,
    builtins::promise::ResolvingFunctions,
    object::{WeakJsObject, builtins::JsPromise},
    property::PropertyDescriptor,
};

use crate::bind::{self, Accessor, Method, arg, dom_error, node_of, text, type_error};
use crate::state::shared;
use crate::{Document, NodeId};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MediaSettings {
    pub muted: bool,
    /// 循环由 DOM 在真实 Ended 后发出 Seek/Play；宿主仍须报告每次实际到达结尾。
    pub looping: bool,
    pub playback_rate: f64,
    pub volume: f64,
}

impl Default for MediaSettings {
    fn default() -> Self {
        Self {
            muted: false,
            looping: false,
            playback_rate: 1.,
            volume: 1.,
        }
    }
}

/// 所有命令按队列顺序执行；宿主不能把 URL 当网络授权，也不能自行联网。
#[derive(Clone, Debug, PartialEq)]
pub struct MediaCommand {
    pub node: NodeId,
    pub generation: u64,
    pub request_id: u64,
    pub kind: MediaCommandKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MediaCommandKind {
    /// blob 地址从 Runtime::object_url_bytes 读取；其他地址由宿主权限策略处理。
    Load {
        src: String,
        settings: MediaSettings,
    },
    Play,
    Pause,
    Seek {
        current_time: f64,
    },
    SetMuted {
        muted: bool,
    },
    SetLoop {
        looping: bool,
    },
    SetPlaybackRate {
        rate: f64,
    },
    SetVolume {
        volume: f64,
    },
    /// 释放该代的所有后端资源，即使节点已被回收也必须执行。
    Dispose,
}

/// 宿主按同代的实际发生顺序交付反馈；首帧必须在 Metadata 后交付。
#[derive(Clone, Debug, PartialEq)]
pub enum MediaUpdate {
    Metadata {
        duration: f64,
        width: u32,
        height: u32,
        seekable: bool,
    },
    /// 已真正解码并可呈现的首帧，而不是收到字节或启动了解码进程。
    FrameReady,
    Playing {
        request_id: u64,
    },
    Paused {
        request_id: u64,
    },
    Seeked {
        request_id: u64,
        current_time: f64,
    },
    Time {
        current_time: f64,
    },
    Ended,
    /// 本代不可恢复的源/解码错误；code 为 HTML MediaError 的 1..=4。
    Error {
        code: u16,
        message: String,
    },
    /// 命令不受支持或执行失败。Seek 失败不会修改 currentTime。
    CommandFailed {
        request_id: u64,
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaError {
    pub code: u16,
    pub message: String,
}

/// 可跨线程读取的当前元数据，没有 GUI 句柄和像素。
#[derive(Clone, Debug, PartialEq)]
pub struct MediaState {
    pub generation: u64,
    pub src: String,
    pub duration: Option<f64>,
    pub width: u32,
    pub height: u32,
    pub current_time: f64,
    pub ready_state: u8,
    pub network_state: u8,
    pub paused: bool,
    pub ended: bool,
    pub seeking: bool,
    pub seekable: bool,
    /// 请求的输出策略；不支持的命令必须收到 CommandFailed。
    /// 暂停和时间以宿主回执为准，settings 保留脚本请求的策略值。
    pub settings: MediaSettings,
    pub error: Option<MediaError>,
}

impl Default for MediaState {
    fn default() -> Self {
        Self {
            generation: 0,
            src: String::new(),
            duration: None,
            width: 0,
            height: 0,
            current_time: 0.,
            ready_state: 0,
            network_state: 0,
            paused: true,
            ended: false,
            seeking: false,
            seekable: false,
            settings: MediaSettings::default(),
            error: None,
        }
    }
}

#[derive(Default)]
pub(crate) struct MediaStore {
    pub records: HashMap<NodeId, Record>,
    commands: Vec<MediaCommand>,
    serial: u64,
}

#[derive(Default)]
pub(crate) struct Record {
    pub data: MediaState,
    source: Option<String>,
    connected: bool,
    pending: Vec<(u64, WeakJsObject)>,
    transport: Option<(u64, bool)>,
    seek: Option<u64>,
    restart_after_seek: bool,
    terminal: bool,
    muted_override: bool,
    settings_requests: Vec<(u64, MediaCommandKind)>,
}

impl MediaStore {
    fn next(&mut self) -> u64 {
        self.serial = self.serial.checked_add(1).expect("媒体请求编号已耗尽");
        self.serial
    }

    fn command(&mut self, node: NodeId, kind: MediaCommandKind) -> u64 {
        let request_id = self.next();
        let generation = self.records[&node].data.generation;
        self.commands.push(MediaCommand {
            node,
            generation,
            request_id,
            kind,
        });
        request_id
    }

    pub fn snapshot(&self) -> HashMap<NodeId, MediaState> {
        self.records
            .iter()
            .filter(|(_, record)| record.data.generation != 0)
            .map(|(id, record)| (*id, record.data.clone()))
            .collect()
    }
}

// Promise 只由 JS 图保活。Rust 状态只握弱引用，避免 then 回调闭包把脱离的节点永久钉住。
#[derive(Trace, Finalize, JsData)]
struct PendingPlay {
    resolvers: ResolvingFunctions,
}

fn promise(context: &mut Context, node: NodeId, request: u64) -> JsResult<JsValue> {
    let (promise, resolvers) = JsPromise::new_pending(context);
    let pending = JsObject::from_proto_and_data(None, PendingPlay { resolvers });
    JsObject::from(promise.clone()).define_property_or_throw(
        JsSymbol::new(None).expect("媒体 Promise 的 symbol 空间不会耗尽"),
        PropertyDescriptor::builder().value(pending.clone()),
        context,
    )?;
    let wrapper = bind::node::wrap(context, node)
        .as_object()
        .expect("a media node");
    verve::dom_events::hold(&wrapper, |held| held.push(pending.clone()));
    shared(context)
        .borrow_mut()
        .media
        .records
        .get_mut(&node)
        .expect("a record")
        .pending
        .push((request, pending.downgrade()));
    Ok(promise.into())
}

fn settle(
    context: &mut Context,
    node: NodeId,
    request: Option<u64>,
    failure: Option<(&str, &str)>,
) -> JsResult<()> {
    let pending = {
        let state = shared(context);
        let mut state = state.borrow_mut();
        let Some(record) = state.media.records.get_mut(&node) else {
            return Ok(());
        };
        let mut taken = Vec::new();
        record.pending.retain(|(id, weak)| {
            if request.is_none_or(|wanted| *id == wanted) {
                if let Some(object) = weak.upgrade() {
                    taken.push(object);
                }
                false
            } else {
                true
            }
        });
        taken
    };
    if let Some(wrapper) = bind::node::existing_wrapper(context, node) {
        verve::dom_events::hold(&wrapper, |held| {
            held.retain(|object| !pending.contains(object))
        });
    }
    let reason = failure
        .map(|(name, message)| dom_error(context, message, name).into_opaque(context))
        .transpose()?;
    for object in pending {
        let resolvers = object
            .downcast_ref::<PendingPlay>()
            .expect("pending play")
            .resolvers
            .clone();
        match &reason {
            Some(reason) => {
                resolvers
                    .reject
                    .call(&JsValue::undefined(), &[reason.clone()], context)?
            }
            None => {
                resolvers
                    .resolve
                    .call(&JsValue::undefined(), &[JsValue::undefined()], context)?
            }
        };
    }
    Ok(())
}

fn active(context: &Context, node: NodeId, generation: u64) -> bool {
    let state = shared(context);
    let state = state.borrow();
    state.dom.exists(node)
        && generation != 0
        && state
            .media
            .records
            .get(&node)
            .is_some_and(|record| record.data.generation == generation && !record.terminal)
}

fn fire(context: &mut Context, node: NodeId, kind: &str) -> JsResult<()> {
    bind::events::fire_at(
        context,
        node,
        "Event",
        kind,
        &[("bubbles", false.into()), ("cancelable", false.into())],
        true,
    )?;
    Ok(())
}

fn ensure(context: &mut Context, node: NodeId) -> JsResult<()> {
    let state = shared(context);
    let mut state = state.borrow_mut();
    if !state.dom.get(node).is_some_and(|n| {
        n.namespace == crate::Namespace::Html && matches!(n.tag(), Some("video" | "audio"))
    }) {
        return Err(type_error("Illegal invocation: not a live media element"));
    }
    state.media.records.entry(node).or_default();
    Ok(())
}

/// DOM 自身的变更队列不依赖 MutationObserver，也不消耗绘制层的变化。
pub(crate) fn sync(context: &mut Context) -> JsResult<()> {
    let nodes = shared(context).borrow_mut().dom.take_media_changes();
    for node in nodes {
        let config = {
            let state = shared(context);
            let state = state.borrow();
            state.dom.get(node).map(|n| {
                (
                    n.attr("src").map(str::to_owned),
                    state.dom.is_connected(node),
                    n.attr("loop").is_some(),
                    n.attr("muted").is_some(),
                )
            })
        };
        let Some((source, connected, looping, muted)) = config else {
            forget(context, node)?;
            continue;
        };
        ensure(context, node)?;
        let (removed, changed, loop_changed) = {
            let state = shared(context);
            let mut state = state.borrow_mut();
            let record = state.media.records.get_mut(&node).expect("record");
            let removed = record.connected && !connected;
            let changed = record.source != source
                || (connected && !record.connected && record.data.generation == 0);
            let loop_changed = record.data.settings.looping != looping;
            record.connected = connected;
            record.source = source;
            // muted 属性是 defaultMuted；初次创建时采用它，之后 muted 属性和运行时静音独立。
            if record.data.generation == 0 && !record.muted_override {
                record.data.settings.muted = muted;
            }
            record.data.settings.looping = looping;
            (removed, changed, loop_changed)
        };
        if removed {
            dispose(context, node)?;
        } else if changed {
            load(context, node)?;
        } else if loop_changed {
            queue_setting(context, node, MediaCommandKind::SetLoop { looping });
        }
    }
    Ok(())
}

fn dispose(context: &mut Context, node: NodeId) -> JsResult<()> {
    settle(context, node, None, Some(("AbortError", "媒体节点已移除")))?;
    let state = shared(context);
    let mut state = state.borrow_mut();
    state
        .media
        .commands
        .retain(|c| c.node != node || matches!(c.kind, MediaCommandKind::Dispose));
    let generation = state
        .media
        .records
        .get(&node)
        .map_or(0, |r| r.data.generation);
    if generation != 0 {
        state.media.command(node, MediaCommandKind::Dispose);
    }
    if let Some(record) = state.media.records.get_mut(&node) {
        let settings = record.data.settings;
        record.data = MediaState {
            settings,
            ..MediaState::default()
        };
        record.transport = None;
        record.seek = None;
        record.restart_after_seek = false;
        record.terminal = false;
        record.settings_requests.clear();
    }
    state.dom.mark(node);
    Ok(())
}

pub(crate) fn forget(context: &mut Context, node: NodeId) -> JsResult<()> {
    if shared(context).borrow().media.records.contains_key(&node) {
        dispose(context, node)?;
        shared(context).borrow_mut().media.records.remove(&node);
    }
    Ok(())
}

fn load(context: &mut Context, node: NodeId) -> JsResult<()> {
    ensure(context, node)?;
    settle(
        context,
        node,
        None,
        Some(("AbortError", "媒体源已重新加载")),
    )?;
    let state = shared(context);
    let mut state = state.borrow_mut();
    let source = state.dom.get_attr(node, "src");
    // 无法解析的原文仍交给宿主返回真实 Error，不能把坏源悄悄当成空源。
    let src = source.as_ref().filter(|s| !s.is_empty()).map(|source| {
        state
            .url
            .join(source)
            .map(String::from)
            .unwrap_or_else(|_| source.clone())
    });
    let generation = state.media.next();
    state.media.commands.retain(|c| {
        c.node != node || (src.is_none() && matches!(c.kind, MediaCommandKind::Dispose))
    });
    let record = state.media.records.get_mut(&node).expect("record");
    let previous_generation = record.data.generation;
    let settings = record.data.settings;
    record.source = source;
    record.transport = None;
    record.seek = None;
    record.restart_after_seek = false;
    record.terminal = false;
    record.settings_requests.clear();
    record.data = MediaState {
        generation,
        src: src.clone().unwrap_or_default(),
        settings,
        network_state: if src.is_some() { 2 } else { 0 },
        ..MediaState::default()
    };
    if let Some(src) = src {
        state
            .media
            .command(node, MediaCommandKind::Load { src, settings });
    } else {
        // 空源同样使旧回执失效；Dispose 仍指向真正占用后端资源的旧代。
        record.data.generation = 0;
        if previous_generation != 0 {
            let request_id = state.media.next();
            state.media.commands.push(MediaCommand {
                node,
                generation: previous_generation,
                request_id,
                kind: MediaCommandKind::Dispose,
            });
        }
    }
    state.dom.mark(node);
    Ok(())
}

fn queue_setting(context: &mut Context, node: NodeId, kind: MediaCommandKind) {
    let state = shared(context);
    let mut state = state.borrow_mut();
    if state.media.records[&node].data.generation != 0 && !state.media.records[&node].terminal {
        let request = state.media.command(node, kind.clone());
        let pending = &mut state
            .media
            .records
            .get_mut(&node)
            .expect("record")
            .settings_requests;
        pending.retain(|(_, previous)| {
            std::mem::discriminant(previous) != std::mem::discriminant(&kind)
        });
        pending.push((request, kind));
    }
    state.dom.mark(node);
}

fn play(context: &mut Context, node: NodeId) -> JsResult<JsValue> {
    ensure(context, node)?;
    let needs_load = shared(context).borrow().media.records[&node]
        .data
        .generation
        == 0;
    if needs_load {
        load(context, node)?;
    }
    let (ended, seeking, generation) = {
        let state = shared(context);
        let state = state.borrow();
        let data = &state.media.records[&node].data;
        (data.ended, data.seeking, data.generation)
    };
    if ended && !seeking {
        seek(context, node, 0.)?;
        if !active(context, node, generation) {
            let error = dom_error(context, "媒体源在 seeking 事件中已失效", "AbortError");
            return bind::rejected(error, context);
        }
    }
    let (generation, terminal, error, ready, paused, existing) = {
        let state = shared(context);
        let state = state.borrow();
        let r = &state.media.records[&node];
        (
            r.data.generation,
            r.terminal,
            r.data.error.clone(),
            r.data.ready_state,
            r.data.paused,
            r.transport,
        )
    };
    if generation == 0 || terminal {
        let error = dom_error(
            context,
            error
                .as_ref()
                .map_or("没有可播放的媒体源", |e| e.message.as_str()),
            "NotSupportedError",
        );
        return bind::rejected(error, context);
    }
    if !paused && ready >= 3 && existing.is_none() {
        return bind::resolved(JsValue::undefined(), context);
    }
    let request = if let Some((request, false)) = existing {
        request
    } else {
        let state = shared(context);
        let mut state = state.borrow_mut();
        let request = state.media.command(node, MediaCommandKind::Play);
        state
            .media
            .records
            .get_mut(&node)
            .expect("record")
            .transport = Some((request, false));
        request
    };
    promise(context, node, request)
}

fn pause(context: &mut Context, node: NodeId) -> JsResult<()> {
    ensure(context, node)?;
    settle(
        context,
        node,
        None,
        Some(("AbortError", "play() 被 pause() 中断")),
    )?;
    let state = shared(context);
    let mut state = state.borrow_mut();
    let r = &state.media.records[&node];
    if r.data.generation != 0 && (!r.data.paused || r.transport.is_some()) {
        let request = state.media.command(node, MediaCommandKind::Pause);
        let r = state.media.records.get_mut(&node).expect("record");
        r.transport = Some((request, true));
        r.restart_after_seek = false;
    }
    Ok(())
}

fn seek(context: &mut Context, node: NodeId, position: f64) -> JsResult<()> {
    ensure(context, node)?;
    if !position.is_finite() || position < 0. {
        return Err(type_error("currentTime 必须是有限非负数"));
    }
    let supported = {
        let state = shared(context);
        let state = state.borrow();
        let r = &state.media.records[&node];
        !r.terminal && r.data.ready_state >= 1 && r.data.seekable
    };
    if !supported {
        return Err(dom_error(
            context,
            "宿主尚未确认媒体可 seek",
            "NotSupportedError",
        ));
    }
    {
        let state = shared(context);
        let mut state = state.borrow_mut();
        let duration = state.media.records[&node]
            .data
            .duration
            .unwrap_or(f64::INFINITY);
        let request = state.media.command(
            node,
            MediaCommandKind::Seek {
                current_time: position.min(duration),
            },
        );
        let r = state.media.records.get_mut(&node).expect("record");
        r.seek = Some(request);
        r.data.seeking = true;
        state.dom.mark(node);
    }
    fire(context, node, "seeking")
}

impl Document {
    /// 排空媒体命令。Load 隐含释放同 node 的旧代，Dispose 释放指定代。
    /// 宿主必须串行处理命令，且在每次异步操作完成时核对 node/generation/request_id。
    pub fn take_media_commands(&self) -> Vec<MediaCommand> {
        std::mem::take(&mut self.state.borrow_mut().media.commands)
    }

    /// 注入真实宿主反馈并交付事件、Promise 微任务。陈旧回执返回 false，非法回执返回错误。
    /// Context 必须来自安装这个 Document 的 Runtime，不能跨 realm 注入。
    pub fn media_update(
        &self,
        runtime: &mut verve::Runtime,
        node: NodeId,
        generation: u64,
        update: MediaUpdate,
    ) -> JsResult<bool> {
        let context = runtime.context();
        let installed = crate::state::try_shared(context);
        if !installed
            .as_ref()
            .is_some_and(|state| Rc::ptr_eq(&self.state, state))
        {
            return Err(type_error("媒体反馈来自不同的 Runtime"));
        }
        if !active(context, node, generation) {
            return Ok(false);
        }
        let Some(effects) = apply(context, node, update)? else {
            return Ok(false);
        };
        for event in effects.events {
            // 事件处理器可以同步换源、移除节点或改变 transport，后续旧事件不能泄漏。
            if !same_generation(context, node, generation) {
                break;
            }
            if let Some((request, paused)) = effects.transport {
                let current = shared(context).borrow().media.records[&node].transport;
                if current != Some((request, paused)) {
                    break;
                }
            }
            fire(context, node, event)?;
        }
        if same_generation(context, node, generation) {
            if let Some((request, failure)) = effects.settlement {
                settle(
                    context,
                    node,
                    request,
                    failure
                        .as_ref()
                        .map(|(name, message)| (name.as_str(), message.as_str())),
                )?;
            }
            if let Some(transport) = effects.transport {
                let state = shared(context);
                let mut state = state.borrow_mut();
                let r = state.media.records.get_mut(&node).expect("record");
                if r.transport == Some(transport) {
                    r.transport = None;
                }
            }
        }
        runtime.run_microtasks();
        Ok(true)
    }
}

fn same_generation(context: &Context, node: NodeId, generation: u64) -> bool {
    let state = shared(context);
    let state = state.borrow();
    state.dom.exists(node)
        && state
            .media
            .records
            .get(&node)
            .is_some_and(|r| r.data.generation == generation)
}

#[derive(Default)]
struct Effects {
    events: Vec<&'static str>,
    settlement: Option<(Option<u64>, Option<(String, String)>)>,
    transport: Option<(u64, bool)>,
}

fn position(value: f64) -> JsResult<f64> {
    if value.is_finite() && value >= 0. {
        Ok(value)
    } else {
        Err(type_error("宿主媒体时间必须是有限非负数"))
    }
}

fn apply(context: &mut Context, node: NodeId, update: MediaUpdate) -> JsResult<Option<Effects>> {
    let state = shared(context);
    let mut state = state.borrow_mut();
    let r = state.media.records.get_mut(&node).expect("active record");
    let mut effects = Effects::default();
    match update {
        MediaUpdate::Metadata {
            duration,
            width,
            height,
            seekable,
        } => {
            if duration.is_nan() || duration < 0. {
                return Err(type_error("宿主 duration 必须非负或为正无穷"));
            }
            if r.data.ready_state == 0 {
                effects.events.push("loadedmetadata");
            } else if r.data.duration != Some(duration) {
                effects.events.push("durationchange");
            }
            r.data.duration = Some(duration);
            r.data.width = width;
            r.data.height = height;
            r.data.seekable = seekable;
            r.data.ready_state = r.data.ready_state.max(1);
        }
        MediaUpdate::FrameReady => {
            if r.data.ready_state == 0 {
                return Err(type_error("首帧之前必须先交付 Metadata"));
            }
            if r.data.ready_state >= 3 {
                return Ok(None);
            }
            r.data.ready_state = 3;
            r.data.network_state = 1;
            effects.events.extend(["loadeddata", "canplay"]);
        }
        MediaUpdate::Playing { request_id } => {
            if r.transport != Some((request_id, false)) {
                return Ok(None);
            }
            if r.data.ready_state < 3 {
                return Err(type_error("真正的首帧之前不能确认 Playing"));
            }
            if r.data.paused {
                effects.events.push("play");
            }
            effects.events.push("playing");
            r.data.paused = false;
            r.data.ended = false;
            r.data.error = None;
            effects.transport = r.transport;
            effects.settlement = Some((Some(request_id), None));
        }
        MediaUpdate::Paused { request_id } => {
            if r.transport != Some((request_id, true)) {
                return Ok(None);
            }
            r.data.paused = true;
            effects.events.push("pause");
            effects.transport = r.transport;
        }
        MediaUpdate::Time { current_time } => {
            let time = position(current_time)?;
            if r.data.ready_state < 3 || r.data.seeking || r.data.ended {
                return Ok(None);
            }
            r.data.current_time = time.min(r.data.duration.unwrap_or(f64::INFINITY));
            effects.events.push("timeupdate");
        }
        MediaUpdate::Seeked {
            request_id,
            current_time,
        } => {
            if r.seek != Some(request_id) {
                return Ok(None);
            }
            r.data.current_time =
                position(current_time)?.min(r.data.duration.unwrap_or(f64::INFINITY));
            r.data.seeking = false;
            r.data.ended = false;
            r.seek = None;
            effects.events.extend(["timeupdate", "seeked"]);
            if r.restart_after_seek {
                r.restart_after_seek = false;
                let request = state.media.command(node, MediaCommandKind::Play);
                state
                    .media
                    .records
                    .get_mut(&node)
                    .expect("record")
                    .transport = Some((request, false));
            }
        }
        MediaUpdate::Ended => {
            if r.data.ready_state < 3 || r.data.ended || r.data.seeking || r.data.paused {
                return Ok(None);
            }
            if r.data.settings.looping && r.data.seekable {
                r.data.seeking = true;
                r.restart_after_seek = true;
                let request = state
                    .media
                    .command(node, MediaCommandKind::Seek { current_time: 0. });
                state.media.records.get_mut(&node).expect("record").seek = Some(request);
                effects.events.push("seeking");
            } else {
                r.data.paused = true;
                r.data.ended = true;
                effects.settlement = r.transport.map(|(request, _)| {
                    (
                        Some(request),
                        Some(("AbortError".into(), "媒体播放已经结束".into())),
                    )
                });
                r.transport = None;
                if let Some(duration) = r.data.duration.filter(|d| d.is_finite()) {
                    r.data.current_time = duration;
                }
                effects.events.extend(["timeupdate", "ended"]);
            }
        }
        MediaUpdate::Error { code, message } => {
            if !(1..=4).contains(&code) {
                return Err(type_error("MediaError code 必须为 1..=4"));
            }
            r.data.error = Some(MediaError {
                code,
                message: message.clone(),
            });
            r.data.paused = true;
            r.data.seeking = false;
            r.data.ready_state = 0;
            r.data.network_state = 3;
            r.transport = None;
            r.seek = None;
            r.restart_after_seek = false;
            r.terminal = true;
            effects.events.push("error");
            effects.settlement = Some((None, Some(("NotSupportedError".into(), message))));
            state.media.commands.retain(|c| c.node != node);
        }
        MediaUpdate::CommandFailed {
            request_id,
            message,
        } => {
            let play = r.transport == Some((request_id, false));
            let pause = r.transport == Some((request_id, true));
            let seek = r.seek == Some(request_id);
            let setting = r.settings_requests.iter().any(|(id, _)| *id == request_id);
            if !play && !pause && !seek && !setting {
                return Ok(None);
            }
            r.settings_requests.retain(|(id, _)| *id != request_id);
            if play || pause {
                r.transport = None;
            }
            if play {
                r.data.paused = true;
            }
            if seek {
                r.seek = None;
                r.data.seeking = false;
                r.restart_after_seek = false;
            }
            r.data.error = Some(MediaError {
                code: 4,
                message: message.clone(),
            });
            effects.events.push("error");
            effects.settlement = Some((
                Some(request_id),
                Some(("NotSupportedError".into(), message)),
            ));
        }
    }
    state.dom.mark(node);
    Ok(Some(effects))
}

pub(crate) const METHODS: &[Method] = &[
    ("play", 0, |this, _, cx| {
        let node = node_of(this)?;
        match play(cx, node) {
            Ok(promise) => Ok(promise),
            Err(error) => bind::rejected(error, cx),
        }
    }),
    ("pause", 0, |this, _, cx| {
        let node = node_of(this)?;
        pause(cx, node)?;
        Ok(JsValue::undefined())
    }),
    ("load", 0, |this, _, cx| {
        let node = node_of(this)?;
        load(cx, node)?;
        Ok(JsValue::undefined())
    }),
    // 解码能力由宿主决定，不能凭 MIME 文本保证本机存在某个解码器。
    ("canPlayType", 1, |_, _, _| Ok(text(""))),
    ("fastSeek", 1, |this, args, cx| {
        let node = node_of(this)?;
        let time = arg(args, 0).to_number(cx)?;
        seek(cx, node, time)?;
        Ok(JsValue::undefined())
    }),
    ("addTextTrack", 3, |_, _, cx| {
        Ok(bind::plain_object(cx).into())
    }),
];

fn read(
    this: &JsValue,
    context: &mut Context,
    f: impl FnOnce(&MediaState) -> JsValue,
) -> JsResult<JsValue> {
    let node = node_of(this)?;
    ensure(context, node)?;
    Ok(f(&shared(context).borrow().media.records[&node].data))
}

fn setting(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
    key: &str,
) -> JsResult<JsValue> {
    let node = node_of(this)?;
    ensure(context, node)?;
    let value = arg(args, 0);
    let number = if key == "muted" {
        0.
    } else {
        value.to_number(context)?
    };
    if key == "playbackRate" && (!number.is_finite() || number <= 0. || number > 16.) {
        return Err(dom_error(
            context,
            "只支持大于 0 且不超过 16 的有限播放速率",
            "NotSupportedError",
        ));
    }
    if key == "volume" && (!number.is_finite() || !(0. ..=1.).contains(&number)) {
        return Err(dom_error(
            context,
            "volume 必须在 0 到 1 之间",
            "IndexSizeError",
        ));
    }
    let command = {
        let state = shared(context);
        let mut state = state.borrow_mut();
        let record = state.media.records.get_mut(&node).expect("record");
        if key == "muted" {
            record.muted_override = true;
        }
        let settings = &mut record.data.settings;
        match key {
            "muted" => {
                settings.muted = value.to_boolean();
                MediaCommandKind::SetMuted {
                    muted: settings.muted,
                }
            }
            "playbackRate" => {
                settings.playback_rate = number;
                MediaCommandKind::SetPlaybackRate { rate: number }
            }
            _ => {
                settings.volume = number;
                MediaCommandKind::SetVolume { volume: number }
            }
        }
    };
    queue_setting(context, node, command);
    Ok(JsValue::undefined())
}

pub(crate) const ACCESSORS: &[Accessor] = &[
    (
        "paused",
        Some(|this, _, cx| read(this, cx, |s| s.paused.into())),
        None,
    ),
    (
        "ended",
        Some(|this, _, cx| read(this, cx, |s| s.ended.into())),
        None,
    ),
    (
        "duration",
        Some(|this, _, cx| read(this, cx, |s| s.duration.unwrap_or(f64::NAN).into())),
        None,
    ),
    (
        "readyState",
        Some(|this, _, cx| read(this, cx, |s| u32::from(s.ready_state).into())),
        None,
    ),
    (
        "networkState",
        Some(|this, _, cx| read(this, cx, |s| u32::from(s.network_state).into())),
        None,
    ),
    (
        "seeking",
        Some(|this, _, cx| read(this, cx, |s| s.seeking.into())),
        None,
    ),
    (
        "videoWidth",
        Some(|this, _, cx| read(this, cx, |s| s.width.into())),
        None,
    ),
    (
        "videoHeight",
        Some(|this, _, cx| read(this, cx, |s| s.height.into())),
        None,
    ),
    (
        "currentSrc",
        Some(|this, _, cx| read(this, cx, |s| text(&s.src))),
        None,
    ),
    (
        "currentTime",
        Some(|this, _, cx| read(this, cx, |s| s.current_time.into())),
        Some(|this, args, cx| {
            let node = node_of(this)?;
            let time = arg(args, 0).to_number(cx)?;
            seek(cx, node, time)?;
            Ok(JsValue::undefined())
        }),
    ),
    (
        "muted",
        Some(|this, _, cx| read(this, cx, |s| s.settings.muted.into())),
        Some(|this, args, cx| setting(this, args, cx, "muted")),
    ),
    (
        "playbackRate",
        Some(|this, _, cx| read(this, cx, |s| s.settings.playback_rate.into())),
        Some(|this, args, cx| setting(this, args, cx, "playbackRate")),
    ),
    (
        "volume",
        Some(|this, _, cx| read(this, cx, |s| s.settings.volume.into())),
        Some(|this, args, cx| setting(this, args, cx, "volume")),
    ),
    (
        "error",
        Some(|this, _, cx| {
            let node = node_of(this)?;
            ensure(cx, node)?;
            let error = shared(cx).borrow().media.records[&node].data.error.clone();
            match error {
                None => Ok(JsValue::null()),
                Some(error) => {
                    let object = bind::plain_object(cx);
                    bind::set(&object, "code", u32::from(error.code), cx)?;
                    bind::set(&object, "message", text(&error.message), cx)?;
                    Ok(object.into())
                }
            }
        }),
        None,
    ),
    (
        "buffered",
        Some(|_, _, cx| bind::platform::empty_ranges(cx)),
        None,
    ),
    (
        "played",
        Some(|_, _, cx| bind::platform::empty_ranges(cx)),
        None,
    ),
    (
        "seekable",
        Some(|_, _, cx| bind::platform::empty_ranges(cx)),
        None,
    ),
    (
        "textTracks",
        Some(|_, _, cx| Ok(bind::array(Vec::new(), cx))),
        None,
    ),
];
