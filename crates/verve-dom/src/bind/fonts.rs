//! `document.fonts` (`FontFaceSet`) and `FontFace`: the fonts a page adds, their loading state
//! (`unloaded`, `loading`, `loaded`, `error`), `check()` and `load()` by font shorthand, and the
//! `loading`, `loadingdone` and `loadingerror` events.
//!
//! A face lives in Rust (its descriptors, status and the promise `loaded` settles); the object
//! a script holds only reads and writes it. Bytes given to `new FontFace(family, buffer)` are
//! loaded at once. A face given an address asks the host to fetch it when `load()` is called
//! ([`crate::Request::LoadFont`]) and waits in `loading` until the host answers with
//! [`crate::Document::settle_font`].

use std::collections::HashMap;

use boa_engine::{
    Context, Finalize, JsData, JsObject, JsResult, JsSymbol, JsValue, Trace, js_string,
    object::builtins::{JsFunction, JsPromise},
};
use boa_gc::GcRefCell;

use super::{
    Accessor, ClassSpec, Method, arg, array, call_method, class, constructor_of, dom_error,
    function, get, hidden, illegal_constructor, prototype_of_target, resolved, string, text,
    type_error,
};
use crate::state::shared;
use crate::{FontFaceId, FontOutcome, Request};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FaceStatus {
    Unloaded,
    Loading,
    Loaded,
    Error,
}

impl FaceStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Unloaded => "unloaded",
            Self::Loading => "loading",
            Self::Loaded => "loaded",
            Self::Error => "error",
        }
    }
}

/// Where a face's font comes from.
#[derive(Clone, Debug)]
enum Source {
    /// The address of the first `url()` of the source string.
    Url(String),
    /// Bytes the script gave.
    Bytes,
}

/// The descriptors of a `FontFace`, in the order `Face::descriptors` keeps them, each with the
/// value it has unless given.
const DESCRIPTORS: [(&str, &str); 13] = [
    ("family", ""),
    ("style", "normal"),
    ("weight", "normal"),
    ("stretch", "normal"),
    ("unicodeRange", "U+0-10FFFF"),
    ("variant", "normal"),
    ("featureSettings", "normal"),
    ("variationSettings", "normal"),
    ("display", "auto"),
    ("ascentOverride", "normal"),
    ("descentOverride", "normal"),
    ("lineGapOverride", "normal"),
    ("sizeAdjust", "100%"),
];

#[derive(Trace, Finalize)]
struct Face {
    #[unsafe_ignore_trace]
    id: FontFaceId,
    #[unsafe_ignore_trace]
    descriptors: Vec<String>,
    #[unsafe_ignore_trace]
    status: FaceStatus,
    #[unsafe_ignore_trace]
    source: Source,
    /// The promise of `loaded`, and what settles it.
    loaded: JsValue,
    resolve: JsFunction,
    reject: JsFunction,
}

#[derive(Trace, Finalize, JsData)]
struct FaceData(GcRefCell<Face>);

/// What `ready` is: a promise already resolved, or one waiting for the fonts loading now.
enum Ready {
    Settled(JsValue),
    Pending { promise: JsValue, resolve: JsValue },
}

impl Default for Ready {
    fn default() -> Self {
        Self::Settled(JsValue::undefined())
    }
}

/// What `document.fonts` holds, kept in the state.
#[derive(Default)]
pub struct Fonts {
    set: Option<JsObject>,
    faces: Vec<JsObject>,
    /// Whether the set said it was loading last: `loading` and `loadingdone` follow its changes.
    loading: bool,
    ready: Ready,
    /// The faces the host was asked to fetch.
    pending: HashMap<FontFaceId, JsObject>,
}

/// Runs `f` on the face `value` is the object of.
fn with_face<R>(value: &JsValue, f: impl FnOnce(&mut Face) -> R) -> JsResult<R> {
    let object = value.as_object();
    match object.as_ref().and_then(|o| o.downcast_ref::<FaceData>()) {
        Some(data) => Ok(f(&mut data.0.borrow_mut())),
        None => Err(type_error("Illegal invocation")),
    }
}

/// The address of the first `url()` of a `src` string.
fn first_url(source: &str) -> String {
    let rest = source.split_once("url(").map_or("", |(_, rest)| rest);
    let inside = rest.split_once(')').map_or(rest, |(inside, _)| inside);
    inside.trim().trim_matches(['"', '\'']).to_owned()
}

