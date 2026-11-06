//! Live plots of message fields over time. A field joins a plot from the inspector (`p`); `[` and `]` change how
//! much time shows, `f` pauses.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use archon_core::source::SourceId;
use archon_ros::TopicState;
use egui::{Color32, RichText, Sense, Ui, vec2};
use egui_plot::{GridInput, GridMark, HoverPosition, Line, MarkerShape, Plot as PlotWidget, PlotPoints, Points};

/// The time windows `[` and `]` step through, in seconds
const WINDOWS: [f64; 8] = [1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0];

/// Series colors, given out in this order. The dark-surface steps of a categorical palette checked for colour
/// blindness (adjacent pairs) and 3:1 contrast on archon's dark panels
pub const SERIES_COLORS: [Color32; 8] = [
    Color32::from_rgb(0x39, 0x87, 0xe5),
    Color32::from_rgb(0xd9, 0x59, 0x26),
    Color32::from_rgb(0x19, 0x9e, 0x70),
    Color32::from_rgb(0xc9, 0x85, 0x00),
    Color32::from_rgb(0xd5, 0x51, 0x81),
    Color32::from_rgb(0x00, 0x83, 0x00),
    Color32::from_rgb(0x90, 0x85, 0xe9),
    Color32::from_rgb(0xe6, 0x67, 0x67),
];

pub struct Plot {
    /// Each series as its topic echo and the field's path
    pub series: Vec<(SourceId, String)>,
    pub seconds: f64,
    /// When it was paused; the plot shows up to then
    pub paused_at: Option<Instant>,
    /// Each series' color, kept when others come and go
    slots: HashMap<(SourceId, String), usize>,
}

impl Plot {
    pub fn new() -> Self {
        Self { series: Vec::new(), seconds: 10.0, paused_at: None, slots: HashMap::new() }
    }

    pub fn wider(&mut self, wider: bool) {
        let at = WINDOWS.iter().position(|w| *w >= self.seconds).unwrap_or(WINDOWS.len() - 1);
        let at = if wider { (at + 1).min(WINDOWS.len() - 1) } else { at.saturating_sub(1) };
        self.seconds = WINDOWS[at];
    }

    pub fn toggle_pause(&mut self) {
        self.paused_at = match self.paused_at {
            Some(_) => None,
            None => Some(Instant::now()),
        };
    }

    /// Each series' color: the one it was given, or the first one free
    fn colors(&mut self) -> Vec<Color32> {
        self.slots.retain(|key, _| self.series.contains(key));
        self.series
            .iter()
            .map(|key| {
                let slot = match self.slots.get(key) {
                    Some(slot) => *slot,
                    None => {
                        let used: Vec<usize> = self.slots.values().copied().collect();
                        let free = (0..).find(|s| !used.contains(s)).expect("unbounded");
                        self.slots.insert(key.clone(), free);
                        free
                    }
                };
                SERIES_COLORS[slot % SERIES_COLORS.len()]
            })
            .collect()
    }
}

pub enum Action {
    Remove(usize),
}

/// A series' points in the window, as seconds before now and value
struct Drawn {
    name: String,
    path: String,
    color: Color32,
    points: Vec<[f64; 2]>,
}

