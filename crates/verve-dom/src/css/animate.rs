//! Transitions and animations (CSS Transitions 1, CSS Animations 1, Web Animations 1): what a
//! property's value is between the two the cascade gives it, and the events that mark the way.
//!
//! The timeline lives in the document's state and moves a frame at a time, so `getComputedStyle`
//! reads the value of the moment and the events fall on the frame they belong to. Nothing here
//! runs unless something animates: the store is empty, and a restyle looks only at the nodes it
//! restyled. What a script asks of an animation (play it, move it, change its rate) takes effect
//! at the next frame, as it does in a browser, so it does not depend on when the last one was.

use std::collections::HashMap;
use std::sync::Arc;

use super::calc::milliseconds;
use super::cascade::{Computed, Style};
use super::easing::Easing;
use super::interpolate::{can_interpolate, interpolate};
use super::sheet::{Declaration, declarations};
use super::shorthand::{expansion, longhands};
use super::styler::Styler;
use super::syntax::{matching_brace, split_outside};
use super::values::{initial_value, substitute};
use crate::dom::NodeId;

/// Which way the iterations of an animation run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Direction {
    Normal,
    Reverse,
    Alternate,
    AlternateReverse,
}

impl Direction {
    /// The direction `animation-direction` or an `animate()` option names; `Normal` for any
    /// other word.
    pub(crate) fn from_name(name: &str) -> Self {
        match name.trim() {
            "reverse" => Self::Reverse,
            "alternate" => Self::Alternate,
            "alternate-reverse" => Self::AlternateReverse,
            _ => Self::Normal,
        }
    }
}

/// What an animation does before it begins and after it ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fill {
    None,
    Forwards,
    Backwards,
    Both,
}

impl Fill {
    /// The fill mode `animation-fill-mode` or an `animate()` option names; `None` for any
    /// other word (`auto` too).
    pub(crate) fn from_name(name: &str) -> Self {
        match name.trim() {
            "forwards" => Self::Forwards,
            "backwards" => Self::Backwards,
            "both" => Self::Both,
            _ => Self::None,
        }
    }

    fn holds_after(self) -> bool {
        matches!(self, Self::Forwards | Self::Both)
    }

    fn holds_before(self) -> bool {
        matches!(self, Self::Backwards | Self::Both)
    }
}

/// Where an animation stands, as `Animation.playState` says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlayState {
    /// Cancelled: it applies nothing and has no time.
    Idle,
    Running,
    Paused,
    Finished,
}

impl PlayState {
    /// The word `playState` answers.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Finished => "finished",
        }
    }
}

/// When an animation goes, and how it moves along. Times are milliseconds.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Timing {
    /// How long one iteration lasts.
    pub(crate) duration: f64,
    /// How long it waits before the first.
    pub(crate) delay: f64,
    /// How the progress of an iteration follows its time.
    pub(crate) easing: Easing,
    /// How many times it runs; infinite is `f64::INFINITY`.
    pub(crate) iterations: f64,
    pub(crate) direction: Direction,
    pub(crate) fill: Fill,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            duration: 0.,
            delay: 0.,
            easing: Easing::Linear,
            iterations: 1.,
            direction: Direction::Normal,
            fill: Fill::None,
        }
    }
}

impl Timing {
    /// When it ends, from the start of its delay; infinite for one that never does.
    pub(crate) fn end(&self) -> f64 {
        self.delay + self.active()
    }

    /// How long it is active: all its iterations, without the delay.
    fn active(&self) -> f64 {
        self.duration * self.iterations
    }

    /// The progress within an iteration at `local`, `None` when it is applying nothing.
    fn progress(&self, local: f64) -> Option<f32> {
        if local < self.delay {
            return self
                .fill
                .holds_before()
                .then(|| self.iteration_progress(0., 0.));
        }
        if self.duration <= 0. {
            return Some(1.);
        }
        let overall = (local - self.delay) / self.duration;
        if overall >= self.iterations {
            // The last iteration, whole or the part of one that was asked for.
            let whole = (self.iterations.ceil() - 1.).max(0.);
            let frac = if self.iterations.fract() == 0. {
                1.
            } else {
                self.iterations - whole
            };
            return self
                .fill
                .holds_after()
                .then(|| self.iteration_progress(whole, frac));
        }
        let whole = overall.floor();
        Some(self.iteration_progress(whole, overall - whole))
    }

    fn iteration_progress(&self, iteration: f64, frac: f64) -> f32 {
        let odd = (iteration as i64) % 2 == 1;
        let reversed = match self.direction {
            Direction::Normal => false,
            Direction::Reverse => true,
            Direction::Alternate => odd,
            Direction::AlternateReverse => !odd,
        };
        self.easing
            .at(if reversed { 1. - frac } else { frac } as f32)
    }
}

/// One keyframe: where it falls, what it sets, and how it moves on to the next.
#[derive(Clone, Debug)]
struct Frame {
    offset: f32,
    declarations: Vec<Declaration>,
    easing: Option<Easing>,
}

/// A `@keyframes` rule: the values a property takes, by where the rule puts them.
#[derive(Clone, Debug, Default)]
pub(crate) struct Keyframes {
    frames: Vec<Frame>,
}

