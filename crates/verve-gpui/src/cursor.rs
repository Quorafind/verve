//! The cursor over a page: which one a hitbox asks for, the GPUI cursor that shows it and the name
//! Electron's `cursor-changed` reports it by.
//!
//! The page's `cursor: auto` is left to the shell, as it is left to the browser: over a link it
//! is the hand and over a field a person types in it is the I-beam. The names are what Electron
//! 39.8.10 reports (observed with `webContents.sendInputEvent` over a box of each `cursor`).

use gpui::CursorStyle;
use verve_dom::{NodeId, Snapshot};
use verve_layout::Cursor;

use crate::link;

/// The cursor a pointer over `node` shows, given the `cursor` its box asks for: a value other
/// than `auto` is taken as it is.
pub(crate) fn resolve(snapshot: Option<&Snapshot>, node: NodeId, asked: Cursor) -> Cursor {
    if asked != Cursor::Auto {
        return asked;
    }
    let Some(snapshot) = snapshot else {
        return Cursor::Default;
    };
    if link::href_of(snapshot, node).is_some() {
        Cursor::Pointer
    } else if shows_ibeam(snapshot, node) {
        Cursor::Text
    } else {
        Cursor::Default
    }
}

/// Whether an `<input>` of this `type` takes typed text: the ones that do not are the buttons,
/// the boxes, the pickers and the files. A `type` nobody knows is a text field, as in Chromium.
fn text_kind(kind: &str) -> bool {
    !matches!(
        kind,
        "checkbox"
            | "radio"
            | "button"
            | "submit"
            | "reset"
            | "image"
            | "file"
            | "color"
            | "range"
            | "hidden"
            | "date"
            | "time"
            | "datetime-local"
            | "month"
            | "week"
    )
}

/// Whether `node` is a field that shows the I-beam: a text field, read-only or not (observed in
/// Electron 39.8.10: a read-only input and a `type=foo` one show `text`), or an editing host.
fn shows_ibeam(snapshot: &Snapshot, node: NodeId) -> bool {
    let dom = &snapshot.dom;
    match dom.get(node).and_then(|n| n.tag()) {
        Some("textarea") => true,
        Some("input") => {
            let kind = dom.get_attr(node, "type").unwrap_or_default();
            text_kind(&kind.to_ascii_lowercase())
        }
        _ => dom.editing_host(node).is_some(),
    }
}

/// Whether `node` is a field an input method can type in: a text field that is not read-only,
/// or an editing host.
pub(crate) fn takes_text(snapshot: &Snapshot, node: NodeId) -> bool {
    let readonly = snapshot
        .dom
        .get(node)
        .is_some_and(|n| n.attr("readonly").is_some());
    shows_ibeam(snapshot, node) && !readonly
}

/// The GPUI cursor that looks like `cursor`; the ones GPUI has no picture for are the arrow
/// (`none` cannot be hidden through GPUI, P32's 「状态」).
pub(crate) fn style(cursor: Cursor) -> CursorStyle {
    match cursor {
        Cursor::Pointer => CursorStyle::PointingHand,
        Cursor::Text => CursorStyle::IBeam,
        Cursor::VerticalText => CursorStyle::IBeamCursorForVerticalLayout,
        Cursor::Crosshair => CursorStyle::Crosshair,
        Cursor::Grab => CursorStyle::OpenHand,
        Cursor::Grabbing => CursorStyle::ClosedHand,
        Cursor::NotAllowed | Cursor::NoDrop => CursorStyle::OperationNotAllowed,
        Cursor::Alias => CursorStyle::DragLink,
        Cursor::Copy => CursorStyle::DragCopy,
        Cursor::ContextMenu => CursorStyle::ContextualMenu,
        Cursor::ColResize => CursorStyle::ResizeColumn,
        Cursor::RowResize => CursorStyle::ResizeRow,
        Cursor::EwResize | Cursor::EResize | Cursor::WResize => CursorStyle::ResizeLeftRight,
        Cursor::NsResize | Cursor::NResize | Cursor::SResize => CursorStyle::ResizeUpDown,
        Cursor::NwseResize | Cursor::NwResize | Cursor::SeResize => {
            CursorStyle::ResizeUpLeftDownRight
        }
        Cursor::NeswResize | Cursor::NeResize | Cursor::SwResize => {
            CursorStyle::ResizeUpRightDownLeft
        }
        _ => CursorStyle::Arrow,
    }
}

/// The name `cursor-changed` gives `cursor`. The arrow is called `pointer` and the hand `hand`;
/// `no-drop` is `nodrop` and `all-scroll` is `move`. The side and corner resizes other than
/// `e-resize` and `n-resize` follow their pattern; those two were observed.
pub(crate) fn name(cursor: Cursor) -> &'static str {
    match cursor {
        Cursor::Auto | Cursor::Default => "pointer",
        Cursor::Pointer => "hand",
        Cursor::None => "none",
        Cursor::ContextMenu => "context-menu",
        Cursor::Help => "help",
        Cursor::Progress => "progress",
        Cursor::Wait => "wait",
        Cursor::Cell => "cell",
        Cursor::Crosshair => "crosshair",
        Cursor::Text => "text",
        Cursor::VerticalText => "vertical-text",
        Cursor::Alias => "alias",
        Cursor::Copy => "copy",
        Cursor::Move | Cursor::AllScroll => "move",
        Cursor::NoDrop => "nodrop",
        Cursor::NotAllowed => "not-allowed",
        Cursor::Grab => "grab",
        Cursor::Grabbing => "grabbing",
        Cursor::ColResize => "col-resize",
        Cursor::RowResize => "row-resize",
        Cursor::NResize => "n-resize",
        Cursor::EResize => "e-resize",
        Cursor::SResize => "s-resize",
        Cursor::WResize => "w-resize",
        Cursor::NeResize => "ne-resize",
        Cursor::NwResize => "nw-resize",
        Cursor::SeResize => "se-resize",
        Cursor::SwResize => "sw-resize",
        Cursor::EwResize => "ew-resize",
        Cursor::NsResize => "ns-resize",
        Cursor::NeswResize => "nesw-resize",
        Cursor::NwseResize => "nwse-resize",
        Cursor::ZoomIn => "zoom-in",
        Cursor::ZoomOut => "zoom-out",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_names_are_what_electron_reports() {
        for (cursor, name) in [
            (Cursor::Auto, "pointer"),
            (Cursor::Default, "pointer"),
            (Cursor::Pointer, "hand"),
            (Cursor::Text, "text"),
            (Cursor::NoDrop, "nodrop"),
            (Cursor::AllScroll, "move"),
            (Cursor::NwseResize, "nwse-resize"),
            (Cursor::ContextMenu, "context-menu"),
        ] {
            assert_eq!(super::name(cursor), name);
        }
    }

    #[test]
    fn a_cursor_the_page_names_is_taken_as_it_is() {
        assert_eq!(resolve(None, 1, Cursor::Crosshair), Cursor::Crosshair);
        assert_eq!(resolve(None, 1, Cursor::Auto), Cursor::Default);
    }

    #[test]
    fn the_hand_is_the_pointer_and_the_arrow_is_the_default() {
        assert_eq!(style(Cursor::Pointer), CursorStyle::PointingHand);
        assert_eq!(style(Cursor::Default), CursorStyle::Arrow);
        assert_eq!(style(Cursor::Text), CursorStyle::IBeam);
    }
}
