//! The paths archon uses (the titan_firmware checkout, firmware_simulator, upload_tool, the agents), what each one
//! is now and where that came from, and a field to set it. Settings go in `~/.config/archon/config.toml`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use egui::{RichText, TextEdit, Ui};

use crate::bench::{self, Config};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Setting {
    TitanFirmware,
    Simulator,
    UploadTool,
    MicroRosAgent,
    NanoRosAgent,
}

pub const ALL: [Setting; 5] =
    [Setting::TitanFirmware, Setting::Simulator, Setting::UploadTool, Setting::MicroRosAgent, Setting::NanoRosAgent];

impl Setting {
    /// Its key in config.toml
    pub fn key(self) -> &'static str {
        match self {
            Setting::TitanFirmware => "titan_firmware",
            Setting::Simulator => "firmware_simulator",
            Setting::UploadTool => "upload_tool",
            Setting::MicroRosAgent => "micro_ros_agent",
            Setting::NanoRosAgent => "nano_ros_agent",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Setting::TitanFirmware => "titan_firmware checkout",
            Setting::Simulator => "firmware_simulator",
            Setting::UploadTool => "upload_tool",
            Setting::MicroRosAgent => "micro-ROS agent",
            Setting::NanoRosAgent => "nano-ROS agent",
        }
    }

    fn help(self) -> &'static str {
        match self {
            Setting::TitanFirmware => {
                "A folder. Its builds come first in Space n f, Space n u and the robot builder; it's TITAN_FIRMWARE_DIR \
                 in robot files, and where the Boards view reads fault names. The TITAN_FIRMWARE_DIR environment \
                 variable, when it's set, wins."
            }
            Setting::Simulator => {
                "The release binary in a firmware_simulator checkout (target/release/firmware_simulator)"
            }
            Setting::UploadTool => "titan_firmware's tools/upload_tool/build/upload_tool, for flashing",
            Setting::MicroRosAgent => {
                "A command, before its transport arguments, like `ros2 run micro_ros_agent micro_ros_agent`"
            }
            Setting::NanoRosAgent => "A command, before its serial port, like the nano_ros_agent binary",
        }
    }

    fn is_command(self) -> bool {
        matches!(self, Setting::MicroRosAgent | Setting::NanoRosAgent)
    }

    /// What's in config.toml for it
    pub fn configured(self, config: &Config) -> Option<String> {
        let path = |p: &Option<PathBuf>| p.as_ref().map(|p| p.display().to_string());
        match self {
            Setting::TitanFirmware => path(&config.titan_firmware),
            Setting::Simulator => path(&config.firmware_simulator),
            Setting::UploadTool => path(&config.upload_tool),
            Setting::MicroRosAgent => config.micro_ros_agent.clone(),
            Setting::NanoRosAgent => config.nano_ros_agent.clone(),
        }
    }

    /// What archon uses now, and where it came from; or why there's nothing
    pub fn current(self, config: &Config, home: &Path) -> Result<(String, &'static str), String> {
        let source = |configured: bool| if configured { "set" } else { "found" };
        match self {
            Setting::TitanFirmware => config
                .titan_firmware(home)
                .map(|(dir, from)| (dir.display().to_string(), from))
                .ok_or_else(|| "none found in ~/dev/*/titan_firmware".to_owned()),
            Setting::Simulator => bench::find_simulator(config.firmware_simulator.as_deref(), home)
                .map(|s| (s.binary.display().to_string(), source(config.firmware_simulator.is_some()))),
            Setting::UploadTool => crate::flash::find(config.upload_tool.as_deref(), config.titan(home).as_deref())
                .map(|tool| {
                    let (crate::flash::Tool::UploadTool(path) | crate::flash::Tool::Picotool(path)) = &tool;
                    (path.display().to_string(), source(config.upload_tool.is_some()))
                }),
            Setting::MicroRosAgent => bench::find_agent(config.micro_ros_agent.as_deref(), home)
                .map(|a| (a.command, source(config.micro_ros_agent.is_some()))),
            Setting::NanoRosAgent => bench::find_nano_agent(config.nano_ros_agent.as_deref(), home)
                .map(|a| (a.command, source(config.nano_ros_agent.is_some()))),
        }
    }

    /// A value as typed, checked, with `~` filled in
    pub fn check(self, value: &str, home: &Path) -> Result<String, String> {
        let value = value.trim();
        if value.is_empty() {
            return Err("it's empty (Find it again goes back to finding it)".to_owned());
        }
        if self.is_command() {
            return Ok(value.to_owned());
        }
        let path = match value.strip_prefix("~/") {
            Some(rest) => home.join(rest),
            None => PathBuf::from(value),
        };
        match self {
            Setting::TitanFirmware if !path.is_dir() => Err(format!("{} isn't a folder", path.display())),
            Setting::Simulator => bench::find_simulator(Some(&path), home).map(|_| ()),
            Setting::UploadTool if !path.is_file() => Err(format!("{} isn't there", path.display())),
            _ => Ok(()),
        }?;
        Ok(path.display().to_string())
    }

    /// Puts a value (or none) into the settings in use
    pub fn apply(self, config: &mut Config, value: Option<String>) {
        match self {
            Setting::TitanFirmware => config.titan_firmware = value.map(PathBuf::from),
            Setting::Simulator => config.firmware_simulator = value.map(PathBuf::from),
            Setting::UploadTool => config.upload_tool = value.map(PathBuf::from),
            Setting::MicroRosAgent => config.micro_ros_agent = value,
            Setting::NanoRosAgent => config.nano_ros_agent = value,
        }
    }
}