impl Keyframes {
    /// Reads the body of `@keyframes`: `0%, 50% { … }` into offsets and declarations.
    pub(crate) fn parse(body: &str) -> Self {
        let mut frames = Vec::new();
        let mut rest = body;
        while let Some(open) = rest.find('{') {
            let close = matching_brace(rest, open).unwrap_or(rest.len() - 1);
            let declarations = declarations(&rest[open + 1..close]);
            let easing = declarations
                .iter()
                .rfind(|d| d.name == "animation-timing-function")
                .map(|d| Easing::parse(&d.value));
            let offsets: Vec<f32> = split_outside(rest[..open].trim(), ',')
                .iter()
                .filter_map(|s| offset(s.trim()))
                .collect();
            frames.extend(
                offsets
                    .into_iter()
                    .map(|at| (at, declarations.clone(), easing.clone())),
            );
            rest = rest.get(close + 1..).unwrap_or("");
        }
        Self::from_frames(frames)
    }

    /// Frames a script gave — offset, what they set, and the easing they move on with — in the
    /// order their offsets put them.
    pub(crate) fn from_frames(frames: Vec<(f32, Vec<Declaration>, Option<Easing>)>) -> Self {
        let mut frames: Vec<Frame> = frames
            .into_iter()
            .map(|(offset, declarations, easing)| Frame {
                offset,
                declarations: animatable(declarations),
                easing,
            })
            .collect();
        frames.sort_by(|a, b| a.offset.total_cmp(&b.offset));
        Keyframes { frames }
    }

    /// The same keyframes with every `var()` in them read in the style of the element they run
    /// on; a value that cannot be is left out, as an invalid declaration is.
    pub(crate) fn in_context(self: &Arc<Self>, base: &Style) -> Arc<Self> {
        let varied = |d: &Declaration| d.value.contains("var(");
        if !self.frames.iter().flat_map(|f| &f.declarations).any(varied) {
            return self.clone();
        }
        let custom: HashMap<String, String> = base
            .iter()
            .filter(|(name, _)| name.starts_with("--"))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        let mut frames = self.frames.clone();
        for frame in &mut frames {
            for declaration in &mut frame.declarations {
                if varied(declaration) {
                    declaration.value =
                        substitute(&declaration.value, &custom, 0).unwrap_or_default();
                }
            }
            frame.declarations.retain(|d| !d.value.is_empty());
        }
        Arc::new(Keyframes { frames })
    }

    /// The values it gives at `t`, over the `base` style: a property no frame sets at an end
    /// takes the base value there — or its initial value, when the element has none — as CSS
    /// says. Between two frames the first one's easing moves it, `interval` when it has none.
    pub(crate) fn style_at(&self, base: &Style, t: f32, interval: &Easing) -> Style {
        let mut properties: Vec<&str> = Vec::new();
        for declaration in self.frames.iter().flat_map(|f| &f.declarations) {
            if !properties.contains(&declaration.name.as_str()) {
                properties.push(&declaration.name);
            }
        }
        let mut out = Style::new();
        for property in properties {
            let mut stops: Vec<(f32, &str, &Easing)> = self
                .frames
                .iter()
                .filter_map(|frame| {
                    let found = frame.declarations.iter().rfind(|d| d.name == property)?;
                    let easing = frame.easing.as_ref().unwrap_or(interval);
                    Some((frame.offset, found.value.as_str(), easing))
                })
                .collect();
            // Under the frames there is the element's own value, held at both ends.
            if let Some(under) = value_of(base, property) {
                if stops.first().is_none_or(|(offset, ..)| *offset > 0.) {
                    stops.insert(0, (0., under, interval));
                }
                if stops.last().is_none_or(|(offset, ..)| *offset < 1.) {
                    stops.push((1., under, interval));
                }
            }
            out.insert(property.to_owned(), sample(&stops, t));
        }
        out
    }
}

/// `property` of `style`, or its initial value when the style says nothing of it: a style holds
/// what the cascade set, and a property no rule mentions still has a value to move from or to.
fn value_of<'a>(style: &'a Style, property: &str) -> Option<&'a str> {
    style
        .get(property)
        .map(String::as_str)
        .or_else(|| initial_value(property, None))
}

/// The declarations of a keyframe that animate: shorthands opened into the longhands that move,
/// and not what is no property of the element's (`animation-timing-function` is the frame's
/// easing).
fn animatable(declarations: Vec<Declaration>) -> Vec<Declaration> {
    let mut out = Vec::new();
    for declaration in declarations {
        if declaration.name.starts_with("animation") || declaration.name.starts_with("transition") {
            continue;
        }
        let Some(expand) = expansion(&declaration.name) else {
            out.push(declaration);
            continue;
        };
        let mut opened = Style::new();
        expand(&mut opened, &declaration.name, &declaration.value);
        opened.remove(&declaration.name);
        let mut named: Vec<_> = opened.into_iter().collect();
        named.sort();
        out.extend(named.into_iter().map(|(name, value)| Declaration {
            name,
            value,
            important: false,
        }));
    }
    out
}

fn offset(text: &str) -> Option<f32> {
    match text {
        "from" => Some(0.),
        "to" => Some(1.),
        _ => text
            .strip_suffix('%')?
            .trim()
            .parse::<f32>()
            .ok()
            .map(|p| p / 100.),
    }
}

