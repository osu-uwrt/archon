//! firmware_simulator robot files: several boards in one run, on shared CAN buses and in one body of water. archon
//! runs one, puts a console on every C board and an agent wherever micro-ROS comes out, and sends a second copy of a
//! USB board (whose topics would mix with the first's) to an isolated window.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use archon_core::profile::{ProcessSpec, RestartPolicy, SerialSpec};

use crate::bench::{Agent, AgentKind, Port, Simulator, agent_command, quote};

/// How a board's firmware reaches its micro-ROS agent
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    /// The second USB serial port (titan_firmware's IVCBoard2)
    Usb,
    /// The CAN bus, which the simulator hands to a UDP agent (`--micro-ros-agent`)
    Can,
    /// Not micro-ROS: a Rust board, nano-ROS, or a test program
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RobotBoard {
    pub name: String,
    /// The board file, which says what hardware it is
    pub board: PathBuf,
    /// The firmware image, or why it can't be found
    pub image: Result<PathBuf, String>,
    pub bus: Option<String>,
    pub transport: Transport,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Robot {
    pub file: PathBuf,
    /// The file's name without `.toml`
    pub name: String,
    pub boards: Vec<RobotBoard>,
    /// Variables the file uses that the simulator has to be given, like `TITAN_FIRMWARE_DIR`
    pub env: Vec<(String, String)>,
    pub modified: Option<std::time::SystemTime>,
}

impl Robot {
    /// Why it can't run: an image that isn't there
    pub fn problem(&self) -> Option<String> {
        self.boards.iter().find_map(|b| b.image.as_ref().err().map(|why| format!("{}: {why}", b.name)))
    }

    /// Like "talos_ivc, surface_ivc"
    pub fn board_names(&self) -> String {
        self.boards.iter().map(|b| b.name.as_str()).collect::<Vec<_>>().join(", ")
    }
}

/// The titan_firmware checkout robot files mean by `TITAN_FIRMWARE_DIR`, when it isn't set
pub fn titan_firmware_dir(home: &Path) -> Option<PathBuf> {
    let mut candidates = vec![home.join("dev/titan_firmware"), home.join("titan_firmware")];
    candidates.extend(
        std::fs::read_dir(home.join("dev")).into_iter().flatten().flatten().map(|e| e.path().join("titan_firmware")),
    );
    candidates.into_iter().find(|dir| dir.join("talos").is_dir())
}

/// The robot files in the simulator's `robots/`, most recently changed first (so your own come before the shipped
/// ones you haven't touched)
pub fn robots(simulator: &Simulator, titan: Option<&Path>) -> Vec<Robot> {
    let mut found: Vec<Robot> = std::fs::read_dir(simulator.checkout.join("robots"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "toml"))
        .filter_map(|p| load(&p, titan).ok())
        .collect();
    found.sort_by_key(|r| std::cmp::Reverse(r.modified));
    found
}

#[derive(serde::Deserialize)]
struct File {
    #[serde(default)]
    board: Vec<BoardEntry>,
}

#[derive(serde::Deserialize)]
struct BoardEntry {
    name: String,
    board: String,
    elf: String,
    bus: Option<String>,
}

pub fn load(file: &Path, titan: Option<&Path>) -> Result<Robot, String> {
    let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let parsed: File = toml::from_str(&text).map_err(|e| format!("{}: {e}", file.display()))?;
    let dir = file.parent().unwrap_or(Path::new("."));
    let mut env = BTreeMap::new();
    let boards = parsed
        .board
        .into_iter()
        .map(|entry| {
            let image = expand(&entry.elf, titan, &mut env).map(|path| dir.join(path)).and_then(|path| {
                if path.is_file() { Ok(path) } else { Err(format!("{} isn't built", path.display())) }
            });
            let transport = image.as_deref().map_or(Transport::Other, transport);
            RobotBoard { name: entry.name, board: dir.join(entry.board), image, bus: entry.bus, transport }
        })
        .collect();
    Ok(Robot {
        file: file.to_owned(),
        name: file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
        boards,
        env: env.into_iter().collect(),
        modified: std::fs::metadata(file).and_then(|m| m.modified()).ok(),
    })
}

/// Fills in `${NAME}` from the environment; `TITAN_FIRMWARE_DIR` falls back to the checkout found, and is then
/// noted in `env` so the simulator gets it too
fn expand(text: &str, titan: Option<&Path>, env: &mut BTreeMap<String, String>) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let end = rest[start..].find('}').ok_or_else(|| format!("unclosed ${{ in {text}"))? + start;
        let name = &rest[start + 2..end];
        let value = match std::env::var(name) {
            Ok(value) => value,
            Err(_) if name == "TITAN_FIRMWARE_DIR" => {
                let dir = titan.ok_or("TITAN_FIRMWARE_DIR isn't set and no titan_firmware checkout was found")?;
                let value = dir.display().to_string();
                env.insert(name.to_owned(), value.clone());
                value
            }
            Err(_) => return Err(format!("{name} isn't set")),
        };
        out.push_str(&value);
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// What a firmware image talks micro-ROS over. A titan_firmware C image (`*_with_bl.uf2`) has its `.elf` beside
/// it, whose symbols name the transport: `transport_usb_open` or `transport_can_open`.
pub fn transport(image: &Path) -> Transport {
    let Some(name) = image.file_name().and_then(|n| n.to_str()) else { return Transport::Other };
    let Some(stem) = name.strip_suffix("_with_bl.uf2") else { return Transport::Other };
    let Ok(elf) = std::fs::read(image.with_file_name(format!("{stem}.elf"))) else {
        // Built, but without its .elf to look at: IVCBoard2 is the one C board on USB
        return if stem.to_ascii_lowercase().contains("ivc") { Transport::Usb } else { Transport::Can };
    };
    let has = |symbol: &[u8]| elf.windows(symbol.len()).any(|w| w == symbol);
    if has(b"transport_usb_open") {
        Transport::Usb
    } else if has(b"transport_can_open") {
        Transport::Can
    } else {
        Transport::Other
    }
}

/// Something to open for a robot: a board's console or agent, or a bus's agent
#[derive(Clone, Debug, PartialEq)]
pub enum Attach {
    /// The agent on a board's micro-ROS terminal, once it appears
    Agent { name: String, path: PathBuf },
    /// The nano-ROS agent on a board's serial port, once it appears
    NanoAgent { name: String, path: PathBuf },
    /// A board's log
    Console { name: String, path: PathBuf },
    /// The agent the simulator hands a bus's micro-ROS sessions to
    UdpAgent { name: String, port: u16 },
}

impl Attach {
    /// As `--agent NAME=PATH` and `--console NAME=PATH` give it to an isolated window
    pub fn args(&self) -> [String; 2] {
        match self {
            Attach::Agent { name, path } => ["--agent".to_owned(), format!("{name}={}", path.display())],
            Attach::NanoAgent { name, path } => ["--nano-agent".to_owned(), format!("{name}={}", path.display())],
            Attach::Console { name, path } => ["--console".to_owned(), format!("{name}={}", path.display())],
            Attach::UdpAgent { name, port } => ["--udp-agent".to_owned(), format!("{name}={port}")],
        }
    }

    /// Reads `NAME=VALUE` back for `flag`
    pub fn parse(flag: &str, value: &str) -> Result<Attach, String> {
        let (name, rest) = value.split_once('=').ok_or_else(|| format!("{flag} takes NAME=PATH, not {value}"))?;
        let name = name.to_owned();
        match flag {
            "--agent" => Ok(Attach::Agent { name, path: rest.into() }),
            "--nano-agent" => Ok(Attach::NanoAgent { name, path: rest.into() }),
            "--console" => Ok(Attach::Console { name, path: rest.into() }),
            "--udp-agent" => {
                rest.parse().map(|port| Attach::UdpAgent { name, port }).map_err(|_| format!("{rest} isn't a port"))
            }
            _ => Err(format!("{flag} isn't an attach flag")),
        }
    }
}

/// The source an attach opens, as a profile would write it
pub enum Spec {
    Process(ProcessSpec),
    Serial(SerialSpec),
}

impl Attach {
    /// The source this opens. `agent` finds an agent of a kind, when it's one; `named` makes the suggested name
    /// (like "agent b_ivc") one that isn't taken.
    pub fn spec(
        &self,
        agent: impl FnOnce(AgentKind) -> Result<Agent, String>,
        named: impl FnOnce(&str) -> String,
    ) -> Result<Spec, String> {
        let process = |name: String, cmd: String| ProcessSpec {
            name,
            cmd,
            cwd: None,
            env: Default::default(),
            autostart: true,
            restart: RestartPolicy::OnFailure,
        };
        Ok(match self {
            Attach::Console { name, path } => Spec::Serial(SerialSpec {
                name: named(&format!("console {name}")),
                port: path.display().to_string(),
                baud: 115_200,
                autostart: true,
                line_ending: Default::default(),
            }),
            Attach::Agent { name, path } | Attach::NanoAgent { name, path } => {
                let kind =
                    if matches!(self, Attach::NanoAgent { .. }) { AgentKind::NanoRos } else { AgentKind::MicroRos };
                // A simulated board's terminal appears once the simulator is up
                let cmd = format!(
                    "while [ ! -e {p} ]; do sleep 0.2; done; {}",
                    agent_command(&agent(kind)?, &Port::Simulated(path.clone())),
                    p = quote(path)
                );
                Spec::Process(process(named(&format!("agent {name}")), cmd))
            }
            Attach::UdpAgent { name, port } => Spec::Process(process(
                named(&format!("agent {name}")),
                agent_command(&agent(AgentKind::MicroRos)?, &Port::Udp(*port)),
            )),
        })
    }
}

/// How a robot runs: the simulator's command, what this window opens, and what each isolated window opens
#[derive(Debug, PartialEq)]
pub struct Plan {
    pub command: String,
    pub here: Vec<Attach>,
    pub windows: Vec<Vec<Attach>>,
}

/// `running` lists the board files already simulated in this window (a robot's copy of one goes elsewhere);
/// `udp_port` hands out a free port for each bus's agent
pub fn plan(
    simulator: &Simulator,
    robot: &Robot,
    slug: &str,
    running: &[PathBuf],
    mut udp_port: impl FnMut() -> u16,
) -> Plan {
    let link = |board: &str, port: &str| PathBuf::from(format!("/tmp/archon-{slug}-{board}-{port}"));
    let mut args = Vec::new();
    let mut links = Vec::new();
    let mut here = Vec::new();
    let mut windows: Vec<Vec<Attach>> = Vec::new();
    // How many copies of each board file are placed so far
    let mut copies: BTreeMap<PathBuf, usize> = BTreeMap::new();
    for board in &robot.boards {
        if board.transport == Transport::Other {
            continue;
        }
        let (log, ros) = (link(&board.name, "log"), link(&board.name, "ros"));
        args.push(format!("--usb-cdc {}=pty:{}", board.name, quote(&log)));
        links.push(log.clone());
        let console = Attach::Console { name: board.name.clone(), path: log };
        if board.transport != Transport::Usb {
            here.push(console);
            continue;
        }
        args.push(format!("--usb-cdc2 {}=pty:{}", board.name, quote(&ros)));
        links.push(ros.clone());
        let agent = Attach::Agent { name: board.name.clone(), path: ros };
        let key = same_file(&board.board);
        let already = running.iter().any(|r| same_file(r) == key);
        let copy = copies.entry(key).or_insert(usize::from(already));
        match *copy {
            0 => here.extend([agent, console]),
            n => {
                if windows.len() < n {
                    windows.resize_with(n, Vec::new);
                }
                windows[n - 1].extend([agent, console]);
            }
        }
        *copy += 1;
    }
    // One agent per bus that has C boards on micro-ROS over CAN
    let mut buses: Vec<&str> = Vec::new();
    for board in robot.boards.iter().filter(|b| b.transport == Transport::Can) {
        let bus = board.bus.as_deref().unwrap_or("can");
        if !buses.contains(&bus) {
            buses.push(bus);
        }
    }
    for bus in buses {
        let port = udp_port();
        args.push(format!("--micro-ros-agent {bus}=127.0.0.1:{port}"));
        here.push(Attach::UdpAgent { name: bus.to_owned(), port });
    }
    let env: String = robot.env.iter().map(|(k, v)| format!("export {k}={}; ", quote(Path::new(v)))).collect();
    let removed: Vec<String> = links.iter().map(|l| quote(l)).collect();
    let command = format!(
        "rm -f {}; {env}exec {} robot {} --realtime --time 86400s {}",
        removed.join(" "),
        quote(&simulator.binary),
        quote(&robot.file),
        args.join(" ")
    );
    Plan { command, here, windows }
}

/// A path that's equal for the same file however it's written: resolved on disk, or with `..` taken out
fn same_file(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| {
        let mut clean = PathBuf::new();
        for part in path.components() {
            match part {
                std::path::Component::ParentDir => _ = clean.pop(),
                std::path::Component::CurDir => {}
                other => clean.push(other),
            }
        }
        clean
    })
}

