//! Bags to play back: archon's own recordings (`bags/` beside the profile) and any other rosbag2 folder near the
//! bench, read from their `metadata.yaml` for how long they run and how much is in them.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::bench::quote;

#[derive(Clone, Debug, PartialEq)]
pub struct Bag {
    pub path: PathBuf,
    pub name: String,
    pub modified: Option<SystemTime>,
    pub seconds: Option<f64>,
    pub messages: Option<u64>,
}

/// The bags in `dirs` and the folders right inside them, newest first
pub fn find(dirs: &[PathBuf]) -> Vec<Bag> {
    let mut found = Vec::new();
    for dir in dirs {
        let mut candidates = vec![dir.clone()];
        candidates.extend(std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()));
        for path in candidates {
            if let Some(bag) = read(&path)
                && !found.iter().any(|b: &Bag| b.path == bag.path)
            {
                found.push(bag);
            }
        }
    }
    found.sort_by_key(|b| std::cmp::Reverse(b.modified));
    found
}

/// A rosbag2 folder: one with a `metadata.yaml`
pub fn read(path: &Path) -> Option<Bag> {
    let metadata = path.join("metadata.yaml");
    let text = std::fs::read_to_string(&metadata).ok()?;
    let mut seconds = None;
    let mut messages = None;
    let mut in_duration = false;
    // The first of each is the bag's own; the per-topic counts come after
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == "duration:" {
            in_duration = true;
        } else if in_duration && let Some(n) = trimmed.strip_prefix("nanoseconds:") {
            seconds = seconds.or(n.trim().parse::<f64>().ok().map(|n| n / 1e9));
            in_duration = false;
        } else if let Some(n) = trimmed.strip_prefix("message_count:") {
            messages = messages.or(n.trim().parse().ok());
        }
    }
    Some(Bag {
        path: path.to_owned(),
        name: path.file_name()?.to_string_lossy().into_owned(),
        modified: std::fs::metadata(&metadata).and_then(|m| m.modified()).ok(),
        seconds,
        messages,
    })
}

impl Bag {
    /// Like "2 min 5 s · 41210 messages"
    pub fn summary(&self) -> String {
        let length = self.seconds.map(|s| {
            if s >= 60.0 { format!("{} min {} s", (s / 60.0) as u64, (s % 60.0) as u64) } else { format!("{s:.1} s") }
        });
        let messages = self.messages.map(|n| format!("{n} messages"));
        [length, messages].into_iter().flatten().collect::<Vec<_>>().join(" · ")
    }
}

/// Whether `ros2 bag` is there: the ROS environment's package index has ros2bag
pub fn available() -> bool {
    let prefixes = std::env::var_os("AMENT_PREFIX_PATH").unwrap_or_default();
    std::env::split_paths(&prefixes).any(|p| p.join("share/ament_index/resource_index/packages/ros2bag").exists())
}

/// What to say when it isn't
pub const MISSING: &str = "ros2 bag isn't in this ROS environment: it comes with rosbag2 (archon's nix develop has it; \
                           the uwrt ros shell alone doesn't)";

pub fn play_command(bag: &Bag, looped: bool) -> String {
    let looping = if looped { " --loop" } else { "" };
    format!("exec ros2 bag play {}{looping}", quote(&bag.path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_bags_length_and_count() {
        let dir = std::env::temp_dir().join(format!("archon-bags-{}", std::process::id()));
        let bag = dir.join("bags/archon-2026-11-06_04-30-00");
        std::fs::create_dir_all(&bag).unwrap();
        std::fs::write(
            bag.join("metadata.yaml"),
            "rosbag2_bagfile_information:\n  version: 5\n  duration:\n    nanoseconds: 125500000000\n  starting_time:\n    nanoseconds_since_epoch: 1\n  message_count: 41210\n  topics_with_message_count:\n    - topic_metadata:\n        name: /rosout\n      message_count: 3\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("not-a-bag")).unwrap();
        let found = find(&[dir.join("bags"), dir.clone()]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].messages, Some(41210));
        assert_eq!(found[0].summary(), "2 min 5 s · 41210 messages");
        assert_eq!(play_command(&found[0], true), format!("exec ros2 bag play {} --loop", bag.display()));
    }
}
