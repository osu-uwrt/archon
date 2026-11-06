//! archon: a keyboard-driven native GUI for ROS 2 firmware testing.

mod app;
mod keymap;
mod picker;
mod view;

use std::path::PathBuf;

use archon_core::profile::Profile;

const USAGE: &str = "\
usage: archon [PROFILE]

Runs the test bench in PROFILE, or in the nearest archon.toml in this directory or above.
Keybindings can be changed in ~/.config/archon/keymap.toml.";

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return Ok(());
    }

    let mut problems = Vec::new();
    let path = match args.first() {
        Some(path) => Some(PathBuf::from(path)),
        None => std::env::current_dir().ok().and_then(|dir| Profile::find(&dir)),
    };
    let profile = match &path {
        Some(path) => Profile::load(path).unwrap_or_else(|e| {
            problems.push(format!("{e:#}"));
            Profile { path: Some(path.clone()), ..Profile::default() }
        }),
        None => Profile::default(),
    };

    let mut keymap = keymap::Keymap::defaults();
    let keymap_path = dirs::config_dir().map(|dir| dir.join("archon").join("keymap.toml"));
    if let Some(keymap_path) = keymap_path.filter(|p| p.is_file())
        && let Err(e) = keymap.apply_file(&keymap_path)
    {
        problems.push(format!("{e:#}"));
    }

    let title = match profile.path.as_ref().and_then(|p| p.canonicalize().ok()) {
        Some(path) => {
            let dir = path.parent().and_then(|d| d.file_name()).map(|n| n.to_string_lossy().into_owned());
            format!("archon — {}", dir.unwrap_or_default())
        }
        None => "archon".to_owned(),
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(title)
            .with_app_id("archon")
            .with_inner_size([1280.0, 800.0]),
        ..Default::default()
    };
    eframe::run_native(
        "archon",
        options,
        Box::new(move |cc| Ok(Box::new(app::ArchonApp::new(cc, profile, keymap, problems)))),
    )
}
