//! The strip along the top that answers "is everything alive?": a chip per process and port, a chip per watched topic
//! with its rate against the rate it should have, and the count of errors not seen yet.

use archon_core::source::{SourceId, SourceKind, Status};
use archon_ros::Watcher;
use archon_sources::Entry;
use egui::{Color32, RichText, Ui, Visuals};

use crate::topics::rate_text;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Health {
    Good,
    /// Running, but not as it should: a slow topic, a port waiting for its device, a restart pending
    Warn,
    Bad,
    /// Stopped on purpose
    Off,
    /// Something archon is doing, like publishing on repeat, so it isn't forgotten
    Sending,
}

impl Health {
    fn colors(self, visuals: &Visuals) -> (Color32, Color32) {
        let (fill, text) = match self {
            Health::Good => (Color32::from_rgb(45, 110, 70), Color32::from_rgb(210, 245, 220)),
            Health::Warn => (Color32::from_rgb(120, 90, 20), Color32::from_rgb(255, 235, 190)),
            Health::Bad => (Color32::from_rgb(130, 40, 40), Color32::from_rgb(255, 215, 215)),
            Health::Off => (visuals.widgets.inactive.bg_fill, visuals.weak_text_color()),
            Health::Sending => (Color32::from_rgb(40, 80, 130), Color32::from_rgb(210, 230, 255)),
        };
        if visuals.dark_mode { (fill, text) } else { (text, fill) }
    }
}

pub fn source_health(entry: &Entry) -> Health {
    match (&entry.status, entry.restart_at) {
        (_, Some(_)) => Health::Warn,
        (Status::Running, _) => Health::Good,
        (Status::Waiting(_), _) => Health::Warn,
        (Status::Exited(Some(0)) | Status::Stopped, _) => Health::Off,
        (Status::Exited(_) | Status::Failed(_), _) => Health::Bad,
    }
}

/// A topic's rate against the rate it should have. No messages at all is bad.
pub fn rate_health(hz: Option<f64>, min_hz: Option<f64>) -> Health {
    match (hz, min_hz) {
        (None, _) => Health::Bad,
        (Some(hz), Some(min)) if hz < min => Health::Warn,
        _ => Health::Good,
    }
}

pub struct Watch {
    pub watcher: Watcher,
    pub min_hz: Option<f64>,
}

pub enum Click {
    Source(SourceId),
    Topic(String),
    Publisher(usize),
    Errors,
}

fn chip(ui: &mut Ui, text: &str, health: Health, hover: &str) -> bool {
    let (fill, color) = health.colors(ui.visuals());
    let label = RichText::new(format!(" {text} ")).small().color(color).background_color(fill);
    let response = ui.add(egui::Label::new(label).sense(egui::Sense::click())).on_hover_text(hover);
    response.clicked()
}

/// `unseen_errors` counts errors in output not on screen yet. `graph` describes the ROS link at the right end.
/// `sending` lists the publishers sending repeatedly, as their number and a label
pub fn show(
    ui: &mut Ui,
    entries: &[Entry],
    watches: &[Watch],
    sending: &[(usize, String)],
    alerts: &[(SourceId, String, String)],
    unseen_errors: u64,
    graph: &str,
) -> Option<Click> {
    let mut click = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for (id, entry) in entries.iter().enumerate().filter(|(_, e)| e.kind != SourceKind::Topic && !e.removed) {
            let health = source_health(entry);
            let hover = format!("{}: {}", entry.name, crate::logview::status_text(&entry.status));
            if chip(ui, &entry.name, health, &hover) {
                click = Some(Click::Source(id));
            }
        }
        for watch in watches {
            let hz = watch.watcher.hz();
            let health = rate_health(hz, watch.min_hz);
            let rate = match hz {
                Some(_) => rate_text(hz),
                None if watch.watcher.failed().is_some() => "can't listen".to_owned(),
                None => "no messages".to_owned(),
            };
            let text = format!("{} {rate}", watch.watcher.topic());
            let hover = match (watch.min_hz, watch.watcher.failed()) {
                (_, Some(why)) => why,
                (Some(min), None) => format!("should be at least {} Hz", rate_text(Some(min)).trim_end_matches(" Hz")),
                (None, None) => "any message is fine".to_owned(),
            };
            if chip(ui, &text, health, &hover) {
                click = Some(Click::Topic(watch.watcher.topic().to_owned()));
            }
        }
        for (index, label) in sending {
            if chip(ui, label, Health::Sending, "Click to open its form; Ctrl+R there stops it") {
                click = Some(Click::Publisher(*index));
            }
        }
        for (source, label, hover) in alerts {
            if chip(ui, label, Health::Bad, hover) {
                click = Some(Click::Source(*source));
            }
        }
        if unseen_errors > 0 {
            let s = if unseen_errors == 1 { "" } else { "s" };
            if chip(ui, &format!("{unseen_errors} new error{s}"), Health::Bad, "Show all logs at the first one") {
                click = Some(Click::Errors);
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(graph).small().weak());
        });
    });
    click
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn judges_rates() {
        assert_eq!(rate_health(None, None), Health::Bad);
        assert_eq!(rate_health(Some(5.0), None), Health::Good);
        assert_eq!(rate_health(Some(80.0), Some(100.0)), Health::Warn);
        assert_eq!(rate_health(Some(200.0), Some(100.0)), Health::Good);
    }
}
