use crate::{Direction, Rect};

/// How far apart two edges may be and still count as touching. Covers
/// inner gaps and off-by-one rounding.
const TOLERANCE: i32 = 64;

/// Finds the rect next to `rects[idx]` in `dir`.
///
/// Candidates must lie on that side (allowing for gaps) and overlap the
/// current rect on the perpendicular axis. The closest one wins; ties go to
/// the larger overlap, then to the one whose centre is closest.
pub fn neighbor(rects: &[Rect], idx: usize, dir: Direction) -> Option<usize> {
    let cur = rects.get(idx)?;
    let (cx, cy) = cur.center();

    rects
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != idx)
        .filter_map(|(i, r)| {
            let (distance, overlap) = match dir {
                Direction::Left => (cur.x - r.right(), cur.vertical_overlap(r)),
                Direction::Right => (r.x - cur.right(), cur.vertical_overlap(r)),
                Direction::Up => (cur.y - r.bottom(), cur.horizontal_overlap(r)),
                Direction::Down => (r.y - cur.bottom(), cur.horizontal_overlap(r)),
            };
            if distance < -TOLERANCE.min(cur.w.min(cur.h) / 2) || overlap <= 0 {
                return None;
            }
            let (rx, ry) = r.center();
            let off_center = match dir {
                Direction::Left | Direction::Right => (ry - cy).abs(),
                Direction::Up | Direction::Down => (rx - cx).abs(),
            };
            Some((i, distance.max(0), overlap, off_center))
        })
        .min_by(|a, b| {
            a.1.cmp(&b.1)
                .then(b.2.cmp(&a.2))
                .then(a.3.cmp(&b.3))
                .then(a.0.cmp(&b.0))
        })
        .map(|(i, ..)| i)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{calculate, LayoutKind, LayoutParams, Overrides};

    fn rects(kind: LayoutKind, n: usize, gap: i32) -> Vec<Rect> {
        let p = LayoutParams {
            inner_gap: gap,
            ..LayoutParams::default()
        };
        calculate(kind, Rect::new(0, 0, 1920, 1080), n, &p, &Overrides::default())
    }

    #[test]
    fn vertical_stack_navigation() {
        for gap in [0, 12] {
            let r = rects(LayoutKind::VerticalStack, 3, gap);
            assert_eq!(neighbor(&r, 0, Direction::Right), Some(1));
            assert_eq!(neighbor(&r, 0, Direction::Left), None);
            assert_eq!(neighbor(&r, 1, Direction::Down), Some(2));
            assert_eq!(neighbor(&r, 2, Direction::Up), Some(1));
            assert_eq!(neighbor(&r, 2, Direction::Left), Some(0));
            assert_eq!(neighbor(&r, 1, Direction::Up), None);
        }
    }

    #[test]
    fn columns_navigation() {
        let r = rects(LayoutKind::Columns, 4, 8);
        assert_eq!(neighbor(&r, 1, Direction::Left), Some(0));
        assert_eq!(neighbor(&r, 1, Direction::Right), Some(2));
        assert_eq!(neighbor(&r, 3, Direction::Right), None);
        assert_eq!(neighbor(&r, 2, Direction::Up), None);
    }

    #[test]
    fn grid_navigation() {
        // 2x2 grid, column-major: 0 1 / 2 3 laid out as
        // [0][2]
        // [1][3]
        let r = rects(LayoutKind::Grid, 4, 0);
        assert_eq!(neighbor(&r, 0, Direction::Down), Some(1));
        assert_eq!(neighbor(&r, 0, Direction::Right), Some(2));
        assert_eq!(neighbor(&r, 3, Direction::Left), Some(1));
        assert_eq!(neighbor(&r, 3, Direction::Up), Some(2));
    }

    #[test]
    fn prefers_aligned_window() {
        // Left column full height, right side has three rows.
        let r = vec![
            Rect::new(0, 0, 100, 300),
            Rect::new(100, 0, 100, 100),
            Rect::new(100, 100, 100, 100),
            Rect::new(100, 200, 100, 100),
        ];
        // All three overlap equally; the middle one is centred.
        assert_eq!(neighbor(&r, 0, Direction::Right), Some(2));
        assert_eq!(neighbor(&r, 3, Direction::Left), Some(0));
    }
}