fn construct_face(
    new_target: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let mut descriptors: Vec<String> = DESCRIPTORS.iter().map(|(_, d)| (*d).to_owned()).collect();
    descriptors[0] = string(&arg(args, 0), context)?;
    let given = arg(args, 2);
    if given.is_object() {
        for (at, (name, _)) in DESCRIPTORS.iter().enumerate().skip(1) {
            let value = get(&given, name, context)?;
            if !value.is_undefined() {
                descriptors[at] = string(&value, context)?;
            }
        }
    }
    let source = match arg(args, 1).as_string() {
        Some(source) => Source::Url(first_url(&source.to_std_string_escaped())),
        None => Source::Bytes,
    };
    let (promise, resolvers) = JsPromise::new_pending(context);
    let id = FontFaceId(shared(context).borrow_mut().id());
    let prototype = prototype_of_target(new_target, context)?;
    let bytes = matches!(source, Source::Bytes);
    let face = Face {
        id,
        descriptors,
        status: FaceStatus::Unloaded,
        source,
        loaded: promise.into(),
        resolve: resolvers.resolve,
        reject: resolvers.reject,
    };
    let object = JsObject::from_proto_and_data(prototype, FaceData(GcRefCell::new(face)));
    if bytes {
        settle(context, &object, FontOutcome::Loaded)?;
    }
    Ok(object.into())
}

/// A face's loading ended: its `status` and `loaded` follow, and so does the set's.
fn settle(context: &mut Context, face: &JsObject, outcome: FontOutcome) -> JsResult<()> {
    let ok = outcome == FontOutcome::Loaded;
    let hook = with_face(&face.clone().into(), |f| {
        f.status = if ok {
            FaceStatus::Loaded
        } else {
            FaceStatus::Error
        };
        if ok {
            f.resolve.clone()
        } else {
            f.reject.clone()
        }
    })?;
    let result: JsValue = if ok {
        face.clone().into()
    } else {
        dom_error(context, "A network error occurred.", "NetworkError").into_opaque(context)?
    };
    hook.call(&JsValue::undefined(), &[result], context)?;
    refresh(context, !ok)
}

/// The host fetched the face it was asked for: see [`crate::Document::settle_font`].
pub fn settle_face(context: &mut Context, id: FontFaceId, outcome: FontOutcome) -> JsResult<()> {
    let face = shared(context).borrow_mut().fonts.pending.remove(&id);
    match face {
        Some(face) => settle(context, &face, outcome),
        None => Ok(()),
    }
}

/// `face.load()`: a face with an address asks the host for it, one already loading or loaded
/// answers as it is.
fn load_face(context: &mut Context, face: &JsObject) -> JsResult<JsValue> {
    let value = face.clone().into();
    let (id, source, loaded, status) = with_face(&value, |f| {
        (f.id, f.source.clone(), f.loaded.clone(), f.status)
    })?;
    if status == FaceStatus::Unloaded {
        with_face(&value, |f| f.status = FaceStatus::Loading)?;
        if let Source::Url(url) = source {
            let state = shared(context);
            let mut state = state.borrow_mut();
            let url = state.base_url().join(&url).map_or(url, String::from);
            state.fonts.pending.insert(id, face.clone());
            state.requests.push(Request::LoadFont { face: id, url });
        }
        refresh(context, false)?;
    }
    Ok(loaded)
}

fn is_loading(faces: &[JsObject]) -> bool {
    let status = |face: &JsObject| with_face(&face.clone().into(), |f| f.status);
    faces
        .iter()
        .any(|face| status(face) == Ok(FaceStatus::Loading))
}

/// Brings the set's `status`, `ready` and events in line with its faces.
fn refresh(context: &mut Context, failed: bool) -> JsResult<()> {
    let faces = shared(context).borrow().fonts.faces.clone();
    let loading = is_loading(&faces);
    let was_loading = std::mem::replace(&mut shared(context).borrow_mut().fonts.loading, loading);
    if loading == was_loading && !failed {
        return Ok(());
    }
    if loading {
        let (promise, resolvers) = JsPromise::new_pending(context);
        let state = shared(context);
        let mut state = state.borrow_mut();
        if matches!(state.fonts.ready, Ready::Settled(_)) {
            state.fonts.ready = Ready::Pending {
                promise: promise.into(),
                resolve: resolvers.resolve.into(),
            };
        }
        drop(state);
        return announce(context, "loading");
    }
    if failed {
        announce(context, "loadingerror")?;
    }
    announce(context, "loadingdone")?;
    let set = font_set(context)?;
    let ready = std::mem::replace(
        &mut shared(context).borrow_mut().fonts.ready,
        Ready::Settled(set.clone()),
    );
    if let Ready::Pending { resolve, .. } = ready {
        verve::call(context, &resolve, &JsValue::undefined(), &[set])?;
    }
    Ok(())
}

