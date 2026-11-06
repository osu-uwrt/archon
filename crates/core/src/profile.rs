//! A profile describes one test bench: the processes and serial ports to bring up together. It lives in the project
//! as `archon.toml`.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::Deserialize;

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
    /// The file this was loaded from
    #[serde(skip)]
    pub path: Option<PathBuf>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct ProcessSpec {
    pub name: String,
    /// Run with `bash -c`, so `source install/setup.bash && ...` works
    pub cmd: String,
    /// Relative to the profile's directory, which is also the default
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub autostart: bool,
    #[serde(default)]
    pub restart: RestartPolicy,
}

#[derive(Deserialize, Debug, Clone)]
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

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
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

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
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
        let all_names = profile.processes.iter().map(|p| &p.name).chain(profile.serials.iter().map(|s| &s.name));
        for name in all_names {
            if !names.insert(name) {
                bail!("two sources are named {name:?}");
            }
        }
        Ok(profile)
    }

    /// Looks for `archon.toml` in `start` and each directory above it
    pub fn find(start: &Path) -> Option<PathBuf> {
        start.ancestors().map(|dir| dir.join(FILE_NAME)).find(|path| path.is_file())
    }
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
            "#,
        )
        .unwrap();
        assert_eq!(profile.env["RMW_IMPLEMENTATION"], "rmw_fastrtps_cpp");
        assert_eq!(profile.processes[0].restart, RestartPolicy::OnFailure);
        assert_eq!(profile.serials[0].baud, 115_200);
        assert!(profile.serials[0].autostart);
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