fn sample(stops: &[(f32, &str, &Easing)], t: f32) -> String {
    if t <= 0. {
        return stops[0].1.to_owned();
    }
    for pair in stops.windows(2) {
        let ((from_offset, from, easing), (to_offset, to, _)) = (pair[0], pair[1]);
        if t <= to_offset {
            let span = to_offset - from_offset;
            let along = if span <= f32::EPSILON {
                1.
            } else {
                easing.at((t - from_offset) / span)
            };
            return interpolate(from, to, along);
        }
    }
    stops.last().map_or_else(String::new, |s| s.1.to_owned())
}

/// What an animation drives.
enum Drives {
    Transition {
        property: String,
        from: String,
        to: String,
    },
    Animation {
        name: String,
        keyframes: Arc<Keyframes>,
        /// How it moves from one keyframe to the next, when the keyframe says nothing.
        interval: Easing,
    },
    Script {
        keyframes: Arc<Keyframes>,
    },
}

/// One transition or animation, running or kept after it ended.
pub(crate) struct Running {
    pub(crate) id: u32,
    pub(crate) node: NodeId,
    drives: Drives,
    pub(crate) timing: Timing,
    /// The time on the timeline `offset` was the animation's own; `None` while it waits for the
    /// next frame to say, or while it is held.
    start: Option<f64>,
    /// How far into its effect it was at `start`, or where it waits to begin.
    offset: f64,
    /// Where a paused or finished animation stands.
    hold: Option<f64>,
    /// How fast it goes: `-1` for one played in reverse.
    pub(crate) rate: f64,
    pub(crate) state: PlayState,
    /// The element's own style, for the parts a keyframe leaves out.
    base: Arc<Style>,
    /// Whether it has ever been given a place on the timeline.
    begun: bool,
    /// Whether its start has been announced.
    announced: bool,
    /// The iteration last announced, for `animationiteration`.
    iteration: f64,
}

impl Running {
    /// `propertyName` for a transition, `animationName` for an animation.
    pub(crate) fn name(&self) -> &str {
        match &self.drives {
            Drives::Transition { property, .. } => property,
            Drives::Animation { name, .. } => name,
            Drives::Script { .. } => "",
        }
    }

    /// Whether a script made it, with `element.animate()`.
    pub(crate) fn is_script(&self) -> bool {
        matches!(self.drives, Drives::Script { .. })
    }

    /// Whether it is a CSS transition.
    pub(crate) fn is_transition(&self) -> bool {
        matches!(self.drives, Drives::Transition { .. })
    }

    /// Milliseconds into its effect at `now`: where it stands, a held one where it is held.
    pub(crate) fn time(&self, now: f64) -> f64 {
        match (self.hold, self.start) {
            (Some(held), _) => held,
            (None, Some(start)) => self.offset + (now - start) * self.rate,
            (None, None) => self.offset,
        }
    }

    /// The time it has been active at `time`, the delay and the end left out: what the events
    /// call `elapsedTime`.
    fn active_at(&self, time: f64) -> f64 {
        let active = self.timing.active();
        (time - self.timing.delay).clamp(0., if active.is_finite() { active } else { f64::MAX })
    }

    /// How long it had been active when it began, if its delay was negative.
    fn head_start(&self) -> f64 {
        let active = self.timing.active();
        (-self.timing.delay).clamp(0., if active.is_finite() { active } else { f64::MAX })
    }

    /// Whether it has got to the end it plays towards.
    fn is_over(&self, now: f64) -> bool {
        let time = self.time(now);
        if self.rate < 0. {
            time <= 0.
        } else {
            self.rate > 0. && self.timing.iterations.is_finite() && time >= self.timing.end()
        }
    }

    fn iteration_at(&self, now: f64) -> f64 {
        let active = self.time(now) - self.timing.delay;
        if active <= 0. || self.timing.duration <= 0. {
            return 0.;
        }
        (active / self.timing.duration).floor()
    }

    /// The properties it sets at `now`.
    fn values(&self, now: f64) -> Option<Style> {
        if self.state == PlayState::Idle {
            return None;
        }
        let progress = self.timing.progress(self.time(now))?;
        match &self.drives {
            Drives::Transition { property, from, to } => {
                let mut style = Style::new();
                style.insert(property.clone(), interpolate(from, to, progress));
                Some(style)
            }
            Drives::Animation {
                keyframes,
                interval,
                ..
            } => Some(keyframes.style_at(&self.base, progress, interval)),
            Drives::Script { keyframes } => {
                Some(keyframes.style_at(&self.base, progress, &Easing::Linear))
            }
        }
    }

    /// Where it is in the order the cascade gives animations: transitions under CSS animations
    /// under a script's.
    fn layer(&self) -> u8 {
        match self.drives {
            Drives::Transition { .. } => 0,
            Drives::Animation { .. } => 1,
            Drives::Script { .. } => 2,
        }
    }

    /// An event of its own at `now`, `elapsed` ms into its active time.
    fn event(&self, kind: EventKind, elapsed: f64, now: f64) -> Event {
        Event {
            id: self.id,
            node: self.node,
            kind,
            name: self.name().to_owned(),
            elapsed,
            time: self.time(now),
            script: self.is_script(),
            silent: false,
        }
    }

