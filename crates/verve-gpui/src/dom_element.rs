//! The GPUI element that draws a page: it fills its parent, puts the page's hitboxes into the
//! frame with their cursors, replays the [plan](crate::plan) of the display list, and turns the
//! wheel into a scroll ([`crate::wheel`]).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use gpui::{
    App, Background, BorderStyle, Bounds, ContentMask, Corners, CursorStyle, Edges, Element,
    ElementId, ElementInputHandler, GlobalElementId, Hitbox, HitboxBehavior, Hsla,
    InspectorElementId, IntoElement, LayoutId, MouseDownEvent, MouseExitEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, RenderImage, ScrollWheelEvent, StrikethroughStyle, Style, UnderlineStyle,
    Window, linear_color_stop, linear_gradient, point, px, size, transparent_black,
};
use verve_dom::Rect;
use verve_layout::{DisplayList, ImageKey, Rgba, VectorSpec};

use crate::cursor;
use crate::dom_view::DomState;
use crate::ime;
use crate::input;
use crate::plan::{Command, Paint, Shape};
use crate::text_measurer::GpuiMeasurer;
use crate::wheel;

/// The page of a window, drawn from the state it shares with its view.
pub(crate) struct DomElement {
    state: Rc<RefCell<DomState>>,
}

impl DomElement {
    /// Registers what the frame listens to: the pointer, the wheel and, when a field has the
    /// keyboard, the platform's text input.
    fn listen(&self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let origin = bounds.origin;
        let state = Rc::clone(&self.state);
        window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, _| {
            if phase.bubble() {
                wheel::turned(&state, event, origin, window);
            }
        });
        let state = Rc::clone(&self.state);
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase.bubble() {
                input::mouse_down(&state, event, origin, window, cx);
            }
        });
        let state = Rc::clone(&self.state);
        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, _| {
            if phase.bubble() {
                input::mouse_up(&state, event, origin);
            }
        });
        let state = Rc::clone(&self.state);
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, _| {
            if phase.bubble() {
                input::mouse_move(&state, event, origin, window);
            }
        });
        let state = Rc::clone(&self.state);
        window.on_mouse_event(move |event: &MouseExitEvent, phase, _, _| {
            if phase.bubble() {
                input::mouse_exit(&state, event);
            }
        });
        let (focus, bridge, editable) = {
            let state = self.state.borrow();
            let editable = (state.snapshot.as_ref()).is_some_and(ime::accepts_ime);
            (state.focus.clone(), state.ime.clone(), editable)
        };
        let paused = std::mem::take(&mut self.state.borrow_mut().ime_pause);
        if paused {
            // One frame without the handler; the next one registers it again.
            window.request_animation_frame();
        } else if let (Some(focus), Some(bridge), true) = (focus, bridge, editable) {
            window.handle_input(&focus, ElementInputHandler::new(bounds, bridge), cx);
        }
    }
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
        if let Some(stats) = state.stats.as_mut() {
            stats.begin();
        }
        let viewport = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        // A window that was resized does not keep a pop-up where it was.
        if std::mem::replace(&mut state.viewport, viewport) != viewport
            && state.popup.take().is_some()
        {
            window.refresh();
        }
        let Some(plan) = state.plan() else {
            return Vec::new();
        };
        // Every hitbox of the page is one of the frame, so the topmost box under the pointer says
        // the cursor; an `auto` one is worked out from the page (a link is a hand).
        plan.hitboxes
            .iter()
            .map(|hitbox| {
                let at = in_window(bounds.origin, hitbox.bounds);
                let shown = state.cursor_of(hitbox.node, hitbox.cursor);
                (
                    window.insert_hitbox(at, HitboxBehavior::Normal),
                    cursor::style(shown),
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
        cx: &mut App,
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
            self.blink(&plan, window, cx);
        }
        self.listen(bounds, window, cx);
        if let Some(stats) = self.state.borrow_mut().stats.as_mut() {
            stats.end();
            // Stats want frames one after the other, not only when the page changes.
            window.request_animation_frame();
        }
    }
}

