//! Each board's health, from the `riptide_msgs2/msg/FirmwareStatus` its firmware publishes: uptime, faults by name,
//! kill switch state, version, and whether it's rebooted (its uptime went back) or gone quiet.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use archon_msg::{Registry, TypeName, Value, cdr};
use archon_ros::{Delivery, Graph, Ros};
use egui::{Color32, RichText, Ui};

pub const STATUS_TYPE: &str = "riptide_msgs2/msg/FirmwareStatus";
/// A board that hasn't been heard from for this long is called silent (they publish about once a second)
const SILENT: Duration = Duration::from_secs(3);
/// How long a reboot stays marked
const RECENT: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Board {
    /// Like `talos/ivc_board`
    pub name: String,
    pub client_id: u64,
    pub bus_id: u64,
    pub topic: String,
    pub uptime_ms: u64,
    pub faults: u64,
    pub kill_enabled: u64,
    pub kill_asserting: u64,
    pub kill_needs_update: u64,
    pub kill_timed_out: u64,
    pub version: String,
    pub heard: Option<Instant>,
    pub reboots: u32,
    pub rebooted: Option<Instant>,
}

impl Board {
    fn key(&self) -> (String, u64, u64) {
        (self.name.clone(), self.bus_id, self.client_id)
    }

    /// Reads a FirmwareStatus message
    fn read(topic: &str, message: &Value) -> Option<Board> {
        let number = |field: &str| message.get(field).and_then(Value::as_f64).map(|n| n as u64);
        let name = match message.get("board_name")? {
            Value::String(name) => name.clone(),
            _ => return None,
        };
        Some(Board {
            name,
            client_id: number("client_id").unwrap_or_default(),
            bus_id: number("bus_id").unwrap_or_default(),
            topic: topic.to_owned(),
            uptime_ms: number("uptime_ms")?,
            faults: number("faults").unwrap_or_default(),
            kill_enabled: number("kill_switches_enabled").unwrap_or_default(),
            kill_asserting: number("kill_switches_asserting_kill").unwrap_or_default(),
            kill_needs_update: number("kill_switches_needs_update").unwrap_or_default(),
            kill_timed_out: number("kill_switches_timed_out").unwrap_or_default(),
            version: format!(
                "{}.{} ({})",
                number("version_major").unwrap_or_default(),
                number("version_minor").unwrap_or_default(),
                match number("version_release_type") {
                    Some(0) => "dev",
                    Some(1) => "release",
                    Some(2) => "clean",
                    _ => "?",
                }
            ),
            ..Default::default()
        })
    }

    pub fn silent(&self) -> bool {
        self.heard.is_none_or(|at| at.elapsed() > SILENT)
    }

    /// The state in one word, worst first
    pub fn state(&self) -> &'static str {
        if self.silent() {
            "SILENT"
        } else if self.faults != 0 {
            "FAULT"
        } else if self.kill_asserting != 0 {
            "KILLED"
        } else if self.rebooted.is_some_and(|at| at.elapsed() < RECENT) {
            "REBOOTED"
        } else {
            "OK"
        }
    }
}

/// A FirmwareStatus topic listened to (for as long as the window is open), and what's arrived since the last poll
struct Listening {
    heard: Arc<Mutex<Vec<Vec<u8>>>>,
}

#[derive(Default)]
pub struct Boards {
    pub boards: Vec<Board>,
    listening: HashMap<String, Listening>,
    /// Each board type's fault names, from its titan_firmware project, looked up once
    fault_names: HashMap<String, Vec<(u32, String)>>,
    titan: Option<PathBuf>,
}

impl Boards {
    /// `titan` is the titan_firmware checkout that names each board's faults
    pub fn new(titan: Option<PathBuf>) -> Self {
        Self { titan, ..Default::default() }
    }

