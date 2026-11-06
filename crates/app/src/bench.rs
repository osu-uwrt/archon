//! Setting up a bench from inside archon instead of writing a profile first: the serial ports and CAN buses there are,
//! and the micro-ROS agent to run on one of them.

use std::path::{Path, PathBuf};

/// Something to run the agent on, or to open a console on
#[derive(Clone, Debug, PartialEq)]
pub enum Port {
    /// A serial device; `/dev/serial/by-id/...` when there is one, which survives the board re-enumerating
    Serial(PathBuf),
    Can(String),
    /// The agent's UDP transport, for boards on the network
    Udp(u16),
    /// A USB serial port of a board running in firmware_simulator: a terminal it holds, by the link it made
    /// (`/tmp/ivc-ros`) or by the terminal itself
    Simulated(PathBuf),
}

impl Port {
    /// What a person calls it: a serial port by the name its USB interface gives itself ("Micro-ROS", "Board CDC")
    /// and its device, else its file name
    pub fn label(&self) -> String {
        match self {
            Port::Serial(path) => {
                let file =
                    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
                // by-id names end in the interface number, like `-if02`, which tells two ports of one board apart
                let number = file.rsplit_once("-if").map(|(_, n)| format!(" (if{n})")).unwrap_or_default();
                match (usb_interface(path), usb_product(path)) {
                    (Some(interface), Some(product)) => format!("{interface}: {product}{number}"),
                    (Some(interface), None) => format!("{interface}{number}"),
                    _ => file,
                }
            }
            Port::Can(interface) => interface.clone(),
            Port::Udp(port) => format!("UDP port {port}"),
            Port::Simulated(path) => format!(
                "simulated: {}",
                path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())
            ),
        }
    }

    /// The path to open, for the ports that are files
    pub fn path(&self) -> Option<&Path> {
        match self {
            Port::Serial(path) | Port::Simulated(path) => Some(path),
            _ => None,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Port::Serial(_) => "serial",
            Port::Can(_) => "CAN",
            Port::Udp(_) => "network",
            Port::Simulated(_) => "firmware_simulator",
        }
    }
}

/// The tty a serial path leads to, like `ttyACM1`
fn tty_name(path: &Path) -> Option<String> {
    let real = std::fs::canonicalize(path).ok()?;
    Some(real.file_name()?.to_string_lossy().into_owned())
}

/// The name a USB serial port's interface gives itself, from sysfs: "Micro-ROS", "Board CDC", ...
pub fn usb_interface(path: &Path) -> Option<String> {
    let name = std::fs::read_to_string(format!("/sys/class/tty/{}/device/interface", tty_name(path)?)).ok()?;
    Some(name.trim().to_owned()).filter(|n| !n.is_empty())
}

/// The USB device's product name, like "Micro-ROS Agent"
fn usb_product(path: &Path) -> Option<String> {
    let name = std::fs::read_to_string(format!("/sys/class/tty/{}/device/../product", tty_name(path)?)).ok()?;
    Some(name.trim().to_owned()).filter(|n| !n.is_empty())
}

/// How likely a port is the one for the agent (a micro-ROS link) or for a console, by its interface's name
fn fit(interface: &str, for_agent: bool) -> u8 {
    let name = interface.to_ascii_lowercase();
    let micro_ros = ["micro-ros", "micro_ros", "microros", "uros", "xrce"].iter().any(|w| name.contains(w));
    let console = ["cdc", "console", "stdio", "debug", "log", "uart"].iter().any(|w| name.contains(w));
    match (for_agent, micro_ros, console) {
        (true, true, _) | (false, false, true) => 2,
        (true, false, false) | (false, false, false) => 1,
        _ => 0,
    }
}

