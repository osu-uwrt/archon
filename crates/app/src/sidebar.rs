//! The sidebar: the views, every source with its status and unread marks, and every topic on the graph.

use std::time::{Duration, Instant};

use archon_core::command::Command;
use archon_core::source::{SourceId, SourceKind, Status};
use archon_sources::Entry;
use egui::{Color32, Context, Margin, RichText, Sense, Ui, UiBuilder};

use crate::app::{ArchonApp, Seen};
use crate::layout::{Dropped, View};
use crate::logview::{kind_tag, status_color, status_dot, status_text};
use crate::topics::rate_text;

/// What a click in the sidebar asks for
enum Action {
    Show(View),
    Beside(View),
    Run(Command, SourceId),
    Topic { name: String, ty: Option<String>, beside: bool },
    Send(usize),
    RemovePublisher(usize),
    Publish { topic: String, ty: String },
}

impl ArchonApp {
    pub(crate) fn sidebar(&mut self, ui: &mut Ui) {
        let mut action = None;
        let active = self.layout.active_view();
        let visible = self.layout.views().to_vec();
        let beside = |ui: &Ui| ui.input(|i| i.modifiers.command);

        heading(ui, "VIEWS", None);
        let unseen = self.unseen_errors();
        for (view, label, key) in [
            (View::Welcome, "Getting started", Command::ShowWelcome),
            (View::AllLogs, "All logs", Command::OpenAllLogs),
            (View::Topics, "Topics", Command::OpenTopics),
        ] {
            ui.horizontal(|ui| {
                let response = ui.selectable_label(active == view, label).interact(Sense::drag());
                response.dnd_set_drag_payload(Dropped::View(view));
                if response.clicked() {
                    action = Some(if beside(ui) { Action::Beside(view) } else { Action::Show(view) });
                }
                if view == View::AllLogs && unseen > 0 {
                    badge(ui, unseen, ui.visuals().error_fg_color, Color32::WHITE)
                        .on_hover_text("errors you haven't seen");
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(self.key_of(key).unwrap_or_default()).small().weak().monospace());
                });
            });
        }

        if self.publishers.iter().any(|p| !p.removed) {
            ui.add_space(10.0);
            heading(ui, "PUBLISH", self.key_of(Command::OpenPublish));
            for (i, publisher) in self.publishers.iter().enumerate().filter(|(_, p)| !p.removed) {
                ui.horizontal(|ui| {
                    let response = ui
                        .selectable_label(active == View::Publish(i), &publisher.name)
                        .on_hover_text(format!("{} {}", publisher.topic, publisher.ty))
                        .interact(Sense::drag());
                    response.dnd_set_drag_payload(Dropped::View(View::Publish(i)));
                    if response.clicked() {
                        let view = View::Publish(i);
                        action = Some(if beside(ui) { Action::Beside(view) } else { Action::Show(view) });
                    }
                    response.context_menu(|ui| {
                        let mut item = |ui: &mut Ui, label: &str, chosen: Action| {
                            if ui.button(label).clicked() {
                                action = Some(chosen);
                                ui.close();
                            }
                        };
                        item(ui, "Open", Action::Show(View::Publish(i)));
                        item(ui, "Open beside", Action::Beside(View::Publish(i)));
                        item(ui, if publisher.repeating { "Stop sending" } else { "Send" }, Action::Send(i));
                        ui.separator();
                        item(ui, "Remove", Action::RemovePublisher(i));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button(if publisher.repeating { "stop" } else { "send" }).clicked() {
                            action = Some(Action::Send(i));
                        }
                        if let Some(key) = &publisher.key {
                            ui.label(RichText::new(key).small().weak().monospace());
                        }
                        if publisher.repeating {
                            ui.label(RichText::new("sending").small().color(ui.visuals().warn_fg_color));
                        }
                    });
                });
            }
        }

        ui.add_space(10.0);
        heading(ui, "SOURCES", self.key_of(Command::QuickOpen));
        let mut hovered = None;
        for (number, (id, entry)) in self.sup.live().enumerate() {
            let selected = active == View::Source(id);
            let shown_elsewhere = !selected && visible.contains(&View::Source(id));
            let show_buttons = selected || self.hovered_row == Some(id);
            let mut button = None;
            let response = ui
                .scope_builder(UiBuilder::new().sense(Sense::click_and_drag()), |ui| {
                    button = source_row(ui, number, entry, self.seen[id], selected, shown_elsewhere, show_buttons);
                })
                .response;
            if response.hovered() || response.contains_pointer() {
                hovered = Some(id);
            }
            response.dnd_set_drag_payload(Dropped::View(View::Source(id)));
            if let Some(command) = button {
                action = Some(Action::Run(command, id));
            } else if response.clicked() {
                let view = View::Source(id);
                action = Some(if beside(ui) { Action::Beside(view) } else { Action::Show(view) });
            }
            response.context_menu(|ui| {
                let mut item = |ui: &mut Ui, label: &str, chosen: Action| {
                    if ui.button(label).clicked() {
                        action = Some(chosen);
                        ui.close();
                    }
                };
                item(ui, "Open", Action::Show(View::Source(id)));
                item(ui, "Open beside", Action::Beside(View::Source(id)));
                ui.separator();
                if entry.status.is_active() || entry.restart_at.is_some() {
                    item(ui, "Stop", Action::Run(Command::Stop, id));
                    item(ui, "Restart", Action::Run(Command::Restart, id));
                } else {
                    item(ui, "Start", Action::Run(Command::Start, id));
                }
                item(ui, "Clear output", Action::Run(Command::ClearView, id));
                ui.separator();
                item(ui, "Remove", Action::Run(Command::Remove, id));
            });
        }
        self.hovered_row = hovered;
        ui.add_space(4.0);
        ui.label(
            RichText::new(
                "Drag onto a pane to open it there, or to an edge for a new pane. Alt+drag a pane to move it.",
            )
            .small()
            .weak(),
        );

        ui.add_space(10.0);
        heading(ui, "TOPICS", self.ros.is_ok().then(|| self.graph.topics.len().to_string()));
        match &self.ros {
            Err(why) => {
                ui.label(RichText::new("ROS isn't running").small().color(ui.visuals().warn_fg_color))
                    .on_hover_text(why);
            }
            Ok(_) if self.graph.topics.is_empty() => {
                _ = ui.label(RichText::new("No topics on the graph yet").small().weak())
            }
            Ok(_) => {
                let rows = self.topic_rows();
                for row in &rows {
                    ui.horizontal(|ui| {
                        let label = RichText::new(&row.name).monospace().small();
                        let response =
                            ui.selectable_label(row.echoed, label).on_hover_text(&row.ty).interact(Sense::drag());
                        response
                            .dnd_set_drag_payload(Dropped::Topic { name: row.name.clone(), ty: Some(row.ty.clone()) });
                        if response.clicked() {
                            action = Some(Action::Topic {
                                name: row.name.clone(),
                                ty: Some(row.ty.clone()),
                                beside: beside(ui),
                            });
                        }
                        response.context_menu(|ui| {
                            let mut item = |ui: &mut Ui, label: &str, chosen: Action| {
                                if ui.button(label).clicked() {
                                    action = Some(chosen);
                                    ui.close();
                                }
                            };
                            let (name, ty) = (row.name.clone(), row.ty.clone());
                            item(ui, "Echo", Action::Topic { name: name.clone(), ty: Some(ty.clone()), beside: false });
                            item(
                                ui,
                                "Echo beside",
                                Action::Topic { name: name.clone(), ty: Some(ty.clone()), beside: true },
                            );
                            item(ui, "Publish to it", Action::Publish { topic: name, ty });
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let (text, color) = match (row.hz, row.publishers) {
                                (Some(_), _) => (rate_text(row.hz), ui.visuals().weak_text_color()),
                                (None, 0) => ("no pub".to_owned(), ui.visuals().warn_fg_color),
                                (None, n) => (format!("{n} pub"), ui.visuals().weak_text_color()),
                            };
                            ui.label(RichText::new(text).small().color(color));
                        });
                    });
                }
            }
        }

        let ctx = ui.ctx().clone();
        match action {
            Some(Action::Show(view)) => self.show_view(view, false),
            Some(Action::Beside(view)) => self.show_view(view, true),
            Some(Action::Run(command, id)) => self.source_command(command, id, &ctx),
            Some(Action::Topic { name, ty, beside }) => self.open_topic(&name, ty.as_deref(), beside),
            Some(Action::Send(i)) => self.run_action(i),
            Some(Action::RemovePublisher(i)) => self.ask_remove_publisher(i),
            Some(Action::Publish { topic, ty }) => self.open_publish(&topic, &ty),
            None => {}
        }
    }
}

