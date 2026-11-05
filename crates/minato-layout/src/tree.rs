use std::collections::HashMap;

use crate::{Axis, Direction, Flip, LayoutKind, LayoutParams, Rect, Sizing, MAX_RATIO, MIN_RATIO};

/// Identifies a split inside a layout tree: the root is `1`, the first child
/// of split `id` is `2 * id`, the second is `2 * id + 1`. Ids that would
/// overflow are `0` and never receive overrides.
pub type SplitId = u64;

/// User adjustments to split ratios, keyed by [`SplitId`]. Each value is the
/// share of the split's extent given to its first child.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Overrides(HashMap<SplitId, f32>);

impl Overrides {
    pub fn get(&self, id: SplitId) -> Option<f32> {
        self.0.get(&id).copied()
    }

    pub fn set(&mut self, id: SplitId, ratio: f32) {
        if id != 0 {
            self.0.insert(id, ratio.clamp(MIN_RATIO, MAX_RATIO));
        }
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

enum Node {
    Leaf(usize),
    Split {
        axis: Axis,
        ratio: f32,
        a: Box<Node>,
        b: Box<Node>,
    },
}

fn split(axis: Axis, ratio: f32, a: Node, b: Node) -> Node {
    Node::Split {
        axis,
        ratio,
        a: Box::new(a),
        b: Box::new(b),
    }
}

/// Divides the area equally between `nodes` along `axis`.
fn chain(axis: Axis, mut nodes: Vec<Node>) -> Node {
    debug_assert!(!nodes.is_empty());
    if nodes.len() == 1 {
        return nodes.pop().unwrap();
    }
    let len = nodes.len();
    let first = nodes.remove(0);
    split(axis, 1.0 / len as f32, first, chain(axis, nodes))
}

fn leaves(range: std::ops::Range<usize>) -> Vec<Node> {
    range.map(Node::Leaf).collect()
}

fn bsp(start: usize, n: usize, depth: usize) -> Node {
    if start + 1 >= n {
        return Node::Leaf(start);
    }
    let axis = if depth.is_multiple_of(2) { Axis::X } else { Axis::Y };
    split(axis, 0.5, Node::Leaf(start), bsp(start + 1, n, depth + 1))
}

fn grid(n: usize) -> Node {
    let cols = (n as f64).sqrt().ceil() as usize;
    let base = n / cols;
    let extra = n % cols;
    let mut next = 0;
    let columns = (0..cols)
        .map(|c| {
            // Later columns take the leftover windows, so the first window
            // keeps a full-height column when the grid isn't square.
            let count = base + usize::from(c >= cols - extra);
            let col = chain(Axis::Y, leaves(next..next + count));
            next += count;
            col
        })
        .collect();
    chain(Axis::X, columns)
}

fn build(kind: LayoutKind, n: usize, primary: f32) -> Node {
    if n == 1 {
        return Node::Leaf(0);
    }
    let primary = primary.clamp(MIN_RATIO, MAX_RATIO);
    match kind {
        LayoutKind::Bsp => bsp(0, n, 0),
        LayoutKind::Columns => chain(Axis::X, leaves(0..n)),
        LayoutKind::Rows => chain(Axis::Y, leaves(0..n)),
        LayoutKind::VerticalStack => split(
            Axis::X,
            primary,
            Node::Leaf(0),
            chain(Axis::Y, leaves(1..n)),
        ),
        LayoutKind::RightMainVerticalStack => split(
            Axis::X,
            1.0 - primary,
            chain(Axis::Y, leaves(1..n)),
            Node::Leaf(0),
        ),
        LayoutKind::HorizontalStack => split(
            Axis::Y,
            primary,
            Node::Leaf(0),
            chain(Axis::X, leaves(1..n)),
        ),
        LayoutKind::UltrawideVerticalStack => {
            if n == 2 {
                split(Axis::X, primary, Node::Leaf(0), Node::Leaf(1))
            } else {
                // [ secondary | primary | stack ], primary takes `primary`
                // of the full width and the sides share the rest.
                let side = (1.0 - primary) / 2.0;
                split(
                    Axis::X,
                    side,
                    Node::Leaf(1),
                    split(
                        Axis::X,
                        primary / (1.0 - side),
                        Node::Leaf(0),
                        chain(Axis::Y, leaves(2..n)),
                    ),
                )
            }
        }
        LayoutKind::Grid => grid(n),
    }
}

/// One split on the way from the root to a leaf.
#[derive(Debug, Clone, Copy)]
struct PathStep {
    id: SplitId,
    axis: Axis,
    /// The leaf is inside the split's first child.
    in_first: bool,
    /// The first child occupies the left (X) / top (Y) part.
    first_is_low: bool,
    /// Width (X) or height (Y) of the split's rect.
    extent: i32,
    /// Effective ratio of the first child.
    ratio: f32,
}

/// The computed placement of `n` containers in an area.
#[derive(Debug, Clone)]
pub struct Arrangement {
    /// Final rect for each container index, gaps applied.
    pub rects: Vec<Rect>,
    paths: Vec<Vec<PathStep>>,
}

impl Arrangement {
    pub fn new(
        kind: LayoutKind,
        area: Rect,
        n: usize,
        params: &LayoutParams,
        overrides: &Overrides,
    ) -> Self {
        if n == 0 {
            return Self {
                rects: Vec::new(),
                paths: Vec::new(),
            };
        }

        let tree = build(kind, n, params.primary_ratio);
        let mut raw = vec![Rect::default(); n];
        let mut paths = vec![Vec::new(); n];
        let mut ctx = Ctx {
            flip: params.flip,
            overrides,
            rects: &mut raw,
            paths: &mut paths,
        };
        ctx.place(&tree, area, 1, &mut Vec::new());

        let rects = raw
            .iter()
            .map(|r| apply_gap(*r, area, params.inner_gap))
            .collect();
        Self { rects, paths }
    }

    /// Moves the `dir` edge of container `idx` by `delta` pixels, growing it
    /// for [`Sizing::Increase`] and shrinking it for [`Sizing::Decrease`].
    ///
    /// The change is written to `overrides`; recompute the arrangement to see
    /// it. Returns `false` when that edge lies on the area boundary.
    pub fn resize(
        &self,
        idx: usize,
        dir: Direction,
        sizing: Sizing,
        delta: i32,
        overrides: &mut Overrides,
    ) -> bool {
        let Some(path) = self.paths.get(idx) else {
            return false;
        };
        let towards_high = matches!(dir, Direction::Right | Direction::Down);

        for step in path.iter().rev() {
            if step.axis != dir.axis() || step.extent <= 0 {
                continue;
            }
            let leaf_is_low = step.in_first == step.first_is_low;
            if leaf_is_low != towards_high {
                continue;
            }
            let d = delta as f32 / step.extent as f32;
            let leaf_delta = match sizing {
                Sizing::Increase => d,
                Sizing::Decrease => -d,
            };
            let first_delta = if step.in_first { leaf_delta } else { -leaf_delta };
            overrides.set(step.id, step.ratio + first_delta);
            return true;
        }
        false
    }
}

struct Ctx<'a> {
    flip: Flip,
    overrides: &'a Overrides,
    rects: &'a mut [Rect],
    paths: &'a mut [Vec<PathStep>],
}

impl Ctx<'_> {
    fn place(&mut self, node: &Node, rect: Rect, id: SplitId, path: &mut Vec<PathStep>) {
        match node {
            Node::Leaf(i) => {
                self.rects[*i] = rect;
                self.paths[*i] = path.clone();
            }
            Node::Split { axis, ratio, a, b } => {
                let ratio = self
                    .overrides
                    .get(id)
                    .unwrap_or(*ratio)
                    .clamp(MIN_RATIO, MAX_RATIO);
                let (extent, flipped) = match axis {
                    Axis::X => (rect.w, self.flip.horizontal),
                    Axis::Y => (rect.h, self.flip.vertical),
                };
                let a_len = if extent >= 2 {
                    ((extent as f32 * ratio).round() as i32).clamp(1, extent - 1)
                } else {
                    extent
                };
                let b_len = extent - a_len;

                let (low_len, low_is_a) = if flipped { (b_len, false) } else { (a_len, true) };
                let (low, high) = match axis {
                    Axis::X => (
                        Rect::new(rect.x, rect.y, low_len, rect.h),
                        Rect::new(rect.x + low_len, rect.y, extent - low_len, rect.h),
                    ),
                    Axis::Y => (
                        Rect::new(rect.x, rect.y, rect.w, low_len),
                        Rect::new(rect.x, rect.y + low_len, rect.w, extent - low_len),
                    ),
                };
                let (a_rect, b_rect) = if low_is_a { (low, high) } else { (high, low) };

                let child = |n: u64| id.checked_mul(2).and_then(|v| v.checked_add(n)).unwrap_or(0);
                let mut step = PathStep {
                    id,
                    axis: *axis,
                    in_first: true,
                    first_is_low: low_is_a,
                    extent,
                    ratio,
                };

                path.push(step);
                self.place(a, a_rect, if id == 0 { 0 } else { child(0) }, path);
                path.pop();

                step.in_first = false;
                path.push(step);
                self.place(b, b_rect, if id == 0 { 0 } else { child(1) }, path);
                path.pop();
            }
        }
    }
}

