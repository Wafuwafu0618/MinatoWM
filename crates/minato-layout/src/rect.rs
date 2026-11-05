use serde::{Deserialize, Serialize};

/// An axis-aligned rectangle in screen pixels.
///
/// Unlike the Win32 `RECT`, this stores an origin plus a size, so `w`/`h`
/// are always the real width and height.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    /// Builds a rect from edge coordinates (`left, top, right, bottom`).
    pub const fn from_edges(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            x: left,
            y: top,
            w: right - left,
            h: bottom - top,
        }
    }

    pub const fn right(&self) -> i32 {
        self.x + self.w
    }

    pub const fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub const fn area(&self) -> i64 {
        self.w as i64 * self.h as i64
    }

    pub fn center(&self) -> (i32, i32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }

    /// Shrinks the rect by `amount` on every side (never below zero size).
    pub fn inset(&self, amount: i32) -> Self {
        self.inset_sides(amount, amount, amount, amount)
    }

    /// Shrinks each side independently. Negative values grow the rect.
    pub fn inset_sides(&self, left: i32, top: i32, right: i32, bottom: i32) -> Self {
        let w = (self.w - left - right).max(0);
        let h = (self.h - top - bottom).max(0);
        Self {
            x: self.x + left,
            y: self.y + top,
            w,
            h,
        }
    }

    pub fn contains_point(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    /// Area of the intersection of two rects (0 when disjoint).
    pub fn intersection_area(&self, other: &Rect) -> i64 {
        let w = (self.right().min(other.right()) - self.x.max(other.x)).max(0) as i64;
        let h = (self.bottom().min(other.bottom()) - self.y.max(other.y)).max(0) as i64;
        w * h
    }

    /// Length of the overlap between the vertical spans of both rects.
    pub fn vertical_overlap(&self, other: &Rect) -> i32 {
        (self.bottom().min(other.bottom()) - self.y.max(other.y)).max(0)
    }

    /// Length of the overlap between the horizontal spans of both rects.
    pub fn horizontal_overlap(&self, other: &Rect) -> i32 {
        (self.right().min(other.right()) - self.x.max(other.x)).max(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges_round_trip() {
        let r = Rect::from_edges(10, 20, 110, 220);
        assert_eq!(r, Rect::new(10, 20, 100, 200));
        assert_eq!(r.right(), 110);
        assert_eq!(r.bottom(), 220);
    }

    #[test]
    fn inset_never_negative() {
        let r = Rect::new(0, 0, 10, 10).inset(8);
        assert_eq!(r.w, 0);
        assert_eq!(r.h, 0);
    }

    #[test]
    fn intersection() {
        let a = Rect::new(0, 0, 100, 100);
        let b = Rect::new(50, 50, 100, 100);
        assert_eq!(a.intersection_area(&b), 2500);
        assert_eq!(a.intersection_area(&Rect::new(100, 0, 10, 10)), 0);
    }
}