/// Puts the likely ports first: micro-ROS links for the agent, consoles for a console. Returns how many lead as
/// likely. `interface` gives a serial port's interface name.
pub fn rank(ports: &mut [Port], for_agent: bool, interface: impl Fn(&Path) -> Option<String>) -> usize {
    let score = |port: &Port| match port {
        Port::Serial(path) => interface(path).map_or(1, |name| fit(&name, for_agent)),
        // The simulator's links are named for their use: `-ros` for micro-ROS, `-log` for the console
        Port::Simulated(path) => {
            let name = path.to_string_lossy().to_ascii_lowercase();
            match (for_agent, name.ends_with("ros"), name.ends_with("log")) {
                (true, true, _) | (false, _, true) => 2,
                _ => 1,
            }
        }
        // A CAN bus is how the agent reaches boards on the bus, so it comes after a direct link but before UDP
        Port::Can(_) => 1,
        Port::Udp(_) => 0,
    };
    ports.sort_by_key(|port| std::cmp::Reverse(score(port)));
    ports.iter().take_while(|port| score(port) == 2).count()
}

/// Serial devices, by their stable `by-id` names where there are any, else the `ttyACM`/`ttyUSB` nodes
pub fn serial_ports() -> Vec<Port> {
    let mut ports: Vec<PathBuf> =
        std::fs::read_dir("/dev/serial/by-id").into_iter().flatten().flatten().map(|e| e.path()).collect();
    if ports.is_empty() {
        ports = std::fs::read_dir("/dev")
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("ttyACM") || n.starts_with("ttyUSB"))
            })
            .collect();
    }
    ports.sort();
    ports.into_iter().map(Port::Serial).collect()
}

/// The terminals running firmware_simulator processes hold, by number (`/dev/pts/N`). A link in /tmp only counts as
/// a simulated port when its terminal is one of these: a link left from an earlier run may point at a terminal
/// someone's shell has since taken.
fn simulator_terminals() -> Vec<u32> {
    let mut terminals = Vec::new();
    for process in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let exe = std::fs::read_link(process.path().join("exe")).ok();
        if !exe.is_some_and(|e| e.file_name().is_some_and(|n| n == "firmware_simulator")) {
            continue;
        }
        for fd in std::fs::read_dir(process.path().join("fd")).into_iter().flatten().flatten() {
            if std::fs::read_link(fd.path()).is_ok_and(|t| t == Path::new("/dev/ptmx")) {
                let info =
                    std::fs::read_to_string(process.path().join("fdinfo").join(fd.file_name())).unwrap_or_default();
                let index =
                    info.lines().find_map(|l| l.strip_prefix("tty-index:")).and_then(|n| n.trim().parse::<u32>().ok());
                terminals.extend(index);
            }
        }
    }
    terminals.sort_unstable();
    terminals
}

/// The serial ports of boards running in firmware_simulator: its links in /tmp, and its terminals without one
pub fn simulated_ports() -> Vec<Port> {
    let live = simulator_terminals();
    let pts = |path: &Path| -> Option<u32> {
        std::fs::read_link(path).ok()?.strip_prefix("/dev/pts").ok()?.to_str()?.parse().ok()
    };
    let mut linked = Vec::new();
    let mut ports: Vec<PathBuf> = std::fs::read_dir("/tmp")
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| match pts(p) {
            Some(n) if live.contains(&n) => {
                linked.push(n);
                true
            }
            _ => false,
        })
        .collect();
    ports.extend(live.iter().filter(|n| !linked.contains(n)).map(|n| PathBuf::from(format!("/dev/pts/{n}"))));
    ports.sort();
    ports.into_iter().map(Port::Simulated).collect()
}

/// Network interfaces that are CAN buses (`can0`, `vcan0`, ...)
pub fn can_ports() -> Vec<Port> {
    // ARPHRD_CAN
    const CAN_TYPE: &str = "280";
    let mut names: Vec<String> = std::fs::read_dir("/sys/class/net")
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| std::fs::read_to_string(e.path().join("type")).is_ok_and(|t| t.trim() == CAN_TYPE))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names.into_iter().map(Port::Can).collect()
}

/// How to run the agent: a command to put the transport arguments after
#[derive(Clone, Debug, PartialEq)]
pub struct Agent {
    pub command: String,
    /// Where it was found, for saying so
    pub found: String,
}

