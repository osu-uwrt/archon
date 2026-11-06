//! A topic's latest message as a tree that updates in place: changed fields flash, numbers get a sparkline of their
//! recent values. Easier to read than a stream once a topic goes faster than a few messages a second.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use archon_msg::Value;
use archon_ros::TopicState;
use archon_sources::Entry;
use egui::{Align2, Color32, Pos2, Rect, RichText, ScrollArea, Sense, Shape, Stroke, TextStyle, Ui, pos2, vec2};

use crate::logview::{status_color, status_dot, status_text};

/// How long a changed field stays highlighted
const FLASH: Duration = Duration::from_millis(800);
/// Arrays of numbers this short show on one line
const INLINE_ITEMS: usize = 8;
/// Elements listed when an array is expanded
const LISTED_ITEMS: usize = 256;
/// The widest a sparkline gets; narrow panes get narrower ones
const SPARKLINE_WIDTH: f32 = 90.0;
const INDENT: f32 = 14.0;

#[derive(Default)]
pub struct InspectorState {
    pub cursor: usize,
    /// Branches closed by hand (structs start open)
    closed: HashSet<String>,
    /// Arrays opened by hand (long ones start closed)
    opened: HashSet<String>,
    /// Scroll the cursor into view next frame
    reveal: bool,
}

/// One line of the tree
#[derive(Debug, PartialEq)]
pub struct Node<'a> {
    pub path: String,
    pub name: String,
    pub depth: usize,
    pub value: &'a Value,
    /// For branches: whether they're open
    pub open: Option<bool>,
}

impl InspectorState {
    fn is_open(&self, path: &str, value: &Value) -> bool {
        match value {
            Value::Struct(_) => !self.closed.contains(path),
            Value::Array(items) => {
                let small = items.len() <= INLINE_ITEMS;
                (small && !self.closed.contains(path)) || self.opened.contains(path)
            }
            Value::Bytes(_) => self.opened.contains(path),
            _ => false,
        }
    }

