//! Screens and workspaces (10.0): pure model, no Wayland types (the output handle is generic).
//!
//! Every connected monitor is a screen with nine workspaces of its own; each workspace is a
//! `Ring` (open tiles + the minimised grid, see orbit.rs). Screens sit side by side, the primary
//! (the built-in panel, else the first) on the left. With a second screen connected, new apps
//! open there by default (`[display] open_apps_on = "external"`); dialogs follow their parent.
use crate::orbit::Ring;

pub const WORKSPACES: usize = 9;

pub struct Screen<O> {
    pub out: O,
    pub name: String,
    /// the laptop's own panel (eDP / LVDS / DSI)
    pub internal: bool,
    /// position and size in the global logical space
    pub loc: (f64, f64),
    pub size: (f64, f64),
    pub ws: Vec<Ring>,
    pub active: usize,
    /// scroll offset of the minimised grid, logical px
    pub grid_scroll: f64,
}

impl<O> Screen<O> {
    pub fn new(out: O, name: &str, size: (f64, f64)) -> Self {
        let n = name.to_ascii_uppercase();
        Screen {
            out,
            name: name.to_string(),
            internal: n.starts_with("EDP") || n.starts_with("LVDS") || n.starts_with("DSI"),
            loc: (0.0, 0.0),
            size,
            ws: (0..WORKSPACES).map(|_| Ring::default()).collect(),
            active: 0,
            grid_scroll: 0.0,
        }
    }
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.loc.0 && x < self.loc.0 + self.size.0 && y >= self.loc.1 && y < self.loc.1 + self.size.1
    }
    pub fn ring(&self) -> &Ring {
        &self.ws[self.active]
    }
}

pub struct Desk<O> {
    pub screens: Vec<Screen<O>>,
    /// the screen with the keyboard (Mod+… acts here)
    pub focused: usize,
    empty: Ring,
}

impl<O> Default for Desk<O> {
    fn default() -> Self {
        Desk { screens: vec![], focused: 0, empty: Ring::default() }
    }
}

impl<O> Desk<O> {
    pub fn ring(&self, si: usize) -> &Ring {
        self.screens.get(si).map(|s| s.ring()).unwrap_or(&self.empty)
    }
    pub fn ring_mut(&mut self, si: usize) -> &mut Ring {
        match self.screens.get_mut(si) {
            Some(s) => &mut s.ws[s.active],
            None => &mut self.empty,
        }
    }
    /// (screen, workspace) holding window `id`
    pub fn locate(&self, id: u64) -> Option<(usize, usize)> {
        for (si, s) in self.screens.iter().enumerate() {
            for (wi, r) in s.ws.iter().enumerate() {
                if r.all().any(|x| *x == id) {
                    return Some((si, wi));
                }
            }
        }
        None
    }
    /// visible: on the active workspace of its screen
    pub fn visible(&self, id: u64) -> bool {
        self.locate(id).is_some_and(|(si, wi)| self.screens[si].active == wi)
    }
    pub fn remove(&mut self, id: u64) {
        for s in &mut self.screens {
            for r in &mut s.ws {
                r.remove(id);
            }
        }
    }
    /// the main screen: the built-in panel, else the leftmost
    pub fn primary(&self) -> usize {
        self.screens.iter().position(|s| s.internal).unwrap_or(0)
    }
    /// where a new app opens: "external" (default: the second screen when one is connected),
    /// "focused" or "primary"
    pub fn open_on(&self, policy: &str) -> usize {
        if self.screens.len() < 2 {
            return 0;
        }
        match policy {
            "focused" => self.focused.min(self.screens.len() - 1),
            "primary" => self.primary(),
            _ => {
                let p = self.primary();
                (0..self.screens.len()).find(|i| *i != p).unwrap_or(p)
            }
        }
    }
    pub fn screen_at(&self, x: f64, y: f64) -> Option<usize> {
        self.screens.iter().position(|s| s.contains(x, y))
    }
    /// the point clamped onto the nearest screen
    pub fn clamp(&self, x: f64, y: f64) -> (f64, f64) {
        if self.screens.is_empty() || self.screen_at(x, y).is_some() {
            return (x, y);
        }
        let mut best = (f64::MAX, (x, y));
        for s in &self.screens {
            let cx = x.clamp(s.loc.0, s.loc.0 + s.size.0 - 1.0);
            let cy = y.clamp(s.loc.1, s.loc.1 + s.size.1 - 1.0);
            let d = (cx - x).powi(2) + (cy - y).powi(2);
            if d < best.0 {
                best = (d, (cx, cy));
            }
        }
        best.1
    }
    /// screens left to right, the primary first; returns true if anything moved
    pub fn arrange(&mut self) -> bool {
        let p = self.primary();
        let mut order: Vec<usize> = (0..self.screens.len()).collect();
        order.sort_by_key(|i| (*i != p, *i));
        let mut x = 0.0;
        let mut moved = false;
        for i in order {
            let s = &mut self.screens[i];
            if s.loc != (x, 0.0) {
                s.loc = (x, 0.0);
                moved = true;
            }
            x += s.size.0;
        }
        moved
    }
    /// Show workspace `n` (0-based) on screen `si`. false if it already was.
    pub fn switch(&mut self, si: usize, n: usize) -> bool {
        match self.screens.get_mut(si) {
            Some(s) if n < s.ws.len() && s.active != n => {
                s.active = n;
                s.grid_scroll = 0.0;
                true
            }
            _ => false,
        }
    }
    /// Move window `id` to workspace `ws` of screen `si`, as an open app with the keyboard there.
    /// Returns the window that had to be minimised to make room (past `max_open`).
    pub fn move_to(&mut self, id: u64, si: usize, ws: usize, max_open: usize) -> Option<u64> {
        if si >= self.screens.len() || ws >= WORKSPACES || self.locate(id) == Some((si, ws)) && self.screens[si].ws[ws].order.contains(&id) {
            return None;
        }
        self.remove(id);
        self.screens[si].ws[ws].spawn(id, max_open)
    }
    /// A monitor was unplugged: its windows move to the primary screen's active workspace (open
    /// ones stay open while there is room, the rest join the minimised grid).
    pub fn remove_screen(&mut self, si: usize, max_open: usize) -> Option<Screen<O>> {
        if si >= self.screens.len() {
            return None;
        }
        let gone = self.screens.remove(si);
        if self.screens.is_empty() {
            self.focused = 0;
            return Some(gone);
        }
        let to = self.primary();
        let ring = self.ring_mut(to);
        for r in &gone.ws {
            for id in r.order.iter().rev() {
                if !ring.order.contains(id) && !ring.min.contains(id) {
                    if ring.order.len() < max_open.max(1) {
                        ring.order.push(*id);
                    } else {
                        ring.min.push(*id);
                    }
                }
            }
            for id in &r.min {
                if !ring.order.contains(id) && !ring.min.contains(id) {
                    ring.min.push(*id);
                }
            }
        }
        if ring.focus.is_none() {
            ring.focus = ring.order.first().copied();
        }
        if self.focused >= self.screens.len() || self.focused == si {
            self.focused = to;
        } else if self.focused > si {
            self.focused -= 1;
        }
        self.arrange();
        Some(gone)
    }
    /// every window id with the screen and workspace it lives on
    pub fn all(&self) -> Vec<(u64, usize, usize)> {
        let mut v = vec![];
        for (si, s) in self.screens.iter().enumerate() {
            for (wi, r) in s.ws.iter().enumerate() {
                v.extend(r.all().map(|id| (*id, si, wi)));
            }
        }
        v
    }
}