    /// The kind of event that tells it ended.
    fn ending(&self) -> EventKind {
        match self.drives {
            Drives::Transition { .. } => EventKind::TransitionEnd,
            Drives::Animation { .. } => EventKind::AnimationEnd,
            Drives::Script { .. } => EventKind::Finish,
        }
    }

    /// The kind of event that tells it was cancelled.
    fn cancelled(&self) -> EventKind {
        match self.drives {
            Drives::Transition { .. } => EventKind::TransitionCancel,
            Drives::Animation { .. } => EventKind::AnimationCancel,
            Drives::Script { .. } => EventKind::Cancel,
        }
    }
}

/// Something an animation has to tell the document about.
pub(crate) struct Event {
    /// The animation it is about, so its object can hear of it and its `finished` settle.
    pub(crate) id: u32,
    pub(crate) node: NodeId,
    pub(crate) kind: EventKind,
    /// `propertyName` for a transition, `animationName` for an animation.
    pub(crate) name: String,
    /// `elapsedTime`, in milliseconds: how long the animation had been active, which leaves its
    /// delay out.
    pub(crate) elapsed: f64,
    /// Where the animation stood, in milliseconds: its `currentTime`.
    pub(crate) time: f64,
    /// Whether it is a script's animation, whose events are the `Animation` object's own.
    pub(crate) script: bool,
    /// Whether nobody is to be told but whoever waits on `finished`: its element is gone.
    pub(crate) silent: bool,
}

/// What happened to an animation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EventKind {
    TransitionRun,
    TransitionStart,
    TransitionEnd,
    TransitionCancel,
    AnimationStart,
    AnimationIteration,
    AnimationEnd,
    AnimationCancel,
    /// A script animation finished: `finish` on its object.
    Finish,
    /// A script animation was cancelled: `cancel` on its object.
    Cancel,
}

impl EventKind {
    /// Whether it is a `TransitionEvent`.
    pub(crate) fn is_transition(self) -> bool {
        matches!(
            self,
            Self::TransitionRun
                | Self::TransitionStart
                | Self::TransitionEnd
                | Self::TransitionCancel
        )
    }

    /// The class of the event object: `TransitionEvent`, `AnimationEvent`, or a plain `Event`.
    pub(crate) fn class(self) -> &'static str {
        match self {
            _ if self.is_transition() => "TransitionEvent",
            Self::Finish | Self::Cancel => "Event",
            _ => "AnimationEvent",
        }
    }

    /// The event's `type`.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::TransitionRun => "transitionrun",
            Self::TransitionStart => "transitionstart",
            Self::TransitionEnd => "transitionend",
            Self::TransitionCancel => "transitioncancel",
            Self::AnimationStart => "animationstart",
            Self::AnimationIteration => "animationiteration",
            Self::AnimationEnd => "animationend",
            Self::AnimationCancel => "animationcancel",
            Self::Finish => "finish",
            Self::Cancel => "cancel",
        }
    }

    /// How it settles the `finished` promise of its animation; `None` leaves it be.
    pub(crate) fn settles(self) -> Option<Outcome> {
        match self {
            Self::TransitionEnd | Self::AnimationEnd | Self::Finish => Some(Outcome::Finished),
            Self::TransitionCancel | Self::AnimationCancel | Self::Cancel => {
                Some(Outcome::Cancelled)
            }
            _ => None,
        }
    }
}

/// How the `finished` promise of an animation was settled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Resolved: it ran to its end.
    Finished,
    /// Rejected with an `AbortError`: it was cancelled.
    Cancelled,
}

/// Every transition and animation the document runs, and the events the last frame queued.
#[derive(Default)]
pub(crate) struct AnimationStore {
    running: Vec<Running>,
    events: Vec<Event>,
    next_id: u32,
    /// The time of the last frame.
    now: f64,
    /// How many nodes a restyle has looked at for animations, over the store's life.
    examined: usize,
}

impl AnimationStore {
    /// Takes the events queued since they were last taken.
    pub(crate) fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    /// Whether any event waits to be taken.
    pub(crate) fn has_events(&self) -> bool {
        !self.events.is_empty()
    }

    /// Whether the store holds nothing at all.
    pub(crate) fn is_empty(&self) -> bool {
        self.running.is_empty()
    }

    /// How many nodes restyles have looked at: only the ones they restyled.
    #[cfg(test)]
    pub(crate) fn examined(&self) -> usize {
        self.examined
    }

    /// The animation `id`, if the store still holds it.
    pub(crate) fn find(&self, id: u32) -> Option<&Running> {
        self.running.iter().find(|r| r.id == id)
    }

    fn find_mut(&mut self, id: u32) -> Option<&mut Running> {
        self.running.iter_mut().find(|r| r.id == id)
    }

    /// The animations on `node`, or on any, that have an effect now or may have again.
    pub(crate) fn of_node(&self, node: Option<NodeId>) -> Vec<u32> {
        self.running
            .iter()
            .filter(|r| {
                node.is_none_or(|node| r.node == node)
                    && (matches!(r.state, PlayState::Running | PlayState::Paused)
                        || r.values(self.now).is_some())
            })
            .map(|r| r.id)
            .collect()
    }