    /// The rows of the tree as it's open now
    pub fn nodes<'a>(&self, value: &'a Value) -> Vec<Node<'a>> {
        let mut out = Vec::new();
        if let Value::Struct(fields) = value {
            for (name, v) in fields {
                self.walk(name.clone(), name.clone(), 0, v, &mut out);
            }
        }
        out
    }

    fn walk<'a>(&self, path: String, name: String, depth: usize, value: &'a Value, out: &mut Vec<Node<'a>>) {
        let branch = match value {
            Value::Struct(fields) => !fields.is_empty(),
            Value::Array(items) => {
                items.iter().any(|v| matches!(v, Value::Struct(_) | Value::Array(_))) || items.len() > INLINE_ITEMS
            }
            Value::Bytes(bytes) => bytes.len() > INLINE_ITEMS,
            _ => false,
        };
        let open = branch.then(|| self.is_open(&path, value));
        out.push(Node { path: path.clone(), name, depth, value, open });
        if open != Some(true) {
            return;
        }
        match value {
            Value::Struct(fields) => {
                for (n, v) in fields {
                    self.walk(format!("{path}.{n}"), n.clone(), depth + 1, v, out);
                }
            }
            Value::Array(items) => {
                for (i, v) in items.iter().take(LISTED_ITEMS).enumerate() {
                    self.walk(format!("{path}[{i}]"), format!("[{i}]"), depth + 1, v, out);
                }
            }
            // Bytes have no Value per element, so an open byte array shows as one row of hex
            _ => {}
        }
    }

    pub fn move_cursor(&mut self, by: isize, rows: usize) {
        if rows == 0 {
            return;
        }
        self.cursor = self.cursor.saturating_add_signed(by).min(rows - 1);
        self.reveal = true;
    }

    pub fn jump_to_end(&mut self, rows: usize, end: bool) {
        self.cursor = if end { rows.saturating_sub(1) } else { 0 };
        self.reveal = true;
    }

    /// Opens the branch at the cursor, or with `toggle` flips it
    pub fn expand(&mut self, nodes: &[Node<'_>], toggle: bool) {
        let Some(node) = nodes.get(self.cursor) else { return };
        match node.open {
            Some(true) if toggle => self.set_open(&node.path, node.value, false),
            Some(false) => self.set_open(&node.path, node.value, true),
            _ => {}
        }
    }

    /// Closes the branch at the cursor, or moves to its parent
    pub fn collapse(&mut self, nodes: &[Node<'_>]) {
        let Some(node) = nodes.get(self.cursor) else { return };
        if node.open == Some(true) {
            self.set_open(&node.path, node.value, false);
            return;
        }
        if let Some(parent) = nodes[..self.cursor].iter().rposition(|n| n.depth < node.depth) {
            self.cursor = parent;
            self.reveal = true;
        }
    }

    fn set_open(&mut self, path: &str, value: &Value, open: bool) {
        let starts_open = match value {
            Value::Struct(_) => true,
            Value::Array(items) => items.len() <= INLINE_ITEMS,
            _ => false,
        };
        let (keep, drop) = if open == starts_open { (None, Some(path)) } else { (Some(path), None) };
        let set = if starts_open { &mut self.closed } else { &mut self.opened };
        if let Some(path) = keep {
            set.insert(path.to_owned());
        }
        if let Some(path) = drop {
            set.remove(path);
        }
    }
}

/// A value as it reads on its row
pub fn value_text(value: &Value, open: Option<bool>) -> String {
    match value {
        Value::Struct(fields) if open == Some(true) || fields.is_empty() => String::new(),
        Value::Struct(fields) => format!("{{{} fields}}", fields.len()),
        Value::Array(items) if open.is_some() => format!("[{} items]", items.len()),
        Value::Array(items) => {
            format!("[{}]", items.iter().map(|v| value_text(v, None)).collect::<Vec<_>>().join(", "))
        }
        Value::Bytes(bytes) if bytes.len() <= INLINE_ITEMS || open == Some(true) => {
            bytes.iter().take(LISTED_ITEMS).map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
        }
        Value::Bytes(bytes) => format!("[{} bytes]", bytes.len()),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::UInt(u) => u.to_string(),
        Value::Float32(f) => format!("{f:?}"),
        Value::Float64(f) => format!("{f:?}"),
        Value::String(s) => format!("{s:?}"),
    }
}

pub enum Action {
    /// The row at the cursor was double-clicked or its triangle clicked
    Toggle,
}

/// `detail` is the source's rate line, taken before `topic` was locked, since the echo locks it to work that out
pub fn show(
    ui: &mut Ui,
    pane: usize,
    entry: &Entry,
    detail: Option<String>,
    topic: &TopicState,
    state: &mut InspectorState,
) -> Option<Action> {
    let visuals = ui.visuals().clone();
    ui.horizontal(|ui| {
        status_dot(ui, status_color(&entry.status, &visuals));
        ui.label(RichText::new(&entry.name).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new("v stream · v v bytes").small().weak());
            let age = topic.rate.last().map(|t| t.elapsed());
            let age = match age {
                Some(age) if age < Duration::from_secs(1) => format!("last {} ms ago", age.as_millis()),
                Some(age) => format!("last {:.0} s ago", age.as_secs_f32()),
                None => "no messages yet".to_owned(),
            };
            ui.label(RichText::new(format!("{} msgs · {age}", topic.count)).small().weak());
            if let Some(detail) = detail {
                ui.label(RichText::new(detail).small());
            }
            // Last, so it's what gets cut short in a narrow pane
            let what = match &topic.ty {
                Some(ty) => format!("{} {ty}", topic.topic),
                None => format!("{} (type not known yet)", topic.topic),
            };
            ui.add(egui::Label::new(RichText::new(what).weak().monospace()).truncate());
        });
    });
    ui.separator();

    let Some(latest) = &topic.latest else {
        ui.add_space(12.0);
        let why = match &entry.status {
            archon_core::source::Status::Running => "Waiting for the first message".to_owned(),
            status => status_text(status),
        };
        ui.label(RichText::new(why).weak());
        return None;
    };
    let nodes = state.nodes(latest);
    state.cursor = state.cursor.min(nodes.len().saturating_sub(1));

    let font = TextStyle::Monospace.resolve(ui.style());
    let row_height = ui.fonts_mut(|f| f.row_height(&font)) + 4.0;
    // Names get what they need up to half the width, values most of the rest, sparklines what's left
    let width = ui.available_width();
    let names =
        nodes.iter().map(|n| n.depth as f32 * INDENT + n.name.len() as f32 * font.size * 0.62).fold(0.0_f32, f32::max);
    let name_column = (names + 36.0).min(width * 0.5);
    let sparkline_width = (width * 0.2).clamp(32.0, SPARKLINE_WIDTH);
    let mut area = ScrollArea::vertical().id_salt(("inspector", pane)).auto_shrink(false);
    if std::mem::take(&mut state.reveal) {
        let viewport = ui.available_height();
        let top = state.cursor as f32 * row_height;
        area = area.vertical_scroll_offset((top - viewport / 3.0).max(0.0));
    }
    let now = Instant::now();
    let mut action = None;
    let mut flashing = false;
    ui.spacing_mut().item_spacing.y = 0.0;
    area.show_rows(ui, row_height, nodes.len(), |ui, range| {
        for i in range {
            let node = &nodes[i];
            let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), row_height), Sense::click());
            let painter = ui.painter_at(rect);
            let changed = topic.changed_at(&node.path).map(|at| now.saturating_duration_since(at));
            if let Some(age) = changed.filter(|age| *age < FLASH) {
                let strength = 1.0 - age.as_secs_f32() / FLASH.as_secs_f32();
                painter.rect_filled(rect, 2.0, visuals.selection.bg_fill.gamma_multiply(0.6 * strength));
                flashing = true;
            }
            if i == state.cursor {
                painter.rect_stroke(
                    rect.shrink(0.5),
                    2.0,
                    Stroke::new(1.0, visuals.selection.stroke.color),
                    egui::StrokeKind::Inside,
                );
            }
            if response.clicked() {
                state.cursor = i;
            }
            let x = rect.left() + 6.0 + node.depth as f32 * INDENT;
            if let Some(open) = node.open {
                triangle(&painter, pos2(x + 4.0, rect.center().y), open, visuals.weak_text_color());
                if response.double_clicked()
                    || (response.clicked() && response.interact_pointer_pos().is_some_and(|p| p.x < x + 12.0))
                {
                    state.cursor = i;
                    action = Some(Action::Toggle);
                }
            }
            // Cut short at the value column rather than running into it
            let name_rect = Rect::from_x_y_ranges(rect.left()..=rect.left() + name_column - 8.0, rect.y_range());
            painter.with_clip_rect(name_rect).text(
                pos2(x + 14.0, rect.center().y),
                Align2::LEFT_CENTER,
                &node.name,
                font.clone(),
                visuals.text_color(),
            );

            let value_x = rect.left() + name_column;
            let spark = rect.right() - sparkline_width - 8.0;
            let value = value_text(node.value, node.open);
            let color =
                if changed.is_some_and(|age| age < FLASH) { visuals.strong_text_color() } else { visuals.text_color() };
            let value_rect = Rect::from_x_y_ranges(value_x..=spark - 8.0, rect.y_range());
            painter.with_clip_rect(value_rect).text(
                pos2(value_x, rect.center().y),
                Align2::LEFT_CENTER,
                value,
                font.clone(),
                color,
            );
            if let Some(series) = topic.series(&node.path).filter(|s| s.len() > 1 && node.open.is_none()) {
                sparkline(
                    &painter,
                    Rect::from_x_y_ranges(spark..=rect.right() - 8.0, rect.top() + 3.0..=rect.bottom() - 3.0),
                    series,
                    &visuals,
                );
            }
        }
    });
    if flashing {
        ui.ctx().request_repaint_after(Duration::from_millis(30));
    }
    action
}

