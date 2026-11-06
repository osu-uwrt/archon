//! Every topic on the graph as a table, like `ros2 topic list` with the columns that matter for checking a bench:
//! rate, publishers, subscribers and how long since the last message. Sortable and filterable.

use std::time::{Duration, Instant};

use egui::{Align2, Rect, RichText, ScrollArea, Sense, Stroke, TextStyle, Ui, pos2, vec2};

use crate::logview::find;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Column {
    Name,
    Type,
    Rate,
    Publishers,
    Subscribers,
    Last,
}

const COLUMNS: [(Column, &str); 6] = [
    (Column::Name, "topic"),
    (Column::Type, "type"),
    (Column::Rate, "rate"),
    (Column::Publishers, "pub"),
    (Column::Subscribers, "sub"),
    (Column::Last, "last msg"),
];

/// A message older than this shows the topic as stale
const STALE: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, PartialEq)]
pub struct TopicRow {
    pub name: String,
    pub ty: String,
    /// Known when archon listens to it: echoed, watched, or measured
    pub hz: Option<f64>,
    pub publishers: usize,
    pub subscribers: usize,
    pub last: Option<Instant>,
    pub echoed: bool,
    pub watched: bool,
}

pub struct TopicsTable {
    pub selected: usize,
    pub filter: String,
    pub sort: Column,
    pub descending: bool,
    /// Measure every topic's rate, not only those archon listens to anyway
    pub measure_all: bool,
    reveal: bool,
}

impl Default for TopicsTable {
    fn default() -> Self {
        Self {
            selected: 0,
            filter: String::new(),
            sort: Column::Name,
            descending: false,
            measure_all: false,
            reveal: false,
        }
    }
}

pub enum Action {
    Open { name: String, ty: String, beside: bool },
}

impl TopicsTable {
    /// The rows to show, filtered and sorted
    pub fn arrange(&self, mut rows: Vec<TopicRow>) -> Vec<TopicRow> {
        rows.retain(|r| {
            self.filter.is_empty()
                || find(&r.name, &self.filter).next().is_some()
                || find(&r.ty, &self.filter).next().is_some()
        });
        let key = |r: &TopicRow| match self.sort {
            Column::Rate => r.hz.unwrap_or(-1.0),
            Column::Publishers => r.publishers as f64,
            Column::Subscribers => r.subscribers as f64,
            // Most recent first when ascending, never-seen last
            Column::Last => r.last.map_or(f64::MAX, |t| t.elapsed().as_secs_f64()),
            Column::Name | Column::Type => 0.0,
        };
        rows.sort_by(|a, b| {
            let order = match self.sort {
                Column::Name => a.name.cmp(&b.name),
                Column::Type => a.ty.cmp(&b.ty).then(a.name.cmp(&b.name)),
                _ => key(a).partial_cmp(&key(b)).unwrap_or(std::cmp::Ordering::Equal).then(a.name.cmp(&b.name)),
            };
            if self.descending { order.reverse() } else { order }
        });
        rows
    }

    pub fn move_selection(&mut self, by: isize, rows: usize) {
        if rows > 0 {
            self.selected = self.selected.saturating_add_signed(by).min(rows - 1);
            self.reveal = true;
        }
    }

    pub fn jump_to_end(&mut self, rows: usize, end: bool) {
        self.selected = if end { rows.saturating_sub(1) } else { 0 };
        self.reveal = true;
    }

    /// Clicking the sorted column again flips its order
    fn sort_by(&mut self, column: Column) {
        if self.sort == column {
            self.descending = !self.descending;
        } else {
            self.sort = column;
            self.descending = matches!(column, Column::Rate | Column::Publishers | Column::Subscribers);
        }
    }
}

pub fn rate_text(hz: Option<f64>) -> String {
    match hz {
        Some(hz) if hz >= 100.0 => format!("{hz:.0} Hz"),
        Some(hz) if hz >= 10.0 => format!("{hz:.1} Hz"),
        Some(hz) => format!("{hz:.2} Hz"),
        None => String::new(),
    }
}

