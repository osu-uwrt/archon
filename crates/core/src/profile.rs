//! A profile describes one test bench: the processes and serial ports to bring up together. It lives in the project
//! as `archon.toml`.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

pub const FILE_NAME: &str = "archon.toml";

#[derive(Deserialize, Default, Debug)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// Environment for every process, such as `RMW_IMPLEMENTATION`
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default, rename = "process")]
    pub processes: Vec<ProcessSpec>,
    #[serde(default, rename = "serial")]
    pub serials: Vec<SerialSpec>,
    /// CAN buses to dump, like `candump`
    #[serde(default, rename = "can")]
    pub cans: Vec<CanSpec>,
    /// ROS topics to echo, opened with the profile
    #[serde(default, rename = "echo")]
    pub echoes: Vec<EchoSpec>,
    /// Topics whose rate the health strip shows
    #[serde(default, rename = "watch")]
    pub watches: Vec<WatchSpec>,
    /// Messages to publish, ready to send from the sidebar, the palette or a key
    #[serde(default, rename = "publish")]
    pub publishes: Vec<PublishSpec>,
    /// The file this was loaded from
    #[serde(skip)]
    pub path: Option<PathBuf>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct ProcessSpec {
    pub name: String,
    /// Run with `bash -c`, so `source install/setup.bash && ...` works
    pub cmd: String,
    /// Relative to the profile's directory, which is also the default
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub autostart: bool,
    #[serde(default)]
    pub restart: RestartPolicy,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct SerialSpec {
    pub name: String,
    /// Prefer a `/dev/serial/by-id/...` path, which survives the board re-enumerating
    pub port: String,
    #[serde(default = "default_baud")]
    pub baud: u32,
    #[serde(default = "yes")]
    pub autostart: bool,
    /// Appended to each line sent from the input bar
    #[serde(default)]
    pub line_ending: LineEnding,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct EchoSpec {
    pub topic: String,
    /// Defaults to the topic
    pub name: Option<String>,
    /// Like `std_msgs/msg/String`; found on the graph when left out
    #[serde(rename = "type")]
    pub ty: Option<String>,
    #[serde(default = "yes")]
    pub autostart: bool,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct PublishSpec {
    pub name: String,
    pub topic: String,
    /// Like `std_msgs/msg/Bool`
    #[serde(rename = "type")]
    pub ty: String,
    /// The fields to set, as a table shaped like the message; the rest stay zero
    #[serde(default)]
    pub value: toml::Table,
    /// Running the action sends at this many Hz until run again, instead of once
    pub rate: Option<f64>,
    /// A key to run it, like `F2` or `ctrl+k`
    pub key: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct CanSpec {
    pub name: String,
    /// Like `can0` or `vcan0`
    pub interface: String,
    #[serde(default = "yes")]
    pub autostart: bool,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct WatchSpec {
    pub topic: String,
    /// Below this the topic shows as a warning; with none, any message is fine
    pub min_hz: Option<f64>,
}

impl EchoSpec {
    pub fn name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.topic)
    }
}

#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    #[default]
    Never,
    OnFailure,
    Always,
}

impl RestartPolicy {
    pub fn should_restart(self, success: bool) -> bool {
        match self {
            RestartPolicy::Never => false,
            RestartPolicy::OnFailure => !success,
            RestartPolicy::Always => true,
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum LineEnding {
    #[default]
    Lf,
    Crlf,
    Cr,
    None,
}

impl LineEnding {
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::Crlf => "\r\n",
            LineEnding::Cr => "\r",
            LineEnding::None => "",
        }
    }
}

fn default_baud() -> u32 {
    115_200
}

fn yes() -> bool {
    true
}

impl Profile {
    pub fn load(path: &Path) -> anyhow::Result<Profile> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut profile = Self::parse(&text).with_context(|| format!("in {}", path.display()))?;
        let dir = path.parent().unwrap_or(Path::new("."));
        for process in &mut profile.processes {
            process.cwd = Some(match process.cwd.take() {
                Some(cwd) => dir.join(cwd),
                None => dir.to_owned(),
            });
        }
        profile.path = Some(path.to_owned());
        Ok(profile)
    }

    pub fn parse(text: &str) -> anyhow::Result<Profile> {
        let profile: Profile = toml::from_str(text)?;
        let mut names = HashSet::new();
        let all_names = profile.processes.iter().map(|p| p.name.as_str());
        let all_names = all_names.chain(profile.serials.iter().map(|s| s.name.as_str()));
        let all_names = all_names.chain(profile.cans.iter().map(|c| c.name.as_str()));
        for name in all_names.chain(profile.echoes.iter().map(EchoSpec::name)) {
            if !names.insert(name) {
                bail!("two sources are named {name:?}");
            }
        }
        Ok(profile)
    }

    /// Whether a source of this name is in the profile file, so removing it can offer to take it out of there too
    pub fn defines(&self, name: &str) -> bool {
        self.processes.iter().any(|p| p.name == name)
            || self.serials.iter().any(|s| s.name == name)
            || self.cans.iter().any(|c| c.name == name)
            || self.echoes.iter().any(|e| e.name() == name)
    }

    /// Takes the source named `name` out of a profile file, keeping the rest of the file as it was (comments too).
    /// Returns whether it was there.
    pub fn remove_from_file(path: &Path, name: &str) -> anyhow::Result<bool> {
        remove_tables(path, &["process", "serial", "can", "echo"], name)
    }

    /// Takes the `[[publish]]` action named `name` out of a profile file, keeping the rest as it was
    pub fn remove_publish_from_file(path: &Path, name: &str) -> anyhow::Result<bool> {
        remove_tables(path, &["publish"], name)
    }

    /// Looks for `archon.toml` in `start` and each directory above it
    pub fn find(start: &Path) -> Option<PathBuf> {
        start.ancestors().map(|dir| dir.join(FILE_NAME)).find(|path| path.is_file())
    }
}

/// Takes the tables under `keys` named `name` out of a profile file, keeping the rest as it was (comments too)
fn remove_tables(path: &Path, keys: &[&str], name: &str) -> anyhow::Result<bool> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut doc: toml_edit::DocumentMut = text.parse().with_context(|| format!("in {}", path.display()))?;
    // Comments at the top of the file belong to the first table; keep them if that's the one going
    let header = first_table(&mut doc).and_then(|d| d.prefix()).and_then(|p| p.as_str()).map(str::to_owned);
    let mut removed = false;
    for key in keys {
        let Some(tables) = doc.get_mut(key).and_then(|item| item.as_array_of_tables_mut()) else { continue };
        let before = tables.len();
        // An echo without a name goes by its topic
        tables.retain(|table| {
            let named = table.get("name").or_else(|| table.get("topic")).and_then(|v| v.as_str());
            named != Some(name)
        });
        removed |= tables.len() != before;
        if tables.is_empty() {
            doc.remove(key);
        }
    }
    if let (Some(header), Some(first)) = (header, first_table(&mut doc)) {
        first.set_prefix(header);
    }
    if removed {
        std::fs::write(path, doc.to_string()).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(removed)
}

/// The decor of the table that comes first in the file
fn first_table(doc: &mut toml_edit::DocumentMut) -> Option<&mut toml_edit::Decor> {
    let mut first: Option<(isize, &mut toml_edit::Decor)> = None;
    for (_, item) in doc.iter_mut() {
        let tables: Vec<&mut toml_edit::Table> = match item {
            toml_edit::Item::Table(table) => vec![table],
            toml_edit::Item::ArrayOfTables(tables) => tables.iter_mut().collect(),
            _ => Vec::new(),
        };
        for table in tables {
            if let Some(at) = table.position()
                && first.as_ref().is_none_or(|(best, _)| at < *best)
            {
                first = Some((at, table.decor_mut()));
            }
        }
    }
    first.map(|(_, decor)| decor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_bench() {
        let profile = Profile::parse(
            r#"
            env = { RMW_IMPLEMENTATION = "rmw_fastrtps_cpp" }

            [[process]]
            name = "agent"
            cmd = "ros2 run micro_ros_agent micro_ros_agent serial --dev /dev/ttyACM0"
            autostart = true
            restart = "on-failure"

            [[serial]]
            name = "debug"
            port = "/dev/ttyACM1"

            [[echo]]
            topic = "/rosout"

            [[watch]]
            topic = "/imu/data"
            min_hz = 100

            [[publish]]
            name = "kill"
            topic = "/talos/state/kill"
            type = "std_msgs/msg/Bool"
            value = { data = true }
            key = "F2"
            "#,
        )
        .unwrap();
        assert_eq!(profile.echoes[0].name(), "/rosout");
        assert!(profile.echoes[0].autostart);
        assert_eq!(profile.watches[0].min_hz, Some(100.0));
        assert_eq!(profile.publishes[0].value["data"].as_bool(), Some(true));
        assert_eq!(profile.env["RMW_IMPLEMENTATION"], "rmw_fastrtps_cpp");
        assert_eq!(profile.processes[0].restart, RestartPolicy::OnFailure);
        assert_eq!(profile.serials[0].baud, 115_200);
        assert!(profile.serials[0].autostart);
    }

    #[test]
    fn shipped_profiles_parse() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut paths = vec![root.join("archon.example.toml")];
        paths.extend(std::fs::read_dir(root.join("examples")).unwrap().map(|e| e.unwrap().path()));
        for path in paths.iter().filter(|p| p.extension().is_some_and(|e| e == "toml")) {
            Profile::load(path).unwrap_or_else(|e| panic!("{e:#}"));
        }
    }

    #[test]
    fn removes_a_source_from_the_file_keeping_the_rest() {
        let path = std::env::temp_dir().join(format!("archon-remove-{}.toml", std::process::id()));
        let text = "# my bench\n\n[[process]]\nname = \"agent\" # the board's\ncmd = \"a\"\n\n[[process]]\nname = \"sim\"\ncmd = \"b\"\n\n[[echo]]\ntopic = \"/rosout\"\n";
        std::fs::write(&path, text).unwrap();
        assert!(Profile::remove_from_file(&path, "agent").unwrap());
        assert!(Profile::remove_from_file(&path, "/rosout").unwrap());
        assert!(!Profile::remove_from_file(&path, "nothing").unwrap());
        let left = std::fs::read_to_string(&path).unwrap();
        assert!(left.starts_with("# my bench"), "{left}");
        let profile = Profile::load(&path).unwrap();
        assert_eq!(profile.processes.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["sim"]);
        assert!(profile.echoes.is_empty());

        std::fs::write(&path, "[[process]]\nname = \"kill\"\ncmd = \"a\"\n\n[[publish]]\nname = \"kill\"\ntopic = \"/kill\"\ntype = \"std_msgs/msg/Bool\"\n").unwrap();
        assert!(Profile::remove_publish_from_file(&path, "kill").unwrap());
        let profile = Profile::load(&path).unwrap();
        assert!(profile.publishes.is_empty());
        assert_eq!(profile.processes.len(), 1, "a source of the same name stays");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rejects_duplicate_names() {
        let err = Profile::parse(
            r#"
            [[process]]
            name = "x"
            cmd = "true"
            [[serial]]
            name = "x"
            port = "/dev/null"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("\"x\""));
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(Profile::parse("[[process]]\nname = \"x\"\ncmd = \"true\"\nautostrat = true\n").is_err());
    }
}