fn triangle(painter: &egui::Painter, center: Pos2, open: bool, color: Color32) {
    let points = if open {
        vec![center + vec2(-4.0, -2.0), center + vec2(4.0, -2.0), center + vec2(0.0, 3.0)]
    } else {
        vec![center + vec2(-2.0, -4.0), center + vec2(3.0, 0.0), center + vec2(-2.0, 4.0)]
    };
    painter.add(Shape::convex_polygon(points, color, Stroke::NONE));
}

fn sparkline(painter: &egui::Painter, rect: Rect, series: &std::collections::VecDeque<f64>, visuals: &egui::Visuals) {
    let finite = series.iter().copied().filter(|v| v.is_finite());
    let (min, max) = finite.fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| (lo.min(v), hi.max(v)));
    if !min.is_finite() {
        return;
    }
    let span = if max > min { max - min } else { 1.0 };
    // A short history still spans a fair part of the width, filling it as samples arrive
    let step = rect.width() / (series.len().max(archon_ros::SERIES_LEN / 4) - 1) as f32;
    let start = rect.right() - step * (series.len() - 1) as f32;
    let points: Vec<Pos2> = series
        .iter()
        .enumerate()
        .filter(|(_, v)| v.is_finite())
        .map(|(i, v)| pos2(start + step * i as f32, rect.bottom() - ((v - min) / span) as f32 * rect.height()))
        .collect();
    let color = visuals.hyperlink_color;
    // A value that never changed draws flat, dimmed
    let stroke = if max == min { Stroke::new(1.0, visuals.weak_text_color()) } else { Stroke::new(1.2, color) };
    painter.add(Shape::line(points, stroke));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(fields: &[(&str, Value)]) -> Value {
        Value::Struct(fields.iter().map(|(n, v)| ((*n).to_owned(), v.clone())).collect())
    }

    fn message() -> Value {
        s(&[
            ("header", s(&[("frame_id", Value::String("base".into()))])),
            ("force", Value::Array(vec![Value::Float32(1.0), Value::Float32(2.0)])),
            ("samples", Value::Array((0..20).map(Value::Int).collect())),
            ("points", Value::Array(vec![s(&[("x", Value::Float64(1.0))])])),
        ])
    }

    fn paths(state: &InspectorState, value: &Value) -> Vec<String> {
        state.nodes(value).into_iter().map(|n| n.path).collect()
    }

    #[test]
    fn structs_start_open_and_long_arrays_closed() {
        let msg = message();
        let state = InspectorState::default();
        assert_eq!(
            paths(&state, &msg),
            ["header", "header.frame_id", "force", "samples", "points", "points[0]", "points[0].x"]
        );
        let nodes = state.nodes(&msg);
        assert_eq!(value_text(nodes[2].value, nodes[2].open), "[1.0, 2.0]");
        assert_eq!(value_text(nodes[3].value, nodes[3].open), "[20 items]");
    }

    #[test]
    fn expands_collapses_and_walks_to_the_parent() {
        let msg = message();
        let mut state = InspectorState { cursor: 3, ..Default::default() };
        state.expand(&state.nodes(&msg), false);
        assert_eq!(paths(&state, &msg).len(), 7 + 20, "samples opened");
        state.cursor = 1;
        state.collapse(&state.nodes(&msg));
        assert_eq!(state.cursor, 0, "a leaf collapses to its parent");
        state.collapse(&state.nodes(&msg));
        assert!(!paths(&state, &msg).contains(&"header.frame_id".to_owned()), "then the parent closes");
        state.expand(&state.nodes(&msg), true);
        assert!(paths(&state, &msg).contains(&"header.frame_id".to_owned()));
    }

    #[test]
    fn cursor_stays_in_range() {
        let mut state = InspectorState::default();
        state.move_cursor(-3, 5);
        assert_eq!(state.cursor, 0);
        state.move_cursor(10, 5);
        assert_eq!(state.cursor, 4);
    }
}