    /// Listens to any new FirmwareStatus topic, takes in what's arrived, and says what changed for the worse
    pub fn poll(&mut self, ros: &Ros, graph: &Graph, registry: &Arc<Mutex<Registry>>) -> Vec<String> {
        for topic in graph.topics.iter().filter(|t| t.types.iter().any(|ty| ty == STATUS_TYPE)) {
            if self.listening.contains_key(&topic.name) {
                continue;
            }
            let heard: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
            let into = heard.clone();
            ros.subscribe(
                &topic.name,
                STATUS_TYPE,
                Box::new(move |delivery| {
                    if let Delivery::Message(bytes) = delivery {
                        into.lock().unwrap().push(bytes.to_vec());
                    }
                }),
            );
            self.listening.insert(topic.name.clone(), Listening { heard });
        }
        let mut news = Vec::new();
        let Some(name) = TypeName::parse(STATUS_TYPE) else { return news };
        let arrived: Vec<(String, Vec<Vec<u8>>)> = self
            .listening
            .iter()
            .map(|(topic, l)| (topic.clone(), std::mem::take(&mut *l.heard.lock().unwrap())))
            .filter(|(_, messages)| !messages.is_empty())
            .collect();
        if arrived.is_empty() {
            return news;
        }
        let mut registry = registry.lock().unwrap();
        if registry.load(&name).is_err() {
            return news;
        }
        for (topic, messages) in arrived {
            for bytes in messages {
                let Some(fresh) = cdr::decode(&registry, &name, &bytes).ok().and_then(|v| Board::read(&topic, &v))
                else {
                    continue;
                };
                news.extend(self.update(fresh));
            }
        }
        news
    }

    fn update(&mut self, mut fresh: Board) -> Option<String> {
        fresh.heard = Some(Instant::now());
        let Some(at) = self.boards.iter().position(|b| b.key() == fresh.key()) else {
            self.boards.push(fresh);
            self.boards.sort_by_key(Board::key);
            return None;
        };
        let before = self.boards[at].clone();
        let mut news = None;
        // Uptime going back is a reboot
        if fresh.uptime_ms + 500 < before.uptime_ms {
            fresh.reboots = before.reboots + 1;
            fresh.rebooted = Some(Instant::now());
            news = Some(format!("{} rebooted (it had been up {})", fresh.name, uptime(before.uptime_ms)));
        } else {
            fresh.reboots = before.reboots;
            fresh.rebooted = before.rebooted;
        }
        let new_faults = fresh.faults & !before.faults;
        if new_faults != 0 {
            let names = self.fault_names(&fresh.name, new_faults);
            news = Some(format!("{} raised {}", fresh.name, names.join(", ")));
        }
        self.boards[at] = fresh;
        news
    }

    /// The names of the faults set in `bits`, from the board's titan_firmware project when it can be found
    pub fn fault_names(&mut self, board: &str, bits: u64) -> Vec<String> {
        if !self.fault_names.contains_key(board) {
            let names = self.titan.as_deref().map(|titan| project_faults(titan, board)).unwrap_or_default();
            self.fault_names.insert(board.to_owned(), names);
        }
        let known = &self.fault_names[board];
        (0..32)
            .filter(|bit| bits & (1 << bit) != 0)
            .map(|bit| match bit {
                0 => "WATCHDOG_RESET".to_owned(),
                1 => "WATCHDOG_WARNING".to_owned(),
                _ => known.iter().find(|(id, _)| *id == bit).map_or(format!("fault {bit}"), |(_, n)| n.clone()),
            })
            .collect()
    }
}

/// A board type's fault IDs and names: the titan_firmware project whose CMakeLists sets `PICO_BOARD` to it, read
/// from its `include/safety_interface.h` (`DEF(FAULT_ADC_ERROR, 6)`)
fn project_faults(titan: &Path, board: &str) -> Vec<(u32, String)> {
    for group in std::fs::read_dir(titan).into_iter().flatten().flatten() {
        for project in std::fs::read_dir(group.path()).into_iter().flatten().flatten() {
            let project = project.path();
            let Ok(cmake) = std::fs::read_to_string(project.join("CMakeLists.txt")) else { continue };
            if !cmake.contains(&format!("set(PICO_BOARD {board})")) {
                continue;
            }
            let Ok(header) = std::fs::read_to_string(project.join("include/safety_interface.h")) else { continue };
            return parse_faults(&header);
        }
    }
    Vec::new()
}