/// `names` gives each series' source name
pub fn show(
    ui: &mut Ui,
    pane: usize,
    plot: &mut Plot,
    topics: &HashMap<SourceId, Arc<Mutex<TopicState>>>,
    names: &dyn Fn(SourceId) -> String,
) -> Option<Action> {
    let mut action = None;
    let visuals = ui.visuals().clone();
    let now = plot.paused_at.unwrap_or_else(Instant::now);
    let colors = plot.colors();
    let drawn: Vec<Drawn> = plot
        .series
        .iter()
        .zip(colors)
        .map(|((id, path), color)| {
            let points = topics
                .get(id)
                .map(|state| {
                    let state = state.lock().unwrap();
                    state
                        .traced(path)
                        .map(|trace| {
                            trace
                                .iter()
                                .filter(|(t, _)| *t <= now)
                                .map(|(t, y)| [-(now - *t).as_secs_f64(), *y])
                                .filter(|[x, y]| *x >= -plot.seconds && y.is_finite())
                                .collect()
                        })
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            Drawn { name: names(*id), path: path.clone(), color, points }
        })
        .collect();

    // Title row: the window, whether it's paused, and the keys
    ui.horizontal(|ui| {
        ui.label(RichText::new("Plot").strong());
        ui.label(RichText::new(format!("last {}", seconds_text(plot.seconds))).weak());
        if plot.paused_at.is_some() {
            ui.label(
                RichText::new(" PAUSED ")
                    .small()
                    .strong()
                    .color(visuals.panel_fill)
                    .background_color(visuals.warn_fg_color),
            );
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new("[ ] time · f pause · Ctrl+L clear · p on a field adds it").small().weak());
        });
    });

    // One row per series: its color, what it is, where it is now and how far it went in the window. The numbers
    // are padded to the widest in each column so they line up down the rows
    let numbers: Vec<Option<[String; 3]>> = drawn
        .iter()
        .map(|d| stats(&d.points).map(|(last, min, max)| [value_text(last), value_text(min), value_text(max)]))
        .collect();
    let widths = (0..3).map(|c| numbers.iter().flatten().map(|n| n[c].len()).max().unwrap_or(0)).collect::<Vec<_>>();
    for (i, series) in drawn.iter().enumerate() {
        ui.horizontal(|ui| {
            // The numbers and the remove button take what they need from the right; the name and path get the rest
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let remove = ui
                    .add(egui::Button::new(RichText::new("×").small()).frame(false))
                    .on_hover_text("Take it off the plot");
                if remove.clicked() {
                    action = Some(Action::Remove(i));
                }
                match &numbers[i] {
                    Some([last, min, max]) => {
                        let (w_last, w_min, w_max) = (widths[0], widths[1], widths[2]);
                        ui.label(RichText::new(format!("max {max:>w_max$}")).small().weak().monospace());
                        ui.label(RichText::new(format!("min {min:>w_min$}")).small().weak().monospace());
                        ui.add_space(4.0);
                        ui.label(RichText::new(format!("{last:>w_last$}")).strong().monospace());
                    }
                    None => _ = ui.label(RichText::new("no values yet").small().weak()),
                }
                ui.add_space(8.0);
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    let (rect, _) = ui.allocate_exact_size(vec2(14.0, 10.0), Sense::hover());
                    ui.painter().rect_filled(
                        egui::Rect::from_center_size(rect.center(), vec2(14.0, 3.0)),
                        1.5,
                        series.color,
                    );
                    ui.label(RichText::new(&series.name).strong());
                    // Cut from the front, so the field's own name, which tells the rows apart, stays
                    let font = egui::TextStyle::Monospace.resolve(ui.style());
                    let fits = fit_tail(ui, &series.path, &font, ui.available_width());
                    ui.label(RichText::new(fits).monospace()).on_hover_text(format!("{} {}", series.name, series.path));
                });
            });
        });
    }
    ui.add_space(2.0);
    if plot.series.is_empty() {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("Nothing plotted yet").strong());
            ui.label(
                RichText::new("Open a topic's inspector, put the cursor on a number and press p.\nPress p on more fields to plot them together.")
                    .weak(),
            );
        });
        return action;
    }

    let (following, seconds) = (plot.paused_at.is_none(), plot.seconds);
    let single = drawn.len() == 1;
    let grid = visuals.weak_text_color().gamma_multiply(0.35);
    PlotWidget::new(("plot", pane))
        .show_background(false)
        .grid_color(grid)
        .grid_fade(0.7)
        .grid_spacing(egui::Rangef::new(48.0, 240.0))
        .cursor_color(visuals.weak_text_color())
        .set_margin_fraction(vec2(0.0, 0.12))
        .default_x_bounds(-seconds, seconds * RIGHT_ROOM)
        .x_grid_spacer(time_marks)
        .allow_drag(!following)
        .allow_zoom(!following)
        .allow_scroll(!following)
        .x_axis_formatter(|mark, _| time_text(mark))
        .y_axis_formatter(|mark, _| tick_text(mark))
        .label_formatter(|hover| match hover {
            HoverPosition::NearDataPoint { plot_name, position, .. } if !plot_name.is_empty() => {
                Some(format!("{plot_name}\n{}\n{} ago", value_text(position.y), seconds_text(-position.x)))
            }
            _ => None,
        })
        .show(ui, |plot_ui| {
            // Following, the window is always the last `seconds`, fitted to the values; paused, it's free to drag
            if following {
                plot_ui.set_plot_bounds_x(-seconds..=seconds * RIGHT_ROOM);
                plot_ui.set_auto_bounds([false, true]);
            }
            for series in &drawn {
                let label = format!("{} {}", series.name, series.path);
                let mut line =
                    Line::new(label.clone(), PlotPoints::from(series.points.clone())).color(series.color).width(2.0);
                // One series gets a soft wash down to its lowest value, which reads as a level at a glance
                if single && let Some((_, min, _)) = stats(&series.points) {
                    line = line.fill(min as f32).fill_alpha(0.12);
                }
                plot_ui.line(line);
                // The newest value, ringed in the panel color so it stands off the line
                if let Some(last) = series.points.last() {
                    plot_ui.points(
                        Points::new("", vec![*last])
                            .shape(MarkerShape::Circle)
                            .radius(6.0)
                            .filled(true)
                            .color(visuals.panel_fill),
                    );
                    plot_ui.points(
                        Points::new("", vec![*last])
                            .shape(MarkerShape::Circle)
                            .radius(4.0)
                            .filled(true)
                            .color(series.color),
                    );
                }
            }
        });
    if following {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
    }
    action
}