/// The agent's binary inside a colcon workspace
const IN_WORKSPACE: &str = "install/lib/micro_ros_agent/micro_ros_agent";

/// Finds the micro-ROS agent: the configured command, then `micro_ros_agent` on the PATH, then a built workspace in
/// the usual places (`~/dev/*/micro-ros-agent`, `~/microros_ws`, ...)
pub fn find_agent(configured: Option<&str>, home: &Path) -> Result<Agent, String> {
    if let Some(command) = configured {
        return Ok(Agent { command: command.to_owned(), found: "the configured command".to_owned() });
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    if let Some(binary) = std::env::split_paths(&path).map(|d| d.join("micro_ros_agent")).find(|p| p.is_file()) {
        return Ok(Agent { command: quote(&binary), found: binary.display().to_string() });
    }
    workspaces(home)
        .into_iter()
        .find(|ws| ws.join(IN_WORKSPACE).is_file())
        .map(|ws| Agent {
            // The workspace's setup puts its libraries on the path
            command: format!(
                "source {} && exec {}",
                quote(&ws.join("install/setup.bash")),
                quote(&ws.join(IN_WORKSPACE))
            ),
            found: ws.display().to_string(),
        })
        .ok_or_else(|| {
            "no micro-ROS agent found: put micro_ros_agent on the PATH, build one in ~/microros_ws, or set \
             micro_ros_agent = \"...\" in ~/.config/archon/config.toml"
                .to_owned()
        })
}

/// Where a micro-ROS agent workspace usually is
fn workspaces(home: &Path) -> Vec<PathBuf> {
    let names = ["micro-ros-agent", "micro_ros_agent", "microros_ws", "uros_ws", "micro_ros_ws"];
    let mut roots = vec![home.to_owned(), home.join("dev"), home.join("src"), home.join("ws")];
    // One level of grouping under ~/dev, like ~/dev/uwrt/micro-ros-agent
    roots.extend(
        std::fs::read_dir(home.join("dev")).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()),
    );
    roots.iter().flat_map(|root| names.iter().map(move |n| root.join(n))).collect()
}

/// The agent's command for a transport
pub fn agent_command(agent: &Agent, port: &Port) -> String {
    match port {
        Port::Serial(path) | Port::Simulated(path) => format!("{} serial --dev {}", agent.command, quote(path)),
        Port::Can(interface) => format!("{} can -D {interface}", agent.command),
        Port::Udp(port) => format!("{} udp4 --port {port}", agent.command),
    }
}

pub fn quote(path: &Path) -> String {
    let text = path.display().to_string();
    if text.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+:".contains(c)) {
        text
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

/// The user's archon settings, `~/.config/archon/config.toml`
#[derive(serde::Deserialize, Default, Debug)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The command that runs the micro-ROS agent, before its transport arguments
    pub micro_ros_agent: Option<String>,
    /// The firmware_simulator binary, inside its checkout (whose boards/ it uses)
    pub firmware_simulator: Option<PathBuf>,
    /// titan_firmware's upload_tool, for flashing boards
    pub upload_tool: Option<PathBuf>,
}

/// firmware_simulator's binary and its checkout, whose `boards/` describes each board
pub struct Simulator {
    pub binary: PathBuf,
    pub checkout: PathBuf,
}

/// Finds firmware_simulator: the configured binary, else a release build in the usual places (`~/dev/*/`, `~/dev/`)
pub fn find_simulator(configured: Option<&Path>, home: &Path) -> Result<Simulator, String> {
    let from_binary = |binary: PathBuf| {
        // target/release/firmware_simulator, three up from the binary
        let checkout = binary.ancestors().nth(3).map(Path::to_path_buf)?;
        checkout.join("boards").is_dir().then_some(Simulator { binary, checkout })
    };
    if let Some(binary) = configured {
        return from_binary(binary.to_owned())
            .ok_or_else(|| format!("{} isn't in a firmware_simulator checkout's target/release", binary.display()));
    }
    let mut roots = vec![home.join("dev"), home.to_owned()];
    roots.extend(
        std::fs::read_dir(home.join("dev")).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()),
    );
    roots
        .iter()
        .map(|root| root.join("firmware_simulator/target/release/firmware_simulator"))
        .find(|binary| binary.is_file())
        .and_then(from_binary)
        .ok_or_else(|| {
            "firmware_simulator isn't built: run `cargo build --release` in its checkout, or set firmware_simulator =              \"/path/to/target/release/firmware_simulator\" in ~/.config/archon/config.toml"
                .to_owned()
        })
}

