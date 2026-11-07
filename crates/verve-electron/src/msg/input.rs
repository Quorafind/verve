//! What a person did to a window, as the page under it is told, and the view the shell reads back.

/// What a person did to the window, as the page under it is told.
#[derive(Clone, Debug)]
pub enum ToRendererInput {
    /// The page's viewport, in CSS pixels, and the window's scale factor.
    Viewport {
        width: f32,
        height: f32,
        scale: f32,
    },
    ColorScheme {
        dark: bool,
    },
    /// Deprecated and ignored: a page lays itself out on its own thread (`verve-layout`, P30), so
    /// no shell reports boxes. The variant is kept so that nothing that matches on it breaks.
    Layout(Vec<(verve_dom::NodeId, verve_dom::Rect)>),
    ScrollOffsets(Vec<(verve_dom::NodeId, [f32; 2])>),
    Frame {
        now: f64,
    },
    Pointer {
        kind: PointerKind,
        pointer: verve_dom::Pointer,
        target: Option<verve_dom::NodeId>,
    },
    Wheel {
        pointer: verve_dom::Pointer,
        dx: f32,
        dy: f32,
        target: Option<verve_dom::NodeId>,
    },
    Key {
        down: bool,
        key: verve_dom::Key,
    },
    /// An input method's commit, or a paste.
    Text(String),
    /// P24 makes it `Update { text, selection } | Commit | Cancel`.
    Composition(CompositionEvent),
    /// The window gained (`true`) or lost focus.
    Focus(bool),
    /// The page is drawn to a screen (`true`) or not: the window was hidden or minimized.
    Visibility(bool),
}

/// What an input method is doing with a field. Empty until P24 gives it its variants.
#[derive(Clone, Debug)]
pub enum CompositionEvent {}

/// What a pointer did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerKind {
    Down,
    Up,
    Move,
}

/// What the shell reads of a page to draw it: `Guest::View` of [`crate::RendererGuest`].
pub struct RendererView {
    /// The number of the last job the page ran, for whoever waits on one.
    pub applied: u64,
    /// The document as it is now, with what changed since the last view; `None` when nothing did.
    pub update: Option<verve_dom::Update>,
    /// Whether the page asked for an animation frame: send [`ToRendererInput::Frame`].
    pub wants_frame: bool,
    /// The CSS `cursor` of the element under the pointer.
    pub cursor: Option<String>,
    /// Where a candidate window goes: the caret's box (P24/P32).
    pub ime_rect: Option<verve_dom::Rect>,
    /// The page laid out: what the shell draws. `None` for a page with no layout.
    pub display_list: Option<std::sync::Arc<verve_layout::DisplayList>>,
}