    /// Whether something is still moving that is worth a frame: on a node `visible` says is
    /// drawn, or a script's animation that ends and so must be let to. A spinner that left the
    /// document, or is under `display: none`, does not keep frames coming.
    pub(crate) fn is_animating(&self, visible: impl Fn(NodeId) -> bool) -> bool {
        self.running.iter().any(|r| {
            r.state == PlayState::Running
                && r.rate != 0.
                && (visible(r.node) || (r.is_script() && r.timing.iterations.is_finite()))
        })
    }

    fn push(&mut self, node: NodeId, drives: Drives, timing: Timing, base: Arc<Style>) -> u32 {
        self.next_id += 1;
        self.running.push(Running {
            id: self.next_id,
            node,
            drives,
            timing,
            start: None,
            offset: 0.,
            hold: None,
            rate: 1.,
            state: PlayState::Running,
            base,
            begun: false,
            announced: false,
            iteration: 0.,
        });
        self.next_id
    }

    /// A script's `element.animate()`.
    pub(crate) fn start_script(
        &mut self,
        node: NodeId,
        keyframes: Arc<Keyframes>,
        base: Arc<Style>,
        timing: Timing,
    ) -> u32 {
        let keyframes = keyframes.in_context(&base);
        self.push(node, Drives::Script { keyframes }, timing, base)
    }

    fn start_transition(
        &mut self,
        node: NodeId,
        property: &str,
        ends: (&str, &str),
        timing: Timing,
    ) {
        let drives = Drives::Transition {
            property: property.to_owned(),
            from: ends.0.to_owned(),
            to: ends.1.to_owned(),
        };
        self.push(node, drives, timing, Arc::default());
    }

    /// Stops `id` where it is: what `cancel()` and a style that no longer asks for it do. A
    /// script's animation stays, idle, to be played again; any other goes.
    pub(crate) fn cancel(&mut self, id: u32) {
        let now = self.now;
        let Some(at) = self.running.iter().position(|r| r.id == id) else {
            return;
        };
        let r = &mut self.running[at];
        if r.state == PlayState::Idle {
            return;
        }
        let event = r.event(r.cancelled(), r.active_at(r.time(now)), now);
        if r.is_script() {
            (r.state, r.hold, r.start, r.offset) = (PlayState::Idle, None, None, 0.);
            r.announced = false;
        } else {
            self.running.remove(at);
        }
        self.events.push(event);
    }

    /// Forgets the animations of nodes that are gone. No one is told, bar whoever waits on
    /// `finished`, who hears that it was cancelled.
    pub(crate) fn forget(&mut self, ids: &[NodeId]) {
        let now = self.now;
        let (gone, kept): (Vec<Running>, Vec<Running>) = std::mem::take(&mut self.running)
            .into_iter()
            .partition(|r| ids.contains(&r.node));
        self.running = kept;
        self.events.extend(gone.iter().map(|r| Event {
            silent: true,
            ..r.event(r.cancelled(), 0., now)
        }));
    }

    /// Lets go of the script animations that are over, or cancelled, and nothing holds the
    /// object of any more.
    pub(crate) fn release_finished(&mut self, held: impl Fn(u32) -> bool) {
        self.running.retain(|r| {
            // A finished one whose fill holds the value is part of the style until it is
            // replaced or cancelled, whether or not anything holds its object.
            let holds = r.state == PlayState::Finished
                && matches!(r.timing.fill, Fill::Forwards | Fill::Both);
            let gone =
                matches!(r.state, PlayState::Finished | PlayState::Idle) && !holds && !held(r.id);
            !(r.is_script() && gone)
        });
    }

    /// Pauses `id` where it stands.
    pub(crate) fn pause(&mut self, id: u32) {
        let now = self.now;
        if let Some(r) = self.find_mut(id).filter(|r| r.state == PlayState::Running) {
            (r.hold, r.start, r.state) = (Some(r.time(now)), None, PlayState::Paused);
        }
    }

    /// Plays `id` on from where it stands, from the start of what it plays towards when it had
    /// finished or was cancelled. It takes its place on the timeline at the next frame.
    pub(crate) fn play(&mut self, id: u32) {
        let now = self.now;
        let Some(r) = self.find_mut(id) else { return };
        let end = if r.timing.end().is_finite() {
            r.timing.end()
        } else {
            0.
        };
        let at = match r.state {
            PlayState::Finished | PlayState::Idle => {
                r.announced = false;
                if r.rate < 0. { end } else { 0. }
            }
            _ => r.time(now),
        };
        (r.hold, r.start, r.offset, r.state) = (None, None, at, PlayState::Running);
    }

    /// Changes how fast `id` goes from where it stands, at the next frame: `updatePlaybackRate()`
    /// and `reverse()`.
    pub(crate) fn update_rate(&mut self, id: u32, rate: f64) {
        let now = self.now;
        if let Some(r) = self.find_mut(id) {
            if r.hold.is_none() && r.state == PlayState::Running {
                (r.offset, r.start) = (r.time(now), None);
            }
            r.rate = rate;
        }
    }

    /// Changes how fast `id` goes, from where it stands now: the `playbackRate` setter.
    pub(crate) fn set_rate(&mut self, id: u32, rate: f64) {
        let now = self.now;
        if let Some(r) = self.find_mut(id) {
            if r.hold.is_none() && r.state == PlayState::Running {
                // Anchored where the timeline is now, so that the time since the last frame
                // goes on at the new rate.
                (r.offset, r.start) = (r.time(now), Some(now));
            }
            r.rate = rate;
        }
    }

