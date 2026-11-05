//! Pure tiling layout calculations for MinatoWM.
//!
//! Every layout is expressed as a binary split tree. Each split divides its
//! rect along one axis with a ratio; leaves are container indices. This one
//! representation gives us:
//!
//! * rect calculation for every layout,
//! * flipping (mirroring the side each split's first child goes on),
//! * edge resizing (adjust the nearest ancestor split whose boundary is the
//!   edge being moved), stored as per-split ratio overrides.
//!
//! Directional neighbours are resolved geometrically from the final rects,
//! so they work the same for every layout.

mod neighbor;
mod rect;
mod tree;

pub use neighbor::neighbor;
pub use rect::Rect;
pub use tree::{Arrangement, Overrides, SplitId};

use serde::{Deserialize, Serialize};

/// Smallest/largest ratio a split may have.
pub const MIN_RATIO: f32 = 0.05;
pub const MAX_RATIO: f32 = 0.95;
pub const DEFAULT_PRIMARY_RATIO: f32 = 0.5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    pub const ALL: [Direction; 4] = [
        Direction::Left,
        Direction::Right,
        Direction::Up,
        Direction::Down,
    ];

    pub fn opposite(self) -> Self {
        match self {
            Direction::Left => Direction::Right,
            Direction::Right => Direction::Left,
            Direction::Up => Direction::Down,
            Direction::Down => Direction::Up,
        }
    }

    pub fn axis(self) -> Axis {
        match self {
            Direction::Left | Direction::Right => Axis::X,
            Direction::Up | Direction::Down => Axis::Y,
        }
    }
}

/// The axis a split divides along. `X` puts its children side by side,
/// `Y` stacks them on top of each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Axis {
    X,
    Y,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sizing {
    Increase,
    Decrease,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutKind {
    /// Recursive halving, alternating between side-by-side and stacked.
    #[default]
    Bsp,
    /// Equal-width columns.
    Columns,
    /// Equal-height rows.
    Rows,
    /// Primary window on the left, the rest stacked on the right.
    VerticalStack,
    /// Primary window on the right, the rest stacked on the left.
    RightMainVerticalStack,
    /// Primary window on top, the rest side by side below.
    HorizontalStack,
    /// Primary in the centre, the second window on the left, the rest
    /// stacked on the right. Meant for ultrawide monitors.
    UltrawideVerticalStack,
    /// Roughly square grid, filled column by column.
    Grid,
}

impl LayoutKind {
    pub const ALL: [LayoutKind; 8] = [
        LayoutKind::Bsp,
        LayoutKind::Columns,
        LayoutKind::Rows,
        LayoutKind::VerticalStack,
        LayoutKind::RightMainVerticalStack,
        LayoutKind::HorizontalStack,
        LayoutKind::UltrawideVerticalStack,
        LayoutKind::Grid,
    ];

    pub fn label(self) -> &'static str {
        match self {
            LayoutKind::Bsp => "BSP",
            LayoutKind::Columns => "Columns",
            LayoutKind::Rows => "Rows",
            LayoutKind::VerticalStack => "Vertical Stack",
            LayoutKind::RightMainVerticalStack => "Right-Main Vertical Stack",
            LayoutKind::HorizontalStack => "Horizontal Stack",
            LayoutKind::UltrawideVerticalStack => "Ultrawide Vertical Stack",
            LayoutKind::Grid => "Grid",
        }
    }

    /// The next layout in [`LayoutKind::ALL`], wrapping around.
    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|k| *k == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    pub fn previous(self) -> Self {
        let i = Self::ALL.iter().position(|k| *k == self).unwrap_or(0);
        Self::ALL[(i + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

/// Mirrors a layout. `horizontal` swaps left/right, `vertical` swaps top/bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Flip {
    #[serde(default)]
    pub horizontal: bool,
    #[serde(default)]
    pub vertical: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutParams {
    /// Share of the area given to the primary window in stack layouts.
    pub primary_ratio: f32,
    /// Gap between adjacent windows, in pixels.
    pub inner_gap: i32,
    pub flip: Flip,
}

impl Default for LayoutParams {
    fn default() -> Self {
        Self {
            primary_ratio: DEFAULT_PRIMARY_RATIO,
            inner_gap: 0,
            flip: Flip::default(),
        }
    }
}

/// Computes the rects for `n` containers. Convenience wrapper over
/// [`Arrangement::new`].
pub fn calculate(
    kind: LayoutKind,
    area: Rect,
    n: usize,
    params: &LayoutParams,
    overrides: &Overrides,
) -> Vec<Rect> {
    Arrangement::new(kind, area, n, params, overrides).rects
}