/// Fires the event `kind` at the set.
fn announce(context: &mut Context, kind: &str) -> JsResult<()> {
    let set = font_set(context)?;
    let bubbles = ("bubbles", false.into());
    super::events::fire(context, &set, "Event", kind, &[bubbles])?;
    Ok(())
}

/// The family names of a font shorthand, lower case.
fn families(font: &str) -> Vec<String> {
    let longhands = crate::css::font_longhands(font).unwrap_or_default();
    let family = longhands
        .into_iter()
        .find(|(name, _)| *name == "font-family");
    let unquote = |family: &str| family.trim().trim_matches(['"', '\'']).to_ascii_lowercase();
    let family = family.map(|(_, family)| family).unwrap_or_default();
    family.split(',').map(unquote).collect()
}

/// Every face in the set whose family `font` lists, with its status.
fn statuses(context: &Context, font: &str) -> JsResult<Vec<(JsObject, FaceStatus)>> {
    let wanted = families(font);
    let faces = shared(context).borrow().fonts.faces.clone();
    let mut found = Vec::new();
    for face in faces {
        let (family, status) = with_face(&face.clone().into(), |f| {
            let family = f.descriptors[0].trim_matches(['"', '\'']);
            (family.to_ascii_lowercase(), f.status)
        })?;
        if wanted.contains(&family) {
            found.push((face, status));
        }
    }
    Ok(found)
}

/// `document.fonts`.
pub fn font_set(context: &mut Context) -> JsResult<JsValue> {
    if let Some(set) = &shared(context).borrow().fonts.set {
        return Ok(set.clone().into());
    }
    let target = constructor_of(context, "EventTarget").expect("made");
    let set = target.construct(&[], None, context)?;
    let prototype = shared(context).borrow().prototype("FontFaceSet");
    set.set_prototype(Some(prototype));
    let ready = resolved(set.clone().into(), context)?;
    let state = shared(context);
    let mut state = state.borrow_mut();
    state.fonts.set = Some(set.clone());
    state.fonts.ready = Ready::Settled(ready);
    Ok(set.into())
}

/// The `FontFace` a method of the set was given.
fn face_arg(args: &[JsValue], method: &str, context: &mut Context) -> JsResult<JsObject> {
    let class = constructor_of(context, "FontFace").expect("made");
    match arg(args, 0).as_object() {
        Some(face) if JsValue::from(face.clone()).instance_of(&class.into(), context)? => Ok(face),
        _ => Err(type_error(format!(
            "Failed to execute '{method}' on 'FontFaceSet': parameter 1 is not of type 'FontFace'."
        ))),
    }
}

fn faces_iterator(context: &mut Context) -> JsResult<JsValue> {
    let faces = shared(context).borrow().fonts.faces.clone();
    let faces = array(faces.into_iter().map(JsValue::from).collect(), context);
    call_method(&faces, "values", &[], context)
}

const SET_METHODS: &[Method] = &[
    ("add", 1, |this, args, cx| {
        let face = face_arg(args, "add", cx)?;
        {
            let state = shared(cx);
            let faces = &mut state.borrow_mut().fonts.faces;
            if !faces.contains(&face) {
                faces.push(face);
            }
        }
        refresh(cx, false)?;
        Ok(this.clone())
    }),
    ("delete", 1, |_, args, cx| {
        let face = face_arg(args, "delete", cx)?;
        let removed = {
            let state = shared(cx);
            let faces = &mut state.borrow_mut().fonts.faces;
            let before = faces.len();
            faces.retain(|f| *f != face);
            faces.len() != before
        };
        refresh(cx, false)?;
        Ok(removed.into())
    }),
    ("has", 1, |_, args, cx| {
        let face = face_arg(args, "has", cx)?;
        Ok(shared(cx).borrow().fonts.faces.contains(&face).into())
    }),
    ("clear", 0, |_, _, cx| {
        shared(cx).borrow_mut().fonts.faces.clear();
        refresh(cx, false)?;
        Ok(JsValue::undefined())
    }),
    ("forEach", 1, |this, args, cx| {
        let faces = shared(cx).borrow().fonts.faces.clone();
        for face in faces {
            let face: JsValue = face.into();
            let given = [face.clone(), face, this.clone()];
            verve::call(cx, &arg(args, 0), &arg(args, 1), &given)?;
        }
        Ok(JsValue::undefined())
    }),
    ("values", 0, |_, _, cx| faces_iterator(cx)),
    ("keys", 0, |_, _, cx| faces_iterator(cx)),
    ("check", 1, |_, args, cx| {
        let font = string(&arg(args, 0), cx)?;
        let loaded = statuses(cx, &font)?
            .iter()
            .all(|(_, status)| *status == FaceStatus::Loaded);
        Ok(loaded.into())
    }),
    ("load", 1, |_, args, cx| {
        let font = string(&arg(args, 0), cx)?;
        let mut loads = Vec::new();
        for (face, _) in statuses(cx, &font)? {
            loads.push(load_face(cx, &face)?);
        }
        let promise = cx.global_object().get(js_string!("Promise"), cx)?;
        let loads = array(loads, cx);
        call_method(&promise, "all", &[loads], cx)
    }),
];

