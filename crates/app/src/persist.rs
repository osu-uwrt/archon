//! The layout of each profile, kept between runs so a bench opens as it was left: the panes and what each shows, and
//! the sidebar and timestamps. Stored under `~/.local/state/archon/`, one file per profile.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::layout::Tile;

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct Saved {
    #[serde(default)]
    pub zoom: Option<f32>,
    pub sidebar: bool,
    pub timestamps: bool,
    pub active: usize,
    pub root: Tile,
    pub panes: Vec<PaneSpec>,
}

/// What a pane shows, by name, since source numbers can change when the profile does
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
#[serde(tag = "view", rename_all = "kebab-case")]
pub enum PaneSpec {
    Source {
        name: String,
    },
    Topic {
        name: String,
    },
    AllLogs,
    Topics,
    Welcome,
    Publish {
        name: String,
        topic: String,
        ty: String,
    },
    /// Each series as its topic echo's name and the field's path
    Plot {
        series: Vec<(String, String)>,
        seconds: f64,
    },
}

/// The state file for a profile: its absolute path with the slashes made safe, so each profile gets its own
pub fn state_file(profile: Option<&Path>) -> Option<PathBuf> {
    let name = match profile.and_then(|p| p.canonicalize().ok()) {
        Some(path) => path.to_string_lossy().trim_start_matches('/').replace(['/', '\\'], "%"),
        None => "no-profile".to_owned(),
    };
    Some(dirs::state_dir()?.join("archon").join(format!("{name}.toml")))
}

/// Where a profile's named layouts are kept: one file per layout, beside its state file
pub fn layouts_dir(state_file: &Path) -> PathBuf {
    let stem = state_file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    state_file.with_file_name(format!("{stem}.layouts"))
}

/// The names of the saved layouts, sorted
pub fn layout_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            e.path()
                .file_stem()
                .filter(|_| e.path().extension().is_some_and(|x| x == "toml"))
                .map(|s| s.to_string_lossy().into_owned())
        })
        .collect();
    names.sort();
    names
}

/// A layout name as a file name: anything but letters, digits, `-` and `_` becomes `_`
pub fn layout_file(dir: &Path, name: &str) -> PathBuf {
    let safe: String = name
        .trim()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' { c } else { '_' })
        .collect();
    dir.join(format!("{safe}.toml"))
}

pub fn load(path: &Path) -> Option<Saved> {
    toml::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

pub fn save(path: &Path, saved: &Saved) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, toml::to_string(saved)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let saved = Saved {
            zoom: Some(1.25),
            sidebar: false,
            timestamps: true,
            active: 1,
            root: Tile::Split {
                side_by_side: true,
                ratio: 0.4,
                first: Box::new(Tile::Pane(0)),
                second: Box::new(Tile::Split {
                    side_by_side: false,
                    ratio: 0.5,
                    first: Box::new(Tile::Pane(1)),
                    second: Box::new(Tile::Pane(2)),
                }),
            },
            panes: vec![
                PaneSpec::Source { name: "agent".into() },
                PaneSpec::Topic { name: "/imu/data".into() },
                PaneSpec::AllLogs,
            ],
        };
        let path = std::env::temp_dir().join(format!("archon-layout-test-{}.toml", std::process::id()));
        save(&path, &saved).unwrap();
        assert_eq!(load(&path), Some(saved));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn named_layouts_list_and_sanitize() {
        let state = std::env::temp_dir().join(format!("archon-layouts-test-{}", std::process::id())).join("bench.toml");
        let dir = layouts_dir(&state);
        let saved = Saved {
            zoom: None,
            sidebar: true,
            timestamps: false,
            active: 0,
            root: Tile::Pane(0),
            panes: vec![PaneSpec::AllLogs],
        };
        save(&layout_file(&dir, "flash test"), &saved).unwrap();
        save(&layout_file(&dir, "a/b"), &saved).unwrap();
        assert_eq!(layout_names(&dir), ["a_b", "flash test"]);
        assert_eq!(load(&layout_file(&dir, "flash test")), Some(saved));
        let _ = std::fs::remove_dir_all(state.parent().unwrap());
    }

    #[test]
    fn a_profile_gets_its_own_file() {
        let a = state_file(Some(Path::new("/"))).unwrap();
        let none = state_file(None).unwrap();
        assert_ne!(a, none);
        assert!(none.ends_with("archon/no-profile.toml"));
    }
}