fn parse_faults(header: &str) -> Vec<(u32, String)> {
    header
        .split("DEF(")
        .skip(1)
        .filter_map(|rest| {
            let (name, rest) = rest.split_once(',')?;
            let id = rest.split(')').next()?.trim().parse().ok()?;
            Some((id, name.trim().trim_start_matches("FAULT_").to_owned()))
        })
        .collect()
}

pub fn uptime(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..60 => format!("{:.1} s", ms as f64 / 1000.0),
        60..3600 => format!("{} min {} s", s / 60, s % 60),
        _ => format!("{} h {} min", s / 3600, (s % 3600) / 60),
    }
}

/// The switches set in a kill switch mask, like "switch 0" or "switches 0, 2"
fn switches(mask: u64) -> String {
    let set: Vec<String> = (0..32).filter(|b| mask & (1 << b) != 0).map(|b| b.to_string()).collect();
    format!("{} {}", if set.len() == 1 { "switch" } else { "switches" }, set.join(", "))
}

pub fn show(ui: &mut Ui, pane: usize, boards: &mut Boards) {
    let visuals = ui.visuals().clone();
    ui.horizontal(|ui| {
        ui.label(RichText::new("Boards").strong());
        ui.label(RichText::new(format!("{} reporting FirmwareStatus", boards.boards.len())).weak());
    });
    ui.separator();
    if boards.boards.is_empty() {
        ui.add_space(12.0);
        ui.label(
            RichText::new(format!(
                "No board has reported yet. titan_firmware boards publish {STATUS_TYPE} once their agent has a session."
            ))
            .weak(),
        );
        return;
    }
    let green = Color32::from_rgb(0x19, 0x9e, 0x70);
    let rows: Vec<Board> = boards.boards.clone();
    egui::ScrollArea::vertical().id_salt(("boards", pane)).auto_shrink(false).show(ui, |ui| {
        // A card a board, so it reads in a narrow pane as well as a wide one
        for board in &rows {
            let state = board.state();
            let color = match state {
                "OK" => green,
                "REBOOTED" | "KILLED" => visuals.warn_fg_color,
                _ => visuals.error_fg_color,
            };
            egui::Frame::new().fill(visuals.faint_bg_color).corner_radius(6.0).inner_margin(8.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(format!(" {state} "))
                            .small()
                            .strong()
                            .color(visuals.panel_fill)
                            .background_color(color),
                    );
                    ui.label(RichText::new(&board.name).strong());
                    ui.label(RichText::new(&board.version).small().weak());
                    let heard = board.heard.map_or("never heard".to_owned(), |at| {
                        format!("heard {} ago", uptime(at.elapsed().as_millis() as u64))
                    });
                    let heard_color = if board.silent() { visuals.error_fg_color } else { visuals.weak_text_color() };
                    ui.label(RichText::new(heard).small().color(heard_color));
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(format!("up {}", uptime(board.uptime_ms))).monospace());
                    if board.reboots > 0 {
                        let ago =
                            board.rebooted.map(|at| format!(", last {} ago", uptime(at.elapsed().as_millis() as u64)));
                        let text = format!(
                            "{} reboot{}{}",
                            board.reboots,
                            if board.reboots == 1 { "" } else { "s" },
                            ago.unwrap_or_default()
                        );
                        ui.label(RichText::new(text).color(visuals.warn_fg_color));
                    }
                    ui.label(
                        RichText::new(format!("client {} · bus {} · {}", board.client_id, board.bus_id, board.topic))
                            .small()
                            .weak(),
                    );
                });
                ui.horizontal_wrapped(|ui| {
                    if board.faults == 0 {
                        ui.label(RichText::new("no faults").weak());
                    } else {
                        let names = boards.fault_names(&board.name, board.faults);
                        ui.label(RichText::new(names.join(", ")).color(visuals.error_fg_color));
                    }
                    ui.label(RichText::new("·").weak());
                    let mut kill = Vec::new();
                    if board.kill_asserting != 0 {
                        kill.push(format!("asserting ({})", switches(board.kill_asserting)));
                    }
                    if board.kill_timed_out != 0 {
                        kill.push(format!("timed out ({})", switches(board.kill_timed_out)));
                    }
                    if board.kill_needs_update != 0 {
                        kill.push(format!("needs update ({})", switches(board.kill_needs_update)));
                    }
                    let clear = if board.kill_enabled == 0 { "none enabled" } else { "clear" };
                    let text = if kill.is_empty() { clear.to_owned() } else { kill.join(", ") };
                    let kill_color = if kill.is_empty() { visuals.text_color() } else { visuals.warn_fg_color };
                    ui.label(RichText::new(format!("kill switch {text}")).color(kill_color));
                });
            });
            ui.add_space(6.0);
        }
    });
    ui.ctx().request_repaint_after(Duration::from_millis(500));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(name: &str, uptime_ms: u64, faults: u64) -> Value {
        Value::Struct(vec![
            ("board_name".into(), Value::String(name.into())),
            ("bus_id".into(), Value::UInt(0)),
            ("client_id".into(), Value::UInt(2)),
            ("version_major".into(), Value::UInt(1)),
            ("version_minor".into(), Value::UInt(0)),
            ("version_release_type".into(), Value::UInt(1)),
            ("uptime_ms".into(), Value::UInt(uptime_ms)),
            ("faults".into(), Value::UInt(faults)),
            ("kill_switches_enabled".into(), Value::UInt(1)),
            ("kill_switches_asserting_kill".into(), Value::UInt(0)),
            ("kill_switches_needs_update".into(), Value::UInt(0)),
            ("kill_switches_timed_out".into(), Value::UInt(0)),
        ])
    }

    #[test]
    fn notices_reboots_and_new_faults() {
        let mut boards = Boards::default();
        let read = |v: &Value| Board::read("/talos/state/firmware", v).unwrap();
        assert_eq!(boards.update(read(&status("mk2/esc_board", 60_000, 0))), None);
        assert_eq!(boards.update(read(&status("mk2/esc_board", 61_000, 0))), None);
        assert_eq!(boards.boards[0].state(), "OK");
        let said = boards.update(read(&status("mk2/esc_board", 400, 0))).unwrap();
        assert_eq!(said, "mk2/esc_board rebooted (it had been up 1 min 1 s)");
        assert_eq!(boards.boards[0].reboots, 1);
        assert_eq!(boards.boards[0].state(), "REBOOTED");
        let said = boards.update(read(&status("mk2/esc_board", 1400, 0b1001))).unwrap();
        assert_eq!(said, "mk2/esc_board raised WATCHDOG_RESET, fault 3");
        assert_eq!(boards.boards[0].state(), "FAULT");
    }

    #[test]
    fn reads_fault_names_from_the_safety_header() {
        let header = "#define SAFETY_FAULTS(DEF) \\\n    XLIST_OF_LIBSAFETY_FAULTS(DEF) \\\n    DEF(FAULT_CAN_INTERNAL_ERROR, 2) \\\n    DEF(FAULT_ROS_ERROR, 3)\n";
        assert_eq!(parse_faults(header), [(2, "CAN_INTERNAL_ERROR".to_owned()), (3, "ROS_ERROR".to_owned())]);
    }

    #[test]
    fn names_a_real_boards_faults_if_titan_firmware_is_here() {
        let Some(titan) = crate::robot::titan_firmware_dir(&dirs::home_dir().unwrap()) else { return };
        let faults = project_faults(&titan, "mk2/esc_board");
        if !faults.is_empty() {
            assert!(faults.iter().any(|(_, n)| n == "ROS_ERROR"), "{faults:?}");
        }
    }
}