const SET_ACCESSORS: &[Accessor] = &[
    (
        "size",
        Some(|_, _, cx| Ok(shared(cx).borrow().fonts.faces.len().into())),
        None,
    ),
    (
        "status",
        Some(|_, _, cx| {
            let loading = shared(cx).borrow().fonts.loading;
            Ok(text(if loading { "loading" } else { "loaded" }))
        }),
        None,
    ),
    (
        "ready",
        Some(|_, _, cx| {
            Ok(match &shared(cx).borrow().fonts.ready {
                Ready::Settled(promise) | Ready::Pending { promise, .. } => promise.clone(),
            })
        }),
        None,
    ),
];

/// The descriptor `AT` of `DESCRIPTORS`, read.
fn descriptor<const AT: usize>(
    this: &JsValue,
    _: &[JsValue],
    _: &mut Context,
) -> JsResult<JsValue> {
    with_face(this, |f| text(&f.descriptors[AT]))
}

/// The descriptor `AT` of `DESCRIPTORS`, written.
fn set_descriptor<const AT: usize>(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let value = string(&arg(args, 0), context)?;
    with_face(this, |f| f.descriptors[AT] = value)?;
    Ok(JsValue::undefined())
}

macro_rules! descriptor_accessors {
    ($($at:literal $name:literal),*) => {
        &[$(($name, Some(descriptor::<$at>), Some(set_descriptor::<$at>))),*]
    };
}

const FACE_ACCESSORS: &[Accessor] = descriptor_accessors!(
    0 "family", 1 "style", 2 "weight", 3 "stretch", 4 "unicodeRange", 5 "variant",
    6 "featureSettings", 7 "variationSettings", 8 "display", 9 "ascentOverride",
    10 "descentOverride", 11 "lineGapOverride", 12 "sizeAdjust"
);

/// What a script reads of a face and cannot write.
const FACE_STATE: &[Accessor] = &[
    (
        "status",
        Some(|this, _, _| with_face(this, |f| text(f.status.as_str()))),
        None,
    ),
    (
        "loaded",
        Some(|this, _, _| with_face(this, |f| f.loaded.clone())),
        None,
    ),
];

const FACE_METHODS: &[Method] = &[("load", 0, |this, _, cx| {
    let face = this
        .as_object()
        .ok_or_else(|| type_error("Illegal invocation"))?;
    load_face(cx, &face)
})];

/// Makes `FontFace` and `FontFaceSet`, whose one instance is `document.fonts`.
pub fn install(context: &mut Context) -> JsResult<()> {
    let (_, face) = class(
        context,
        ClassSpec {
            name: "FontFace",
            length: 2,
            construct: construct_face,
            parent: None,
            methods: FACE_METHODS,
            accessors: FACE_ACCESSORS,
            statics: &[],
            global: true,
        },
    )?;
    super::extend_prototype(context, &face, &[], FACE_STATE)?;
    let (_, set) = class(
        context,
        ClassSpec {
            name: "FontFaceSet",
            length: 0,
            construct: illegal_constructor,
            parent: Some("EventTarget"),
            methods: SET_METHODS,
            accessors: SET_ACCESSORS,
            statics: &[],
            global: true,
        },
    )?;
    let iterate = function(context, "values", 0, |_, _, cx| faces_iterator(cx));
    hidden(&set, JsSymbol::iterator(), iterate, context)?;
    Ok(())
}
