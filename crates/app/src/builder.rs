//! A view for writing a firmware_simulator robot file: boards picked from the firmware builds, each matched to its
//! board file, with buses and positions filled in, the file shown as it's built, then saved and run.

use std::path::{Path, PathBuf};

use egui::{RichText, ScrollArea, TextEdit, Ui};

use crate::bench::{Firmware, Simulator};
use crate::robot::Transport;

pub struct Builder {
    /// The file's name, without `.toml`
    pub name: String,
    /// The simulator's checkout; the file goes in its `robots/`
    pub checkout: PathBuf,
    /// An acoustic environment from `environments/`, by name
    pub environment: Option<String>,
    pub environments: Vec<String>,
    pub boards: Vec<BuilderBoard>,
    pub selected: usize,
    /// Saving would replace this file; saving again does
    pub replacing: Option<PathBuf>,
}

pub struct BuilderBoard {
    pub name: String,
    pub firmware: Firmware,
    /// The board file the image names, if one does
    pub board: Option<PathBuf>,
    pub bus: String,
    /// Metres, as typed: "2, 6, 1"
    pub position: String,
    pub transport: Transport,
}

pub enum Action {
    AddBoard,
    Save,
    SaveAndRun,
}

impl Builder {
    pub fn new(simulator: &Simulator) -> Self {
        let mut environments: Vec<String> = std::fs::read_dir(simulator.checkout.join("environments"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.path().file_stem().map(|s| s.to_string_lossy().into_owned()))
            .collect();
        environments.sort();
        Self {
            name: String::new(),
            checkout: simulator.checkout.clone(),
            environment: None,
            environments,
            boards: Vec::new(),
            selected: 0,
            replacing: None,
        }
    }

    /// Adds a board running `firmware`, named, bused and placed so it runs as it is
    pub fn add(&mut self, firmware: Firmware, board: Option<PathBuf>) {
        let transport = crate::robot::transport(&firmware.image);
        let base: String = firmware
            .name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
            .collect::<String>()
            .trim_end_matches("_with_bl")
            .to_owned();
        let name = (1..)
            .map(|n| if n == 1 { base.clone() } else { format!("{base}_{n}") })
            .find(|n| !self.boards.iter().any(|b| b.name == *n))
            .expect("unbounded");
        // An IVC board is a vehicle of its own in the water: its own bus, 5 m along from the last one
        let acoustic = board.as_deref().is_some_and(|b| b.to_string_lossy().contains("ivc"));
        let (bus, position) = if acoustic {
            if self.environment.is_none() && self.environments.iter().any(|e| e == "pool") {
                self.environment = Some("pool".to_owned());
            }
            let along = self.boards.iter().filter(|b| b.is_acoustic()).count();
            let bus = match along {
                0 => "talos".to_owned(),
                1 => "surface".to_owned(),
                _ => name.clone(),
            };
            (bus, format!("{}, 6, 1", 2 + 5 * along))
        } else if transport == Transport::Can {
            ("internal".to_owned(), String::new())
        } else {
            (name.clone(), String::new())
        };
        if self.name.is_empty() {
            self.name = format!("my_{base}");
        } else if self.boards.len() == 1 && self.name == format!("my_{}", self.boards[0].firmware_stem()) {
            self.name = format!("{}_test", self.name);
        }
        self.boards.push(BuilderBoard { name, firmware, board, bus, position, transport });
        self.selected = self.boards.len() - 1;
        self.replacing = None;
    }

    pub fn remove_selected(&mut self) {
        if self.selected < self.boards.len() {
            self.boards.remove(self.selected);
            self.selected = self.selected.min(self.boards.len().saturating_sub(1));
        }
    }

    pub fn path(&self) -> PathBuf {
        self.checkout.join("robots").join(format!("{}.toml", self.name.trim()))
    }

    /// The robot file, or what's in the way of writing it
    pub fn toml(&self, home: &Path) -> Result<String, String> {
        let name = self.name.trim();
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            return Err("name the file with letters, digits, _ and -".to_owned());
        }
        if self.boards.is_empty() {
            return Err("add a board (a)".to_owned());
        }
        for (i, board) in self.boards.iter().enumerate() {
            if board.name.trim().is_empty() || self.boards[..i].iter().any(|b| b.name.trim() == board.name.trim()) {
                return Err(format!("board names have to be there and different ({:?})", board.name));
            }
            if board.board.is_none() {
                return Err(format!(
                    "{}: no board file in {}/boards names this image",
                    board.name,
                    self.checkout.display()
                ));
            }
            if self.environment.is_some() && board.is_acoustic() && position(&board.position).is_none() {
                return Err(format!("{}: a position is three numbers in metres, like 2, 6, 1", board.name));
            }
        }
        // Paths as a person would write them: the checkout's own relative to robots/, the rest under ${HOME}
        let relative = |path: &Path| match path.strip_prefix(&self.checkout) {
            Ok(inside) => format!("../{}", inside.display()),
            Err(_) => match path.strip_prefix(home) {
                Ok(under) => format!("${{HOME}}/{}", under.display()),
                Err(_) => path.display().to_string(),
            },
        };
        let mut out = format!(
            "# Built in archon. Run it there (Space n f), or:\n#   firmware_simulator robot robots/{name}.toml --realtime\n"
        );
        if let Some(environment) = &self.environment {
            out.push_str(&format!("[environment]\nfile = \"../environments/{environment}.toml\"\n"));
        }
        let mut buses: Vec<&str> = Vec::new();
        for board in &self.boards {
            let bus = board.bus.trim();
            if !bus.is_empty() && !buses.contains(&bus) {
                buses.push(bus);
            }
        }
        for bus in buses {
            out.push_str(&format!("\n[[bus]]\nname = \"{bus}\"\n"));
        }
        for board in &self.boards {
            out.push_str(&format!(
                "\n[[board]]\nname = \"{}\"\nboard = \"{}\"\nelf = \"{}\"\n",
                board.name.trim(),
                relative(board.board.as_deref().expect("checked")),
                relative(&board.firmware.image),
            ));
            if board.firmware.image.to_string_lossy().ends_with("_with_bl.uf2") {
                out.push_str("rom_boot = true\n");
            }
            if !board.bus.trim().is_empty() {
                out.push_str(&format!("bus = \"{}\"\n", board.bus.trim()));
            }
            if self.environment.is_some()
                && let Some([x, y, z]) = position(&board.position)
            {
                out.push_str(&format!("position_m = [{x:?}, {y:?}, {z:?}]\n"));
            }
        }
        Ok(out)
    }