#[derive(Default)]
pub struct SettingsState {
    drafts: HashMap<Setting, String>,
    /// The field to put the cursor in, once
    pub focus: Option<Setting>,
    /// What the last save of each said
    pub said: HashMap<Setting, Result<String, String>>,
}

pub enum Action {
    Save(Setting, String),
    Clear(Setting),
}

pub fn show(ui: &mut Ui, state: &mut SettingsState, config: &Config, home: &Path) -> Option<Action> {
    let visuals = ui.visuals().clone();
    let mut action = None;
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Paths").strong());
        let file = Config::file().map(|f| f.display().to_string()).unwrap_or_default();
        ui.label(RichText::new(format!("what archon uses, saved in {file}")).weak());
    });
    ui.separator();
    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        for setting in ALL {
            ui.add_space(6.0);
            ui.label(RichText::new(setting.title()).strong());
            ui.label(RichText::new(setting.help()).small().weak());
            match setting.current(config, home) {
                Ok((value, from)) => {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(format!("now ({from})")).small().weak());
                        ui.label(RichText::new(value).monospace());
                    });
                }
                Err(why) => _ = ui.label(RichText::new(why).small().color(visuals.warn_fg_color)),
            }
            let draft = state.drafts.entry(setting).or_insert_with(|| setting.configured(config).unwrap_or_default());
            ui.horizontal(|ui| {
                let hint = if setting.is_command() { "a command" } else { "a path (~/ works)" };
                let field =
                    ui.add(TextEdit::singleline(draft).hint_text(hint).desired_width(ui.available_width() - 190.0));
                if state.focus == Some(setting) {
                    field.request_focus();
                    state.focus = None;
                }
                let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.button("Save").clicked() || entered {
                    action = Some(Action::Save(setting, draft.clone()));
                }
                if setting.configured(config).is_some()
                    && ui.button("Find it again").on_hover_text("Take the setting out").clicked()
                {
                    draft.clear();
                    action = Some(Action::Clear(setting));
                }
            });
            match state.said.get(&setting) {
                Some(Ok(note)) => _ = ui.label(RichText::new(note).small().color(visuals.weak_text_color())),
                Some(Err(why)) => _ = ui.label(RichText::new(why).small().color(visuals.error_fg_color)),
                None => {}
            }
            ui.add_space(4.0);
            ui.separator();
        }
    });
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_paths_and_saves_into_the_file() {
        let dir = std::env::temp_dir().join(format!("archon-settings-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("titan")).unwrap();
        let home = dir.clone();
        assert_eq!(Setting::TitanFirmware.check("~/titan", &home).unwrap(), dir.join("titan").display().to_string());
        assert!(Setting::TitanFirmware.check("~/nope", &home).unwrap_err().contains("isn't a folder"));
        assert!(Setting::Simulator.check("/no/firmware_simulator", &home).is_err());
        assert_eq!(
            Setting::MicroRosAgent.check(" ros2 run micro_ros_agent micro_ros_agent ", &home).unwrap(),
            "ros2 run micro_ros_agent micro_ros_agent"
        );
        assert!(Setting::NanoRosAgent.check("  ", &home).is_err());

        // Saved into a file that has a comment, which stays; then taken out again
        let file = dir.join("config.toml");
        std::fs::write(&file, "# mine\nmicro_ros_agent = \"agent\"\n").unwrap();
        Config::save(&file, "titan_firmware", Some(&dir.join("titan").display().to_string())).unwrap();
        let config = Config::load(&file).unwrap();
        assert_eq!(config.titan_firmware, Some(dir.join("titan")));
        assert_eq!(config.micro_ros_agent.as_deref(), Some("agent"));
        assert!(std::fs::read_to_string(&file).unwrap().starts_with("# mine"));
        Config::save(&file, "titan_firmware", None).unwrap();
        assert_eq!(Config::load(&file).unwrap().titan_firmware, None);

        let mut config = Config::default();
        Setting::TitanFirmware.apply(&mut config, Some(dir.join("titan").display().to_string()));
        if std::env::var_os("TITAN_FIRMWARE_DIR").is_none() {
            assert_eq!(config.titan_firmware(&home), Some((dir.join("titan"), "set")));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