/// A free UDP port for an agent, from 8888 up, skipping ones already handed out
pub fn free_udp_port(taken: &[u16]) -> u16 {
    (8888..9888).find(|p| !taken.contains(p) && std::net::UdpSocket::bind(("127.0.0.1", *p)).is_ok()).unwrap_or(8888)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("archon-robot-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A fake C build: the image, and an .elf naming its transport
    fn build(dir: &Path, stem: &str, symbol: &str) -> PathBuf {
        let image = dir.join(format!("{stem}_with_bl.uf2"));
        std::fs::write(&image, b"UF2").unwrap();
        std::fs::write(dir.join(format!("{stem}.elf")), format!("\0{symbol}\0")).unwrap();
        image
    }

    #[test]
    fn tells_usb_from_can_by_the_elf() {
        let dir = temp("transport");
        assert_eq!(transport(&build(&dir, "IVCBoard2", "transport_usb_open")), Transport::Usb);
        assert_eq!(transport(&build(&dir, "esc_board", "transport_can_open")), Transport::Can);
        assert_eq!(transport(&dir.join("ivc_board2")), Transport::Other, "a Rust ELF");
    }

    #[test]
    fn reads_the_real_c_builds_if_there_are_any() {
        let Some(titan) = titan_firmware_dir(&dirs::home_dir().unwrap()) else { return };
        let ivc = titan.join("build/IVCBoard2/IVCBoard2_with_bl.uf2");
        if ivc.is_file() {
            assert_eq!(transport(&ivc), Transport::Usb);
        }
        let esc = titan.join("talos/ESCBoard/build/esc_board_with_bl.uf2");
        if esc.is_file() {
            assert_eq!(transport(&esc), Transport::Can);
        }
    }

    fn robot_file(dir: &Path, body: &str) -> PathBuf {
        let file = dir.join("robots/test.toml");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, body).unwrap();
        file
    }

    #[test]
    fn plans_consoles_agents_and_isolated_copies() {
        let dir = temp("plan");
        let titan = dir.join("titan");
        std::fs::create_dir_all(titan.join("build")).unwrap();
        build(&titan.join("build"), "IVCBoard2", "transport_usb_open");
        build(&titan.join("build"), "esc_board", "transport_can_open");
        let file = robot_file(
            &dir,
            r#"
            [[board]]
            name = "a_ivc"
            board = "../boards/ivc.toml"
            elf = "${TITAN_FIRMWARE_DIR}/build/IVCBoard2_with_bl.uf2"
            bus = "talos"
            [[board]]
            name = "b_ivc"
            board = "../boards/ivc.toml"
            elf = "${TITAN_FIRMWARE_DIR}/build/IVCBoard2_with_bl.uf2"
            bus = "surface"
            [[board]]
            name = "esc0"
            board = "../boards/esc.toml"
            elf = "${TITAN_FIRMWARE_DIR}/build/esc_board_with_bl.uf2"
            bus = "internal"
            [[board]]
            name = "esc1"
            board = "../boards/esc.toml"
            elf = "${TITAN_FIRMWARE_DIR}/build/esc_board_with_bl.uf2"
            bus = "internal"
            [[board]]
            name = "rust"
            board = "../boards/ivc.toml"
            elf = "rust_board"
            "#,
        );
        std::fs::write(dir.join("robots/rust_board"), b"ELF").unwrap();
        let robot = load(&file, Some(&titan)).unwrap();
        assert_eq!(robot.problem(), None);
        assert_eq!(robot.env, [("TITAN_FIRMWARE_DIR".to_owned(), titan.display().to_string())]);
        let transports: Vec<Transport> = robot.boards.iter().map(|b| b.transport).collect();
        assert_eq!(transports, [Transport::Usb, Transport::Usb, Transport::Can, Transport::Can, Transport::Other]);

        let simulator = Simulator { binary: "/sim/firmware_simulator".into(), checkout: dir.clone() };
        let plan = plan(&simulator, &robot, "t", &[], || 8890);
        let path = |b: &str, p: &str| PathBuf::from(format!("/tmp/archon-t-{b}-{p}"));
        assert_eq!(
            plan.here,
            [
                Attach::Agent { name: "a_ivc".into(), path: path("a_ivc", "ros") },
                Attach::Console { name: "a_ivc".into(), path: path("a_ivc", "log") },
                Attach::Console { name: "esc0".into(), path: path("esc0", "log") },
                Attach::Console { name: "esc1".into(), path: path("esc1", "log") },
                Attach::UdpAgent { name: "internal".into(), port: 8890 },
            ]
        );
        assert_eq!(
            plan.windows,
            [vec![
                Attach::Agent { name: "b_ivc".into(), path: path("b_ivc", "ros") },
                Attach::Console { name: "b_ivc".into(), path: path("b_ivc", "log") },
            ]],
            "the second IVC goes to an isolated window"
        );
        assert!(plan.command.contains("--usb-cdc2 b_ivc=pty:/tmp/archon-t-b_ivc-ros"), "{}", plan.command);
        assert!(plan.command.contains("--micro-ros-agent internal=127.0.0.1:8890"), "{}", plan.command);
        assert!(plan.command.contains(&format!("export TITAN_FIRMWARE_DIR={};", titan.display())), "{}", plan.command);
        assert!(!plan.command.contains("rust="), "a Rust board gets no ports");

        // With an IVC already simulated in this window, both of the robot's go elsewhere
        let running = [dir.join("boards/ivc.toml")];
        let plan2 = super::plan(&simulator, &robot, "t", &running, || 8890);
        assert_eq!(plan2.windows.len(), 2);
    }

    #[test]
    fn says_which_image_is_missing() {
        let dir = temp("missing");
        let file =
            robot_file(&dir, "[[board]]\nname = \"x\"\nboard = \"b.toml\"\nelf = \"${NOT_SET_ANYWHERE}/x.uf2\"\n");
        let robot = load(&file, None).unwrap();
        assert_eq!(robot.problem().as_deref(), Some("x: NOT_SET_ANYWHERE isn't set"));
    }

    #[test]
    fn attach_flags_round_trip() {
        for attach in [
            Attach::Agent { name: "b_ivc".into(), path: "/tmp/x-ros".into() },
            Attach::Console { name: "b_ivc".into(), path: "/tmp/x-log".into() },
            Attach::UdpAgent { name: "internal".into(), port: 8890 },
            Attach::NanoAgent { name: "led".into(), path: "/tmp/led".into() },
        ] {
            let [flag, value] = attach.args();
            assert_eq!(Attach::parse(&flag, &value).unwrap(), attach);
        }
        assert!(Attach::parse("--agent", "no-equals").is_err());
    }
}