    /// Writes the file, unless that would replace one, which it says first; returns where it went
    pub fn save(&mut self, home: &Path) -> Result<PathBuf, String> {
        let text = self.toml(home)?;
        let path = self.path();
        if path.exists() && self.replacing.as_deref() != Some(path.as_path()) {
            self.replacing = Some(path.clone());
            return Err(format!("{} exists; save again to replace it", path.display()));
        }
        std::fs::write(&path, text).map_err(|e| format!("couldn't write {}: {e}", path.display()))?;
        self.replacing = None;
        Ok(path)
    }
}

impl BuilderBoard {
    fn is_acoustic(&self) -> bool {
        self.board.as_deref().is_some_and(|b| b.to_string_lossy().contains("ivc"))
    }

    fn firmware_stem(&self) -> String {
        self.firmware.name.to_ascii_lowercase().chars().filter(char::is_ascii_alphanumeric).collect()
    }
}

/// "2, 6, 1" or "2 6 1" as metres
fn position(text: &str) -> Option<[f64; 3]> {
    let parts: Vec<f64> = text
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|p| !p.is_empty())
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    parts.try_into().ok()
}

/// `keys` gives the save, save-and-run and add keys for the hints
pub fn show(ui: &mut Ui, builder: &mut Builder, home: &Path, keys: [&str; 3]) -> Option<Action> {
    let visuals = ui.visuals().clone();
    let mut action = None;
    let typing = ui.ctx().text_edit_focused();
    if !typing && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::A)) {
        action = Some(Action::AddBoard);
    }
    let [save_key, run_key, add_key] = keys;
    ui.horizontal(|ui| {
        ui.label(RichText::new("New robot").strong());
        ui.label(RichText::new("several boards in one simulation, on shared buses and in one body of water").weak());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(format!(
                    "{add_key} add · j/k pick · Delete remove · {save_key} save · {run_key} save and run"
                ))
                .small()
                .weak(),
            );
        });
    });
    ui.separator();

    egui::Grid::new("robot-file").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("File");
        ui.horizontal(|ui| {
            ui.add(TextEdit::singleline(&mut builder.name).desired_width(220.0).hint_text("my_robot"));
            ui.label(RichText::new(format!(".toml in {}", builder.checkout.join("robots").display())).weak());
        });
        ui.end_row();
        ui.label("Water");
        egui::ComboBox::from_id_salt("robot-environment")
            .selected_text(builder.environment.clone().unwrap_or_else(|| "none (no acoustics)".to_owned()))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut builder.environment, None, "none (no acoustics)");
                for environment in builder.environments.clone() {
                    ui.selectable_value(&mut builder.environment, Some(environment.clone()), environment);
                }
            });
        ui.end_row();
    });
    ui.add_space(8.0);

    if builder.boards.is_empty() {
        ui.label(
            RichText::new("No boards yet. Add one, and archon matches it to its board file and fills in the rest.")
                .weak(),
        );
    } else {
        let mut remove = None;
        egui::Grid::new("robot-boards").num_columns(7).striped(true).spacing([12.0, 4.0]).show(ui, |ui| {
            for heading in ["Name", "Firmware", "Board", "micro-ROS", "Bus", "Position (m)", ""] {
                ui.label(RichText::new(heading).small().weak());
            }
            ui.end_row();
            for (i, board) in builder.boards.iter_mut().enumerate() {
                let selected = i == builder.selected;
                let name = ui.add_sized([130.0, 20.0], TextEdit::singleline(&mut board.name));
                if name.gained_focus() {
                    builder.selected = i;
                }
                let firmware = RichText::new(&board.firmware.name);
                ui.label(if selected { firmware.strong() } else { firmware });
                match &board.board {
                    Some(path) => {
                        let short = path
                            .strip_prefix(builder.checkout.join("boards"))
                            .map(|p| p.with_extension("").display().to_string())
                            .unwrap_or_else(|_| path.display().to_string());
                        ui.label(RichText::new(short).monospace());
                    }
                    None => _ = ui.label(RichText::new("no match").color(visuals.error_fg_color)),
                }
                let transport = match board.transport {
                    Transport::Usb => "USB · its own agent",
                    Transport::Can => "CAN · the bus's agent",
                    Transport::Other => "none",
                };
                ui.label(RichText::new(transport).small());
                ui.add_sized([90.0, 20.0], TextEdit::singleline(&mut board.bus));
                let placed = builder.environment.is_some();
                ui.add_enabled_ui(placed, |ui| {
                    ui.add_sized([90.0, 20.0], TextEdit::singleline(&mut board.position).hint_text("x, y, z"))
                });
                if ui.add(egui::Button::new("×").frame(false)).on_hover_text("Take this board out").clicked() {
                    remove = Some(i);
                }
                ui.end_row();
            }
        });
        if let Some(i) = remove {
            builder.selected = i;
            builder.remove_selected();
        }
    }
    ui.add_space(8.0);

    let file = builder.toml(home);
    ui.horizontal(|ui| {
        if ui.button(format!("Add a board ({add_key})")).clicked() {
            action = Some(Action::AddBoard);
        }
        let ready = file.is_ok();
        if ui.add_enabled(ready, egui::Button::new(format!("Save ({save_key})"))).clicked() {
            action = Some(Action::Save);
        }
        if ui.add_enabled(ready, egui::Button::new(format!("Save and run ({run_key})"))).clicked() {
            action = Some(Action::SaveAndRun);
        }
        match (&file, &builder.replacing) {
            (Err(why), _) => _ = ui.label(RichText::new(why).color(visuals.warn_fg_color)),
            (Ok(_), Some(path)) => {
                ui.label(
                    RichText::new(format!("{} exists; save again to replace it", path.display()))
                        .color(visuals.warn_fg_color),
                );
            }
            (Ok(_), None) => {}
        }
    });
    ui.add_space(8.0);
    if let Ok(text) = &file {
        ui.label(RichText::new(format!("robots/{}.toml", builder.name.trim())).small().weak());
        egui::Frame::new().fill(visuals.extreme_bg_color).inner_margin(8.0).corner_radius(4.0).show(ui, |ui| {
            ScrollArea::both().id_salt("robot-preview").auto_shrink([false, true]).show(ui, |ui| {
                let font = egui::TextStyle::Monospace.resolve(ui.style());
                ui.add(egui::Label::new(crate::rainbow::highlight(text, &font, &visuals)).extend());
            });
        });
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkout() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("archon-builder-{}", std::process::id()));
        for sub in ["robots", "environments", "boards/talos", "boards/mk2"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        std::fs::write(dir.join("environments/pool.toml"), "").unwrap();
        dir
    }

    fn firmware(dir: &Path, stem: &str, symbol: &str) -> Firmware {
        let image = dir.join(format!("{stem}_with_bl.uf2"));
        std::fs::write(&image, b"UF2").unwrap();
        std::fs::write(dir.join(format!("{stem}.elf")), format!("\0{symbol}\0")).unwrap();
        Firmware { image, name: stem.to_owned(), built: None }
    }

    #[test]
    fn two_ivcs_go_in_the_pool_on_buses_of_their_own() {
        let dir = checkout();
        let simulator = Simulator { binary: dir.join("sim"), checkout: dir.clone() };
        let mut builder = Builder::new(&simulator);
        let ivc = firmware(&dir, "IVCBoard2", "transport_usb_open");
        let esc = firmware(&dir, "esc_board", "transport_can_open");
        let ivc_board = Some(dir.join("boards/talos/ivc_board.toml"));
        builder.add(ivc.clone(), ivc_board.clone());
        builder.add(ivc, ivc_board);
        builder.add(esc, Some(dir.join("boards/mk2/esc_board.toml")));
        assert_eq!(builder.environment.as_deref(), Some("pool"));
        let names: Vec<&str> = builder.boards.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["ivcboard2", "ivcboard2_2", "esc_board"]);
        let buses: Vec<&str> = builder.boards.iter().map(|b| b.bus.as_str()).collect();
        assert_eq!(buses, ["talos", "surface", "internal"]);
        assert_eq!(builder.name, "my_ivcboard2_test");

        let text = builder.toml(Path::new("/nowhere")).unwrap();
        assert!(text.contains("file = \"../environments/pool.toml\""), "{text}");
        assert!(text.contains("board = \"../boards/talos/ivc_board.toml\""), "{text}");
        assert!(text.contains("position_m = [7.0, 6.0, 1.0]"), "{text}");
        // It reads back as the robot it describes
        let path = builder.save(Path::new("/nowhere")).unwrap();
        let robot = crate::robot::load(&path, None).unwrap();
        let transports: Vec<Transport> = robot.boards.iter().map(|b| b.transport).collect();
        assert_eq!(transports, [Transport::Usb, Transport::Usb, Transport::Can]);
        assert_eq!(robot.problem(), None);

        // Saving over it asks first
        assert!(builder.save(Path::new("/nowhere")).is_err());
        assert!(builder.save(Path::new("/nowhere")).is_ok());
    }

    #[test]
    fn says_what_is_missing() {
        let dir = checkout();
        let simulator = Simulator { binary: dir.join("sim"), checkout: dir.clone() };
        let mut builder = Builder::new(&simulator);
        assert_eq!(builder.toml(&dir).unwrap_err(), "name the file with letters, digits, _ and -");
        builder.name = "x".into();
        assert_eq!(builder.toml(&dir).unwrap_err(), "add a board (a)");
        builder.add(firmware(&dir, "mystery", "nothing"), None);
        assert!(builder.toml(&dir).unwrap_err().contains("no board file"));
        assert_eq!(position("2, 6,1"), Some([2.0, 6.0, 1.0]));
        assert_eq!(position("2 6"), None);
    }
}