/// A firmware build to run in the simulator
#[derive(Clone, Debug, PartialEq)]
pub struct Firmware {
    pub image: PathBuf,
    /// Like `IVCBoard2`
    pub name: String,
    pub built: Option<std::time::SystemTime>,
}

impl Firmware {
    /// A build named by its image, as `--simulate` gives it
    pub fn from_image(image: PathBuf) -> Option<Self> {
        let name = image.file_name()?.to_str()?;
        let name = name.strip_suffix("_with_bl.uf2").or_else(|| name.strip_suffix(".uf2"))?.to_owned();
        let built = std::fs::metadata(&image).and_then(|m| m.modified()).ok();
        Some(Self { image, name, built })
    }
}

/// The ports isolated windows run their discovery servers on (11811 is Fast DDS's own default)
const DISCOVERY_PORTS: std::ops::Range<u16> = 11811..11911;

/// A port for a new discovery server: the first one nothing is listening on
pub fn free_discovery_port() -> Option<u16> {
    DISCOVERY_PORTS.clone().find(|port| std::net::UdpSocket::bind(("127.0.0.1", *port)).is_ok())
}

/// Runs the Fast DDS discovery server that makes an isolated window's ROS network
pub fn discovery_command(port: u16) -> String {
    format!("exec fast-discovery-server -i 0 -l 127.0.0.1 -p {port}")
}

/// What a process needs to join the network of the discovery server on `port`. Super client, because a plain
/// discovery client only hears about the topics it uses itself, and archon (and `ros2 topic list`) want them all.
pub fn isolated_env(port: u16) -> [(&'static str, String); 2] {
    [("ROS_DISCOVERY_SERVER", format!("127.0.0.1:{port}")), ("ROS_SUPER_CLIENT", "TRUE".to_owned())]
}

/// Whether a program is on the PATH
pub fn on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}

/// titan_firmware builds with their bootloader (`*_with_bl.uf2`), as the simulator boots them, newest first
pub fn firmware_builds(home: &Path) -> Vec<Firmware> {
    let mut roots = vec![home.join("dev/titan_firmware"), home.join("titan_firmware")];
    roots.extend(
        std::fs::read_dir(home.join("dev")).into_iter().flatten().flatten().map(|e| e.path().join("titan_firmware")),
    );
    let mut found = Vec::new();
    for root in roots.iter().filter(|r| r.is_dir()) {
        walk_for_images(root, 5, &mut found);
    }
    found.sort_by_key(|f| std::cmp::Reverse(f.built));
    found.dedup_by(|a, b| a.image == b.image);
    found
}

fn walk_for_images(dir: &Path, depth: usize, found: &mut Vec<Firmware>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(stem) = name.strip_suffix("_with_bl.uf2") {
            let built = entry.metadata().and_then(|m| m.modified()).ok();
            found.push(Firmware { image: path, name: stem.to_owned(), built });
        } else if depth > 0 && path.is_dir() && !name.starts_with('.') && name != "lib" && name != "CMakeFiles" {
            walk_for_images(&path, depth - 1, found);
        }
    }
}