    /// Moves `id` to `time` milliseconds into its effect: a running one goes on from there at
    /// the next frame, any other waits there, paused.
    pub(crate) fn seek(&mut self, id: u32, time: f64) {
        let Some(r) = self.find_mut(id) else { return };
        if r.state == PlayState::Running {
            (r.offset, r.start, r.hold) = (time, None, None);
        } else {
            (r.hold, r.state) = (Some(time), PlayState::Paused);
        }
        r.iteration = if r.timing.duration > 0. {
            ((time - r.timing.delay) / r.timing.duration)
                .floor()
                .max(0.)
        } else {
            0.
        };
    }

    /// Takes `id` to the end it plays towards, now: `finish()`, for it alone. `None` for one
    /// that has no end, which cannot be finished (or is not there): `false`.
    pub(crate) fn finish(&mut self, id: u32) -> bool {
        let Some(r) = self.find(id) else { return false };
        if r.rate >= 0. && !r.timing.end().is_finite() {
            return false;
        }
        if r.state != PlayState::Finished {
            self.end(id);
        }
        true
    }

    /// Advances everything to `now`: gives what has no place on the timeline its place, queues
    /// the events this frame crosses, and lets go of what finished. What a style made on a node
    /// `connected` does not know is dropped without a word.
    pub(crate) fn advance(&mut self, now: f64, connected: impl Fn(NodeId) -> bool) {
        self.now = now;
        // A script's animation goes on in a tree of its own; a style's does not outlive its
        // element's place in the document.
        let (kept, gone): (Vec<Running>, Vec<Running>) = std::mem::take(&mut self.running)
            .into_iter()
            .partition(|r| r.is_script() || connected(r.node));
        self.running = kept;
        self.events.extend(gone.iter().map(|r| Event {
            silent: true,
            ..r.event(r.cancelled(), 0., now)
        }));
        let mut ended = Vec::new();
        for r in &mut self.running {
            if r.state != PlayState::Running {
                continue;
            }
            if r.start.is_none() {
                r.start = Some(now);
                if !std::mem::replace(&mut r.begun, true) && r.is_transition() {
                    self.events
                        .push(r.event(EventKind::TransitionRun, r.head_start(), now));
                }
            }
            if !r.announced && r.time(now) >= r.timing.delay {
                r.announced = true;
                let kind = if r.is_transition() {
                    EventKind::TransitionStart
                } else {
                    EventKind::AnimationStart
                };
                if !r.is_script() {
                    self.events.push(r.event(kind, r.head_start(), now));
                }
            }
            if r.is_over(now) {
                ended.push(r.id);
            } else if !r.is_transition() && !r.is_script() {
                let at = r.iteration_at(now);
                while r.iteration < at {
                    r.iteration += 1.;
                    let elapsed = r.iteration * r.timing.duration;
                    self.events
                        .push(r.event(EventKind::AnimationIteration, elapsed, now));
                }
            }
        }
        for id in ended {
            self.end(id);
        }
    }

    /// `id` got to its end: its event, and out of the store unless something keeps its effect —
    /// a script's animation, a fill that holds the last frame.
    fn end(&mut self, id: u32) {
        let now = self.now;
        let Some(at) = self.running.iter().position(|r| r.id == id) else {
            return;
        };
        let r = &mut self.running[at];
        let end = if r.rate < 0. { 0. } else { r.timing.end() };
        let elapsed = if r.rate < 0. { 0. } else { r.timing.active() };
        let mut event = r.event(r.ending(), elapsed, now);
        event.time = end;
        let keeps = r.is_script() || (!r.is_transition() && r.timing.fill.holds_after());
        if keeps {
            (r.hold, r.start, r.state) = (Some(end), None, PlayState::Finished);
        } else {
            self.running.remove(at);
        }
        self.events.push(event);
    }

    /// The style of every node an animation drives at `now`, over what `base` gives it.
    pub(crate) fn styles(
        &self,
        now: f64,
        base: impl Fn(NodeId) -> Option<Arc<Style>>,
    ) -> HashMap<NodeId, Arc<Style>> {
        let mut nodes: Vec<NodeId> = self.running.iter().map(|r| r.node).collect();
        nodes.sort_unstable();
        nodes.dedup();
        let mut out = HashMap::new();
        for node in nodes {
            let Some(base) = base(node) else { continue };
            if let Some(animated) = self.animated(node, now) {
                let mut style = (*base).clone();
                style.extend(animated);
                out.insert(node, Arc::new(style));
            }
        }
        out
    }

    /// What the animations on `node` make of its properties at `now`, and only those; `None`
    /// when none of them applies anything.
    pub(crate) fn animated(&self, node: NodeId, now: f64) -> Option<Style> {
        let mut running: Vec<&Running> = self.running.iter().filter(|r| r.node == node).collect();
        running.sort_by_key(|r| r.layer());
        let mut style: Option<Style> = None;
        for values in running.iter().filter_map(|r| r.values(now)) {
            style.get_or_insert_with(Style::new).extend(values);
        }
        style
    }

