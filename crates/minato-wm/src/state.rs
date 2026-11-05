//! The window manager's data model, free of Win32 calls so it can be tested.

use minato_layout::{Flip, LayoutKind, Overrides, Rect};

use crate::win32::{Hwnd, MonitorInfo};

/// A list with one focused element.
#[derive(Debug, Clone)]
pub struct Ring<T> {
    items: Vec<T>,
    focused: usize,
}

impl<T> Default for Ring<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            focused: 0,
        }
    }
}

impl<T> Ring<T> {
    pub fn from_vec(items: Vec<T>) -> Self {
        Self { items, focused: 0 }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn focused_idx(&self) -> usize {
        self.focused
    }

    pub fn focused(&self) -> Option<&T> {
        self.items.get(self.focused)
    }

    pub fn focused_mut(&mut self) -> Option<&mut T> {
        self.items.get_mut(self.focused)
    }

    pub fn focus(&mut self, idx: usize) {
        if idx < self.items.len() {
            self.focused = idx;
        }
    }

    pub fn get(&self, idx: usize) -> Option<&T> {
        self.items.get(idx)
    }

    pub fn get_mut(&mut self, idx: usize) -> Option<&mut T> {
        self.items.get_mut(idx)
    }

    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.items.iter()
    }

    pub fn push(&mut self, item: T) {
        self.items.push(item);
    }

    pub fn insert(&mut self, idx: usize, item: T) {
        let idx = idx.min(self.items.len());
        self.items.insert(idx, item);
        if idx <= self.focused && self.items.len() > 1 {
            self.focused += 1;
        }
    }

    /// Removes an element, keeping focus on the same element when possible,
    /// otherwise on its predecessor.
    pub fn remove(&mut self, idx: usize) -> Option<T> {
        if idx >= self.items.len() {
            return None;
        }
        let item = self.items.remove(idx);
        if idx < self.focused || (idx == self.focused && self.focused >= self.items.len()) {
            self.focused = self.focused.saturating_sub(1);
        }
        Some(item)
    }

    pub fn swap(&mut self, a: usize, b: usize) {
        if a < self.items.len() && b < self.items.len() {
            self.items.swap(a, b);
        }
    }

    pub fn position(&self, f: impl FnMut(&T) -> bool) -> Option<usize> {
        self.items.iter().position(f)
    }

    pub fn truncate(&mut self, len: usize) {
        self.items.truncate(len);
        if self.focused >= len {
            self.focused = len.saturating_sub(1);
        }
    }
}

/// One tile. More than one window means a stack; only the focused window
/// of a stack is visible.
#[derive(Debug, Clone, Default)]
pub struct Container {
    pub windows: Ring<Hwnd>,
}

impl Container {
    pub fn new(hwnd: Hwnd) -> Self {
        Self {
            windows: Ring::from_vec(vec![hwnd]),
        }
    }

    pub fn focused_window(&self) -> Option<Hwnd> {
        self.windows.focused().copied()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Tiled { container: usize, window: usize },
    Floating(usize),
}

#[derive(Debug, Clone)]
pub struct Workspace {
    pub name: String,
    pub containers: Ring<Container>,
    pub floating: Vec<Hwnd>,
    pub layout: LayoutKind,
    pub flip: Flip,
    pub overrides: Overrides,
    /// Show only the focused container, filling the work area.
    pub monocle: bool,
    /// Rects from the last retile, indexed like `containers`.
    pub last_rects: Vec<Rect>,
}

impl Workspace {
    pub fn new(name: impl Into<String>, layout: LayoutKind, flip: Flip) -> Self {
        Self {
            name: name.into(),
            containers: Ring::default(),
            floating: Vec::new(),
            layout,
            flip,
            overrides: Overrides::default(),
            monocle: false,
            last_rects: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.containers.is_empty() && self.floating.is_empty()
    }

    pub fn find(&self, hwnd: Hwnd) -> Option<Slot> {
        for (c, container) in self.containers.iter().enumerate() {
            if let Some(w) = container.windows.position(|h| *h == hwnd) {
                return Some(Slot::Tiled {
                    container: c,
                    window: w,
                });
            }
        }
        self.floating.iter().position(|h| *h == hwnd).map(Slot::Floating)
    }

    pub fn windows(&self) -> impl Iterator<Item = Hwnd> + '_ {
        self.containers
            .iter()
            .flat_map(|c| c.windows.iter().copied())
            .chain(self.floating.iter().copied())
    }

    /// Windows that should be on screen while this workspace is active.
    pub fn visible_windows(&self) -> Vec<Hwnd> {
        let mut out: Vec<Hwnd> = if self.monocle {
            self.containers.focused().and_then(Container::focused_window).into_iter().collect()
        } else {
            self.containers.iter().filter_map(Container::focused_window).collect()
        };
        out.extend(&self.floating);
        out
    }

    pub fn focused_tiled_window(&self) -> Option<Hwnd> {
        self.containers.focused().and_then(Container::focused_window)
    }

    /// Adds a window in its own container right after the focused one and
    /// focuses it.
    pub fn add_tiled(&mut self, hwnd: Hwnd) {
        let at = if self.containers.is_empty() {
            0
        } else {
            self.containers.focused_idx() + 1
        };
        self.containers.insert(at, Container::new(hwnd));
        self.containers.focus(at);
    }