/// The simulator's board file for an image: the board id the firmware embeds (`talos/ivc_board`), matched against
/// `boards/<dir>/<board>.toml`; the longest id wins, so `mk2/esc_board` beats `esc_board`
pub fn board_for(simulator: &Simulator, image: &Path) -> Option<PathBuf> {
    let bytes = std::fs::read(image).ok()?;
    let mut best: Option<(usize, PathBuf)> = None;
    for dir in std::fs::read_dir(simulator.checkout.join("boards")).ok()?.flatten().filter(|d| d.path().is_dir()) {
        for file in std::fs::read_dir(dir.path()).into_iter().flatten().flatten() {
            let path = file.path();
            let (Some(group), Some(stem)) =
                (dir.file_name().to_str().map(str::to_owned), path.file_stem().and_then(|s| s.to_str()))
            else {
                continue;
            };
            if path.extension().is_none_or(|e| e != "toml") {
                continue;
            }
            let id = format!("{group}/{stem}");
            let found = bytes.windows(id.len()).any(|w| w == id.as_bytes());
            if found && best.as_ref().is_none_or(|(len, _)| id.len() > *len) {
                best = Some((id.len(), path));
            }
        }
    }
    best.map(|(_, path)| path)
}

/// The commands for a simulated board: the simulator with both USB ports on terminals linked from /tmp, and the
/// agent once the micro-ROS one appears. Returns (simulator, agent, console link).
pub fn simulation_commands(
    simulator: &Simulator,
    firmware: &Firmware,
    board: &Path,
    agent: &Agent,
    slug: &str,
) -> (String, String, PathBuf) {
    let log = PathBuf::from(format!("/tmp/archon-{slug}-log"));
    let ros = PathBuf::from(format!("/tmp/archon-{slug}-ros"));
    // A link left from an earlier run could lead to a terminal someone else has now, so they go before the run makes
    // new ones; the agent waits for the new one
    let simulator_command = format!(
        "rm -f {log} {ros}; exec {} run {} --board {} --usb-cdc pty:{log} --usb-cdc2 pty:{ros} --realtime --time 86400s",
        quote(&simulator.binary),
        quote(&firmware.image),
        quote(board),
        log = quote(&log),
        ros = quote(&ros),
    );
    let agent_command = format!(
        "while [ ! -e {ros} ]; do sleep 0.2; done; {}",
        agent_command(agent, &Port::Simulated(ros.clone())),
        ros = quote(&ros),
    );
    (simulator_command, agent_command, log)
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_workspace_build() {
        let home = std::env::temp_dir().join(format!("archon-home-{}", std::process::id()));
        let ws = home.join("dev/uwrt/micro-ros-agent");
        std::fs::create_dir_all(ws.join("install/lib/micro_ros_agent")).unwrap();
        std::fs::write(ws.join(IN_WORKSPACE), "").unwrap();
        let agent = find_agent(None, &home).unwrap();
        assert!(agent.command.starts_with("source "), "{}", agent.command);
        assert!(agent.command.ends_with("install/lib/micro_ros_agent/micro_ros_agent"));
        let command =
            agent_command(&agent, &Port::Serial("/dev/serial/by-id/usb-OSU_UWRT_Micro-ROS_Agent_X-if02".into()));
        assert!(command.ends_with(" serial --dev /dev/serial/by-id/usb-OSU_UWRT_Micro-ROS_Agent_X-if02"));
        assert!(agent_command(&agent, &Port::Can("can0".into())).ends_with(" can -D can0"));
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn the_configured_command_wins() {
        let agent = find_agent(Some("ros2 run micro_ros_agent micro_ros_agent"), Path::new("/nonexistent")).unwrap();
        assert_eq!(
            agent_command(&agent, &Port::Udp(8888)),
            "ros2 run micro_ros_agent micro_ros_agent udp4 --port 8888"
        );
    }

    #[test]
    fn says_how_to_fix_a_missing_agent() {
        let err = find_agent(None, Path::new("/nonexistent")).unwrap_err();
        assert!(err.contains("config.toml"));
    }

    #[test]
    fn ranks_the_micro_ros_port_first_for_the_agent() {
        let board = |n: &str| Port::Serial(format!("/dev/serial/by-id/usb-OSU_UWRT_Micro-ROS_Agent_X-if{n}").into());
        let interface = |path: &Path| {
            Some(if path.to_string_lossy().ends_with("if00") { "Board CDC" } else { "Micro-ROS" }.to_owned())
        };
        let mut ports = vec![board("00"), board("02"), Port::Can("vcan0".into()), Port::Udp(8888)];
        assert_eq!(rank(&mut ports, true, interface), 1);
        assert_eq!(ports[0], board("02"), "the micro-ROS link first");
        assert_eq!(ports[3], Port::Udp(8888));
        let mut ports = vec![board("02"), board("00")];
        assert_eq!(rank(&mut ports, false, interface), 1);
        assert_eq!(ports[0], board("00"), "the console first");
    }

    #[test]
    fn matches_a_board_by_the_id_in_the_image() {
        let root = std::env::temp_dir().join(format!("archon-sim-{}", std::process::id()));
        for board in ["talos/ivc_board", "talos/esc_board", "mk2/esc_board"] {
            let (dir, stem) = board.split_once('/').unwrap();
            std::fs::create_dir_all(root.join("boards").join(dir)).unwrap();
            std::fs::write(root.join("boards").join(dir).join(format!("{stem}.toml")), "").unwrap();
        }
        let simulator = Simulator { binary: root.join("target/release/firmware_simulator"), checkout: root.clone() };
        let image = root.join("IVCBoard2_with_bl.uf2");
        std::fs::write(&image, b"\x00titan_bootloader 1.0-talos/talos/ivc_board blake/ivc\x00").unwrap();
        assert_eq!(board_for(&simulator, &image), Some(root.join("boards/talos/ivc_board.toml")));
        std::fs::write(&image, b"nothing").unwrap();
        assert_eq!(board_for(&simulator, &image), None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn names_a_build_by_its_image() {
        let firmware = Firmware::from_image("/fw/talos/IVCBoard2_with_bl.uf2".into()).unwrap();
        assert_eq!(firmware.name, "IVCBoard2");
        assert!(Firmware::from_image("/fw/notes.txt".into()).is_none());
    }

    #[test]
    fn picks_a_discovery_port_nobody_has() {
        let taken = std::net::UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let port = free_discovery_port().unwrap();
        assert!(DISCOVERY_PORTS.contains(&port));
        assert_ne!(Some(port), taken.local_addr().ok().map(|a| a.port()));
        let _held = std::net::UdpSocket::bind(("127.0.0.1", port)).unwrap();
        assert_ne!(free_discovery_port(), Some(port), "a port in use is skipped");
    }

    #[test]
    fn simulation_waits_for_the_micro_ros_terminal() {
        let simulator = Simulator { binary: "/sim/target/release/firmware_simulator".into(), checkout: "/sim".into() };
        let firmware = Firmware { image: "/fw/IVCBoard2_with_bl.uf2".into(), name: "IVCBoard2".into(), built: None };
        let agent = Agent { command: "agent".into(), found: String::new() };
        let (sim, agent, log) = simulation_commands(
            &simulator,
            &firmware,
            Path::new("/sim/boards/talos/ivc_board.toml"),
            &agent,
            "ivcboard2",
        );
        assert!(sim.starts_with(
            "rm -f /tmp/archon-ivcboard2-log /tmp/archon-ivcboard2-ros; exec /sim/target/release/firmware_simulator run"
        ));
        assert!(sim.contains("--usb-cdc2 pty:/tmp/archon-ivcboard2-ros --realtime"));
        assert_eq!(
            agent,
            "while [ ! -e /tmp/archon-ivcboard2-ros ]; do sleep 0.2; done; agent serial --dev /tmp/archon-ivcboard2-ros"
        );
        assert_eq!(log, Path::new("/tmp/archon-ivcboard2-log"));
    }

    /// With firmware_simulator running a board with USB terminals, they show up as simulated ports
    #[test]
    fn finds_a_running_simulators_terminals() {
        let ports = simulated_ports();
        for port in &ports {
            assert!(matches!(port, Port::Simulated(_)));
        }
    }

    #[test]
    fn quotes_paths_with_spaces() {
        assert_eq!(quote(Path::new("/dev/ttyACM0")), "/dev/ttyACM0");
        assert_eq!(quote(Path::new("/tmp/my ws")), "'/tmp/my ws'");
    }
}
