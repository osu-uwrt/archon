//! What archon shows with nothing open yet: what it's for, what it found on this machine, and the first steps as
//! buttons with their keys, so the keys are learned by using them.

use archon_core::command::Command;
use egui::{RichText, ScrollArea, Ui};

use crate::bench::{Agent, Port};

pub struct Facts<'a> {
    pub agent: &'a Result<Agent, String>,
    pub serial: &'a [Port],
    pub can: &'a [Port],
    /// firmware_simulator's checkout, or why it wasn't found
    pub simulator: Result<String, String>,
    pub ros: Result<(), &'a str>,
    /// The profile file, if there is one
    pub profile: Option<&'a str>,
    /// What's been opened since and isn't in a profile yet
    pub unsaved: usize,
}

/// `key` gives a command's keys. Returns a command whose button was clicked.
pub fn show(ui: &mut Ui, facts: &Facts<'_>, key: &dyn Fn(Command) -> String) -> Option<Command> {
    let visuals = ui.visuals().clone();
    let mut clicked = None;
    ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        ui.set_max_width(720.0);
        ui.add_space(12.0);
        ui.heading("Getting started");
        ui.label("archon runs a firmware bench in one window: the micro-ROS agent, the boards' serial consoles, CAN buses, and every ROS topic.");
        ui.add_space(14.0);

        let mut n = 0;
        let mut step = |ui: &mut Ui, title: &str, body: &str, command: Command, button: &str| {
            n += 1;
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{n}")).heading().weak());
                ui.vertical(|ui| {
                    ui.label(RichText::new(title).strong());
                    ui.label(RichText::new(body).weak());
                    ui.horizontal(|ui| {
                        if ui.button(button).clicked() {
                            clicked = Some(command);
                        }
                        let keys = key(command);
                        if !keys.is_empty() {
                            ui.label(RichText::new(format!("or {keys}")).small().weak().monospace());
                        }
                    });
                });
            });
            ui.add_space(12.0);
        };

        let agent_note = match facts.agent {
            Ok(agent) => format!("Pick the board's port, or a CAN bus, and archon runs the agent there. Found: {}.", agent.found),
            Err(why) => why.clone(),
        };
        step(ui, "Start the micro-ROS agent", &agent_note, Command::StartAgent, "Start the agent");
        let ports = match facts.serial.len() {
            0 => "No serial ports are plugged in right now.".to_owned(),
            1 => "1 serial port is plugged in.".to_owned(),
            n => format!("{n} serial ports are plugged in."),
        };
        step(
            ui,
            "Open a board's serial console",
            &format!("Like screen, but it reconnects when the board is reflashed. {ports}"),
            Command::OpenSerial,
            "Open a console",
        );
        step(
            ui,
            "Flash a board",
            "With titan_firmware's upload_tool: pick a build, then the application or the whole image. The console comes \
             back by itself once the board does.",
            Command::Flash,
            "Flash",
        );
        if !facts.can.is_empty() {
            let buses: Vec<String> = facts.can.iter().map(Port::label).collect();
            step(ui, "Watch a CAN bus", &format!("Like candump. Buses here: {}.", buses.join(", ")), Command::OpenCan, "Dump a bus");
        }
        let ros = match facts.ros {
            Ok(()) => "Every topic with its rate. Enter on one inspects it, p plots a field, P publishes to it.".to_owned(),
            Err(why) => format!("ROS isn't running ({why}), so topics won't show."),
        };
        let simulator = match &facts.simulator {
            Ok(checkout) => format!(
                "No board on the desk? Runs a titan_firmware build in firmware_simulator ({checkout}), with the agent \
                 and its console attached, like a board on USB, or a robot file with several boards ({} builds one). \
                 A second copy of a board opens in a window of its own, so the two boards' topics don't mix.",
                key(Command::NewRobot)
            ),
            Err(why) => format!("No board on the desk? Runs a titan_firmware build in firmware_simulator. {why}"),
        };
        step(ui, "Or simulate a board or a robot", &simulator, Command::Simulate, "Simulate");
        step(ui, "Look at the topics", &ros, Command::OpenTopics, "Show topics");
        let save = match (facts.profile, facts.unsaved) {
            (Some(path), 0) => format!("This bench comes from {path}."),
            (Some(path), n) => format!("{n} opened since aren't in {path} yet."),
            (None, _) => "Writes archon.toml here, so next time `archon` in this folder brings it all back.".to_owned(),
        };
        step(ui, "Keep this bench", &save, Command::SaveBench, "Save the bench");

        ui.separator();
        ui.add_space(6.0);
        ui.label(RichText::new("Getting around").strong());
        for (command, what) in [
            (Command::QuickOpen, "go to any source, topic or view"),
            (Command::OpenPalette, "every command, with its keys"),
            (Command::PublishAny, "publish to any topic"),
            (Command::OpenAllLogs, "every log merged in time order"),
            (Command::Remove, "take the selected source off the bench"),
            (Command::ShowKeys, "all the keys"),
            (Command::ShowWelcome, "this page again"),
        ] {
            ui.horizontal(|ui| {
                ui.label(RichText::new(key(command)).monospace().color(visuals.strong_text_color()));
                ui.label(RichText::new(what).weak());
            });
        }
        ui.label(RichText::new("Esc goes back. Drag things from the sidebar onto panes; Alt+drag a pane to move it.").weak());
    });
    clicked
}
