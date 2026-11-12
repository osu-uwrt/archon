//! Tiled panes, like vim windows: the main area splits side by side or stacked, every pane shows one view, and the
//! dividers drag.

use archon_core::source::SourceId;
use egui::{CursorIcon, Rect, Sense, Stroke, Ui, UiBuilder, pos2};
use serde::{Deserialize, Serialize};

/// What a pane shows
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    /// A source's output, or a topic's inspector
    Source(SourceId),
    AllLogs,
    Topics,
    /// The form for publisher number this
    Publish(usize),
    /// Plot number this
    Plot(usize),
    /// What archon is and how to start, with the first steps as buttons
    Welcome,
    /// Writing a robot file
    RobotBuilder,
    /// The nodes on the graph and what each one has
    Nodes,
    /// The bench test running, or the last one
    Test,
    /// Each board's health, from its FirmwareStatus
    Boards,
    /// The paths archon uses, and setting them
    Settings,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum Tile {
    Pane(usize),
    Split {
        /// Side by side, or one above the other
        side_by_side: bool,
        /// The share of the first tile
        ratio: f32,
        first: Box<Tile>,
        second: Box<Tile>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// Room between panes, which is also the divider's grab area
const GAP: f32 = 6.0;
/// Views each pane remembers for going back
const HISTORY: usize = 20;

pub struct Layout {
    root: Tile,
    /// One per pane, indexed by the numbers in the tiles
    views: Vec<View>,
    /// What each pane showed before, for going back
    history: Vec<Vec<View>>,
    active: usize,
    /// The pane active before, where "open beside" goes
    previous: Option<usize>,
    /// Where each pane was drawn last frame, for moving focus by direction
    rects: Vec<Rect>,
}

struct Divider {
    /// Which split, as the turns from the root: `false` for the first tile, `true` for the second
    path: Vec<bool>,
    handle: Rect,
    /// The area the split divides
    area: Rect,
    side_by_side: bool,
}

impl Layout {
    pub fn new(view: View) -> Self {
        Self {
            root: Tile::Pane(0),
            views: vec![view],
            history: vec![Vec::new()],
            active: 0,
            previous: None,
            rects: Vec::new(),
        }
    }

    /// A layout from saved parts, if they fit together: every pane in the tiles exactly once
    pub fn from_parts(root: Tile, views: Vec<View>, active: usize) -> Option<Self> {
        let mut panes = Vec::new();
        collect_panes(&root, &mut panes);
        panes.sort_unstable();
        let valid = panes == (0..views.len()).collect::<Vec<_>>() && active < views.len();
        let history = vec![Vec::new(); views.len()];
        valid.then(|| Self { root, views, history, active, previous: None, rects: Vec::new() })
    }

    pub fn root(&self) -> &Tile {
        &self.root
    }

    pub fn views(&self) -> &[View] {
        &self.views
    }

    pub fn active(&self) -> usize {
        self.active
    }

    pub fn active_view(&self) -> View {
        self.views[self.active]
    }

    /// Shows a view in a pane, remembering what it showed so `back` can return to it
    pub fn set_view(&mut self, pane: usize, view: View) {
        let old = std::mem::replace(&mut self.views[pane], view);
        if old != view {
            let history = &mut self.history[pane];
            history.retain(|v| *v != view);
            history.push(old);
            if history.len() > HISTORY {
                history.remove(0);
            }
        }
    }

    pub fn set_active_view(&mut self, view: View) {
        self.set_view(self.active, view);
    }

    /// Shows `with` wherever `gone` was shown, and forgets `gone` in every pane's history
    pub fn replace_everywhere(&mut self, gone: View, with: View) {
        for view in &mut self.views {
            if *view == gone {
                *view = with;
            }
        }
        for history in &mut self.history {
            history.retain(|v| *v != gone);
        }
    }

    /// Returns the active pane to what it showed before. False when there's nothing to go back to.
    pub fn back(&mut self) -> bool {
        match self.history[self.active].pop() {
            Some(view) => {
                self.views[self.active] = view;
                true
            }
            None => false,
        }
    }

    pub fn can_go_back(&self) -> bool {
        !self.history[self.active].is_empty()
    }

    /// Where a pane was drawn last frame
    #[cfg(test)]
    pub fn rect(&self, pane: usize) -> Option<Rect> {
        self.rects.get(pane).copied()
    }

    pub fn len(&self) -> usize {
        self.views.len()
    }

    pub fn focus(&mut self, pane: usize) {
        if pane != self.active && pane < self.views.len() {
            self.previous = Some(self.active);
            self.active = pane;
        }
    }

    /// Splits the active pane; the new pane shows `view` and becomes active
    pub fn split(&mut self, side_by_side: bool, view: View) {
        let side = if side_by_side { Direction::Right } else { Direction::Down };
        let new = self.split_at(self.active, side, view);
        self.focus(new);
    }

    /// Splits pane `target`, putting a new pane showing `view` on one side of it. Returns the new pane.
    pub fn split_at(&mut self, target: usize, side: Direction, view: View) -> usize {
        let new = self.views.len();
        self.views.push(view);
        self.history.push(Vec::new());
        let side_by_side = matches!(side, Direction::Left | Direction::Right);
        let new_first = matches!(side, Direction::Left | Direction::Up);
        replace(&mut self.root, target, &mut |old| {
            let (first, second) = if new_first { (Tile::Pane(new), old) } else { (old, Tile::Pane(new)) };
            Tile::Split { side_by_side, ratio: 0.5, first: Box::new(first), second: Box::new(second) }
        });
        new
    }

    /// Swaps what two panes show
    pub fn swap(&mut self, a: usize, b: usize) {
        self.views.swap(a, b);
        self.history.swap(a, b);
    }

    /// Moves pane `pane` to a side of pane `target`. Returns where it ended up.
    pub fn move_pane(&mut self, pane: usize, target: usize, side: Direction) -> Option<usize> {
        if pane == target || self.views.len() < 2 {
            return None;
        }
        let view = self.views[pane];
        let history = self.history[pane].clone();
        self.remove_pane(pane);
        let target = if target > pane { target - 1 } else { target };
        let new = self.split_at(target, side, view);
        self.history[new] = history;
        self.focus(new);
        Some(new)
    }

    /// Closes the active pane, unless it's the last. Its neighbour takes its space and the focus.
    pub fn close_active(&mut self) -> bool {
        self.remove_pane(self.active)
    }

    /// Closes a pane, unless it's the last. Its neighbour takes its space, and the focus if it had it.
    fn remove_pane(&mut self, closing: usize) -> bool {
        if self.views.len() == 1 {
            return false;
        }
        let had_focus = closing == self.active;
        let Some(sibling) = remove(&mut self.root, closing) else { return false };
        self.views.remove(closing);
        self.history.remove(closing);
        if closing < self.rects.len() {
            self.rects.remove(closing);
        }
        renumber(&mut self.root, closing);
        let shift = |pane: usize| if pane > closing { pane - 1 } else { pane };
        let next = first_pane(&sibling);
        self.active = if had_focus { shift(next) } else { shift(self.active) };
        self.previous = self.previous.filter(|&p| p != closing).map(shift);
        true
    }

    /// Moves the focus to the next pane in reading order
    pub fn cycle(&mut self) {
        let mut order = Vec::new();
        collect_panes(&self.root, &mut order);
        let at = order.iter().position(|&p| p == self.active).unwrap_or(0);
        self.focus(order[(at + 1) % order.len()]);
    }

    /// Moves the focus to the nearest pane in a direction, by where the panes were last drawn
    pub fn focus_direction(&mut self, direction: Direction) -> bool {
        let Some(&from) = self.rects.get(self.active) else { return false };
        let best = self
            .rects
            .iter()
            .enumerate()
            .filter(|&(pane, _)| pane != self.active)
            .filter_map(|(pane, rect)| {
                let (along, across) = match direction {
                    Direction::Left => (from.left() - rect.right(), overlap(from.y_range(), rect.y_range())),
                    Direction::Right => (rect.left() - from.right(), overlap(from.y_range(), rect.y_range())),
                    Direction::Up => (from.top() - rect.bottom(), overlap(from.x_range(), rect.x_range())),
                    Direction::Down => (rect.top() - from.bottom(), overlap(from.x_range(), rect.x_range())),
                };
                // In that direction, and beside it rather than diagonal
                (along >= -GAP && across > 0.0).then_some((pane, along, -across))
            })
            .min_by(|a, b| (a.1, a.2).partial_cmp(&(b.1, b.2)).unwrap_or(std::cmp::Ordering::Equal));
        match best {
            Some((pane, ..)) => {
                self.focus(pane);
                true
            }
            None => false,
        }
    }

    /// The pane to open something beside the active one: the one used before, or a new one to the right
    pub fn beside(&mut self) -> usize {
        match self.previous.filter(|&p| p < self.views.len() && p != self.active) {
            Some(pane) => pane,
            None => {
                let current = self.active;
                self.split(true, self.views[current]);
                let new = self.active;
                self.focus(current);
                new
            }
        }
    }

    /// Draws every pane with `draw(ui, pane, view)`, and handles clicks to focus, divider drags, Alt+drags of panes
    /// and drops from the sidebar. A drop is returned, since opening what was dropped is up to the app.
    pub fn show(&mut self, ui: &mut Ui, mut draw: impl FnMut(&mut Ui, usize, View)) -> Option<Drop> {
        let area = ui.available_rect_before_wrap();
        let mut panes = Vec::new();
        let mut dividers = Vec::new();
        place(&self.root, area, &mut Vec::new(), &mut panes, &mut dividers);

        for divider in &dividers {
            let id = ui.scope_id().with(("divider", &divider.path));
            let response = ui.interact(divider.handle, id, Sense::drag());
            let cursor = if divider.side_by_side { CursorIcon::ResizeHorizontal } else { CursorIcon::ResizeVertical };
            let response = response.on_hover_cursor(cursor);
            if response.dragged()
                && let Some(pointer) = response.interact_pointer_pos()
            {
                let ratio = if divider.side_by_side {
                    (pointer.x - divider.area.left()) / divider.area.width()
                } else {
                    (pointer.y - divider.area.top()) / divider.area.height()
                };
                set_ratio(&mut self.root, &divider.path, ratio.clamp(0.1, 0.9));
            }
            let color = if response.hovered() || response.dragged() {
                ui.visuals().selection.stroke.color
            } else {
                ui.visuals().widgets.noninteractive.bg_stroke.color
            };
            let line = if divider.side_by_side {
                [
                    pos2(divider.handle.center().x, divider.handle.top()),
                    pos2(divider.handle.center().x, divider.handle.bottom()),
                ]
            } else {
                [
                    pos2(divider.handle.left(), divider.handle.center().y),
                    pos2(divider.handle.right(), divider.handle.center().y),
                ]
            };
            ui.painter().line_segment(line, Stroke::new(1.0, color));
        }

        self.rects = vec![Rect::NOTHING; self.views.len()];
        let pressed = ui.input(|i| i.pointer.any_pressed());
        for (pane, rect) in panes {
            self.rects[pane] = rect;
            if pressed && ui.rect_contains_pointer(rect) {
                self.focus(pane);
            }
            let mut child = ui.new_child(UiBuilder::new().max_rect(rect).id_salt(("pane", pane)));
            child.set_clip_rect(rect.intersect(ui.clip_rect()));
            draw(&mut child, pane, self.views[pane]);
            if self.views.len() > 1 && pane == self.active {
                let stroke = Stroke::new(1.0, ui.visuals().selection.stroke.color);
                ui.painter().rect_stroke(rect.expand(1.0), 4.0, stroke, egui::StrokeKind::Outside);
            }
            // With Alt held, the pane itself drags, over whatever is in it
            if ui.input(|i| i.modifiers.alt) {
                let response = ui.interact(rect, ui.scope_id().with(("pane-drag", pane)), Sense::drag());
                response.on_hover_cursor(CursorIcon::Grab).dnd_set_drag_payload(PaneDrag(pane));
            }
        }
        let dropped = self.drops(ui);
        ui.allocate_rect(area, Sense::hover());
        dropped
    }
}

/// A pane being Alt+dragged
#[derive(Clone, Copy)]
struct PaneDrag(usize);

/// Something dragged from the sidebar onto a pane
#[derive(Clone, Debug, PartialEq)]
pub enum Dropped {
    View(View),
    /// A topic, which needs its echo opened first
    Topic {
        name: String,
        ty: Option<String>,
    },
}

/// Something from the sidebar let go over a pane
pub struct Drop {
    pub what: Dropped,
    pub pane: usize,
    /// The edge it docks to, or `None` for the middle, which replaces what the pane shows
    pub side: Option<Direction>,
}

/// Where on a pane a drop lands: its middle to replace or swap, an edge to dock beside it
pub fn drop_zone(rect: Rect, pointer: egui::Pos2) -> Option<Direction> {
    let x = (pointer.x - rect.left()) / rect.width();
    let y = (pointer.y - rect.top()) / rect.height();
    let edge = 0.25;
    // The nearest edge within the band, or the middle
    let candidates =
        [(x, Direction::Left), (1.0 - x, Direction::Right), (y, Direction::Up), (1.0 - y, Direction::Down)];
    let (distance, side) = candidates.into_iter().min_by(|a, b| a.0.total_cmp(&b.0)).expect("four candidates");
    (distance < edge).then_some(side)
}

impl Layout {
    /// Shows where a drag would land, and carries out a pane move on release
    fn drops(&mut self, ui: &mut Ui) -> Option<Drop> {
        let dragging_pane = egui::DragAndDrop::payload::<PaneDrag>(ui.ctx()).map(|p| p.0);
        let dragging_other = egui::DragAndDrop::has_payload_of_type::<Dropped>(ui.ctx());
        if dragging_pane.is_none() && !dragging_other {
            return None;
        }
        let pointer = ui.input(|i| i.pointer.interact_pos())?;
        let (target, rect) = self.rects.iter().copied().enumerate().find(|(_, r)| r.contains(pointer))?;
        let zone = drop_zone(rect, pointer);
        let preview = match zone {
            None => rect,
            Some(Direction::Left) => rect.with_max_x(rect.center().x),
            Some(Direction::Right) => rect.with_min_x(rect.center().x),
            Some(Direction::Up) => rect.with_max_y(rect.center().y),
            Some(Direction::Down) => rect.with_min_y(rect.center().y),
        };
        let same = dragging_pane == Some(target) && zone.is_none();
        if !same {
            let color = ui.visuals().selection.bg_fill;
            ui.painter().rect_filled(preview.shrink(2.0), 6.0, color.gamma_multiply(0.35));
            ui.painter().rect_stroke(preview.shrink(2.0), 6.0, Stroke::new(1.5, color), egui::StrokeKind::Inside);
        }
        if !ui.input(|i| i.pointer.any_released()) {
            return None;
        }
        if let Some(pane) = dragging_pane {
            egui::DragAndDrop::clear_payload(ui.ctx());
            match zone {
                None if pane != target => {
                    self.swap(pane, target);
                    self.focus(target);
                }
                None => {}
                Some(side) => _ = self.move_pane(pane, target, side),
            }
            return None;
        }
        let dropped = egui::DragAndDrop::take_payload::<Dropped>(ui.ctx())?;
        Some(Drop { what: (*dropped).clone(), pane: target, side: zone })
    }

    /// Puts a view where it was dropped: replacing a pane's view, or in a new pane beside it
    pub fn place(&mut self, view: View, target: usize, side: Option<Direction>) {
        match side {
            None => {
                self.set_view(target, view);
                self.focus(target);
            }
            Some(side) => {
                let new = self.split_at(target, side, view);
                self.focus(new);
            }
        }
    }
}

fn overlap(a: egui::Rangef, b: egui::Rangef) -> f32 {
    a.max.min(b.max) - a.min.max(b.min)
}

fn collect_panes(tile: &Tile, out: &mut Vec<usize>) {
    match tile {
        Tile::Pane(pane) => out.push(*pane),
        Tile::Split { first, second, .. } => {
            collect_panes(first, out);
            collect_panes(second, out);
        }
    }
}

fn first_pane(tile: &Tile) -> usize {
    match tile {
        Tile::Pane(pane) => *pane,
        Tile::Split { first, .. } => first_pane(first),
    }
}

/// Replaces the tile showing `target` with `with(old)`
fn replace(tile: &mut Tile, target: usize, with: &mut dyn FnMut(Tile) -> Tile) -> bool {
    match tile {
        Tile::Pane(pane) if *pane == target => {
            let old = std::mem::replace(tile, Tile::Pane(usize::MAX));
            *tile = with(old);
            true
        }
        Tile::Pane(_) => false,
        Tile::Split { first, second, .. } => replace(first, target, with) || replace(second, target, with),
    }
}

/// Removes pane `target`, putting its sibling in its parent's place. Returns the sibling.
fn remove(tile: &mut Tile, target: usize) -> Option<Tile> {
    let Tile::Split { first, second, .. } = tile else { return None };
    let sibling = match (&**first, &**second) {
        (Tile::Pane(p), _) if *p == target => Some((**second).clone()),
        (_, Tile::Pane(p)) if *p == target => Some((**first).clone()),
        _ => None,
    };
    match sibling {
        Some(sibling) => {
            *tile = sibling.clone();
            Some(sibling)
        }
        None => remove(first, target).or_else(|| remove(second, target)),
    }
}

/// Closes the gap left by removing pane `removed`
fn renumber(tile: &mut Tile, removed: usize) {
    match tile {
        Tile::Pane(pane) if *pane > removed => *pane -= 1,
        Tile::Pane(_) => {}
        Tile::Split { first, second, .. } => {
            renumber(first, removed);
            renumber(second, removed);
        }
    }
}

fn set_ratio(tile: &mut Tile, path: &[bool], value: f32) {
    if let Tile::Split { ratio, first, second, .. } = tile {
        match path.split_first() {
            None => *ratio = value,
            Some((false, rest)) => set_ratio(first, rest, value),
            Some((true, rest)) => set_ratio(second, rest, value),
        }
    }
}

fn place(tile: &Tile, area: Rect, path: &mut Vec<bool>, panes: &mut Vec<(usize, Rect)>, dividers: &mut Vec<Divider>) {
    match tile {
        Tile::Pane(pane) => panes.push((*pane, area)),
        Tile::Split { side_by_side, ratio, first, second } => {
            let (a, b, handle) = if *side_by_side {
                let x = area.left() + area.width() * ratio;
                (
                    area.with_max_x(x - GAP / 2.0),
                    area.with_min_x(x + GAP / 2.0),
                    Rect::from_x_y_ranges(x - GAP / 2.0..=x + GAP / 2.0, area.y_range()),
                )
            } else {
                let y = area.top() + area.height() * ratio;
                (
                    area.with_max_y(y - GAP / 2.0),
                    area.with_min_y(y + GAP / 2.0),
                    Rect::from_x_y_ranges(area.x_range(), y - GAP / 2.0..=y + GAP / 2.0),
                )
            };
            dividers.push(Divider { path: path.clone(), handle, area, side_by_side: *side_by_side });
            path.push(false);
            place(first, a, path, panes, dividers);
            path.pop();
            path.push(true);
            place(second, b, path, panes, dividers);
            path.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lays out panes in a 1000×600 area as `show` would, so direction moves have rects to work with
    fn lay_out(layout: &mut Layout) {
        let mut panes = Vec::new();
        place(
            &layout.root,
            Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(1000.0, 600.0)),
            &mut Vec::new(),
            &mut panes,
            &mut Vec::new(),
        );
        layout.rects = vec![Rect::NOTHING; layout.views.len()];
        for (pane, rect) in panes {
            layout.rects[pane] = rect;
        }
    }

    #[test]
    fn splits_and_moves_by_direction() {
        // [0 | 1/2]: 0 on the left, 1 above 2 on the right
        let mut layout = Layout::new(View::Source(0));
        layout.split(true, View::Source(1));
        layout.split(false, View::AllLogs);
        assert_eq!(layout.active(), 2);
        lay_out(&mut layout);
        assert!(layout.focus_direction(Direction::Up));
        assert_eq!(layout.active(), 1);
        assert!(layout.focus_direction(Direction::Left));
        assert_eq!(layout.active(), 0);
        assert!(!layout.focus_direction(Direction::Left), "nothing further left");
        assert!(layout.focus_direction(Direction::Right));
        assert!(matches!(layout.active(), 1 | 2));
    }

    #[test]
    fn closing_gives_the_space_to_the_sibling() {
        let mut layout = Layout::new(View::Source(0));
        layout.split(true, View::Source(1));
        layout.split(false, View::AllLogs);
        assert!(layout.close_active());
        assert_eq!(layout.views(), [View::Source(0), View::Source(1)]);
        assert_eq!(layout.active_view(), View::Source(1));
        assert!(matches!(layout.root(), Tile::Split { side_by_side: true, .. }));
        assert!(layout.close_active());
        assert!(!layout.close_active(), "the last pane stays");
        assert_eq!(layout.root(), &Tile::Pane(0));
    }

    #[test]
    fn closing_a_pane_renumbers_the_rest() {
        let mut layout = Layout::new(View::Source(0));
        layout.split(true, View::Source(1));
        layout.split(true, View::Source(2));
        layout.focus(0);
        assert!(layout.close_active());
        let mut panes = Vec::new();
        collect_panes(layout.root(), &mut panes);
        assert_eq!(panes, [0, 1]);
        assert_eq!(layout.views(), [View::Source(1), View::Source(2)]);
    }

    #[test]
    fn beside_reuses_the_previous_pane() {
        let mut layout = Layout::new(View::Source(0));
        let first = layout.beside();
        assert_eq!(layout.len(), 2, "split to make room");
        assert_eq!(layout.active(), 0, "the focus stays");
        assert_eq!(layout.beside(), first, "then reuses it");
        assert_eq!(layout.len(), 2);
    }

    #[test]
    fn goes_back_through_what_a_pane_showed() {
        let mut layout = Layout::new(View::Source(0));
        layout.set_active_view(View::Topics);
        layout.set_active_view(View::Source(3));
        assert!(layout.back());
        assert_eq!(layout.active_view(), View::Topics);
        assert!(layout.back());
        assert_eq!(layout.active_view(), View::Source(0));
        assert!(!layout.back());
    }

    #[test]
    fn moves_and_swaps_panes() {
        // [0 | 1]: move 0 below 1, which leaves one column of [1 / 0]
        let mut layout = Layout::new(View::Source(0));
        layout.split(true, View::Source(1));
        let moved = layout.move_pane(0, 1, Direction::Down).unwrap();
        assert_eq!(layout.views()[moved], View::Source(0));
        assert!(matches!(layout.root(), Tile::Split { side_by_side: false, .. }));
        assert_eq!(layout.views().len(), 2);
        layout.swap(0, 1);
        assert_eq!(layout.views(), [View::Source(0), View::Source(1)]);
    }

    #[test]
    fn docks_on_the_side_dropped() {
        let mut layout = Layout::new(View::Source(0));
        layout.place(View::Topics, 0, Some(Direction::Left));
        let Tile::Split { side_by_side: true, first, .. } = layout.root() else { panic!() };
        assert_eq!(**first, Tile::Pane(1), "the new pane is on the left");
        layout.place(View::AllLogs, 1, None);
        assert_eq!(layout.views(), [View::Source(0), View::AllLogs]);
    }

    #[test]
    fn drop_zones() {
        let rect = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
        assert_eq!(drop_zone(rect, pos2(50.0, 50.0)), None);
        assert_eq!(drop_zone(rect, pos2(5.0, 50.0)), Some(Direction::Left));
        assert_eq!(drop_zone(rect, pos2(60.0, 95.0)), Some(Direction::Down));
    }

    #[test]
    fn cycles_in_reading_order() {
        let mut layout = Layout::new(View::Source(0));
        layout.split(true, View::Source(1));
        layout.cycle();
        assert_eq!(layout.active(), 0);
        layout.cycle();
        assert_eq!(layout.active(), 1);
    }

    #[test]
    fn rejects_parts_that_dont_fit() {
        let root = Tile::Split {
            side_by_side: true,
            ratio: 0.5,
            first: Box::new(Tile::Pane(0)),
            second: Box::new(Tile::Pane(0)),
        };
        assert!(Layout::from_parts(root, vec![View::AllLogs, View::Topics], 0).is_none());
        assert!(Layout::from_parts(Tile::Pane(0), vec![View::AllLogs], 1).is_none());
    }
}
