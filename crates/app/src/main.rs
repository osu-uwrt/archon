//! archon: a keyboard-driven native GUI for ROS 2 firmware testing.

mod app;
mod bags;
mod bench;
mod builder;
mod bytes;
mod flash;
mod health;
mod inspector;
mod keymap;
mod layout;
mod logview;
mod nodes;
mod persist;
mod picker;
mod plot;
mod publish;
mod rainbow;
mod robot;
mod sidebar;
mod topics;
mod welcome;

use std::path::PathBuf;

use archon_core::profile::Profile;

const USAGE: &str = "\
usage: archon [--no-ros] [--isolated] [--simulate IMAGE|ROBOT] [--agent NAME=PORT] [--console NAME=PORT] [PROFILE]

Runs the test bench in PROFILE, or in the nearest archon.toml in this directory or above.
--no-ros          leaves out the ROS graph (topics and echoes).
--isolated        runs on a ROS network of this window's own (a Fast DDS discovery server it starts), so two boards
                  with the same topics, each in its own window, don't mix. Without PROFILE it starts empty.
--simulate IMAGE  simulates a firmware build (a *_with_bl.uf2), or a firmware_simulator robot file (a .toml), on
                  start, as Space n f does.
--agent NAME=PORT, --console NAME=PORT, --udp-agent NAME=UDP_PORT
                  run a micro-ROS agent on a serial port (waiting for it to appear), open a console on one, or run
                  an agent on a UDP port; repeatable. A robot's isolated windows are opened with these.
Keybindings can be changed in ~/.config/archon/keymap.toml.";

fn main() -> eframe::Result {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return Ok(());
    }
    let with_ros = !args.iter().any(|a| a == "--no-ros");
    let isolated = args.iter().any(|a| a == "--isolated");
    args.retain(|a| a != "--no-ros" && a != "--isolated");
    // The flags that take a value
    let (mut simulate, mut attach, mut rest) = (None, Vec::new(), Vec::new());
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        if !matches!(arg.as_str(), "--simulate" | "--agent" | "--console" | "--udp-agent") {
            rest.push(arg);
            continue;
        }
        let Some(value) = iter.next() else {
            eprintln!("{arg} needs a value\n\n{USAGE}");
            std::process::exit(2);
        };
        if arg == "--simulate" {
            simulate = Some(PathBuf::from(value));
            continue;
        }
        match robot::Attach::parse(&arg, &value) {
            Ok(one) => attach.push(one),
            Err(why) => {
                eprintln!("{why}\n\n{USAGE}");
                std::process::exit(2);
            }
        }
    }
    let args = rest;

    let mut problems = Vec::new();
    let path = match args.first() {
        Some(path) => Some(PathBuf::from(path)),
        // An isolated window is for a second board, so it doesn't bring up this folder's bench again
        None if isolated => None,
        None => std::env::current_dir().ok().and_then(|dir| Profile::find(&dir)),
    };
    let profile = match &path {
        Some(path) => Profile::load(path).unwrap_or_else(|e| {
            problems.push(format!("{e:#}"));
            Profile { path: Some(path.clone()), ..Profile::default() }
        }),
        None => Profile::default(),
    };

    // archon's own node joins the same graph as the profile's processes
    for (key, value) in &profile.env {
        if key.starts_with("RMW_") || key.starts_with("ROS_") {
            // SAFETY: no other threads exist yet
            unsafe { std::env::set_var(key, value) };
        }
    }

    // An isolated window, and everything it starts, joins the network of a discovery server of its own
    // A bench saved from an isolated window opens isolated again
    let isolated = isolated || profile.isolated;
    let discovery = if isolated {
        match bench::free_discovery_port() {
            Some(port) => {
                for (key, value) in bench::isolated_env(port) {
                    // SAFETY: no other threads exist yet
                    unsafe { std::env::set_var(key, value) };
                }
                if !bench::on_path("fast-discovery-server") {
                    problems.push(
                        "fast-discovery-server isn't on the PATH (it comes with Fast DDS, in the ROS shell), so this \
                         window's ROS network has no server"
                            .to_owned(),
                    );
                }
                Some(port)
            }
            None => {
                problems.push(
                    "no free port for a discovery server (tried 11811 to 11910), so this window isn't isolated"
                        .to_owned(),
                );
                None
            }
        }
    } else {
        None
    };
    // Joining someone's discovery server, see the whole network, not only the topics archon itself uses
    if std::env::var_os("ROS_DISCOVERY_SERVER").is_some() && std::env::var_os("ROS_SUPER_CLIENT").is_none() {
        // SAFETY: no other threads exist yet
        unsafe { std::env::set_var("ROS_SUPER_CLIENT", "TRUE") };
    }

    let config_path = dirs::config_dir().map(|dir| dir.join("archon").join("config.toml"));
    let config = match config_path.as_deref().map(bench::Config::load) {
        Some(Ok(config)) => config,
        Some(Err(why)) => {
            problems.push(why);
            bench::Config::default()
        }
        None => bench::Config::default(),
    };

    let mut keymap = keymap::Keymap::defaults();
    let keymap_path = dirs::config_dir().map(|dir| dir.join("archon").join("keymap.toml"));
    if let Some(keymap_path) = keymap_path.filter(|p| p.is_file())
        && let Err(e) = keymap.apply_file(&keymap_path)
    {
        problems.push(format!("{e:#}"));
    }

    let mut title = match profile.path.as_ref().and_then(|p| p.canonicalize().ok()) {
        Some(path) => {
            let dir = path.parent().and_then(|d| d.file_name()).map(|n| n.to_string_lossy().into_owned());
            format!("archon — {}", dir.unwrap_or_default())
        }
        None => "archon".to_owned(),
    };
    if let Some(port) = discovery {
        title.push_str(&format!(" (isolated :{port})"));
    }
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
        Box::new(move |cc| {
            // An empty isolated window has nothing of its own to restore
            let state_file = match (&profile.path, discovery) {
                (None, Some(_)) => None,
                (path, _) => persist::state_file(path.as_deref()),
            };
            let startup =
                app::Startup { profile, keymap, problems, with_ros, state_file, config, discovery, simulate, attach };
            Ok(Box::new(app::ArchonApp::new(cc, startup)))
        }),
    )
}
