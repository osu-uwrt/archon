//! Live plots of message fields over time. A field joins a plot from the inspector (`p`); `[` and `]` change how
//! much time shows, `f` pauses.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use archon_core::source::SourceId;
use archon_ros::TopicState;
use egui::{RichText, Ui};
use egui_plot::{Legend, Line, Plot as PlotWidget, PlotPoints};

use crate::logview::source_color;

/// The time windows `[` and `]` step through, in seconds
const WINDOWS: [f64; 8] = [1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0];

pub struct Plot {
    /// Each series as its topic echo and the field's path
    pub series: Vec<(SourceId, String)>,
    pub seconds: f64,
    /// When it was paused; the plot shows up to then
    pub paused_at: Option<Instant>,
}

impl Plot {
    pub fn new() -> Self {
        Self { series: Vec::new(), seconds: 10.0, paused_at: None }
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
}

pub enum Action {
    Remove(usize),
}

/// `names` gives each series' source name for the legend
pub fn show(
    ui: &mut Ui,
    pane: usize,
    plot: &mut Plot,
    topics: &HashMap<SourceId, Arc<Mutex<TopicState>>>,
    names: &dyn Fn(SourceId) -> String,
) -> Option<Action> {
    let mut action = None;
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Plot").strong());
        ui.label(RichText::new(format!("last {} s", plot.seconds)).weak());
        if plot.paused_at.is_some() {
            ui.label(RichText::new("PAUSED").small().color(ui.visuals().warn_fg_color));
        }
        ui.label(RichText::new("[ ] time · f pause · Ctrl+L clear").small().weak());
        for (i, (id, path)) in plot.series.iter().enumerate() {
            let color = source_color(*id + i);
            ui.label(RichText::new(format!("{} {path}", names(*id))).small().color(color));
            if ui.small_button("remove").clicked() {
                action = Some(Action::Remove(i));
            }
        }
    });
    ui.separator();
    if plot.series.is_empty() {
        ui.add_space(12.0);
        ui.label(RichText::new("Nothing plotted yet. In a topic's inspector, select a number and press p.").weak());
        return action;
    }

    let now = plot.paused_at.unwrap_or_else(Instant::now);
    let lines: Vec<(String, egui::Color32, Vec<[f64; 2]>)> = plot
        .series
        .iter()
        .enumerate()
        .map(|(i, (id, path))| {
            let points = topics
                .get(id)
                .map(|state| {
                    let state = state.lock().unwrap();
                    let trace = state.traced(path).map(|t| t.iter().copied().collect::<Vec<_>>()).unwrap_or_default();
                    trace
                        .into_iter()
                        .filter(|(t, _)| *t <= now)
                        .map(|(t, y)| [-(now - t).as_secs_f64(), y])
                        .filter(|[x, _]| *x >= -plot.seconds)
                        .collect()
                })
                .unwrap_or_default();
            (format!("{} {path}", names(*id)), source_color(*id + i), points)
        })
        .collect();

    let (following, seconds) = (plot.paused_at.is_none(), plot.seconds);
    PlotWidget::new(("plot", pane))
        .legend(Legend::default())
        .default_x_bounds(-seconds, 0.0)
        .allow_drag(plot.paused_at.is_some())
        .allow_zoom(plot.paused_at.is_some())
        .x_axis_formatter(|mark, _| format!("{:.1} s", mark.value))
        .show(ui, |plot_ui| {
            // Following, the window is always the last `seconds`, fitted to the values; paused, it's free to drag
            if following {
                plot_ui.set_plot_bounds_x(-seconds..=0.0);
                plot_ui.set_auto_bounds([false, true]);
            }
            for (name, color, points) in lines {
                plot_ui.line(Line::new(name, PlotPoints::from(points)).color(color));
            }
        });
    if plot.paused_at.is_none() {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
    }
    action
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
}