#[cfg(test)]
mod t {
    use super::*;

    fn two() -> Desk<()> {
        let mut d = Desk::default();
        d.screens.push(Screen::new((), "HDMI-A-1", (1920.0, 1080.0)));
        d.screens.push(Screen::new((), "eDP-1", (1280.0, 800.0)));
        d.arrange();
        d
    }

    #[test]
    fn the_panel_is_primary_and_apps_open_on_the_other_screen() {
        let d = two();
        assert_eq!(d.primary(), 1, "the built-in panel is the primary screen");
        assert_eq!(d.screens[1].loc, (0.0, 0.0), "and sits on the left");
        assert_eq!(d.screens[0].loc, (1280.0, 0.0));
        assert_eq!(d.open_on("external"), 0, "a new app opens on the second screen");
        assert_eq!(d.open_on("primary"), 1);
        let mut one: Desk<()> = Desk::default();
        one.screens.push(Screen::new((), "eDP-1", (1280.0, 800.0)));
        assert_eq!(one.open_on("external"), 0, "one screen: there");
        assert_eq!(d.screen_at(1300.0, 10.0), Some(0));
        assert_eq!(d.screen_at(10.0, 900.0), None, "below the shorter panel is off-screen");
        assert_eq!(d.clamp(10.0, 900.0), (10.0, 799.0));
    }

    #[test]
    fn workspaces_keep_their_windows() {
        let mut d = two();
        d.ring_mut(0).spawn(1, 4);
        d.ring_mut(0).spawn(2, 4);
        assert!(d.switch(0, 3));
        assert!(!d.switch(0, 3), "already there");
        assert!(d.ring(0).order.is_empty(), "workspace 4 starts empty");
        assert!(!d.visible(1) && d.locate(1) == Some((0, 0)));
        d.ring_mut(0).spawn(3, 4);
        assert_eq!(d.move_to(2, 0, 3, 4), None);
        assert_eq!(d.ring(0).order, vec![2, 3], "moved in, with the keyboard");
        assert_eq!(d.screens[0].ws[0].order, vec![1]);
        d.switch(0, 0);
        assert!(d.visible(1) && !d.visible(2));
        // to the other screen
        d.move_to(1, 1, 0, 4);
        assert_eq!(d.locate(1), Some((1, 0)));
        assert_eq!(d.ring(1).center(), Some(1));
        d.remove(3);
        assert_eq!(d.locate(3), None);
    }

    #[test]
    fn unplugging_a_screen_keeps_every_window() {
        let mut d = two();
        for id in 1..=3 {
            d.ring_mut(0).spawn(id, 4);
        }
        d.ring_mut(0).minimize(2);
        d.switch(0, 5);
        d.ring_mut(0).spawn(9, 4);
        d.ring_mut(1).spawn(7, 4);
        d.focused = 0;
        d.remove_screen(0, 3);
        assert_eq!(d.screens.len(), 1);
        assert_eq!(d.focused, 0);
        let mut ids: Vec<u64> = d.all().into_iter().map(|x| x.0).collect();
        ids.sort();
        assert_eq!(ids, vec![1, 2, 3, 7, 9], "nothing is lost");
        assert!(d.ring(0).order.len() <= 3, "past max_open the rest is minimised");
        assert!(d.ring(0).min.contains(&2), "minimised stays minimised");
        assert_eq!(d.screens[0].loc, (0.0, 0.0));
    }
}