/// Shrinks a rect by half the gap on every side that touches another window,
/// so neighbours end up exactly `gap` apart while the outer edges stay put.
fn apply_gap(r: Rect, area: Rect, gap: i32) -> Rect {
    if gap <= 0 {
        return r;
    }
    let lo = gap / 2;
    let hi = gap - lo;
    let left = if r.x > area.x { hi } else { 0 };
    let top = if r.y > area.y { hi } else { 0 };
    let right = if r.right() < area.right() { lo } else { 0 };
    let bottom = if r.bottom() < area.bottom() { lo } else { 0 };
    r.inset_sides(left, top, right, bottom)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calculate;

    const AREA: Rect = Rect::new(100, 50, 1920, 1080);

    fn params() -> LayoutParams {
        LayoutParams::default()
    }

    fn calc(kind: LayoutKind, n: usize) -> Vec<Rect> {
        calculate(kind, AREA, n, &params(), &Overrides::default())
    }

    /// With no gap, every layout must tile the area exactly: rects inside
    /// the area, pairwise disjoint, areas summing to the full area.
    #[test]
    fn every_layout_tiles_exactly() {
        for kind in LayoutKind::ALL {
            for n in 1..=16 {
                let rects = calc(kind, n);
                assert_eq!(rects.len(), n, "{kind:?} n={n}");
                let mut total = 0;
                for (i, r) in rects.iter().enumerate() {
                    assert!(r.w > 0 && r.h > 0, "{kind:?} n={n} #{i} empty: {r:?}");
                    assert!(
                        r.x >= AREA.x
                            && r.y >= AREA.y
                            && r.right() <= AREA.right()
                            && r.bottom() <= AREA.bottom(),
                        "{kind:?} n={n} #{i} outside: {r:?}"
                    );
                    for o in &rects[i + 1..] {
                        assert_eq!(r.intersection_area(o), 0, "{kind:?} n={n} overlap");
                    }
                    total += r.area();
                }
                assert_eq!(total, AREA.area(), "{kind:?} n={n} coverage");
            }
        }
    }

    #[test]
    fn single_window_fills_area() {
        for kind in LayoutKind::ALL {
            assert_eq!(calc(kind, 1), vec![AREA]);
        }
    }

    #[test]
    fn columns_are_equal_width() {
        let rects = calc(LayoutKind::Columns, 3);
        let widths: Vec<_> = rects.iter().map(|r| r.w).collect();
        assert_eq!(widths.iter().sum::<i32>(), 1920);
        assert!(widths.iter().all(|w| (w - 640).abs() <= 1), "{widths:?}");
        assert!(rects.windows(2).all(|p| p[0].x < p[1].x));
    }

    #[test]
    fn vertical_stack_shape() {
        let rects = calc(LayoutKind::VerticalStack, 3);
        assert_eq!(rects[0], Rect::new(100, 50, 960, 1080));
        assert_eq!(rects[1], Rect::new(1060, 50, 960, 540));
        assert_eq!(rects[2], Rect::new(1060, 590, 960, 540));
    }

    #[test]
    fn right_main_puts_primary_right() {
        let rects = calc(LayoutKind::RightMainVerticalStack, 3);
        assert_eq!(rects[0], Rect::new(1060, 50, 960, 1080));
        assert_eq!(rects[1].x, 100);
    }

    #[test]
    fn horizontal_stack_shape() {
        let rects = calc(LayoutKind::HorizontalStack, 3);
        assert_eq!(rects[0], Rect::new(100, 50, 1920, 540));
        assert_eq!(rects[1], Rect::new(100, 590, 960, 540));
        assert_eq!(rects[2], Rect::new(1060, 590, 960, 540));
    }

    #[test]
    fn ultrawide_shape() {
        let rects = calc(LayoutKind::UltrawideVerticalStack, 4);
        assert_eq!(rects[1], Rect::new(100, 50, 480, 1080));
        assert_eq!(rects[0], Rect::new(580, 50, 960, 1080));
        assert_eq!(rects[2], Rect::new(1540, 50, 480, 540));
        assert_eq!(rects[3], Rect::new(1540, 590, 480, 540));
    }

    #[test]
    fn bsp_alternates() {
        let rects = calc(LayoutKind::Bsp, 3);
        assert_eq!(rects[0], Rect::new(100, 50, 960, 1080));
        assert_eq!(rects[1], Rect::new(1060, 50, 960, 540));
        assert_eq!(rects[2], Rect::new(1060, 590, 960, 540));
        let rects = calc(LayoutKind::Bsp, 4);
        assert_eq!(rects[2], Rect::new(1060, 590, 480, 540));
        assert_eq!(rects[3], Rect::new(1540, 590, 480, 540));
    }

    #[test]
    fn grid_three_keeps_first_column_full() {
        let rects = calc(LayoutKind::Grid, 3);
        assert_eq!(rects[0], Rect::new(100, 50, 960, 1080));
        assert_eq!(rects[1].h, 540);
        let rects = calc(LayoutKind::Grid, 4);
        assert!(rects.iter().all(|r| r.w == 960 && r.h == 540));
    }

    #[test]
    fn flip_mirrors() {
        let p = LayoutParams {
            flip: Flip {
                horizontal: true,
                vertical: false,
            },
            ..params()
        };
        let rects = calculate(LayoutKind::VerticalStack, AREA, 2, &p, &Overrides::default());
        assert_eq!(rects[0], Rect::new(1060, 50, 960, 1080));
        assert_eq!(rects[1], Rect::new(100, 50, 960, 1080));

        let p = LayoutParams {
            flip: Flip {
                horizontal: false,
                vertical: true,
            },
            ..params()
        };
        let rects = calculate(LayoutKind::HorizontalStack, AREA, 2, &p, &Overrides::default());
        assert_eq!(rects[0].y, 590);
    }

    #[test]
    fn gaps_separate_neighbours_but_not_outer_edges() {
        let p = LayoutParams {
            inner_gap: 10,
            ..params()
        };
        let rects = calculate(LayoutKind::Columns, AREA, 2, &p, &Overrides::default());
        assert_eq!(rects[0], Rect::new(100, 50, 955, 1080));
        assert_eq!(rects[1], Rect::new(1065, 50, 955, 1080));
        assert_eq!(rects[1].x - rects[0].right(), 10);
    }

    #[test]
    fn resize_moves_shared_edge() {
        let mut ov = Overrides::default();
        let p = params();
        let arr = Arrangement::new(LayoutKind::VerticalStack, AREA, 3, &p, &ov);
        // Grow the primary's right edge by 192px (10%).
        assert!(arr.resize(0, Direction::Right, Sizing::Increase, 192, &mut ov));
        let rects = calculate(LayoutKind::VerticalStack, AREA, 3, &p, &ov);
        assert_eq!(rects[0].w, 1152);
        assert_eq!(rects[1].x, 100 + 1152);

        // Shrinking the stack's left edge moves the same boundary back.
        let arr = Arrangement::new(LayoutKind::VerticalStack, AREA, 3, &p, &ov);
        assert!(arr.resize(1, Direction::Left, Sizing::Decrease, 192, &mut ov));
        let rects = calculate(LayoutKind::VerticalStack, AREA, 3, &p, &ov);
        assert_eq!(rects[0].w, 1344);
    }

    #[test]
    fn resize_inside_stack_and_on_boundary() {
        let mut ov = Overrides::default();
        let p = params();
        let arr = Arrangement::new(LayoutKind::VerticalStack, AREA, 3, &p, &ov);
        // The primary's left edge is the screen edge: nothing to move.
        assert!(!arr.resize(0, Direction::Left, Sizing::Increase, 50, &mut ov));
        assert!(ov.is_empty());
        // Window 1's bottom edge is shared with window 2.
        assert!(arr.resize(1, Direction::Down, Sizing::Increase, 108, &mut ov));
        let rects = calculate(LayoutKind::VerticalStack, AREA, 3, &p, &ov);
        assert_eq!(rects[1].h, 648);
        assert_eq!(rects[2].y, 50 + 648);
    }

    #[test]
    fn resize_respects_flip() {
        let mut ov = Overrides::default();
        let p = LayoutParams {
            flip: Flip {
                horizontal: true,
                vertical: false,
            },
            ..params()
        };
        let arr = Arrangement::new(LayoutKind::VerticalStack, AREA, 2, &p, &ov);
        // Primary is on the right now, so its movable edge is the left one.
        assert!(!arr.resize(0, Direction::Right, Sizing::Increase, 100, &mut ov));
        assert!(arr.resize(0, Direction::Left, Sizing::Increase, 192, &mut ov));
        let rects = calculate(LayoutKind::VerticalStack, AREA, 2, &p, &ov);
        assert_eq!(rects[0].w, 1152);
        assert_eq!(rects[0].right(), AREA.right());
    }

    #[test]
    fn ratios_are_clamped() {
        let mut ov = Overrides::default();
        let p = params();
        for _ in 0..50 {
            let arr = Arrangement::new(LayoutKind::Columns, AREA, 2, &p, &ov);
            arr.resize(0, Direction::Right, Sizing::Increase, 500, &mut ov);
        }
        let rects = calculate(LayoutKind::Columns, AREA, 2, &p, &ov);
        assert_eq!(rects[0].w, (1920.0 * MAX_RATIO).round() as i32);
    }

    #[test]
    fn zero_windows() {
        assert!(calc(LayoutKind::Bsp, 0).is_empty());
    }
}