    /// Starts and stops the transitions and animations the cascade now asks for of `nodes`,
    /// the ones it restyled, given what it said before. A property that changed value and is
    /// covered by `transition-property` moves from where it was to where it is now — from the
    /// value it shows, if a transition was under way — and an `animation-name` that is new
    /// begins; one that is gone, or a node that no longer asks, cancels.
    pub(crate) fn restart(
        &mut self,
        styler: &Styler,
        (before, after): (&Computed, &Computed),
        nodes: &[NodeId],
    ) {
        for node in nodes {
            self.examined += 1;
            let Some(base) = after.cascaded(*node) else {
                continue;
            };
            self.restart_transitions(*node, &base, before.cascaded(*node).as_deref());
            self.restart_animations(styler, *node, &base);
        }
    }

    fn restart_transitions(&mut self, node: NodeId, base: &Style, previous: Option<&Style>) {
        let wanted = transitions(base);
        let covered = |property: &str| wanted.iter().any(|(p, _)| p == property || p == "all");
        let stale: Vec<u32> = self
            .running
            .iter()
            .filter(|r| r.node == node && r.is_transition() && !covered(r.name()))
            .map(|r| r.id)
            .collect();
        for id in stale {
            self.cancel(id);
        }
        // A node new to the document does not transition into it.
        let Some(previous) = previous else { return };
        // `all` moves whatever changed; a list moves the properties it names.
        let mut properties: Vec<&str> = wanted
            .iter()
            .map(|(p, _)| p.as_str())
            .filter(|p| *p != "all")
            .collect();
        if wanted.iter().any(|(p, _)| p == "all") {
            let changed = |p: &&String| !p.starts_with("--") && previous.get(*p) != base.get(*p);
            properties.extend(
                base.keys()
                    .chain(previous.keys())
                    .filter(changed)
                    .map(String::as_str),
            );
        }
        properties.sort_unstable();
        properties.dedup();
        for property in properties {
            // The last entry that names it (or `all`) is the one that applies.
            let Some((_, timing)) = wanted.iter().rfind(|(p, _)| p == property || p == "all")
            else {
                continue;
            };
            let ends = (value_of(previous, property), value_of(base, property));
            if let (Some(from), Some(to)) = ends {
                self.restart_transition(node, property, (from, to), timing);
            }
        }
    }

    /// The transition of `property` on `node`, its place in the store, where it is heading and
    /// the value it shows now.
    fn transition_of(&self, node: NodeId, property: &str) -> Option<(u32, String, Option<String>)> {
        self.running.iter().find_map(|r| match &r.drives {
            Drives::Transition {
                property: p, to, ..
            } if r.node == node && p == property => Some((
                r.id,
                to.clone(),
                r.values(self.now).and_then(|s| s.get(property).cloned()),
            )),
            _ => None,
        })
    }

    fn restart_transition(
        &mut self,
        node: NodeId,
        property: &str,
        (from, to): (&str, &str),
        timing: &Timing,
    ) {
        let going = self.transition_of(node, property);
        // A duration of nothing is no transition, and ends the one that was under way.
        if timing.duration <= 0. {
            if let Some((id, ..)) = going {
                self.cancel(id);
            }
            return;
        }
        let from = match going {
            Some((_, heading, _)) if heading == to => return,
            // Interrupted: the new one starts from the value the old one shows.
            Some((id, _, shown)) => {
                self.cancel(id);
                shown.unwrap_or_else(|| from.to_owned())
            }
            None => from.to_owned(),
        };
        if can_interpolate(&from, to) {
            self.start_transition(node, property, (&from, to), timing.clone());
        }
    }

    fn restart_animations(&mut self, styler: &Styler, node: NodeId, base: &Arc<Style>) {
        let wanted = animations(base);
        let gone: Vec<u32> = self
            .running
            .iter()
            .filter(|r| {
                r.node == node
                    && matches!(&r.drives, Drives::Animation { name, .. } if !wanted.iter().any(|w| w.name == *name))
            })
            .map(|r| r.id)
            .collect();
        for id in gone {
            self.cancel(id);
        }
        for CssAnimation {
            name,
            timing,
            interval,
            paused,
        } in wanted
        {
            let running = self.running.iter().find(|r| {
                r.node == node
                    && matches!(&r.drives, Drives::Animation { name: n, .. } if *n == name)
            });
            match running.map(|r| (r.id, r.state)) {
                Some((id, PlayState::Running)) if paused => self.pause(id),
                Some((id, PlayState::Paused)) if !paused => self.play(id),
                Some(_) => {}
                None => {
                    let Some(keyframes) = styler.keyframes(&name) else {
                        continue;
                    };
                    let keyframes = keyframes.in_context(base);
                    let drives = Drives::Animation {
                        name,
                        keyframes,
                        interval,
                    };
                    let id = self.push(node, drives, timing, base.clone());
                    if paused {
                        self.pause(id);
                    }
                }
            }
        }
    }

    /// Writes what `id` shows now into the style of its element: `commitStyles()`.
    pub(crate) fn committed(&self, id: u32) -> Option<(NodeId, Style)> {
        let r = self.find(id)?;
        Some((r.node, r.values(self.now).unwrap_or_default()))
    }
}

/// The items of a comma-separated list a property holds.
fn listed(style: &Style, property: &str) -> Vec<String> {
    style.get(property).map_or_else(Vec::new, |value| {
        split_outside(value, ',')
            .into_iter()
            .map(|part| part.trim().to_owned())
            .collect()
    })
}