impl DomElement {
    /// Paints again when the caret is due to flip, if the plan has one.
    fn blink(&self, plan: &crate::plan::Plan, window: &mut Window, cx: &mut App) {
        let has_caret = plan
            .commands
            .iter()
            .any(|command| matches!(command.shape, Shape::Caret { .. }));
        // A window that is not the active one does not blink: it would only repaint for nothing.
        if !has_caret
            || !window.is_window_active()
            || std::mem::replace(&mut self.state.borrow_mut().blink.waiting, true)
        {
            return;
        }
        let state = Rc::clone(&self.state);
        let wait = state.borrow().blink.until_flip(Instant::now());
        window
            .spawn(cx, async move |cx| {
                cx.background_executor().timer(wait).await;
                state.borrow_mut().blink.waiting = false;
                cx.update(|window, _| window.refresh()).ok();
            })
            .detach();
    }

    /// A caret, when it is in the shown half of its blink.
    fn caret(
        &self,
        bounds: Rect,
        color: Rgba,
        list: &DisplayList,
        origin: gpui::Point<Pixels>,
        window: &mut Window,
    ) {
        let shown = (self.state.borrow_mut().blink).visible(bounds, Instant::now());
        if shown {
            self.fill(bounds, [0.; 4], &Paint::Solid(color), list, origin, window);
        }
    }

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
        // Text and pictures are the many shapes of a page: the ones outside the window or
        // outside their own clip are not asked of the text system or uploaded to the atlas.
        let seen = match &command.shape {
            Shape::Glyphs { bounds, .. } => Some(Some(*bounds)),
            Shape::Picture {
                bounds, visible, ..
            } => Some(visible.intersection(bounds)),
            _ => None,
        };
        if let Some(seen) = seen
            && !seen.is_some_and(|seen| self.shows(seen, command.clip, origin, window))
        {
            return;
        }
        window.with_content_mask(mask, |window| match &command.shape {
            Shape::Fill {
                bounds,
                radii,
                paint,
            } => self.fill(*bounds, *radii, paint, list, origin, window),
            Shape::Glyphs {
                origin: at,
                size,
                color,
                glyphs,
                ..
            } => paint_glyphs(*at, *size, *color, glyphs, origin, window),
            Shape::Decoration {
                bounds,
                color,
                wavy,
                through,
            } => paint_decoration(*bounds, *color, *wavy, *through, origin, window),
            Shape::Picture {
                bounds,
                visible,
                radii,
                image,
                frame,
                alpha,
            } => self.page_picture(
                *bounds,
                *visible,
                *radii,
                (*image, *frame as usize, *alpha, &list.vectors),
                origin,
                window,
            ),
            Shape::Caret { bounds, color } => self.caret(*bounds, *color, list, origin, window),
            Shape::Border {
                bounds,
                radii,
                widths,
                color,
                dashed,
            } => paint_border(*bounds, *radii, *widths, *color, *dashed, origin, window),
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
                paint_shadow(in_window(origin, *bounds), *radii, shadow, window);
            }
        });
    }

    /// Whether a rectangle of the page is in the window and in its clip.
    fn shows(
        &self,
        bounds: Rect,
        clip: Option<Rect>,
        origin: gpui::Point<Pixels>,
        window: &Window,
    ) -> bool {
        let at = in_window(origin, bounds);
        let view = Bounds::new(point(px(0.), px(0.)), window.viewport_size());
        at.intersects(&view) && clip.is_none_or(|clip| at.intersects(&in_window(origin, clip)))
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

    /// A picture the page loaded, the whole of it over `bounds`, of which `visible` shows.
    fn page_picture(
        &self,
        bounds: Rect,
        visible: Rect,
        radii: [f32; 4],
        (key, frame, alpha, vectors): (ImageKey, usize, f32, &[VectorSpec]),
        origin: gpui::Point<Pixels>,
        window: &mut Window,
    ) {
        // An SVG is drawn at the size it is shown at, in the window pixels.
        let scale = window.scale_factor();
        let pixels = [bounds.width * scale, bounds.height * scale];
        let Some(picture) = self
            .state
            .borrow_mut()
            .pictures
            .page(key, alpha, vectors, pixels)
        else {
            return;
        };
        let frame = frame.min(picture.frame_count().saturating_sub(1));
        // An error is a picture GPUI could not take; the page shows without it.
        let _ = window.paint_image(
            in_window(origin, visible),
            in_window(origin, bounds),
            corners(radii),
            picture,
            frame,
            false,
        );
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

/// Draws one box shadow, inside the box or behind it.
fn paint_shadow(at: Bounds<Pixels>, radii: [f32; 4], shadow: gpui::BoxShadow, window: &mut Window) {
    if shadow.inset {
        window.paint_inset_shadows(at, corners(radii), &[shadow]);
    } else {
        window.paint_drop_shadows(at, corners(radii), &[shadow]);
    }
}

/// Draws an underline, an overline (`through` false) or a line through text.
fn paint_decoration(
    bounds: Rect,
    color: Rgba,
    wavy: bool,
    through: bool,
    origin: gpui::Point<Pixels>,
    window: &mut Window,
) {
    let at = in_window(origin, bounds);
    let color = hsla(color);
    if through {
        let style = StrikethroughStyle {
            thickness: px(bounds.height),
            color: Some(color),
        };
        window.paint_strikethrough(at.origin, at.size.width, &style);
    } else {
        let style = UnderlineStyle {
            thickness: px(bounds.height),
            color: Some(color),
            wavy,
        };
        window.paint_underline(at.origin, at.size.width, &style);
    }
}

/// Draws a border: one colour, each side as wide as `widths` (top, right, bottom, left) says.
fn paint_border(
    bounds: Rect,
    radii: [f32; 4],
    widths: [f32; 4],
    color: Rgba,
    dashed: bool,
    origin: gpui::Point<Pixels>,
    window: &mut Window,
) {
    let edges = Edges {
        top: px(widths[0]),
        right: px(widths[1]),
        bottom: px(widths[2]),
        left: px(widths[3]),
    };
    let style = if dashed {
        BorderStyle::Dashed
    } else {
        BorderStyle::Solid
    };
    window.paint_quad(gpui::quad(
        in_window(origin, bounds),
        corners(radii),
        transparent_black(),
        edges,
        hsla(color),
        style,
    ));
}

/// Draws shaped glyphs from the left end of their baseline: each in the font that shaped it, an
/// emoji in colour. GPUI's `paint_emoji` takes no colour and does not follow the element's
/// opacity (`with_element_opacity` is `pub(crate)` in the fork), so an emoji whose faded alpha
/// is nothing is left out and one that is partly faded is drawn in full: the fork patch that makes
/// `with_element_opacity` public lifts that.
fn paint_glyphs(
    at: [f32; 2],
    size: f32,
    color: Rgba,
    glyphs: &[verve_layout::PlacedGlyph],
    origin: gpui::Point<Pixels>,
    window: &mut Window,
) {
    let color = hsla(color);
    for glyph in glyphs {
        if glyph.is_emoji && color.a <= 0.004 {
            continue;
        }
        let place = point(origin.x + px(at[0] + glyph.x), origin.y + px(at[1]));
        let font = GpuiMeasurer::font_id(glyph.font);
        // An error is a glyph the platform could not draw; the text goes on without it.
        let _ = if glyph.is_emoji {
            window.paint_emoji(place, font, gpui::GlyphId(glyph.id), px(size))
        } else {
            window.paint_glyph(place, font, gpui::GlyphId(glyph.id), px(size), color)
        };
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