/// Room right of "now", as a share of the window, for the newest value's dot and the "now" label
const RIGHT_ROOM: f64 = 0.04;

/// Time ticks on 1, 2, 5 steps, about six across, with fainter ones between
fn time_marks(input: GridInput) -> Vec<GridMark> {
    let (min, max) = input.bounds;
    let span = (max - min).max(1e-9);
    let rough = span / 6.0;
    let power = 10f64.powf(rough.log10().floor());
    let step = [1.0, 2.0, 5.0, 10.0].into_iter().map(|m| m * power).find(|s| *s >= rough).unwrap_or(10.0 * power);
    let minor = step / 5.0;
    let mut marks = Vec::new();
    let mut n = (min / minor).floor() as i64;
    while (n as f64) * minor <= max {
        let value = n as f64 * minor;
        // A major tick is a whole number of steps; floating point puts it a hair off
        let major = ((value / step).round() * step - value).abs() < minor / 10.0;
        marks.push(GridMark { value, step_size: if major { step } else { minor } });
        n += 1;
    }
    marks
}

/// The end of `text` that fits in `width`, with an ellipsis in front when it's cut
fn fit_tail(ui: &Ui, text: &str, font: &egui::FontId, width: f32) -> String {
    let wide = |t: &str| ui.fonts_mut(|f| f.layout_no_wrap(t.to_owned(), font.clone(), Color32::WHITE).size().x);
    if wide(text) <= width {
        return text.to_owned();
    }
    let chars: Vec<char> = text.chars().collect();
    (1..chars.len())
        .map(|cut| format!("…{}", chars[cut..].iter().collect::<String>()))
        .find(|t| wide(t) <= width)
        .unwrap_or_else(|| "…".to_owned())
}

/// The newest value, and the lowest and highest in the window
fn stats(points: &[[f64; 2]]) -> Option<(f64, f64, f64)> {
    let last = points.last()?[1];
    let (min, max) =
        points.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), [_, y]| (lo.min(*y), hi.max(*y)));
    Some((last, min, max))
}

