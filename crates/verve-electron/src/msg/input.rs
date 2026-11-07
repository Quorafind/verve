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
    /// An input method is composing, or has finished.
    Composition(CompositionEvent),
    /// The window gained (`true`) or lost focus.
    Focus(bool),
    /// The page is drawn to a screen (`true`) or not: the window was hidden or minimized.
    Visibility(bool),
}

/// What an input method is doing with the field or editable region that has the keyboard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompositionEvent {
    /// It is composing `text` at the caret, replacing what it composed before; `selection` is
    /// where its caret sits inside `text`, in UTF-16 units.
    Update {
        text: String,
        selection: Option<std::ops::Range<usize>>,
    },
    /// The composition is over, settled as this text (which may differ from the last update).
    Commit(String),
    /// The composition was abandoned (Escape, focus lost): what it composed goes.
    Cancel,
}

/// What a pointer did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerKind {
    Down,
    Up,
    Move,
    /// The pointer left the window: nothing is under it any more.
    Leave,
}

/// What the shell reads of a page to draw it: `Guest::View` of [`crate::RendererGuest`].
pub struct RendererView {
    /// The number of the last job the page ran, for whoever waits on one.
    pub applied: u64,
    /// The document as it is now, with what changed since the last view; `None` when nothing did.
    pub update: Option<verve_dom::Update>,
    /// Whether the page asked for an animation frame: send [`ToRendererInput::Frame`].
    pub wants_frame: bool,
    /// Not used: the renderer does not know where the pointer is. The shell reads the cursor from
    /// the display list's hitboxes and reports it itself (P32).
    pub cursor: Option<String>,
    /// Where a candidate window goes: the caret's box (P24/P32).
    pub ime_rect: Option<verve_dom::Rect>,
    /// The page laid out: what the shell draws. `None` for a page with no layout.
    pub display_list: Option<std::sync::Arc<verve_layout::DisplayList>>,
    /// The pictures the page has decoded, which the display list names by key (P35). The same
    /// `Arc` until a picture arrives or leaves.
    pub images: std::sync::Arc<verve_layout::ImageTable>,
}