    /// Removes the window from wherever it is. Empty containers are dropped.
    pub fn remove(&mut self, hwnd: Hwnd) -> Option<Slot> {
        let slot = self.find(hwnd)?;
        match slot {
            Slot::Tiled { container, window } => {
                let c = self.containers.get_mut(container).unwrap();
                c.windows.remove(window);
                if c.windows.is_empty() {
                    self.containers.remove(container);
                }
            }
            Slot::Floating(i) => {
                self.floating.remove(i);
            }
        }
        Some(slot)
    }

    /// Focuses whatever holds `hwnd`. Returns `false` if it isn't here.
    pub fn focus_window(&mut self, hwnd: Hwnd) -> bool {
        match self.find(hwnd) {
            Some(Slot::Tiled { container, window }) => {
                self.containers.focus(container);
                self.containers.get_mut(container).unwrap().windows.focus(window);
                true
            }
            Some(Slot::Floating(_)) => true,
            None => false,
        }
    }

    /// Moves the focused window of container `from` into container `to`'s
    /// stack, focusing it there.
    pub fn stack_into(&mut self, from: usize, to: usize) {
        if from == to || from >= self.containers.len() || to >= self.containers.len() {
            return;
        }
        let src = self.containers.get_mut(from).unwrap();
        let idx = src.windows.focused_idx();
        let Some(hwnd) = src.windows.remove(idx) else {
            return;
        };
        let emptied = src.windows.is_empty();
        let dst = self.containers.get_mut(to).unwrap();
        dst.windows.push(hwnd);
        let last = dst.windows.len() - 1;
        dst.windows.focus(last);
        let mut to = to;
        if emptied {
            self.containers.remove(from);
            if from < to {
                to -= 1;
            }
        }
        self.containers.focus(to);
    }

    /// Splits the focused window out of its stack into a new container.
    pub fn unstack_focused(&mut self) -> bool {
        let ci = self.containers.focused_idx();
        let Some(c) = self.containers.get_mut(ci) else {
            return false;
        };
        if c.windows.len() < 2 {
            return false;
        }
        let wi = c.windows.focused_idx();
        let hwnd = c.windows.remove(wi).unwrap();
        self.containers.insert(ci + 1, Container::new(hwnd));
        self.containers.focus(ci + 1);
        true
    }
}

#[derive(Debug, Clone)]
pub struct Monitor {
    pub info: MonitorInfo,
    pub workspaces: Ring<Workspace>,
}

impl Monitor {
    pub fn workspace(&self) -> &Workspace {
        self.workspaces.focused().expect("monitor has no workspaces")
    }

    pub fn workspace_mut(&mut self) -> &mut Workspace {
        self.workspaces.focused_mut().expect("monitor has no workspaces")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(n: isize) -> Hwnd {
        Hwnd(n)
    }

    fn ws() -> Workspace {
        Workspace::new("1", LayoutKind::Bsp, Flip::default())
    }

    #[test]
    fn ring_remove_keeps_focus() {
        let mut r = Ring::from_vec(vec![1, 2, 3, 4]);
        r.focus(2);
        r.remove(0);
        assert_eq!(r.focused(), Some(&3));
        r.remove(1); // remove the focused element (3)
        assert_eq!(r.focused(), Some(&4));
        r.remove(1);
        assert_eq!(r.focused(), Some(&2));
        r.remove(0);
        assert!(r.focused().is_none());
    }

    #[test]
    fn ring_insert_before_focus_shifts_it() {
        let mut r = Ring::from_vec(vec![1, 2]);
        r.focus(1);
        r.insert(0, 0);
        assert_eq!(r.focused(), Some(&2));
    }

    #[test]
    fn add_after_focused() {
        let mut w = ws();
        w.add_tiled(h(1));
        w.add_tiled(h(2));
        w.containers.focus(0);
        w.add_tiled(h(3));
        let order: Vec<_> = w.windows().collect();
        assert_eq!(order, vec![h(1), h(3), h(2)]);
        assert_eq!(w.focused_tiled_window(), Some(h(3)));
    }

    #[test]
    fn stack_and_unstack() {
        let mut w = ws();
        for i in 1..=3 {
            w.add_tiled(h(i));
        }
        // Stack window 1 (container 0) into container 1.
        w.containers.focus(0);
        w.stack_into(0, 1);
        assert_eq!(w.containers.len(), 2);
        assert_eq!(w.containers.focused_idx(), 0);
        assert_eq!(w.focused_tiled_window(), Some(h(1)));
        assert_eq!(w.containers.get(0).unwrap().windows.len(), 2);
        // Only the focused window of the stack is visible.
        assert_eq!(w.visible_windows(), vec![h(1), h(3)]);

        assert!(w.unstack_focused());
        assert_eq!(w.containers.len(), 3);
        assert_eq!(w.focused_tiled_window(), Some(h(1)));
        assert!(!w.unstack_focused());
    }

    #[test]
    fn remove_drops_empty_containers() {
        let mut w = ws();
        w.add_tiled(h(1));
        w.add_tiled(h(2));
        w.floating.push(h(9));
        assert_eq!(w.remove(h(1)), Some(Slot::Tiled { container: 0, window: 0 }));
        assert_eq!(w.containers.len(), 1);
        assert_eq!(w.remove(h(9)), Some(Slot::Floating(0)));
        assert_eq!(w.remove(h(9)), None);
    }

    #[test]
    fn monocle_shows_only_focused() {
        let mut w = ws();
        w.add_tiled(h(1));
        w.add_tiled(h(2));
        w.floating.push(h(5));
        w.monocle = true;
        assert_eq!(w.visible_windows(), vec![h(2), h(5)]);
    }
}