/// A value with as many digits as its size calls for: 1234, 98.6, 9.740, 0.0123
pub fn value_text(value: f64) -> String {
    let magnitude = value.abs();
    let decimals = if magnitude >= 1000.0 {
        0
    } else if magnitude >= 100.0 {
        1
    } else if magnitude >= 1.0 {
        3
    } else {
        4
    };
    let text = format!("{value:.decimals$}");
    // A clean zero rather than -0.0000
    if text.trim_start_matches('-').chars().all(|c| c == '0' || c == '.') { "0".to_owned() } else { text }
}

/// An axis tick with the decimals its spacing needs, without trailing zeros
fn tick_text(mark: GridMark) -> String {
    let decimals = (-mark.step_size.log10().floor()).max(0.0) as usize;
    let text = format!("{:.decimals$}", mark.value);
    if text.trim_start_matches('-').chars().all(|c| c == '0' || c == '.') { "0".to_owned() } else { text }
}

/// A time tick: "now" at the right edge, seconds (or minutes) before it elsewhere
fn time_text(mark: GridMark) -> String {
    if mark.value.abs() < mark.step_size / 2.0 {
        return "now".to_owned();
    }
    format!("−{}", seconds_text(-mark.value))
}

fn seconds_text(seconds: f64) -> String {
    if seconds >= 60.0 && (seconds % 60.0).abs() < 1e-6 {
        format!("{} min", seconds / 60.0)
    } else if seconds >= 10.0 || (seconds.fract()).abs() < 1e-6 {
        format!("{seconds:.0} s")
    } else if seconds >= 1.0 {
        format!("{seconds:.1} s")
    } else {
        format!("{:.0} ms", seconds * 1000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_through_windows() {
        let mut plot = Plot::new();
        plot.wider(true);
        assert_eq!(plot.seconds, 30.0);
        plot.wider(false);
        plot.wider(false);
        assert_eq!(plot.seconds, 5.0);
        for _ in 0..10 {
            plot.wider(true);
        }
        assert_eq!(plot.seconds, 300.0);
    }

    #[test]
    fn colors_stay_with_their_series() {
        let mut plot = Plot::new();
        plot.series = vec![(1, "a".into()), (1, "b".into()), (2, "c".into())];
        let first = plot.colors();
        assert_eq!(first, SERIES_COLORS[..3]);
        plot.series.remove(0);
        assert_eq!(plot.colors(), first[1..], "the others keep theirs");
        plot.series.push((3, "d".into()));
        assert_eq!(plot.colors()[2], SERIES_COLORS[0], "a new one takes the first free color");
    }

    #[test]
    fn time_ticks_step_by_one_two_five() {
        let majors = |span: f64| -> Vec<f64> {
            let marks = time_marks(GridInput { bounds: (-span, span * RIGHT_ROOM), base_step_size: 0.001 });
            let major = marks.iter().map(|m| m.step_size).fold(0.0, f64::max);
            marks.into_iter().filter(|m| m.step_size == major).map(|m| (m.value * 1000.0).round() / 1000.0).collect()
        };
        assert_eq!(majors(1.0), [-1.0, -0.8, -0.6, -0.4, -0.2, 0.0]);
        assert_eq!(majors(10.0), [-10.0, -8.0, -6.0, -4.0, -2.0, 0.0]);
        assert_eq!(majors(30.0), [-30.0, -20.0, -10.0, 0.0]);
    }

    #[test]
    fn values_and_ticks_read_cleanly() {
        assert_eq!(value_text(9.740341840812093), "9.740");
        assert_eq!(value_text(1234.5), "1234");
        assert_eq!(value_text(-0.00001), "0");
        assert_eq!(value_text(0.01234), "0.0123");
        assert_eq!(seconds_text(10.0), "10 s");
        assert_eq!(seconds_text(120.0), "2 min");
        assert_eq!(seconds_text(0.25), "250 ms");
        assert_eq!(seconds_text(2.5), "2.5 s");
    }
}