/// The item of a list at `index`, the list repeating to fill a longer one as CSS says.
fn at(list: &[String], index: usize) -> Option<&str> {
    (!list.is_empty()).then(|| list[index % list.len()].as_str())
}

/// The timing the `index`th entry of the lists `prefix`-duration, -delay and -timing-function
/// give; a timing function nothing says is `ease`.
fn timing_at(style: &Style, prefix: &str, index: usize) -> Timing {
    let entries = |suffix: &str| listed(style, &format!("{prefix}-{suffix}"));
    let (durations, delays, functions) = (
        entries("duration"),
        entries("delay"),
        entries("timing-function"),
    );
    Timing {
        duration: at(&durations, index).and_then(milliseconds).unwrap_or(0.),
        delay: at(&delays, index).and_then(milliseconds).unwrap_or(0.),
        easing: Easing::parse(at(&functions, index).unwrap_or("ease")),
        ..Timing::default()
    }
}

/// The transitions a node's style asks for: each property (a shorthand opened into its
/// longhands, or `all`, which is what a style that names none means) and when it runs. A
/// transition shows its starting value through its delay.
pub(crate) fn transitions(style: &Style) -> Vec<(String, Timing)> {
    let mut properties = listed(style, "transition-property");
    if properties.is_empty() {
        properties.push("all".to_owned());
    }
    let mut out = Vec::new();
    for (index, property) in properties.iter().enumerate() {
        if property == "none" {
            continue;
        }
        let timing = Timing {
            fill: Fill::Backwards,
            ..timing_at(style, "transition", index)
        };
        out.extend(
            longhands(property)
                .into_iter()
                .map(|name| (name, timing.clone())),
        );
    }
    out
}

/// One animation a node's style asks for.
pub(crate) struct CssAnimation {
    /// The `@keyframes` it runs.
    pub(crate) name: String,
    pub(crate) timing: Timing,
    /// How it moves from one keyframe to the next: `animation-timing-function`, which is not
    /// how the whole iteration moves.
    pub(crate) interval: Easing,
    pub(crate) paused: bool,
}

/// The animations a node's style asks for.
pub(crate) fn animations(style: &Style) -> Vec<CssAnimation> {
    let (counts, directions, fills, states) = (
        listed(style, "animation-iteration-count"),
        listed(style, "animation-direction"),
        listed(style, "animation-fill-mode"),
        listed(style, "animation-play-state"),
    );
    listed(style, "animation-name")
        .into_iter()
        .enumerate()
        .filter(|(_, name)| name != "none")
        .map(|(index, name)| {
            let iterations = at(&counts, index).map_or(1., |count| match count {
                "infinite" => f64::INFINITY,
                count => count.parse().unwrap_or(1.),
            });
            let mut timing = Timing {
                iterations,
                direction: at(&directions, index).map_or(Direction::Normal, Direction::from_name),
                fill: at(&fills, index).map_or(Fill::None, Fill::from_name),
                ..timing_at(style, "animation", index)
            };
            let interval = std::mem::replace(&mut timing.easing, Easing::Linear);
            CssAnimation {
                name,
                timing,
                interval,
                paused: at(&states, index) == Some("paused"),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyframes_take_the_base_value_at_both_ends() {
        let keyframes = Keyframes::parse("50% { opacity: 0.5 }");
        let mut base = Style::new();
        base.insert("opacity".into(), "1".into());
        let linear = Easing::Linear;
        assert_eq!(keyframes.style_at(&base, 0., &linear)["opacity"], "1");
        assert_eq!(keyframes.style_at(&base, 0.5, &linear)["opacity"], "0.5");
        assert_eq!(keyframes.style_at(&base, 1., &linear)["opacity"], "1");
        // A property the element has no value for starts from its initial one.
        let keyframes = Keyframes::parse("to { transform: rotate(360deg) }");
        let half = keyframes.style_at(&Style::new(), 0.5, &linear);
        assert_eq!(half["transform"], "rotate(180deg)");
    }

    #[test]
    fn keyframes_open_shorthands_and_move_on_by_their_own_easing() {
        let keyframes = Keyframes::parse(
            "from { background: red; animation-timing-function: steps(2, end) } to { background: blue }",
        );
        let mut base = Style::new();
        base.insert("background-color".into(), "red".into());
        let style = keyframes.style_at(&base, 0.4, &Easing::Linear);
        // The first frame's `steps(2)` holds red until the half, and the shorthand is gone.
        assert_eq!(style["background-color"], "red");
        assert!(
            !style.contains_key("animation-timing-function") && !style.contains_key("background")
        );
    }

    #[test]
    fn fractional_iterations_end_part_way_and_a_delay_obeys_the_direction() {
        let timing = Timing {
            duration: 100.,
            iterations: 2.5,
            fill: Fill::Forwards,
            ..Timing::default()
        };
        assert_eq!(timing.progress(250.), Some(0.5));
        let backwards = Timing {
            duration: 100.,
            delay: 50.,
            direction: Direction::Reverse,
            fill: Fill::Backwards,
            ..Timing::default()
        };
        assert_eq!(backwards.progress(10.), Some(1.));
    }
}
