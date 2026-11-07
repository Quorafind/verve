//! The box the drawer reports for an element: `getBoundingClientRect` and friends answer from it.

/// A box, in CSS pixels from the viewport's top left.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }

    pub fn union(&self, other: &Rect) -> Rect {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Rect {
            x,
            y,
            width: (self.x + self.width).max(other.x + other.width) - x,
            height: (self.y + self.height).max(other.y + other.height) - y,
        }
    }

    pub fn intersection(&self, other: &Rect) -> Option<Rect> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = (self.x + self.width).min(other.x + other.width);
        let bottom = (self.y + self.height).min(other.y + other.height);
        (right >= x
            && bottom >= y
            && (right > x || bottom > y || self.width == 0. || self.height == 0.))
            .then_some(Rect {
                x,
                y,
                width: right - x,
                height: bottom - y,
            })
    }
}

impl crate::Document {
    /// The box the layout provider (or the drawer, through [`Document::set_layout`](crate::Document::set_layout))
    /// keeps for `node`, laid out afresh first if the document changed: `None` for a node with no box.
    pub fn rect(&self, node: crate::NodeId) -> Option<Rect> {
        self.laid().rects.get(&node).copied()
    }
}
