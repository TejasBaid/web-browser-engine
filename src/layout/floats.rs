//! Float placement and exclusion queries within one block formatting context.
//! Coordinates are relative to the BFC root's content box.

use super::Rect;

/// Which side a float sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// `float: left`
    Left,
    /// `float: right`
    Right,
}

#[derive(Clone, Copy, Debug)]
struct PlacedFloat {
    rect: Rect, // margin box
    side: Side,
}

/// The floats placed so far in a block formatting context.
#[derive(Clone, Debug, Default)]
pub struct FloatContext {
    floats: Vec<PlacedFloat>,
    /// Floats may not be placed above the top of an earlier float.
    lowest_top: f32,
}

impl FloatContext {
    /// An empty context.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether no floats have been placed.
    pub fn is_empty(&self) -> bool {
        self.floats.is_empty()
    }

    /// Available horizontal band `[left, right]` within `[min_x, max_x]` for content
    /// occupying `[y, y + height)`.
    pub fn available(&self, y: f32, height: f32, min_x: f32, max_x: f32) -> (f32, f32) {
        let mut left = min_x;
        let mut right = max_x;
        let h = height.max(0.01);
        for f in &self.floats {
            if f.rect.y < y + h && f.rect.bottom() > y && f.rect.height > 0.0 {
                match f.side {
                    Side::Left => left = left.max(f.rect.right()),
                    Side::Right => right = right.min(f.rect.x),
                }
            }
        }
        (left, right)
    }

    /// The smallest float bottom edge strictly below `y` among floats overlapping `y`,
    /// i.e. the next position where the available width may grow.
    pub fn next_bottom_below(&self, y: f32) -> Option<f32> {
        self.floats
            .iter()
            .filter(|f| f.rect.y <= y + 0.001 && f.rect.bottom() > y + 0.001)
            .map(|f| f.rect.bottom())
            .fold(None, |acc: Option<f32>, b| Some(acc.map_or(b, |a| a.min(b))))
    }

    /// Places a float whose margin box is `width` x `height`, no higher than `min_y`,
    /// inside `[min_x, max_x]`. Returns the margin box's top-left corner.
    pub fn place(&mut self, width: f32, height: f32, side: Side, min_y: f32, min_x: f32, max_x: f32) -> (f32, f32) {
        let mut y = min_y.max(self.lowest_top);
        // Floats must also stay below earlier floats on the same side that would
        // otherwise overlap (handled by the band search) and be no higher than any
        // earlier float's top.
        loop {
            let (l, r) = self.available(y, height, min_x, max_x);
            let fits = r - l >= width - 0.01;
            if fits || self.next_bottom_below(y).is_none() {
                let x = match side {
                    Side::Left => l,
                    Side::Right => r - width,
                };
                let rect = Rect::new(x, y, width, height);
                self.floats.push(PlacedFloat { rect, side });
                self.lowest_top = self.lowest_top.max(y);
                return (x, y);
            }
            y = self.next_bottom_below(y).unwrap();
        }
    }

    /// The y below which no float on the given side(s) extends (for `clear`).
    pub fn clearance_y(&self, left: bool, right: bool) -> f32 {
        self.floats
            .iter()
            .filter(|f| (left && f.side == Side::Left) || (right && f.side == Side::Right))
            .map(|f| f.rect.bottom())
            .fold(f32::NEG_INFINITY, f32::max)
    }

    /// The lowest float bottom (for BFC roots, whose auto height includes floats).
    pub fn max_bottom(&self) -> f32 {
        self.floats.iter().map(|f| f.rect.bottom()).fold(0.0, f32::max)
    }

    /// Ensures later floats are not placed above `y` (used after a line containing
    /// float anchors).
    pub fn raise_floor(&mut self, y: f32) {
        self.lowest_top = self.lowest_top.max(y);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_stack_and_wrap() {
        let mut fc = FloatContext::new();
        assert_eq!(fc.place(100.0, 50.0, Side::Left, 0.0, 0.0, 300.0), (0.0, 0.0));
        assert_eq!(fc.place(100.0, 30.0, Side::Left, 0.0, 0.0, 300.0), (100.0, 0.0));
        assert_eq!(fc.place(50.0, 20.0, Side::Right, 0.0, 0.0, 300.0), (250.0, 0.0));
        // Doesn't fit next to the others: moves below the shortest overlapping float.
        assert_eq!(fc.place(120.0, 10.0, Side::Left, 0.0, 0.0, 300.0), (100.0, 30.0));
        assert_eq!(fc.available(10.0, 5.0, 0.0, 300.0), (200.0, 250.0));
        assert_eq!(fc.available(60.0, 5.0, 0.0, 300.0), (0.0, 300.0));
        assert_eq!(fc.clearance_y(true, false), 50.0);
        assert_eq!(fc.clearance_y(false, true), 20.0);
    }

    #[test]
    fn later_floats_never_rise_above_earlier_ones() {
        let mut fc = FloatContext::new();
        fc.place(50.0, 100.0, Side::Left, 40.0, 0.0, 300.0);
        let (_, y) = fc.place(50.0, 10.0, Side::Right, 0.0, 0.0, 300.0);
        assert_eq!(y, 40.0);
    }
}
