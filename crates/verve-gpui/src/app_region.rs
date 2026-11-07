//! `-webkit-app-region`: the parts of a window a person drags it by.
//!
//! The page's display list says, for every element whose style has `-webkit-app-region: drag` or
//! `no-drag`, where it is ([`regions_of`]): an [`AppRegion`] per element, in paint order (a later
//! one is on top of an earlier one), in the window's own coordinates. The drag area is what the
//! `drag` rectangles cover minus what a later `no-drag` one does (a button in a title bar), and a
//! drag rectangle after a `no-drag` one covers it again.
//! Each rectangle of the area becomes a hitbox the system asks about when it works out where a
//! click is: Windows answers it `HTCAPTION`, so dragging, a double click that maximizes and Aero
//! Snap are the system's own. On macOS the window is moved by the app, from the mouse down.

use gpui::{
    Bounds, Hitbox, HitboxBehavior, IntoElement, MouseButton, MouseDownEvent, Pixels, Styled,
    WindowControlArea, canvas, point, px, size,
};
use verve_electron::msg::Rect;

/// One element's `-webkit-app-region`, with the box it was laid out in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AppRegion {
    /// Where the element is, in the window's content area.
    pub rect: Rect,
    /// `true` for `drag`, `false` for `no-drag`.
    pub drag: bool,
}

/// The regions a page's display list holds: its hitboxes that have a `-webkit-app-region`, placed
/// in the window.
pub(crate) fn regions_of(list: &verve_layout::DisplayList) -> Vec<AppRegion> {
    list.resolved_hitboxes(&|_| None)
        .into_iter()
        .filter_map(|hitbox| {
            hitbox.app_region.map(|kind| AppRegion {
                rect: Rect {
                    x: hitbox.bounds.x,
                    y: hitbox.bounds.y,
                    width: hitbox.bounds.width,
                    height: hitbox.bounds.height,
                },
                drag: kind == verve_layout::AppRegionKind::Drag,
            })
        })
        .collect()
}

/// What `rect` leaves of itself when `hole` is taken out: up to four rectangles.
fn subtract(rect: Rect, hole: Rect) -> Vec<Rect> {
    let (right, bottom) = (rect.x + rect.width, rect.y + rect.height);
    let (hole_right, hole_bottom) = (hole.x + hole.width, hole.y + hole.height);
    if hole.x >= right || hole_right <= rect.x || hole.y >= bottom || hole_bottom <= rect.y {
        return vec![rect];
    }
    let (left, top) = (hole.x.max(rect.x), hole.y.max(rect.y));
    let (cut_right, cut_bottom) = (hole_right.min(right), hole_bottom.min(bottom));
    let piece = |x: f32, y: f32, to_x: f32, to_y: f32| Rect {
        x,
        y,
        width: to_x - x,
        height: to_y - y,
    };
    [
        piece(rect.x, rect.y, right, top),
        piece(rect.x, cut_bottom, right, bottom),
        piece(rect.x, top, left, cut_bottom),
        piece(cut_right, top, right, cut_bottom),
    ]
    .into_iter()
    .filter(|piece| piece.width > 0. && piece.height > 0.)
    .collect()
}

/// The rectangles a window is dragged by.
pub fn drag_area(regions: &[AppRegion]) -> Vec<Rect> {
    let mut area: Vec<Rect> = Vec::new();
    for region in regions {
        if region.drag {
            area.push(region.rect);
        } else {
            area = area
                .into_iter()
                .flat_map(|piece| subtract(piece, region.rect))
                .collect();
        }
    }
    area
}

fn bounds_of(rect: Rect) -> Bounds<Pixels> {
    Bounds::new(
        point(px(rect.x), px(rect.y)),
        size(px(rect.width), px(rect.height)),
    )
}

/// The layer that puts the drag area into a window's frame: it draws nothing.
pub(crate) fn layer(regions: &[AppRegion]) -> impl IntoElement {
    let area = drag_area(regions);
    canvas(
        move |_, window, _| {
            area.into_iter()
                .map(|rect| window.insert_hitbox(bounds_of(rect), HitboxBehavior::Normal))
                .collect::<Vec<Hitbox>>()
        },
        move |_, hitboxes, window, _| {
            for hitbox in &hitboxes {
                window.insert_window_control_hitbox(WindowControlArea::Drag, hitbox.clone());
            }
            move_by_mouse(hitboxes, window);
        },
    )
    .absolute()
    .size_full()
}

/// macOS: the app moves the window when the mouse goes down in the drag area, and a double click
/// zooms it. Elsewhere the system does both.
fn move_by_mouse(hitboxes: Vec<Hitbox>, window: &mut gpui::Window) {
    if !cfg!(target_os = "macos") {
        return;
    }
    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, _| {
        let inside = hitboxes.iter().any(|hitbox| hitbox.is_hovered(window));
        if phase.bubble() && inside && event.button == MouseButton::Left {
            match event.click_count {
                2 => window.zoom_window(),
                _ => window.start_window_move(),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, width: f32, height: f32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    fn drag(rect: Rect) -> AppRegion {
        AppRegion { rect, drag: true }
    }

    fn no_drag(rect: Rect) -> AppRegion {
        AppRegion { rect, drag: false }
    }

    fn covered(area: &[Rect]) -> f32 {
        area.iter().map(|piece| piece.width * piece.height).sum()
    }

    #[test]
    fn the_regions_of_a_list_are_its_hitboxes_that_have_one() {
        use verve_layout::{AppRegionKind, Cursor, DisplayList, PaintItem};
        let hitbox = |node, kind, x| PaintItem::Hitbox {
            node,
            bounds: verve_dom::Rect::new(x, 0., 10., 10.),
            cursor: Cursor::Auto,
            app_region: kind,
        };
        let list = DisplayList {
            revision: 1,
            items: vec![
                hitbox(1, Some(AppRegionKind::Drag), 0.),
                hitbox(2, None, 20.),
                hitbox(3, Some(AppRegionKind::NoDrag), 5.),
            ],
            rasters: Vec::new(),
            root: None,
        };
        assert_eq!(
            regions_of(&list),
            [
                drag(rect(0., 0., 10., 10.)),
                no_drag(rect(5., 0., 10., 10.))
            ]
        );
    }

    #[test]
    fn a_title_bar_is_dragged_everywhere_but_where_its_buttons_are() {
        let bar = rect(0., 0., 800., 32.);
        let buttons = rect(700., 0., 100., 32.);
        let area = drag_area(&[drag(bar), no_drag(buttons)]);
        assert_eq!(area, vec![rect(0., 0., 700., 32.)]);
    }

    #[test]
    fn a_no_drag_in_the_middle_leaves_four_pieces_that_add_up() {
        let area = drag_area(&[
            drag(rect(0., 0., 100., 100.)),
            no_drag(rect(40., 40., 20., 20.)),
        ]);
        assert_eq!(area.len(), 4);
        assert_eq!(covered(&area), 100. * 100. - 20. * 20.);
    }

    #[test]
    fn the_later_region_wins_where_two_overlap() {
        let hole = rect(10., 0., 10., 10.);
        let area = drag_area(&[drag(rect(0., 0., 30., 10.)), no_drag(hole), drag(hole)]);
        assert_eq!(covered(&area), 300.);
        assert!(drag_area(&[no_drag(hole)]).is_empty());
    }

    #[test]
    fn a_no_drag_that_misses_the_drag_region_changes_nothing() {
        let bar = rect(0., 0., 50., 10.);
        let area = drag_area(&[drag(bar), no_drag(rect(0., 20., 50., 10.))]);
        assert_eq!(area, vec![bar]);
    }
}