fn age_text(last: Option<Instant>) -> String {
    match last.map(|t| t.elapsed()) {
        Some(age) if age < Duration::from_secs(1) => format!("{} ms", age.as_millis()),
        Some(age) if age < Duration::from_secs(120) => format!("{:.0} s", age.as_secs_f32()),
        Some(age) => format!("{:.0} min", age.as_secs_f32() / 60.0),
        None => String::new(),
    }
}

/// `rows` already arranged. Returns what was clicked.
pub fn show(
    ui: &mut Ui,
    pane: usize,
    rows: &[TopicRow],
    table: &mut TopicsTable,
    ros_note: Option<&str>,
) -> Option<Action> {
    let visuals = ui.visuals().clone();
    table.selected = table.selected.min(rows.len().saturating_sub(1));
    ui.horizontal(|ui| {
        ui.label(RichText::new("Topics").strong());
        ui.label(RichText::new(format!("{} on the graph", rows.len())).weak());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let measure = if table.measure_all { "m measuring every rate" } else { "m measure every rate" };
            ui.label(RichText::new(format!("Enter echo · w watch · {measure}")).small().weak());
            if !table.filter.is_empty() {
                ui.label(RichText::new(format!("/{}", table.filter)).small());
            }
        });
    });
    ui.separator();
    if let Some(note) = ros_note {
        ui.label(RichText::new(note).color(visuals.warn_fg_color));
        return None;
    }

    let font = TextStyle::Monospace.resolve(ui.style());
    let small = TextStyle::Small.resolve(ui.style());
    let row_height = ui.fonts_mut(|f| f.row_height(&font)) + 4.0;
    let width = ui.available_width();
    // Fixed widths for the numbers; the names and types share the rest. A narrow pane drops the type, then the
    // subscribers, rather than squeezing everything.
    let (show_type, show_subscribers) = (width >= 560.0, width >= 420.0);
    let fixed = [80.0, 40.0, if show_subscribers { 40.0 } else { 0.0 }, 70.0];
    let flexible = (width - fixed.iter().sum::<f32>() - 16.0).max(80.0);
    let (name_width, type_width) = if show_type { (flexible * 0.55, flexible * 0.45) } else { (flexible, 0.0) };
    let widths = [name_width, type_width, fixed[0], fixed[1], fixed[2], fixed[3]];

    // The header sorts
    let (header, _) = ui.allocate_exact_size(vec2(width, row_height), Sense::hover());
    let columns_x: Vec<f32> = widths
        .iter()
        .scan(header.left() + 12.0, |x, w| {
            let at = *x;
            *x += w;
            Some(at)
        })
        .collect();
    for (i, (column, title)) in COLUMNS.iter().enumerate() {
        if widths[i] == 0.0 {
            continue;
        }
        let cell = Rect::from_x_y_ranges(columns_x[i]..=columns_x[i] + widths[i], header.y_range());
        let response = ui.interact(cell, ui.scope_id().with(("sort", pane, i)), Sense::click());
        let arrow = if table.sort == *column { if table.descending { " v" } else { " ^" } } else { "" };
        let color = if response.hovered() { visuals.strong_text_color() } else { visuals.weak_text_color() };
        let align = if i >= 2 { Align2::RIGHT_CENTER } else { Align2::LEFT_CENTER };
        let anchor =
            if i >= 2 { pos2(cell.right() - 8.0, cell.center().y) } else { pos2(cell.left(), cell.center().y) };
        ui.painter().text(anchor, align, format!("{title}{arrow}"), small.clone(), color);
        if response.clicked() {
            table.sort_by(*column);
        }
    }

    let mut action = None;
    let mut area = ScrollArea::vertical().id_salt(("topics", pane)).auto_shrink(false);
    if std::mem::take(&mut table.reveal) {
        let viewport = ui.available_height();
        area = area.vertical_scroll_offset((table.selected as f32 * row_height - viewport / 3.0).max(0.0));
    }
    ui.spacing_mut().item_spacing.y = 0.0;
    area.show_rows(ui, row_height, rows.len(), |ui, range| {
        for i in range {
            let row = &rows[i];
            let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), row_height), Sense::click());
            let painter = ui.painter_at(rect);
            if i == table.selected {
                painter.rect_filled(rect, 2.0, visuals.selection.bg_fill.gamma_multiply(0.5));
            } else if response.hovered() {
                painter.rect_filled(rect, 2.0, visuals.widgets.hovered.weak_bg_fill);
            }
            if response.clicked() {
                table.selected = i;
            }
            if response.double_clicked() {
                let beside = ui.input(|i| i.modifiers.command);
                action = Some(Action::Open { name: row.name.clone(), ty: row.ty.clone(), beside });
            }
            let stale = row.last.is_some_and(|t| t.elapsed() > STALE);
            let cells = [
                (
                    row.name.clone(),
                    if row.echoed || row.watched { visuals.strong_text_color() } else { visuals.text_color() },
                ),
                (row.ty.clone(), visuals.weak_text_color()),
                (
                    rate_text(row.hz),
                    if row.hz.is_none() && (row.echoed || row.watched) {
                        visuals.warn_fg_color
                    } else {
                        visuals.text_color()
                    },
                ),
                (
                    row.publishers.to_string(),
                    if row.publishers == 0 { visuals.warn_fg_color } else { visuals.text_color() },
                ),
                (row.subscribers.to_string(), visuals.text_color()),
                (age_text(row.last), if stale { visuals.warn_fg_color } else { visuals.weak_text_color() }),
            ];
            for (c, (text, color)) in cells.into_iter().enumerate() {
                if widths[c] == 0.0 {
                    continue;
                }
                let cell = Rect::from_x_y_ranges(columns_x[c]..=columns_x[c] + widths[c] - 8.0, rect.y_range());
                let (anchor, align) = if c >= 2 {
                    (pos2(cell.right(), cell.center().y), Align2::RIGHT_CENTER)
                } else {
                    (pos2(cell.left(), cell.center().y), Align2::LEFT_CENTER)
                };
                painter.with_clip_rect(cell).text(anchor, align, text, font.clone(), color);
            }
            if row.watched {
                painter.circle_stroke(
                    pos2(rect.left() + 5.0, rect.center().y),
                    2.0,
                    Stroke::new(1.0, visuals.hyperlink_color),
                );
            }
        }
    });
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, ty: &str, hz: Option<f64>, publishers: usize) -> TopicRow {
        TopicRow {
            name: name.to_owned(),
            ty: ty.to_owned(),
            hz,
            publishers,
            subscribers: 0,
            last: None,
            echoed: false,
            watched: false,
        }
    }

    fn names(rows: &[TopicRow]) -> Vec<&str> {
        rows.iter().map(|r| r.name.as_str()).collect()
    }

    #[test]
    fn sorts_and_filters() {
        let rows = vec![
            row("/imu/data", "sensor_msgs/msg/Imu", Some(200.0), 1),
            row("/rosout", "rcl_interfaces/msg/Log", Some(3.0), 4),
            row("/thrusters", "riptide_msgs2/msg/Thrusters", None, 0),
        ];
        let mut table = TopicsTable::default();
        assert_eq!(names(&table.arrange(rows.clone())), ["/imu/data", "/rosout", "/thrusters"]);
        table.sort_by(Column::Rate);
        assert_eq!(names(&table.arrange(rows.clone())), ["/imu/data", "/rosout", "/thrusters"], "fastest first");
        table.sort_by(Column::Rate);
        assert_eq!(names(&table.arrange(rows.clone()))[0], "/thrusters", "flipped");
        table.filter = "riptide".to_owned();
        assert_eq!(names(&table.arrange(rows.clone())), ["/thrusters"], "matches types too");
    }

    #[test]
    fn formats_rates() {
        assert_eq!(rate_text(Some(200.4)), "200 Hz");
        assert_eq!(rate_text(Some(12.34)), "12.3 Hz");
        assert_eq!(rate_text(Some(0.5)), "0.50 Hz");
        assert_eq!(rate_text(None), "");
    }
}
