//! The GPUI element that draws a page: it fills its parent, puts the page's hitboxes into the
//! frame with their cursors, replays the [plan](crate::plan) of the display list, and turns the
//! wheel into a scroll ([`crate::wheel`]).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    App, Background, BorderStyle, Bounds, ContentMask, Corners, CursorStyle, Edges, Element,
    ElementId, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement,
    LayoutId, Pixels, RenderImage, ScrollWheelEvent, Style, Window, linear_color_stop,
    linear_gradient, point, px, size, transparent_black,
};
use verve_dom::Rect;
use verve_layout::{Cursor, DisplayList, ImageKey, Rgba};

use crate::dom_view::DomState;
use crate::plan::{Command, Paint, Shape};
use crate::wheel;

/// The page of a window, drawn from the state it shares with its view.
pub(crate) struct DomElement {
    state: Rc<RefCell<DomState>>,
}

impl DomElement {
    pub(crate) fn new(state: Rc<RefCell<DomState>>) -> Self {
        Self { state }
    }
}

impl IntoElement for DomElement {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for DomElement {
    type RequestLayoutState = ();
    /// The hitboxes the page's cursors ask for, with the cursor of each.
    type PrepaintState = Vec<(Hitbox, CursorStyle)>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = gpui::relative(1.).into();
        style.size.height = gpui::relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        _: &mut App,
    ) -> Vec<(Hitbox, CursorStyle)> {
        let mut state = self.state.borrow_mut();
        state.viewport = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        let Some(plan) = state.plan() else {
            return Vec::new();
        };
        // Every hitbox of the page is one of the frame, so the topmost box under the pointer says
        // the cursor, `auto` (the arrow, for now) included: P32 brings the rest when the pointer
        // is forwarded to the page.
        plan.hitboxes
            .iter()
            .map(|hitbox| {
                let at = in_window(bounds.origin, hitbox.bounds);
                (
                    window.insert_hitbox(at, HitboxBehavior::Normal),
                    cursor_style(hitbox.cursor),
                )
            })
            .collect()
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        hitboxes: &mut Vec<(Hitbox, CursorStyle)>,
        window: &mut Window,
        _: &mut App,
    ) {
        for (hitbox, style) in hitboxes.iter() {
            window.set_cursor_style(*style, hitbox);
        }
        let (list, plan) = {
            let mut state = self.state.borrow_mut();
            (state.list.clone(), state.plan())
        };
        if let (Some(list), Some(plan)) = (list, plan) {
            self.give_back_pictures(window);
            for command in &plan.commands {
                self.paint_command(command, &list, bounds.origin, window);
            }
            self.state.borrow_mut().pictures.end_frame();
        }
        let state = Rc::clone(&self.state);
        let origin = bounds.origin;
        window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, _| {
            if phase.bubble() {
                wheel::turned(&state, event, origin, window);
            }
        });
    }
}

impl DomElement {
    /// Gives the pictures that no list asks for any more back to GPUI's atlas.
    fn give_back_pictures(&self, window: &mut Window) {
        let retired = self.state.borrow_mut().pictures.take_retired();
        for picture in retired {
            // An error is a picture GPUI never had.
            let _ = window.drop_image(picture);
        }
    }

    fn paint_command(
        &self,
        command: &Command,
        list: &DisplayList,
        origin: gpui::Point<Pixels>,
        window: &mut Window,
    ) {
        let mask = command.clip.map(|clip| ContentMask {
            bounds: in_window(origin, clip),
        });
        window.with_content_mask(mask, |window| match &command.shape {
            Shape::Fill {
                bounds,
                radii,
                paint,
            } => self.fill(*bounds, *radii, paint, list, origin, window),
            Shape::Border {
                bounds,
                radii,
                widths,
                color,
                dashed,
            } => {
                let edges = Edges {
                    top: px(widths[0]),
                    right: px(widths[1]),
                    bottom: px(widths[2]),
                    left: px(widths[3]),
                };
                let style = if *dashed {
                    BorderStyle::Dashed
                } else {
                    BorderStyle::Solid
                };
                window.paint_quad(gpui::quad(
                    in_window(origin, *bounds),
                    corners(*radii),
                    transparent_black(),
                    edges,
                    hsla(*color),
                    style,
                ));
            }
            Shape::Shadow {
                bounds,
                radii,
                offset,
                blur,
                spread,
                color,
                inset,
            } => {
                let shadow = gpui::BoxShadow {
                    color: hsla(*color),
                    offset: point(px(offset[0]), px(offset[1])),
                    blur_radius: px(*blur),
                    spread_radius: px(*spread),
                    inset: *inset,
                };
                let at = in_window(origin, *bounds);
                if *inset {
                    window.paint_inset_shadows(at, corners(*radii), &[shadow]);
                } else {
                    window.paint_drop_shadows(at, corners(*radii), &[shadow]);
                }
            }
        });
    }

    fn fill(
        &self,
        bounds: Rect,
        radii: [f32; 4],
        paint: &Paint,
        list: &DisplayList,
        origin: gpui::Point<Pixels>,
        window: &mut Window,
    ) {
        let at = in_window(origin, bounds);
        let background: Background = match paint {
            Paint::Solid(color) => hsla(*color).into(),
            Paint::Linear { angle, from, to } => linear_gradient(
                *angle,
                linear_color_stop(hsla(from.0), from.1),
                linear_color_stop(hsla(to.0), to.1),
            ),
            Paint::Raster { key, alpha } => {
                if let Some(picture) = self.picture(*key, *alpha, list, window) {
                    // An error is a picture GPUI could not take; the box stays as it is.
                    let _ = window.paint_image(at, at, corners(radii), picture, 0, false);
                }
                return;
            }
        };
        let mut quad = gpui::fill(at, background);
        quad.corner_radii = corners(radii);
        window.paint_quad(quad);
    }

    /// The picture of a gradient the renderer asked the shell to rasterise, for the window's scale
    /// and the opacity it is shown with.
    fn picture(
        &self,
        key: ImageKey,
        alpha: f32,
        list: &DisplayList,
        window: &Window,
    ) -> Option<Arc<RenderImage>> {
        let spec = list.rasters.iter().find(|spec| spec.key == key)?;
        let scale = window.scale_factor().ceil().max(1.);
        self.state.borrow_mut().pictures.get(spec, scale, alpha)
    }
}

fn in_window(origin: gpui::Point<Pixels>, rect: Rect) -> Bounds<Pixels> {
    Bounds::new(
        point(origin.x + px(rect.x), origin.y + px(rect.y)),
        size(px(rect.width), px(rect.height)),
    )
}

fn corners(radii: [f32; 4]) -> Corners<Pixels> {
    Corners {
        top_left: px(radii[0]),
        top_right: px(radii[1]),
        bottom_right: px(radii[2]),
        bottom_left: px(radii[3]),
    }
}

fn hsla([r, g, b, a]: Rgba) -> Hsla {
    gpui::Rgba { r, g, b, a }.into()
}

/// The cursor GPUI shows for a CSS `cursor`; the ones it has no picture for are the arrow.
fn cursor_style(cursor: Cursor) -> CursorStyle {
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