fn heading(ui: &mut Ui, title: &str, right: Option<String>) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).small().weak());
        if let Some(right) = right {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(right).small().weak().monospace());
            });
        }
    });
    ui.add_space(2.0);
}

/// One source: number, status, name and unread marks, with its status detail underneath. Returns a button clicked.
fn source_row(
    ui: &mut Ui,
    number: usize,
    entry: &Entry,
    seen: Seen,
    selected: bool,
    shown_elsewhere: bool,
    show_buttons: bool,
) -> Option<Command> {
    let visuals = ui.visuals().clone();
    let fill = if selected {
        visuals.selection.bg_fill.gamma_multiply(0.6)
    } else if shown_elsewhere {
        visuals.selection.bg_fill.gamma_multiply(0.25)
    } else if show_buttons {
        visuals.widgets.hovered.weak_bg_fill
    } else {
        Color32::TRANSPARENT
    };
    let mut clicked = None;
    egui::Frame::NONE.fill(fill).corner_radius(4.0).inner_margin(Margin::symmetric(6, 4)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let number = if number < 9 { format!("{}", number + 1) } else { " ".to_owned() };
            ui.label(RichText::new(number).weak().monospace());
            status_dot(ui, status_color(&entry.status, &visuals));
            // Bold when there's output you haven't seen, like an unread chat
            let unread = entry.buffer.total() > seen.total;
            let name = RichText::new(&entry.name);
            ui.add(egui::Label::new(if unread || selected { name.strong() } else { name }).selectable(false));
            let errors = entry.buffer.errors() - seen.errors;
            let warnings = entry.buffer.warnings() - seen.warnings;
            if errors > 0 {
                badge(ui, errors, visuals.error_fg_color, Color32::WHITE).on_hover_text(format!("{errors} new errors"));
            } else if warnings > 0 {
                badge(ui, warnings, visuals.warn_fg_color, Color32::BLACK)
                    .on_hover_text(format!("{warnings} new warnings"));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !show_buttons {
                    ui.label(RichText::new(kind_tag(entry.kind)).small().weak());
                    return;
                }
                let buttons: &[(&str, Command)] = if entry.status.is_active() || entry.restart_at.is_some() {
                    &[("stop", Command::Stop), ("restart", Command::Restart)]
                } else {
                    &[("start", Command::Start)]
                };
                for &(label, command) in buttons {
                    if ui.small_button(label).clicked() {
                        clicked = Some(command);
                    }
                }
            });
        });
        ui.label(RichText::new(detail(entry, ui.ctx())).small().weak());
    });
    clicked
}

pub(crate) fn badge(ui: &mut Ui, count: u64, fill: Color32, text_color: Color32) -> egui::Response {
    let text = if count > 99 { "99+".to_owned() } else { count.to_string() };
    ui.label(RichText::new(format!(" {text} ")).small().strong().color(text_color).background_color(fill))
}

/// The line under a source's name: how long it's been up, its rate, when it restarts, or why it stopped
fn detail(entry: &Entry, ctx: &Context) -> String {
    if let Some(at) = entry.restart_at {
        ctx.request_repaint_after(Duration::from_millis(250));
        let left = at.saturating_duration_since(Instant::now()).as_secs_f32().ceil();
        return format!("{} · restarting in {left}s", status_text(&entry.status));
    }
    match &entry.status {
        Status::Running => {
            ctx.request_repaint_after(Duration::from_millis(500));
            match entry.detail() {
                Some(detail) => detail,
                None if entry.kind == SourceKind::Topic => "running".to_owned(),
                None => format!("running · {}", uptime(entry.started.elapsed())),
            }
        }
        status => status_text(status),
    }
}

pub(crate) fn uptime(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m{:02}s", secs / 60, secs % 60),
        _ => format!("{}h{:02}m", secs / 3600, secs % 3600 / 60),
    }
}
